// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod actions;
pub mod chain;
pub mod deadline;
pub mod deadline_actions;
pub mod delayed;
pub mod maze;
pub mod resource;
pub mod route;
pub mod worlds;
use worlds::{State, World};

use searcher::search::{
    archive::{ArchiveEntryReport, ArchiveKey, Input, RetentionPolicy, entries_by_suffix},
    campaign::{
        ArchiveReportState, CampaignActionResult, CampaignConfig, CampaignExecutionOptions,
        CampaignJobResult, CampaignOrigin, CampaignTypes, Evaluation, InputPolicy, Reporting,
        TargetExecution, WorkloadPolicies, postcard_value_sha256, replay_campaign_checkpointed,
        run_campaign_checkpointed_with_options,
    },
    draw::SuffixShape,
    rand::RomuDuoJrRand,
    rollout::ExecutionDisposition,
};
use serde::{Deserialize, Serialize};
use std::{error::Error, io::Write, num::NonZeroUsize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Key {
    pub stock: u8,
    pub place: u16,
    pub context: u16,
    pub charge: u8,
    pub health: u8,
    pub goal: bool,
}
impl ArchiveKey for Key {
    type Place = u16;
    type Progress = bool;
    type Identity = u16;
    type Lineage = ();
    fn place(self) -> u16 {
        self.place
    }
    fn progress(self) -> bool {
        self.goal
    }
    fn identity(self) -> u16 {
        self.context
    }
    fn capacity() -> usize {
        1
    }
    fn preferences() -> usize {
        2
    }
    fn preference_cmp(self, preference: usize, other: Self) -> std::cmp::Ordering {
        if self.place / 256 != other.place / 256 {
            return self.stock.cmp(&other.stock);
        }
        if preference == 0 {
            (self.stock, self.charge, self.health).cmp(&(other.stock, other.charge, other.health))
        } else {
            (self.health, self.charge, self.stock).cmp(&(other.health, other.charge, other.stock))
        }
    }
    fn complete(self, _: Option<(Self, &())>) -> Self {
        self
    }
    fn record(_: &mut (), _: Self) {}
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Evidence {
    pub route_trace: Vec<route::Trace>,
    pub observations: u64,
    pub chain_stage_actions: Vec<u64>,
    pub chain_parent_selections: Vec<u64>,
    pub chain_pre_objective_parent_selections: Vec<u64>,
    pub chain_pre_objective_work_by_parent_stage: Vec<u64>,
    pub chain_work_by_parent_stage: Vec<u64>,
    pub chain_selected_charge: Vec<Vec<u64>>,
    pub chain_first_reach_work: Vec<Option<u64>>,
    pub chain_stage_entries: Vec<u64>,
    pub chain_entry_charge: Vec<Vec<u64>>,
    pub arrivals: [u64; 32],
    pub refills: u64,
    pub objectives: u64,
    pub health_recoveries: u64,
    pub correct_history_arrivals: u64,
    pub wrong_history_arrivals: u64,
    pub loop_resets: u64,
    pub explicit_resets: u64,
    pub action_attempts: [u64; 12],
    pub action_advances: [u64; 3],
    pub deadline_arrivals_by_remaining: Vec<u64>,
    pub deadline_expirations: u64,
    pub deadline_obstacle_attempts: u64,
    pub delayed_useful_state_actions: u64,
    pub delayed_distraction_actions: u64,
    pub delayed_progress_observations: Vec<u64>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArchiveReport {
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<ArchiveEntryReport<u8, Key, bool>>,
    pub evidence: Evidence,
    pub selector: searcher::search::archive::SelectorAccounting,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Observation {
    before: State,
    after: State,
    execution_work: u64,
    job_work: Option<u64>,
}
pub struct Target {
    state: State,
    work: u64,
    observation: Option<Observation>,
}
pub struct Workload {
    pub config: World,
    pub broken: bool,
}
impl CampaignTypes for Workload {
    type Target = Target;
    type Action = u8;
    type Key = Key;
    type Milestones = bool;
    type Progress = bool;
    type Snapshot = State;
    type Observations = Observation;
    type Evidence = Evidence;
    type ArchiveReport = ArchiveReport;
    type Run = ();
}
impl Reporting for Workload {
    fn stream_format(&self) -> &'static str {
        "tiny-world-v1"
    }
    fn checkpoint_format(&self) -> &'static str {
        "tiny-world-checkpoint-v1"
    }
    fn workload_identity_sha256(&self) -> String {
        postcard_value_sha256(&(&self.config, self.broken)).expect("serializable config")
    }
    fn action_cost_unit(&self) -> &'static str {
        "transitions"
    }
    fn execution_work_unit(&self) -> &'static str {
        "transitions"
    }
    fn result_sha256(&self, result: &CampaignJobResult<Self>) -> Result<String, Box<dyn Error>> {
        postcard_value_sha256(result)
    }
    fn archive_report(
        &self,
        evidence: &Evidence,
        state: ArchiveReportState<Self>,
    ) -> ArchiveReport {
        ArchiveReport {
            entries: state.entries,
            evidence: evidence.clone(),
            selector: state.selector,
        }
    }
}
impl InputPolicy for Workload {
    fn max_action_limit(&self) -> usize {
        self.config.action_limit()
    }
    fn max_action_cost(&self) -> u64 {
        1
    }
    fn policies(&self, _: &()) -> WorkloadPolicies {
        [(
            "tiny_actions".into(),
            if self.config.changes_actions() && self.broken {
                "frozen-land-v1".into()
            } else {
                "uniform-four-v1".into()
            },
        )]
        .into_iter()
        .collect()
    }
    fn resolve_recorded(&self, policies: &WorkloadPolicies) -> Result<(), Box<dyn Error>> {
        if *policies != self.policies(&()) {
            return Err("unknown tiny-world policy".into());
        }
        Ok(())
    }
    fn sample_alphabet(&self, _: &(), rand: &mut RomuDuoJrRand) -> Result<u8, Box<dyn Error>> {
        Ok(self
            .config
            .sample_action(rand.below(NonZeroUsize::new(4).unwrap()) as u8, self.broken))
    }
}
impl TargetExecution for Workload {
    fn new_target(&self) -> Result<Target, String> {
        self.config.validate()?;
        Ok(Target {
            state: self.config.initial(),
            work: 0,
            observation: None,
        })
    }
    fn reset(&self, target: &mut Target) {
        target.state = self.config.initial();
        target.observation = None;
    }
    fn restore(&self, target: &mut Target, snapshot: &State) -> Result<(), Box<dyn Error>> {
        if !self.config.valid_state(*snapshot) {
            return Err("invalid snapshot state for world".into());
        }
        target.state = *snapshot;
        target.observation = None;
        Ok(())
    }
    fn execution_work(&self, target: &Target) -> u64 {
        target.work
    }
    fn action_cost_fn(&self) -> fn(&u8) -> u64 {
        |_| 1
    }
    fn snapshot_memory_charge(_: &State) -> usize {
        std::mem::size_of::<State>()
    }
    fn apply_action(
        &self,
        target: &mut Target,
        action: &u8,
        milestones: &mut bool,
    ) -> Result<(), Box<dyn Error>> {
        let before = target.state;
        target.state = self.config.step(target.state, *action);
        target.observation = Some(Observation {
            before,
            after: target.state,
            execution_work: target.work + 1,
            job_work: None,
        });
        target.work += 1;
        *milestones |= self.config.goal(target.state);
        Ok(())
    }
    fn rollout_observations(&self, target: &Target) -> Vec<Observation> {
        target.observation.iter().cloned().collect()
    }
    #[allow(clippy::too_many_arguments)]
    fn execute_job(
        &self,
        run: &(),
        target: &mut Target,
        origin_snapshot: &State,
        replay: &[u8],
        parent_actions: usize,
        parent_milestones: bool,
        suffix: &[u8],
        max_actions: usize,
        retention: RetentionPolicy,
        stop_rollout_on_objective: bool,
    ) -> Result<CampaignJobResult<Self>, Box<dyn Error>> {
        let start_work = target.work;
        let mut result = searcher::search::rollout::execute_job(
            self,
            run,
            target,
            origin_snapshot,
            replay,
            parent_actions,
            parent_milestones,
            suffix,
            max_actions,
            retention,
            stop_rollout_on_objective,
        )?;
        if matches!(self.config, World::Chain(_)) {
            let work = target.work - start_work;
            if let Some(observation) = result
                .actions
                .first_mut()
                .and_then(|a| a.observations.first_mut())
            {
                observation.job_work = Some(work);
            } else if work != 0 {
                return Err("chain job executed work without an observation".into());
            }
        }
        Ok(result)
    }
    fn snapshot(&self, target: &mut Target) -> Result<State, Box<dyn Error>> {
        Ok(target.state)
    }
}
impl Evaluation for Workload {
    fn execution_disposition(&self, _: &Target) -> ExecutionDisposition {
        ExecutionDisposition::Runnable
    }
    fn objective_reached(&self, _: &(), target: &Target) -> Result<bool, Box<dyn Error>> {
        Ok(self.config.goal(target.state))
    }
    fn current_key(&self, target: &Target) -> Result<Key, Box<dyn Error>> {
        Ok(self.config.key(target.state, self.broken))
    }
    fn complete_candidate_key(&self, key: Key, _: &State) -> Result<Key, Box<dyn Error>> {
        Ok(key)
    }
    fn merge_milestones(&self, into: &mut bool, from: bool) {
        *into |= from;
    }
    fn aggregate_milestones(e: &Evidence) -> bool {
        e.objectives > 0
    }
    fn aggregate_progress(e: &Evidence) -> bool {
        e.objectives > 0
    }
    fn merge_origin_evidence(&self, e: &mut Evidence, s: &ArchiveReport) {
        *e = s.evidence.clone();
    }
    fn merge_snapshot_root_evidence(
        &self,
        e: &mut Evidence,
        t: &Target,
    ) -> Result<(), Box<dyn Error>> {
        e.objectives += u64::from(self.config.goal(t.state));
        Ok(())
    }
    fn merge_import_evidence(&self, e: &mut Evidence, m: bool, _: &Input<u8>) {
        e.objectives += u64::from(m);
    }
    fn merge_action_evidence<F>(
        &self,
        e: &mut Evidence,
        a: &CampaignActionResult<Self>,
        sequence: u64,
        _: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<Input<u8>, Box<dyn Error>>,
    {
        for observation in &a.observations {
            e.observations += 1;
            match (&self.config, observation.before, observation.after) {
                (World::Route(_), State::Route(before), State::Route(after)) => {
                    e.route_trace.push(route::Trace {
                        sequence,
                        action: a.action,
                        candidate: a.candidate.is_some(),
                        objective: a.outcome.objective_reached,
                        before,
                        after,
                    });
                }
                (World::Resource(w), State::Resource(before), State::Resource(after)) => {
                    if after.place == w.corridor_len && before.place != after.place {
                        e.arrivals[usize::from(after.charge)] += 1;
                    }
                    e.refills += u64::from(after.charge > before.charge);
                    e.health_recoveries += u64::from(after.health > before.health);
                }
                (World::Maze(w), State::Maze(before), State::Maze(after)) => {
                    if after.place == w.length && before.place != after.place {
                        if w.goal(w.step(after, 0)) {
                            e.correct_history_arrivals += 1;
                        } else {
                            e.wrong_history_arrivals += 1;
                        }
                    }
                    e.loop_resets +=
                        u64::from(before.place == w.length && after.place == 0 && a.action == 0);
                    e.explicit_resets +=
                        u64::from(before.place > 0 && after.place == 0 && a.action == 2);
                }
                (World::Actions(_), State::Actions(before), State::Actions(after)) => {
                    if !before.goal {
                        let index = usize::from(before.regime) * 4 + usize::from(a.action);
                        e.action_attempts[index] += 1;
                        e.action_advances[usize::from(before.regime)] += u64::from(before != after);
                    }
                }
                (World::Deadline(w), State::Deadline(before), State::Deadline(after)) => {
                    e.deadline_arrivals_by_remaining.resize(65, 0);
                    if before.position != after.position {
                        e.deadline_arrivals_by_remaining[usize::from(after.remaining)] += 1;
                    }
                    e.deadline_expirations +=
                        u64::from(before.remaining > 0 && after.remaining == 0 && !after.goal);
                    e.deadline_obstacle_attempts += u64::from(
                        before.position == w.length && before.remaining > 0 && a.action == 2,
                    );
                }
                (World::Delayed(_), State::Delayed(before), State::Delayed(after)) => {
                    if !before.goal {
                        e.delayed_useful_state_actions += u64::from(before.lane == 0);
                        e.delayed_distraction_actions += u64::from(before.lane != 0);
                        e.delayed_progress_observations.resize(17, 0);
                        e.delayed_progress_observations[usize::from(after.progress)] += 1;
                    }
                }
                (
                    World::DeadlineActions(w),
                    State::DeadlineActions(before),
                    State::DeadlineActions(after),
                ) => {
                    if !w.goal(before) && before.remaining > 0 {
                        let index = usize::from(before.actions.regime) * 4 + usize::from(a.action);
                        e.action_attempts[index] += 1;
                        let advanced = before.actions != after.actions;
                        e.action_advances[usize::from(before.actions.regime)] +=
                            u64::from(advanced);
                        e.deadline_arrivals_by_remaining.resize(65, 0);
                        if advanced {
                            e.deadline_arrivals_by_remaining[usize::from(after.remaining)] += 1;
                        }
                        e.deadline_expirations += u64::from(after.remaining == 0 && !w.goal(after));
                    }
                }
                (World::Chain(w), State::Chain(before), State::Chain(after)) => {
                    e.chain_parent_selections.resize(w.stages.len(), 0);
                    e.chain_pre_objective_parent_selections
                        .resize(w.stages.len(), 0);
                    e.chain_pre_objective_work_by_parent_stage
                        .resize(w.stages.len(), 0);
                    e.chain_work_by_parent_stage.resize(w.stages.len(), 0);
                    e.chain_selected_charge
                        .resize_with(w.stages.len(), || vec![0; 32]);
                    if let Some(work) = observation.job_work {
                        let stage = usize::from(before.stage);
                        e.chain_parent_selections[stage] += 1;
                        if e.objectives == 0 {
                            e.chain_pre_objective_parent_selections[stage] += 1;
                            e.chain_pre_objective_work_by_parent_stage[stage] += work;
                        }
                        e.chain_work_by_parent_stage[stage] += work;
                        e.chain_selected_charge[stage][usize::from(before.charge)] += 1;
                    }
                    e.chain_first_reach_work.resize(w.stages.len(), None);
                    e.chain_first_reach_work[0] = Some(0);
                    e.chain_stage_actions.resize(w.stages.len(), 0);
                    e.chain_stage_entries.resize(w.stages.len(), 0);
                    e.chain_entry_charge
                        .resize_with(w.stages.len(), || vec![0; 32]);
                    if !w.goal(before) {
                        e.chain_stage_actions[usize::from(before.stage)] += 1;
                    }
                    if after.stage != before.stage {
                        e.chain_first_reach_work[usize::from(after.stage)]
                            .get_or_insert(observation.execution_work);
                        e.chain_stage_entries[usize::from(after.stage)] += 1;
                        e.chain_entry_charge[usize::from(after.stage)]
                            [usize::from(after.charge)] += 1;
                    }
                }
                _ => return Err("observation family mismatch".into()),
            }
            e.objectives += u64::from(self.config.goal(observation.after));
        }
        Ok(())
    }
    fn source_entries<'a>(&self, s: &'a ArchiveReport) -> &'a [ArchiveEntryReport<u8, Key, bool>] {
        &s.entries
    }
    fn resume_input(&self, _: &ArchiveReport) -> Result<Input<u8>, Box<dyn Error>> {
        Err("archive origin unsupported".into())
    }
}

struct BoundedStream(Vec<u8>);
impl Write for BoundedStream {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len() + bytes.len() > 32_000_000 {
            return Err(std::io::Error::other("stream bound exceeded"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn run(
    workload: &Workload,
    seed: u64,
    budget: u64,
    verify: bool,
) -> Result<serde_json::Value, Box<dyn Error>> {
    workload.config.validate()?;
    if budget == 0 || budget > 20_000 {
        return Err("work budget must be 1..=20000".into());
    }
    let config = campaign_config(workload, seed, budget);
    let mut stream = BoundedStream(Vec::new());
    let started = telemetry_now();
    let live = run_campaign_checkpointed_with_options(
        workload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
        CampaignExecutionOptions {
            work_budget: Some(budget),
            ..Default::default()
        },
    )?;
    let elapsed = started.elapsed().as_secs_f64();
    if verify {
        let replay = replay_campaign_checkpointed(workload, &stream.0, None, None)?;
        if replay != live {
            return Err("campaign replay mismatch".into());
        }
        let mut again = BoundedStream(Vec::new());
        run_campaign_checkpointed_with_options(
            workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut again,
            None,
            CampaignExecutionOptions {
                work_budget: Some(budget),
                ..Default::default()
            },
        )?;
        if again.0 != stream.0 {
            return Err("fixed-work rerun mismatch".into());
        }
    }
    let report = live.0;
    if let Some(witness) = &report.objective_witness {
        let mut state = workload.config.initial();
        for action in &witness.actions {
            state = workload.config.step(state, *action);
        }
        if !workload.config.goal(state) {
            return Err("invalid objective witness".into());
        }
    }
    let route_evidence = route::summarize(&report.archive.evidence.route_trace, &stream.0)?;
    let mut work = 0;
    let mut first_objective_work = None;
    let mut continuation_work = 0;
    let mut continuation_jobs = 0;
    let mut pre_objective_continuation_jobs = 0;
    let mut pre_objective_continuation_work = 0;
    for line in stream
        .0
        .split(|c| *c == b'\n')
        .filter(|line| !line.is_empty())
    {
        let value: serde_json::Value = serde_json::from_slice(line)?;
        if value["event"] == "job" {
            let job_work = value["execution_work"].as_u64().ok_or("missing work")?;
            work += job_work;
            if value["selector"]["path"] == "continuation" {
                continuation_work += job_work;
                continuation_jobs += 1;
                if first_objective_work.is_none() {
                    pre_objective_continuation_jobs += 1;
                    pre_objective_continuation_work += job_work;
                }
            }
            if value["decisions"]
                .as_array()
                .ok_or("missing decisions")?
                .iter()
                .any(|d| d["decision"] == "objective")
            {
                first_objective_work.get_or_insert(work);
            }
        }
    }
    if work > budget + workload.config.action_limit() as u64 - 1 {
        return Err("one-reservation work overshoot exceeded the action bound".into());
    }
    if work != report.execution_work {
        return Err("independent work accounting mismatch".into());
    }
    if first_objective_work != report.work_to_first_objective
        || first_objective_work.is_some() != report.objective_witness.is_some()
    {
        return Err("independent first-objective scoring mismatch".into());
    }
    if matches!(workload.config, World::Chain(_))
        && report
            .archive
            .evidence
            .chain_work_by_parent_stage
            .iter()
            .sum::<u64>()
            != work
    {
        return Err("chain parent work differs from independently summed execution work".into());
    }
    if matches!(workload.config, World::Chain(_))
        && report
            .archive
            .evidence
            .chain_pre_objective_work_by_parent_stage
            .iter()
            .sum::<u64>()
            != first_objective_work.unwrap_or(work)
    {
        return Err("chain pre-objective work differs from first-objective accounting".into());
    }
    Ok(
        serde_json::json!({"engine_source_sha256":env!("TINY_ENGINE_SOURCE_SHA256"),
        "workload_source_sha256":env!("TINY_WORKLOAD_SOURCE_SHA256"),"seed":seed,"broken":workload.broken,
        "config":workload.config,"work_budget":budget,"work":work,"work_overshoot":work.saturating_sub(budget),
        "chain_parent_selections":report.archive.evidence.chain_parent_selections,
        "chain_work_by_parent_stage":report.archive.evidence.chain_work_by_parent_stage,
        "chain_selected_charge":report.archive.evidence.chain_selected_charge,
        "action_limit":workload.config.action_limit(),
        "route_evidence":route_evidence,
        "pre_objective_continuation_jobs":pre_objective_continuation_jobs,
        "pre_objective_continuation_work":pre_objective_continuation_work,
        "continuation_work":continuation_work,"continuation_jobs":continuation_jobs,
        "first_objective_work":first_objective_work,
        "success":first_objective_work.is_some_and(|w|w<=budget),
        "elapsed_seconds":elapsed,"stream_bytes":stream.0.len(),"stream_sha256":report.stream_sha256,
        "resident_memory_bytes":report.resident_memory_bytes,"evidence":report.archive.evidence,
        "exported_entry_count":report.archive.entries.len(),"live_entries":report.live_entries,"selector":report.archive.selector,"verified":verify}),
    )
}

fn campaign_config(workload: &Workload, seed: u64, budget: u64) -> CampaignConfig<Workload> {
    CampaignConfig {
        campaign_seed: seed,
        workers: 1,
        execution_budget: budget,
        action_limit: workload.config.action_limit(),
        host: "tiny-worlds".into(),
        wall_budget: None,
        stop_rollout_on_objective: true,
        stop_campaign_on_objective: false,
        archive_entry_limit: 4096,
        reservations_per_worker: 1,
        memory_budget_mib: Some(32),
        materialize_final_artifacts: true,
        run: (),
        suffix: SuffixShape::OneOrTwo,
        mixture: workload.config.mixture(),
        retention: RetentionPolicy::Unprobed,
        objective_witness_path: None,
    }
}

#[allow(clippy::disallowed_methods)]
fn telemetry_now() -> std::time::Instant {
    std::time::Instant::now()
}

#[cfg(test)]
fn test_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let seed = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    eprintln!("campaign replay seed: {seed}");
    seed
}

#[cfg(test)]
mod tests {
    use super::*;
    use searcher::search::campaign::{
        CampaignCheckpoint, SnapshotCheckpoint, SnapshotCheckpointEntry,
    };

    fn world() -> Workload {
        Workload {
            config: World::Resource(resource::Config {
                initial_charge: 0,
                initial_health: 3,
                barrier_charge: 5,
                route_cost: 0,
                health_cost: 0,
                refill_amount: 1,
                max_charge: 8,
                corridor_len: 1,
                refill_health_cost: 0,
            }),
            broken: false,
        }
    }

    #[test]
    fn restore_preserves_state_but_not_cumulative_work() {
        let w = world();
        let mut t = w.new_target().unwrap();
        let snapshot = w.snapshot(&mut t).unwrap();
        w.apply_action(&mut t, &1, &mut false).unwrap();
        assert_ne!(t.state, snapshot);
        w.restore(&mut t, &snapshot).unwrap();
        assert_eq!(t.state, snapshot);
        assert_eq!(w.execution_work(&t), 1);
    }

    #[test]
    fn root_objective_does_not_depend_on_first_observation() {
        let w = world();
        let mut state = w.config.initial();
        for _ in 0..5 {
            state = w.config.step(state, 1);
        }
        for _ in 0..3 {
            state = w.config.step(state, 0);
        }
        assert!(w.config.goal(state));
        assert_root_objective(w, state);
    }

    fn assert_root_objective(w: Workload, state: State) {
        let snapshots = SnapshotCheckpoint {
            format: w.checkpoint_format().into(),
            entries: vec![SnapshotCheckpointEntry {
                id: 0,
                snapshot: state,
            }],
        };
        let origin = CampaignOrigin::SnapshotRoot {
            checkpoint: CampaignCheckpoint {
                path: "constructed-root".into(),
                file_sha256: postcard_value_sha256(&snapshots).unwrap(),
                snapshots,
            },
        };
        let mut config = campaign_config(&w, crate::test_seed(), 20);
        config.stop_campaign_on_objective = true;
        let mut stream = BoundedStream(Vec::new());
        let (report, _) = run_campaign_checkpointed_with_options(
            &w,
            &config,
            &origin,
            &mut stream,
            None,
            CampaignExecutionOptions::default(),
        )
        .unwrap();
        assert_eq!(report.work_to_first_objective, Some(0));
        assert_eq!(report.execution_work, 0);
    }

    #[test]
    fn maze_goal_at_root_and_delayed_submit_are_distinct() {
        let config = maze::Config {
            length: 5,
            pattern: 19,
            reverse_actions: true,
        };
        let w = Workload {
            config: World::Maze(config),
            broken: false,
        };
        let prefix = (0..5).fold(config.initial(), |s, bit| {
            config.step(s, ((12 >> bit) & 1) as u8)
        });
        let mut target = w.new_target().unwrap();
        w.restore(&mut target, &State::Maze(prefix)).unwrap();
        assert!(w.rollout_observations(&target).is_empty());
        assert!(!w.objective_reached(&(), &target).unwrap());
        let terminal = config.step(prefix, 0);
        assert!(
            w.restore(
                &mut target,
                &State::Maze(maze::State {
                    goal: false,
                    ..terminal
                })
            )
            .is_err()
        );
        assert_root_objective(w, State::Maze(terminal));
    }

    #[test]
    fn restore_rejects_wrong_family_and_out_of_bounds_state() {
        let w = world();
        let mut target = w.new_target().unwrap();
        assert!(
            w.restore(
                &mut target,
                &State::Maze(maze::State {
                    place: 0,
                    history: 0,
                    goal: false
                })
            )
            .is_err()
        );
        let mut invalid = match w.config.initial() {
            State::Resource(s) => s,
            _ => unreachable!(),
        };
        invalid.charge = 255;
        assert!(w.restore(&mut target, &State::Resource(invalid)).is_err());
        let World::Resource(config) = w.config else {
            unreachable!()
        };
        let terminal = resource::State {
            place: config.corridor_len + 1,
            charge: 0,
            health: 1,
            goal: true,
        };
        w.restore(&mut target, &State::Resource(terminal)).unwrap();
        assert!(
            w.restore(
                &mut target,
                &State::Resource(resource::State {
                    goal: false,
                    ..terminal
                })
            )
            .is_err()
        );
    }

    #[test]
    fn exhaustive_small_oracle_matches_independent_resource_bound() {
        for corridor_len in 1..=3 {
            for route_cost in 0..=2 {
                for max_charge in 4..=12 {
                    for health_cost in 0..=3 {
                        let config = resource::Config {
                            corridor_len,
                            route_cost,
                            max_charge,
                            health_cost,
                            ..match world().config {
                                World::Resource(w) => w,
                                _ => unreachable!(),
                            }
                        };
                        let expected =
                            max_charge >= 5 + corridor_len * route_cost && 3 > health_cost;
                        assert_eq!(config.reachable().unwrap(), expected, "{config:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn observations_are_absent_until_an_action_and_never_restore_stale_events() {
        let w = world();
        let mut t = w.new_target().unwrap();
        assert!(w.rollout_observations(&t).is_empty());
        let root = w.snapshot(&mut t).unwrap();
        w.apply_action(&mut t, &1, &mut false).unwrap();
        assert_eq!(w.rollout_observations(&t).len(), 1);
        w.restore(&mut t, &root).unwrap();
        assert!(w.rollout_observations(&t).is_empty());
        assert!(!w.objective_reached(&(), &t).unwrap());
    }

    #[test]
    fn fixed_work_campaign_replays_and_accounts_for_every_transition() {
        let report = run(&world(), crate::test_seed(), 1000, true).unwrap();
        assert_eq!(report["verified"], true);
        let mut broken = world();
        broken.broken = true;
        let control = run(&broken, crate::test_seed(), 1000, true).unwrap();
        assert_eq!(control["verified"], true);
    }

    #[test]
    fn every_family_uses_the_real_engine_and_replays_at_fixed_work() {
        let configs = [
            World::Deadline(deadline::Config {
                length: 2,
                initial_time: 8,
                fast_ticks: 1,
                slow_ticks: 4,
                obstacle_ticks: 2,
            }),
            World::Delayed(delayed::Config {
                horizon: 5,
                distractions: 8,
                mode: delayed::Mode::Wait,
            }),
            World::DeadlineActions(deadline_actions::Config {
                actions: actions::Config {
                    segment_len: 2,
                    return_to_land: true,
                    observable: true,
                    water_action: 1,
                },
                initial_time: 12,
                action_ticks: [1, 2, 1, 1],
            }),
            World::Maze(maze::Config {
                length: 4,
                pattern: 10,
                reverse_actions: true,
            }),
            World::Actions(actions::Config {
                segment_len: 3,
                return_to_land: true,
                observable: true,
                water_action: 1,
            }),
            World::Actions(actions::Config {
                segment_len: 3,
                return_to_land: true,
                observable: false,
                water_action: 1,
            }),
        ];
        for config in configs {
            for broken in [false, true] {
                let workload = Workload {
                    config: config.clone(),
                    broken,
                };
                let report = run(&workload, crate::test_seed(), 1000, true).unwrap();
                assert_eq!(report["verified"], true);
            }
        }
    }

    #[test]
    fn deadline_restore_keeps_clock_separate_from_cumulative_work() {
        let config = deadline::Config {
            length: 2,
            initial_time: 10,
            fast_ticks: 1,
            slow_ticks: 4,
            obstacle_ticks: 2,
        };
        let w = Workload {
            config: World::Deadline(config),
            broken: false,
        };
        let mut t = w.new_target().unwrap();
        let root = w.snapshot(&mut t).unwrap();
        w.apply_action(&mut t, &0, &mut false).unwrap();
        assert_eq!(
            t.state,
            State::Deadline(deadline::State {
                position: 1,
                remaining: 6,
                ..config.initial()
            })
        );
        assert_eq!(w.execution_work(&t), 1);
        w.restore(&mut t, &root).unwrap();
        assert_eq!(t.state, root);
        assert_eq!(w.execution_work(&t), 1);
        let goal = [1, 1, 1, 1, 2]
            .into_iter()
            .fold(config.initial(), |s, a| config.step(s, a));
        assert_root_objective(w, State::Deadline(goal));
    }

    #[test]
    fn clock_preference_preserves_fast_arrivals_in_both_orders() {
        use searcher::search::archive::{Archive, ArchiveCandidate};
        let w = deadline::Config {
            length: 2,
            initial_time: 10,
            fast_ticks: 1,
            slow_ticks: 4,
            obstacle_ticks: 3,
        };
        for fast_first in [false, true] {
            for broken in [false, true] {
                let mut archive = Archive::<u8, Key, bool, State>::new(|_| 1);
                for fast in [fast_first, !fast_first] {
                    let suffix = if fast { vec![1, 1, 1, 1] } else { vec![0, 0] };
                    let state = suffix.iter().fold(w.initial(), |s, &a| w.step(s, a));
                    archive
                        .insert(
                            None,
                            suffix.len() as u64,
                            ArchiveCandidate {
                                suffix,
                                key: w.key(state, broken),
                                milestones: false,
                            },
                            State::Deadline(state),
                        )
                        .unwrap();
                }
                assert_eq!(archive.active_count(), 1);
                let (id, _) = archive
                    .select_parent(&mut RomuDuoJrRand::with_seed(crate::test_seed()), 128)
                    .unwrap();
                let (reports, snapshots) = archive.take_entry_reports_and_snapshots();
                let snapshot_id = reports[id].id;
                let (_, State::Deadline(state)) = snapshots
                    .into_iter()
                    .find(|(i, _)| *i == snapshot_id)
                    .unwrap()
                else {
                    panic!("deadline snapshot");
                };
                assert_eq!(state.remaining, if broken { 2 } else { 6 });
                assert_eq!(w.goal(w.step(state, 2)), !broken);
            }
        }
    }

    #[test]
    fn archive_retains_useful_maze_history_in_both_arrival_orders() {
        use searcher::search::archive::{Archive, ArchiveCandidate};
        let maze = maze::Config {
            length: 4,
            pattern: 10,
            reverse_actions: false,
        };
        for order in [[10_u16, 5_u16], [5_u16, 10_u16]] {
            for broken in [false, true] {
                let mut archive = Archive::<u8, Key, bool, State>::new(|_| 1);
                for history in order {
                    let input: Vec<u8> = (0..4).map(|bit| ((history >> bit) & 1) as u8).collect();
                    let state = input.iter().fold(maze.initial(), |s, &a| maze.step(s, a));
                    assert_eq!(state.place, 4);
                    archive
                        .insert(
                            None,
                            0,
                            ArchiveCandidate {
                                suffix: input,
                                key: maze.key(state, broken),
                                milestones: false,
                            },
                            State::Maze(state),
                        )
                        .unwrap();
                }
                assert_eq!(archive.active_count(), if broken { 1 } else { 2 });
                if !broken {
                    let (_, snapshots) = archive.take_entry_reports_and_snapshots();
                    assert!(snapshots.iter().any(|(_, state)| matches!(state,
                        State::Maze(state) if state.history == 10)));
                }
            }
        }
    }

    #[test]
    fn reversed_history_diagnostics_do_not_depend_on_control_or_reset_choice() {
        use searcher::search::rollout::Outcome;
        let config = maze::Config {
            length: 5,
            pattern: 19,
            reverse_actions: true,
        };
        for broken in [false, true] {
            let w = Workload {
                config: World::Maze(config),
                broken,
            };
            let mut evidence = Evidence::default();
            for history in [12_u16, 19_u16] {
                let before = maze::State {
                    place: 4,
                    history: history & 15,
                    goal: false,
                };
                let action = ((history >> 4) & 1) as u8;
                let after = config.step(before, action);
                let result = CampaignActionResult::<Workload> {
                    action,
                    observations: vec![Observation {
                        execution_work: 0,
                        job_work: None,
                        before: State::Maze(before),
                        after: State::Maze(after),
                    }],
                    milestones: false,
                    outcome: Outcome::default(),
                    candidate: None,
                };
                w.merge_action_evidence(&mut evidence, &result, 0, || Ok(Input::default()))
                    .unwrap();
            }
            assert_eq!(evidence.correct_history_arrivals, 1);
            assert_eq!(evidence.wrong_history_arrivals, 1);
            for action in [0, 2] {
                let before = maze::State {
                    place: 5,
                    history: 19,
                    goal: false,
                };
                let result = CampaignActionResult::<Workload> {
                    action,
                    observations: vec![Observation {
                        execution_work: 0,
                        job_work: None,
                        before: State::Maze(before),
                        after: State::Maze(config.step(before, action)),
                    }],
                    milestones: false,
                    outcome: Outcome::default(),
                    candidate: None,
                };
                w.merge_action_evidence(&mut evidence, &result, 0, || Ok(Input::default()))
                    .unwrap();
            }
            assert_eq!(evidence.loop_resets, 1);
            assert_eq!(evidence.explicit_resets, 1);
        }
    }

    #[test]
    fn tagged_world_schema_rejects_unconsumed_fields() {
        let valid = serde_json::to_value(world().config).unwrap();
        let mut extra = valid.clone();
        extra["unconsumed"] = serde_json::json!(1);
        assert!(serde_json::from_value::<World>(extra).is_err());
        let mut wrong = valid;
        wrong["family"] = serde_json::json!("unsupported");
        assert!(serde_json::from_value::<World>(wrong).is_err());
    }

    #[test]
    fn broken_representation_erases_only_stock_preference() {
        let correct = world();
        let mut broken = world();
        broken.broken = true;
        let mut t = correct.new_target().unwrap();
        let empty = correct.current_key(&t).unwrap();
        for _ in 0..5 {
            correct.apply_action(&mut t, &1, &mut false).unwrap();
        }
        let stocked = correct.current_key(&t).unwrap();
        assert!(stocked.preference_cmp(0, empty).is_gt());
        assert_eq!(broken.current_key(&t).unwrap(), empty);
        assert_eq!(
            correct.config.step(t.state, 0),
            broken.config.step(t.state, 0)
        );
    }
}
