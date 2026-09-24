// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::search::archive::{SelectorAccounting, entries_by_suffix};
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
    type Place = u8;
    type Progress = ();
    type Identity = ();

    fn place(self) -> Self::Place {
        self.0 % 16
    }

    fn progress(self) -> Self::Progress {}

    fn identity(self) -> Self::Identity {}

    fn capacity() -> usize {
        1
    }
    fn preferences() -> usize {
        1
    }

    fn preference_cmp(self, _preference: usize, other: Self) -> std::cmp::Ordering {
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

fn continuation_config(
    workers: u32,
    mixture: DrawMixture,
    budget_mib: usize,
) -> CampaignConfig<TestWorkload> {
    CampaignConfig {
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
        memory_budget_mib: Some(budget_mib),
        materialize_final_artifacts: true,
        run: (),
        suffix: SuffixShape::OneOrTwo,
        mixture,
        retention: RetentionPolicy::Unprobed,
        objective_witness_path: None,
    }
}

#[test]
fn continuations_replay_exactly_under_snapshot_pressure() {
    for (workers, mixture, budget_mib) in [
        (1, DrawMixture::EnergySplice { scale: 6 }, 12),
        (4, DrawMixture::EnergySplice { scale: 6 }, 12),
        (4, DrawMixture::Energy { scale: 6 }, 18),
        (4, DrawMixture::BiasedHalf, 12),
    ] {
        let config = continuation_config(workers, mixture, budget_mib);
        let mut bytes = Vec::new();
        let (live, checkpoint) = run_campaign_checkpointed(
            &TestWorkload,
            &config,
            &CampaignOrigin::Genesis,
            &mut bytes,
            None,
        )
        .unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        let continuations = text
            .lines()
            .filter(|line| line.contains("\"path\":\"continuation\""))
            .count();
        assert!(
            continuations > 0,
            "{workers} workers never dispatched a continuation"
        );
        assert!(
            live.snapshot_evictions > 0,
            "{workers} workers never met memory pressure"
        );
        let (replayed, replay_checkpoint) =
            replay_campaign_checkpointed(&TestWorkload, &bytes, None, None).unwrap();
        assert_eq!(live, replayed, "{workers} workers diverged on replay");
        assert_eq!(checkpoint, replay_checkpoint);
    }
}

#[test]
fn a_tampered_continuation_record_fails_its_replay() {
    let config = continuation_config(4, DrawMixture::EnergySplice { scale: 6 }, 12);
    let mut bytes = Vec::new();
    run_campaign_checkpointed(
        &TestWorkload,
        &config,
        &CampaignOrigin::Genesis,
        &mut bytes,
        None,
    )
    .unwrap();
    let text = std::str::from_utf8(&bytes).unwrap().to_owned();
    for (label, expected, tamper) in [
        (
            "tail",
            "disagrees with the replayed queue entry",
            (|value: &mut serde_json::Value| {
                value["splice"]["tail_postcard"] = serde_json::json!([1, 255, 120]);
            }) as fn(&mut serde_json::Value),
        ),
        (
            "path",
            "which is an ordinary job",
            |value: &mut serde_json::Value| {
                value["selector"]["path"] = serde_json::json!("tiers");
            },
        ),
        (
            "share",
            "disagrees with the rebuilt continuation share",
            |value: &mut serde_json::Value| {
                value["continuation_energy"] = serde_json::json!(3);
            },
        ),
    ] {
        let mut lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
        let line = lines
            .iter_mut()
            .find(|line| line.contains("\"path\":\"continuation\""))
            .expect("a continuation record");
        let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
        tamper(&mut value);
        *line = serde_json::to_string(&value).unwrap();
        let error =
            replay_campaign_checkpointed(&TestWorkload, lines.join("\n").as_bytes(), None, None)
                .expect_err(&format!("replay accepted a tampered {label}"))
                .to_string();
        assert!(
            error.contains(expected),
            "a tampered {label} failed for another reason: {error}"
        );
    }
}

#[test]
fn a_progress_line_reports_the_continuation_accounting() {
    let config = continuation_config(4, DrawMixture::EnergySplice { scale: 6 }, 12);
    let mut bytes = Vec::new();
    let mut progress = Vec::new();
    run_campaign_checkpointed(
        &TestWorkload,
        &config,
        &CampaignOrigin::Genesis,
        &mut bytes,
        Some(&mut progress),
    )
    .unwrap();
    let text = String::from_utf8(progress).unwrap();
    let last = text.lines().last().expect("a progress line");
    let record: CampaignProgressRecord<serde_json::Value> = serde_json::from_str(last).unwrap();
    let accounting = record.continuations;
    assert!(accounting.edges > 0);
    assert!(accounting.jobs > 0);
    assert!(accounting.execution_work > 0);
    assert!(accounting.reservations_taken > 0);
    assert!(accounting.reservations_taken <= accounting.reservations_drawn);
    assert!(accounting.landed <= accounting.jobs);
    assert!(accounting.replaced <= accounting.landed);
    assert!(accounting.opened_new_cell <= accounting.jobs);
    assert!(accounting.energy <= 256);
}

fn replay_error(lines: &[String]) -> String {
    replay_campaign_checkpointed(&TestWorkload, lines.join("\n").as_bytes(), None, None)
        .expect_err("replay accepted a tampered stream")
        .to_string()
}

fn with_header_field(lines: &[String], field: &str, value: &str) -> Vec<String> {
    let mut changed = lines.to_vec();
    let mut header: serde_json::Value = serde_json::from_str(&changed[0]).unwrap();
    header[field] = serde_json::json!(value);
    changed[0] = serde_json::to_string(&header).unwrap();
    changed
}

#[test]
fn replay_rejects_a_stream_whose_header_or_draw_state_was_changed() {
    let config = continuation_config(1, DrawMixture::EnergySplice { scale: 6 }, 12);
    let mut bytes = Vec::new();
    let live = run_campaign_checkpointed(
        &TestWorkload,
        &config,
        &CampaignOrigin::Genesis,
        &mut bytes,
        None,
    )
    .unwrap();
    assert_eq!(
        replay_campaign_checkpointed(&TestWorkload, &bytes, None, None).unwrap(),
        live
    );
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(!text.contains("workload_diagnostics"));
    let lines = text.lines().map(str::to_owned).collect::<Vec<_>>();

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
    assert_eq!(observed, live);
    let final_point: serde_json::Value = serde_json::from_str(
        std::str::from_utf8(&sidecar)
            .unwrap()
            .lines()
            .last()
            .expect("a progress line"),
    )
    .unwrap();
    assert_eq!(final_point["workload_diagnostics"]["observed"], 42);

    for (field, expected) in [
        (
            "action_cost_unit",
            "campaign replay action-cost unit does not match the recorded stream",
        ),
        (
            "execution_work_unit",
            "campaign replay execution-work unit does not match the recorded stream",
        ),
    ] {
        let error = replay_error(&with_header_field(&lines, field, "wrong-unit"));
        assert_eq!(error, expected, "a changed {field}");
    }

    for event in ["job", "skip"] {
        let mut changed = lines.clone();
        let line = changed
            .iter_mut()
            .find(|line| {
                line.contains(&format!("\"event\":\"{event}\""))
                    && line.contains("\"draw_checkpoint_after\":{")
            })
            .unwrap_or_else(|| panic!("the stream records a {event} with a draw checkpoint"));
        let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
        value["draw_checkpoint_after"]["table_sha256"] = serde_json::json!("0".repeat(64));
        *line = serde_json::to_string(&value).unwrap();
        let error = replay_error(&changed);
        assert!(
            error.starts_with(&format!("replayed {event} "))
                && error.ends_with("draw-table checkpoint diverged"),
            "a changed {event} checkpoint failed for another reason: {error}"
        );
    }

    let recorded = draw_mixture_identifier(config.mixture);
    assert!(text.contains(&format!("\"mixture_policy\":\"{recorded}\"")));
    let error = replay_error(&with_header_field(
        &lines,
        "mixture_policy",
        "alphabet_only",
    ));
    assert!(
        error.contains("result digest") && error.contains("diverged"),
        "a swapped mixture failed for another reason: {error}"
    );
    let error = replay_error(&with_header_field(
        &lines,
        "mixture_policy",
        "unknown_mixture",
    ));
    assert_eq!(error, "draw mixture unknown_mixture is not recognized");
}

#[test]
fn history_growth_before_maintenance_does_not_stop_the_campaign() {
    for (workers, campaign_seed) in [(1, 947), (2, 947), (2, 11), (2, 12), (4, 947)] {
        let config = CampaignConfig {
            campaign_seed,
            ..continuation_config(workers, DrawMixture::EnergySplice { scale: 6 }, 8)
        };
        let mut stream = Vec::new();
        let live = run_campaign_checkpointed(
            &TestWorkload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .unwrap();
        assert_eq!(live.0.executions_completed, config.execution_budget);
        assert!(live.0.history_compactions > 0);
        assert!(live.0.snapshot_evictions > 0);
        assert_eq!(
            replay_campaign_checkpointed(&TestWorkload, &stream, None, None).unwrap(),
            live,
            "workers {workers} seed {campaign_seed}"
        );
    }
}

#[test]
fn a_budget_that_fits_the_bootstrap_state_finishes_the_campaign() {
    for (workers, campaign_seed) in [(1, 2), (1, 3), (2, 0), (2, 1), (3, 1), (4, 1)] {
        let config = CampaignConfig {
            campaign_seed,
            workers,
            execution_budget: 800,
            action_limit: 64,
            host: "test".into(),
            wall_budget: None,
            stop_rollout_on_objective: true,
            stop_campaign_on_objective: true,
            archive_entry_limit: 128,
            reservations_per_worker: 2,
            memory_budget_mib: Some(7),
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
        let mut stream = Vec::new();
        let (live, checkpoint) = run_campaign_checkpointed(
            &TestWorkload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .unwrap_or_else(|error| panic!("workers {workers} seed {campaign_seed}: {error}"));
        assert_eq!(live.executions_completed, config.execution_budget);
        assert_eq!(
            replay_campaign_checkpointed(&TestWorkload, &stream, None, None).unwrap(),
            (live, checkpoint)
        );
    }
}
