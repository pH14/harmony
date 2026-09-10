// SPDX-License-Identifier: AGPL-3.0-or-later

//! Investigation over a durable workspace: fork a finding, advance a branch by
//! a bounded amount of guest time, run a guest command, and cite what came
//! back.
//!
//! The verbs here own the workload's semantics; the CLI owns presentation.
//! Every verb takes a [`Continuation`] — the live guest — so the same code runs
//! against the Consonance backend on a KVM host and against a deterministic
//! stand-in in tests, and the workspace bookkeeping is exercised everywhere.
//!
//! Two rules shape the bookkeeping. A verb that changes the guest commits its
//! checkpoint, its evidence, its request result, and the branch head in one
//! transaction, so a crash leaves the previous head rather than a branch whose
//! saved state does not exist. A caller-supplied request id makes that commit
//! idempotent: retrying returns the committed result, and a command interrupted
//! before its commit is never silently delivered twice.

use std::error::Error;

use serde::{Deserialize, Serialize};

use crate::target::{FaultAction, FaultObservations, FaultStop};
use crate::workspace::{
    Branch, EvidenceRecord, History, MomentRecord, PendingCommand, Record, Selector, StopReason,
    Workspace,
};

/// Default virtual-time bound on one `exec`.
pub const DEFAULT_EXEC_NANOS: u64 = 1_000_000_000;
/// Default host watchdog on any advancing command. Virtual bounds cannot
/// notice a guest that stops making progress; only host time can.
pub const DEFAULT_WALL_SECONDS: u64 = 30;
/// Evidence bytes one capture retains before it reports truncation.
pub const EVIDENCE_LIMIT: usize = 256 * 1024;

/// One SDK report the guest published, with the stream position that places it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SdkEventRecord {
    /// Position in the guest's event stream. Two events at one virtual moment
    /// are still ordered by this.
    pub position: u64,
    /// Virtual time the guest published it at.
    pub virtual_time: u64,
    /// The event id, whose namespace says whether it is a property report.
    pub event: u32,
    /// The raw payload, lowercase hex.
    pub payload: String,
}

/// What an advance is allowed to do.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Advance {
    /// Additional virtual nanoseconds this call may run. It is added to the
    /// branch's current virtual time, never counted from boot.
    pub within_nanos: u64,
    /// A new guest report to watch for. An old report already in the history
    /// does not satisfy it.
    pub until: Option<Condition>,
    /// Whether the advance may run past the source continuation's recorded end.
    pub extend: bool,
    /// Host seconds the call may spend before it gives up.
    pub wall_seconds: u64,
}

/// A guest report an advance watches for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Condition {
    /// A new failed evaluation of one property.
    AssertionFail(u32),
    /// A new hit of one property.
    AssertionHit(u32),
}

impl Condition {
    /// Read `assertion:2:fail` or `assertion:24:hit`.
    ///
    /// # Errors
    ///
    /// Returns an error naming the malformed part and the accepted shapes.
    pub fn parse(text: &str) -> Result<Self, Box<dyn Error>> {
        let mut fields = text.split(':');
        let kind = fields.next().unwrap_or_default();
        if kind != "assertion" {
            return Err(format!(
                "{text:?} is not a watchable condition; write assertion:<id>:fail or assertion:<id>:hit"
            )
            .into());
        }
        let id = fields
            .next()
            .ok_or("a watched condition needs a property id")?
            .parse::<u32>()
            .map_err(|error| format!("the watched property id is not a u32: {error}"))?;
        match fields.next() {
            Some("fail") | None => Ok(Self::AssertionFail(id)),
            Some("hit") => Ok(Self::AssertionHit(id)),
            Some(other) => Err(format!("{other:?} is not a disposition; write fail or hit").into()),
        }
    }

    /// The property id this condition watches.
    #[must_use]
    pub fn point(self) -> u32 {
        match self {
            Self::AssertionFail(id) | Self::AssertionHit(id) => id,
        }
    }

    /// The text a caller writes to ask for it again.
    #[must_use]
    pub fn selector(self) -> String {
        match self {
            Self::AssertionFail(id) => format!("assertion:{id}:fail"),
            Self::AssertionHit(id) => format!("assertion:{id}:hit"),
        }
    }
}

/// Where an advance left the guest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Endpoint {
    /// Virtual time at the endpoint.
    pub virtual_time: u64,
    /// Why it stopped.
    pub stop: StopReason,
    /// The endpoint's observations.
    pub observations: FaultObservations,
    /// Whether the watched condition was observed. A virtual deadline reached
    /// without it is not a passing property.
    pub condition_met: bool,
}

/// What a guest command did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandOutcome {
    /// Where the advance that carried it left the guest.
    pub endpoint: Endpoint,
    /// Captured output bytes.
    pub output: Vec<u8>,
    /// Whether the command completed within the bound.
    pub completed: bool,
    /// The guest exit status, when completion reported one. Completion alone
    /// does not establish it.
    pub exit_status: Option<i32>,
}

/// The live guest an investigation drives. One implementation runs the
/// Consonance backend; tests supply a deterministic stand-in.
pub trait Continuation {
    /// Restore a recorded execution's endpoint, rewound by `rewind_nanos`
    /// from the finding, inheriting the source's remaining inputs at their
    /// original virtual times.
    ///
    /// # Errors
    ///
    /// Returns an error when the guest cannot reach that point.
    fn open_recorded(
        &mut self,
        actions: &[FaultAction],
        rewind_nanos: u64,
    ) -> Result<Endpoint, String>;

    /// Restore retained checkpoint bytes and make that point current.
    ///
    /// `actions` is the recorded execution the restored point continues, which
    /// a later advance still has to cross window by window.
    ///
    /// # Errors
    ///
    /// Returns an error when the checkpoint does not belong to this execution
    /// identity or cannot be imported.
    fn restore(&mut self, checkpoint: &[u8], actions: &[FaultAction]) -> Result<Endpoint, String>;

    /// Advance the current point under `bound`.
    ///
    /// # Errors
    ///
    /// Returns an error when the guest cannot run.
    fn advance(&mut self, bound: &Advance) -> Result<Endpoint, String>;

    /// Deliver `argv` to the guest and advance until it completes or the bound
    /// expires. Scheduled inputs and guest service requests continue to be
    /// handled while it runs.
    ///
    /// # Errors
    ///
    /// Returns an error when the guest cannot run.
    fn exec(&mut self, argv: &[String], bound: &Advance) -> Result<CommandOutcome, String>;

    /// Export the current point as checkpoint bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the point cannot be exported.
    fn checkpoint(&mut self) -> Result<Vec<u8>, String>;

    /// Serial console bytes captured so far.
    ///
    /// # Errors
    ///
    /// Returns an error when the console cannot be drained.
    fn console(&mut self) -> Result<Vec<u8>, String>;

    /// Guest SDK reports with their stream positions.
    ///
    /// # Errors
    ///
    /// Returns an error when the event stream cannot be read.
    fn events(&mut self) -> Result<Vec<SdkEventRecord>, String>;

    /// The whole-VM state hash of the current point, lowercase hex.
    ///
    /// # Errors
    ///
    /// Returns an error when the state cannot be hashed.
    fn state_hash(&mut self) -> Result<String, String>;

    /// Read guest physical memory.
    ///
    /// # Errors
    ///
    /// Returns an error when the range cannot be read.
    fn read_memory(&mut self, gpa: u64, len: u32) -> Result<Vec<u8>, String>;
}

/// What `fork` was asked for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForkRequest {
    /// The finding or branch point to fork from.
    pub source: Selector,
    /// Virtual nanoseconds before the source point to start at.
    pub rewind_nanos: u64,
    /// The branch name.
    pub name: String,
    /// Whether this is a one-off probe rather than a named investigation.
    pub probe: bool,
    /// The caller's request id, when it supplied one.
    pub request_id: Option<String>,
}

/// What `run` was asked for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunRequest {
    /// The branch to advance.
    pub branch: String,
    /// The bound on this advance.
    pub bound: Advance,
    /// The caller's request id, when it supplied one.
    pub request_id: Option<String>,
}

/// What `exec` was asked for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecRequest {
    /// The branch to run on, or the point to probe from.
    pub target: Selector,
    /// The command, as argv. Shell expressions need an explicit `sh -c`.
    pub argv: Vec<String>,
    /// The bound on the advance that carries it.
    pub bound: Advance,
    /// The caller's request id, when it supplied one.
    pub request_id: Option<String>,
}

/// The versioned result shape every verb returns. Text and JSON render the
/// same value, so they cannot drift.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Outcome {
    /// The verb that produced it.
    pub operation: String,
    /// The branch it acted on.
    pub branch: String,
    /// The immutable moment it ended at.
    pub moment: String,
    /// Virtual time at that moment.
    pub virtual_time: u64,
    /// Whole-VM state hash at that moment, lowercase hex, when it was
    /// computed. A cold continuation is compared against a recorded execution
    /// on this value, so it is part of every reply rather than a separate view.
    pub state_hash: Option<String>,
    /// Whether the history is still the recorded one.
    pub history: History,
    /// Why it stopped.
    pub stop: StopReason,
    /// Whether the watched condition was observed.
    pub condition_met: bool,
    /// Whether a guest command completed, when the verb ran one.
    pub command_completed: Option<bool>,
    /// The guest exit status, when completion reported one.
    pub exit_status: Option<i32>,
    /// Properties the guest reported violated at this point.
    pub violations: Vec<u32>,
    /// Properties the guest reported reaching a verdict on.
    pub evaluated: Vec<u32>,
    /// Declared properties this point holds no evaluation evidence for.
    pub unevaluated: Vec<u32>,
    /// Evidence ids this call retained.
    pub evidence: Vec<String>,
    /// Whether any retained evidence dropped content.
    pub truncated: bool,
    /// Whether the result was replayed from a committed request rather than
    /// run again.
    pub replayed_request: bool,
    /// Commands that are valid next steps from here.
    pub next: Vec<String>,
}

/// Create a branch before a recorded finding, or from a branch point.
///
/// # Errors
///
/// Returns an error when the source does not exist, the name is taken, or the
/// guest cannot reach the requested point.
pub fn fork(
    workspace: &mut Workspace,
    engine: &mut dyn Continuation,
    request: &ForkRequest,
) -> Result<Outcome, Box<dyn Error>> {
    if let Some(replayed) = committed(workspace, request.request_id.as_deref())? {
        return Ok(replayed);
    }
    if workspace.branch(&request.name).is_some() {
        return Err(format!(
            "branch {:?} already exists; choose another --name or run it with `run {}`",
            request.name, request.name
        )
        .into());
    }
    let (actions, source_end, source_label) = source_execution(workspace, &request.source)?;
    let endpoint = match &request.source {
        Selector::Finding(_) => engine
            .open_recorded(&actions, request.rewind_nanos)
            .map_err(|error| format!("open {source_label}: {error}"))?,
        Selector::BranchHead(_) | Selector::BranchAt(_, _) | Selector::Moment(_) => {
            let checkpoint = checkpoint_bytes(workspace, &request.source)?;
            engine
                .restore(&checkpoint, &actions)
                .map_err(|error| format!("restore {source_label}: {error}"))?
        }
    };
    let history = source_history(workspace, &request.source);
    let checkpoint = engine.checkpoint().map_err(|error| error.to_string())?;
    let digest = workspace.store_blob(&checkpoint)?;
    let moment = workspace.next_moment_id();
    let branch = Branch {
        name: request.name.clone(),
        source: source_label,
        start: moment.clone(),
        head: moment.clone(),
        history,
        continuation_end: source_end,
        inherited_actions: actions,
        probe: request.probe,
        pending_command: None,
    };
    let state_hash = engine.state_hash().ok();
    let record = MomentRecord {
        id: moment.clone(),
        branch: Some(request.name.clone()),
        virtual_time: endpoint.virtual_time,
        history,
        checkpoint: Some(digest),
        state_hash: state_hash.clone(),
        stop: Some(endpoint.stop.clone()),
        observations: Some(endpoint.observations.clone()),
    };
    let outcome = Outcome {
        operation: "fork".to_owned(),
        branch: request.name.clone(),
        moment,
        virtual_time: endpoint.virtual_time,
        state_hash,
        history,
        stop: endpoint.stop.clone(),
        condition_met: false,
        command_completed: None,
        exit_status: None,
        violations: endpoint.observations.violations.iter().copied().collect(),
        evaluated: endpoint.observations.sometimes.iter().copied().collect(),
        unevaluated: unevaluated(workspace, &endpoint.observations),
        evidence: Vec::new(),
        truncated: false,
        replayed_request: false,
        next: vec![
            format!("harmony -w W run {} --for 1s", request.name),
            format!(
                "harmony -w W exec {} --within 1s -- sh -c 'command'",
                request.name
            ),
        ],
    };
    let mut records = vec![
        Record::Moment(Box::new(record)),
        Record::Branch(Box::new(branch)),
    ];
    records.extend(request_record(
        workspace,
        request.request_id.as_deref(),
        "fork",
        &outcome,
    )?);
    workspace.commit(records)?;
    Ok(outcome)
}

/// Advance a branch by a bounded amount of guest time and save its endpoint.
///
/// # Errors
///
/// Returns an error when the branch does not exist or the guest cannot run.
pub fn run(
    workspace: &mut Workspace,
    engine: &mut dyn Continuation,
    request: &RunRequest,
) -> Result<Outcome, Box<dyn Error>> {
    if let Some(replayed) = committed(workspace, request.request_id.as_deref())? {
        return Ok(replayed);
    }
    let branch = named_branch(workspace, &request.branch)?.clone();
    let checkpoint = head_checkpoint(workspace, &branch)?;
    engine
        .restore(&checkpoint, &branch.inherited_actions)
        .map_err(|error| format!("restore {}@head: {error}", branch.name))?;
    let bound = bounded(&request.bound, &branch, workspace)?;
    let endpoint = engine
        .advance(&bound)
        .map_err(|error| format!("run {}: {error}", branch.name))?;
    commit_advance(
        workspace,
        engine,
        &branch,
        &endpoint,
        "run",
        request.request_id.as_deref(),
        None,
    )
}

/// Deliver a guest command on a branch, or on a probe created from a point.
///
/// The branch becomes modified: command delivery is outside the recorded
/// reproducer, so the resulting checkpoint is the only way back to this state.
/// The original finding stays reproducible.
///
/// # Errors
///
/// Returns an error when the target does not exist, a command is already
/// pending, or the guest cannot run.
pub fn exec(
    workspace: &mut Workspace,
    engine: &mut dyn Continuation,
    request: &ExecRequest,
) -> Result<Outcome, Box<dyn Error>> {
    if let Some(replayed) = committed(workspace, request.request_id.as_deref())? {
        return Ok(replayed);
    }
    if request.argv.is_empty() {
        return Err("exec needs a command; write -- sh -c 'command' for a shell expression".into());
    }
    let branch = match &request.target {
        Selector::Finding(_) | Selector::Moment(_) | Selector::BranchAt(_, _) => {
            let name = workspace.next_probe_name();
            fork(
                workspace,
                engine,
                &ForkRequest {
                    source: request.target.clone(),
                    rewind_nanos: 0,
                    name: name.clone(),
                    probe: true,
                    request_id: None,
                },
            )?;
            named_branch(workspace, &name)?.clone()
        }
        Selector::BranchHead(name) => {
            let branch = named_branch(workspace, name)?.clone();
            let checkpoint = head_checkpoint(workspace, &branch)?;
            engine
                .restore(&checkpoint, &branch.inherited_actions)
                .map_err(|error| format!("restore {name}@head: {error}"))?;
            branch
        }
    };
    if let Some(pending) = &branch.pending_command {
        return Err(format!(
            "branch {} still has {:?} running from request {}; \
             finish it with `run {} --for 1s` before another exec",
            branch.name,
            pending.argv.join(" "),
            pending.request_id,
            branch.name
        )
        .into());
    }
    let bound = bounded(&request.bound, &branch, workspace)?;
    let outcome = engine
        .exec(&request.argv, &bound)
        .map_err(|error| format!("exec on {}: {error}", branch.name))?;
    let pending = (!outcome.completed).then(|| PendingCommand {
        request_id: request
            .request_id
            .clone()
            .unwrap_or_else(|| "unidentified".to_owned()),
        argv: request.argv.clone(),
        evidence: String::new(),
    });
    let endpoint = outcome.endpoint.clone();
    commit_advance(
        workspace,
        engine,
        &branch,
        &endpoint,
        "exec",
        request.request_id.as_deref(),
        Some((outcome, pending)),
    )
}

/// Write the original recorded reproducer, and separately the investigation
/// evidence, into `destination`.
///
/// # Errors
///
/// Returns an error when the finding does not exist or the files cannot be
/// written.
pub fn export(
    workspace: &Workspace,
    finding: &str,
    destination: &std::path::Path,
    with_evidence: bool,
) -> Result<ExportResult, Box<dyn Error>> {
    let found = workspace
        .finding(finding)
        .ok_or_else(|| unknown_finding(workspace, finding))?;
    std::fs::create_dir_all(destination)?;
    let reproducer = serde_json::json!({
        "format": "harmony-reproducer-v1",
        "finding": found.id,
        "identity": workspace.facts().identity,
        "image": workspace.facts().image,
        "image_sha256": workspace.facts().image_sha256,
        "kernel_sha256": workspace.facts().kernel_sha256,
        "fault_agent_sha256": workspace.facts().fault_agent_sha256,
        "root_seal": workspace.facts().root_seal,
        "horizon_nanos": workspace.facts().horizon_nanos,
        "ram_mib": workspace.facts().ram_mib,
        "knobs": workspace.facts().knobs,
        "actions": found.actions,
        "standing": found.standing,
        "history": History::Recorded,
        "violations": found.violations,
        "state_hash": found.state_hash,
        "confirmed": found.confirmed,
    });
    let reproducer_path = destination.join("reproducer.json");
    std::fs::write(&reproducer_path, serde_json::to_vec_pretty(&reproducer)?)?;
    let mut evidence_files = Vec::new();
    if with_evidence {
        let directory = destination.join("evidence");
        std::fs::create_dir_all(&directory)?;
        for record in investigation_evidence(workspace) {
            let bytes = workspace.read_blob(&record.blob)?;
            let name = format!("{}.{}", record.id, extension(&record.kind));
            std::fs::write(directory.join(&name), &bytes)?;
            evidence_files.push(name);
        }
        let provenance = serde_json::json!({
            "format": "harmony-investigation-evidence-v1",
            "finding": found.id,
            "note": "captured during investigation; histories marked modified \
                     carry inputs that are not in the reproducer",
            "records": investigation_evidence(workspace),
            "branches": workspace.branches(),
            "moments": workspace.moments(),
        });
        std::fs::write(
            directory.join("provenance.json"),
            serde_json::to_vec_pretty(&provenance)?,
        )?;
        evidence_files.push("provenance.json".to_owned());
    }
    Ok(ExportResult {
        finding: found.id.clone(),
        destination: destination.display().to_string(),
        reproducer: reproducer_path.display().to_string(),
        evidence: evidence_files,
        verification_scope: verification_scope(found.confirmed),
    })
}

/// What an export wrote.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExportResult {
    /// The finding exported.
    pub finding: String,
    /// The directory written to.
    pub destination: String,
    /// The recorded reproducer's path.
    pub reproducer: String,
    /// Investigation evidence file names, when evidence was requested.
    pub evidence: Vec<String>,
    /// What the export's verification establishes.
    pub verification_scope: String,
}

/// What a confirmed or unconfirmed finding's replay establishes.
#[must_use]
pub fn verification_scope(confirmed: bool) -> String {
    if confirmed {
        "replayed from boot on a fresh session; anchor: setup seal, target: the recorded failure"
            .to_owned()
    } else {
        "no replay reproduced the recorded evidence; this reproducer is unverified".to_owned()
    }
}

fn extension(kind: &str) -> &'static str {
    match kind {
        "events" => "json",
        "memory" => "bin",
        _ => "txt",
    }
}

fn investigation_evidence(workspace: &Workspace) -> Vec<&EvidenceRecord> {
    workspace
        .moments()
        .into_iter()
        .flat_map(|moment| workspace.evidence_at(&moment.id))
        .collect()
}

/// Commit one advance: the checkpoint, its evidence, the request result, and
/// the branch head, together.
fn commit_advance(
    workspace: &mut Workspace,
    engine: &mut dyn Continuation,
    branch: &Branch,
    endpoint: &Endpoint,
    operation: &str,
    request_id: Option<&str>,
    command: Option<(CommandOutcome, Option<PendingCommand>)>,
) -> Result<Outcome, Box<dyn Error>> {
    let modified = operation == "exec";
    let history = if modified {
        History::Modified
    } else {
        branch.history
    };
    let moment = workspace.next_moment_id();
    let mut records = Vec::new();
    let mut evidence_ids = Vec::new();
    let mut truncated = false;
    let mut evidence_count = 0;

    // Retain the endpoint before any evidence: a checkpoint that fails to
    // export must not leave a branch head pointing at nothing.
    let checkpoint = engine.checkpoint().map_err(|error| error.to_string())?;
    let digest = workspace.store_blob(&checkpoint)?;
    let state_hash = engine.state_hash().ok();

    if let Some((outcome, _)) = &command {
        let (bytes, cut) = bound_evidence(&outcome.output);
        truncated |= cut;
        let blob = workspace.store_blob(&bytes)?;
        let id = numbered_evidence(workspace, evidence_count);
        evidence_count += 1;
        evidence_ids.push(id.clone());
        records.push(Record::Evidence(Box::new(EvidenceRecord {
            id,
            kind: "command".to_owned(),
            moment: moment.clone(),
            blob,
            bytes: bytes.len() as u64,
            truncated: cut,
            precision: "the command's own captured output".to_owned(),
        })));
    }
    if let Ok(console) = engine.console() {
        let (bytes, cut) = bound_evidence(&console);
        truncated |= cut;
        let blob = workspace.store_blob(&bytes)?;
        let id = numbered_evidence(workspace, evidence_count);
        evidence_count += 1;
        evidence_ids.push(id.clone());
        records.push(Record::Evidence(Box::new(EvidenceRecord {
            id,
            kind: "console".to_owned(),
            moment: moment.clone(),
            blob,
            bytes: bytes.len() as u64,
            truncated: cut,
            precision: "drained over the interval this call ran; lines carry no \
                        individual timestamp"
                .to_owned(),
        })));
    }
    if let Ok(events) = engine.events() {
        let bytes = serde_json::to_vec_pretty(&events)?;
        let (bytes, cut) = bound_evidence(&bytes);
        truncated |= cut;
        let blob = workspace.store_blob(&bytes)?;
        let id = numbered_evidence(workspace, evidence_count);
        evidence_ids.push(id.clone());
        records.push(Record::Evidence(Box::new(EvidenceRecord {
            id,
            kind: "events".to_owned(),
            moment: moment.clone(),
            blob,
            bytes: bytes.len() as u64,
            truncated: cut,
            precision: "each report carries its stream position and virtual time".to_owned(),
        })));
    }

    let pending = command
        .as_ref()
        .and_then(|(_, pending)| pending.clone())
        .map(|mut pending| {
            pending.evidence = evidence_ids.first().cloned().unwrap_or_default();
            pending
        });
    let outcome = Outcome {
        operation: operation.to_owned(),
        branch: branch.name.clone(),
        moment: moment.clone(),
        virtual_time: endpoint.virtual_time,
        state_hash: state_hash.clone(),
        history,
        stop: endpoint.stop.clone(),
        condition_met: endpoint.condition_met,
        command_completed: command.as_ref().map(|(outcome, _)| outcome.completed),
        exit_status: command
            .as_ref()
            .and_then(|(outcome, _)| outcome.exit_status),
        violations: endpoint.observations.violations.iter().copied().collect(),
        evaluated: endpoint.observations.sometimes.iter().copied().collect(),
        unevaluated: unevaluated(workspace, &endpoint.observations),
        evidence: evidence_ids,
        truncated,
        replayed_request: false,
        next: next_commands(branch, endpoint, &moment),
    };
    records.push(Record::Moment(Box::new(MomentRecord {
        id: moment.clone(),
        branch: Some(branch.name.clone()),
        virtual_time: endpoint.virtual_time,
        history,
        checkpoint: Some(digest),
        state_hash: state_hash.clone(),
        stop: Some(endpoint.stop.clone()),
        observations: Some(endpoint.observations.clone()),
    })));
    records.push(Record::Branch(Box::new(Branch {
        head: moment,
        history,
        pending_command: pending,
        ..branch.clone()
    })));
    records.extend(request_record(workspace, request_id, operation, &outcome)?);
    workspace.commit(records)?;
    Ok(outcome)
}

fn numbered_evidence(workspace: &Workspace, offset: usize) -> String {
    let base = workspace.next_evidence_id();
    let number: usize = base
        .strip_prefix("ev-")
        .and_then(|digits| digits.parse().ok())
        .unwrap_or(1);
    format!("ev-{:04}", number.saturating_add(offset))
}

fn bound_evidence(bytes: &[u8]) -> (Vec<u8>, bool) {
    if bytes.len() <= EVIDENCE_LIMIT {
        return (bytes.to_vec(), false);
    }
    (bytes[bytes.len() - EVIDENCE_LIMIT..].to_vec(), true)
}

fn next_commands(branch: &Branch, endpoint: &Endpoint, moment: &str) -> Vec<String> {
    let mut next = Vec::new();
    if matches!(endpoint.stop, StopReason::ContinuationEnd) {
        next.push(format!(
            "harmony -w W run {} --for 1s --extend",
            branch.name
        ));
    } else if endpoint.observations.stop.is_continuable() {
        next.push(format!("harmony -w W run {} --for 1s", branch.name));
    }
    next.push(format!(
        "harmony -w W inspect {}@head console --since {moment}",
        branch.name
    ));
    next.push(format!(
        "harmony -w W inspect {}@head events --since {moment}",
        branch.name
    ));
    next
}

fn unevaluated(workspace: &Workspace, observations: &FaultObservations) -> Vec<u32> {
    let evaluated: Vec<u32> = observations
        .sometimes
        .iter()
        .chain(observations.violations.iter())
        .copied()
        .collect();
    workspace.facts().declarations.unevaluated(&evaluated)
}

fn committed(
    workspace: &Workspace,
    request_id: Option<&str>,
) -> Result<Option<Outcome>, Box<dyn Error>> {
    let Some(id) = request_id else {
        return Ok(None);
    };
    let Some(record) = workspace.request(id) else {
        return Ok(None);
    };
    let mut outcome: Outcome = serde_json::from_value(record.result.clone())?;
    outcome.replayed_request = true;
    Ok(Some(outcome))
}

fn request_record(
    workspace: &Workspace,
    request_id: Option<&str>,
    operation: &str,
    outcome: &Outcome,
) -> Result<Vec<Record>, Box<dyn Error>> {
    let Some(id) = request_id else {
        return Ok(Vec::new());
    };
    Ok(vec![Record::Request(Box::new(
        crate::workspace::RequestRecord {
            id: id.to_owned(),
            operation: operation.to_owned(),
            sequence: workspace.sequence().saturating_add(1),
            result: serde_json::to_value(outcome)?,
        },
    ))])
}

fn named_branch<'a>(workspace: &'a Workspace, name: &str) -> Result<&'a Branch, Box<dyn Error>> {
    workspace.branch(name).ok_or_else(|| {
        let known: Vec<&str> = workspace
            .branches()
            .iter()
            .map(|branch| branch.name.as_str())
            .collect();
        if known.is_empty() {
            format!("no branch named {name:?}; create one with `fork <finding> --name {name}`")
                .into()
        } else {
            format!(
                "no branch named {name:?}; this workspace has {}",
                known.join(", ")
            )
            .into()
        }
    })
}

fn unknown_finding(workspace: &Workspace, id: &str) -> Box<dyn Error> {
    let known: Vec<&str> = workspace
        .findings()
        .iter()
        .map(|finding| finding.id.as_str())
        .collect();
    if known.is_empty() {
        "this workspace recorded no findings; `harmony -w W findings` lists what a search left"
            .to_owned()
            .into()
    } else {
        format!(
            "no finding named {id:?}; this workspace has {}",
            known.join(", ")
        )
        .into()
    }
}

/// The recorded execution behind a selector: its action list, the virtual time
/// its continuation ends at, and a label naming the source.
fn source_execution(
    workspace: &Workspace,
    selector: &Selector,
) -> Result<(Vec<FaultAction>, u64, String), Box<dyn Error>> {
    match selector {
        Selector::Finding(id) => {
            let finding = workspace
                .finding(id)
                .ok_or_else(|| unknown_finding(workspace, id))?;
            Ok((
                finding.actions.clone(),
                finding.virtual_time(),
                finding.id.clone(),
            ))
        }
        Selector::BranchHead(name) => {
            let branch = named_branch(workspace, name)?;
            Ok((
                branch.inherited_actions.clone(),
                branch.continuation_end,
                format!("{name}@head"),
            ))
        }
        Selector::BranchAt(name, at) => {
            let branch = named_branch(workspace, name)?;
            Ok((
                branch.inherited_actions.clone(),
                branch.continuation_end,
                format!("{name}@{at}ns"),
            ))
        }
        Selector::Moment(id) => {
            let moment = workspace
                .moment(id)
                .ok_or_else(|| -> Box<dyn Error> { format!("no moment named {id:?}").into() })?;
            let branch = moment
                .branch
                .as_deref()
                .and_then(|name| workspace.branch(name));
            Ok((
                branch
                    .map(|branch| branch.inherited_actions.clone())
                    .unwrap_or_default(),
                branch.map_or(moment.virtual_time, |branch| branch.continuation_end),
                id.clone(),
            ))
        }
    }
}

fn source_history(workspace: &Workspace, selector: &Selector) -> History {
    match selector {
        Selector::Finding(_) => History::Recorded,
        Selector::BranchHead(name) | Selector::BranchAt(name, _) => workspace
            .branch(name)
            .map_or(History::Recorded, |branch| branch.history),
        Selector::Moment(id) => workspace
            .moment(id)
            .map_or(History::Recorded, |moment| moment.history),
    }
}

/// The retained checkpoint a selector names. A moment whose checkpoint was not
/// retained is refused rather than answered with a later state.
fn checkpoint_bytes(workspace: &Workspace, selector: &Selector) -> Result<Vec<u8>, Box<dyn Error>> {
    let moment = match selector {
        Selector::BranchHead(name) => named_branch(workspace, name)?.head.clone(),
        Selector::Moment(id) => id.clone(),
        Selector::BranchAt(name, at) => {
            let branch = named_branch(workspace, name)?;
            workspace
                .moments()
                .into_iter()
                .filter(|moment| moment.branch.as_deref() == Some(branch.name.as_str()))
                .filter(|moment| moment.virtual_time == *at)
                .map(|moment| moment.id.clone())
                .next()
                .ok_or_else(|| -> Box<dyn Error> {
                    format!(
                        "branch {name} has no retained point at {at} ns; \
                         `harmony -w W branches` lists its saved endpoints"
                    )
                    .into()
                })?
        }
        Selector::Finding(id) => workspace
            .finding(id)
            .ok_or_else(|| unknown_finding(workspace, id))?
            .moment
            .clone(),
    };
    let record = workspace
        .moment(&moment)
        .ok_or_else(|| -> Box<dyn Error> { format!("no moment named {moment:?}").into() })?;
    let digest = record
        .checkpoint
        .as_deref()
        .ok_or_else(|| -> Box<dyn Error> {
            format!(
                "moment {moment} retained no checkpoint, so it cannot be restored; \
             fork the recorded finding instead"
            )
            .into()
        })?;
    workspace.read_blob(digest)
}

fn head_checkpoint(workspace: &Workspace, branch: &Branch) -> Result<Vec<u8>, Box<dyn Error>> {
    checkpoint_bytes(workspace, &Selector::Moment(branch.head.clone()))
}

/// Apply the branch's continuation end and the default watchdog to a bound.
fn bounded(
    bound: &Advance,
    branch: &Branch,
    workspace: &Workspace,
) -> Result<Advance, Box<dyn Error>> {
    let head = workspace
        .moment(&branch.head)
        .map_or(0, |moment| moment.virtual_time);
    if !bound.extend && head >= branch.continuation_end {
        return Err(format!(
            "branch {} is already at the recorded continuation's end ({} ns); \
             pass --extend to run past it under its final environment",
            branch.name, branch.continuation_end
        )
        .into());
    }
    Ok(Advance {
        wall_seconds: if bound.wall_seconds == 0 {
            DEFAULT_WALL_SECONDS
        } else {
            bound.wall_seconds
        },
        ..bound.clone()
    })
}

/// The stop a workload observation implies when no other condition intervened.
#[must_use]
pub fn stop_from_observations(stop: FaultStop, deadline_reached: bool) -> StopReason {
    match stop {
        FaultStop::Assertion { point } => StopReason::Assertion { point },
        FaultStop::Crash => StopReason::Crash,
        FaultStop::Quiescent => StopReason::Quiescent,
        FaultStop::Deadline | FaultStop::Unexpected if deadline_reached => {
            StopReason::VirtualDeadline
        }
        _ => StopReason::VirtualDeadline,
    }
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
pub mod live;

#[cfg(test)]
mod tests;
