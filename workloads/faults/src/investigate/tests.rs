// SPDX-License-Identifier: AGPL-3.0-or-later

//! Investigation checks over a deterministic stand-in guest.
//!
//! The stand-in is not the Consonance backend and proves nothing about KVM
//! execution. It does prove the parts that are the same everywhere: that the
//! verbs commit atomically, that a request id makes a retry return its
//! committed result, that a modified history is marked, and that splitting one
//! advance across calls and processes reaches the same point. The Consonance
//! backend is exercised against the same trait on a Linux KVM host.

use super::*;
use crate::declarations::Declarations;
use crate::target::FaultStop;
use crate::workspace::{Finding, WorkspaceFacts};

const ROOT_SEAL: u64 = 1_000;
const HORIZON: u64 = 500_000_000;
/// Virtual time the model's checker reaches its verdict.
const VERDICT_AT: u64 = ROOT_SEAL + 2 * HORIZON;
/// Virtual time the model's property 2 fails.
const FAILURE_AT: u64 = ROOT_SEAL + 4 * HORIZON;

/// The persisted state of the stand-in guest. A checkpoint is exactly this,
/// so restoring one in a fresh instance continues the same execution.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct ModelState {
    virtual_time: u64,
    console: Vec<u8>,
    events: Vec<SdkEventRecord>,
    position: u64,
    delivered: Vec<Vec<String>>,
    actions: Vec<FaultAction>,
    /// Raised by a logging command; more console output per second afterwards.
    verbose: bool,
    /// A command still running when its bound expired.
    pending: Option<Vec<String>>,
}

/// A deterministic guest: every observable is a function of the restored state
/// and the virtual time advanced, never of how the advance was split.
#[derive(Debug, Default)]
struct ModelGuest {
    state: ModelState,
    /// Host seconds the guest pretends each virtual second costs, so a
    /// watchdog can be exercised.
    wall_cost_per_second: u64,
}

impl ModelGuest {
    fn new() -> Self {
        Self::default()
    }

    /// Emit the console lines and SDK reports of the interval
    /// `(virtual_time, target]`, stopping early on a watched condition.
    fn step(&mut self, target: u64, until: Option<Condition>) -> (u64, bool) {
        let mut scheduled: Vec<(u64, u32, bool)> =
            vec![(VERDICT_AT, 24, false), (FAILURE_AT, 2, true)];
        scheduled.sort_unstable();
        for (at, event, violation) in scheduled {
            if at <= self.state.virtual_time || at > target {
                continue;
            }
            self.emit_console(at);
            self.state.virtual_time = at;
            self.state.position = self.state.position.saturating_add(1);
            self.state.events.push(SdkEventRecord {
                position: self.state.position,
                virtual_time: at,
                event,
                payload: if violation {
                    "02".to_owned()
                } else {
                    "01".to_owned()
                },
            });
            let matched = match until {
                Some(Condition::AssertionFail(id)) => violation && id == event,
                Some(Condition::AssertionHit(id)) => !violation && id == event,
                None => false,
            };
            if matched {
                return (at, true);
            }
        }
        self.emit_console(target);
        self.state.virtual_time = target;
        (target, false)
    }

    /// One line per whole virtual second crossed, so splitting an advance
    /// cannot change the transcript.
    fn emit_console(&mut self, target: u64) {
        let second = 1_000_000_000;
        let from = self.state.virtual_time / second;
        let to = target / second;
        for mark in (from + 1)..=to {
            self.state
                .console
                .extend_from_slice(format!("LOG:  second {mark}\n").as_bytes());
            if self.state.verbose {
                self.state
                    .console
                    .extend_from_slice(format!("DEBUG:  second {mark} detail\n").as_bytes());
            }
        }
    }

    fn observations(&self) -> FaultObservations {
        let mut observations = FaultObservations {
            moment: self.state.virtual_time,
            ..FaultObservations::default()
        };
        for event in &self.state.events {
            if event.payload == "02" {
                observations.violations.insert(event.event);
                observations.stop = FaultStop::Assertion { point: event.event };
            } else {
                observations.sometimes.insert(event.event);
            }
        }
        observations
    }

    fn endpoint(&self, stop: StopReason, condition_met: bool) -> Endpoint {
        Endpoint {
            virtual_time: self.state.virtual_time,
            stop,
            observations: self.observations(),
            condition_met,
        }
    }
}

impl Continuation for ModelGuest {
    fn open_recorded(
        &mut self,
        actions: &[FaultAction],
        rewind_nanos: u64,
    ) -> Result<Endpoint, String> {
        let end = ROOT_SEAL.saturating_add(HORIZON.saturating_mul(actions.len() as u64));
        let start = end
            .checked_sub(rewind_nanos)
            .ok_or("rewind precedes boot")?;
        self.state = ModelState {
            actions: actions.to_vec(),
            ..ModelState::default()
        };
        self.step(start, None);
        Ok(self.endpoint(StopReason::VirtualDeadline, false))
    }

    fn restore(&mut self, checkpoint: &[u8]) -> Result<Endpoint, String> {
        self.state =
            serde_json::from_slice(checkpoint).map_err(|error| format!("checkpoint: {error}"))?;
        Ok(self.endpoint(StopReason::VirtualDeadline, false))
    }

    fn advance(&mut self, bound: &Advance) -> Result<Endpoint, String> {
        if let Some(argv) = self.state.pending.take() {
            self.state.delivered.push(argv);
        }
        let target = self.state.virtual_time.saturating_add(bound.within_nanos);
        if self.wall_cost_per_second > 0
            && bound.within_nanos / 1_000_000_000 * self.wall_cost_per_second > bound.wall_seconds
        {
            let reachable = bound
                .wall_seconds
                .saturating_div(self.wall_cost_per_second.max(1))
                .saturating_mul(1_000_000_000);
            self.step(self.state.virtual_time.saturating_add(reachable), None);
            return Ok(self.endpoint(StopReason::HostWatchdog, false));
        }
        let (_, met) = self.step(target, bound.until);
        let stop = if met {
            StopReason::ConditionMet
        } else {
            StopReason::VirtualDeadline
        };
        Ok(self.endpoint(stop, met))
    }

    fn exec(&mut self, argv: &[String], bound: &Advance) -> Result<CommandOutcome, String> {
        let joined = argv.join(" ");
        // A command asking for more logging changes the transcript from here
        // on, which is what an investigation is for.
        if joined.contains("log_min_messages") {
            self.state.verbose = true;
        }
        // The model's long command outlives a bound under 500 ms.
        let long = joined.contains("sleep");
        let completed = !long || bound.within_nanos >= 500_000_000;
        if completed {
            self.state.delivered.push(argv.to_vec());
        } else {
            self.state.pending = Some(argv.to_vec());
        }
        let (_, _) = self.step(
            self.state.virtual_time.saturating_add(bound.within_nanos),
            None,
        );
        let stop = if completed {
            StopReason::CommandComplete
        } else {
            StopReason::VirtualDeadline
        };
        Ok(CommandOutcome {
            endpoint: self.endpoint(stop, false),
            output: format!("{joined}\n").into_bytes(),
            completed,
            exit_status: completed.then_some(if joined.contains("false") { 1 } else { 0 }),
        })
    }

    fn checkpoint(&mut self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(&self.state).map_err(|error| error.to_string())
    }

    fn console(&mut self) -> Result<Vec<u8>, String> {
        Ok(self.state.console.clone())
    }

    fn events(&mut self) -> Result<Vec<SdkEventRecord>, String> {
        Ok(self.state.events.clone())
    }

    fn state_hash(&mut self) -> Result<String, String> {
        let bytes = self.checkpoint()?;
        Ok(format!("{:x}", sha2::Sha256::digest(&bytes)))
    }

    fn read_memory(&mut self, gpa: u64, len: u32) -> Result<Vec<u8>, String> {
        Ok((0..len)
            .map(|index| (gpa.wrapping_add(u64::from(index))) as u8)
            .collect())
    }
}

use sha2::Digest;

fn declarations() -> Declarations {
    Declarations::parse(
        "node postgres /opt/harmony/node.sh\n\
         hook 3 /opt/harmony/hooks.sh 3\n\
         describe hook 3 pg_amcheck --heapallindexed against cic_k_idx\n\
         assert always 2 from 3 every required heap tuple has a matching index entry\n\
         assert sometimes 24 the checker reached a verdict\n\
         assert sometimes 25 a concurrent build overlapped a vacuum\n\
         diagnostic amcheck sh -c cat /run/amcheck.out\n",
    )
    .expect("declarations")
}

fn facts() -> WorkspaceFacts {
    WorkspaceFacts {
        format: crate::workspace::FORMAT.to_owned(),
        package: crate::package::PACKAGE.to_owned(),
        identity: "faults-model-v1".to_owned(),
        image: "pgcic-14.3.oci".to_owned(),
        root_seal: ROOT_SEAL,
        horizon_nanos: HORIZON,
        ram_mib: 1024,
        seed: 7,
        executions: 4_000,
        declarations: declarations(),
        ..WorkspaceFacts::default()
    }
}

fn finding() -> Finding {
    Finding {
        id: "bug-1".to_owned(),
        execution: 1_212,
        actions: vec![
            FaultAction::Hook(1),
            FaultAction::Hook(2),
            FaultAction::Kill(0),
            FaultAction::Hook(3),
        ],
        standing: "0001".to_owned(),
        observations: FaultObservations {
            moment: FAILURE_AT,
            stop: FaultStop::Assertion { point: 2 },
            ..FaultObservations::default()
        },
        violations: vec![2],
        evaluated: vec![2, 24],
        moment: "m-0001".to_owned(),
        confirmed: true,
        state_hash: "beef".to_owned(),
    }
}

fn workspace() -> (tempfile::TempDir, Workspace) {
    let directory = tempfile::tempdir().expect("temp dir");
    let mut workspace = Workspace::create(directory.path(), facts()).expect("create");
    workspace
        .commit(vec![
            Record::Moment(Box::new(MomentRecord {
                id: "m-0001".to_owned(),
                virtual_time: FAILURE_AT,
                history: History::Recorded,
                stop: Some(StopReason::Assertion { point: 2 }),
                ..MomentRecord::default()
            })),
            Record::Finding(Box::new(finding())),
        ])
        .expect("commit");
    (directory, workspace)
}

fn fork_request(name: &str, rewind: u64) -> ForkRequest {
    ForkRequest {
        source: Selector::Finding("bug-1".to_owned()),
        rewind_nanos: rewind,
        name: name.to_owned(),
        probe: false,
        request_id: None,
    }
}

fn advance(within: u64) -> Advance {
    Advance {
        within_nanos: within,
        until: None,
        extend: true,
        wall_seconds: DEFAULT_WALL_SECONDS,
    }
}

#[test]
fn a_fork_names_its_branch_and_an_immutable_starting_moment() {
    let (_directory, mut workspace) = workspace();
    let mut guest = ModelGuest::new();
    let outcome = fork(&mut workspace, &mut guest, &fork_request("trace", HORIZON))
        .expect("fork the finding");
    assert_eq!(outcome.branch, "trace");
    assert_eq!(outcome.history, History::Recorded);
    assert_eq!(outcome.virtual_time, FAILURE_AT - HORIZON);
    let branch = workspace.branch("trace").expect("the branch is committed");
    assert_eq!(branch.source, "bug-1");
    assert_eq!(branch.start, outcome.moment);
    assert_eq!(branch.head, outcome.moment);
    assert_eq!(branch.continuation_end, FAILURE_AT);
    assert_eq!(branch.inherited_actions, finding().actions);
    assert!(
        workspace
            .moment(&outcome.moment)
            .expect("moment")
            .checkpoint
            .is_some(),
        "a fork retains the checkpoint its branch head names"
    );
}

#[test]
fn forking_a_name_that_exists_says_how_to_use_it_instead() {
    let (_directory, mut workspace) = workspace();
    let mut guest = ModelGuest::new();
    fork(&mut workspace, &mut guest, &fork_request("trace", 0)).expect("fork");
    let error = fork(&mut workspace, &mut guest, &fork_request("trace", 0))
        .expect_err("a second fork under the same name");
    assert!(error.to_string().contains("run trace"), "{error}");
}

#[test]
fn an_unknown_finding_names_the_ones_the_workspace_has() {
    let (_directory, mut workspace) = workspace();
    let mut guest = ModelGuest::new();
    let error = fork(
        &mut workspace,
        &mut guest,
        &ForkRequest {
            source: Selector::Finding("bug-9".to_owned()),
            ..fork_request("trace", 0)
        },
    )
    .expect_err("an unknown finding");
    assert!(error.to_string().contains("bug-1"), "{error}");
}

#[test]
fn a_cold_continuation_reaches_the_same_point_as_one_that_never_exited() {
    let (_directory, mut workspace) = workspace();
    let mut warm = ModelGuest::new();
    fork(
        &mut workspace,
        &mut warm,
        &fork_request("warm", 4 * HORIZON),
    )
    .expect("fork");
    let long = run(
        &mut workspace,
        &mut warm,
        &RunRequest {
            branch: "warm".to_owned(),
            bound: advance(4 * HORIZON),
            request_id: None,
        },
    )
    .expect("one long advance");

    // The same amount of guest time, split across two calls, each of which
    // restores the committed checkpoint in a guest instance that has never
    // seen this execution.
    let mut cold = ModelGuest::new();
    fork(
        &mut workspace,
        &mut cold,
        &fork_request("cold", 4 * HORIZON),
    )
    .expect("fork");
    for _ in 0..2 {
        let mut fresh = ModelGuest::new();
        run(
            &mut workspace,
            &mut fresh,
            &RunRequest {
                branch: "cold".to_owned(),
                bound: advance(2 * HORIZON),
                request_id: None,
            },
        )
        .expect("half an advance in a fresh process");
    }
    let split = workspace
        .moment(&workspace.branch("cold").expect("branch").head.clone())
        .expect("head moment")
        .clone();
    let whole = workspace.moment(&long.moment).expect("head moment");
    assert_eq!(split.virtual_time, whole.virtual_time);
    assert_eq!(split.state_hash, whole.state_hash);
    assert_eq!(split.observations, whole.observations);
}

#[test]
fn a_watched_condition_stops_on_a_new_report_and_a_deadline_is_not_a_pass() {
    let (_directory, mut workspace) = workspace();
    let mut guest = ModelGuest::new();
    fork(
        &mut workspace,
        &mut guest,
        &fork_request("trace", 4 * HORIZON),
    )
    .expect("fork");
    let hit = run(
        &mut workspace,
        &mut guest,
        &RunRequest {
            branch: "trace".to_owned(),
            bound: Advance {
                until: Some(Condition::AssertionFail(2)),
                ..advance(8 * HORIZON)
            },
            request_id: None,
        },
    )
    .expect("watch for the failure");
    assert_eq!(hit.stop, StopReason::ConditionMet);
    assert!(hit.condition_met);
    assert_eq!(hit.virtual_time, FAILURE_AT);
    assert_eq!(hit.violations, vec![2]);

    // Watching again past the failure finds no new report and says so.
    let missed = run(
        &mut workspace,
        &mut guest,
        &RunRequest {
            branch: "trace".to_owned(),
            bound: Advance {
                until: Some(Condition::AssertionFail(2)),
                ..advance(2 * HORIZON)
            },
            request_id: None,
        },
    )
    .expect("watch again");
    assert_eq!(missed.stop, StopReason::VirtualDeadline);
    assert!(
        !missed.condition_met,
        "a deadline is not a passing property"
    );
}

#[test]
fn a_declared_property_with_no_evaluation_is_reported_unevaluated() {
    let (_directory, mut workspace) = workspace();
    let mut guest = ModelGuest::new();
    let outcome = fork(
        &mut workspace,
        &mut guest,
        &fork_request("trace", 4 * HORIZON),
    )
    .expect("fork");
    assert!(
        outcome.unevaluated.contains(&25),
        "silence from property 25 is not a pass: {:?}",
        outcome.unevaluated
    );
}

#[test]
fn advancing_past_the_recorded_continuation_needs_extend() {
    let (_directory, mut workspace) = workspace();
    let mut guest = ModelGuest::new();
    fork(&mut workspace, &mut guest, &fork_request("trace", 0)).expect("fork at the failure");
    let refused = run(
        &mut workspace,
        &mut guest,
        &RunRequest {
            branch: "trace".to_owned(),
            bound: Advance {
                extend: false,
                ..advance(HORIZON)
            },
            request_id: None,
        },
    )
    .expect_err("the recorded continuation has ended");
    assert!(refused.to_string().contains("--extend"), "{refused}");
    run(
        &mut workspace,
        &mut guest,
        &RunRequest {
            branch: "trace".to_owned(),
            bound: advance(HORIZON),
            request_id: None,
        },
    )
    .expect("--extend runs past it");
}

#[test]
fn exec_marks_the_history_modified_and_leaves_the_finding_reproducible() {
    let (_directory, mut workspace) = workspace();
    let mut guest = ModelGuest::new();
    fork(
        &mut workspace,
        &mut guest,
        &fork_request("trace", 2 * HORIZON),
    )
    .expect("fork");
    let outcome = exec(
        &mut workspace,
        &mut guest,
        &ExecRequest {
            target: Selector::BranchHead("trace".to_owned()),
            argv: vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "echo diagnostic".to_owned(),
            ],
            bound: advance(DEFAULT_EXEC_NANOS),
            request_id: Some("diagnostic-1".to_owned()),
        },
    )
    .expect("exec");
    assert_eq!(outcome.history, History::Modified);
    assert_eq!(outcome.command_completed, Some(true));
    assert_eq!(outcome.exit_status, Some(0));
    assert!(!outcome.evidence.is_empty());
    assert_eq!(
        workspace.branch("trace").expect("branch").history,
        History::Modified
    );
    assert!(
        workspace.finding("bug-1").expect("finding").confirmed,
        "the original finding is untouched"
    );
    let moment = workspace.moment(&outcome.moment).expect("moment");
    assert_eq!(moment.history, History::Modified);
    assert!(
        moment.checkpoint.is_some(),
        "a modified point keeps its checkpoint: no earlier input rebuilds it"
    );
}

#[test]
fn a_retried_request_returns_its_committed_result_without_running_the_guest() {
    let (_directory, mut workspace) = workspace();
    let mut guest = ModelGuest::new();
    fork(
        &mut workspace,
        &mut guest,
        &fork_request("trace", 2 * HORIZON),
    )
    .expect("fork");
    let request = ExecRequest {
        target: Selector::BranchHead("trace".to_owned()),
        argv: vec!["sh".to_owned(), "-c".to_owned(), "echo once".to_owned()],
        bound: advance(DEFAULT_EXEC_NANOS),
        request_id: Some("diagnostic-1".to_owned()),
    };
    let first = exec(&mut workspace, &mut guest, &request).expect("first");
    let sequence = workspace.sequence();
    let delivered = guest.state.delivered.len();
    let second = exec(&mut workspace, &mut guest, &request).expect("retry");
    assert!(second.replayed_request);
    assert_eq!(second.moment, first.moment);
    assert_eq!(second.exit_status, first.exit_status);
    assert_eq!(
        guest.state.delivered.len(),
        delivered,
        "an interrupted command is never silently delivered twice"
    );
    assert_eq!(
        workspace.sequence(),
        sequence,
        "a retry commits nothing new"
    );
}

#[test]
fn a_timed_out_command_stays_pending_until_a_run_finishes_it() {
    let (_directory, mut workspace) = workspace();
    let mut guest = ModelGuest::new();
    fork(
        &mut workspace,
        &mut guest,
        &fork_request("trace", 4 * HORIZON),
    )
    .expect("fork");
    let outcome = exec(
        &mut workspace,
        &mut guest,
        &ExecRequest {
            target: Selector::BranchHead("trace".to_owned()),
            argv: vec!["sh".to_owned(), "-c".to_owned(), "sleep 5".to_owned()],
            bound: advance(100_000_000),
            request_id: Some("slow-1".to_owned()),
        },
    )
    .expect("exec");
    assert_eq!(outcome.command_completed, Some(false));
    assert_eq!(
        outcome.exit_status, None,
        "an incomplete command has no exit status"
    );
    let pending = workspace
        .branch("trace")
        .expect("branch")
        .pending_command
        .clone()
        .expect("the capture stays pending");
    assert_eq!(pending.request_id, "slow-1");
    let refused = exec(
        &mut workspace,
        &mut guest,
        &ExecRequest {
            target: Selector::BranchHead("trace".to_owned()),
            argv: vec!["true".to_owned()],
            bound: advance(DEFAULT_EXEC_NANOS),
            request_id: Some("next-1".to_owned()),
        },
    )
    .expect_err("a second command while one is pending");
    assert!(refused.to_string().contains("run trace"), "{refused}");
    run(
        &mut workspace,
        &mut guest,
        &RunRequest {
            branch: "trace".to_owned(),
            bound: advance(HORIZON),
            request_id: None,
        },
    )
    .expect("a run finishes it");
    assert!(
        workspace
            .branch("trace")
            .expect("branch")
            .pending_command
            .is_none()
    );
}

#[test]
fn a_probe_reads_evidence_without_moving_the_source() {
    let (_directory, mut workspace) = workspace();
    let mut guest = ModelGuest::new();
    fork(
        &mut workspace,
        &mut guest,
        &fork_request("trace", 4 * HORIZON),
    )
    .expect("fork");
    let before = workspace.branch("trace").expect("branch").head.clone();
    let probe = exec(
        &mut workspace,
        &mut guest,
        &ExecRequest {
            target: Selector::Finding("bug-1".to_owned()),
            argv: vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "cat /run/amcheck.out".to_owned(),
            ],
            bound: advance(DEFAULT_EXEC_NANOS),
            request_id: None,
        },
    )
    .expect("probe the finding");
    assert_eq!(probe.branch, "probe-1");
    assert_eq!(probe.history, History::Modified);
    assert_eq!(
        workspace.branch("trace").expect("branch").head,
        before,
        "the named branch did not move"
    );
    assert!(workspace.branch("probe-1").expect("probe").probe);
}

#[test]
fn a_logging_change_survives_a_cold_continuation() {
    let (_directory, mut workspace) = workspace();
    let mut guest = ModelGuest::new();
    fork(
        &mut workspace,
        &mut guest,
        &fork_request("trace", 4 * HORIZON),
    )
    .expect("fork");
    exec(
        &mut workspace,
        &mut guest,
        &ExecRequest {
            target: Selector::BranchHead("trace".to_owned()),
            argv: vec![
                "psql".to_owned(),
                "-c".to_owned(),
                "ALTER SYSTEM SET log_min_messages = 'debug5';".to_owned(),
            ],
            bound: advance(DEFAULT_EXEC_NANOS),
            request_id: None,
        },
    )
    .expect("raise logging");
    // A guest instance that has never seen this execution restores the
    // committed checkpoint and must carry the changed setting.
    let mut fresh = ModelGuest::new();
    let outcome = run(
        &mut workspace,
        &mut fresh,
        &RunRequest {
            branch: "trace".to_owned(),
            bound: advance(2_000_000_000),
            request_id: None,
        },
    )
    .expect("advance in a fresh process");
    let console = String::from_utf8(
        workspace
            .read_blob(
                &workspace
                    .evidence_at(&outcome.moment)
                    .into_iter()
                    .find(|record| record.kind == "console")
                    .expect("console evidence")
                    .blob,
            )
            .expect("read"),
    )
    .expect("utf8");
    assert!(
        console.contains("DEBUG:"),
        "the raised logging survived: {console}"
    );
}

#[test]
fn the_host_watchdog_bounds_host_waiting_separately_from_virtual_time() {
    let (_directory, mut workspace) = workspace();
    let mut guest = ModelGuest::new();
    guest.wall_cost_per_second = 10;
    fork(
        &mut workspace,
        &mut guest,
        &fork_request("trace", 4 * HORIZON),
    )
    .expect("fork");
    let outcome = run(
        &mut workspace,
        &mut guest,
        &RunRequest {
            branch: "trace".to_owned(),
            bound: Advance {
                wall_seconds: 5,
                ..advance(10_000_000_000)
            },
            request_id: None,
        },
    )
    .expect("run");
    assert_eq!(outcome.stop, StopReason::HostWatchdog);
    assert!(!outcome.condition_met);
}

#[test]
fn a_nonzero_command_reports_its_exit_status() {
    let (_directory, mut workspace) = workspace();
    let mut guest = ModelGuest::new();
    fork(
        &mut workspace,
        &mut guest,
        &fork_request("trace", 4 * HORIZON),
    )
    .expect("fork");
    let outcome = exec(
        &mut workspace,
        &mut guest,
        &ExecRequest {
            target: Selector::BranchHead("trace".to_owned()),
            argv: vec!["sh".to_owned(), "-c".to_owned(), "false".to_owned()],
            bound: advance(DEFAULT_EXEC_NANOS),
            request_id: None,
        },
    )
    .expect("exec");
    assert_eq!(outcome.exit_status, Some(1));
    assert_eq!(outcome.command_completed, Some(true));
}

#[test]
fn an_empty_command_says_how_to_write_a_shell_expression() {
    let (_directory, mut workspace) = workspace();
    let mut guest = ModelGuest::new();
    fork(
        &mut workspace,
        &mut guest,
        &fork_request("trace", 4 * HORIZON),
    )
    .expect("fork");
    let error = exec(
        &mut workspace,
        &mut guest,
        &ExecRequest {
            target: Selector::BranchHead("trace".to_owned()),
            argv: Vec::new(),
            bound: advance(DEFAULT_EXEC_NANOS),
            request_id: None,
        },
    )
    .expect_err("no command");
    assert!(error.to_string().contains("sh -c"), "{error}");
}

#[test]
fn export_separates_the_recorded_reproducer_from_investigation_evidence() {
    let (_directory, mut workspace) = workspace();
    let mut guest = ModelGuest::new();
    fork(
        &mut workspace,
        &mut guest,
        &fork_request("trace", 2 * HORIZON),
    )
    .expect("fork");
    exec(
        &mut workspace,
        &mut guest,
        &ExecRequest {
            target: Selector::BranchHead("trace".to_owned()),
            argv: vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "echo diagnostic".to_owned(),
            ],
            bound: advance(DEFAULT_EXEC_NANOS),
            request_id: None,
        },
    )
    .expect("exec");
    let destination = tempfile::tempdir().expect("temp dir");
    let result = export(&workspace, "bug-1", destination.path(), true).expect("export");
    assert!(result.verification_scope.contains("anchor"), "{:?}", result);
    let reproducer: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(destination.path().join("reproducer.json")).expect("read"),
    )
    .expect("json");
    assert_eq!(reproducer["history"], "recorded");
    assert_eq!(reproducer["finding"], "bug-1");
    assert_eq!(
        reproducer["actions"].as_array().expect("actions").len(),
        finding().actions.len()
    );
    assert!(
        destination.path().join("evidence/provenance.json").exists(),
        "investigation evidence carries its own provenance"
    );
    assert!(result.evidence.len() > 1);
}

#[test]
fn an_unconfirmed_finding_exports_as_unverified() {
    let directory = tempfile::tempdir().expect("temp dir");
    let mut workspace = Workspace::create(directory.path(), facts()).expect("create");
    workspace
        .commit(vec![Record::Finding(Box::new(Finding {
            confirmed: false,
            ..finding()
        }))])
        .expect("commit");
    let destination = tempfile::tempdir().expect("temp dir");
    let result = export(&workspace, "bug-1", destination.path(), false).expect("export");
    assert!(
        result.verification_scope.contains("unverified"),
        "{result:?}"
    );
    assert!(result.evidence.is_empty());
}

#[test]
fn a_moment_with_no_retained_checkpoint_is_refused_rather_than_substituted() {
    let (_directory, mut workspace) = workspace();
    let mut guest = ModelGuest::new();
    let error = fork(
        &mut workspace,
        &mut guest,
        &ForkRequest {
            source: Selector::Moment("m-0001".to_owned()),
            ..fork_request("trace", 0)
        },
    )
    .expect_err("m-0001 retained no checkpoint");
    assert!(
        error.to_string().contains("retained no checkpoint"),
        "{error}"
    );
}

#[test]
fn watched_conditions_parse_and_round_trip() {
    assert_eq!(
        Condition::parse("assertion:2:fail").expect("parse"),
        Condition::AssertionFail(2)
    );
    assert_eq!(
        Condition::parse("assertion:24:hit").expect("parse"),
        Condition::AssertionHit(24)
    );
    assert_eq!(
        Condition::parse("assertion:2").expect("parse"),
        Condition::AssertionFail(2)
    );
    assert_eq!(Condition::AssertionHit(24).selector(), "assertion:24:hit");
    assert_eq!(Condition::AssertionFail(2).point(), 2);
    for bad in ["crash", "assertion:x:fail", "assertion:2:maybe"] {
        assert!(Condition::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn every_outcome_names_a_next_valid_command() {
    let (_directory, mut workspace) = workspace();
    let mut guest = ModelGuest::new();
    let outcome = fork(
        &mut workspace,
        &mut guest,
        &fork_request("trace", 2 * HORIZON),
    )
    .expect("fork");
    assert!(
        outcome
            .next
            .iter()
            .any(|command| command.contains("run trace")),
        "{:?}",
        outcome.next
    );
    let run_outcome = run(
        &mut workspace,
        &mut guest,
        &RunRequest {
            branch: "trace".to_owned(),
            bound: advance(HORIZON),
            request_id: None,
        },
    )
    .expect("run");
    assert!(
        run_outcome
            .next
            .iter()
            .any(|command| command.contains("inspect trace@head events")),
        "{:?}",
        run_outcome.next
    );
}
