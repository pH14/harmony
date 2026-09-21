// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{cmp::Ordering, error::Error};

use searcher::search::{
    archive::{
        ArchiveEntryReport, ArchiveKey, Input, RetentionPolicy, RetireThresholds, SelectorPolicy,
        entries_by_suffix,
    },
    campaign::{
        ArchiveReportState, CampaignActionResult, CampaignCheckpoint, CampaignConfig,
        CampaignExecutionOptions, CampaignJobResult, CampaignOrigin, CampaignStreamRecord,
        CampaignTypes, Evaluation, InputPolicy, Reporting, TargetExecution, WorkloadPolicies,
        replay_campaign_checkpointed, run_campaign_checkpointed,
        run_campaign_checkpointed_with_options,
    },
    draw::{DrawMixture, SuffixShape},
    empirical_steps::EmpiricalStepCheckpoint,
    rand::RomuDuoJrRand,
    rollout::ExecutionDisposition,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct TickAction(u8);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct TickKey(u8);

impl ArchiveKey for TickKey {
    type Group = u8;
    type Lineage = ();

    fn groups() -> usize {
        1
    }

    fn group(self, depth: usize) -> Self::Group {
        assert_eq!(depth, 0);
        self.0
    }

    fn slot_capacity() -> usize {
        1
    }

    fn progress_cmp(_: Self::Group, _: Self::Group) -> Ordering {
        Ordering::Equal
    }

    fn preference_cmp(self, _: Self) -> Ordering {
        let _ = self;
        Ordering::Equal
    }

    fn complete(self, _: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_: &mut Self::Lineage, _: Self) {}
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct HorizonArchiveReport {
    #[serde(with = "entries_by_suffix")]
    entries: Vec<ArchiveEntryReport<TickAction, TickKey, ()>>,
}

#[derive(Default)]
struct HorizonTarget {
    ticks: u8,
    branch: u8,
    execution_work: u64,
}

struct HorizonWorkload {
    goal_after: u8,
    distractors: bool,
}

impl CampaignTypes for HorizonWorkload {
    type Target = HorizonTarget;
    type Action = TickAction;
    type Key = TickKey;
    type Milestones = ();
    type Progress = ();
    type Snapshot = (u8, u8);
    type Observations = ();
    type Evidence = ();
    type ArchiveReport = HorizonArchiveReport;
    type Run = ();
}

impl Reporting for HorizonWorkload {
    fn stream_format(&self) -> &'static str {
        "adaptive-horizon-test-v1"
    }

    fn checkpoint_format(&self) -> &'static str {
        "adaptive-horizon-test-snapshot-v1"
    }

    fn workload_identity_sha256(&self) -> String {
        "adaptive-horizon-test".to_owned()
    }

    fn action_cost_unit(&self) -> &'static str {
        "tick"
    }

    fn execution_work_unit(&self) -> &'static str {
        "tick"
    }

    fn result_sha256(&self, result: &CampaignJobResult<Self>) -> Result<String, Box<dyn Error>> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(result)?)))
    }

    fn archive_report(
        &self,
        _: &Self::Evidence,
        state: ArchiveReportState<Self>,
    ) -> Self::ArchiveReport {
        HorizonArchiveReport {
            entries: state.entries,
        }
    }
}

impl InputPolicy for HorizonWorkload {
    fn max_action_limit(&self) -> usize {
        64
    }

    fn max_action_cost(&self) -> u64 {
        1
    }

    fn policies(&self, _: &Self::Run) -> WorkloadPolicies {
        WorkloadPolicies::new()
    }

    fn resolve_recorded(&self, _: &WorkloadPolicies) -> Result<Self::Run, Box<dyn Error>> {
        Ok(())
    }

    fn sample_alphabet(
        &self,
        _: &Self::Run,
        rand: &mut RomuDuoJrRand,
    ) -> Result<Self::Action, Box<dyn Error>> {
        Ok(TickAction(u8::try_from(rand.next_u64() % 251 + 1)?))
    }
}

impl TargetExecution for HorizonWorkload {
    fn new_target(&self) -> Result<Self::Target, String> {
        Ok(HorizonTarget::default())
    }

    fn reset(&self, target: &mut Self::Target) {
        target.ticks = 0;
        target.branch = 0;
    }

    fn restore(
        &self,
        target: &mut Self::Target,
        snapshot: &Self::Snapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.ticks = snapshot.0;
        target.branch = snapshot.1;
        Ok(())
    }

    fn execution_work(&self, target: &Self::Target) -> u64 {
        target.execution_work
    }

    fn action_cost_fn(&self) -> fn(&Self::Action) -> u64 {
        |_| 1
    }

    fn snapshot_memory_charge(_: &Self::Snapshot) -> usize {
        1
    }

    fn apply_action(
        &self,
        target: &mut Self::Target,
        action: &Self::Action,
        _: &mut Self::Milestones,
    ) -> Result<(), Box<dyn Error>> {
        if self.distractors && target.ticks == 0 {
            target.branch = if action.0.is_multiple_of(2) {
                0
            } else {
                action.0.saturating_add(1)
            };
        }
        target.ticks = target.ticks.saturating_add(1);
        target.execution_work = target.execution_work.saturating_add(1);
        Ok(())
    }

    fn snapshot(&self, target: &mut Self::Target) -> Result<Self::Snapshot, Box<dyn Error>> {
        Ok((target.ticks, target.branch))
    }
}

impl Evaluation for HorizonWorkload {
    fn execution_disposition(&self, _: &Self::Target) -> ExecutionDisposition {
        ExecutionDisposition::Runnable
    }

    fn objective_reached(
        &self,
        _: &Self::Run,
        target: &Self::Target,
    ) -> Result<bool, Box<dyn Error>> {
        Ok(self.goal_after != 0
            && target.ticks >= self.goal_after
            && (!self.distractors || target.branch == 0))
    }

    fn current_key(&self, target: &Self::Target) -> Result<Self::Key, Box<dyn Error>> {
        if self.distractors && target.branch != 0 {
            return Ok(TickKey(target.branch));
        }
        Ok(TickKey(u8::from(
            self.goal_after != 0 && target.ticks >= self.goal_after,
        )))
    }

    fn complete_candidate_key(
        &self,
        key: Self::Key,
        _: &Self::Snapshot,
    ) -> Result<Self::Key, Box<dyn Error>> {
        Ok(key)
    }

    fn merge_milestones(&self, _: &mut Self::Milestones, _: Self::Milestones) {}

    fn aggregate_milestones(_: &Self::Evidence) -> Self::Milestones {}

    fn aggregate_progress(_: &Self::Evidence) -> Self::Progress {}

    fn merge_origin_evidence(&self, _: &mut Self::Evidence, _: &Self::ArchiveReport) {}

    fn merge_snapshot_root_evidence(
        &self,
        _: &mut Self::Evidence,
        _: &Self::Target,
    ) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    fn merge_import_evidence(
        &self,
        _: &mut Self::Evidence,
        _: Self::Milestones,
        _: &Input<Self::Action>,
    ) {
    }

    fn merge_action_evidence<F>(
        &self,
        _: &mut Self::Evidence,
        _: &CampaignActionResult<Self>,
        _: u64,
        _: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<Input<Self::Action>, Box<dyn Error>>,
    {
        Ok(())
    }

    fn source_entries<'a>(
        &self,
        source: &'a Self::ArchiveReport,
    ) -> &'a [ArchiveEntryReport<Self::Action, Self::Key, Self::Milestones>] {
        &source.entries
    }

    fn resume_input(
        &self,
        source: &Self::ArchiveReport,
    ) -> Result<Input<Self::Action>, Box<dyn Error>> {
        source
            .entries
            .last()
            .map(|entry| entry.input.clone())
            .ok_or_else(|| "horizon fixture has no archive entry".into())
    }
}

fn config(
    execution_budget: u64,
    suffix: SuffixShape,
    workers: u32,
    action_limit: usize,
) -> CampaignConfig<HorizonWorkload> {
    CampaignConfig {
        campaign_seed: 0x51a7_e001,
        workers,
        execution_budget,
        action_limit,
        host: "adaptive-horizon-test".to_owned(),
        wall_budget: None,
        stop_rollout_on_objective: false,
        stop_campaign_on_objective: false,
        archive_entry_limit: 16,
        reservations_per_worker: 1,
        memory_budget_mib: None,
        materialize_final_artifacts: true,
        run: (),
        suffix,
        mixture: DrawMixture::AlphabetOnly,
        retention: RetentionPolicy::Unprobed,
        selector: SelectorPolicy::Retire(RetireThresholds {
            entry: 3,
            groups: Vec::new(),
        }),
        objective_witness_path: None,
    }
}

#[test]
fn adaptive_horizon_crosses_wait_and_replays_exactly() {
    let workload = HorizonWorkload {
        goal_after: 20,
        distractors: false,
    };
    let mut stream = Vec::new();
    let live = run_campaign_checkpointed(
        &workload,
        &config(32, SuffixShape::OneToSix, 1, 64),
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
    )
    .expect("wait campaign");
    assert!(live.0.execution_work > 0);
    assert!(live.0.archive.entries.len() >= 2);
    assert!(
        live.0
            .archive
            .entries
            .iter()
            .any(|entry| entry.key == TickKey(1) && entry.input.actions.len() >= 20)
    );
    let recorded_extensions = stream
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .skip(1)
        .map(|line| {
            serde_json::from_slice::<CampaignStreamRecord<EmpiricalStepCheckpoint, TickKey>>(line)
                .expect("horizon stream record")
        })
        .map(|record| match record {
            CampaignStreamRecord::Job(job) => job.adaptive_horizon_extension,
            CampaignStreamRecord::Skip(skip) => skip.adaptive_horizon_extension,
        })
        .collect::<Vec<_>>();
    assert!(recorded_extensions.iter().any(|extension| *extension > 0));
    let replayed =
        replay_campaign_checkpointed(&workload, &stream, None, None).expect("wait replay");
    assert_eq!(replayed, live);
}

#[test]
fn adaptive_horizon_counters_survive_archive_import() {
    let source_workload = HorizonWorkload {
        goal_after: 7,
        distractors: false,
    };
    let mut source_stream = Vec::new();
    let source = run_campaign_checkpointed(
        &source_workload,
        &config(8, SuffixShape::OneToSix, 1, 8),
        &CampaignOrigin::Genesis,
        &mut source_stream,
        None,
    )
    .expect("source campaign");
    let source_root = source
        .0
        .archive
        .entries
        .iter()
        .find(|entry| entry.key == TickKey(0))
        .expect("source root");
    assert!(
        source_root
            .selector
            .as_ref()
            .is_some_and(|counters| counters.horizon_unproductive > 0),
        "source archive did not retain horizon feedback: {:?}",
        source.0.archive.entries
    );
    let mut imported_report = source.0.archive.clone();
    let imported_root = imported_report
        .entries
        .iter_mut()
        .find(|entry| entry.key == TickKey(0))
        .expect("imported root");
    let imported_counters = imported_root
        .selector
        .as_mut()
        .expect("imported selector counters");
    imported_counters.horizon_unproductive = 20;
    imported_counters.horizon_failed_extension = 8;
    let origin = CampaignOrigin::Archive {
        path: "adaptive-horizon-source.json".to_owned(),
        file_sha256: "source".to_owned(),
        report: Box::new(imported_report),
        checkpoint: Some(CampaignCheckpoint {
            path: "adaptive-horizon-source.snapshots".to_owned(),
            file_sha256: "snapshots".to_owned(),
            snapshots: source.1,
        }),
    };
    let workload = HorizonWorkload {
        goal_after: 7,
        distractors: false,
    };
    let mut stream = Vec::new();
    run_campaign_checkpointed(
        &workload,
        &config(1, SuffixShape::OneToSix, 1, 16),
        &origin,
        &mut stream,
        None,
    )
    .expect("archive import campaign");
    let imported_extensions = stream
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .skip(1)
        .map(|line| {
            serde_json::from_slice::<CampaignStreamRecord<EmpiricalStepCheckpoint, TickKey>>(line)
                .expect("import stream record")
        })
        .map(|record| match record {
            CampaignStreamRecord::Job(job) => job.adaptive_horizon_extension,
            CampaignStreamRecord::Skip(skip) => skip.adaptive_horizon_extension,
        })
        .collect::<Vec<_>>();
    assert!(imported_extensions.iter().any(|extension| *extension > 0));
}

#[test]
fn adaptive_horizon_records_a_zero_extension_full_trial() {
    let source_workload = HorizonWorkload {
        goal_after: 7,
        distractors: false,
    };
    let mut source_stream = Vec::new();
    let source = run_campaign_checkpointed(
        &source_workload,
        &config(8, SuffixShape::OneToSix, 1, 64),
        &CampaignOrigin::Genesis,
        &mut source_stream,
        None,
    )
    .expect("source campaign");
    let mut imported_report = source.0.archive.clone();
    let target = imported_report
        .entries
        .iter_mut()
        .find(|entry| entry.key == TickKey(1))
        .expect("goal entry");
    target
        .selector
        .as_mut()
        .expect("goal selector counters")
        .horizon_unproductive = 20;
    let target_id = target.id;
    let action_limit = target.input.actions.len().saturating_add(1);
    let source_checkpoint = CampaignCheckpoint {
        path: "adaptive-horizon-zero-extension.snapshots".to_owned(),
        file_sha256: "snapshots".to_owned(),
        snapshots: source.1.clone(),
    };
    let replay_report = imported_report.clone();
    let origin = CampaignOrigin::Archive {
        path: "adaptive-horizon-zero-extension.json".to_owned(),
        file_sha256: "source".to_owned(),
        report: Box::new(imported_report),
        checkpoint: Some(source_checkpoint.clone()),
    };
    let mut stream = Vec::new();
    let live = run_campaign_checkpointed(
        &source_workload,
        &config(64, SuffixShape::OneToSix, 1, action_limit),
        &origin,
        &mut stream,
        None,
    )
    .expect("zero-extension campaign");
    let target_jobs = stream
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .skip(1)
        .filter_map(|line| {
            serde_json::from_slice::<CampaignStreamRecord<EmpiricalStepCheckpoint, TickKey>>(line)
                .ok()
        })
        .filter_map(|record| match record {
            CampaignStreamRecord::Job(job) if job.parent_id == target_id => Some(job),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        target_jobs.iter().any(|job| {
            job.adaptive_horizon_extension == 0 && job.adaptive_horizon_full_capacity
        })
    );
    let replayed = replay_campaign_checkpointed(
        &source_workload,
        &stream,
        Some(&replay_report),
        Some(&source_checkpoint),
    )
    .expect("zero replay");
    assert_eq!(replayed, live);
}

#[test]
fn adaptive_horizon_cycles_without_growing_past_action_limit() {
    let workload = HorizonWorkload {
        goal_after: 0,
        distractors: false,
    };
    let mut stream = Vec::new();
    let live = run_campaign_checkpointed_with_options(
        &workload,
        &config(16, SuffixShape::OneToSix, 1, 8),
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
        CampaignExecutionOptions {
            work_budget: Some(128),
            ..CampaignExecutionOptions::default()
        },
    )
    .expect("cycle campaign");
    assert_eq!(live.0.archive.entries.len(), 1);
    assert!(live.0.execution_work <= 128);
    assert!(String::from_utf8_lossy(&stream).contains("adaptive_horizon_extension"));
    assert_eq!(live.0.archive.entries[0].input.actions.len(), 0);
    let extensions = stream
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .skip(1)
        .map(|line| {
            serde_json::from_slice::<CampaignStreamRecord<EmpiricalStepCheckpoint, TickKey>>(line)
                .expect("cycle stream record")
        })
        .map(|record| match record {
            CampaignStreamRecord::Job(job) => job.adaptive_horizon_extension,
            CampaignStreamRecord::Skip(skip) => skip.adaptive_horizon_extension,
        })
        .collect::<Vec<_>>();
    let full_trial = extensions
        .iter()
        .position(|extension| *extension > 0 && !extension.is_power_of_two())
        .expect("full-capacity horizon trial");
    assert!(
        extensions[full_trial.saturating_add(1)..]
            .iter()
            .all(|extension| *extension == 0)
    );
    let replayed =
        replay_campaign_checkpointed(&workload, &stream, None, None).expect("cycle replay");
    assert_eq!(replayed, live);
}

#[test]
fn adaptive_horizon_reactivates_through_a_large_distractor_archive() {
    let workload = HorizonWorkload {
        goal_after: 20,
        distractors: true,
    };
    let mut stream = Vec::new();
    let live = run_campaign_checkpointed(
        &workload,
        &CampaignConfig {
            archive_entry_limit: 256,
            ..config(2048, SuffixShape::OneToSix, 2, 64)
        },
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
    )
    .expect("distractor campaign");
    assert!(live.0.archive.entries.len() >= 32);
    assert!(
        live.0
            .archive
            .entries
            .iter()
            .any(|entry| entry.key == TickKey(1) && entry.input.actions.len() >= 20)
    );
    let replayed =
        replay_campaign_checkpointed(&workload, &stream, None, None).expect("distractor replay");
    assert_eq!(replayed, live);
}

#[test]
fn bounded_suffix_keeps_its_existing_work_bound() {
    let workload = HorizonWorkload {
        goal_after: 20,
        distractors: false,
    };
    let mut stream = Vec::new();
    let live = run_campaign_checkpointed(
        &workload,
        &config(2, SuffixShape::OneToSixBounded, 1, 8),
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
    )
    .expect("bounded campaign");
    assert_eq!(live.0.execution_work, 6);
    assert_eq!(live.0.archive.entries.len(), 1);
    assert_eq!(live.0.archive.entries[0].input.actions.len(), 0);
    let replayed =
        replay_campaign_checkpointed(&workload, &stream, None, None).expect("bounded replay");
    assert_eq!(replayed, live);
}

#[test]
fn adaptive_horizon_replays_with_reservation_lag_and_variable_extensions() {
    let workload = HorizonWorkload {
        goal_after: 20,
        distractors: false,
    };
    let mut stream = Vec::new();
    let live = run_campaign_checkpointed(
        &workload,
        &config(32, SuffixShape::OneToSix, 2, 64),
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
    )
    .expect("multi-worker wait campaign");
    assert_eq!(live.0.workers, 2);
    let extended = live
        .0
        .archive
        .entries
        .iter()
        .find(|entry| entry.input.actions.len() >= 7)
        .expect("extended entry");
    assert!(
        extended.input.actions[6..]
            .iter()
            .all(|action| action.0 != 0)
    );
    let replayed =
        replay_campaign_checkpointed(&workload, &stream, None, None).expect("multi-worker replay");
    assert_eq!(replayed, live);
}
