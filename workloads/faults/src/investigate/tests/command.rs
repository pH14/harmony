// SPDX-License-Identifier: AGPL-3.0-or-later

//! Portable command continuation tests.
//!
//! The continuation below deliberately keeps its command tracker in the
//! retained checkpoint sidecar.  A cold restore therefore exercises the same
//! durable boundary as a real engine: a pending command is resumed from
//! serialized state, rather than from an in-process capture queue.

use super::*;
use crate::{
    checkpoint::Checkpoint,
    execution::ActionCursor,
    retained::RetainedContinuation,
    target::{FaultAction, FaultObservations, FaultStop},
    workspace::CommandCompletion,
};
use control_proto::{ExecCompletion, ExecStatus, Moment};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const COMPLETION_DELTA: u64 = 25;
const NONZERO_STATUS: u64 = 37;
const PENDING_OUTPUT: &[u8] = b"pending output\n";
const COMPLETED_OUTPUT: &[u8] = b"completed output\n";

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct FakeSidecar {
    command: Option<FakeCommand>,
    injection_count: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct FakeCommand {
    id: u64,
    argv: Vec<String>,
    output: Vec<u8>,
    started_at: u64,
    completion_at: u64,
    completed: bool,
    status: u64,
}

#[derive(Debug, Default)]
struct StatefulContinuation {
    at: u64,
    state: FakeSidecar,
    open_calls: usize,
    restore_calls: usize,
    advance_calls: usize,
    start_calls: usize,
    capture_calls: usize,
    fail_advance: bool,
    fail_capture_at: Option<usize>,
    reject: bool,
}

impl StatefulContinuation {
    fn with_advance_failure() -> Self {
        Self {
            fail_advance: true,
            ..Self::default()
        }
    }

    fn with_capture_failure(call: usize) -> Self {
        Self {
            fail_capture_at: Some(call),
            ..Self::default()
        }
    }

    fn rejecting() -> Self {
        Self {
            reject: true,
            ..Self::default()
        }
    }

    fn current_endpoint(&self) -> Endpoint {
        let completed = self
            .state
            .command
            .as_ref()
            .is_some_and(|command| command.completed);
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

    fn status(&self) -> Option<ExecStatus> {
        let command = self.state.command.as_ref()?;
        let completion = if command.completed {
            ExecCompletion::Exited {
                status: command.status,
                at: Moment(command.completion_at),
            }
        } else {
            ExecCompletion::Pending
        };
        Some(ExecStatus {
            id: command.id,
            at: Moment(self.at),
            completion,
            output: command.output.clone(),
            truncated: false,
        })
    }

    fn retained_checkpoint(&self) -> Result<Vec<u8>, String> {
        let sidecar = serde_json::to_vec(&self.state).map_err(|error| error.to_string())?;
        RetainedContinuation {
            checkpoint: Checkpoint {
                setup: super::ROOT_SEAL,
                at: self.at,
                image_identity: [0x5a; 32],
                pages: Vec::new(),
                sidecar,
                cursor: Some(ActionCursor::default()),
            },
            windows: super::windows(),
            actions: super::actions(),
        }
        .encode()
        .map_err(|error| error.to_string())
    }

    fn state_hash(&self) -> String {
        if self.state == FakeSidecar::default() {
            return super::hash(self.at);
        }
        let sidecar = serde_json::to_vec(&self.state).expect("fake sidecar serializes");
        let mut input = Vec::with_capacity(8 + sidecar.len());
        input.extend_from_slice(&self.at.to_le_bytes());
        input.extend_from_slice(&sidecar);
        format!("{:x}", Sha256::digest(input))
    }

    fn capture_result(&self) -> Result<CapturedEndpoint, String> {
        Ok(CapturedEndpoint {
            endpoint: self.current_endpoint(),
            checkpoint: self.retained_checkpoint()?,
            state_hash: self.state_hash(),
            console: format!("fake {}\n", self.at).into_bytes(),
            events: Vec::new(),
            command: self.status(),
        })
    }
}

impl Continuation for StatefulContinuation {
    fn open_recorded(
        &mut self,
        actions: &[FaultAction],
        source_moment: u64,
        _rewind_nanos: u64,
    ) -> Result<Endpoint, String> {
        self.open_calls += 1;
        if self.reject {
            return Err("runtime must not be opened on a retry".to_owned());
        }
        if actions != super::actions() {
            return Err("fake received an unexpected action list".to_owned());
        }
        self.at = source_moment;
        self.state = FakeSidecar::default();
        Ok(self.current_endpoint())
    }

    fn restore(
        &mut self,
        checkpoint: &[u8],
        actions: &[FaultAction],
        endpoint: &Endpoint,
    ) -> Result<Endpoint, String> {
        self.restore_calls += 1;
        if self.reject {
            return Err("runtime must not be restored on a retry".to_owned());
        }
        if actions != super::actions() {
            return Err("fake received an unexpected retained action list".to_owned());
        }
        let retained = RetainedContinuation::decode(checkpoint)
            .map_err(|error| format!("decode fake retained checkpoint: {error}"))?;
        if retained.windows != super::windows() || retained.actions != super::actions() {
            return Err("fake retained plan changed".to_owned());
        }
        if retained.checkpoint.at != endpoint.virtual_time {
            return Err("fake retained endpoint changed".to_owned());
        }
        self.at = retained.checkpoint.at;
        self.state = if retained.checkpoint.sidecar.is_empty() {
            FakeSidecar::default()
        } else {
            serde_json::from_slice(&retained.checkpoint.sidecar)
                .map_err(|error| format!("decode fake command sidecar: {error}"))?
        };
        Ok(self.current_endpoint())
    }

    fn advance(&mut self, bound: &Advance) -> Result<Endpoint, String> {
        self.advance_calls += 1;
        if self.reject {
            return Err("runtime must not advance on a retry".to_owned());
        }
        if self.fail_advance {
            return Err("scripted advance failure".to_owned());
        }
        let pending = self
            .state
            .command
            .as_ref()
            .is_some_and(|command| !command.completed);
        if pending {
            let completion_at = self
                .state
                .command
                .as_ref()
                .ok_or_else(|| "fake pending command disappeared".to_owned())?
                .completion_at;
            let remaining = completion_at.saturating_sub(self.at);
            if bound.within_nanos < remaining {
                self.at = self
                    .at
                    .checked_add(bound.within_nanos)
                    .ok_or_else(|| "fake virtual time overflow".to_owned())?;
                return Ok(self.current_endpoint());
            }
            self.at = completion_at;
            let command = self
                .state
                .command
                .as_mut()
                .ok_or_else(|| "fake pending command disappeared".to_owned())?;
            command.completed = true;
            command.status = NONZERO_STATUS;
            command.output = COMPLETED_OUTPUT.to_vec();
        }
        Ok(self.current_endpoint())
    }

    fn start_command(&mut self, argv: &[String]) -> Result<ExecStatus, String> {
        self.start_calls += 1;
        if self.reject {
            return Err("runtime must not inject on a retry".to_owned());
        }
        if self
            .state
            .command
            .as_ref()
            .is_some_and(|command| !command.completed)
        {
            return Err("fake command is already pending".to_owned());
        }
        self.state.injection_count = self
            .state
            .injection_count
            .checked_add(1)
            .ok_or_else(|| "fake command id overflow".to_owned())?;
        let id = 1_000_u64
            .checked_add(self.state.injection_count)
            .ok_or_else(|| "fake command id overflow".to_owned())?;
        let completion_at = self
            .at
            .checked_add(COMPLETION_DELTA)
            .ok_or_else(|| "fake command completion time overflow".to_owned())?;
        self.state.command = Some(FakeCommand {
            id,
            argv: argv.to_vec(),
            output: PENDING_OUTPUT.to_vec(),
            started_at: self.at,
            completion_at,
            completed: false,
            status: 0,
        });
        Ok(self.status().expect("fake command was just installed"))
    }

    fn capture(&mut self) -> Result<CapturedEndpoint, String> {
        self.capture_calls += 1;
        if self.reject {
            return Err("runtime must not capture on a retry".to_owned());
        }
        if self.fail_capture_at == Some(self.capture_calls) {
            return Err("scripted capture failure".to_owned());
        }
        self.capture_result()
    }
}

fn pending_bound() -> Advance {
    Advance {
        within_nanos: 1,
        extend: true,
        ..Advance::default()
    }
}

fn completion_bound() -> Advance {
    Advance {
        within_nanos: COMPLETION_DELTA,
        extend: true,
        ..Advance::default()
    }
}

fn exec_request(target: Selector, probe: bool, request_id: &str, bound: Advance) -> ExecRequest {
    ExecRequest {
        target,
        probe,
        argv: vec!["echo".to_owned(), "command".to_owned()],
        bound,
        request_id: Some(request_id.to_owned()),
    }
}

#[test]
fn exec_pending_survives_cold_restore_and_run_completes_without_reinjection() {
    let (directory, mut workspace) = super::workspace();
    let request = exec_request(
        Selector::Finding("bug-1".to_owned()),
        true,
        "exec-pending",
        pending_bound(),
    );
    let mut first = StatefulContinuation::default();
    let pending = exec(&mut workspace, &mut first, &request).expect("pending exec");

    assert_eq!(first.start_calls, 1);
    assert_eq!(first.state.injection_count, 1);
    assert_eq!(first.at, super::SOURCE_AT + 1);
    let pending_status = first.status().expect("pending status");
    assert_eq!(pending_status.at, Moment(super::SOURCE_AT + 1));
    assert_eq!(pending_status.completion, ExecCompletion::Pending);
    assert_eq!(pending_status.output, PENDING_OUTPUT);
    assert_eq!(pending.virtual_time, super::SOURCE_AT + 1);
    assert_eq!(pending.stop, StopReason::VirtualDeadline);
    assert_eq!(
        pending.command.as_ref().map(|command| &command.completion),
        Some(&CommandCompletion::Pending)
    );
    let pending_moment = workspace.moment(&pending.moment).expect("pending moment");
    let pending_checkpoint = workspace
        .read_blob(
            pending_moment
                .checkpoint
                .as_ref()
                .expect("checkpoint digest"),
        )
        .expect("pending checkpoint bytes");
    let retained = RetainedContinuation::decode(&pending_checkpoint).expect("retained envelope");
    let sidecar: FakeSidecar =
        serde_json::from_slice(&retained.checkpoint.sidecar).expect("serialized fake sidecar");
    assert_eq!(sidecar.injection_count, 1);
    assert!(sidecar.command.is_some(), "pending command is serialized");

    let branch = pending.branch.clone();
    let sequence = workspace.sequence();
    drop(workspace);

    let mut workspace = Workspace::open(directory.path()).expect("reopen workspace");
    let mut resumed = StatefulContinuation::default();
    let completed = run(
        &mut workspace,
        &mut resumed,
        &RunRequest {
            branch: branch.clone(),
            bound: completion_bound(),
            request_id: Some("run-complete".to_owned()),
        },
    )
    .expect("resume pending command");

    assert_eq!(resumed.restore_calls, 1);
    assert_eq!(
        resumed.start_calls, 0,
        "resume must not reinject the command"
    );
    assert_eq!(
        resumed.state.injection_count, 1,
        "counter came from sidecar"
    );
    let completed_status = resumed.status().expect("completed status");
    assert_eq!(
        completed_status.at,
        Moment(super::SOURCE_AT + COMPLETION_DELTA)
    );
    assert_eq!(
        completed_status.completion,
        ExecCompletion::Exited {
            status: NONZERO_STATUS,
            at: Moment(super::SOURCE_AT + COMPLETION_DELTA),
        }
    );
    assert_eq!(completed_status.output, COMPLETED_OUTPUT);
    assert_eq!(completed.branch, branch);
    assert_eq!(completed.virtual_time, super::SOURCE_AT + COMPLETION_DELTA);
    assert_eq!(
        completed
            .command
            .as_ref()
            .map(|command| &command.completion),
        Some(&CommandCompletion::Exited {
            status: NONZERO_STATUS,
            virtual_time: super::SOURCE_AT + COMPLETION_DELTA,
        })
    );
    let command = completed.command.as_ref().expect("completed command");
    let output = workspace
        .read_blob(
            &workspace
                .evidence(&command.evidence)
                .expect("command evidence")
                .blob,
        )
        .expect("completed command output");
    assert_eq!(output, COMPLETED_OUTPUT);
    assert!(
        workspace
            .branch(&branch)
            .expect("resumed branch")
            .pending_command
            .is_none()
    );
    assert_eq!(workspace.sequence(), sequence + 1);
}

#[test]
fn completed_command_can_be_replaced_at_a_branch_head() {
    let (_directory, mut workspace) = super::workspace();
    let initial_request = exec_request(
        Selector::Finding("bug-1".to_owned()),
        true,
        "exec-first",
        pending_bound(),
    );
    let mut first_guest = StatefulContinuation::default();
    let pending = exec(&mut workspace, &mut first_guest, &initial_request).expect("initial exec");
    let branch = pending.branch.clone();

    let mut resume_guest = StatefulContinuation::default();
    let completed = run(
        &mut workspace,
        &mut resume_guest,
        &RunRequest {
            branch: branch.clone(),
            bound: completion_bound(),
            request_id: Some("run-first".to_owned()),
        },
    )
    .expect("complete initial command");
    let completed_moment = workspace
        .moment(&completed.moment)
        .expect("completed head")
        .clone();
    let sequence = workspace.sequence();

    let second_request = exec_request(
        Selector::BranchHead(branch.clone()),
        false,
        "exec-second",
        pending_bound(),
    );
    let mut second_guest = StatefulContinuation::default();
    let second = exec(&mut workspace, &mut second_guest, &second_request)
        .expect("replace completed command");

    assert_eq!(second.branch, branch);
    assert_eq!(second.virtual_time, completed.virtual_time + 1);
    assert_eq!(second_guest.start_calls, 1);
    assert_eq!(second_guest.state.injection_count, 2);
    assert_eq!(
        second
            .command
            .as_ref()
            .expect("second command")
            .invocation
            .engine_id,
        1_002
    );
    assert_eq!(
        second.command.as_ref().expect("second command").completion,
        CommandCompletion::Pending
    );
    assert_eq!(workspace.sequence(), sequence + 1);
    assert_eq!(workspace.moment(&completed.moment), Some(&completed_moment));
    assert!(
        workspace
            .branch(&branch)
            .expect("branch")
            .pending_command
            .is_some()
    );
}

#[test]
fn one_shot_and_split_command_paths_reach_the_same_terminal_state() {
    let (_direct_directory, mut direct_workspace) = super::workspace();
    let direct_request = exec_request(
        Selector::Finding("bug-1".to_owned()),
        true,
        "one-shot",
        completion_bound(),
    );
    let mut direct_guest = StatefulContinuation::default();
    let direct =
        exec(&mut direct_workspace, &mut direct_guest, &direct_request).expect("one-shot command");

    let (_split_directory, mut split_workspace) = super::workspace();
    let split_request = exec_request(
        Selector::Finding("bug-1".to_owned()),
        true,
        "split-start",
        pending_bound(),
    );
    let mut split_start_guest = StatefulContinuation::default();
    let pending = exec(&mut split_workspace, &mut split_start_guest, &split_request)
        .expect("first split segment");
    let mut split_resume_guest = StatefulContinuation::default();
    let split = run(
        &mut split_workspace,
        &mut split_resume_guest,
        &RunRequest {
            branch: pending.branch,
            bound: Advance {
                within_nanos: COMPLETION_DELTA - 1,
                extend: true,
                ..Advance::default()
            },
            request_id: Some("split-resume".to_owned()),
        },
    )
    .expect("second split segment");

    assert_eq!(direct.virtual_time, split.virtual_time);
    assert_eq!(direct.state_hash, split.state_hash);
    let pristine_hash = super::hash(direct.virtual_time);
    assert_ne!(direct.state_hash.as_deref(), Some(pristine_hash.as_str()));
    assert_eq!(direct.stop, split.stop);
    assert_eq!(
        direct.command.as_ref().map(|command| &command.completion),
        split.command.as_ref().map(|command| &command.completion)
    );
    let direct_output = direct
        .command
        .as_ref()
        .and_then(|command| direct_workspace.evidence(&command.evidence))
        .map(|evidence| {
            direct_workspace
                .read_blob(&evidence.blob)
                .expect("direct output")
        });
    let split_output = split
        .command
        .as_ref()
        .and_then(|command| split_workspace.evidence(&command.evidence))
        .map(|evidence| {
            split_workspace
                .read_blob(&evidence.blob)
                .expect("split output")
        });
    assert_eq!(direct_output, Some(COMPLETED_OUTPUT.to_vec()));
    assert_eq!(split_output, direct_output);
}

#[test]
fn committed_exec_retry_rejects_runtime_and_fingerprint_collisions() {
    let (_directory, mut workspace) = super::workspace();
    let request = exec_request(
        Selector::Finding("bug-1".to_owned()),
        true,
        "exec-retry",
        pending_bound(),
    );
    let mut first_guest = StatefulContinuation::default();
    let first = exec(&mut workspace, &mut first_guest, &request).expect("initial exec");
    let sequence = workspace.sequence();
    let branches = workspace
        .branches()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    let moments = workspace.moments().into_iter().cloned().collect::<Vec<_>>();

    let mut rejecting = StatefulContinuation::rejecting();
    let replay = exec(&mut workspace, &mut rejecting, &request).expect("cached exec reply");
    assert!(replay.replayed_request);
    let mut expected_replay = first.clone();
    expected_replay.replayed_request = true;
    assert_eq!(replay, expected_replay);
    assert_eq!(rejecting.open_calls, 0);
    assert_eq!(rejecting.restore_calls, 0);
    assert_eq!(rejecting.advance_calls, 0);
    assert_eq!(rejecting.start_calls, 0);
    assert_eq!(rejecting.capture_calls, 0);

    let mut changed = request.clone();
    changed.argv.push("changed".to_owned());
    let error = exec(&mut workspace, &mut rejecting, &changed)
        .expect_err("same request id with changed argv");
    assert!(error.to_string().contains("different arguments"), "{error}");
    assert_eq!(workspace.sequence(), sequence);
    assert_eq!(
        workspace
            .branches()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>(),
        branches
    );
    assert_eq!(
        workspace.moments().into_iter().cloned().collect::<Vec<_>>(),
        moments
    );
    assert_eq!(rejecting.open_calls, 0);
    assert_eq!(rejecting.restore_calls, 0);
    assert_eq!(rejecting.advance_calls, 0);
    assert_eq!(rejecting.start_calls, 0);
    assert_eq!(rejecting.capture_calls, 0);
}

#[test]
fn empty_exec_request_id_is_rejected_before_runtime() {
    let (_directory, mut workspace) = super::workspace();
    let initial_sequence = workspace.sequence();
    let request = exec_request(
        Selector::Finding("bug-1".to_owned()),
        true,
        "",
        pending_bound(),
    );
    let mut rejecting = StatefulContinuation::rejecting();
    let error = exec(&mut workspace, &mut rejecting, &request).expect_err("empty request id");

    assert!(error.to_string().contains("request id"), "{error}");
    assert_eq!(workspace.sequence(), initial_sequence);
    assert!(workspace.branches().is_empty());
    assert_eq!(rejecting.open_calls, 0);
    assert_eq!(rejecting.restore_calls, 0);
    assert_eq!(rejecting.advance_calls, 0);
    assert_eq!(rejecting.start_calls, 0);
    assert_eq!(rejecting.capture_calls, 0);
}

#[test]
fn branch_at_head_probe_leaves_the_source_unchanged() {
    let (_directory, mut workspace) = super::workspace();
    let mut fork_guest =
        FakeContinuation::open(endpoint(super::SOURCE_AT), Ok(captured(super::SOURCE_AT)));
    fork(
        &mut workspace,
        &mut fork_guest,
        &fork_request("trace", None),
    )
    .expect("recorded branch");
    let source_branch = workspace.branch("trace").expect("trace branch").clone();
    let source_moment = workspace
        .moment(&source_branch.head)
        .expect("trace head")
        .clone();
    let source_finding = workspace.finding("bug-1").expect("finding").clone();
    let sequence = workspace.sequence();

    let request = exec_request(
        Selector::BranchAt("trace".to_owned(), super::SOURCE_AT),
        true,
        "probe-at-head",
        pending_bound(),
    );
    let mut guest = StatefulContinuation::default();
    let probe = exec(&mut workspace, &mut guest, &request).expect("probe branch");

    assert_eq!(probe.branch, "probe-1");
    assert_eq!(workspace.sequence(), sequence + 1);
    assert_eq!(workspace.branch("trace"), Some(&source_branch));
    assert_eq!(workspace.moment(&source_branch.head), Some(&source_moment));
    assert_eq!(workspace.finding("bug-1"), Some(&source_finding));
    assert!(workspace.branch(&probe.branch).expect("probe").probe);
    assert!(
        workspace
            .branch(&probe.branch)
            .expect("probe")
            .pending_command
            .is_some()
    );
}

#[test]
fn failed_command_advance_publishes_no_probe() {
    let (_directory, mut workspace) = super::workspace();
    let initial_sequence = workspace.sequence();
    let request = exec_request(
        Selector::Finding("bug-1".to_owned()),
        true,
        "failed-advance",
        pending_bound(),
    );
    let mut guest = StatefulContinuation::with_advance_failure();
    let error = exec(&mut workspace, &mut guest, &request).expect_err("advance failure");

    assert!(error.to_string().contains("advance"), "{error}");
    assert_eq!(guest.start_calls, 1);
    assert_eq!(guest.advance_calls, 1);
    assert_eq!(guest.capture_calls, 1);
    assert_eq!(workspace.sequence(), initial_sequence);
    assert!(workspace.branches().is_empty());
    assert_eq!(workspace.moments().len(), 1);
}

#[test]
fn failed_final_capture_publishes_no_probe() {
    let (_directory, mut workspace) = super::workspace();
    let initial_sequence = workspace.sequence();
    let request = exec_request(
        Selector::Finding("bug-1".to_owned()),
        true,
        "failed-capture",
        pending_bound(),
    );
    let mut guest = StatefulContinuation::with_capture_failure(2);
    let error = exec(&mut workspace, &mut guest, &request).expect_err("capture failure");

    assert!(error.to_string().contains("capture"), "{error}");
    assert_eq!(guest.start_calls, 1);
    assert_eq!(guest.advance_calls, 1);
    assert_eq!(guest.capture_calls, 2);
    assert_eq!(workspace.sequence(), initial_sequence);
    assert!(workspace.branches().is_empty());
    assert_eq!(workspace.moments().len(), 1);
}

#[test]
fn probe_names_skip_gaps_and_collisions() {
    let (_directory, mut workspace) = super::workspace();
    for name in ["probe-1", "probe-3"] {
        let mut request = fork_request(name, None);
        request.probe = true;
        let mut guest =
            FakeContinuation::open(endpoint(super::SOURCE_AT), Ok(captured(super::SOURCE_AT)));
        fork(&mut workspace, &mut guest, &request).expect("existing probe branch");
    }

    let first_request = exec_request(
        Selector::Finding("bug-1".to_owned()),
        true,
        "probe-gap-1",
        pending_bound(),
    );
    let mut first_guest = StatefulContinuation::default();
    let first = exec(&mut workspace, &mut first_guest, &first_request).expect("probe-2");
    assert_eq!(first.branch, "probe-2");

    let second_request = exec_request(
        Selector::Finding("bug-1".to_owned()),
        true,
        "probe-gap-2",
        pending_bound(),
    );
    let mut second_guest = StatefulContinuation::default();
    let second = exec(&mut workspace, &mut second_guest, &second_request).expect("probe-4");
    assert_eq!(second.branch, "probe-4");
    assert!(workspace.branch("probe-2").is_some());
    assert!(workspace.branch("probe-4").is_some());
}
