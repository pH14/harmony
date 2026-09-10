// SPDX-License-Identifier: AGPL-3.0-or-later

//! Durable investigation over a recorded fault execution.
//!
//! This module owns the portable fork/run bookkeeping. A [`Continuation`]
//! supplies the guest operations, while the workspace owns durable records and
//! content-addressed evidence. Every guest operation is followed by one
//! complete capture, so a reply and the journal point at the same endpoint.

use std::{
    error::Error,
    path::{Component, Path},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    package::StateHashEncoding,
    retained::RetainedContinuation,
    target::{FaultAction, FaultObservations, FaultStop},
    workspace::{
        Branch, EvidenceRecord, Finding, History, MomentRecord, PendingCommand, Record, Selector,
        StopReason, Workspace, WorkspaceFacts,
    },
};

/// Default virtual-time bound on one run.
pub const DEFAULT_EXEC_NANOS: u64 = 1_000_000_000;
/// Default host watchdog on an advancing command.
pub const DEFAULT_WALL_SECONDS: u64 = 30;
/// Maximum bytes retained for one console or event evidence blob.
pub const EVIDENCE_LIMIT: usize = 256 * 1024;

/// One SDK report the guest published, with its stream position.
pub use crate::package::replay_evidence::SdkEventRecord;

/// What a bounded run may do.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Advance {
    /// Additional virtual nanoseconds this call may run.
    pub within_nanos: u64,
    /// A new guest report to watch for.
    pub until: Option<Condition>,
    /// Whether the call may run past the recorded action list.
    pub extend: bool,
    /// Host seconds the call may spend before it gives up.
    pub wall_seconds: u64,
}

/// A guest property report an advance watches for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Condition {
    /// A new failed evaluation of one property.
    AssertionFail(u32),
    /// A new hit of one property.
    AssertionHit(u32),
}

impl Condition {
    /// Parse `assertion:<id>[:fail|:hit]`.
    ///
    /// A missing disposition retains the historical `fail` default. Extra
    /// fields are rejected so a malformed request cannot silently change its
    /// fingerprint or meaning.
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
        let condition = match fields.next() {
            None | Some("fail") => Self::AssertionFail(id),
            Some("hit") => Self::AssertionHit(id),
            Some(other) => {
                return Err(format!("{other:?} is not a disposition; write fail or hit").into());
            }
        };
        if let Some(extra) = fields.next() {
            return Err(format!(
                "condition has trailing field {extra:?}; write assertion:<id>:fail or assertion:<id>:hit"
            )
            .into());
        }
        Ok(condition)
    }

    /// The property id this condition watches.
    #[must_use]
    pub fn point(self) -> u32 {
        match self {
            Self::AssertionFail(id) | Self::AssertionHit(id) => id,
        }
    }

    /// The canonical selector text for this condition.
    #[must_use]
    pub fn selector(self) -> String {
        match self {
            Self::AssertionFail(id) => format!("assertion:{id}:fail"),
            Self::AssertionHit(id) => format!("assertion:{id}:hit"),
        }
    }
}

/// One endpoint returned by a continuation operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Endpoint {
    /// Virtual time at the endpoint.
    pub virtual_time: u64,
    /// Why the guest stopped.
    pub stop: StopReason,
    /// Observations decoded at the endpoint.
    pub observations: FaultObservations,
    /// Whether the requested condition was observed.
    pub condition_met: bool,
}

/// All state and evidence read from one stopped continuation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedEndpoint {
    /// Endpoint corresponding to every other field in this capture.
    pub endpoint: Endpoint,
    /// Retained `HARMEXEC` checkpoint bytes.
    pub checkpoint: Vec<u8>,
    /// Whole-VM state digest, lowercase hexadecimal.
    pub state_hash: String,
    /// Serial console bytes.
    pub console: Vec<u8>,
    /// SDK reports in stream order.
    pub events: Vec<SdkEventRecord>,
}

/// The live guest operations investigation needs.
pub trait Continuation {
    /// Reproduce the recorded action list to a point at or before its source
    /// finding. `source_moment` is the finding's actual endpoint time.
    fn open_recorded(
        &mut self,
        actions: &[FaultAction],
        source_moment: u64,
        rewind_nanos: u64,
    ) -> Result<Endpoint, String>;

    /// Restore retained checkpoint bytes and preserve the supplied endpoint's
    /// original stop and observations.
    fn restore(
        &mut self,
        checkpoint: &[u8],
        actions: &[FaultAction],
        endpoint: &Endpoint,
    ) -> Result<Endpoint, String>;

    /// Advance the current point under a virtual and host bound.
    fn advance(&mut self, bound: &Advance) -> Result<Endpoint, String>;

    /// Capture checkpoint, state hash, console, and SDK events at one point.
    fn capture(&mut self) -> Result<CapturedEndpoint, String>;
}

/// What `fork` was asked to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForkRequest {
    /// The finding or retained endpoint to fork from.
    pub source: Selector,
    /// Virtual nanoseconds before a recorded finding to start at.
    pub rewind_nanos: u64,
    /// Name of the new branch.
    pub name: String,
    /// Whether the branch is a one-off probe.
    pub probe: bool,
    /// Caller-supplied idempotency key.
    pub request_id: Option<String>,
}

/// What `run` was asked to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunRequest {
    /// Branch to advance.
    pub branch: String,
    /// Virtual/host bound.
    pub bound: Advance,
    /// Caller-supplied idempotency key.
    pub request_id: Option<String>,
}

/// The durable reply shape for fork and run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Outcome {
    /// Operation that produced the reply.
    pub operation: String,
    /// Branch acted on.
    pub branch: String,
    /// Immutable moment id for the endpoint.
    pub moment: String,
    /// Virtual time at the endpoint.
    pub virtual_time: u64,
    /// State hash retained with the endpoint.
    pub state_hash: Option<String>,
    /// Whether the history remains recorded.
    pub history: History,
    /// Why the endpoint stopped.
    pub stop: StopReason,
    /// Whether a watched condition was observed.
    pub condition_met: bool,
    /// Property ids reported violated.
    pub violations: Vec<u32>,
    /// Property ids reported reached.
    pub evaluated: Vec<u32>,
    /// Declared properties without evaluation evidence.
    pub unevaluated: Vec<u32>,
    /// Evidence ids retained by this operation.
    pub evidence: Vec<String>,
    /// Whether evidence was truncated.
    pub truncated: bool,
    /// Whether this reply came from a committed request retry.
    pub replayed_request: bool,
    /// Suggested next read/run commands.
    pub next: Vec<String>,
}

/// Return the committed result for a fork request, if any.
///
/// This read is intentionally separate from [`fork`], allowing a CLI to check
/// idempotency before it constructs any lazy guest/runtime artifacts.
pub fn retry_fork(
    workspace: &Workspace,
    request: &ForkRequest,
) -> Result<Option<Outcome>, Box<dyn Error>> {
    retry_request(
        workspace,
        request.request_id.as_deref(),
        "fork",
        &ForkFingerprintArgs::from_request(request),
    )
}

/// Return the committed result for a run request, if any.
pub fn retry_run(
    workspace: &Workspace,
    request: &RunRequest,
) -> Result<Option<Outcome>, Box<dyn Error>> {
    retry_request(
        workspace,
        request.request_id.as_deref(),
        "run",
        &RunFingerprintArgs::from_request(request),
    )
}

/// Create a branch from a recorded finding or retained endpoint.
pub fn fork(
    workspace: &mut Workspace,
    engine: &mut dyn Continuation,
    request: &ForkRequest,
) -> Result<Outcome, Box<dyn Error>> {
    if let Some(outcome) = retry_fork(workspace, request)? {
        return Ok(outcome);
    }
    if request.name.is_empty() {
        return Err("branch name cannot be empty".into());
    }
    if request.name.contains('@') {
        return Err("branch name cannot contain '@' because it is used in selectors".into());
    }
    if workspace.branch(&request.name).is_some() {
        return Err(format!(
            "branch {:?} already exists; choose another --name or run it with `run {}`",
            request.name, request.name
        )
        .into());
    }

    let request_fingerprint = request
        .request_id
        .as_deref()
        .map(|_| fingerprint("fork", ForkFingerprintArgs::from_request(request)))
        .transpose()?;
    let source = source_details(workspace, &request.source)?;
    if source.retained && request.rewind_nanos != 0 {
        return Err(
            "rewind is only supported from a recorded finding; a retained branch or moment already names an exact endpoint"
                .into(),
        );
    }

    let returned = if source.retained {
        let source_checkpoint = source
            .checkpoint
            .as_deref()
            .ok_or("retained source has no checkpoint bytes")?;
        engine
            .restore(source_checkpoint, &source.actions, &source.endpoint)
            .map_err(|error| format!("restore {}: {error}", source.label))?
    } else {
        engine
            .open_recorded(
                &source.actions,
                source.endpoint.virtual_time,
                request.rewind_nanos,
            )
            .map_err(|error| format!("open {}: {error}", source.label))?
    };
    let captured = capture_verified(engine, returned, workspace.facts(), &source.actions)?;
    if source.retained {
        validate_restored_plan(
            &captured,
            source
                .checkpoint
                .as_deref()
                .ok_or("retained source has no checkpoint bytes")?,
        )?;
    }

    if source.retained || request.rewind_nanos == 0 {
        validate_source_match(&captured, &source)?;
    } else if captured.endpoint.virtual_time > source.endpoint.virtual_time {
        return Err(format!(
            "rewound fork {} stopped at {}, after its source finding at {}",
            source.label, captured.endpoint.virtual_time, source.endpoint.virtual_time
        )
        .into());
    }

    let branch = Branch {
        name: request.name.clone(),
        source: source.label,
        start: String::new(),
        head: String::new(),
        history: source.history,
        continuation_end: source.continuation_end,
        inherited_actions: source.actions,
        probe: request.probe,
        pending_command: None,
    };
    commit_endpoint(
        workspace,
        branch,
        &request.name,
        captured,
        "fork",
        request.request_id.as_deref(),
        request_fingerprint,
    )
}

/// Advance a named branch and commit its endpoint and evidence.
pub fn run(
    workspace: &mut Workspace,
    engine: &mut dyn Continuation,
    request: &RunRequest,
) -> Result<Outcome, Box<dyn Error>> {
    if let Some(outcome) = retry_run(workspace, request)? {
        return Ok(outcome);
    }
    let request_fingerprint = request
        .request_id
        .as_deref()
        .map(|_| fingerprint("run", RunFingerprintArgs::from_request(request)))
        .transpose()?;
    let branch = workspace
        .branch(&request.branch)
        .ok_or_else(|| unknown_branch(workspace, &request.branch))?
        .clone();
    reject_pending(&branch)?;
    let source_moment = workspace
        .moment(&branch.head)
        .ok_or_else(|| {
            format!(
                "branch {} points at missing moment {}",
                branch.name, branch.head
            )
        })?
        .clone();
    let source_endpoint = endpoint_from_moment(&source_moment)?;
    let source_hash = expected_hash_from_moment(&source_moment)?;
    let checkpoint_digest = source_moment
        .checkpoint
        .as_deref()
        .ok_or_else(|| format!("branch {} head has no retained checkpoint", branch.name))?;
    let checkpoint = workspace.read_blob(checkpoint_digest)?;
    let returned = engine
        .restore(&checkpoint, &branch.inherited_actions, &source_endpoint)
        .map_err(|error| format!("restore {}@head: {error}", branch.name))?;
    let restored = capture_verified(
        engine,
        returned,
        workspace.facts(),
        &branch.inherited_actions,
    )?;
    validate_restored_plan(&restored, &checkpoint)?;
    validate_hash_match(&restored.state_hash, &source_hash)?;
    if restored.endpoint != source_endpoint {
        return Err(format!(
            "restored {}@head endpoint does not match its retained moment",
            branch.name
        )
        .into());
    }

    let bound = normalize_advance(&request.bound);
    let returned = engine
        .advance(&bound)
        .map_err(|error| format!("run {}: {error}", branch.name))?;
    let captured = capture_verified(
        engine,
        returned,
        workspace.facts(),
        &branch.inherited_actions,
    )?;
    commit_endpoint(
        workspace,
        branch,
        &request.branch,
        captured,
        "run",
        request.request_id.as_deref(),
        request_fingerprint,
    )
}

/// Write the recorded reproducer and, optionally, investigation evidence.
pub fn export(
    workspace: &Workspace,
    finding: &str,
    destination: &Path,
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
        "state_hash_encoding": found.state_hash_encoding,
        "confirmed": found.confirmed,
    });
    let reproducer_path = destination.join("reproducer.json");
    std::fs::write(&reproducer_path, serde_json::to_vec_pretty(&reproducer)?)?;
    let mut evidence_files = Vec::new();
    if with_evidence {
        let directory = destination.join("evidence");
        std::fs::create_dir_all(&directory)?;
        let records = investigation_evidence(workspace);
        for record in &records {
            validate_export_filename(&record.id)?;
            let bytes = workspace.read_blob(&record.blob)?;
            let name = format!("{}.{}", record.id, evidence_extension(&record.kind));
            std::fs::write(directory.join(&name), &bytes)?;
            evidence_files.push(name);
        }
        let provenance = serde_json::json!({
            "format": "harmony-investigation-evidence-v1",
            "finding": found.id,
            "note": "captured during investigation; histories marked modified carry inputs that are not in the reproducer",
            "records": records,
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

/// What an export's replay establishes about its finding.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExportResult {
    /// The finding exported.
    pub finding: String,
    /// The destination directory.
    pub destination: String,
    /// The recorded reproducer's path.
    pub reproducer: String,
    /// Evidence files written when requested.
    pub evidence: Vec<String>,
    /// The scope of the finding's verification.
    pub verification_scope: String,
}

/// Describe what a finding's confirmation establishes.
#[must_use]
pub fn verification_scope(confirmed: bool) -> String {
    if confirmed {
        "replayed from boot on a fresh session; anchor: setup seal, target: the recorded failure"
            .to_owned()
    } else {
        "no replay reproduced the recorded evidence; this reproducer is unverified".to_owned()
    }
}

fn evidence_extension(kind: &str) -> &'static str {
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

fn validate_export_filename(name: &str) -> Result<(), Box<dyn Error>> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(format!("evidence id {name:?} is not a safe single filename").into());
    }
    Ok(())
}

fn capture_verified(
    engine: &mut dyn Continuation,
    returned: Endpoint,
    facts: &WorkspaceFacts,
    actions: &[FaultAction],
) -> Result<CapturedEndpoint, Box<dyn Error>> {
    let captured = engine
        .capture()
        .map_err(|error| format!("capture: {error}"))?;
    if captured.endpoint != returned {
        return Err("capture endpoint does not match the continuation result".into());
    }
    validate_capture(&captured, facts, actions)?;
    Ok(captured)
}

fn validate_restored_plan(
    captured: &CapturedEndpoint,
    source_checkpoint: &[u8],
) -> Result<(), Box<dyn Error>> {
    let source = RetainedContinuation::decode(source_checkpoint)
        .map_err(|error| format!("source checkpoint is not a valid HARMEXEC envelope: {error}"))?;
    let actual = RetainedContinuation::decode(&captured.checkpoint).map_err(|error| {
        format!("captured checkpoint is not a valid HARMEXEC envelope: {error}")
    })?;
    if actual.windows != source.windows || actual.actions != source.actions {
        return Err("restored checkpoint changed the retained action plan".into());
    }
    if actual.checkpoint.cursor != source.checkpoint.cursor {
        return Err("restored checkpoint changed the retained action cursor".into());
    }
    if actual.checkpoint.image_identity != source.checkpoint.image_identity {
        return Err("restored checkpoint changed the retained image identity".into());
    }
    if actual.checkpoint.setup != source.checkpoint.setup {
        return Err("restored checkpoint changed the retained setup handle".into());
    }
    if actual.checkpoint.at != source.checkpoint.at {
        return Err("restored checkpoint changed the retained endpoint time".into());
    }
    Ok(())
}

fn validate_capture(
    captured: &CapturedEndpoint,
    facts: &WorkspaceFacts,
    actions: &[FaultAction],
) -> Result<(), Box<dyn Error>> {
    let endpoint = &captured.endpoint;
    if endpoint.observations.moment != endpoint.virtual_time {
        return Err(format!(
            "capture observations name moment {}, endpoint is {}",
            endpoint.observations.moment, endpoint.virtual_time
        )
        .into());
    }
    if endpoint.condition_met != matches!(&endpoint.stop, StopReason::ConditionMet) {
        return Err(format!(
            "capture condition_met={} disagrees with stop {}",
            endpoint.condition_met, endpoint.stop
        )
        .into());
    }
    validate_hash(&captured.state_hash)?;
    let mut raw_events = Vec::with_capacity(captured.events.len());
    for (expected, event) in captured.events.iter().enumerate() {
        let expected = u64::try_from(expected)?;
        if event.position != expected {
            return Err(format!(
                "captured SDK event position {}, expected contiguous position {expected}",
                event.position
            )
            .into());
        }
        if event.virtual_time > endpoint.virtual_time {
            return Err(format!(
                "captured SDK event at {} is after endpoint {}",
                event.virtual_time, endpoint.virtual_time
            )
            .into());
        }
        validate_hex_payload(&event.payload)?;
        raw_events.push((event.virtual_time, event.event, decode_hex(&event.payload)?));
    }
    let sdk_capture = crate::target::decode_sdk_events(&raw_events)
        .map_err(|error| format!("captured SDK events are invalid: {error}"))?;
    let observations = FaultObservations::new(
        endpoint.virtual_time,
        &sdk_capture,
        endpoint.observations.stop,
    );
    if observations != endpoint.observations {
        return Err(
            "captured SDK events decode to observations different from the endpoint".into(),
        );
    }

    let retained = RetainedContinuation::decode(&captured.checkpoint)
        .map_err(|error| format!("capture checkpoint is not a valid HARMEXEC envelope: {error}"))?;
    if retained.checkpoint.at != endpoint.virtual_time {
        return Err(format!(
            "capture checkpoint is at {}, endpoint is {}",
            retained.checkpoint.at, endpoint.virtual_time
        )
        .into());
    }
    let expected_windows = crate::target::ActionWindows {
        root_seal: facts.root_seal,
        horizon_nanos: facts.horizon_nanos,
    };
    if retained.windows != expected_windows {
        return Err("capture checkpoint action windows do not match workspace facts".into());
    }
    if retained.actions != actions {
        return Err("capture checkpoint action list does not match the source execution".into());
    }
    Ok(())
}

fn commit_endpoint(
    workspace: &mut Workspace,
    mut branch: Branch,
    branch_name: &str,
    captured: CapturedEndpoint,
    operation: &str,
    request_id: Option<&str>,
    request_fingerprint: Option<String>,
) -> Result<Outcome, Box<dyn Error>> {
    let history = branch.history;
    let moment = workspace.next_moment_id();
    let checkpoint = workspace.store_blob(&captured.checkpoint)?;
    let (console, console_truncated) = bound_evidence(&captured.console);
    let console_blob = workspace.store_blob(&console)?;
    let (events, events_truncated) = bound_event_evidence(&captured.events)?;
    let events_blob = workspace.store_blob(&events)?;
    let mut reserved = Vec::new();
    let console_id = allocate_evidence_id(workspace, &mut reserved)?;
    let events_id = allocate_evidence_id(workspace, &mut reserved)?;
    let evidence = vec![console_id.clone(), events_id.clone()];
    let mut records = vec![
        Record::Evidence(Box::new(EvidenceRecord {
            id: console_id,
            kind: "console".to_owned(),
            moment: moment.clone(),
            blob: console_blob,
            bytes: u64::try_from(console.len())?,
            truncated: console_truncated,
            precision: "captured at the endpoint returned by the continuation".to_owned(),
        })),
        Record::Evidence(Box::new(EvidenceRecord {
            id: events_id,
            kind: "events".to_owned(),
            moment: moment.clone(),
            blob: events_blob,
            bytes: u64::try_from(events.len())?,
            truncated: events_truncated,
            precision: "each report carries its stream position and virtual time".to_owned(),
        })),
    ];
    let truncated = console_truncated || events_truncated;
    records.push(Record::Moment(Box::new(MomentRecord {
        id: moment.clone(),
        branch: Some(branch_name.to_owned()),
        virtual_time: captured.endpoint.virtual_time,
        history,
        checkpoint: Some(checkpoint),
        state_hash: Some(captured.state_hash.clone()),
        state_hash_encoding: StateHashEncoding::EngineDigest,
        stop: Some(captured.endpoint.stop.clone()),
        observations: Some(captured.endpoint.observations.clone()),
    })));
    branch.head = moment.clone();
    branch.start = if branch.start.is_empty() {
        moment.clone()
    } else {
        branch.start
    };
    records.push(Record::Branch(Box::new(branch.clone())));
    let outcome = Outcome {
        operation: operation.to_owned(),
        branch: branch_name.to_owned(),
        moment,
        virtual_time: captured.endpoint.virtual_time,
        state_hash: Some(captured.state_hash),
        history,
        stop: captured.endpoint.stop.clone(),
        condition_met: captured.endpoint.condition_met,
        violations: captured
            .endpoint
            .observations
            .violations
            .iter()
            .copied()
            .collect(),
        evaluated: {
            let mut evaluated = captured.endpoint.observations.sometimes.clone();
            evaluated.extend(&captured.endpoint.observations.violations);
            evaluated.into_iter().collect()
        },
        unevaluated: unevaluated(workspace, &captured.endpoint.observations),
        evidence,
        truncated,
        replayed_request: false,
        next: next_commands(branch_name, &captured.endpoint),
    };
    records.extend(request_record(
        workspace,
        request_id,
        operation,
        request_fingerprint,
        &outcome,
    )?);
    workspace.commit(records)?;
    Ok(outcome)
}

fn validate_source_match(
    captured: &CapturedEndpoint,
    source: &Source,
) -> Result<(), Box<dyn Error>> {
    if captured.endpoint != source.endpoint {
        return Err(format!(
            "captured {} endpoint does not match its source endpoint",
            source.label
        )
        .into());
    }
    let expected = source.state_hash.as_ref().ok_or_else(|| {
        format!(
            "source {} has no state hash for endpoint verification",
            source.label
        )
    })?;
    validate_hash_match(&captured.state_hash, expected)
}

fn validate_hash_match(actual: &str, expected: &ExpectedHash) -> Result<(), Box<dyn Error>> {
    validate_hash(actual)?;
    validate_hash(&expected.value)?;
    let matches = match expected.encoding {
        StateHashEncoding::EngineDigest => actual == expected.value,
        StateHashEncoding::LegacySha256OfDigest => {
            let raw = decode_hex(actual)?;
            hex_digest(&raw) == expected.value
        }
    };
    if !matches {
        return Err(format!(
            "captured state hash {actual} does not match source state hash {} ({:?})",
            expected.value, expected.encoding
        )
        .into());
    }
    Ok(())
}

fn validate_hash(hash: &str) -> Result<(), Box<dyn Error>> {
    if hash.len() != 64
        || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        || hash.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return Err(format!(
            "state hash {hash:?} must be exactly 64 lowercase hexadecimal characters"
        )
        .into());
    }
    Ok(())
}

fn validate_hex_payload(payload: &str) -> Result<(), Box<dyn Error>> {
    if !payload.len().is_multiple_of(2)
        || !payload.bytes().all(|byte| byte.is_ascii_hexdigit())
        || payload.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return Err(format!("SDK payload {payload:?} must be lowercase hexadecimal").into());
    }
    Ok(())
}

fn decode_hex(text: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().chunks_exact(2) {
        let high = char::from(pair[0])
            .to_digit(16)
            .ok_or("invalid hexadecimal digit")?;
        let low = char::from(pair[1])
            .to_digit(16)
            .ok_or("invalid hexadecimal digit")?;
        bytes.push(u8::try_from((high << 4) | low)?);
    }
    Ok(bytes)
}

fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn normalize_advance(bound: &Advance) -> Advance {
    Advance {
        wall_seconds: if bound.wall_seconds == 0 {
            DEFAULT_WALL_SECONDS
        } else {
            bound.wall_seconds
        },
        ..bound.clone()
    }
}

fn bound_evidence(bytes: &[u8]) -> (Vec<u8>, bool) {
    if bytes.len() <= EVIDENCE_LIMIT {
        (bytes.to_vec(), false)
    } else {
        (bytes[bytes.len() - EVIDENCE_LIMIT..].to_vec(), true)
    }
}

fn bound_event_evidence(events: &[SdkEventRecord]) -> Result<(Vec<u8>, bool), Box<dyn Error>> {
    let complete = serde_json::to_vec_pretty(events)?;
    if complete.len() <= EVIDENCE_LIMIT {
        return Ok((complete, false));
    }
    for start in (0..events.len()).rev() {
        let suffix = serde_json::to_vec_pretty(&events[start..])?;
        if suffix.len() <= EVIDENCE_LIMIT {
            return Ok((suffix, true));
        }
    }
    Ok((b"[]".to_vec(), true))
}

fn allocate_evidence_id(
    workspace: &Workspace,
    reserved: &mut Vec<String>,
) -> Result<String, Box<dyn Error>> {
    let mut number = 1_u64;
    loop {
        let id = format!("ev-{number:04}");
        if workspace.evidence(&id).is_none() && !reserved.iter().any(|used| used == &id) {
            reserved.push(id.clone());
            return Ok(id);
        }
        number = number
            .checked_add(1)
            .ok_or("evidence id sequence overflows")?;
    }
}

fn next_commands(branch: &str, endpoint: &Endpoint) -> Vec<String> {
    let mut next = Vec::new();
    if matches!(endpoint.stop, StopReason::ContinuationEnd) {
        next.push(format!("harmony -w W run {branch} --for 1s --extend"));
    } else {
        next.push(format!("harmony -w W run {branch} --for 1s"));
    }
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

fn request_record(
    workspace: &Workspace,
    request_id: Option<&str>,
    operation: &str,
    fingerprint: Option<String>,
    outcome: &Outcome,
) -> Result<Vec<Record>, Box<dyn Error>> {
    let Some(id) = request_id else {
        return Ok(Vec::new());
    };
    let fingerprint = fingerprint.ok_or("request fingerprint was not computed")?;
    Ok(vec![Record::Request(Box::new(
        crate::workspace::RequestRecord {
            id: id.to_owned(),
            operation: operation.to_owned(),
            sequence: workspace
                .sequence()
                .checked_add(1)
                .ok_or("request sequence overflows")?,
            fingerprint: Some(fingerprint),
            result: serde_json::to_value(outcome)?,
        },
    ))])
}

#[derive(Clone, Debug)]
struct ExpectedHash {
    value: String,
    encoding: StateHashEncoding,
}

#[derive(Clone, Debug)]
struct Source {
    actions: Vec<FaultAction>,
    continuation_end: u64,
    label: String,
    history: History,
    endpoint: Endpoint,
    state_hash: Option<ExpectedHash>,
    checkpoint: Option<Vec<u8>>,
    retained: bool,
}

fn source_details(workspace: &Workspace, selector: &Selector) -> Result<Source, Box<dyn Error>> {
    match selector {
        Selector::Finding(id) => {
            let finding = workspace
                .finding(id)
                .ok_or_else(|| unknown_finding(workspace, id))?;
            let moment = workspace.moment(&finding.moment);
            let endpoint = endpoint_from_finding(finding, moment)?;
            let facts = workspace.facts();
            Ok(Source {
                actions: finding.actions.clone(),
                continuation_end: facts.root_seal.saturating_add(
                    facts
                        .horizon_nanos
                        .saturating_mul(finding.actions.len() as u64),
                ),
                label: finding.id.clone(),
                history: History::Recorded,
                endpoint,
                state_hash: expected_hash_from_finding(finding, moment)?,
                checkpoint: None,
                retained: false,
            })
        }
        Selector::BranchHead(name) => {
            let branch = workspace
                .branch(name)
                .ok_or_else(|| unknown_branch(workspace, name))?;
            reject_pending(branch)?;
            source_from_branch_moment(workspace, branch, branch.head.clone())
        }
        Selector::BranchAt(name, at) => {
            let branch = workspace
                .branch(name)
                .ok_or_else(|| unknown_branch(workspace, name))?;
            reject_pending(branch)?;
            let moments: Vec<&MomentRecord> = workspace
                .moments()
                .into_iter()
                .filter(|moment| moment.branch.as_deref() == Some(name.as_str()))
                .filter(|moment| moment.virtual_time == *at)
                .collect();
            let moment = match moments.as_slice() {
                [] => {
                    return Err(format!("branch {name} has no retained point at {at} ns").into());
                }
                [moment] => *moment,
                many => {
                    let ids = many
                        .iter()
                        .map(|moment| moment.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(format!(
                        "branch {name} has multiple retained points at {at} ns: {ids}"
                    )
                    .into());
                }
            };
            source_from_branch_moment(workspace, branch, moment.id.clone())
        }
        Selector::Moment(id) => {
            let moment = workspace
                .moment(id)
                .ok_or_else(|| format!("no moment named {id:?}"))?;
            if let Some(branch_name) = &moment.branch
                && let Some(branch) = workspace.branch(branch_name)
            {
                reject_pending(branch)?;
            }
            let actions = moment
                .branch
                .as_deref()
                .and_then(|branch| workspace.branch(branch))
                .map(|branch| branch.inherited_actions.clone())
                .or_else(|| {
                    workspace
                        .findings()
                        .into_iter()
                        .find(|finding| finding.moment == *id)
                        .map(|finding| finding.actions.clone())
                })
                .ok_or_else(|| format!("moment {id} has no retained action plan"))?;
            let facts = workspace.facts();
            let continuation_end = facts
                .root_seal
                .saturating_add(facts.horizon_nanos.saturating_mul(actions.len() as u64));
            Ok(Source {
                actions,
                continuation_end,
                label: id.clone(),
                history: moment.history,
                endpoint: endpoint_from_moment(moment)?,
                state_hash: Some(expected_hash_from_moment(moment)?),
                checkpoint: Some(checkpoint_bytes(workspace, moment)?),
                retained: true,
            })
        }
    }
}

fn source_from_branch_moment(
    workspace: &Workspace,
    branch: &Branch,
    moment_id: String,
) -> Result<Source, Box<dyn Error>> {
    let moment = workspace.moment(&moment_id).ok_or_else(|| {
        format!(
            "branch {} points at missing moment {moment_id}",
            branch.name
        )
    })?;
    Ok(Source {
        actions: branch.inherited_actions.clone(),
        continuation_end: branch.continuation_end,
        label: format!("{}@{}", branch.name, moment_id),
        history: branch.history,
        endpoint: endpoint_from_moment(moment)?,
        state_hash: Some(expected_hash_from_moment(moment)?),
        checkpoint: Some(checkpoint_bytes(workspace, moment)?),
        retained: true,
    })
}

fn endpoint_from_finding(
    finding: &Finding,
    moment: Option<&MomentRecord>,
) -> Result<Endpoint, Box<dyn Error>> {
    if let Some(moment) = moment {
        if let Some(recorded) = moment.observations.as_ref()
            && recorded != &finding.observations
        {
            return Err(format!(
                "finding {} and moment {} record different endpoint observations",
                finding.id, moment.id
            )
            .into());
        }
        let expected_stop = stop_reason_from_fault(finding.observations.stop);
        if moment.stop.as_ref() != Some(&expected_stop) {
            return Err(format!(
                "finding {} and moment {} record different endpoint stop reasons",
                finding.id, moment.id
            )
            .into());
        }
        let endpoint =
            endpoint_from_moment_with_observations(moment, finding.observations.clone())?;
        return Ok(endpoint);
    }
    Ok(Endpoint {
        virtual_time: finding.observations.moment,
        stop: stop_reason_from_fault(finding.observations.stop),
        observations: finding.observations.clone(),
        condition_met: false,
    })
}

fn endpoint_from_moment(moment: &MomentRecord) -> Result<Endpoint, Box<dyn Error>> {
    let observations = moment
        .observations
        .clone()
        .ok_or_else(|| format!("moment {} has no endpoint observations", moment.id))?;
    endpoint_from_moment_with_observations(moment, observations)
}

fn endpoint_from_moment_with_observations(
    moment: &MomentRecord,
    observations: FaultObservations,
) -> Result<Endpoint, Box<dyn Error>> {
    if observations.moment != moment.virtual_time {
        return Err(format!(
            "moment {} records virtual time {}, observations name {}",
            moment.id, moment.virtual_time, observations.moment
        )
        .into());
    }
    let stop = moment
        .stop
        .clone()
        .ok_or_else(|| format!("moment {} has no stop reason", moment.id))?;
    Ok(Endpoint {
        virtual_time: moment.virtual_time,
        condition_met: matches!(stop, StopReason::ConditionMet),
        stop,
        observations,
    })
}

fn stop_reason_from_fault(stop: FaultStop) -> StopReason {
    match stop {
        FaultStop::Assertion { point } => StopReason::Assertion { point },
        FaultStop::Crash => StopReason::Crash,
        FaultStop::Quiescent => StopReason::Quiescent,
        FaultStop::Deadline | FaultStop::Unexpected => StopReason::VirtualDeadline,
    }
}

fn expected_hash_from_finding(
    finding: &Finding,
    moment: Option<&MomentRecord>,
) -> Result<Option<ExpectedHash>, Box<dyn Error>> {
    let finding_hash = (!finding.state_hash.is_empty()).then(|| ExpectedHash {
        value: finding.state_hash.clone(),
        encoding: finding.state_hash_encoding,
    });
    let moment_hash = moment.and_then(|moment| {
        moment.state_hash.as_ref().map(|value| ExpectedHash {
            value: value.clone(),
            encoding: moment.state_hash_encoding,
        })
    });
    if let (Some(finding_hash), Some(moment_hash)) = (&finding_hash, &moment_hash)
        && (finding_hash.value != moment_hash.value
            || finding_hash.encoding != moment_hash.encoding)
    {
        return Err(format!(
            "finding {} and moment {} record different state hashes",
            finding.id, finding.moment
        )
        .into());
    }
    Ok(moment_hash.or(finding_hash))
}

fn expected_hash_from_moment(moment: &MomentRecord) -> Result<ExpectedHash, Box<dyn Error>> {
    let value = moment.state_hash.clone().ok_or_else(|| {
        format!(
            "moment {} has no state hash for endpoint verification",
            moment.id
        )
    })?;
    Ok(ExpectedHash {
        value,
        encoding: moment.state_hash_encoding,
    })
}

fn checkpoint_bytes(
    workspace: &Workspace,
    moment: &MomentRecord,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let digest = moment
        .checkpoint
        .as_deref()
        .ok_or_else(|| format!("moment {} retained no checkpoint", moment.id))?;
    workspace.read_blob(digest)
}

fn reject_pending(branch: &Branch) -> Result<(), Box<dyn Error>> {
    if let Some(PendingCommand { request_id, .. }) = &branch.pending_command {
        return Err(format!(
            "branch {} has pending command request {}; durable exec continuation is not implemented",
            branch.name, request_id
        )
        .into());
    }
    Ok(())
}

fn unknown_branch(workspace: &Workspace, name: &str) -> Box<dyn Error> {
    let known = workspace
        .branches()
        .iter()
        .map(|branch| branch.name.as_str())
        .collect::<Vec<_>>();
    if known.is_empty() {
        format!("no branch named {name:?}").into()
    } else {
        format!(
            "no branch named {name:?}; this workspace has {}",
            known.join(", ")
        )
        .into()
    }
}

fn unknown_finding(workspace: &Workspace, id: &str) -> Box<dyn Error> {
    let known = workspace
        .findings()
        .iter()
        .map(|finding| finding.id.as_str())
        .collect::<Vec<_>>();
    if known.is_empty() {
        "this workspace recorded no findings".to_owned().into()
    } else {
        format!(
            "no finding named {id:?}; this workspace has {}",
            known.join(", ")
        )
        .into()
    }
}

#[derive(Serialize)]
struct RequestFingerprint<'a, T: Serialize> {
    domain: &'static str,
    operation: &'a str,
    args: T,
}

#[derive(Serialize)]
struct ForkFingerprintArgs {
    source: FingerprintSelector,
    rewind_nanos: u64,
    name: String,
    probe: bool,
}

impl ForkFingerprintArgs {
    fn from_request(request: &ForkRequest) -> Self {
        Self {
            source: FingerprintSelector::from_selector(&request.source),
            rewind_nanos: request.rewind_nanos,
            name: request.name.clone(),
            probe: request.probe,
        }
    }
}

#[derive(Serialize)]
struct RunFingerprintArgs {
    branch: String,
    within_nanos: u64,
    until: Option<FingerprintCondition>,
    extend: bool,
    wall_seconds: u64,
}

impl RunFingerprintArgs {
    fn from_request(request: &RunRequest) -> Self {
        let bound = normalize_advance(&request.bound);
        Self {
            branch: request.branch.clone(),
            within_nanos: bound.within_nanos,
            until: bound.until.map(FingerprintCondition::from_condition),
            extend: bound.extend,
            wall_seconds: bound.wall_seconds,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", content = "value")]
enum FingerprintSelector {
    Finding(String),
    BranchHead(String),
    BranchAt { name: String, at: u64 },
    Moment(String),
}

impl FingerprintSelector {
    fn from_selector(selector: &Selector) -> Self {
        match selector {
            Selector::Finding(value) => Self::Finding(value.clone()),
            Selector::BranchHead(value) => Self::BranchHead(value.clone()),
            Selector::BranchAt(name, at) => Self::BranchAt {
                name: name.clone(),
                at: *at,
            },
            Selector::Moment(value) => Self::Moment(value.clone()),
        }
    }
}

#[derive(Serialize)]
enum FingerprintCondition {
    AssertionFail { point: u32 },
    AssertionHit { point: u32 },
}

impl FingerprintCondition {
    fn from_condition(condition: Condition) -> Self {
        match condition {
            Condition::AssertionFail(point) => Self::AssertionFail { point },
            Condition::AssertionHit(point) => Self::AssertionHit { point },
        }
    }
}

fn fingerprint<T: Serialize>(operation: &str, args: T) -> Result<String, Box<dyn Error>> {
    let value = RequestFingerprint {
        domain: "harmony-request-fingerprint-v1",
        operation,
        args,
    };
    Ok(hex_digest(&serde_json::to_vec(&value)?))
}

fn retry_request<T: Serialize>(
    workspace: &Workspace,
    request_id: Option<&str>,
    operation: &str,
    args: &T,
) -> Result<Option<Outcome>, Box<dyn Error>> {
    let Some(id) = request_id else {
        return Ok(None);
    };
    let expected = fingerprint(operation, args)?;
    let Some(record) = workspace.request(id) else {
        return Ok(None);
    };
    let actual = record
        .fingerprint
        .as_deref()
        .ok_or_else(|| format!("request {id} is a legacy record without a request fingerprint"))?;
    if actual != expected {
        return Err(
            format!("request id {id} was already committed for different arguments").into(),
        );
    }
    if record.operation != operation {
        return Err(format!(
            "request id {id} was committed for operation {}, not {operation}",
            record.operation
        )
        .into());
    }
    let mut outcome: Outcome = serde_json::from_value(record.result.clone())?;
    validate_cached_outcome(workspace, &outcome)?;
    outcome.replayed_request = true;
    Ok(Some(outcome))
}

fn validate_cached_outcome(workspace: &Workspace, outcome: &Outcome) -> Result<(), Box<dyn Error>> {
    let moment = workspace
        .moment(&outcome.moment)
        .ok_or_else(|| format!("request result names missing moment {}", outcome.moment))?;
    if moment.virtual_time != outcome.virtual_time {
        return Err(format!(
            "request result moment {} has time {}, result claims {}",
            moment.id, moment.virtual_time, outcome.virtual_time
        )
        .into());
    }
    if moment.history != outcome.history {
        return Err(format!(
            "request result moment {} has history {}, result claims {}",
            moment.id, moment.history, outcome.history
        )
        .into());
    }
    if moment.state_hash != outcome.state_hash {
        return Err(format!(
            "request result moment {} has a different state hash",
            moment.id
        )
        .into());
    }
    if let Some(state_hash) = moment.state_hash.as_deref() {
        validate_hash(state_hash)?;
    }
    if moment.stop.as_ref() != Some(&outcome.stop) {
        return Err(format!(
            "request result moment {} has a different stop reason",
            moment.id
        )
        .into());
    }
    let observations = moment.observations.as_ref().ok_or_else(|| {
        format!(
            "request result moment {} has no endpoint observations",
            moment.id
        )
    })?;
    if outcome.condition_met != matches!(&outcome.stop, StopReason::ConditionMet) {
        return Err("request result condition_met disagrees with its stop reason".into());
    }
    let violations: Vec<u32> = observations.violations.iter().copied().collect();
    if outcome.violations != violations {
        return Err(format!(
            "request result moment {} has different violation observations",
            moment.id
        )
        .into());
    }
    let mut evaluated_set = observations.sometimes.clone();
    evaluated_set.extend(&observations.violations);
    let evaluated: Vec<u32> = evaluated_set.into_iter().collect();
    if outcome.evaluated != evaluated {
        return Err(format!(
            "request result moment {} has different evaluated observations",
            moment.id
        )
        .into());
    }
    let expected_unevaluated = workspace.facts().declarations.unevaluated(&evaluated);
    if outcome.unevaluated != expected_unevaluated {
        return Err(format!(
            "request result moment {} has different unevaluated declarations",
            moment.id
        )
        .into());
    }
    if moment.branch.as_deref() != Some(outcome.branch.as_str()) {
        return Err(format!(
            "request result moment {} belongs to a different branch",
            moment.id
        )
        .into());
    }
    let mut seen = std::collections::BTreeSet::new();
    for evidence_id in &outcome.evidence {
        if !seen.insert(evidence_id) {
            return Err(format!("request result repeats evidence id {evidence_id}").into());
        }
        let evidence = workspace
            .evidence(evidence_id)
            .ok_or_else(|| format!("request result names missing evidence {evidence_id}"))?;
        if evidence.moment != outcome.moment {
            return Err(format!(
                "request result evidence {evidence_id} belongs to moment {}, not {}",
                evidence.moment, outcome.moment
            )
            .into());
        }
    }
    if outcome.truncated
        != workspace
            .evidence_at(&outcome.moment)
            .into_iter()
            .filter(|evidence| seen.contains(&evidence.id))
            .any(|evidence| evidence.truncated)
    {
        return Err(format!(
            "request result moment {} has inconsistent evidence truncation",
            moment.id
        )
        .into());
    }
    Ok(())
}

/// Convert a fault stop to the workspace stop vocabulary.
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

#[cfg(test)]
mod tests;

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
pub mod live;
