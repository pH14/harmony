// SPDX-License-Identifier: AGPL-3.0-or-later
//! Generic sixteen-location resource fixture: no NES or game dependencies.
use super::*;
use crate::search::archive::{RetireThresholds, entries_by_suffix};
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
    #[serde(with = "entries_by_suffix")]
    entries: Vec<ArchiveEntryReport<TestAction, TestKey, ()>>,
}

struct TestGame;
impl CampaignTypes for TestGame {
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
    type DrawState = ();
    type DrawCheckpoint = ();
    type TableHeader = ();
}

impl Reporting for TestGame {
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
            entries: state.entries,
        }
    }
}

impl InputPolicy for TestGame {
    fn draw_state_memory_reserve_bytes(&self, _run: &Self::Run, _max_actions: usize) -> usize {
        0
    }
    fn draw_state_memory_bytes(&self, _state: &Self::DrawState) -> usize {
        0
    }
    fn policies(&self, _run: &Self::Run) -> GamePolicies {
        GamePolicies::new()
    }
    fn resolve_recorded(&self, _policies: &GamePolicies) -> Result<Self::Run, Box<dyn Error>> {
        Ok(())
    }
    fn initial_draw_state(
        &self,
        _run: &Self::Run,
        _origin: Option<(&str, &Self::ArchiveReport)>,
    ) -> Result<InitialDrawState<Self>, Box<dyn Error>> {
        Ok(((), None))
    }
    fn expand_suffix(
        &self,
        _run: &Self::Run,
        _state: &Self::DrawState,
        _shape: SuffixShape,
        _mixture: MixtureDraw,
        mutation_seed: u64,
    ) -> Result<Vec<Self::Action>, Box<dyn Error>> {
        Ok(vec![TestAction::new(mutation_seed as u8, 1)])
    }

    fn max_action_limit(&self) -> usize {
        64
    }

    fn longest_action_time(&self) -> u64 {
        1
    }
}

impl TargetExecution for TestGame {
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

impl Evaluation for TestGame {
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
fn continuations_and_count_selection_replay_under_snapshot_pressure() {
    for (workers, semantic) in [(1, false), (4, false), (4, true)] {
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
            memory_budget_mib: Some(12),
            materialize_final_artifacts: true,
            run: (),
            suffix: SuffixShape::OneOrTwo,
            mixture: DrawMixture::EnergySpliceContinuation { scale: 6 },
            retention: RetentionPolicy::AdmitAlive,
            selector: if semantic {
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
            &TestGame,
            &config,
            &CampaignOrigin::Genesis,
            &mut bytes,
            None,
        )
        .unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        if workers == 1 {
            let mut with_sidecar = Vec::new();
            let mut sidecar = Vec::new();
            let observed = run_campaign_checkpointed(
                &TestGame,
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
            replay_campaign_checkpointed(&TestGame, &bytes, None, None).unwrap();
        assert_eq!(live, replayed);
        assert_eq!(checkpoint, replay_checkpoint);
        let mut bounded_stream = Vec::new();
        let (bounded, bounded_checkpoint) = run_campaign_checkpointed_with_frame_budget(
            &TestGame,
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
        assert_eq!(
            replay_campaign_checkpointed(&TestGame, &bounded_stream, None, None).unwrap(),
            (bounded, bounded_checkpoint)
        );
        let tampered = text.replacen("energy_splice_continuation_v1:6", "energy_splice:6", 1);
        assert!(replay_campaign_checkpointed(&TestGame, tampered.as_bytes(), None, None).is_err());
        let mut lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
        let line = lines
            .iter_mut()
            .find(|line| line.contains("\"path\":\"continuation\""))
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
        value["splice"]["tail_postcard"] = serde_json::json!([1, 255, 120]);
        *line = serde_json::to_string(&value).unwrap();
        assert!(
            replay_campaign_checkpointed(&TestGame, lines.join("\n").as_bytes(), None, None)
                .is_err()
        );
    }
}
