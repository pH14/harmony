// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::search::archive::{RetireThresholds, SelectorAccounting, entries_by_suffix};
use crate::search::rollout::ExecutionDisposition;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
struct TestAction {
    input: u8,
    work_units: u8,
}

impl TestAction {
    fn new(input: u8, work_units: u8) -> Self {
        Self {
            input,
            work_units: work_units.max(1),
        }
    }
}

fn test_action_cost(action: &TestAction) -> u64 {
    u64::from(action.work_units).saturating_mul(2)
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

    fn route_context(self) -> Option<Self::Group> {
        Some(self.0 % 4)
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
    execution_work: u64,
}

impl TestTarget {
    fn apply(&mut self, action: &TestAction) {
        self.value = self.value.wrapping_add(action.input);
        self.execution_work = self
            .execution_work
            .saturating_add(u64::from(action.work_units));
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
    fn workload_identity_sha256(&self) -> String {
        "test-image".to_owned()
    }
    fn action_cost_unit(&self) -> &'static str {
        "test-cost"
    }
    fn execution_work_unit(&self) -> &'static str {
        "test-work"
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
    fn policies(&self, _run: &Self::Run) -> WorkloadPolicies {
        WorkloadPolicies::new()
    }
    fn resolve_recorded(&self, _policies: &WorkloadPolicies) -> Result<Self::Run, Box<dyn Error>> {
        Ok(())
    }
    fn sample_alphabet(
        &self,
        _run: &Self::Run,
        rand: &mut RomuDuoJrRand,
    ) -> Result<Self::Action, Box<dyn Error>> {
        Ok(TestAction::new(rand.next_u64() as u8, 1))
    }
    fn draw_table_parameters(&self, _run: &Self::Run) -> EmpiricalStepParameters {
        EmpiricalStepParameters {
            update_every_records: 1,
            hash_every_records: 1,
            ..DEFAULT_DRAW_TABLE_PARAMETERS
        }
    }

    fn max_action_limit(&self) -> usize {
        64
    }

    fn max_action_cost(&self) -> u64 {
        2
    }

    fn remember_draw_version(
        &self,
        state: &mut DrawTables<Self::Action>,
        schedule: &DrawVersionSchedule,
    ) -> Result<(), Box<dyn Error>> {
        state.remember_version(schedule)?;
        REPLAY_DRAW_VERSIONS.with_borrow_mut(|counts| counts.push(state.version_count()));
        Ok(())
    }
}

thread_local! {
    static REPLAY_DRAW_VERSIONS: std::cell::RefCell<Vec<usize>> =
        const { std::cell::RefCell::new(Vec::new()) };
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
    fn execution_work(&self, target: &Self::Target) -> u64 {
        target.execution_work
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
    fn action_cost_fn(&self) -> fn(&Self::Action) -> u64 {
        test_action_cost
    }

    fn snapshot_memory_charge(_snapshot: &Self::Snapshot) -> usize {
        1024 * 1024
    }
}

impl Evaluation for TestWorkload {
    fn execution_disposition(&self, _target: &Self::Target) -> ExecutionDisposition {
        ExecutionDisposition::Runnable
    }

    fn objective_reached(
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
        SelectorPath::HierarchyWalk,
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
    for (workers, persistent, mode) in [
        (1, false, 0),
        (4, false, 0),
        (4, true, 0),
        (1, false, 1),
        (4, false, 1),
        (4, true, 1),
        (1, false, 2),
        (4, false, 2),
        (4, false, 3),
        (4, false, 4),
    ] {
        let config = CampaignConfig {
            campaign_seed: 947,
            workers,
            execution_budget: 800,
            action_limit: 64,
            host: "test".into(),
            wall_budget: None,
            stop_rollout_on_objective: true,
            stop_campaign_on_objective: true,
            archive_entry_limit: 128,
            reservations_per_worker: 2,
            memory_budget_mib: Some(if persistent { 18 } else { 12 }),
            materialize_final_artifacts: true,
            run: (),
            suffix: SuffixShape::OneOrTwo,
            mixture: match mode {
                1 => DrawMixture::AlphabetContinuation,
                2 => DrawMixture::EnergySpliceContinuationIsolated { scale: 6 },
                3 => DrawMixture::AlphabetRouteReuse,
                4 => DrawMixture::AlphabetRouteReuseDeduplicated,
                _ => DrawMixture::EnergySpliceContinuation { scale: 6 },
            },
            retention: RetentionPolicy::Unprobed,
            selector: if persistent {
                SelectorPolicy::EnergyFrontierCheapestKeyCount(RetireThresholds {
                    entry: 3,
                    groups: vec![],
                })
            } else {
                SelectorPolicy::EnergyFrontierCheapestCount(RetireThresholds {
                    entry: 3,
                    groups: vec![],
                })
            },
            objective_witness_path: None,
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
                work_budget: None,
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
        if workers == 1 && !persistent && mode == 0 {
            for field in ["action_cost_unit", "execution_work_unit"] {
                let mut lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
                let mut header: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
                header[field] = serde_json::Value::String("wrong-unit".to_owned());
                lines[0] = serde_json::to_string(&header).unwrap();
                assert!(
                    replay_campaign_checkpointed(
                        &TestWorkload,
                        lines.join("\n").as_bytes(),
                        None,
                        None,
                    )
                    .is_err(),
                    "replay accepted a mismatched {field}"
                );
            }
        }
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
        let route_count = text
            .lines()
            .filter(|line| line.contains("\"tail_postcard\""))
            .count();
        if mode == 3 || mode == 4 {
            assert!(
                route_count > 0,
                "fixture must exercise route reuse dispatch"
            );
            let cross_group_route = text
                .lines()
                .filter_map(|line| {
                    let value: serde_json::Value = serde_json::from_str(line).ok()?;
                    (value.get("event")?.as_str()? == "job").then_some(())?;
                    let parent_id = value.get("parent_id")?.as_u64()?;
                    let donor_id = value.get("splice")?.get("donor_id")?.as_u64()?;
                    let parent = live
                        .archive
                        .entries
                        .iter()
                        .find(|entry| entry.id == parent_id)?;
                    let donor = live
                        .archive
                        .entries
                        .iter()
                        .find(|entry| entry.id == donor_id)?;
                    Some(parent.key.group(0) != donor.key.group(0))
                })
                .any(|different_group| different_group);
            assert!(
                cross_group_route,
                "route reuse must dispatch an observed tail across exact archive groups"
            );
            let warm_origin = CampaignOrigin::Archive {
                path: "warm-route-test.json".to_owned(),
                file_sha256: "0".repeat(64),
                report: Box::new(live.archive.clone()),
                checkpoint: None,
            };
            let mut warm_bytes = Vec::new();
            let (warm_live, _) = run_campaign_checkpointed(
                &TestWorkload,
                &config,
                &warm_origin,
                &mut warm_bytes,
                None,
            )
            .unwrap();
            let warm_text = std::str::from_utf8(&warm_bytes).unwrap();
            let warm_cross_group_route = warm_text
                .lines()
                .filter_map(|line| {
                    let value: serde_json::Value = serde_json::from_str(line).ok()?;
                    (value.get("event")?.as_str()? == "job").then_some(())?;
                    let parent_id = value.get("parent_id")?.as_u64()?;
                    let donor_id = value.get("splice")?.get("donor_id")?.as_u64()?;
                    let parent = warm_live
                        .archive
                        .entries
                        .iter()
                        .find(|entry| entry.id == parent_id)?;
                    let donor = warm_live
                        .archive
                        .entries
                        .iter()
                        .find(|entry| entry.id == donor_id)?;
                    Some(parent.key.group(0) != donor.key.group(0))
                })
                .any(|different_group| different_group);
            assert!(
                warm_cross_group_route,
                "warm route reuse must dispatch an observed tail across exact archive groups"
            );
            let repeated_route_count = text
                .lines()
                .filter(|line| line.contains("\"outcome\":\"repeated_route\""))
                .count();
            if mode == 4 {
                assert!(
                    repeated_route_count > 0,
                    "deduplicating route policy must suppress at least one repeated route"
                );
            }
            if mode == 3
                && let Some(index) = text
                    .lines()
                    .position(|line| line.contains("\"outcome\":\"unavailable\""))
            {
                let mut corrupted = text.lines().map(str::to_owned).collect::<Vec<_>>();
                corrupted[index] = corrupted[index].replace(
                    "\"outcome\":\"unavailable\"",
                    "\"outcome\":\"repeated_route\"",
                );
                assert!(
                    replay_campaign_checkpointed(
                        &TestWorkload,
                        corrupted.join("\n").as_bytes(),
                        None,
                        None,
                    )
                    .is_err(),
                    "old route policy accepted repeated-route evidence"
                );
            }
        } else {
            assert!(
                continuation_count > 0,
                "fixture must actually exercise continuation dispatch"
            );
        }
        let dispatch_count = text
            .lines()
            .filter(|line| line.contains("\"path\":"))
            .count();
        assert!(
            (continuation_count.max(route_count)) * 2 <= dispatch_count,
            "splice dispatch exceeded its reservation share: {} of {dispatch_count}",
            continuation_count.max(route_count)
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
        assert!(
            checkpoint_value["draw_checkpoint_after"]["table_sha256"].is_string(),
            "checkpoint carries its table hash"
        );
        checkpoint_value["draw_checkpoint_after"]["table_sha256"] =
            serde_json::json!("0".repeat(64));
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
        let (bounded, bounded_checkpoint) = run_campaign_checkpointed_with_options(
            &TestWorkload,
            &config,
            &CampaignOrigin::Genesis,
            &mut bounded_stream,
            None,
            CampaignExecutionOptions {
                work_budget: Some(128),
                result_buffering: ResultBuffering::OnePerWorker,
            },
        )
        .unwrap();
        assert_eq!(bounded.work_budget, Some(128));
        assert!(bounded.execution_work >= 128);
        assert!(bounded.executions_completed < config.execution_budget);
        let mut bounded_buffered_bytes = Vec::new();
        let bounded_buffered = run_campaign_checkpointed_with_options(
            &TestWorkload,
            &config,
            &CampaignOrigin::Genesis,
            &mut bounded_buffered_bytes,
            None,
            CampaignExecutionOptions {
                work_budget: Some(128),
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
        let tampered_identifier = if mode == 3 || mode == 4 {
            "not_a_mixture"
        } else {
            "alphabet_only"
        };
        let tampered = text.replacen(
            &draw_mixture_identifier(config.mixture),
            tampered_identifier,
            1,
        );
        assert!(
            replay_campaign_checkpointed(&TestWorkload, tampered.as_bytes(), None, None).is_err()
        );
        let mut lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
        let line = lines
            .iter_mut()
            .find(|line| {
                if mode == 3 || mode == 4 {
                    line.contains("\"event\":\"job\"") && line.contains("\"tail_postcard\"")
                } else {
                    line.contains("\"path\":\"continuation\"")
                }
            })
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
        value["splice"]["tail_postcard"] = serde_json::json!([1, 255, 120]);
        *line = serde_json::to_string(&value).unwrap();
        assert!(
            replay_campaign_checkpointed(&TestWorkload, lines.join("\n").as_bytes(), None, None)
                .is_err()
        );
        if mode == 3 || mode == 4 {
            let mut provenance_lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
            let provenance_line = provenance_lines
                .iter_mut()
                .find(|line| {
                    line.contains("\"event\":\"job\"") && line.contains("\"tail_postcard\"")
                })
                .unwrap();
            let mut provenance: serde_json::Value = serde_json::from_str(provenance_line).unwrap();
            provenance["splice"]["donor_id"] = serde_json::json!(u64::MAX);
            *provenance_line = serde_json::to_string(&provenance).unwrap();
            assert!(
                replay_campaign_checkpointed(
                    &TestWorkload,
                    provenance_lines.join("\n").as_bytes(),
                    None,
                    None,
                )
                .is_err(),
                "route splice provenance corruption was accepted"
            );
        }
    }
}

#[test]
fn a_live_progress_line_reports_the_selector_accounting() {
    let config = CampaignConfig {
        campaign_seed: 947,
        workers: 4,
        execution_budget: 800,
        action_limit: 64,
        host: "test".into(),
        wall_budget: None,
        stop_rollout_on_objective: false,
        stop_campaign_on_objective: false,
        archive_entry_limit: 128,
        reservations_per_worker: 2,
        memory_budget_mib: Some(18),
        materialize_final_artifacts: true,
        run: (),
        suffix: SuffixShape::OneOrTwo,
        mixture: DrawMixture::EnergySpliceContinuation { scale: 6 },
        retention: RetentionPolicy::Unprobed,
        selector: SelectorPolicy::EnergyFrontierCheapestCount(RetireThresholds {
            entry: 3,
            groups: vec![],
        }),
        objective_witness_path: None,
    };
    let mut bytes = Vec::new();
    let mut progress = Vec::new();
    let (outcome, _) = run_campaign_checkpointed(
        &TestWorkload,
        &config,
        &CampaignOrigin::Genesis,
        &mut bytes,
        Some(&mut progress),
    )
    .unwrap();
    let text = String::from_utf8(progress).unwrap();
    let last = text.lines().last().expect("a progress line was written");
    let record: CampaignProgressRecord<serde_json::Value> = serde_json::from_str(last).unwrap();
    assert_eq!(record.selector, outcome.archive.selector);
    assert!(record.selector.cell_selections > 0);
    assert!(!record.selector.class_draws_by_rank.is_empty());
}

#[test]
fn a_replay_holds_only_the_draw_table_versions_its_remaining_records_name() {
    let config = CampaignConfig {
        campaign_seed: 0x5eed_0f01,
        workers: 4,
        execution_budget: 600,
        action_limit: 64,
        host: "test".into(),
        wall_budget: None,
        stop_rollout_on_objective: false,
        stop_campaign_on_objective: false,
        archive_entry_limit: 128,
        reservations_per_worker: 2,
        memory_budget_mib: Some(12),
        materialize_final_artifacts: true,
        run: (),
        suffix: SuffixShape::OneOrTwo,
        mixture: DrawMixture::BiasedHalf,
        retention: RetentionPolicy::Unprobed,
        selector: SelectorPolicy::GroupUniform,
        objective_witness_path: None,
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
    let records = std::str::from_utf8(&bytes).unwrap().lines().count() - 1;
    REPLAY_DRAW_VERSIONS.with_borrow_mut(Vec::clear);
    let replayed = replay_campaign_checkpointed(&TestWorkload, &bytes, None, None).unwrap();
    assert_eq!(replayed, (live, checkpoint));
    let counts = REPLAY_DRAW_VERSIONS.with_borrow(Clone::clone);
    assert!(counts.len() > 1, "replay remembered no table version");
    let held = counts.iter().copied().max().unwrap_or(0);
    assert!(
        held * 4 < records,
        "replay held {held} table versions across {records} records"
    );
    assert!(
        counts.last().copied().unwrap_or(0) < held,
        "the version map never shrank: {counts:?}"
    );
}
