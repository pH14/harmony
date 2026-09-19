use std::{cmp::Ordering, error::Error};

use searcher::search::{
    archive::{
        ArchiveEntryReport, ArchiveKey, Input, RetentionPolicy, SelectorPolicy, entries_by_suffix,
    },
    campaign::{
        ArchiveReportState, CampaignActionResult, CampaignConfig, CampaignJobResult,
        CampaignOrigin, CampaignTypes, Evaluation, InputPolicy, Reporting, TargetExecution,
        WorkloadPolicies, replay_campaign_checkpointed, run_campaign_checkpointed,
    },
    draw::{DrawMixture, MixtureDraw, SuffixShape},
    draw_tables::DrawTables,
    empirical_steps::EmpiricalStepCheckpoint,
    rand::RomuDuoJrRand,
    rollout::ExecutionDisposition,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct TickAction;

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
    execution_work: u64,
}

struct HorizonWorkload {
    goal_after_seven: bool,
}

impl CampaignTypes for HorizonWorkload {
    type Target = HorizonTarget;
    type Action = TickAction;
    type Key = TickKey;
    type Milestones = ();
    type Progress = ();
    type Snapshot = u8;
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
        8
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
        _: &mut RomuDuoJrRand,
    ) -> Result<Self::Action, Box<dyn Error>> {
        Ok(TickAction)
    }

    fn expand_suffix(
        &self,
        _: &Self::Run,
        _: &DrawTables<Self::Action>,
        _: SuffixShape,
        _: MixtureDraw,
        _: u64,
    ) -> Result<Vec<Self::Action>, Box<dyn Error>> {
        Ok(vec![TickAction; 6])
    }

    fn expand_suffix_recorded(
        &self,
        run: &Self::Run,
        state: &DrawTables<Self::Action>,
        shape: SuffixShape,
        mixture: MixtureDraw,
        _: Option<&EmpiricalStepCheckpoint>,
        mutation_seed: u64,
    ) -> Result<Vec<Self::Action>, Box<dyn Error>> {
        self.expand_suffix(run, state, shape, mixture, mutation_seed)
    }
}

impl TargetExecution for HorizonWorkload {
    fn new_target(&self) -> Result<Self::Target, String> {
        Ok(HorizonTarget::default())
    }

    fn reset(&self, target: &mut Self::Target) {
        target.ticks = 0;
    }

    fn restore(
        &self,
        target: &mut Self::Target,
        snapshot: &Self::Snapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.ticks = *snapshot;
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
        _: &Self::Action,
        _: &mut Self::Milestones,
    ) -> Result<(), Box<dyn Error>> {
        target.ticks = target.ticks.saturating_add(1);
        target.execution_work = target.execution_work.saturating_add(1);
        Ok(())
    }

    fn snapshot(&self, target: &mut Self::Target) -> Result<Self::Snapshot, Box<dyn Error>> {
        Ok(target.ticks)
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
        Ok(self.goal_after_seven && target.ticks >= 7)
    }

    fn current_key(&self, target: &Self::Target) -> Result<Self::Key, Box<dyn Error>> {
        Ok(TickKey(u8::from(
            self.goal_after_seven && target.ticks >= 7,
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

fn config(execution_budget: u64, suffix: SuffixShape) -> CampaignConfig<HorizonWorkload> {
    CampaignConfig {
        campaign_seed: 0x51a7_e001,
        workers: 1,
        execution_budget,
        action_limit: 8,
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
        selector: SelectorPolicy::GroupUniform,
        objective_witness_path: None,
    }
}

#[test]
fn adaptive_horizon_crosses_wait_and_replays_exactly() {
    let workload = HorizonWorkload {
        goal_after_seven: true,
    };
    let mut stream = Vec::new();
    let live = run_campaign_checkpointed(
        &workload,
        &config(2, SuffixShape::OneToSix),
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
    )
    .expect("wait campaign");
    assert_eq!(live.0.execution_work, 13);
    assert_eq!(live.0.archive.entries.len(), 2);
    assert_eq!(live.0.archive.entries[1].key, TickKey(1));
    assert_eq!(live.0.archive.entries[1].input.actions.len(), 7);
    let replayed =
        replay_campaign_checkpointed(&workload, &stream, None, None).expect("wait replay");
    assert_eq!(replayed, live);
}

#[test]
fn adaptive_horizon_cycles_without_growing_past_action_limit() {
    let workload = HorizonWorkload {
        goal_after_seven: false,
    };
    let mut stream = Vec::new();
    let live = run_campaign_checkpointed(
        &workload,
        &config(4, SuffixShape::OneToSix),
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
    )
    .expect("cycle campaign");
    assert_eq!(live.0.archive.entries.len(), 1);
    assert_eq!(live.0.execution_work, 29);
    assert_eq!(live.0.archive.entries[0].input.actions.len(), 0);
    let replayed =
        replay_campaign_checkpointed(&workload, &stream, None, None).expect("cycle replay");
    assert_eq!(replayed, live);
}

#[test]
fn bounded_suffix_keeps_its_existing_work_bound() {
    let workload = HorizonWorkload {
        goal_after_seven: true,
    };
    let mut stream = Vec::new();
    let live = run_campaign_checkpointed(
        &workload,
        &config(2, SuffixShape::OneToSixBounded),
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
