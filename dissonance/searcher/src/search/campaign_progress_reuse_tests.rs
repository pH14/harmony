// SPDX-License-Identifier: AGPL-3.0-or-later
//! Small explicit worlds for progress-word reuse, including a fatal phase change.
use super::*;
use crate::search::archive::{RetireThresholds, ScopedProgress, SlotRetentionPolicy};
use crate::search::continuation::ContinuationLearning;
use crate::search::draw::draw_suffix;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct Key {
    value: u8,
    health: u8,
}
impl ArchiveKey for Key {
    type Group = u8;
    type Lineage = ();
    fn groups() -> usize {
        2
    }
    fn group(self, depth: usize) -> u8 {
        assert!(depth < 2);
        if depth == 0 { self.value / 4 } else { 0 }
    }
    fn slot_capacity() -> usize {
        1
    }
    fn preference_cmp(self, other: Self) -> std::cmp::Ordering {
        self.health.cmp(&other.health)
    }
    fn retention_resources(self) -> Option<[u64; 2]> {
        Some([u64::from(self.health), 0])
    }
    fn retention_progress(self) -> Option<ScopedProgress> {
        Some(ScopedProgress {
            scope: 1,
            value: u64::from(self.value),
        })
    }
    fn complete(self, _: Option<(Self, &())>) -> Self {
        self
    }
    fn record(_: &mut (), _: Self) {}
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct State {
    value: u8,
    health: u8,
    previous: Option<u8>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            value: 0,
            health: 1,
            previous: None,
        }
    }
}
impl State {
    fn key(&self) -> Key {
        Key {
            value: self.value,
            health: self.health,
        }
    }
    fn step(&mut self, adverse: bool, bit: u8) {
        if self.health == 0 {
            return;
        }
        let desired = if adverse && self.value % 2 == 1 {
            [1, 0]
        } else {
            [0, 1]
        };
        match self.previous {
            Some(first) if [first, bit] == desired => {
                self.value = self.value.saturating_add(1);
                self.previous = None;
            }
            Some(0) if adverse && self.value % 2 == 1 && bit == 1 => {
                // Same known scope/resources at dispatch, different hidden phase.
                self.health = 0;
                self.previous = None;
            }
            _ => self.previous = Some(bit),
        }
    }
}
struct ModelTarget {
    state: State,
    frames: u64,
}
struct Model(bool);
impl CampaignTypes for Model {
    type Target = ModelTarget;
    type Action = u8;
    type Key = Key;
    type Milestones = ();
    type Progress = ();
    type Snapshot = State;
    type Observations = ();
    type Evidence = Option<u64>;
    type ArchiveReport = Vec<ArchiveEntryReport<u8, Key, ()>>;
    type Run = ();
    type DrawState = ();
    type DrawCheckpoint = ();
    type TableHeader = ();
}
impl Reporting for Model {
    fn stream_format(&self) -> &'static str {
        "progress-word-model-v1"
    }
    fn checkpoint_format(&self) -> &'static str {
        "progress-word-model-checkpoint-v1"
    }
    fn image_sha256(&self) -> String {
        "finite-world".into()
    }
    fn result_sha256(&self, result: &CampaignJobResult<Self>) -> Result<String, Box<dyn Error>> {
        postcard_value_sha256(result)
    }
    fn archive_report(
        &self,
        _: &Self::Evidence,
        state: ArchiveReportState<Self>,
    ) -> Self::ArchiveReport {
        state.entries
    }
}
impl InputPolicy for Model {
    fn draw_state_memory_reserve_bytes(&self, _: &(), _: usize) -> usize {
        0
    }
    fn draw_state_memory_bytes(&self, _: &()) -> usize {
        0
    }
    fn policies(&self, _: &()) -> GamePolicies {
        GamePolicies::from([(
            "phase_law".into(),
            if self.0 { "adverse" } else { "stationary" }.into(),
        )])
    }
    fn resolve_recorded(&self, policies: &GamePolicies) -> Result<(), Box<dyn Error>> {
        if *policies != self.policies(&()) {
            return Err("wrong finite world".into());
        }
        Ok(())
    }
    fn initial_draw_state(
        &self,
        _: &(),
        _: Option<(&str, &Self::ArchiveReport)>,
    ) -> Result<InitialDrawState<Self>, Box<dyn Error>> {
        Ok(((), None))
    }
    fn expand_suffix(
        &self,
        _: &(),
        _: &(),
        shape: SuffixShape,
        mixture: MixtureDraw,
        seed: u64,
    ) -> Result<Vec<u8>, Box<dyn Error>> {
        draw_suffix(
            shape,
            mixture.mixture,
            mixture.weight,
            seed,
            |_| Ok(None),
            |rng| {
                Ok(u8::try_from(
                    rng.below(std::num::NonZeroUsize::new(2).unwrap()),
                )?)
            },
        )
    }
    fn max_action_limit(&self) -> usize {
        64
    }
    fn longest_action_time(&self) -> u64 {
        1
    }
}
impl TargetExecution for Model {
    fn new_target(&self) -> Result<ModelTarget, String> {
        Ok(ModelTarget {
            state: State::default(),
            frames: 7,
        })
    }
    fn reset(&self, target: &mut ModelTarget) {
        target.state = State::default();
    }
    fn restore(&self, target: &mut ModelTarget, snapshot: &State) -> Result<(), Box<dyn Error>> {
        target.state = snapshot.clone();
        Ok(())
    }
    fn frames_clocked(&self, target: &ModelTarget) -> u64 {
        target.frames
    }
    fn apply_action(
        &self,
        target: &mut ModelTarget,
        action: &u8,
        _: &mut (),
    ) -> Result<(), Box<dyn Error>> {
        target.state.step(self.0, *action);
        target.frames += 1;
        Ok(())
    }
    fn snapshot(&self, target: &mut ModelTarget) -> Result<State, Box<dyn Error>> {
        Ok(target.state.clone())
    }
    fn action_time_fn(&self) -> fn(&u8) -> u64 {
        |_| 1
    }
    fn snapshot_memory_charge(_: &State) -> usize {
        1024 * 1024
    } // Deliberate pressure fixture.
}
impl Evaluation for Model {
    fn is_terminal(&self, target: &ModelTarget) -> bool {
        target.state.health == 0
    }
    fn is_run_terminal(&self, _: &(), _: &ModelTarget) -> Result<bool, Box<dyn Error>> {
        Ok(false)
    }
    fn current_key(&self, target: &ModelTarget) -> Result<Key, Box<dyn Error>> {
        Ok(target.state.key())
    }
    fn complete_candidate_key(&self, key: Key, _: &State) -> Result<Key, Box<dyn Error>> {
        Ok(key)
    }
    fn merge_milestones(&self, _: &mut (), _: ()) {}
    fn aggregate_milestones(_: &Self::Evidence) {}
    fn aggregate_progress(_: &Self::Evidence) {}
    fn merge_origin_evidence(&self, _: &mut Self::Evidence, _: &Self::ArchiveReport) {}
    fn merge_snapshot_root_evidence(
        &self,
        _: &mut Self::Evidence,
        _: &ModelTarget,
    ) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn merge_import_evidence(&self, _: &mut Self::Evidence, _: (), _: &Input<u8>) {}
    fn merge_action_evidence<F>(
        &self,
        evidence: &mut Self::Evidence,
        action: &CampaignActionResult<Self>,
        sequence: u64,
        _: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<Input<u8>, Box<dyn Error>>,
    {
        if action
            .candidate
            .as_ref()
            .is_some_and(|c| c.key.value >= 2 && c.key.health > 0)
        {
            evidence.get_or_insert(sequence);
        }
        Ok(())
    }
    fn source_entries<'a>(
        &self,
        source: &'a Self::ArchiveReport,
    ) -> &'a [ArchiveEntryReport<u8, Key, ()>] {
        source
    }
    fn resume_input(&self, source: &Self::ArchiveReport) -> Result<Input<u8>, Box<dyn Error>> {
        source
            .last()
            .map(|e| e.input.clone())
            .ok_or_else(|| "empty model archive".into())
    }
}

fn config(workers: u32, mixture: DrawMixture) -> CampaignConfig<Model> {
    CampaignConfig {
        campaign_seed: 947,
        workers,
        execution_budget: 800,
        action_limit: 64,
        host: "finite-world".into(),
        wall_budget: None,
        continue_after_victory: false,
        archive_entry_limit: 128,
        reservations_per_worker: 1,
        memory_budget_mib: Some(4),
        materialize_final_artifacts: true,
        run: (),
        suffix: SuffixShape::OneToSix,
        mixture,
        retention: RetentionPolicy::AdmitAlive,
        selector: SelectorPolicy::EnergyProgressCheapest(RetireThresholds {
            entry: 3,
            groups: vec![],
        }),
        victory_input_path: None,
    }
}

fn run(
    game: &Model,
    config: &CampaignConfig<Model>,
    buffering: ResultBuffering,
) -> (Vec<u8>, CampaignOutcome<Model>) {
    let mut stream = Vec::new();
    let outcome = run_campaign_checkpointed_with_options(
        game,
        config,
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
        CampaignExecutionOptions {
            frame_budget: None,
            result_buffering: buffering,
            slot_retention: SlotRetentionPolicy::ResourceGuardedProgress2,
            ..CampaignExecutionOptions::default()
        },
    )
    .unwrap();
    (stream, outcome)
}

#[test]
fn observed_word_transports_in_one_world_and_kills_in_the_other() {
    for adverse in [false, true] {
        let mut state = State::default();
        let mut archive = Archive::<u8, Key, (), State>::new(|_| 1);
        archive.slot_retention = SlotRetentionPolicy::ResourceGuardedProgress2;
        archive.enable_continuations(Some(ContinuationLearning::ScopedProgress));
        archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: vec![],
                    key: state.key(),
                    milestones: (),
                },
                state.clone(),
            )
            .unwrap();
        for action in [0, 1] {
            state.step(adverse, action);
        }
        assert_eq!((state.value, state.health, state.previous), (1, 1, None));
        archive
            .insert(
                Some(0),
                1,
                ArchiveCandidate {
                    suffix: vec![0, 1],
                    key: state.key(),
                    milestones: (),
                },
                state.clone(),
            )
            .unwrap();
        let trial = archive.pop_continuation().unwrap();
        assert_eq!((trial.parent, trial.donor, trial.leaf), (1, 0, 1));
        let mut repeated = state.clone();
        for action in &trial.actions {
            repeated.step(adverse, *action);
        }
        let mut successes = 0;
        let mut trials = 0;
        for seed in 0..512 {
            if energy_strategy(seed, 0, 255).unwrap() != EnergyStrategy::Splice {
                continue;
            }
            let word = Model(adverse)
                .expand_suffix(
                    &(),
                    &(),
                    SuffixShape::OneOrTwo,
                    MixtureDraw {
                        mixture: DrawMixture::AlphabetOnly,
                        weight: 128,
                        splice_weight: 0,
                    },
                    seed,
                )
                .unwrap();
            let mut fresh = state.clone();
            for action in word {
                fresh.step(adverse, action);
            }
            successes += usize::from(fresh.value == 2 && fresh.health == 1);
            trials += 1;
        }
        assert!(successes > 0 && successes < trials);
        eprintln!(
            "fixed seed-domain witness: adverse={adverse}, fresh_successes={successes}/{trials}, learned={:?}",
            (repeated.value, repeated.health)
        );
        assert_eq!(
            (repeated.value, repeated.health),
            if adverse { (1, 0) } else { (2, 1) }
        );
    }
}

#[test]
fn a_retained_cross_slot_progress_event_supplies_no_reuse_word() {
    let mut state = State {
        value: 3,
        ..State::default()
    };
    let mut archive = Archive::<u8, Key, (), State>::new(|_| 1);
    archive.enable_continuations(Some(ContinuationLearning::ScopedProgress));
    archive
        .insert(
            None,
            0,
            ArchiveCandidate {
                suffix: vec![],
                key: state.key(),
                milestones: (),
            },
            state.clone(),
        )
        .unwrap();
    for bit in [0, 1] {
        state.step(false, bit);
    }
    assert_eq!(state.value, 4);
    assert!(
        archive
            .insert(
                Some(0),
                1,
                ArchiveCandidate {
                    suffix: vec![0, 1],
                    key: state.key(),
                    milestones: (),
                },
                state
            )
            .unwrap()
            .is_some()
    );
    assert!(archive.pop_continuation().is_none());
}

#[test]
fn real_progress_trials_replay_with_workers_buffers_and_snapshot_pressure() {
    for adverse in [false, true] {
        for workers in [1, 4] {
            for mode in [
                DrawMixture::AlphabetScopedProgressReuse,
                DrawMixture::AlphabetScopedProgressFreshControl,
            ] {
                let game = Model(adverse);
                let cfg = config(workers, mode);
                let (bytes, outcome) = run(&game, &cfg, ResultBuffering::OnePerWorker);
                let (buffered, other) = run(&game, &cfg, ResultBuffering::TwoPerWorker);
                assert_eq!(bytes, buffered, "buffering changed the recorded process");
                assert_eq!(outcome, other);
                assert_eq!(
                    replay_campaign_checkpointed(&game, &bytes, None, None).unwrap(),
                    outcome
                );
                let records: Vec<serde_json::Value> = bytes
                    .split(|b| *b == b'\n')
                    .filter(|s| !s.is_empty())
                    .map(|s| serde_json::from_slice(s).unwrap())
                    .collect();
                let count = records
                    .iter()
                    .filter(|r| r["selector"]["path"] == "continuation")
                    .count();
                assert!(count > 0, "must exercise actual learned-word dispatch");
                assert!(
                    outcome.0.snapshot_evictions > 0,
                    "must exercise pressure: adverse={adverse}, workers={workers}, mode={mode:?}"
                );
                if mode.redraws_progress_word() {
                    let pos = records
                        .iter()
                        .position(|r| r["selector"]["path"] == "continuation")
                        .unwrap();
                    for oversized in [false, true] {
                        let mut corrupted = records.clone();
                        if oversized {
                            corrupted[pos]["splice"]["tail_postcard"] = serde_json::to_value(
                                postcard::to_allocvec(&vec![0_u8; 7]).unwrap(),
                            )
                            .unwrap();
                        } else {
                            let bytes = corrupted[pos]["splice"]["tail_postcard"]
                                .as_array_mut()
                                .unwrap();
                            bytes[1] = serde_json::json!(bytes[1].as_u64().unwrap() ^ 1);
                        }
                        let input = corrupted
                            .iter()
                            .map(|r| serde_json::to_string(r).unwrap() + "\n")
                            .collect::<String>();
                        let error =
                            replay_campaign_checkpointed(&game, input.as_bytes(), None, None)
                                .unwrap_err()
                                .to_string();
                        assert!(
                            error.contains(if oversized {
                                "six-action bound"
                            } else {
                                "fresh-control suffix diverged"
                            }),
                            "{error}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn first_actual_action_change_has_a_matched_parent_seed_and_learning_history() {
    let game = Model(false);
    let (control, _) = run(
        &game,
        &config(4, DrawMixture::AlphabetScopedProgressFreshControl),
        ResultBuffering::OnePerWorker,
    );
    let (candidate, _) = run(
        &game,
        &config(4, DrawMixture::AlphabetScopedProgressReuse),
        ResultBuffering::OnePerWorker,
    );
    for (a, b) in control
        .split(|b| *b == b'\n')
        .skip(1)
        .zip(candidate.split(|b| *b == b'\n').skip(1))
    {
        if a == b {
            continue;
        }
        let a: serde_json::Value = serde_json::from_slice(a).unwrap();
        let b: serde_json::Value = serde_json::from_slice(b).unwrap();
        assert_eq!(a["selector"]["path"], "continuation");
        for key in [
            "event",
            "sequence",
            "worker",
            "parent_id",
            "mutation_seed",
            "selector",
        ] {
            assert_eq!(a[key], b[key], "{key}");
        }
        for key in ["donor_id", "leaf_id"] {
            assert_eq!(a["splice"][key], b["splice"][key]);
        }
        assert_ne!(a["splice"]["tail_postcard"], b["splice"]["tail_postcard"]);
        return;
    }
    panic!("must actually change a trial's actions");
}
