// SPDX-License-Identifier: AGPL-3.0-or-later
//! Workspace verbs: discover findings, fork a continuation, advance it by a
//! bounded amount of guest time, and cite the evidence.
//!
//! `-w W` opens a directory of durable execution history. It does not attach to
//! a running VM: each command opens the workspace, does bounded work, commits,
//! and exits, and guest time is frozen in between.
//!
//! Text and `--json` render the same value. Every verb builds one JSON document
//! and the text form is that document printed as indented `key: value` lines,
//! so the two cannot report different facts.

use std::{error::Error, path::PathBuf, process::ExitCode};

use faults_workload::investigate::{
    Advance, Condition, DEFAULT_WALL_SECONDS, ForkRequest, RunRequest,
};
use faults_workload::workspace::{Selector, Workspace, format_duration, parse_duration};

/// Where the workspace lives and how results are printed.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Common {
    /// The workspace directory a search wrote.
    #[arg(short = 'w', long = "workspace", global = true, value_name = "DIR")]
    pub workspace: Option<PathBuf>,
    /// Emit the versioned JSON result instead of text.
    #[arg(long, global = true)]
    pub json: bool,
    /// The controlled guest kernel to boot, when it is not installed beside
    /// this binary or named by HARMONY_GUEST_DIR.
    #[arg(long, global = true, value_name = "PATH")]
    pub kernel: Option<PathBuf>,
    /// The guest base image the workload is staged into.
    #[arg(long, global = true, value_name = "PATH")]
    pub base_initramfs: Option<PathBuf>,
    /// The guest fault agent the workload image supervises with.
    #[arg(long, global = true, value_name = "PATH")]
    pub fault_agent: Option<PathBuf>,
}

/// One investigation verb.
#[derive(clap::Subcommand)]
pub enum Command {
    /// List findings, their properties, and their verification results.
    Findings,
    /// List named continuations and their saved endpoints.
    Branches,
    /// Describe the workspace, a finding, or a retained view of a point.
    Inspect(InspectArgs),
    /// Create a continuation before a finding and name its starting moment.
    Fork(ForkArgs),
    /// Advance a branch by a bounded amount of guest time and save its endpoint.
    Run(RunArgs),
    /// Write the recorded reproducer, and separately the investigation evidence.
    Export(ExportArgs),
}

/// What to describe and which retained view to read.
#[derive(clap::Args)]
pub struct InspectArgs {
    /// A finding (`bug-1`), a branch point (`trace@head`, `trace@12.3s`), or a
    /// moment (`m-0004`). Omitted, the workspace itself is described.
    pub target: Option<String>,
    /// `console`, `events`, `command`, or `hash`. Omitted, the point is
    /// summarized.
    pub view: Option<String>,
    /// Start reading at this immutable moment.
    #[arg(long, value_name = "MOMENT")]
    pub since: Option<String>,
    /// Most lines or reports to print.
    #[arg(long, default_value_t = 200)]
    pub limit: usize,
    /// Skip this many lines or reports first, so a bounded view can be paged.
    #[arg(long, default_value_t = 0)]
    pub offset: usize,
    /// Keep only console lines containing this text.
    #[arg(long, value_name = "TEXT")]
    pub matches: Option<String>,
}

/// Where a continuation starts and what it is called.
#[derive(clap::Args)]
pub struct ForkArgs {
    /// The finding or point to fork from.
    pub source: String,
    /// Virtual time before the source point to start at, such as `3s`.
    #[arg(long, default_value = "0s")]
    pub rewind: String,
    /// The branch name.
    #[arg(long)]
    pub name: String,
    /// A caller id that makes a retry return this call's committed result.
    #[arg(long, value_name = "ID")]
    pub request_id: Option<String>,
}

/// How far to advance a branch and what to watch for.
#[derive(clap::Args)]
pub struct RunArgs {
    /// The branch to advance.
    pub branch: String,
    /// Additional virtual time to run, such as `4s`.
    #[arg(long = "for", value_name = "DURATION")]
    pub run_for: Option<String>,
    /// A new guest report to watch for, such as `assertion:2:fail`.
    #[arg(long, value_name = "CONDITION")]
    pub until: Option<String>,
    /// Additional virtual time to wait for the condition.
    #[arg(long, value_name = "DURATION")]
    pub within: Option<String>,
    /// Allow running past the recorded continuation's end.
    #[arg(long)]
    pub extend: bool,
    /// Host seconds allowed for advancing the guest before it gives up.
    #[arg(long, default_value_t = DEFAULT_WALL_SECONDS)]
    pub wall_seconds: u64,
    /// A caller id that makes a retry return this call's committed result.
    #[arg(long, value_name = "ID")]
    pub request_id: Option<String>,
}

/// Where to write an export and whether to include evidence.
#[derive(clap::Args)]
pub struct ExportArgs {
    /// The finding to export.
    pub finding: String,
    /// The destination directory.
    #[arg(long)]
    pub out: PathBuf,
    /// Also write the investigation evidence, with its own provenance.
    #[arg(long)]
    pub evidence: bool,
}

/// Run one investigation verb.
///
/// # Errors
///
/// Returns an error naming the missing workspace, the unknown selector, or the
/// guest failure, with the next valid command where one exists.
pub fn run(common: &Common, command: Command) -> Result<ExitCode, Box<dyn Error>> {
    let root = common
        .workspace
        .clone()
        .ok_or("this command needs a workspace: pass -w DIR, the --out of a search")?;
    let mut workspace = Workspace::open(&root)?;
    let value = match command {
        Command::Findings => findings(&workspace),
        Command::Branches => branches(&workspace),
        Command::Inspect(args) => inspect(&workspace, &args)?,
        Command::Fork(args) => {
            let request = ForkRequest {
                source: Selector::parse(&args.source)?,
                rewind_nanos: parse_duration(&args.rewind)?,
                name: args.name.clone(),
                probe: false,
                request_id: args.request_id.clone(),
            };
            match faults_workload::investigate::retry_fork(&workspace, &request)? {
                Some(outcome) => serde_json::to_value(outcome)?,
                None => {
                    let mut engine = guest(&workspace, common)?;
                    serde_json::to_value(faults_workload::investigate::fork(
                        &mut workspace,
                        engine.as_mut(),
                        &request,
                    )?)?
                }
            }
        }
        Command::Run(args) => {
            let request = RunRequest {
                branch: args.branch.clone(),
                bound: run_bound(&args)?,
                request_id: args.request_id.clone(),
            };
            match faults_workload::investigate::retry_run(&workspace, &request)? {
                Some(outcome) => serde_json::to_value(outcome)?,
                None => {
                    let mut engine = guest(&workspace, common)?;
                    serde_json::to_value(faults_workload::investigate::run(
                        &mut workspace,
                        engine.as_mut(),
                        &request,
                    )?)?
                }
            }
        }
        Command::Export(args) => serde_json::to_value(faults_workload::investigate::export(
            &workspace,
            &args.finding,
            &args.out,
            args.evidence,
        )?)?,
    };
    let mut value = value;
    humanize(&mut value);
    print!("{}", render(&value, common.json));
    Ok(ExitCode::SUCCESS)
}

fn run_bound(args: &RunArgs) -> Result<Advance, Box<dyn Error>> {
    let within = match (&args.run_for, &args.within) {
        (Some(_), Some(_)) => {
            return Err(
                "pass --for to advance guest time or --within to bound a wait, not both".into(),
            );
        }
        (Some(text), None) | (None, Some(text)) => parse_duration(text)?,
        (None, None) => {
            return Err("run needs a bound: --for 4s advances guest time, \
                        --until assertion:2:fail --within 4s watches for a report"
                .into());
        }
    };
    Ok(Advance {
        within_nanos: within,
        until: args.until.as_deref().map(Condition::parse).transpose()?,
        extend: args.extend,
        wall_seconds: args.wall_seconds,
    })
}

fn findings(workspace: &Workspace) -> serde_json::Value {
    let declarations = &workspace.facts().declarations;
    let findings: Vec<serde_json::Value> = workspace
        .findings()
        .into_iter()
        .map(|finding| {
            serde_json::json!({
                "finding": finding.id,
                "execution": finding.execution,
                "violations": finding.violations,
                "meanings": finding
                    .violations
                    .iter()
                    .map(|id| declarations
                        .assertion(*id)
                        .map_or_else(
                            || format!("{id}: no meaning is declared in the workload bundle"),
                            |check| format!("{id}: {}", check.meaning),
                        ))
                    .collect::<Vec<_>>(),
                "virtual_time": finding.virtual_time(),
                "moment": finding.moment,
                "actions": finding.actions.len(),
                "verified": finding.confirmed,
                "verification_scope":
                    faults_workload::investigate::verification_scope(finding.confirmed),
            })
        })
        .collect();
    serde_json::json!({
        "format": "harmony-findings-v1",
        "workspace": workspace.root().display().to_string(),
        "findings": findings,
        "next": next_after_findings(workspace),
    })
}

fn next_after_findings(workspace: &Workspace) -> Vec<String> {
    workspace.findings().first().map_or_else(
        || {
            vec![
                "this campaign recorded no findings; a bounded campaign without one \
                 is not proof of correctness"
                    .to_owned(),
            ]
        },
        |finding| {
            vec![
                format!("harmony -w W inspect {}", finding.id),
                format!("harmony -w W fork {} --rewind 3s --name trace", finding.id),
            ]
        },
    )
}

fn branches(workspace: &Workspace) -> serde_json::Value {
    let branches: Vec<serde_json::Value> = workspace
        .branches()
        .into_iter()
        .map(|branch| {
            let head = workspace.moment(&branch.head);
            serde_json::json!({
                "branch": branch.name,
                "source": branch.source,
                "start": branch.start,
                "head": branch.head,
                "virtual_time": head.map_or(0, |moment| moment.virtual_time),
                "continuation_end": format_duration(branch.continuation_end),
                "history": branch.history,
                "probe": branch.probe,
                "pending_command": branch
                    .pending_command
                    .as_ref()
                    .map(|pending| pending.argv.join(" ")),
            })
        })
        .collect();
    serde_json::json!({
        "format": "harmony-branches-v1",
        "workspace": workspace.root().display().to_string(),
        "branches": branches,
    })
}

fn inspect(workspace: &Workspace, args: &InspectArgs) -> Result<serde_json::Value, Box<dyn Error>> {
    let Some(target) = args.target.as_deref() else {
        return Ok(describe_workspace(workspace));
    };
    let selector = Selector::parse(target)?;
    let moment = resolve_moment(workspace, &selector)?;
    match args.view.as_deref() {
        None => Ok(describe_point(workspace, &selector, &moment)),
        Some("hash") => Ok(serde_json::json!({
            "format": "harmony-inspect-hash-v1",
            "moment": moment,
            "state_hash": workspace
                .moment(&moment)
                .and_then(|record| record.state_hash.clone()),
            "state_hash_encoding": workspace
                .moment(&moment)
                .map(|record| record.state_hash_encoding),
        })),
        Some(view @ ("console" | "events" | "command")) => {
            read_evidence(workspace, &moment, view, args)
        }
        Some(other) => Err(format!(
            "{other:?} is not a view; this build reads console, events, command, and hash"
        )
        .into()),
    }
}

fn describe_workspace(workspace: &Workspace) -> serde_json::Value {
    let facts = workspace.facts();
    serde_json::json!({
        "format": "harmony-workspace-summary-v1",
        "workspace": workspace.root().display().to_string(),
        "package": facts.package,
        "identity": facts.identity,
        "image": facts.image,
        "image_sha256": facts.image_sha256,
        "kernel_sha256": facts.kernel_sha256,
        "fault_agent_sha256": facts.fault_agent_sha256,
        "seed": facts.seed,
        "executions": facts.executions,
        "horizon": format_duration(facts.horizon_nanos),
        "ram_mib": facts.ram_mib,
        "knobs": facts.knobs,
        "findings": workspace.findings().len(),
        "branches": workspace.branches().len(),
        "nodes": facts.declarations.nodes,
        "hooks": facts.declarations.hooks,
        "properties": facts.declarations.assertions,
        "diagnostics": facts.declarations.diagnostics,
        "next": ["harmony -w W findings", "harmony -w W branches"],
    })
}

fn describe_point(workspace: &Workspace, selector: &Selector, moment: &str) -> serde_json::Value {
    let declarations = &workspace.facts().declarations;
    let record = workspace.moment(moment);
    let finding = match selector {
        Selector::Finding(id) => workspace.finding(id),
        _ => None,
    };
    let violations: Vec<u32> = finding.map_or_else(
        || {
            record
                .and_then(|record| record.observations.as_ref())
                .map(|observations| observations.violations.iter().copied().collect())
                .unwrap_or_default()
        },
        |finding| finding.violations.clone(),
    );
    let properties: Vec<serde_json::Value> = violations
        .iter()
        .map(|id| {
            let declared = declarations.assertion(*id);
            serde_json::json!({
                "assertion": id,
                "meaning": declared.map(|check| check.meaning.clone()),
                "kind": declared.map(|check| check.kind),
                "reported_by_hook": declared.and_then(|check| check.reported_by_hook),
                "hook_command": declared
                    .and_then(|check| check.reported_by_hook)
                    .and_then(|hook| declarations.hook(hook))
                    .map(|hook| hook.argv.join(" ")),
                "result": "failed; the checker reached a verdict",
            })
        })
        .collect();
    let evaluated: Vec<u32> = finding
        .map(|finding| finding.evaluated.clone())
        .unwrap_or_default();
    serde_json::json!({
        "format": "harmony-inspect-v1",
        "target": selector.to_string(),
        "moment": moment,
        "virtual_time": record.map_or(0, |record| record.virtual_time),
        "history": record.map(|record| record.history),
        "stop": record.and_then(|record| record.stop.clone()),
        "properties": properties,
        "unevaluated": declarations.unevaluated(&evaluated),
        "unevaluated_means": "a declared property with no evaluation evidence here; \
                              silence from a failure-only check is not a pass",
        "preceding_inputs": finding.map(|finding| finding.actions.clone()),
        "verified": finding.map(|finding| finding.confirmed),
        "verification_scope": finding
            .map(|finding| faults_workload::investigate::verification_scope(finding.confirmed)),
        "state_hash": record.and_then(|record| record.state_hash.clone()),
        "state_hash_encoding": record.map(|record| record.state_hash_encoding),
        "evidence": workspace
            .evidence_at(moment)
            .into_iter()
            .map(|record| {
                serde_json::json!({
                    "evidence": record.id,
                    "kind": record.kind,
                    "bytes": record.bytes,
                    "truncated": record.truncated,
                    "precision": record.precision,
                })
            })
            .collect::<Vec<_>>(),
        "diagnostics": declarations.diagnostics,
        "next": next_from_point(workspace, selector),
    })
}

fn next_from_point(workspace: &Workspace, selector: &Selector) -> Vec<String> {
    match selector {
        Selector::Finding(id) => vec![
            format!("harmony -w W fork {id} --rewind 3s --name trace"),
            format!("harmony -w W export {id} --out shared --evidence"),
        ],
        Selector::BranchHead(name) | Selector::BranchAt(name, _) => vec![
            format!("harmony -w W run {name} --for 1s"),
            format!("harmony -w W inspect {name}@head console"),
        ],
        Selector::Moment(id) => workspace
            .moment(id)
            .and_then(|record| record.branch.clone())
            .map_or_else(Vec::new, |branch| {
                vec![format!(
                    "harmony -w W inspect {branch}@head events --since {id}"
                )]
            }),
    }
}

fn read_evidence(
    workspace: &Workspace,
    moment: &str,
    view: &str,
    args: &InspectArgs,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let start = args.since.as_deref().unwrap_or(moment);
    let record = workspace
        .evidence_at(start)
        .into_iter()
        .find(|record| record.kind == view)
        .ok_or_else(|| -> Box<dyn Error> {
            format!(
                "no {view} evidence was retained at {start}; \
                 `harmony -w W inspect {start}` lists what this point has"
            )
            .into()
        })?;
    let bytes = workspace.read_blob(&record.blob)?;
    let text = String::from_utf8_lossy(&bytes);
    let matched: Vec<&str> = text
        .lines()
        .filter(|line| {
            args.matches
                .as_deref()
                .is_none_or(|needle| line.contains(needle))
        })
        .collect();
    let kept: Vec<&str> = matched
        .iter()
        .skip(args.offset)
        .take(args.limit)
        .copied()
        .collect();
    let shown = kept.len();
    Ok(serde_json::json!({
        "format": "harmony-inspect-evidence-v1",
        "moment": start,
        "view": view,
        "evidence": record.id,
        "precision": record.precision,
        "lines": kept,
        "shown": shown,
        "matched": matched.len(),
        "offset": args.offset,
        "truncated": record.truncated
            || shown < matched.len().saturating_sub(args.offset),
        "next": (shown == args.limit).then(|| {
            format!(
                "harmony -w W inspect {start} {view} --offset {} --limit {}",
                args.offset.saturating_add(args.limit),
                args.limit
            )
        }),
    }))
}

/// The immutable moment a selector names. A selector that resolves to nothing
/// is refused rather than answered with a nearby point.
fn resolve_moment(workspace: &Workspace, selector: &Selector) -> Result<String, Box<dyn Error>> {
    match selector {
        Selector::Finding(id) => workspace
            .finding(id)
            .map(|finding| finding.moment.clone())
            .ok_or_else(|| format!("no finding named {id:?}").into()),
        Selector::Moment(id) => workspace
            .moment(id)
            .map(|record| record.id.clone())
            .ok_or_else(|| format!("no moment named {id:?}").into()),
        Selector::BranchHead(name) => workspace
            .branch(name)
            .map(|branch| branch.head.clone())
            .ok_or_else(|| format!("no branch named {name:?}").into()),
        Selector::BranchAt(name, at) => branch_moment(workspace, name, *at),
    }
}

/// Resolve an absolute branch time without silently choosing among duplicate
/// retained moments. A selector must name one immutable point before a
/// mutation or read can use it.
fn branch_moment(workspace: &Workspace, name: &str, at: u64) -> Result<String, Box<dyn Error>> {
    let branch = workspace
        .branch(name)
        .ok_or_else(|| -> Box<dyn Error> { format!("no branch named {name:?}").into() })?;
    let matches: Vec<String> = workspace
        .moments()
        .into_iter()
        .filter(|record| record.branch.as_deref() == Some(branch.name.as_str()))
        .filter(|record| record.virtual_time == at)
        .map(|record| record.id.clone())
        .collect();
    match matches.as_slice() {
        [] => Err(format!(
            "branch {name} has no retained point at {}; \
             `harmony -w W branches` lists its saved endpoints",
            format_duration(at)
        )
        .into()),
        [moment] => Ok(moment.clone()),
        _ => Err(format!(
            "branch {name} has multiple retained points at {} ns: {}; \
             select an explicit moment id",
            at,
            matches.join(", ")
        )
        .into()),
    }
}

/// Boot the guest this workspace's verbs advance.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn guest(
    workspace: &Workspace,
    common: &Common,
) -> Result<Box<dyn faults_workload::investigate::Continuation>, Box<dyn Error>> {
    let facts = workspace.facts();
    let artifacts = crate::search::prepared_artifacts(
        &facts.image,
        facts.image_sha256.as_str(),
        facts.kernel_sha256.as_str(),
        facts.fault_agent_sha256.as_str(),
        common.kernel.clone(),
        common.base_initramfs.clone(),
        common.fault_agent.clone(),
    )?;
    let config = faults_workload::consonance::FaultConfig {
        knobs: facts.knobs.clone(),
        horizon_nanos: facts.horizon_nanos,
        ram_mib: facts.ram_mib,
    };
    Ok(Box::new(
        faults_workload::investigate::live::ConsonanceGuest::new(
            artifacts.kernel,
            artifacts.initramfs,
            config,
        ),
    ))
}

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
fn guest(
    _workspace: &Workspace,
    _common: &Common,
) -> Result<Box<dyn faults_workload::investigate::Continuation>, Box<dyn Error>> {
    Err("advancing a workspace needs a Linux KVM host; \
         findings, branches, inspect, and export read the retained history anywhere"
        .into())
}

/// Add a human-readable duration beside every numeric virtual time. The
/// machine-readable `virtual_time` field remains numeric in JSON and text;
/// consumers can use `virtual_time_nanos` without parsing display text.
fn humanize(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            if let Some(nanos) = fields
                .get("virtual_time")
                .and_then(serde_json::Value::as_u64)
            {
                fields.insert("virtual_time_nanos".to_owned(), nanos.into());
                fields.insert(
                    "virtual_time_duration".to_owned(),
                    format_duration(nanos).into(),
                );
            }
            for field in fields.values_mut() {
                humanize(field);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                humanize(item);
            }
        }
        _ => {}
    }
}

/// Render one result as indented text or as its versioned JSON.
///
/// Both renderings come from the same document, so the two carry the same
/// facts by construction rather than by matching two writers.
fn render(value: &serde_json::Value, json: bool) -> String {
    if json {
        let mut text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
        text.push('\n');
        return text;
    }
    let mut out = String::new();
    write_text(value, 0, &mut out);
    out
}

/// Render the same document as text: one `key: value` line per fact, nested
/// values indented under their key.
fn write_text(value: &serde_json::Value, depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth);
    match value {
        serde_json::Value::Object(fields) => {
            for (key, field) in fields {
                if field.is_null() {
                    continue;
                }
                if is_scalar(field) {
                    out.push_str(&format!("{pad}{key}: {}\n", scalar(field)));
                } else if field.as_array().is_some_and(Vec::is_empty) {
                    continue;
                } else {
                    out.push_str(&format!("{pad}{key}:\n"));
                    write_text(field, depth + 1, out);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                if is_scalar(item) {
                    out.push_str(&format!("{pad}- {}\n", scalar(item)));
                } else {
                    out.push_str(&format!("{pad}-\n"));
                    write_text(item, depth + 1, out);
                }
            }
        }
        other => out.push_str(&format!("{pad}{}\n", scalar(other))),
    }
}

fn is_scalar(value: &serde_json::Value) -> bool {
    !value.is_object() && !value.is_array()
}

fn scalar(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), ToOwned::to_owned)
}

#[cfg(test)]
mod tests;
