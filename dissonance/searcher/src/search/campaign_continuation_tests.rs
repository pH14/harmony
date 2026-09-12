// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::search::archive::{RetireThresholds, SelectorAccounting, entries_by_suffix};
use std::collections::{BTreeSet, VecDeque};
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
struct TestAction {
    input: u8,
    hold_frames: u8,
}

impl TestAction {
    fn new(input: u8, hold_frames: u8) -> Self {
        Self {
            input,
            hold_frames: hold_frames.max(1),
        }
    }
}

fn test_action_time(action: &TestAction) -> u64 {
    u64::from(action.hold_frames)
}

fn test_draw_action(fingerprint: u32, mutation_seed: u64) -> TestAction {
    TestAction::new((mutation_seed as u8).wrapping_add(fingerprint as u8), 1)
}

const TEST_DRAW_VERSION_CAP: usize = 16;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct TestDrawCheckpoint {
    generation: u64,
    fingerprint: u32,
}

#[derive(Default)]
struct TestDrawState {
    generation: u64,
    fingerprint: u32,
    versions: VecDeque<TestDrawCheckpoint>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct TestKey(u8);

impl ArchiveKey for TestKey {
    type Group = u8;

    fn groups() -> usize {
        1
    }

    fn group(self, depth: usize) -> Self::Group {
        assert_eq!(depth, 0);
        self.0 % 16
    }

    fn slot_capacity() -> usize {
        1
    }
    fn preference_cmp(self, other: Self) -> std::cmp::Ordering {
        (self.0 / 16).cmp(&(other.0 / 16))
    }
    type Lineage = ();

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

#[derive(Default)]
struct TestTarget {
    value: u8,
    frames: u64,
}

impl TestTarget {
    fn apply(&mut self, action: &TestAction) {
        self.value = self.value.wrapping_add(action.input);
        self.frames = self.frames.saturating_add(u64::from(action.hold_frames));
    }

    fn snapshot(&self) -> Result<u8, Box<dyn Error>> {
        Ok(self.value)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct TestArchiveReport {
    selector: SelectorAccounting,
    #[serde(with = "entries_by_suffix")]
    entries: Vec<ArchiveEntryReport<TestAction, TestKey, ()>>,
}

struct TestWorkload;
impl CampaignTypes for TestWorkload {
    type Target = TestTarget;
    type Action = TestAction;
    type Key = TestKey;
    type Milestones = ();
    type Progress = ();
    type Snapshot = u8;
    type Observations = ();
    type Evidence = ();
    type ArchiveReport = TestArchiveReport;
    type Run = ();
    type DrawState = TestDrawState;
    type DrawCheckpoint = TestDrawCheckpoint;
    type DrawHeader = ();
}

impl Reporting for TestWorkload {
    fn diagnostics(_: &()) -> Option<serde_json::Value> {
        Some(serde_json::json!({"observed": 42}))
    }
    fn stream_format(&self) -> &'static str {
        "test-campaign-v1"
    }
    fn checkpoint_format(&self) -> &'static str {
        "test-checkpoint-v1"
    }
    fn image_sha256(&self) -> String {
        "test-image".to_owned()
    }
    fn result_sha256(&self, result: &CampaignJobResult<Self>) -> Result<String, Box<dyn Error>> {
        postcard_value_sha256(result)
    }

    fn archive_report(
        &self,
        _evidence: &Self::Evidence,
        state: ArchiveReportState<Self>,
    ) -> Self::ArchiveReport {
        TestArchiveReport {
            selector: state.selector,
            entries: state.entries,
        }
    }
}

impl InputPolicy for TestWorkload {
    fn draw_state_memory_reserve_bytes(&self, _run: &Self::Run, _max_actions: usize) -> usize {
        std::mem::size_of::<TestDrawState>()
            + TEST_DRAW_VERSION_CAP * std::mem::size_of::<TestDrawCheckpoint>()
    }
    fn draw_state_memory_bytes(&self, _state: &Self::DrawState) -> usize {
        std::mem::size_of::<TestDrawState>()
            + TEST_DRAW_VERSION_CAP * std::mem::size_of::<TestDrawCheckpoint>()
    }
    fn policies(&self, _run: &Self::Run) -> WorkloadPolicies {
        WorkloadPolicies::new()
    }
    fn resolve_recorded(&self, _policies: &WorkloadPolicies) -> Result<Self::Run, Box<dyn Error>> {
        Ok(())
    }
    fn initial_draw_state(
        &self,
        _run: &Self::Run,
        _origin: Option<(&str, &Self::ArchiveReport)>,
    ) -> Result<InitialDrawState<Self>, Box<dyn Error>> {
        let mut state = TestDrawState::default();
        state.versions.push_back(TestDrawCheckpoint {
            generation: 0,
            fingerprint: 0,
        });
        Ok((state, None))
    }
    fn expand_suffix(
        &self,
        _run: &Self::Run,
        state: &Self::DrawState,
        _shape: SuffixShape,
        _mixture: MixtureDraw,
        mutation_seed: u64,
    ) -> Result<Vec<Self::Action>, Box<dyn Error>> {
        Ok(vec![test_draw_action(state.fingerprint, mutation_seed)])
    }

    fn draw_checkpoint(
        &self,
        state: &Self::DrawState,
    ) -> Result<Option<Self::DrawCheckpoint>, Box<dyn Error>> {
        Ok(Some(TestDrawCheckpoint {
            generation: state.generation,
            fingerprint: state.fingerprint,
        }))
    }

    fn draw_checkpoint_version(&self, checkpoint: &Self::DrawCheckpoint) -> u64 {
        checkpoint.generation
    }

    fn expand_suffix_recorded(
        &self,
        run: &Self::Run,
        state: &Self::DrawState,
        shape: SuffixShape,
        mixture: MixtureDraw,
        before: Option<&Self::DrawCheckpoint>,
        mutation_seed: u64,
    ) -> Result<Vec<Self::Action>, Box<dyn Error>> {
        let Some(before) = before else {
            return Err("stateful fixture omitted its draw checkpoint".into());
        };
        if !state.versions.iter().any(|checkpoint| checkpoint == before) {
            return Err("recorded draw checkpoint does not match live state".into());
        }
        let _ = (run, shape, mixture);
        Ok(vec![test_draw_action(before.fingerprint, mutation_seed)])
    }

    fn finish_stream_record(
        &self,
        _run: &Self::Run,
        state: &mut Self::DrawState,
        retained: &[(usize, &[Self::Action])],
    ) -> Result<Option<Self::DrawCheckpoint>, Box<dyn Error>> {
        let contribution = retained
            .iter()
            .flat_map(|(_, actions)| actions.iter())
            .fold(0_u32, |fingerprint, action| {
                fingerprint.wrapping_add(u32::from(action.input))
            });
        state.generation = state.generation.saturating_add(1);
        state.fingerprint = state.fingerprint.wrapping_add(contribution);
        let checkpoint = self
            .draw_checkpoint(state)?
            .ok_or("stateful fixture omitted its draw checkpoint")?;
        if state.versions.len() == TEST_DRAW_VERSION_CAP {
            state.versions.pop_front();
        }
        state.versions.push_back(checkpoint.clone());
        Ok(Some(checkpoint))
    }

    fn remember_draw_version(
        &self,
        state: &mut Self::DrawState,
        required: &BTreeSet<u64>,
    ) -> Result<(), Box<dyn Error>> {
        state
            .versions
            .retain(|checkpoint| required.contains(&checkpoint.generation));
        Ok(())
    }

    fn max_action_limit(&self) -> usize {
        64
    }

    fn longest_action_time(&self) -> u64 {
        1
    }
}

impl TargetExecution for TestWorkload {
    fn new_target(&self) -> Result<Self::Target, String> {
        Ok(TestTarget::default())
    }
    fn reset(&self, target: &mut Self::Target) {
        target.value = 0;
    }
    fn restore(
        &self,
        target: &mut Self::Target,
        snapshot: &Self::Snapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.value = *snapshot;

        Ok(())
    }
    fn frames_clocked(&self, target: &Self::Target) -> u64 {
        target.frames
    }
    fn apply_action(
        &self,
        target: &mut Self::Target,
        action: &Self::Action,
        _milestones: &mut Self::Milestones,
    ) -> Result<(), Box<dyn Error>> {
        target.apply(action);
        Ok(())
    }
    fn snapshot(&self, target: &mut Self::Target) -> Result<Self::Snapshot, Box<dyn Error>> {
        target.snapshot()
    }
    fn action_time_fn(&self) -> fn(&Self::Action) -> u64 {
        test_action_time
    }

    fn snapshot_memory_charge(_snapshot: &Self::Snapshot) -> usize {
        1024 * 1024
    }
}

impl Evaluation for TestWorkload {
    fn is_terminal(&self, _target: &Self::Target) -> bool {
        false
    }

    fn is_run_terminal(
        &self,
        _run: &Self::Run,
        _target: &Self::Target,
    ) -> Result<bool, Box<dyn Error>> {
        Ok(false)
    }

    fn current_key(&self, target: &Self::Target) -> Result<Self::Key, Box<dyn Error>> {
        Ok(TestKey(target.value))
    }

    fn complete_candidate_key(
        &self,
        key: Self::Key,
        _snapshot: &Self::Snapshot,
    ) -> Result<Self::Key, Box<dyn Error>> {
        Ok(key)
    }

    fn merge_milestones(&self, _into: &mut Self::Milestones, _from: Self::Milestones) {}
    fn aggregate_milestones(_evidence: &Self::Evidence) -> Self::Milestones {}
    fn aggregate_progress(_evidence: &Self::Evidence) -> Self::Progress {}
    fn merge_origin_evidence(&self, _evidence: &mut Self::Evidence, _source: &Self::ArchiveReport) {
    }
    fn merge_snapshot_root_evidence(
        &self,
        _evidence: &mut Self::Evidence,
        _target: &Self::Target,
    ) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn merge_import_evidence(
        &self,
        _evidence: &mut Self::Evidence,
        _milestones: Self::Milestones,
        _input: &Input<Self::Action>,
    ) {
    }
    fn merge_action_evidence<F>(
        &self,
        _evidence: &mut Self::Evidence,
        _action: &CampaignActionResult<Self>,
        _sequence: u64,
        _input: F,
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
            .iter()
            .max_by_key(|entry| (entry.key, entry.id))
            .map(|entry| entry.input.clone())
            .ok_or_else(|| "test fixture has no source archive".into())
    }
}
#[test]
fn isolated_continuation_admission_does_not_tune_the_next_ordinary_splice_draw() {
    let seed = (0..512)
        .find(|seed| energy_strategy(*seed, 0, 255).unwrap() == EnergyStrategy::Splice)
        .unwrap();
    let mut energy = MixtureEnergy::default();
    energy.record_outcome(EnergyStrategy::Splice, false);
    let before = energy.splice_weights(1);
    let isolated = DrawMixture::EnergySpliceContinuationIsolated { scale: 1 };
    for productive in [false, true] {
        record_mixture_outcome(
            &mut energy,
            isolated,
            SelectorPath::Continuation,
            seed,
            0,
            255,
            productive,
        )
        .unwrap();
        assert_eq!(energy.splice_weights(1), before);
    }
    let mut legacy = energy;
    record_mixture_outcome(
        &mut legacy,
        DrawMixture::EnergySpliceContinuation { scale: 1 },
        SelectorPath::Continuation,
        seed,
        0,
        255,
        false,
    )
    .unwrap();
    assert_ne!(
        legacy.splice_weights(1),
        before,
        "the legacy identifier keeps its old coupling"
    );
    record_mixture_outcome(
        &mut energy,
        isolated,
        SelectorPath::GroupWalk,
        seed,
        0,
        255,
        false,
    )
    .unwrap();
    assert_eq!(
        energy.splice_weights(1),
        legacy.splice_weights(1),
        "ordinary splice outcomes must still tune the mixture"
    );
}

#[test]
fn continuations_and_count_selection_replay_under_snapshot_pressure() {
    for (workers, semantic, persistent, mode) in [
        (1, false, false, 0),
        (4, false, false, 0),
        (4, true, false, 0),
        (4, false, true, 0),
        (1, false, false, 1),
        (4, false, false, 1),
        (4, false, true, 1),
        (1, false, false, 2),
        (4, false, false, 2),
    ] {
        let config = CampaignConfig {
            campaign_seed: 947,
            workers,
            execution_budget: 800,
            action_limit: 64,
            host: "test".into(),
            wall_budget: None,
            continue_after_victory: false,
            archive_entry_limit: 128,
            reservations_per_worker: 2,
            memory_budget_mib: Some(if persistent { 18 } else { 12 }),
            materialize_final_artifacts: true,
            run: (),
            suffix: SuffixShape::OneOrTwo,
            mixture: match mode {
                1 => DrawMixture::AlphabetContinuation,
                2 => DrawMixture::EnergySpliceContinuationIsolated { scale: 6 },
                _ => DrawMixture::EnergySpliceContinuation { scale: 6 },
            },
            retention: RetentionPolicy::AdmitAlive,
            selector: if persistent {
                SelectorPolicy::EnergyFrontierCheapestKeyCount(RetireThresholds {
                    entry: 3,
                    groups: vec![],
                })
            } else if semantic {
                SelectorPolicy::EnergyProgressCheapestCount(RetireThresholds {
                    entry: 3,
                    groups: vec![],
                })
            } else {
                SelectorPolicy::EnergyFrontierCheapestCount(RetireThresholds {
                    entry: 3,
                    groups: vec![],
                })
            },
            victory_input_path: None,
        };
        let mut bytes = Vec::new();
        let (live, checkpoint) = run_campaign_checkpointed(
            &TestWorkload,
            &config,
            &CampaignOrigin::Genesis,
            &mut bytes,
            None,
        )
        .unwrap();
        let mut buffered_bytes = Vec::new();
        let buffered = run_campaign_checkpointed_with_options(
            &TestWorkload,
            &config,
            &CampaignOrigin::Genesis,
            &mut buffered_bytes,
            None,
            CampaignExecutionOptions {
                frame_budget: None,
                result_buffering: ResultBuffering::TwoPerWorker,
            },
        )
        .unwrap();
        assert_eq!(
            buffered_bytes, bytes,
            "physical overlap changed search order"
        );
        assert_eq!(buffered, (live.clone(), checkpoint.clone()));
        let text = std::str::from_utf8(&bytes).unwrap();
        if workers == 1 {
            let mut with_sidecar = Vec::new();
            let mut sidecar = Vec::new();
            let observed = run_campaign_checkpointed(
                &TestWorkload,
                &config,
                &CampaignOrigin::Genesis,
                &mut with_sidecar,
                Some(&mut sidecar),
            )
            .unwrap();
            assert_eq!(with_sidecar, bytes);
            assert_eq!(observed, (live.clone(), checkpoint.clone()));
            let final_point: serde_json::Value = serde_json::from_str(
                std::str::from_utf8(&sidecar)
                    .unwrap()
                    .lines()
                    .last()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(final_point["workload_diagnostics"]["observed"], 42);
        }
        let continuation_count = text
            .lines()
            .filter(|line| line.contains("\"path\":\"continuation\""))
            .count();
        assert!(
            continuation_count > 0,
            "fixture must actually exercise continuation dispatch"
        );
        assert!(
            continuation_count <= 200,
            "continuations exceeded their quarter share"
        );
        assert!(
            live.snapshot_evictions > 0,
            "fixture must exercise memory pressure"
        );
        let (replayed, replay_checkpoint) =
            replay_campaign_checkpointed(&TestWorkload, &bytes, None, None).unwrap();
        assert_eq!(live, replayed);
        assert_eq!(checkpoint, replay_checkpoint);
        let mut corrupted = text.lines().map(str::to_owned).collect::<Vec<_>>();
        let checkpoint_line = corrupted
            .iter_mut()
            .find(|line| line.contains("\"draw_checkpoint_after\""))
            .expect("stateful fixture records a draw checkpoint");
        let mut checkpoint_value: serde_json::Value =
            serde_json::from_str(checkpoint_line).expect("checkpoint record parses");
        let fingerprint = checkpoint_value["draw_checkpoint_after"]["fingerprint"]
            .as_u64()
            .expect("checkpoint fingerprint is numeric");
        checkpoint_value["draw_checkpoint_after"]["fingerprint"] =
            serde_json::json!(fingerprint.saturating_add(1));
        *checkpoint_line = serde_json::to_string(&checkpoint_value).expect("checkpoint re-encodes");
        assert!(
            replay_campaign_checkpointed(
                &TestWorkload,
                corrupted.join("\n").as_bytes(),
                None,
                None
            )
            .is_err(),
            "replay accepted a corrupted typed draw checkpoint"
        );
        if persistent {
            let counts = live
                .archive
                .entries
                .iter()
                .map(|entry| entry.selector.map_or(0, |counters| counters.selected))
                .sum::<u64>();
            assert!(
                counts < live.executions_completed,
                "replacement must remove entry-local sampling history"
            );
            let history = live.archive.selector.key_counts.as_ref().unwrap();
            assert!(history.keys > 0 && history.hits > 0);
        }
        let mut bounded_stream = Vec::new();
        let (bounded, bounded_checkpoint) = run_campaign_checkpointed_with_frame_budget(
            &TestWorkload,
            &config,
            &CampaignOrigin::Genesis,
            &mut bounded_stream,
            None,
            Some(128),
        )
        .unwrap();
        assert_eq!(bounded.frame_budget, Some(128));
        assert!(bounded.frames_emulated >= 128);
        assert!(bounded.executions_completed < config.execution_budget);
        let mut bounded_buffered_bytes = Vec::new();
        let bounded_buffered = run_campaign_checkpointed_with_options(
            &TestWorkload,
            &config,
            &CampaignOrigin::Genesis,
            &mut bounded_buffered_bytes,
            None,
            CampaignExecutionOptions {
                frame_budget: Some(128),
                result_buffering: ResultBuffering::TwoPerWorker,
            },
        )
        .unwrap();
        assert_eq!(bounded_buffered_bytes, bounded_stream);
        assert_eq!(
            bounded_buffered,
            (bounded.clone(), bounded_checkpoint.clone())
        );
        assert_eq!(
            replay_campaign_checkpointed(&TestWorkload, &bounded_stream, None, None).unwrap(),
            (bounded, bounded_checkpoint)
        );
        let tampered = text.replacen(&draw_mixture_identifier(config.mixture), "alphabet_only", 1);
        assert!(
            replay_campaign_checkpointed(&TestWorkload, tampered.as_bytes(), None, None).is_err()
        );
        let mut lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
        let line = lines
            .iter_mut()
            .find(|line| line.contains("\"path\":\"continuation\""))
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
        value["splice"]["tail_postcard"] = serde_json::json!([1, 255, 120]);
        *line = serde_json::to_string(&value).unwrap();
        assert!(
            replay_campaign_checkpointed(&TestWorkload, lines.join("\n").as_bytes(), None, None)
                .is_err()
        );
    }
}
