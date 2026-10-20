// SPDX-License-Identifier: AGPL-3.0-or-later

use super::super::CoreCheckpoint;
use super::*;
use crate::search::archive::Archive;
use crate::search::campaign::{
    ExpansionCoordinator, ExpansionDecision, ExpansionEdgeId, ExpansionEvidence,
    ExpansionTaskOutcome, ExpansionTaskSpec, ExpansionWatch, ExpansionWorkload, run_expansion_task,
};
use crate::search::checkpoint::{CheckpointPlan, CheckpointReader};

std::thread_local! {
    static EXPANSION_WITNESSES: std::cell::RefCell<Vec<(u64, Vec<TestAction>)>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl ExpansionWorkload for TestWorkload {
    type ExpansionQuality = u8;
    type ExpansionBucket = u8;

    fn expansion_policy(&self) -> &'static str {
        "test_forward_value_distinct_place_v1"
    }
    fn expansion_quality(&self, key: TestKey) -> u8 {
        key.0
    }
    fn expansion_bucket(&self, key: TestKey) -> u8 {
        key.0
    }
    fn expansion_observation_memory_charge(_: &()) -> usize {
        0
    }
    fn validate_expansion_action(
        &self,
        requested: TestAction,
        recorded: TestAction,
        disposition: ExecutionDisposition,
    ) -> bool {
        requested == recorded
            || disposition == ExecutionDisposition::Terminal
                && requested.input == recorded.input
                && recorded.work_units <= requested.work_units
    }
    fn merge_expansion_evidence<F>(
        &self,
        evidence: &mut u8,
        action: &ExpansionEvidence<Self>,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<Input<TestAction>, Box<dyn Error>>,
    {
        *evidence = evidence.checked_add(1).ok_or("fixture evidence overflow")?;
        if action.preparation_failure.is_some()
            || action.outcome.objective_reached
            || action.outcome.disposition != ExecutionDisposition::Runnable
        {
            let actions = input()?.actions;
            EXPANSION_WITNESSES.with(|paths| paths.borrow_mut().push((sequence, actions)));
        }
        Ok(())
    }
}

fn expansion_config(budget: u64, window: usize) -> CampaignConfig<TestWorkload> {
    let mut config = three_worker_config();
    config.execution_budget = budget;
    config.window = window;
    config.stop_rollout_on_objective = true;
    config.archive_entry_limit = 512;
    config
}

fn expansion_workload() -> TestWorkload {
    TestWorkload {
        bootstrap_objective: false,
        targets: None,
    }
}

fn execute_expansion(
    spec: &ExpansionTaskSpec<TestWorkload>,
    workload: &TestWorkload,
) -> ExpansionTaskOutcome<TestWorkload> {
    let mut target = workload.new_target().unwrap();
    run_expansion_task(workload, &(), &mut target, spec, &()).unwrap()
}

type TraceProjectionRow = (
    u32,
    Vec<TestAction>,
    Option<TestKey>,
    Option<u64>,
    bool,
    String,
    u64,
);

fn trace_projection(outcome: &ExpansionTaskOutcome<TestWorkload>) -> Vec<TraceProjectionRow> {
    outcome
        .trace
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .skip(1)
        .map(|node| {
            let evidence = &node.evidence.as_ref().unwrap().evidence;
            (
                node.ordinal,
                node.suffix.clone(),
                node.facts.as_ref().map(|facts| facts.key),
                node.cost_in_group,
                node.objective,
                evidence.result_sha256.clone(),
                evidence.execution_work,
            )
        })
        .collect()
}

fn run_expansion_completion_order(reverse: bool) -> (Vec<u8>, u8, Vec<(u64, String)>, usize) {
    let workload = expansion_workload();
    let config = expansion_config(56, 2);
    let mut coordinator = ExpansionCoordinator::<TestWorkload, ()>::start(
        &workload,
        &config,
        &CampaignOrigin::Genesis,
    )
    .unwrap();
    let mut queued = std::collections::VecDeque::new();
    while let Some(spec) = coordinator.reserve().unwrap() {
        queued.push_back(spec);
    }
    let mut rows = Vec::new();
    let mut attempted = 0;
    while let Some(spec) = if reverse {
        queued.pop_back()
    } else {
        queued.pop_front()
    } {
        let outcome = execute_expansion(&spec, &workload);
        let trace = outcome.trace.as_ref().unwrap();
        attempted += trace.accounting.task_unique_edge_attempts;
        assert!(trace.nodes.iter().all(|node| node.suffix.len() <= 4));
        let archive_before = postcard::to_stdvec(coordinator.archive()).unwrap();
        let evidence_before = *coordinator.evidence();
        coordinator.complete(outcome).unwrap();
        assert_eq!(
            postcard::to_stdvec(coordinator.archive()).unwrap(),
            archive_before
        );
        assert_eq!(*coordinator.evidence(), evidence_before);
        while let Some(admission) = coordinator.admit_next().unwrap() {
            rows.push((admission.outcome.task, format!("{:?}", admission.decisions)));
            while let Some(spec) = coordinator.reserve().unwrap() {
                queued.push_back(spec);
            }
        }
    }
    assert!(coordinator.finished());
    assert!(coordinator.reserve().unwrap().is_none());
    assert!(attempted <= 56);
    (
        postcard::to_stdvec(coordinator.archive()).unwrap(),
        *coordinator.evidence(),
        rows,
        attempted,
    )
}

#[test]
fn expansion_fifo_reordered_task_completions_preserve_admissions_archive_and_evidence() {
    let forward = run_expansion_completion_order(false);
    let reverse = run_expansion_completion_order(true);
    assert_eq!(forward, reverse);
    assert!(forward.2.len() >= 2);
    assert_eq!(usize::from(forward.1), forward.3);
}

#[test]
fn expansion_frontier_siblings_are_admitted_independently_against_the_same_root() {
    let workload = expansion_workload();
    let config = expansion_config(14, 1);
    let mut coordinator = ExpansionCoordinator::<TestWorkload, ()>::start(
        &workload,
        &config,
        &CampaignOrigin::Genesis,
    )
    .unwrap();
    let spec = coordinator.reserve().unwrap().unwrap();
    let root_index = coordinator.archive().index_of_id(spec.parent_id).unwrap();
    let prefix = coordinator
        .archive()
        .entry_input(root_index)
        .unwrap()
        .actions;
    let outcome = execute_expansion(&spec, &workload);
    let trace = outcome.trace.as_ref().unwrap();
    assert_eq!(trace.frontier.len(), 2);
    let expected = trace
        .frontier
        .iter()
        .map(|ordinal| {
            let node = trace
                .nodes
                .iter()
                .find(|node| node.ordinal == *ordinal)
                .unwrap();
            let mut full = prefix.clone();
            full.extend_from_slice(&node.suffix);
            (
                full,
                node.cost_in_group.unwrap(),
                node.facts.as_ref().unwrap().key,
            )
        })
        .collect::<Vec<_>>();
    coordinator.complete(outcome).unwrap();
    let admission = coordinator.admit_next().unwrap().unwrap();
    assert_eq!(
        admission.decisions.len(),
        admission
            .outcome
            .trace
            .as_ref()
            .unwrap()
            .accounting
            .task_unique_edge_attempts
    );
    for (input, cost, key) in expected {
        let archive = coordinator.archive();
        let (index, entry) = archive
            .entries
            .iter()
            .enumerate()
            .find(|(index, entry)| {
                entry.key == key && archive.entry_input(*index).unwrap().actions == input
            })
            .unwrap();
        assert_eq!(entry.parent_id, Some(spec.parent_id));
        assert_eq!(archive.entry_cost_in_group(index), cost);
        assert_eq!(
            archive.entry_input(index).unwrap().actions.len(),
            prefix.len() + 4
        );
    }
}

#[test]
fn expansion_quotas_bound_attempts_without_fabricating_a_genesis_proposal() {
    for budget in [0, 1, 13, 14, 15, 29] {
        let workload = expansion_workload();
        let config = expansion_config(budget, 2);
        let mut coordinator = ExpansionCoordinator::<TestWorkload, ()>::start(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
        )
        .unwrap();
        let mut quota = 0;
        let mut attempts = 0;
        let mut tasks = 0;
        loop {
            let mut specs = Vec::new();
            while let Some(spec) = coordinator.reserve().unwrap() {
                assert!(spec.edge_budget.get() <= 14);
                quota += u64::from(spec.edge_budget.get());
                specs.push(spec);
            }
            if specs.is_empty() {
                assert!(coordinator.finished());
                break;
            }
            let claims = specs
                .iter()
                .map(|spec| u64::from(spec.edge_budget.get()))
                .sum::<u64>();
            assert!(coordinator.counters().budget_consumed + claims <= budget);
            for spec in specs {
                let outcome = execute_expansion(&spec, &workload);
                attempts += outcome
                    .trace
                    .as_ref()
                    .unwrap()
                    .accounting
                    .task_unique_edge_attempts;
                coordinator.complete(outcome).unwrap();
            }
            while let Some(admission) = coordinator.admit_next().unwrap() {
                tasks += 1;
                assert!(
                    !admission
                        .decisions
                        .iter()
                        .any(|decision| decision.id.ordinal == 0)
                );
            }
        }
        assert!(quota >= attempts as u64);
        assert!(attempts as u64 <= budget);
        assert!(coordinator.counters().budget_consumed <= budget);
        assert_eq!(coordinator.counters().edge_attempts, attempts as u64);
        assert_eq!(usize::from(*coordinator.evidence()), attempts);
        if budget == 0 {
            assert_eq!(tasks, 0);
            assert_eq!(attempts, 0);
            assert_eq!(coordinator.archive().entries.len(), 1);
        }
    }
}

struct BranchEvidenceWatch<'a>(&'a TestWorkload);
impl ExpansionWatch<TestWorkload> for BranchEvidenceWatch<'_> {
    type Note = ();
    fn note_memory_charge(&self, _: &()) -> usize {
        0
    }
    fn run_edge(
        &self,
        id: ExpansionEdgeId,
        job: &JobSpec<TestWorkload>,
        execute: impl FnOnce() -> Result<JobOutcome<TestWorkload>, Box<dyn Error>>,
    ) -> super::super::WatchedJob<TestWorkload, ()> {
        let mut outcome = if id.ordinal == 4 {
            let result = CampaignJobResult::<TestWorkload> {
                actions: Vec::new(),
                preparation_failure: Some(vec![()]),
            };
            JobOutcome {
                reservation: job.reservation,
                result_sha256: self.0.result_sha256(&result)?,
                result,
                execution_work: 0,
            }
        } else {
            execute()?
        };
        if id.ordinal == 3 {
            let action = outcome.result.actions.last_mut().unwrap();
            action.outcome.disposition = ExecutionDisposition::Failed;
            action.candidate = None;
            outcome.result_sha256 = self.0.result_sha256(&outcome.result)?;
        }
        if id.ordinal == 6 {
            outcome
                .result
                .actions
                .last_mut()
                .unwrap()
                .outcome
                .objective_reached = true;
            outcome.result_sha256 = self.0.result_sha256(&outcome.result)?;
        }
        Ok((outcome, ()))
    }
}

#[test]
fn expansion_retains_filtered_branch_objectives_failures_and_lazy_full_witnesses() {
    EXPANSION_WITNESSES.with(|paths| paths.borrow_mut().clear());
    let workload = expansion_workload();
    let config = expansion_config(14, 1);
    let mut coordinator = ExpansionCoordinator::<TestWorkload, ()>::start(
        &workload,
        &config,
        &CampaignOrigin::Genesis,
    )
    .unwrap();
    let spec = coordinator.reserve().unwrap().unwrap();
    let mut target = workload.new_target().unwrap();
    let outcome = run_expansion_task(
        &workload,
        &(),
        &mut target,
        &spec,
        &BranchEvidenceWatch(&workload),
    )
    .unwrap();
    let trace = outcome.trace.as_ref().unwrap();
    assert_eq!(trace.objectives, [6]);
    assert_eq!(trace.failures, [3, 4]);
    assert!(
        !trace
            .nodes
            .iter()
            .any(|node| node.parent_ordinal == Some(6))
    );
    let attempts = trace.accounting.task_unique_edge_attempts;
    let objective_suffix = trace
        .nodes
        .iter()
        .find(|node| node.ordinal == 6)
        .unwrap()
        .suffix
        .clone();
    let failure_suffix = trace
        .nodes
        .iter()
        .find(|node| node.ordinal == 3)
        .unwrap()
        .suffix
        .clone();
    coordinator.complete(outcome).unwrap();
    let admission = coordinator.admit_next().unwrap().unwrap();
    assert_eq!(usize::from(*coordinator.evidence()), attempts - 1);
    assert_eq!(
        coordinator.objective_witness().unwrap().actions,
        objective_suffix
    );
    for (ordinal, decision) in [
        (3, ExpansionDecision::Filtered),
        (4, ExpansionDecision::PreparationFailed),
        (6, ExpansionDecision::Objective),
    ] {
        assert_eq!(
            admission
                .decisions
                .iter()
                .find(|row| row.id.ordinal == ordinal)
                .unwrap()
                .decision,
            decision
        );
    }
    assert_eq!(coordinator.counters().preparation_failures, 1);
    assert_eq!(coordinator.counters().executed_edges, attempts as u64 - 1);
    let retained = trace_projection(&admission.outcome);
    assert!(retained.iter().any(|row| row.0 == 6 && row.4));
    assert!(
        admission
            .outcome
            .trace
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|node| node.ordinal == 4)
            .unwrap()
            .evidence
            .as_ref()
            .unwrap()
            .evidence
            .preparation_failure
            .is_some()
    );
    let witnesses = EXPANSION_WITNESSES.with(|paths| paths.borrow().clone());
    assert!(witnesses.iter().any(|(_, path)| *path == objective_suffix));
    assert!(witnesses.iter().any(|(_, path)| *path == failure_suffix));
}

fn assert_only_reserved_pins_released(
    before: &[u8],
    after: &crate::search::archive::Archive<TestAction, TestKey, (), u8>,
    spec: &ExpansionTaskSpec<TestWorkload>,
) {
    let mut expected: crate::search::archive::Archive<TestAction, TestKey, (), u8> =
        postcard::from_bytes(before).unwrap();
    assert_eq!(postcard::to_stdvec(&expected).unwrap(), before);
    expected.unpin_job_origin(spec.snapshot_id);
    expected.unpin_metadata(spec.parent_id);
    let released = postcard::to_stdvec(&expected).unwrap();
    assert_ne!(released, before);
    assert_eq!(postcard::to_stdvec(after).unwrap(), released);
    expected.unpin_job_origin(spec.snapshot_id);
    expected.unpin_metadata(spec.parent_id);
    assert_eq!(postcard::to_stdvec(&expected).unwrap(), released);
}

#[test]
fn expansion_later_task_failure_preserves_the_earlier_successful_admission() {
    let workload = expansion_workload();
    let config = expansion_config(28, 2);
    let mut coordinator = ExpansionCoordinator::<TestWorkload, ()>::start(
        &workload,
        &config,
        &CampaignOrigin::Genesis,
    )
    .unwrap();
    let first = coordinator.reserve().unwrap().unwrap();
    let later = coordinator.reserve().unwrap().unwrap();
    coordinator
        .complete_error(later.task, "later expansion execution refused".into())
        .unwrap();
    assert!(coordinator.admit_next().unwrap().is_none());
    assert_eq!(coordinator.counters().tasks_admitted, 0);
    coordinator
        .complete(execute_expansion(&first, &workload))
        .unwrap();
    let admitted = coordinator.admit_next().unwrap().unwrap();
    assert_eq!(admitted.outcome.task, first.task);
    assert!(coordinator.counters().executed_edges > 0);
    assert!(*coordinator.evidence() > 0);
    let before = postcard::to_stdvec(coordinator.archive()).unwrap();
    let error = coordinator.admit_next().err().unwrap();
    assert!(
        error
            .to_string()
            .contains("later expansion execution refused")
    );
    assert_eq!(coordinator.counters().tasks_admitted, 1);
    assert_only_reserved_pins_released(&before, coordinator.archive(), &later);
    assert!(coordinator.reserve().is_err());
}

#[test]
fn expansion_invalid_reserved_root_is_rejected_before_evidence_or_archive_admission() {
    let workload = expansion_workload();
    let config = expansion_config(14, 1);
    let mut coordinator = ExpansionCoordinator::<TestWorkload, ()>::start(
        &workload,
        &config,
        &CampaignOrigin::Genesis,
    )
    .unwrap();
    let spec = coordinator.reserve().unwrap().unwrap();
    let mut outcome = execute_expansion(&spec, &workload);
    outcome.trace.as_mut().unwrap().root.cost_in_group += 1;
    let before = postcard::to_stdvec(coordinator.archive()).unwrap();
    coordinator.complete(outcome).unwrap();
    assert_eq!(*coordinator.evidence(), 0);
    let error = coordinator.admit_next().err().unwrap();
    assert!(error.to_string().contains("reserved root or quota"));
    assert_eq!(*coordinator.evidence(), 0);
    assert_eq!(coordinator.counters().edge_attempts, 0);
    assert_only_reserved_pins_released(&before, coordinator.archive(), &spec);
}

#[test]
fn expansion_malformed_tree_context_and_frontier_are_rejected_before_any_publication() {
    for planted in 0..8 {
        let workload = expansion_workload();
        let config = expansion_config(14, 1);
        let mut coordinator = ExpansionCoordinator::<TestWorkload, ()>::start(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
        )
        .unwrap();
        let spec = coordinator.reserve().unwrap().unwrap();
        let mut outcome = execute_expansion(&spec, &workload);
        let trace = outcome.trace.as_mut().unwrap();
        let mut external_snapshot = None;
        match planted {
            0 => trace.nodes[1].parent_ordinal = Some(trace.nodes[1].ordinal),
            1 => trace.root.last_action = Some(TestAction::new(17, 1)),
            2 => trace.frontier = vec![0],
            3 => trace.nodes[1].suffix[0].input ^= 1,
            4 => trace.nodes[1].status = crate::search::expansion::Status::Failed,
            5 => {
                let ordinal = trace.frontier[0];
                external_snapshot = Some(Arc::clone(
                    trace
                        .nodes
                        .iter()
                        .find(|node| node.ordinal == ordinal)
                        .unwrap()
                        .snapshot
                        .as_ref()
                        .unwrap(),
                ));
            }
            6 => {
                let index = trace
                    .nodes
                    .iter()
                    .rposition(|node| node.depth == 4 && !trace.frontier.contains(&node.ordinal))
                    .unwrap();
                let node = &mut trace.nodes[index];
                node.status = crate::search::expansion::Status::Terminal;
                node.evidence.as_mut().unwrap().evidence.outcome.disposition =
                    ExecutionDisposition::Terminal;
                node.snapshot = Some(Arc::new(0));
            }
            7 => trace.accounting.task_unique_edge_attempts = usize::MAX,
            _ => unreachable!(),
        }
        let before = postcard::to_stdvec(coordinator.archive()).unwrap();
        coordinator.complete(outcome).unwrap();
        let error = coordinator
            .admit_next()
            .err()
            .expect("malformed completion must fail");
        assert!(error.to_string().contains("expansion"));
        assert_eq!(*coordinator.evidence(), 0);
        assert_eq!(coordinator.counters().edge_attempts, 0);
        assert_only_reserved_pins_released(&before, coordinator.archive(), &spec);
        assert!(coordinator.reserve().is_err());
        if let Some(snapshot) = external_snapshot {
            assert_eq!(Arc::strong_count(&snapshot), 1);
            drop(snapshot);
        }
    }
}

#[test]
fn expansion_empty_local_tree_consumes_one_unit_without_reinserting_genesis() {
    let workload = expansion_workload();
    let config = expansion_config(1, 1);
    let mut coordinator = ExpansionCoordinator::<TestWorkload, ()>::start(
        &workload,
        &config,
        &CampaignOrigin::Genesis,
    )
    .unwrap();
    let spec = coordinator.reserve().unwrap().unwrap();
    let outcome = ExpansionTaskOutcome::<TestWorkload> {
        task: spec.task,
        reconstruction_work: 0,
        preparation_failure: None,
        trace: Some(crate::search::expansion::Trace {
            root: crate::search::expansion::Root {
                parent_id: spec.parent_id,
                facts: Arc::new(crate::search::campaign::ExpansionFacts {
                    key: spec.parent_key,
                    milestones: spec.parent_milestones,
                    progress: (),
                    quality: spec.parent_key.0,
                    bucket: spec.parent_key.0,
                }),
                cost_in_group: spec.root_cost_in_group,
                last_action: spec.last_action,
                task_seed: spec.task_seed,
                edge_budget: spec.edge_budget,
            },
            nodes: vec![crate::search::expansion::Node {
                ordinal: 0,
                parent_ordinal: None,
                depth: 0,
                requested_action: None,
                suffix: Vec::new(),
                facts: Some(Arc::new(crate::search::campaign::ExpansionFacts {
                    key: spec.parent_key,
                    milestones: (),
                    progress: (),
                    quality: spec.parent_key.0,
                    bucket: spec.parent_key.0,
                })),
                path_cost: 0,
                cost_in_group: Some(spec.root_cost_in_group),
                edge_cost: 0,
                status: crate::search::expansion::Status::Runnable,
                objective: false,
                evidence: None,
                snapshot: None,
                charged_snapshot_bytes: 0,
            }],
            duplicates: Vec::new(),
            frontier: Vec::new(),
            objectives: Vec::new(),
            failures: Vec::new(),
            accounting: Default::default(),
        }),
    };
    coordinator.complete(outcome).unwrap();
    let admission = coordinator.admit_next().unwrap().unwrap();
    assert!(admission.decisions.is_empty());
    assert!(!admission.useful);
    assert_eq!(coordinator.counters().budget_consumed, 1);
    assert_eq!(coordinator.counters().empty_tasks, 1);
    assert_eq!(coordinator.counters().edge_attempts, 0);
    assert_eq!(coordinator.counters().executed_edges, 0);
    assert_eq!(coordinator.archive().entries.len(), 1);
    assert_eq!(*coordinator.evidence(), 0);
    assert!(coordinator.reserve().unwrap().is_none());
}

#[test]
fn expansion_rejects_a_config_that_disables_required_objective_stopping() {
    let workload = expansion_workload();
    let mut config = expansion_config(14, 1);
    config.stop_rollout_on_objective = false;
    let error = ExpansionCoordinator::<TestWorkload, ()>::start(
        &workload,
        &config,
        &CampaignOrigin::Genesis,
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("objective"));
}

fn expansion_checkpoint_plan(directory: &std::path::Path) -> CheckpointPlan {
    CheckpointPlan {
        directory: directory.to_owned(),
        every: None,
        on_marks: false,
        on_top_progress: false,
        checkpoint_owner: None,
        max_image_bytes: 64 << 20,
        before_write: None,
    }
}

fn finish_expansion_checkpoint_fixture(
    campaign: &mut ExpansionCoordinator<'_, TestWorkload>,
    workload: &TestWorkload,
    initial: Vec<ExpansionTaskSpec<TestWorkload>>,
) -> Vec<(u64, String)> {
    let mut queue = std::collections::VecDeque::from(initial);
    while let Some(spec) = campaign.reserve().unwrap() {
        queue.push_back(spec);
    }
    let mut rows = Vec::new();
    while let Some(spec) = queue.pop_front() {
        campaign
            .complete(execute_expansion(&spec, workload))
            .unwrap();
        while let Some(admission) = campaign.admit_next().unwrap() {
            rows.push((admission.spec.task, format!("{:?}", admission.decisions)));
            while let Some(spec) = campaign.reserve().unwrap() {
                queue.push_back(spec);
            }
        }
    }
    assert!(campaign.finished());
    assert!(campaign.reserve().unwrap().is_none());
    rows
}

#[test]
fn expansion_checkpoint_reruns_buffered_tasks_and_preserves_adaptive_continuation() {
    let directory = std::env::temp_dir().join(format!(
        "expansion-checkpoint-pending-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    let workload = expansion_workload();
    let config = expansion_config(56, 2);
    let mut original =
        ExpansionCoordinator::<TestWorkload>::start(&workload, &config, &CampaignOrigin::Genesis)
            .unwrap();
    let first = original.reserve().unwrap().unwrap();
    let later = original.reserve().unwrap().unwrap();
    original
        .complete(execute_expansion(&later, &workload))
        .unwrap();
    assert!(original.admit_next().unwrap().is_none());
    original
        .enable_checkpoints(expansion_checkpoint_plan(&directory))
        .unwrap();
    original.checkpoint("pending").unwrap();
    original.finish_persistence(Ok(())).unwrap();
    let path = directory.join("000000000000-pending.ckpt");
    let header = crate::search::checkpoint::read_header(&path).unwrap();
    assert_eq!(header.reserved, 2);
    assert_eq!(header.admitted, 0);
    assert_eq!(header.executions, 0);
    assert_eq!(header.planned_clock, 0);
    let origin = CampaignOrigin::SearchCheckpoint {
        path,
        expected_sha256: None,
    };
    let mut restored =
        ExpansionCoordinator::<TestWorkload>::start(&workload, &config, &origin).unwrap();
    let pending = restored.pending_tasks().unwrap();
    assert_eq!(
        pending.iter().map(|spec| spec.task).collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(pending[0].task_seed, first.task_seed);
    assert_eq!(pending[1].task_seed, later.task_seed);
    assert!(Arc::ptr_eq(&pending[0].snapshot, &pending[1].snapshot));
    assert_eq!(
        postcard::to_stdvec(restored.archive()).unwrap(),
        postcard::to_stdvec(original.archive()).unwrap()
    );
    assert_eq!(restored.counters(), original.counters());
    assert_eq!(
        restored.counters().bootstrap_work,
        original.counters().bootstrap_work
    );
    let expected_rows = finish_expansion_checkpoint_fixture(&mut original, &workload, vec![first]);
    let actual_rows = finish_expansion_checkpoint_fixture(&mut restored, &workload, pending);
    assert_eq!(actual_rows, expected_rows);
    assert_eq!(restored.counters(), original.counters());
    assert_eq!(restored.evidence(), original.evidence());
    assert_eq!(
        postcard::to_stdvec(restored.archive()).unwrap(),
        postcard::to_stdvec(original.archive()).unwrap()
    );
    assert!(restored.pending_tasks().unwrap().is_empty());
    drop(original);
    drop(restored);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn expansion_checkpoint_rejects_changed_policies_and_insufficient_budget() {
    let directory = std::env::temp_dir().join(format!(
        "expansion-checkpoint-policy-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    let workload = expansion_workload();
    let config = expansion_config(28, 2);
    let mut campaign =
        ExpansionCoordinator::<TestWorkload>::start(&workload, &config, &CampaignOrigin::Genesis)
            .unwrap();
    campaign.reserve().unwrap().unwrap();
    campaign.reserve().unwrap().unwrap();
    campaign
        .enable_checkpoints(expansion_checkpoint_plan(&directory))
        .unwrap();
    campaign.checkpoint("pending").unwrap();
    campaign.finish_persistence(Ok(())).unwrap();
    let origin = CampaignOrigin::SearchCheckpoint {
        path: directory.join("000000000000-pending.ckpt"),
        expected_sha256: None,
    };
    for field in 0..6 {
        let mut changed = expansion_config(28, 2);
        match field {
            0 => changed.campaign_seed += 1,
            1 => changed.window += 1,
            2 => changed.archive_entry_limit += 1,
            3 => changed.memory_budget_mib = Some(512),
            4 => changed.stop_campaign_on_objective = !changed.stop_campaign_on_objective,
            5 => changed.execution_budget = 27,
            _ => unreachable!(),
        }
        assert!(
            ExpansionCoordinator::<TestWorkload>::start(&workload, &changed, &origin).is_err(),
            "field {field}"
        );
    }
    let extended = expansion_config(42, 2);
    let resumed =
        ExpansionCoordinator::<TestWorkload>::start(&workload, &extended, &origin).unwrap();
    assert_eq!(resumed.window_len(), 2);
    assert_eq!(resumed.counters().budget_consumed, 0);
    assert_eq!(
        resumed.counters().bootstrap_work,
        campaign.counters().bootstrap_work
    );
    drop(resumed);
    drop(campaign);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn expansion_checkpoint_rejects_valid_envelopes_with_malformed_expansion_state() {
    use crate::search::campaign::ExpansionReservationRecord;
    use crate::search::checkpoint::CheckpointStoreWriter;
    use std::num::NonZeroU8;
    let directory = std::env::temp_dir().join(format!(
        "expansion-checkpoint-tamper-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    let source = directory.join("source");
    let workload = expansion_workload();
    let config = expansion_config(28, 2);
    let mut campaign =
        ExpansionCoordinator::<TestWorkload>::start(&workload, &config, &CampaignOrigin::Genesis)
            .unwrap();
    campaign.reserve().unwrap().unwrap();
    campaign.reserve().unwrap().unwrap();
    campaign
        .enable_checkpoints(expansion_checkpoint_plan(&source))
        .unwrap();
    campaign.checkpoint("pending").unwrap();
    campaign.finish_persistence(Ok(())).unwrap();
    let path = source.join("000000000000-pending.ckpt");
    for plant in 0..15 {
        let mut reader = CheckpointReader::open(&path).unwrap();
        let mut header = reader.header.clone();
        let mut format: String = reader.next().unwrap();
        let mut state: CoreCheckpoint<TestWorkload> = reader.next().unwrap();
        let mut archive: Archive<TestAction, TestKey, (), u8> = reader.next().unwrap();
        let evidence: Vec<u8> = reader.next().unwrap();
        let rand: RomuDuoJrRand = reader.next().unwrap();
        let mut counters: crate::search::campaign::ExpansionCounters = reader.next().unwrap();
        let mut records: Vec<ExpansionReservationRecord<TestWorkload>> = reader.next().unwrap();
        let snapshots = reader.snapshots::<u8>().unwrap();
        reader.finish().unwrap();
        match plant {
            0 => {
                header
                    .policies
                    .insert("algorithm".into(), "legacy_search".into());
            }
            1 => format = "legacy_body".into(),
            2 => state.sequence = 1,
            3 => counters.executions_to_first_objective = Some(0),
            4 => {
                state.objectives_reached = 1;
            }
            5 => archive.unpin_metadata(records[0].parent_id),
            6 => records.swap(0, 1),
            7 => records[0].edge_budget = NonZeroU8::new(15).unwrap(),
            8 => records[0].parent_id = u64::MAX,
            9 => records[0].snapshot_id = u64::MAX,
            10 => records[0].root_cost_in_group += 1,
            11 => records[0].last_action = Some(TestAction::new(99, 1)),
            12 => records[0].replay.push(TestAction::new(99, 1)),
            13 => records[0].replay_certificates.push(Some([9; 32])),
            14 => {
                state.objectives_reached = 1;
                state.objective_witness = Some(Input::default());
                counters.executions_to_first_objective = Some(1);
                counters.work_to_first_objective = Some(u64::MAX);
            }
            _ => unreachable!(),
        }
        let output = directory.join(format!("plant-{plant}"));
        let mut writer = CheckpointStoreWriter::create(expansion_checkpoint_plan(&output)).unwrap();
        writer
            .write(
                &header,
                snapshots.iter().map(|(id, snapshot)| (*id, snapshot)),
                |out| {
                    postcard::to_io(&format, &mut *out)?;
                    postcard::to_io(&state, &mut *out)?;
                    postcard::to_io(&archive, &mut *out)?;
                    postcard::to_io(&evidence, &mut *out)?;
                    postcard::to_io(&rand, &mut *out)?;
                    postcard::to_io(&counters, &mut *out)?;
                    postcard::to_io(&records, &mut *out)?;
                    Ok(())
                },
            )
            .unwrap();
        let origin = CampaignOrigin::SearchCheckpoint {
            path: output.join("000000000000-pending.ckpt"),
            expected_sha256: None,
        };
        assert!(
            ExpansionCoordinator::<TestWorkload>::start(&workload, &config, &origin).is_err(),
            "plant {plant}"
        );
    }
    drop(campaign);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn expansion_observer_stopped_tracks_policy_exhaustion_instead_of_empty_window() {
    use crate::search::campaign::CampaignView;
    let workload = expansion_workload();
    let config = expansion_config(28, 1);
    let mut campaign =
        ExpansionCoordinator::<TestWorkload>::start(&workload, &config, &CampaignOrigin::Genesis)
            .unwrap();
    assert!(campaign.finished());
    assert!(!CampaignView::finished(&campaign));
    let first = campaign.reserve().unwrap().unwrap();
    assert!(!CampaignView::finished(&campaign));
    campaign
        .complete(execute_expansion(&first, &workload))
        .unwrap();
    campaign.admit_next().unwrap().unwrap();
    assert!(campaign.finished());
    assert!(campaign.counters().budget_consumed < config.execution_budget);
    assert!(!CampaignView::finished(&campaign));
    while let Some(spec) = campaign.reserve().unwrap() {
        assert!(!CampaignView::finished(&campaign));
        campaign
            .complete(execute_expansion(&spec, &workload))
            .unwrap();
        campaign.admit_next().unwrap().unwrap();
    }
    assert_eq!(campaign.counters().budget_consumed, config.execution_budget);
    assert!(campaign.finished());
    assert!(CampaignView::finished(&campaign));

    let mut stopped =
        ExpansionCoordinator::<TestWorkload>::start(&workload, &config, &CampaignOrigin::Genesis)
            .unwrap();
    let spec = stopped.reserve().unwrap().unwrap();
    stopped.stop_reservations();
    assert!(!CampaignView::finished(&stopped));
    stopped
        .complete(execute_expansion(&spec, &workload))
        .unwrap();
    stopped.admit_next().unwrap().unwrap();
    assert!(stopped.counters().budget_consumed < config.execution_budget);
    assert!(stopped.finished());
    assert!(CampaignView::finished(&stopped));
    assert!(stopped.reserve().unwrap().is_none());
}

#[test]
fn expansion_checkpoint_restore_finalizes_sessions_on_success_and_malformed_body() {
    use crate::search::checkpoint::CheckpointStoreWriter;
    let directory = std::env::temp_dir().join(format!(
        "expansion-checkpoint-session-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    let workload = expansion_workload();
    let config = expansion_config(28, 2);
    FINALIZATION.with(|state| state.set((0, false)));
    let mut campaign =
        ExpansionCoordinator::<TestWorkload>::start(&workload, &config, &CampaignOrigin::Genesis)
            .unwrap();
    campaign.reserve().unwrap().unwrap();
    let source = directory.join("source");
    campaign
        .enable_checkpoints(expansion_checkpoint_plan(&source))
        .unwrap();
    campaign.checkpoint("pending").unwrap();
    campaign.finish_persistence(Ok(())).unwrap();
    let path = source.join("000000000000-pending.ckpt");
    let mut header = crate::search::checkpoint::read_header(&path).unwrap();
    header.reason = "malformed".into();
    let malformed = directory.join("malformed");
    let mut writer = CheckpointStoreWriter::create(expansion_checkpoint_plan(&malformed)).unwrap();
    writer
        .write(&header, std::iter::empty::<(u64, &u8)>(), |out| {
            postcard::to_io(&"not-an-expansion-body", out)?;
            Ok(())
        })
        .unwrap();
    for fail_cleanup in [false, true] {
        for malformed_body in [false, true] {
            FINALIZATION.with(|state| state.set((0, fail_cleanup)));
            let origin = CampaignOrigin::SearchCheckpoint {
                path: if malformed_body {
                    malformed.join("000000000000-malformed.ckpt")
                } else {
                    path.clone()
                },
                expected_sha256: None,
            };
            let result = ExpansionCoordinator::<TestWorkload>::start(&workload, &config, &origin);
            assert_eq!(FINALIZATION.with(|state| state.get().0), 1);
            match result {
                Ok(restored) => {
                    assert!(!fail_cleanup && !malformed_body);
                    assert_eq!(restored.counters(), campaign.counters());
                    assert_eq!(restored.window_len(), 1);
                }
                Err(error) if malformed_body => {
                    assert!(error.to_string().contains("body algorithm"));
                    assert!(!error.to_string().contains("shutdown unconfirmed"));
                }
                Err(error) => {
                    assert!(fail_cleanup);
                    assert!(error.to_string().contains("shutdown unconfirmed"));
                }
            }
        }
    }
    FINALIZATION.with(|state| state.set((0, false)));
    drop(campaign);
    std::fs::remove_dir_all(directory).unwrap();
}
