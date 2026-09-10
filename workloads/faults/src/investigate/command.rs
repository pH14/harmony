// SPDX-License-Identifier: AGPL-3.0-or-later
//! Command publication binds physical completion to one immutable journal cut.

use super::*;

#[derive(Serialize)]
struct ExecFingerprintArgs {
    target: FingerprintSelector,
    probe: bool,
    argv: Vec<String>,
    within_nanos: u64,
    until: Option<FingerprintCondition>,
    extend: bool,
    wall_seconds: u64,
}

impl ExecFingerprintArgs {
    fn new(request: &ExecRequest) -> Self {
        let bound = normalize_advance(&request.bound);
        Self {
            target: FingerprintSelector::from_selector(&request.target),
            probe: request.probe,
            argv: request.argv.clone(),
            within_nanos: bound.within_nanos,
            until: bound.until.map(FingerprintCondition::from_condition),
            extend: bound.extend,
            wall_seconds: bound.wall_seconds,
        }
    }
}

fn validate_request(request: &ExecRequest) -> Result<(), Box<dyn Error>> {
    if request.argv.is_empty()
        || request.argv[0].is_empty()
        || request.argv.iter().any(|arg| arg.contains('\0'))
    {
        return Err("a guest command needs nonempty argv and cannot contain NUL bytes".into());
    }
    if !request.probe && !matches!(request.target, Selector::BranchHead(_)) {
        return Err("exec on an immutable source requires probe intent".into());
    }
    Ok(())
}

/// Return a committed command reply without preparing artifacts or opening a VM.
pub fn retry_exec(
    workspace: &Workspace,
    request: &ExecRequest,
) -> Result<Option<Outcome>, Box<dyn Error>> {
    validate_request(request)?;
    retry_request(
        workspace,
        request.request_id.as_deref(),
        "exec",
        &ExecFingerprintArgs::new(request),
    )
}

/// Execute once and publish the checkpoint, output, reply and branch atomically.
pub fn exec(
    workspace: &mut Workspace,
    engine: &mut dyn Continuation,
    request: &ExecRequest,
) -> Result<Outcome, Box<dyn Error>> {
    if let Some(outcome) = retry_exec(workspace, request)? {
        return Ok(outcome);
    }
    let request_fingerprint = request
        .request_id
        .as_ref()
        .map(|_| fingerprint("exec", ExecFingerprintArgs::new(request)))
        .transpose()?;
    let source = source_details(workspace, &request.target)?;
    if source
        .command
        .as_ref()
        .is_some_and(|command| matches!(command.completion, CommandCompletion::Pending))
    {
        return Err("the source has a pending command; resume it before starting another".into());
    }
    let mut branch = if request.probe {
        Branch {
            name: next_probe_name(workspace)?,
            source: source.label.clone(),
            start: String::new(),
            head: String::new(),
            history: source.history,
            continuation_end: source.continuation_end,
            inherited_actions: source.actions.clone(),
            probe: true,
            pending_command: None,
        }
    } else {
        let Selector::BranchHead(name) = &request.target else {
            unreachable!("validated target")
        };
        let branch = workspace
            .branch(name)
            .ok_or_else(|| unknown_branch(workspace, name))?
            .clone();
        validate_pending_branch(&branch, source.command.as_ref())?;
        branch
    };
    let returned = if let Some(checkpoint) = &source.checkpoint {
        engine.restore(checkpoint, &source.actions, &source.endpoint)
    } else {
        engine.open_recorded(&source.actions, source.endpoint.virtual_time, 0)
    }
    .map_err(|error| format!("open command source {}: {error}", source.label))?;
    let restored = capture_verified(engine, returned, workspace.facts(), &source.actions)?;
    if let Some(checkpoint) = &source.checkpoint {
        validate_restored_plan(&restored, checkpoint)?;
    }
    validate_source_match(&restored, &source)?;
    validate_restored_command(workspace, &restored, source.command.as_ref())?;

    let status = engine
        .start_command(&request.argv)
        .map_err(|error| format!("start command: {error}"))?;
    if status.at.0 != restored.endpoint.virtual_time
        || status.completion != control_proto::ExecCompletion::Pending
    {
        return Err("command injection changed the stopped moment or completed the command".into());
    }
    let invocation = CommandInvocation {
        engine_id: status.id,
        request_id: request
            .request_id
            .clone()
            .unwrap_or_else(|| format!("exec:{}", workspace.next_moment_id())),
        argv: request.argv.clone(),
    };
    let returned = engine
        .advance(&normalize_advance(&request.bound))
        .map_err(|error| format!("advance command: {error}"))?;
    let captured = capture_verified(engine, returned, workspace.facts(), &source.actions)?;
    branch.history = History::Modified;
    let name = branch.name.clone();
    commit_endpoint(
        workspace,
        branch,
        &name,
        Publication {
            captured,
            invocation: Some(invocation),
        },
        "exec",
        request.request_id.as_deref(),
        request_fingerprint,
    )
}

fn next_probe_name(workspace: &Workspace) -> Result<String, Box<dyn Error>> {
    let mut number = 1_u64;
    loop {
        let name = format!("probe-{number}");
        if workspace.branch(&name).is_none() {
            return Ok(name);
        }
        number = number
            .checked_add(1)
            .ok_or("probe name sequence overflows")?;
    }
}

fn completion(status: control_proto::ExecCompletion) -> CommandCompletion {
    match status {
        control_proto::ExecCompletion::Pending => CommandCompletion::Pending,
        control_proto::ExecCompletion::Exited { status, at } => CommandCompletion::Exited {
            status,
            virtual_time: at.0,
        },
        control_proto::ExecCompletion::Aborted { at } => {
            CommandCompletion::Aborted { virtual_time: at.0 }
        }
    }
}

pub(super) fn validate_pending_branch(
    branch: &Branch,
    command: Option<&CommandRecord>,
) -> Result<(), Box<dyn Error>> {
    match (&branch.pending_command, command) {
        (Some(pending), Some(command)) if matches!(command.completion, CommandCompletion::Pending)
            && pending.engine_id == command.invocation.engine_id
            && pending.request_id == command.invocation.request_id
            && pending.argv == command.invocation.argv && pending.evidence == command.evidence => Ok(()),
        (None, None) => Ok(()),
        (None, Some(command)) if !matches!(command.completion, CommandCompletion::Pending) => Ok(()),
        _ => Err("branch pending command differs from its immutable head; legacy pending state cannot be silently resumed".into()),
    }
}

pub(super) fn validate_restored_command(
    workspace: &Workspace,
    captured: &CapturedEndpoint,
    record: Option<&CommandRecord>,
) -> Result<(), Box<dyn Error>> {
    match (&captured.command, record) {
        (None, None) => Ok(()),
        (Some(status), Some(record)) => {
            if status.id != record.invocation.engine_id
                || completion(status.completion) != record.completion
            {
                return Err(
                    "restored command identity or completion differs from its retained moment"
                        .into(),
                );
            }
            let evidence = workspace
                .evidence(&record.evidence)
                .ok_or("retained command evidence is absent")?;
            let (output, truncated) = bound_evidence(&status.output);
            if evidence.kind != "command"
                || evidence.truncated != (status.truncated || truncated)
                || workspace.read_blob(&evidence.blob)? != output
            {
                return Err("restored command output differs from its retained evidence".into());
            }
            Ok(())
        }
        _ => Err("retained command metadata differs from the physical checkpoint".into()),
    }
}

pub(super) fn publish_command(
    workspace: &Workspace,
    moment: &str,
    captured: &CapturedEndpoint,
    invocation: Option<CommandInvocation>,
    reserved: &mut Vec<String>,
    records: &mut Vec<Record>,
) -> Result<Option<CommandRecord>, Box<dyn Error>> {
    match (&captured.command, invocation) {
        (None, None) => Ok(None),
        (Some(status), Some(invocation)) => {
            if status.id != invocation.engine_id {
                return Err("captured command id differs from invocation".into());
            }
            let (output, truncated) = bound_evidence(&status.output);
            let blob = workspace.store_blob(&output)?;
            let evidence = allocate_evidence_id(workspace, reserved)?;
            records.push(Record::Evidence(Box::new(EvidenceRecord {
                id: evidence.clone(),
                kind: "command".to_owned(),
                moment: moment.to_owned(),
                blob,
                bytes: u64::try_from(output.len())?,
                truncated: status.truncated || truncated,
                precision: "command output and status captured at this exact endpoint".to_owned(),
            })));
            Ok(Some(CommandRecord {
                invocation,
                completion: completion(status.completion),
                evidence,
            }))
        }
        _ => Err("command capture and invocation metadata disagree".into()),
    }
}
