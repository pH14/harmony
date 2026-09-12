// SPDX-License-Identifier: AGPL-3.0-or-later
//! Generic sixteen-location resource fixture: no NES or game dependencies.
use super::*;
use crate::search::archive::{RetireThresholds, SelectorAccounting, entries_by_suffix};
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
    fn retention_resources(self) -> Option<[u64; 2]> {
        let level = u64::from(self.0 / 16);
        Some([level, 15 - level])
    }
    fn retention_context(self) -> Option<u64> {
        Some(u64::from(self.0 / 16) % 3)
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

struct TestGame;
impl CampaignTypes for TestGame {
    type Target = TestTarget;
    type Action = TestAction;
    type Key = TestKey;
    type Milestones = ();
    type Progress = ();
    type Snapshot = u8;
    type Observations = ();
    type Evidence = Option<u64>;
    type ArchiveReport = TestArchiveReport;
    type Run = ();
    type DrawState = ();
    type DrawCheckpoint = ();
    type TableHeader = ();
}

impl Reporting for TestGame {
    fn diagnostics(_: &Self::Evidence) -> Option<serde_json::Value> {
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
        // Constructor work must be visible to lifetime accounting without
        // entering snapshots or the campaign's historical admission clock.
        Ok(TestTarget {
            frames: 7,
            ..TestTarget::default()
        })
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
    fn named_milestone(&self, name: &str) -> Option<(&'static str, &'static str)> {
        match name {
            "zero_endpoint" => Some(("zero_endpoint", "test-endpoint-v1")),
            "never" => Some(("never", "test-endpoint-v1")),
            _ => None,
        }
    }

    fn milestone_first_execution(&self, evidence: &Self::Evidence, name: &str) -> Option<u64> {
        (name == "zero_endpoint").then_some(*evidence).flatten()
    }

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
        evidence: &mut Self::Evidence,
        target: &Self::Target,
    ) -> Result<(), Box<dyn Error>> {
        if target.value == 0 {
            evidence.get_or_insert(0);
        }
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
        evidence: &mut Self::Evidence,
        action: &CampaignActionResult<Self>,
        sequence: u64,
        _input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<Input<Self::Action>, Box<dyn Error>>,
    {
        if action
            .candidate
            .as_ref()
            .is_some_and(|candidate| candidate.key.0 == 0)
        {
            evidence.get_or_insert(sequence);
        }
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

fn milestone_config(workers: u32) -> CampaignConfig<TestGame> {
    CampaignConfig {
        campaign_seed: 947,
        workers,
        execution_budget: 2_000,
        action_limit: 64,
        host: "milestone-test".into(),
        wall_budget: None,
        continue_after_victory: false,
        archive_entry_limit: 128,
        reservations_per_worker: 2,
        memory_budget_mib: Some(12),
        materialize_final_artifacts: true,
        run: (),
        suffix: SuffixShape::OneOrTwo,
        mixture: DrawMixture::AlphabetOnly,
        retention: RetentionPolicy::AdmitAlive,
        selector: SelectorPolicy::EnergyProgressNoCost(RetireThresholds {
            entry: 3,
            groups: vec![],
        }),
        victory_input_path: None,
    }
}

#[test]
fn lifetime_meter_preserves_campaign_bytes_and_closes_search_and_replay_costs() {
    for workers in [1, 4] {
        let mut config = milestone_config(workers);
        config.execution_budget = 60;
        let options = CampaignExecutionOptions {
            frame_budget: Some(40),
            ..Default::default()
        };
        let mut ordinary = Vec::new();
        let baseline = run_campaign_checkpointed_with_options(
            &TestGame,
            &config,
            &CampaignOrigin::Genesis,
            &mut ordinary,
            None,
            options,
        )
        .unwrap();
        for result_buffering in [ResultBuffering::OnePerWorker, ResultBuffering::TwoPerWorker] {
            let meter = PhysicalWorkMeter::default();
            let mut measured = Vec::new();
            let outcome = run_campaign_checkpointed_measured(
                &TestGame,
                &config,
                &CampaignOrigin::Genesis,
                &mut measured,
                None,
                CampaignExecutionOptions {
                    result_buffering,
                    ..options
                },
                &meter,
            )
            .unwrap();
            assert_eq!(ordinary, measured, "meter altered deterministic bytes");
            assert_eq!(
                serde_json::to_value(&baseline.0).unwrap(),
                serde_json::to_value(&outcome.0).unwrap()
            );
            assert_eq!(baseline.1, outcome.1);
            let receipt = meter.receipt();
            assert_eq!(receipt.targets_created, u64::from(workers) + 1);
            assert_eq!(receipt.constructor_frames, 7 * (u64::from(workers) + 1));
            assert_eq!(
                receipt.complete_frames(),
                Some(outcome.0.frames_emulated + receipt.constructor_frames)
            );
            let replay_meter = PhysicalWorkMeter::default();
            let replay = replay_campaign_checkpointed_measured(
                &TestGame,
                &measured,
                None,
                None,
                &replay_meter,
            )
            .unwrap();
            assert_eq!(
                serde_json::to_value(&outcome.0).unwrap(),
                serde_json::to_value(&replay.0).unwrap()
            );
            assert_eq!(outcome.1, replay.1);
            let receipt = replay_meter.receipt();
            assert_eq!(receipt.targets_created, 1);
            assert_eq!(receipt.constructor_frames, 7);
            assert_eq!(
                receipt.complete_frames(),
                Some(replay.0.frames_emulated + 7)
            );
        }
    }
}

#[test]
fn lifetime_meter_counts_work_when_output_fails_before_any_job_is_recorded() {
    struct HeaderOnlyWriter(usize);
    impl Write for HeaderOnlyWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0 == 0 {
                return Err(std::io::Error::other("planted stream failure"));
            }
            self.0 -= 1;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let config = milestone_config(4);
    let meter = PhysicalWorkMeter::default();
    let result = run_campaign_checkpointed_measured(
        &TestGame,
        &config,
        &CampaignOrigin::Genesis,
        &mut HeaderOnlyWriter(2),
        None,
        CampaignExecutionOptions::default(),
        &meter,
    );
    assert!(result.is_err());
    let receipt = meter.receipt();
    assert_eq!(receipt.targets_created, 5);
    assert_eq!(receipt.targets_closed, 5);
    assert_eq!(receipt.constructor_frames, 35);
    assert!(
        receipt.complete_frames().unwrap() > 35,
        "unrecorded physical job work vanished"
    );
}

#[test]
fn lifetime_meter_does_not_rewind_with_snapshots_and_live_receipts_are_incomplete() {
    use crate::search::physical_work::TrackedTarget;
    let meter = PhysicalWorkMeter::default();
    let mut target = TrackedTarget::new(&TestGame, Some(&meter)).unwrap();
    assert_eq!(meter.receipt().complete_frames(), None);
    target.apply(&TestAction::new(1, 5));
    let snapshot = target.snapshot().unwrap();
    target.apply(&TestAction::new(2, 3));
    TestGame.restore(&mut target, &snapshot).unwrap();
    TestGame.reset(&mut target);
    target.apply(&TestAction::new(3, 2));
    drop(target);
    let receipt = meter.receipt();
    assert_eq!(receipt.constructor_frames, 7);
    assert_eq!(receipt.closed_post_constructor_frames, 10);
    assert_eq!(receipt.complete_frames(), Some(17));
}

#[test]
fn lifetime_meter_rejects_a_regressed_or_saturated_target_clock() {
    use crate::search::physical_work::TrackedTarget;
    for clock in [6, u64::MAX] {
        let meter = PhysicalWorkMeter::default();
        let mut target = TrackedTarget::new(&TestGame, Some(&meter)).unwrap();
        target.frames = clock;
        drop(target);
        let receipt = meter.receipt();
        assert_eq!(receipt.targets_created, receipt.targets_closed);
        assert!(receipt.invalid_counter);
        assert_eq!(receipt.complete_frames(), None);
    }
}

#[test]
fn milestone_stops_preserve_prefix_order_cost_and_full_replay() {
    for workers in [1, 4] {
        let config = milestone_config(workers);
        let mut baseline = Vec::new();
        let (ordinary, _) = run_campaign_checkpointed(
            &TestGame,
            &config,
            &CampaignOrigin::Genesis,
            &mut baseline,
            None,
        )
        .unwrap();
        let ordinary_json = serde_json::to_value(&ordinary).unwrap();
        assert!(ordinary_json.get("milestone_stop").is_none());
        assert!(ordinary_json.get("first_milestone").is_none());
        let baseline_lines = std::str::from_utf8(&baseline)
            .unwrap()
            .lines()
            .collect::<Vec<_>>();
        let mut previous = None;
        for buffering in [ResultBuffering::OnePerWorker, ResultBuffering::TwoPerWorker] {
            let mut bytes = Vec::new();
            let outcome = run_campaign_checkpointed_with_options(
                &TestGame,
                &config,
                &CampaignOrigin::Genesis,
                &mut bytes,
                None,
                CampaignExecutionOptions {
                    stop_after_milestone: Some("zero_endpoint"),
                    result_buffering: buffering,
                    ..CampaignExecutionOptions::default()
                },
            )
            .unwrap();
            let first = outcome.0.first_milestone.expect("fixture reaches zero");
            assert!(first.execution > 0 && first.execution < config.execution_budget);
            assert!(outcome.0.executions_completed < first.execution + u64::from(workers) * 2);
            assert!(outcome.0.frames_emulated >= first.frames_emulated);
            let lines = std::str::from_utf8(&bytes)
                .unwrap()
                .lines()
                .collect::<Vec<_>>();
            let mut event_line = None;
            let mut charged = outcome.0.bootstrap_frames;
            for (index, line) in lines.iter().enumerate().skip(1) {
                if let CampaignStreamRecord::Job(job) = serde_json::from_str(line).unwrap() {
                    charged += job.frames;
                    if job.sequence == first.execution {
                        event_line = Some(index);
                        break;
                    }
                }
            }
            let event_line = event_line.unwrap();
            assert_eq!(
                charged, first.frames_emulated,
                "first cost excludes later drain"
            );
            assert_eq!(
                &lines[1..=event_line],
                &baseline_lines[1..=event_line],
                "requested endpoint changed pre-event decisions"
            );
            assert_eq!(
                replay_campaign_checkpointed(&TestGame, &bytes, None, None).unwrap(),
                outcome
            );
            if let Some((old_bytes, old_outcome)) =
                previous.replace((bytes.clone(), outcome.clone()))
            {
                assert_eq!(
                    old_bytes, bytes,
                    "physical buffering changed the stop boundary"
                );
                assert_eq!(old_outcome, outcome);
            }

            // The endpoint job was already reserved when this smaller frame
            // cap was crossed. Its event remains observable but exceeds budget.
            let mut capped = Vec::new();
            let capped_outcome = run_campaign_checkpointed_with_options(
                &TestGame,
                &config,
                &CampaignOrigin::Genesis,
                &mut capped,
                None,
                CampaignExecutionOptions {
                    frame_budget: Some(first.frames_emulated - 1),
                    stop_after_milestone: Some("zero_endpoint"),
                    result_buffering: buffering,
                    ..CampaignExecutionOptions::default()
                },
            )
            .unwrap();
            assert_eq!(capped_outcome.0.first_milestone, Some(first));
            assert!(first.frames_emulated > capped_outcome.0.frame_budget.unwrap());
            assert_eq!(
                replay_campaign_checkpointed(&TestGame, &capped, None, None).unwrap(),
                capped_outcome
            );
        }

        // A longer unrestricted stream cannot be relabelled as a milestone stop.
        let mut header: serde_json::Value = serde_json::from_str(baseline_lines[0]).unwrap();
        header["milestone_stop"] = serde_json::json!({
            "name":"zero_endpoint", "observation_policy":"test-endpoint-v1"
        });
        let forged = std::iter::once(header.to_string())
            .chain(baseline_lines.iter().skip(1).map(|line| (*line).to_owned()))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let error =
            replay_campaign_checkpointed(&TestGame, forged.as_bytes(), None, None).unwrap_err();
        assert!(error.to_string().contains("milestone stop"), "{error}");
    }
}

#[test]
fn an_observed_snapshot_root_stops_before_reserving_any_job() {
    let config = milestone_config(4);
    let snapshots = SnapshotCheckpoint {
        format: TestGame.checkpoint_format().into(),
        entries: vec![SnapshotCheckpointEntry { id: 0, snapshot: 0 }],
    };
    let checkpoint = CampaignCheckpoint {
        path: "observed-zero-origin".into(),
        file_sha256: format!("{:x}", Sha256::digest(snapshots.to_bytes().unwrap())),
        snapshots,
    };
    let origin = CampaignOrigin::SnapshotRoot {
        checkpoint: checkpoint.clone(),
    };
    let mut stream = Vec::new();
    let outcome = run_campaign_checkpointed_with_options(
        &TestGame,
        &config,
        &origin,
        &mut stream,
        None,
        CampaignExecutionOptions {
            stop_after_milestone: Some("zero_endpoint"),
            ..CampaignExecutionOptions::default()
        },
    )
    .unwrap();
    assert_eq!(outcome.0.executions_completed, 0);
    assert_eq!(
        outcome.0.first_milestone,
        Some(CampaignMilestoneObservation {
            execution: 0,
            frames_emulated: 0,
        })
    );
    assert_eq!(std::str::from_utf8(&stream).unwrap().lines().count(), 1);
    assert_eq!(
        replay_campaign_checkpointed(&TestGame, &stream, None, Some(&checkpoint)).unwrap(),
        outcome
    );
}

#[test]
fn unobserved_milestones_keep_censoring_and_reject_unknown_meanings() {
    let config = milestone_config(4);
    let mut bytes = Vec::new();
    let outcome = run_campaign_checkpointed_with_options(
        &TestGame,
        &config,
        &CampaignOrigin::Genesis,
        &mut bytes,
        None,
        CampaignExecutionOptions {
            frame_budget: Some(128),
            stop_after_milestone: Some("never"),
            ..CampaignExecutionOptions::default()
        },
    )
    .unwrap();
    assert!(outcome.0.frames_emulated >= 128);
    assert!(outcome.0.first_milestone.is_none());
    assert_eq!(
        replay_campaign_checkpointed(&TestGame, &bytes, None, None).unwrap(),
        outcome
    );
    let text = std::str::from_utf8(&bytes).unwrap();
    let unknown_version = text.replacen("test-endpoint-v1", "test-endpoint-v2", 1);
    assert!(
        replay_campaign_checkpointed(&TestGame, unknown_version.as_bytes(), None, None).is_err()
    );
    let mut invalid = Vec::new();
    assert!(
        run_campaign_checkpointed_with_options(
            &TestGame,
            &config,
            &CampaignOrigin::Genesis,
            &mut invalid,
            None,
            CampaignExecutionOptions {
                stop_after_milestone: Some("unknown"),
                ..CampaignExecutionOptions::default()
            },
        )
        .is_err()
    );
    assert!(
        invalid.is_empty(),
        "unknown criterion wrote a campaign header"
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
        (1, true, false, 3),
        (4, true, false, 3),
        (1, true, false, 4),
        (4, true, false, 4),
        (1, true, false, 5),
        (4, true, false, 5),
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
            selector: if mode == 4 {
                SelectorPolicy::EnergyProgressCheapestScopedReturnControl(RetireThresholds {
                    entry: 3,
                    groups: vec![],
                })
            } else if mode == 5 {
                SelectorPolicy::EnergyProgressCheapestScopedReturnHalf(RetireThresholds {
                    entry: 3,
                    groups: vec![],
                })
            } else if mode == 3 {
                SelectorPolicy::EnergyProgressNoCost(RetireThresholds {
                    entry: 3,
                    groups: vec![],
                })
            } else if persistent {
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
            &TestGame,
            &config,
            &CampaignOrigin::Genesis,
            &mut bytes,
            None,
        )
        .unwrap();
        let mut buffered_bytes = Vec::new();
        let buffered = run_campaign_checkpointed_with_options(
            &TestGame,
            &config,
            &CampaignOrigin::Genesis,
            &mut buffered_bytes,
            None,
            CampaignExecutionOptions {
                slot_retention: Default::default(),
                frame_budget: None,
                result_buffering: ResultBuffering::TwoPerWorker,
                ..CampaignExecutionOptions::default()
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
        let mut bounded_buffered_bytes = Vec::new();
        let bounded_buffered = run_campaign_checkpointed_with_options(
            &TestGame,
            &config,
            &CampaignOrigin::Genesis,
            &mut bounded_buffered_bytes,
            None,
            CampaignExecutionOptions {
                slot_retention: Default::default(),
                frame_budget: Some(128),
                result_buffering: ResultBuffering::TwoPerWorker,
                ..CampaignExecutionOptions::default()
            },
        )
        .unwrap();
        assert_eq!(bounded_buffered_bytes, bounded_stream);
        assert_eq!(
            bounded_buffered,
            (bounded.clone(), bounded_checkpoint.clone())
        );
        assert_eq!(
            replay_campaign_checkpointed(&TestGame, &bounded_stream, None, None).unwrap(),
            (bounded, bounded_checkpoint)
        );
        let tampered = text.replacen(&draw_mixture_identifier(config.mixture), "alphabet_only", 1);
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

#[test]
fn optional_slot_policies_replay_active_and_unsupported_paths() {
    use crate::search::archive::SlotRetentionPolicy;
    for policy in [
        SlotRetentionPolicy::ResourceExtremes2,
        SlotRetentionPolicy::ResourceCoverage2,
        SlotRetentionPolicy::RepresentativeJobSample2,
        SlotRetentionPolicy::QualityRepresentatives2,
        SlotRetentionPolicy::ContextRepresentatives2,
        SlotRetentionPolicy::ResourceGuardedProgress2,
        SlotRetentionPolicy::ResourceGuardedProgressQuality2,
    ] {
        let config = CampaignConfig {
            campaign_seed: 947,
            workers: 4,
            execution_budget: 1200,
            action_limit: 64,
            host: "resource-test".into(),
            wall_budget: None,
            continue_after_victory: false,
            archive_entry_limit: 128,
            reservations_per_worker: 2,
            memory_budget_mib: Some(12),
            materialize_final_artifacts: true,
            run: (),
            suffix: SuffixShape::OneOrTwo,
            mixture: DrawMixture::AlphabetContinuation,
            retention: RetentionPolicy::AdmitAlive,
            selector: SelectorPolicy::EnergyFrontierCheapestCount(RetireThresholds {
                entry: 3,
                groups: vec![],
            }),
            victory_input_path: None,
        };
        let mut stream = Vec::new();
        let mut sidecar = Vec::new();
        let (live, checkpoint) = run_campaign_checkpointed_with_options(
            &TestGame,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            Some(&mut sidecar),
            CampaignExecutionOptions {
                frame_budget: None,
                result_buffering: ResultBuffering::TwoPerWorker,
                slot_retention: policy,
                ..CampaignExecutionOptions::default()
            },
        )
        .unwrap();
        assert_eq!(live.slot_retention.as_deref(), policy.identifier());
        assert!(live.snapshot_evictions > 0, "must exercise pressure");
        assert!(
            std::str::from_utf8(&stream)
                .unwrap()
                .lines()
                .any(|line| line.contains("\"path\":\"continuation\"")),
            "must exercise continuation dispatch"
        );
        let last: CampaignProgressRecord<TestKey> = serde_json::from_slice(
            sidecar
                .split(|byte| *byte == b'\n')
                .rfind(|line| !line.is_empty())
                .unwrap(),
        )
        .unwrap();
        if matches!(
            policy,
            SlotRetentionPolicy::ResourceGuardedProgress2
                | SlotRetentionPolicy::ResourceGuardedProgressQuality2
        ) {
            assert_eq!(
                last.retention_diagnostics.alternative_admissions, 0,
                "this fixture supplies no scoped progress; do not invent an alternate"
            );
        } else {
            assert!(
                last.retention_diagnostics.alternative_admissions > 0,
                "must keep actual alternatives"
            );
        }
        assert!(
            last.retention_diagnostics.removed > 0,
            "must replace or evict entries"
        );
        let (replayed, replay_checkpoint) =
            replay_campaign_checkpointed(&TestGame, &stream, None, None).unwrap();
        assert_eq!(live, replayed);
        assert_eq!(checkpoint, replay_checkpoint);
        let corrupted = String::from_utf8(stream).unwrap().replacen(
            policy.identifier().unwrap(),
            "resource_policy_invalid",
            1,
        );
        assert!(replay_campaign_checkpointed(&TestGame, corrupted.as_bytes(), None, None).is_err());
    }
}
