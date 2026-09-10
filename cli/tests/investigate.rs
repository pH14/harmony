// SPDX-License-Identifier: AGPL-3.0-or-later

//! Process boundaries for the durable investigation CLI.
//!
//! The fixture uses the public investigation trait only to create one genuine
//! committed fork and request fingerprint. The subprocess then reopens that
//! workspace with deliberately unusable guest inputs. A successful retry is
//! therefore evidence of durable request handling and CLI reopening, not a
//! claim about KVM execution.

use std::{
    collections::VecDeque,
    fs,
    path::Path,
    process::{Command, Output},
};

use faults_workload::{
    FaultAction,
    checkpoint::Checkpoint,
    declarations::Declarations,
    execution::ActionCursor,
    investigate::{CapturedEndpoint, Continuation, Endpoint, ForkRequest, Outcome, fork},
    package::StateHashEncoding,
    retained::RetainedContinuation,
    target::FaultObservations,
    workspace::{
        FORMAT, Finding, History, MomentRecord, Record, Selector, StopReason, Workspace,
        WorkspaceFacts,
    },
};

const ROOT_SEAL: u64 = 1_000;
const HORIZON: u64 = 100;
const SOURCE_AT: u64 = ROOT_SEAL + HORIZON;

fn actions() -> Vec<FaultAction> {
    vec![FaultAction::Wait]
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

fn state_hash(at: u64) -> String {
    format!("{at:064x}")
}

fn retained_checkpoint(at: u64) -> Vec<u8> {
    RetainedContinuation {
        checkpoint: Checkpoint {
            setup: ROOT_SEAL,
            at,
            image_identity: [0x5a; 32],
            pages: Vec::new(),
            sidecar: Vec::new(),
            cursor: Some(ActionCursor::default()),
        },
        windows: faults_workload::target::ActionWindows {
            root_seal: ROOT_SEAL,
            horizon_nanos: HORIZON,
        },
        actions: actions(),
    }
    .encode()
    .expect("the public retained envelope accepts the fixture state")
}

fn captured(at: u64) -> CapturedEndpoint {
    CapturedEndpoint {
        endpoint: endpoint(at),
        checkpoint: retained_checkpoint(at),
        state_hash: state_hash(at),
        console: format!("endpoint {at}\n").into_bytes(),
        events: Vec::new(),
    }
}

fn facts() -> WorkspaceFacts {
    WorkspaceFacts {
        format: FORMAT.to_owned(),
        package: faults_workload::package::PACKAGE.to_owned(),
        identity: "cli-process-investigation-v1".to_owned(),
        image: "missing-workload.oci".to_owned(),
        root_seal: ROOT_SEAL,
        horizon_nanos: HORIZON,
        ram_mib: 64,
        declarations: Declarations::parse("node 1 service /opt/harmony/service\n")
            .expect("the fixture bundle parses"),
        ..WorkspaceFacts::default()
    }
}

fn workspace() -> (tempfile::TempDir, Workspace, Endpoint) {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let mut workspace = Workspace::create(directory.path(), facts()).expect("workspace facts");
    let source = endpoint(SOURCE_AT);
    let hash = state_hash(SOURCE_AT);
    workspace
        .commit(vec![
            Record::Moment(Box::new(MomentRecord {
                id: "m-0001".to_owned(),
                virtual_time: SOURCE_AT,
                history: History::Recorded,
                state_hash: Some(hash.clone()),
                state_hash_encoding: StateHashEncoding::EngineDigest,
                stop: Some(source.stop.clone()),
                observations: Some(source.observations.clone()),
                ..MomentRecord::default()
            })),
            Record::Finding(Box::new(Finding {
                id: "bug-1".to_owned(),
                execution: 1,
                actions: actions(),
                observations: source.observations.clone(),
                moment: "m-0001".to_owned(),
                confirmed: true,
                state_hash: hash,
                state_hash_encoding: StateHashEncoding::EngineDigest,
                ..Finding::default()
            })),
        ])
        .expect("source records");
    (directory, workspace, source)
}

#[derive(Debug)]
struct FixtureContinuation {
    endpoint: Endpoint,
    captures: VecDeque<Result<CapturedEndpoint, String>>,
}

impl FixtureContinuation {
    fn new(at: u64) -> Self {
        Self {
            endpoint: endpoint(at),
            captures: VecDeque::from([Ok(captured(at))]),
        }
    }
}

impl Continuation for FixtureContinuation {
    fn open_recorded(
        &mut self,
        _actions: &[FaultAction],
        _source_moment: u64,
        _rewind_nanos: u64,
    ) -> Result<Endpoint, String> {
        Ok(self.endpoint.clone())
    }

    fn restore(
        &mut self,
        _checkpoint: &[u8],
        _actions: &[FaultAction],
        _endpoint: &Endpoint,
    ) -> Result<Endpoint, String> {
        Err("restore is not needed for the initial finding fork".to_owned())
    }

    fn advance(
        &mut self,
        _bound: &faults_workload::investigate::Advance,
    ) -> Result<Endpoint, String> {
        Err("advance is not needed for the initial finding fork".to_owned())
    }

    fn capture(&mut self) -> Result<CapturedEndpoint, String> {
        self.captures
            .pop_front()
            .unwrap_or_else(|| Err("unexpected capture".to_owned()))
    }
}

fn initial_workspace() -> (tempfile::TempDir, Outcome, u64, Vec<(String, Vec<u8>)>) {
    let (directory, mut workspace, source) = workspace();
    let request = ForkRequest {
        source: Selector::Finding("bug-1".to_owned()),
        rewind_nanos: 0,
        name: "trace".to_owned(),
        probe: false,
        request_id: Some("known".to_owned()),
    };
    let mut guest = FixtureContinuation::new(source.virtual_time);
    let outcome = fork(&mut workspace, &mut guest, &request).expect("initial public fork");
    let sequence = workspace.sequence();
    let journal = journal_snapshot(directory.path());
    (directory, outcome, sequence, journal)
}

fn journal_snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut entries = fs::read_dir(root.join("journal"))
        .expect("journal directory")
        .map(|entry| {
            let path = entry.expect("journal entry").path();
            (
                path.file_name()
                    .expect("journal filename")
                    .to_string_lossy()
                    .into_owned(),
                fs::read(path).expect("journal contents"),
            )
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries
}

fn missing_artifact_args(root: &Path) -> Vec<String> {
    ["--kernel", "--base-initramfs", "--fault-agent"]
        .into_iter()
        .flat_map(|flag| {
            [
                flag.to_owned(),
                root.join(format!("missing-{flag}")).display().to_string(),
            ]
        })
        .collect()
}

fn fork_process(root: &Path, source: &str, rewind: &str, name: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_harmony"));
    command.args(["-w", root.to_str().expect("UTF-8 temp path"), "--json"]);
    command.args(missing_artifact_args(root));
    command.args([
        "fork",
        source,
        "--rewind",
        rewind,
        "--name",
        name,
        "--request-id",
        "known",
    ]);
    command.output().expect("run harmony fork subprocess")
}

fn inspect_events_process(root: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_harmony"))
        .args([
            "-w",
            root.to_str().expect("UTF-8 temp path"),
            "--json",
            "inspect",
            "trace@head",
            "events",
        ])
        .output()
        .expect("run harmony inspect subprocess")
}

fn json_output(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("CLI emitted valid JSON")
}

fn failed_output(output: Output) -> String {
    assert!(!output.status.success(), "unexpected successful CLI retry");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn process_retry_reopens_saved_fork_without_guest_or_journal_changes() {
    let (directory, first, sequence, journal) = initial_workspace();
    let root = directory.path().to_path_buf();
    // Drop the library workspace before invoking the subprocess so it can
    // acquire the advisory lock exactly as an independent CLI process would.
    let output = fork_process(&root, "bug-1", "0s", "trace");
    let replay = json_output(&output);
    assert_eq!(replay["replayed_request"], true);
    let initial = serde_json::to_value(&first).expect("initial outcome JSON");
    for field in ["moment", "virtual_time", "state_hash", "evidence"] {
        assert_eq!(replay[field], initial[field], "retry changed {field}");
    }

    let reopened = Workspace::open(&root).expect("reopen after retry");
    assert_eq!(
        reopened.sequence(),
        sequence,
        "retry appended a transaction"
    );
    drop(reopened);
    assert_eq!(
        journal_snapshot(&root),
        journal,
        "retry added journal records"
    );

    let changed_name = failed_output(fork_process(&root, "bug-1", "0s", "different"));
    assert!(
        changed_name.contains("different arguments"),
        "{changed_name}"
    );
    assert_eq!(
        journal_snapshot(&root),
        journal,
        "name mismatch mutated journal"
    );

    let changed_source = failed_output(fork_process(&root, "m-0001", "0s", "trace"));
    assert!(
        changed_source.contains("different arguments"),
        "{changed_source}"
    );
    assert_eq!(
        journal_snapshot(&root),
        journal,
        "source mismatch mutated journal"
    );

    let changed_rewind = failed_output(fork_process(&root, "bug-1", "1ns", "trace"));
    assert!(
        changed_rewind.contains("different arguments"),
        "{changed_rewind}"
    );
    assert_eq!(
        journal_snapshot(&root),
        journal,
        "rewind mismatch mutated journal"
    );
}

#[test]
fn process_inspect_events_reads_the_saved_endpoint_as_json() {
    let (directory, _first, _sequence, _journal) = initial_workspace();
    let value = json_output(&inspect_events_process(directory.path()));
    assert_eq!(value["format"], "harmony-inspect-evidence-v1");
    assert_eq!(value["view"], "events");
    assert_eq!(value["moment"], "m-0002");
    assert_eq!(value["lines"], serde_json::json!(["[]"]));
}
