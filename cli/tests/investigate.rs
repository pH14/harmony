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

use control_proto::{ExecCompletion, ExecStatus, Moment};
use faults_workload::{
    FaultAction,
    checkpoint::Checkpoint,
    declarations::Declarations,
    execution::ActionCursor,
    investigate::{
        Advance, CapturedEndpoint, Continuation, Endpoint, ExecRequest, ForkRequest, Outcome, exec,
        fork,
    },
    package::StateHashEncoding,
    retained::RetainedContinuation,
    target::{FaultObservations, FaultStop},
    workspace::{
        CommandCompletion, FORMAT, Finding, History, MomentRecord, Record, Selector, StopReason,
        Workspace, WorkspaceFacts,
    },
};

const ROOT_SEAL: u64 = 1_000;
const HORIZON: u64 = 100;
const SOURCE_AT: u64 = ROOT_SEAL + HORIZON;
const COMMAND_DELTA: u64 = 25;
const COMMAND_AT: u64 = SOURCE_AT + COMMAND_DELTA;
const COMMAND_STATUS: u64 = 37;
const COMMAND_REQUEST_ID: &str = "exec-known";
const COMMAND_OUTPUT: &[u8] = b"committed command output\n";

fn command_argv() -> Vec<String> {
    vec!["echo".to_owned(), "committed-output".to_owned()]
}

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
        command: None,
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

fn command_request() -> ExecRequest {
    ExecRequest {
        target: Selector::BranchHead("trace".to_owned()),
        probe: false,
        argv: command_argv(),
        bound: Advance {
            within_nanos: COMMAND_DELTA,
            until: None,
            extend: true,
            wall_seconds: 30,
        },
        request_id: Some(COMMAND_REQUEST_ID.to_owned()),
    }
}

#[derive(Debug)]
struct CommandContinuation {
    at: u64,
    command: Option<ExecStatus>,
}

impl CommandContinuation {
    fn new(at: u64) -> Self {
        Self { at, command: None }
    }

    fn endpoint(&self) -> Endpoint {
        let completed = self.command.as_ref().is_some_and(|status| {
            matches!(
                status.completion,
                ExecCompletion::Exited { .. } | ExecCompletion::Aborted { .. }
            )
        });
        let (stop, observation_stop) = if completed {
            (StopReason::CommandComplete, FaultStop::CommandComplete)
        } else {
            (StopReason::VirtualDeadline, FaultStop::Deadline)
        };
        Endpoint {
            virtual_time: self.at,
            stop,
            observations: FaultObservations {
                moment: self.at,
                stop: observation_stop,
                ..FaultObservations::default()
            },
            condition_met: false,
        }
    }

    fn capture_endpoint(&self) -> Result<CapturedEndpoint, String> {
        Ok(CapturedEndpoint {
            command: self.command.clone(),
            endpoint: self.endpoint(),
            checkpoint: retained_checkpoint(self.at),
            state_hash: state_hash(self.at),
            console: format!("command endpoint {}\n", self.at).into_bytes(),
            events: Vec::new(),
        })
    }
}

impl Continuation for CommandContinuation {
    fn open_recorded(
        &mut self,
        _actions: &[FaultAction],
        _source_moment: u64,
        _rewind_nanos: u64,
    ) -> Result<Endpoint, String> {
        Err("command fixture requires a retained branch".to_owned())
    }

    fn restore(
        &mut self,
        checkpoint: &[u8],
        _actions: &[FaultAction],
        endpoint: &Endpoint,
    ) -> Result<Endpoint, String> {
        let retained = RetainedContinuation::decode(checkpoint)
            .map_err(|error| format!("decode command fixture checkpoint: {error}"))?;
        if retained.checkpoint.at != endpoint.virtual_time {
            return Err("command fixture restored a different endpoint".to_owned());
        }
        self.at = endpoint.virtual_time;
        self.command = None;
        Ok(endpoint.clone())
    }

    fn advance(&mut self, bound: &Advance) -> Result<Endpoint, String> {
        if bound.within_nanos < COMMAND_DELTA {
            return Ok(self.endpoint());
        }
        self.at = COMMAND_AT;
        let command = self
            .command
            .as_mut()
            .ok_or("command fixture was advanced before injection")?;
        command.at = Moment(COMMAND_AT);
        command.completion = ExecCompletion::Exited {
            status: COMMAND_STATUS,
            at: Moment(COMMAND_AT),
        };
        command.output = COMMAND_OUTPUT.to_vec();
        Ok(self.endpoint())
    }

    fn start_command(&mut self, _argv: &[String]) -> Result<ExecStatus, String> {
        if self.command.is_some() {
            return Err("command fixture already has a command".to_owned());
        }
        let status = ExecStatus {
            id: 1_001,
            at: Moment(self.at),
            completion: ExecCompletion::Pending,
            output: Vec::new(),
            truncated: false,
        };
        self.command = Some(status.clone());
        Ok(status)
    }

    fn capture(&mut self) -> Result<CapturedEndpoint, String> {
        self.capture_endpoint()
    }
}

fn initial_command_workspace() -> (tempfile::TempDir, Outcome, u64, Vec<(String, Vec<u8>)>) {
    let (directory, mut workspace, source) = workspace();
    let fork_request = ForkRequest {
        source: Selector::Finding("bug-1".to_owned()),
        rewind_nanos: 0,
        name: "trace".to_owned(),
        probe: false,
        request_id: None,
    };
    let mut guest = FixtureContinuation::new(source.virtual_time);
    fork(&mut workspace, &mut guest, &fork_request).expect("initial public fork");

    let request = command_request();
    let mut command_guest = CommandContinuation::new(source.virtual_time);
    let outcome = exec(&mut workspace, &mut command_guest, &request).expect("public command exec");
    assert_eq!(
        command_guest.command.as_ref().map(|status| status.id),
        Some(1_001)
    );
    assert_eq!(
        outcome.command.as_ref().map(|command| &command.completion),
        Some(&CommandCompletion::Exited {
            status: COMMAND_STATUS,
            virtual_time: COMMAND_AT,
        })
    );
    let replay = faults_workload::investigate::retry_exec(&workspace, &request)
        .expect("command request lookup")
        .expect("command request was committed");
    assert!(replay.replayed_request);
    assert_eq!(replay.moment, outcome.moment);
    assert_eq!(replay.command, outcome.command);

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

fn exec_process(root: &Path, target: &[&str], argv: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_harmony"));
    command.args(["-w", root.to_str().expect("UTF-8 temp path"), "--json"]);
    command.args(missing_artifact_args(root));
    command.args(["exec"]);
    command.args(target);
    command.args([
        "--within",
        "25ns",
        "--extend",
        "--wall-seconds",
        "30",
        "--request-id",
        COMMAND_REQUEST_ID,
        "--",
    ]);
    command.args(argv);
    command.output().expect("run harmony exec subprocess")
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

fn inspect_command_process(root: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_harmony"))
        .args([
            "-w",
            root.to_str().expect("UTF-8 temp path"),
            "--json",
            "inspect",
            "trace@head",
            "command",
        ])
        .output()
        .expect("run harmony inspect command subprocess")
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

#[test]
fn process_exec_retry_returns_saved_completion_before_guest_boot() {
    let (directory, first, sequence, journal) = initial_command_workspace();
    let root = directory.path().to_path_buf();
    let argv = ["echo", "committed-output"];

    // These artifact paths cannot be opened. A successful result proves the
    // fresh CLI process returned the committed request before constructing a
    // guest continuation.
    let replay = json_output(&exec_process(&root, &["trace"], &argv));
    assert_eq!(replay["replayed_request"], true);
    assert_eq!(replay["operation"], "exec");
    assert_eq!(replay["branch"], "trace");
    assert_eq!(
        replay["command"]["invocation"]["argv"],
        serde_json::json!(argv)
    );
    assert_eq!(
        replay["command"]["completion"]["exited"]["status"],
        COMMAND_STATUS
    );
    assert_eq!(
        replay["command"]["completion"]["exited"]["virtual_time"],
        COMMAND_AT
    );

    let initial = serde_json::to_value(&first).expect("initial command outcome JSON");
    let command_evidence_id = initial["command"]["evidence"].clone();
    assert_eq!(replay["command"]["evidence"], command_evidence_id);
    for field in [
        "moment",
        "virtual_time",
        "state_hash",
        "history",
        "stop",
        "evidence",
    ] {
        assert_eq!(replay[field], initial[field], "retry changed {field}");
    }

    let command_evidence = json_output(&inspect_command_process(&root));
    assert_eq!(command_evidence["moment"], replay["moment"]);
    assert_eq!(command_evidence["view"], "command");
    assert_eq!(command_evidence["evidence"], command_evidence_id);
    assert_eq!(
        command_evidence["lines"],
        serde_json::json!(["committed command output"])
    );
    assert_eq!(command_evidence["truncated"], false);

    let reopened = Workspace::open(&root).expect("reopen after command retry");
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

    let changed_argv = failed_output(exec_process(&root, &["trace"], &["echo", "changed"]));
    assert!(
        changed_argv.contains("different arguments"),
        "{changed_argv}"
    );
    assert_eq!(
        journal_snapshot(&root),
        journal,
        "argv collision mutated journal"
    );

    let changed_probe = failed_output(exec_process(&root, &["--at", "trace@head"], &argv));
    assert!(
        changed_probe.contains("different arguments"),
        "{changed_probe}"
    );
    assert_eq!(
        journal_snapshot(&root),
        journal,
        "probe-intent collision mutated journal"
    );
}
