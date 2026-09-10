// SPDX-License-Identifier: AGPL-3.0-or-later

//! Portable investigation bookkeeping tests.
//!
//! The fake continuation checks the durable boundary around a guest operation.
//! It does not claim anything about KVM execution; live execution is covered by
//! the Linux adapter and backend tests.

use super::*;
use crate::{
    checkpoint::Checkpoint,
    declarations::Declarations,
    execution::ActionCursor,
    package::StateHashEncoding,
    retained::RetainedContinuation,
    target::{ActionWindows, FaultAction, decode_sdk_events},
    workspace::{FORMAT, Finding, MomentRecord, Record, WorkspaceFacts},
};
use std::collections::VecDeque;

const ROOT_SEAL: u64 = 1_000;
const HORIZON: u64 = 100;
const SOURCE_AT: u64 = ROOT_SEAL + 4 * HORIZON;
const NEXT_AT: u64 = SOURCE_AT + 50;

fn actions() -> Vec<FaultAction> {
    vec![
        FaultAction::Wait,
        FaultAction::Hook(3),
        FaultAction::Kill(0),
        FaultAction::Wait,
    ]
}

fn windows() -> ActionWindows {
    ActionWindows {
        root_seal: ROOT_SEAL,
        horizon_nanos: HORIZON,
    }
}

fn declarations() -> Declarations {
    Declarations::parse(
        "node service /opt/harmony/service\n\
         hook 3 /opt/harmony/check\n\
         assert sometimes 7 the checker reached a verdict\n",
    )
    .expect("valid declarations")
}

fn facts() -> WorkspaceFacts {
    WorkspaceFacts {
        format: FORMAT.to_owned(),
        package: crate::package::PACKAGE.to_owned(),
        identity: "investigate-test-v1".to_owned(),
        image: "test.oci".to_owned(),
        root_seal: ROOT_SEAL,
        horizon_nanos: HORIZON,
        ram_mib: 64,
        declarations: declarations(),
        ..WorkspaceFacts::default()
    }
}

fn endpoint(at: u64) -> Endpoint {
    Endpoint {
        virtual_time: at,
        stop: StopReason::VirtualDeadline,
        observations: FaultObservations {
            moment: at,
            ..FaultObservations::default()
        },
        condition_met: false,
    }
}

fn hash(at: u64) -> String {
    format!("{at:064x}")
}

fn checkpoint_with_cursor(at: u64, cursor: ActionCursor) -> Vec<u8> {
    RetainedContinuation {
        checkpoint: Checkpoint {
            setup: ROOT_SEAL,
            at,
            image_identity: [0x5a; 32],
            pages: Vec::new(),
            sidecar: Vec::new(),
            cursor: Some(cursor),
        },
        windows: windows(),
        actions: actions(),
    }
    .encode()
    .expect("valid retained checkpoint")
}

fn checkpoint(at: u64) -> Vec<u8> {
    checkpoint_with_cursor(at, ActionCursor::default())
}

fn captured(at: u64) -> CapturedEndpoint {
    CapturedEndpoint {
        endpoint: endpoint(at),
        checkpoint: checkpoint(at),
        state_hash: hash(at),
        console: format!("endpoint {at}\n").into_bytes(),
        events: Vec::new(),
    }
}

fn workspace() -> (tempfile::TempDir, Workspace) {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let mut workspace = Workspace::create(directory.path(), facts()).expect("workspace facts");
    let source_endpoint = endpoint(SOURCE_AT);
    workspace
        .commit(vec![
            Record::Moment(Box::new(MomentRecord {
                id: "m-0001".to_owned(),
                virtual_time: SOURCE_AT,
                history: History::Recorded,
                state_hash: Some(hash(SOURCE_AT)),
                state_hash_encoding: StateHashEncoding::EngineDigest,
                stop: Some(source_endpoint.stop.clone()),
                observations: Some(source_endpoint.observations.clone()),
                ..MomentRecord::default()
            })),
            Record::Finding(Box::new(Finding {
                id: "bug-1".to_owned(),
                execution: 11,
                actions: actions(),
                standing: String::new(),
                observations: source_endpoint.observations,
                moment: "m-0001".to_owned(),
                confirmed: true,
                state_hash: hash(SOURCE_AT),
                state_hash_encoding: StateHashEncoding::EngineDigest,
                ..Finding::default()
            })),
        ])
        .expect("source records");
    (directory, workspace)
}

#[derive(Debug)]
struct FakeContinuation {
    open_result: Result<Endpoint, String>,
    restore_result: Result<Endpoint, String>,
    advance_result: Result<Endpoint, String>,
    captures: VecDeque<Result<CapturedEndpoint, String>>,
    open_calls: usize,
    restore_calls: usize,
    advance_calls: usize,
    capture_calls: usize,
    bounds: Vec<Advance>,
}

impl Default for FakeContinuation {
    fn default() -> Self {
        Self {
            open_result: Err("open result not configured".to_owned()),
            restore_result: Err("restore result not configured".to_owned()),
            advance_result: Err("advance result not configured".to_owned()),
            captures: VecDeque::new(),
            open_calls: 0,
            restore_calls: 0,
            advance_calls: 0,
            capture_calls: 0,
            bounds: Vec::new(),
        }
    }
}

impl FakeContinuation {
    fn open(endpoint: Endpoint, capture: Result<CapturedEndpoint, String>) -> Self {
        Self {
            open_result: Ok(endpoint),
            captures: VecDeque::from([capture]),
            ..Self::default()
        }
    }

    fn run(
        source: Endpoint,
        next: Endpoint,
        captures: impl IntoIterator<Item = Result<CapturedEndpoint, String>>,
    ) -> Self {
        Self {
            restore_result: Ok(source),
            advance_result: Ok(next),
            captures: captures.into_iter().collect(),
            ..Self::default()
        }
    }

    fn rejecting() -> Self {
        Self {
            open_result: Err("runtime must not be constructed on a retry".to_owned()),
            restore_result: Err("runtime must not be restored on a retry".to_owned()),
            advance_result: Err("runtime must not advance on a retry".to_owned()),
            ..Self::default()
        }
    }
}

impl Continuation for FakeContinuation {
    fn open_recorded(
        &mut self,
        _actions: &[FaultAction],
        _source_moment: u64,
        _rewind_nanos: u64,
    ) -> Result<Endpoint, String> {
        self.open_calls += 1;
        self.open_result.clone()
    }

    fn restore(
        &mut self,
        _checkpoint: &[u8],
        _actions: &[FaultAction],
        _endpoint: &Endpoint,
    ) -> Result<Endpoint, String> {
        self.restore_calls += 1;
        self.restore_result.clone()
    }

    fn advance(&mut self, bound: &Advance) -> Result<Endpoint, String> {
        self.advance_calls += 1;
        self.bounds.push(bound.clone());
        self.advance_result.clone()
    }

    fn capture(&mut self) -> Result<CapturedEndpoint, String> {
        self.capture_calls += 1;
        self.captures
            .pop_front()
            .unwrap_or_else(|| Err("unexpected capture".to_owned()))
    }
}

fn fork_request(name: &str, request_id: Option<&str>) -> ForkRequest {
    ForkRequest {
        source: Selector::Finding("bug-1".to_owned()),
        rewind_nanos: 0,
        name: name.to_owned(),
        probe: false,
        request_id: request_id.map(str::to_owned),
    }
}

#[test]
fn capture_failure_leaves_no_new_branch_or_journal_records() {
    let (_directory, mut workspace) = workspace();
    let initial_sequence = workspace.sequence();
    let mut guest = FakeContinuation::open(
        endpoint(SOURCE_AT),
        Err("state hash read failed".to_owned()),
    );

    let error = fork(&mut workspace, &mut guest, &fork_request("trace", None))
        .expect_err("capture failure");

    assert!(error.to_string().contains("capture"), "{error}");
    assert_eq!(workspace.sequence(), initial_sequence);
    assert!(workspace.branch("trace").is_none());
    assert_eq!(workspace.moments().len(), 1);
    assert_eq!(guest.open_calls, 1);
    assert_eq!(guest.capture_calls, 1);
}

#[test]
fn forking_keeps_the_recorded_finding_and_moment_unchanged() {
    let (_directory, mut workspace) = workspace();
    let finding_before = workspace.finding("bug-1").cloned().expect("finding");
    let moment_before = workspace.moment("m-0001").cloned().expect("moment");
    let mut guest = FakeContinuation::open(endpoint(SOURCE_AT), Ok(captured(SOURCE_AT)));

    fork(&mut workspace, &mut guest, &fork_request("trace", None)).expect("fork branch");

    assert_eq!(workspace.finding("bug-1"), Some(&finding_before));
    assert_eq!(workspace.moment("m-0001"), Some(&moment_before));
    assert_eq!(workspace.branch("trace").unwrap().source, "bug-1");
}

#[test]
fn endpoint_observations_must_be_derived_from_the_captured_events() {
    let (_directory, mut workspace) = workspace();
    let mut inconsistent = captured(SOURCE_AT);
    inconsistent.endpoint.observations.sometimes.insert(7);
    let inconsistent_endpoint = inconsistent.endpoint.clone();
    let mut guest = FakeContinuation::open(inconsistent_endpoint, Ok(inconsistent));

    let error = fork(&mut workspace, &mut guest, &fork_request("trace", None))
        .expect_err("event and endpoint mismatch");

    assert!(error.to_string().contains("observations"), "{error}");
    assert!(workspace.branch("trace").is_none());
    assert_eq!(workspace.moments().len(), 1);

    let valid_events = vec![SdkEventRecord {
        position: 0,
        virtual_time: SOURCE_AT,
        event: (1_u32 << 24) | 7,
        payload: "000000".to_owned(),
    }];
    let raw = vec![(SOURCE_AT, (1_u32 << 24) | 7, vec![0, 0, 0])];
    let observations = FaultObservations::new(
        SOURCE_AT,
        &decode_sdk_events(&raw).expect("event decode"),
        FaultStop::Deadline,
    );
    let mut event_capture = captured(SOURCE_AT);
    event_capture.endpoint.observations = observations;
    event_capture.events = valid_events;
    let mut event_guest = FakeContinuation::open(endpoint(SOURCE_AT), Ok(event_capture.clone()));
    // The returned endpoint must carry the same complete observations as the
    // capture; otherwise the mismatch is caught before any journal write.
    let error = fork(
        &mut workspace,
        &mut event_guest,
        &fork_request("events", None),
    )
    .expect_err("returned endpoint and captured endpoint mismatch");
    assert!(error.to_string().contains("capture endpoint"), "{error}");

    validate_capture(&event_capture, &facts(), &actions()).expect("valid event-derived endpoint");
}

#[test]
fn event_evidence_is_a_valid_contiguous_suffix_when_bounded() {
    let events: Vec<SdkEventRecord> = (0..400)
        .map(|position| SdkEventRecord {
            position,
            virtual_time: SOURCE_AT,
            event: 99,
            payload: "aa".repeat(1_000),
        })
        .collect();
    let (bytes, truncated) = bound_event_evidence(&events).expect("serialize suffix");
    assert!(truncated);
    let suffix: Vec<SdkEventRecord> = serde_json::from_slice(&bytes).expect("valid JSON array");
    assert!(!suffix.is_empty());
    assert!(suffix.len() < events.len());
    assert_eq!(suffix.last().expect("last suffix event").position, 399);
    assert_eq!(
        suffix
            .iter()
            .map(|event| event.position)
            .collect::<Vec<_>>(),
        (suffix[0].position..=399).collect::<Vec<_>>()
    );

    let oversized = vec![SdkEventRecord {
        position: 0,
        virtual_time: SOURCE_AT,
        event: 99,
        payload: "aa".repeat(EVIDENCE_LIMIT),
    }];
    let (bytes, truncated) = bound_event_evidence(&oversized).expect("serialize empty suffix");
    assert!(truncated);
    assert_eq!(
        serde_json::from_slice::<Vec<SdkEventRecord>>(&bytes).unwrap(),
        Vec::new()
    );
}

#[test]
fn condition_parser_rejects_trailing_fields() {
    assert!(Condition::parse("assertion:7:fail:extra").is_err());
    assert!(Condition::parse("assertion:7:hit:").is_err());
    assert_eq!(
        Condition::parse("assertion:7").unwrap(),
        Condition::AssertionFail(7)
    );
}

#[test]
fn request_fingerprint_rejects_argument_reuse_and_retries_without_runtime_calls() {
    let (_directory, mut workspace) = workspace();
    let request = fork_request("trace", Some("request-1"));
    let mut guest = FakeContinuation::open(endpoint(SOURCE_AT), Ok(captured(SOURCE_AT)));
    let first = fork(&mut workspace, &mut guest, &request).expect("initial fork");
    assert!(
        workspace
            .request("request-1")
            .unwrap()
            .fingerprint
            .is_some()
    );

    let rejecting = FakeContinuation::rejecting();
    let replay = retry_fork(&workspace, &request)
        .expect("retry lookup")
        .expect("committed result");
    assert!(replay.replayed_request);
    assert_eq!(replay.moment, first.moment);
    assert_eq!(rejecting.open_calls, 0);
    assert_eq!(rejecting.restore_calls, 0);
    assert_eq!(rejecting.advance_calls, 0);

    let changed = fork_request("different", Some("request-1"));
    let error = retry_fork(&workspace, &changed).expect_err("same id, different arguments");
    assert!(error.to_string().contains("different arguments"), "{error}");

    let legacy_outcome = first.clone();
    workspace
        .commit(vec![Record::Request(Box::new(
            crate::workspace::RequestRecord {
                id: "legacy".to_owned(),
                operation: "fork".to_owned(),
                sequence: workspace.sequence() + 1,
                fingerprint: None,
                result: serde_json::to_value(legacy_outcome).expect("outcome JSON"),
            },
        ))])
        .expect("legacy fixture");
    let error = retry_fork(&workspace, &fork_request("legacy", Some("legacy")))
        .expect_err("legacy request cannot be safely retried");
    assert!(error.to_string().contains("legacy record"), "{error}");
}

#[test]
fn reopened_workspace_replays_a_lost_reply_without_reopening_the_guest() {
    let (directory, mut workspace) = workspace();
    let request = fork_request("trace", Some("lost-reply"));
    let mut guest = FakeContinuation::open(endpoint(SOURCE_AT), Ok(captured(SOURCE_AT)));
    let committed = fork(&mut workspace, &mut guest, &request).expect("commit fork");
    let sequence = workspace.sequence();
    drop(workspace);

    let reopened = Workspace::open(directory.path()).expect("reopen workspace");
    let rejecting = FakeContinuation::rejecting();
    let replay = retry_fork(&reopened, &request)
        .expect("retry after reopen")
        .expect("durable request");
    assert!(replay.replayed_request);
    assert_eq!(replay.moment, committed.moment);
    assert_eq!(reopened.sequence(), sequence);
    assert_eq!(reopened.branch("trace").unwrap().head, committed.moment);
    assert_eq!(rejecting.open_calls, 0);
}

#[test]
fn cached_reply_is_checked_against_its_immutable_endpoint() {
    let (_directory, mut workspace) = workspace();
    let request = fork_request("trace", Some("bad-reply"));
    let mut guest = FakeContinuation::open(endpoint(SOURCE_AT), Ok(captured(SOURCE_AT)));
    let mut result =
        fork(&mut workspace, &mut guest, &fork_request("trace", None)).expect("initial fork");
    result.virtual_time += 1;
    workspace
        .commit(vec![Record::Request(Box::new(
            crate::workspace::RequestRecord {
                id: "bad-reply".to_owned(),
                operation: "fork".to_owned(),
                sequence: workspace.sequence() + 1,
                fingerprint: Some(
                    fingerprint("fork", ForkFingerprintArgs::from_request(&request))
                        .expect("fingerprint"),
                ),
                result: serde_json::to_value(result).expect("result JSON"),
            },
        ))])
        .expect("bad reply fixture");

    let error = retry_fork(&workspace, &request).expect_err("inconsistent cached endpoint");
    assert!(error.to_string().contains("claims"), "{error}");
}

#[test]
fn retained_restore_must_preserve_the_saved_action_cursor() {
    let (_directory, mut workspace) = workspace();
    let mut fork_guest = FakeContinuation::open(endpoint(SOURCE_AT), Ok(captured(SOURCE_AT)));
    fork(
        &mut workspace,
        &mut fork_guest,
        &fork_request("trace", None),
    )
    .expect("fork branch");
    let mut changed = captured(SOURCE_AT);
    changed.checkpoint = checkpoint_with_cursor(
        SOURCE_AT,
        ActionCursor::from_parts(0, true).expect("active cursor"),
    );
    let mut guest = FakeContinuation::run(endpoint(SOURCE_AT), endpoint(NEXT_AT), [Ok(changed)]);
    let error = run(
        &mut workspace,
        &mut guest,
        &RunRequest {
            branch: "trace".to_owned(),
            bound: Advance {
                within_nanos: NEXT_AT - SOURCE_AT,
                extend: true,
                ..Advance::default()
            },
            request_id: None,
        },
    )
    .expect_err("changed cursor");
    assert!(error.to_string().contains("action cursor"), "{error}");
    assert_eq!(guest.advance_calls, 0);
}

#[test]
fn export_refuses_evidence_ids_that_escape_the_destination() {
    let (_directory, mut workspace) = workspace();
    let mut guest = FakeContinuation::open(endpoint(SOURCE_AT), Ok(captured(SOURCE_AT)));
    let outcome =
        fork(&mut workspace, &mut guest, &fork_request("trace", None)).expect("fork branch");
    let blob = workspace.store_blob(b"outside").expect("blob");
    workspace
        .commit(vec![Record::Evidence(Box::new(
            crate::workspace::EvidenceRecord {
                id: "../escape".to_owned(),
                kind: "console".to_owned(),
                moment: outcome.moment,
                blob,
                bytes: 7,
                truncated: false,
                precision: "test".to_owned(),
            },
        ))])
        .expect("malicious evidence fixture");
    let destination = workspace.root().join("export");
    let error = export(&workspace, "bug-1", &destination, true).expect_err("unsafe evidence id");
    assert!(
        error.to_string().contains("safe single filename"),
        "{error}"
    );
    assert!(!workspace.root().join("escape.txt").exists());
}

#[test]
fn run_uses_the_normalized_bound_and_commits_the_union_of_evaluations() {
    let (_directory, mut workspace) = workspace();
    let mut fork_guest = FakeContinuation::open(endpoint(SOURCE_AT), Ok(captured(SOURCE_AT)));
    fork(
        &mut workspace,
        &mut fork_guest,
        &fork_request("trace", None),
    )
    .expect("fork branch");

    let raw = vec![(NEXT_AT, (1_u32 << 24) | 7, vec![0, 0, 0])];
    let observations = FaultObservations::new(
        NEXT_AT,
        &decode_sdk_events(&raw).expect("event decode"),
        FaultStop::Deadline,
    );
    let next_endpoint = Endpoint {
        virtual_time: NEXT_AT,
        stop: StopReason::VirtualDeadline,
        observations,
        condition_met: false,
    };
    let mut next_capture = captured(NEXT_AT);
    next_capture.endpoint = next_endpoint.clone();
    next_capture.events = vec![SdkEventRecord {
        position: 0,
        virtual_time: NEXT_AT,
        event: (1_u32 << 24) | 7,
        payload: "000000".to_owned(),
    }];
    let mut guest = FakeContinuation::run(
        endpoint(SOURCE_AT),
        next_endpoint,
        [Ok(captured(SOURCE_AT)), Ok(next_capture)],
    );
    let outcome = run(
        &mut workspace,
        &mut guest,
        &RunRequest {
            request_id: Some("run-1".to_owned()),
            bound: Advance {
                within_nanos: NEXT_AT - SOURCE_AT,
                wall_seconds: 0,
                extend: true,
                ..Advance::default()
            },
            branch: "trace".to_owned(),
        },
    )
    .expect("run branch");
    assert_eq!(outcome.evaluated, vec![7]);
    assert_eq!(guest.bounds.len(), 1);
    assert_eq!(guest.bounds[0].wall_seconds, DEFAULT_WALL_SECONDS);
    assert!(workspace.request("run-1").unwrap().fingerprint.is_some());
    assert_eq!(
        workspace
            .moment(&outcome.moment)
            .unwrap()
            .state_hash_encoding,
        StateHashEncoding::EngineDigest
    );
}
