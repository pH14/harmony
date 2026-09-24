// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod actions;
pub mod maze;
pub mod resource;
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
        if preference == 0 {
            (self.charge, self.health).cmp(&(other.charge, other.health))
        } else {
            (self.health, self.charge).cmp(&(other.health, other.charge))
        }
    }
    fn complete(self, _: Option<(Self, &())>) -> Self {
        self
    }
    fn record(_: &mut (), _: Self) {}
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Evidence {
    pub observations: u64,
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
        128
    }
    fn max_action_cost(&self) -> u64 {
        1
    }
    fn policies(&self, _: &()) -> WorkloadPolicies {
        [(
            "tiny_actions".into(),
            if matches!(self.config, World::Actions(_)) && self.broken {
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
        });
        target.work += 1;
        *milestones |= self.config.goal(target.state);
        Ok(())
    }
    fn rollout_observations(&self, target: &Target) -> Vec<Observation> {
        target.observation.iter().cloned().collect()
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
        _: u64,
        _: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<Input<u8>, Box<dyn Error>>,
    {
        for observation in &a.observations {
            e.observations += 1;
            match (self.config, observation.before, observation.after) {
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
    let mut work = 0;
    let mut first_objective_work = None;
    let mut continuation_work = 0;
    let mut continuation_jobs = 0;
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
    if work > budget + 127 {
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
    Ok(
        serde_json::json!({"engine_source_sha256":env!("TINY_ENGINE_SOURCE_SHA256"),
        "workload_source_sha256":env!("TINY_WORKLOAD_SOURCE_SHA256"),"seed":seed,"broken":workload.broken,
        "config":workload.config,"work_budget":budget,"work":work,"work_overshoot":work.saturating_sub(budget),
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
        action_limit: 128,
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
        let mut config = campaign_config(&w, 17, 20);
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
        let report = run(&world(), 17, 1000, true).unwrap();
        assert_eq!(report["verified"], true);
        assert_eq!(report["success"], true);
        let mut broken = world();
        broken.broken = true;
        let control = run(&broken, 17, 1000, true).unwrap();
        assert_eq!(control["success"], false);
    }

    #[test]
    fn every_family_uses_the_real_engine_and_replays_at_fixed_work() {
        let configs = [
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
                let workload = Workload { config, broken };
                let report = run(&workload, 17, 1000, true).unwrap();
                assert_eq!(report["verified"], true);
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
                    let mut random = RomuDuoJrRand::with_seed(17);
                    let selected_correct = (0..64).any(|_| {
                        let (id, _) = archive.select_parent(&mut random, 128).unwrap();
                        archive.entry_key(id).unwrap().context == 10
                    });
                    assert!(selected_correct);
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
