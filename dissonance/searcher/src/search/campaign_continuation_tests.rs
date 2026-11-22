// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::search::archive::{SelectorAccounting, entries_by_suffix};
use crate::search::choice::{CERTAIN, ChoiceProfile, ChoiceProfiles};
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
        let micros = if action.input == 0 {
            2_000
        } else {
            u64::from(action.input % 4) * 25
        };
        std::thread::sleep(std::time::Duration::from_micros(micros));
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

thread_local! {
    static TEST_EXECUTABLE_IDENTITY: std::cell::Cell<Option<Result<&'static str, &'static str>>> = const { std::cell::Cell::new(None) };
    static TEST_GENESIS: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    static TEST_CHOICE_POLICY: std::cell::Cell<Option<ChoicePolicy>> = const { std::cell::Cell::new(None) };
    static TEST_UNTRIED_SITES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

const TEST_ASK: u8 = 200;
const TEST_DEFAULT: u8 = 201;

fn test_offered(value: u8) -> (Vec<u16>, u16) {
    if value.is_multiple_of(2) {
        (vec![0, 1, 2, 3], u16::from(value / 2 % 4))
    } else {
        (vec![1, 3], 1 + 2 * u16::from(value / 2 % 2))
    }
}

struct ChoiceTestWorkload<const CHOICES: bool>;

type TestWorkload = ChoiceTestWorkload<false>;

#[allow(non_upper_case_globals)]
const TestWorkload: TestWorkload = ChoiceTestWorkload;
impl<const CHOICES: bool> CampaignTypes for ChoiceTestWorkload<CHOICES> {
    type JobObservation = ();
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

impl<const CHOICES: bool> Reporting for ChoiceTestWorkload<CHOICES> {
    fn diagnostics(_: &()) -> Option<serde_json::Value> {
        Some(serde_json::json!({"observed": 42}))
    }
    fn write_evidence_checkpoint(
        _evidence: &(),
        _out: &mut dyn std::io::Write,
    ) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn evidence_from_checkpoint(_bytes: &[u8]) -> Result<(), Box<dyn Error>> {
        Ok(())
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
    fn executable_identity(&self) -> Result<String, Box<dyn Error>> {
        if let Some(identity) = TEST_EXECUTABLE_IDENTITY.with(std::cell::Cell::get) {
            return Ok(identity?.to_owned());
        }
        Ok(crate::search::executable_identity::loaded_executable_identity_self()?)
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
    fn snapshot_identity_sha256(
        &self,
        snapshot: &Self::Snapshot,
    ) -> Result<String, Box<dyn Error>> {
        postcard_value_sha256(snapshot)
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

impl<const CHOICES: bool> InputPolicy for ChoiceTestWorkload<CHOICES> {
    fn policies(&self, _run: &Self::Run) -> WorkloadPolicies {
        WorkloadPolicies::from([(
            PREFERENCE_POLICY_FIELD.to_owned(),
            "place_high_nibble".to_owned(),
        )])
    }
    fn resolve_recorded(&self, _policies: &WorkloadPolicies) -> Result<Self::Run, Box<dyn Error>> {
        Ok(())
    }
    fn sample_alphabet(
        &self,
        _run: &Self::Run,
        _previous: Option<&Self::Action>,
        rand: &mut RomuDuoJrRand,
    ) -> Result<Self::Action, Box<dyn Error>> {
        let drawn = rand.next_u64() as u8;
        Ok(TestAction::new(if CHOICES { TEST_ASK } else { drawn }, 1))
    }
    fn choice_sites(&self, _run: &Self::Run) -> Vec<ChoiceSite> {
        if CHOICES {
            vec![ChoiceSite::new(
                "step",
                (1..=4).map(|label: u8| label.to_string()),
            )]
        } else {
            Vec::new()
        }
    }
    fn choice_policy(&self, _run: &Self::Run) -> ChoicePolicy {
        TEST_CHOICE_POLICY
            .with(std::cell::Cell::get)
            .unwrap_or_default()
    }
    fn untried_site(&self, _run: &Self::Run, suffix: &[Self::Action]) -> u16 {
        u16::from(TEST_UNTRIED_SITES.with(std::cell::Cell::get) && !suffix.len().is_multiple_of(2))
    }
    fn draw_table_parameters(&self, _run: &Self::Run) -> EmpiricalStepParameters {
        EmpiricalStepParameters {
            update_every_records: 1,
            hash_every_records: 1,
            ..DEFAULT_DRAW_TABLE_PARAMETERS
        }
    }
}

impl<const CHOICES: bool> TargetExecution for ChoiceTestWorkload<CHOICES> {
    fn new_target(&self) -> Result<Self::Target, String> {
        Ok(TestTarget::default())
    }
    fn reset(&self, target: &mut Self::Target) {
        target.value = TEST_GENESIS.with(std::cell::Cell::get);
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
        if CHOICES && action.input == TEST_DEFAULT {
            let (_, default) = test_offered(target.value);
            target.apply(&TestAction {
                input: u8::try_from(default + 1)?,
                ..*action
            });
        } else {
            target.apply(action);
        }
        Ok(())
    }
    fn resolve_action(
        &self,
        target: &Self::Target,
        action: &Self::Action,
        chooser: &mut Chooser,
    ) -> Result<Self::Action, Box<dyn Error>> {
        if action.input != TEST_ASK {
            return Ok(*action);
        }
        let (offered, default) = test_offered(target.value);
        let input = match chooser.choose(0, &offered, default) {
            Choice::Ask | Choice::Default => TEST_DEFAULT,
            Choice::Option(label) => u8::try_from(label + 1)?,
        };
        Ok(TestAction { input, ..*action })
    }
    fn action_choices(&self, action: &Self::Action) -> Vec<(u16, Choice)> {
        match action.input {
            TEST_DEFAULT if CHOICES => vec![(0, Choice::Default)],
            input @ 1..=4 if CHOICES => vec![(0, Choice::Option(u16::from(input - 1)))],
            _ => Vec::new(),
        }
    }
    fn snapshot(&self, target: &mut Self::Target) -> Result<Self::Snapshot, Box<dyn Error>> {
        target.snapshot()
    }
    fn planned_cost_fn(&self) -> fn(&Self::Action) -> u64 {
        test_action_cost
    }

    fn snapshot_memory_charge(_snapshot: &Self::Snapshot) -> usize {
        1024 * 1024
    }
}

impl<const CHOICES: bool> Evaluation for ChoiceTestWorkload<CHOICES> {
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
        host: "test".into(),
        wall_budget: None,
        stop_rollout_on_objective: true,
        stop_campaign_on_objective: true,
        archive_entry_limit: 128,
        window: 2 * workers as usize,
        memory_budget_mib: Some(budget_mib),
        materialize_final_artifacts: true,
        run: (),
        mixture,
        retention: RetentionPolicy::Unprobed,
        objective_witness_path: None,
        minimize: None,
    }
}

#[test]
fn replay_rejects_duplicate_skip_for_taken_continuation() {
    let mut config = continuation_config(1, DrawMixture::EnergySplice { scale: 6 }, 24);
    config.stop_campaign_on_objective = false;
    let mut stream = Vec::new();
    run_campaign_checkpointed(
        &TestWorkload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
    )
    .unwrap();
    let records = std::str::from_utf8(&stream)
        .unwrap()
        .lines()
        .skip(1)
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    replay_campaign_checkpointed(&TestWorkload, &stream, None, None).unwrap();
    let reservation = 18_u64;
    let job = records
        .iter()
        .find(|record| record["event"] == "job" && record["reservation"] == reservation)
        .unwrap();
    assert_eq!(job["selector"]["path"], "continuation");
    let mut skip = records
        .iter()
        .find(|record| record["event"] == "skip")
        .unwrap()
        .clone();
    skip["reservation"] = serde_json::json!(reservation);
    skip["draw_checkpoint_before"] = job["draw_checkpoint_before"].clone();
    let mut after = skip["draw_checkpoint_before"].clone();
    after["records"] = serde_json::json!(after["records"].as_u64().unwrap() + 1);
    skip["draw_checkpoint_after"] = after;
    let mut changed = records.clone();
    let insert_at = changed
        .iter()
        .enumerate()
        .filter(|(_, record)| record["event"] == "job")
        .nth(usize::try_from(reservation).unwrap() - config.window)
        .unwrap()
        .0
        + 1;
    changed.insert(insert_at, skip);
    for record in changed.iter_mut().skip(insert_at + 1) {
        if record["event"] != "job" && record["event"] != "skip" {
            continue;
        }
        if record["reservation"].as_u64().unwrap() >= reservation {
            let before = &mut record["draw_checkpoint_before"];
            before["records"] = serde_json::json!(before["records"].as_u64().unwrap() + 1);
        }
        let after = &mut record["draw_checkpoint_after"];
        if !after.is_null() {
            after["records"] = serde_json::json!(after["records"].as_u64().unwrap() + 1);
        }
    }
    let end = changed.last_mut().unwrap();
    assert_eq!(end["event"], "end");
    end["skips"] = serde_json::json!(end["skips"].as_u64().unwrap() + 1);
    let mut changed_stream = std::str::from_utf8(&stream)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_owned();
    changed_stream.push('\n');
    for record in &changed {
        changed_stream.push_str(&serde_json::to_string(record).unwrap());
        changed_stream.push('\n');
    }
    let error = replay_campaign_checkpointed(&TestWorkload, changed_stream.as_bytes(), None, None)
        .expect_err("a taken continuation cannot skip an ordinary duplicate");
    assert!(
        error
            .to_string()
            .contains("skip before a taken continuation"),
        "{error}"
    );
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
fn replay_rejects_a_splice_weight_impossible_for_the_recorded_energy_policy() {
    let config = continuation_config(1, DrawMixture::Energy { scale: 6 }, 18);
    let mut stream = Vec::new();
    run_campaign_checkpointed(
        &TestWorkload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
    )
    .unwrap();
    let mut lines = std::str::from_utf8(&stream)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let line = lines
        .iter_mut()
        .find(|line| {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            if value["event"] != "job" || value["selector"]["path"] == "continuation" {
                return false;
            }
            let seed = value["mutation_seed"].as_u64().unwrap();
            let weight = u8::try_from(value["mixture_weight"].as_u64().unwrap()).unwrap();
            energy_strategy(seed, weight, 0).unwrap() == energy_strategy(seed, weight, 1).unwrap()
        })
        .expect("an ordinary draw whose strategy survives a one-point change");
    let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(value["splice_weight"], 0);
    value["splice_weight"] = serde_json::json!(1);
    *line = serde_json::to_string(&value).unwrap();
    let error = replay_error(&lines);
    assert!(error.contains("mixture weights diverge"), "{error}");
}

#[test]
fn replay_rejects_an_adaptive_weight_not_derived_from_prior_admissions() {
    let config = continuation_config(1, DrawMixture::EnergySplice { scale: 6 }, 24);
    let mut stream = Vec::new();
    run_campaign_checkpointed(
        &TestWorkload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
    )
    .unwrap();
    let mut lines = std::str::from_utf8(&stream)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let initial = MixtureEnergy::default().splice_weights(6);
    let line = lines
        .iter_mut()
        .find(|line| {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            if value["event"] != "job" || value["selector"]["path"] == "continuation" {
                return false;
            }
            if value["reservation"].as_u64().unwrap() < config.window as u64 {
                return false;
            }
            let seed = value["mutation_seed"].as_u64().unwrap();
            let weight = u8::try_from(value["mixture_weight"].as_u64().unwrap()).unwrap();
            let splice = u8::try_from(value["splice_weight"].as_u64().unwrap()).unwrap();
            (weight, splice) != initial
                && weight < 253
                && energy_strategy(seed, weight, splice).unwrap()
                    == energy_strategy(seed, weight + 1, splice).unwrap()
        })
        .expect("an adaptive draw whose strategy survives a one-point change");
    let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
    value["mixture_weight"] = serde_json::json!(value["mixture_weight"].as_u64().unwrap() + 1);
    *line = serde_json::to_string(&value).unwrap();
    let error = replay_error(&lines);
    assert!(error.contains("mixture weights diverge"), "{error}");
}

#[test]
fn replay_rejects_a_skip_weight_not_derived_from_the_energy_policy() {
    let config = continuation_config(1, DrawMixture::Energy { scale: 6 }, 18);
    let mut stream = Vec::new();
    run_campaign_checkpointed(
        &TestWorkload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
    )
    .unwrap();
    let mut lines = std::str::from_utf8(&stream)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let line = lines
        .iter_mut()
        .find(|line| {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            value["event"] == "skip"
        })
        .expect("an ordinary duplicate skip");
    let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(value["splice_weight"], 0);
    value["splice_weight"] = serde_json::json!(1);
    *line = serde_json::to_string(&value).unwrap();
    let error = replay_error(&lines);
    assert!(error.contains("mixture weights diverge"), "{error}");
}

#[test]
fn replay_rejects_selector_tiers_beyond_the_archive_population_limit() {
    let config = continuation_config(1, DrawMixture::Energy { scale: 6 }, 18);
    let mut stream = Vec::new();
    run_campaign_checkpointed(
        &TestWorkload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
    )
    .unwrap();
    let original = std::str::from_utf8(&stream)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for kind in ["job", "skip"] {
        let mut lines = original.clone();
        let line = lines
            .iter_mut()
            .find(|line| {
                let value: serde_json::Value = serde_json::from_str(line).unwrap();
                value["event"] == kind && value["selector"]["path"] != "continuation"
            })
            .expect("an ordinary tier selection");
        let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
        value["selector"]["tier_rank"] = serde_json::json!(config.archive_entry_limit);
        *line = serde_json::to_string(&value).unwrap();
        let error = replay_error(&lines);
        assert!(
            error.contains("tier rank exceeds the archive population limit"),
            "{error}"
        );
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

#[test]
fn cell_intervals_credit_continuation_dispatches_to_their_cells() {
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
    let records = String::from_utf8(progress)
        .unwrap()
        .lines()
        .map(|line| {
            serde_json::from_str::<CampaignProgressRecord<serde_json::Value>>(line).unwrap()
        })
        .collect::<Vec<_>>();
    let final_record = records.last().expect("a final progress line");
    let continuation_selections = final_record
        .selector
        .continuation_selections
        .expect("continuation dispatches were counted");
    assert!(continuation_selections > 0);
    let intervals = records
        .iter()
        .filter_map(|record| record.selector.cell_interval.as_ref())
        .collect::<Vec<_>>();
    let interval_total: u64 = intervals
        .iter()
        .map(|interval| interval.totals.continuation)
        .sum();
    assert_eq!(interval_total, continuation_selections);
    for interval in intervals {
        if interval.cells == interval.top.len() {
            let row_total: u64 = interval.top.iter().map(|row| row.counts.continuation).sum();
            assert_eq!(row_total, interval.totals.continuation);
        }
        assert!(interval.totals.continuation <= interval.totals.draws);
    }
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
    let config = continuation_config(1, DrawMixture::EnergySplice { scale: 6 }, 24);
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
        error.contains("mixture weights diverge from the rebuilt policy"),
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
            execution_budget: 300,
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
            execution_budget: 300,
            ..continuation_config(workers, DrawMixture::EnergySplice { scale: 6 }, 7)
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

fn checkpoint_directory(label: &str) -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "dissonance-search-checkpoint-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    directory
}

fn progress_lines_after(progress: &[u8], after: u64) -> Vec<serde_json::Value> {
    String::from_utf8(progress.to_vec())
        .unwrap()
        .lines()
        .map(|line| {
            let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
            let fields = value.as_object_mut().unwrap();
            fields.remove("search_elapsed_millis");
            fields.remove("unix_time");
            value
        })
        .filter(|value| value["executions"].as_u64().unwrap() > after)
        .collect()
}

fn run_with_checkpoints(
    config: &CampaignConfig<TestWorkload>,
    origin: &CampaignOrigin<TestWorkload>,
    checkpoints: Option<CheckpointPlan>,
) -> (CampaignOutcome<TestWorkload>, Vec<u8>) {
    let mut stream = Vec::new();
    let mut progress = Vec::new();
    let outcome = run_campaign_checkpointed_with_options(
        &TestWorkload,
        config,
        origin,
        &mut stream,
        Some(&mut progress),
        CampaignExecutionOptions {
            checkpoints,
            ..CampaignExecutionOptions::default()
        },
    )
    .unwrap();
    (outcome, progress)
}

#[test]
fn a_campaign_resumed_from_a_search_checkpoint_repeats_the_original_progress() {
    for (workers, mixture, budget_mib) in [
        (1, DrawMixture::EnergySplice { scale: 6 }, 12),
        (4, DrawMixture::EnergySplice { scale: 6 }, 12),
        (4, DrawMixture::Energy { scale: 6 }, 18),
    ] {
        let label = format!("resume-{workers}-{budget_mib}");
        let directory = checkpoint_directory(&label);
        let mut config = continuation_config(workers, mixture, budget_mib);
        config.stop_campaign_on_objective = false;
        let (plain, plain_progress) = run_with_checkpoints(&config, &CampaignOrigin::Genesis, None);
        let (original, original_progress) = run_with_checkpoints(
            &config,
            &CampaignOrigin::Genesis,
            Some(CheckpointPlan {
                directory: directory.clone(),
                every: NonZeroU64::new(300),
                on_marks: true,
                on_top_progress: true,
                on_stall: false,
                checkpoint_owner: None,
                max_image_bytes: 64 << 20,
                before_write: None,
            }),
        );
        assert_eq!(
            progress_lines_after(&original_progress, 0),
            progress_lines_after(&plain_progress, 0),
            "writing checkpoints changed the progress of {label}"
        );
        assert_eq!(original.0.archive, plain.0.archive);
        let mut kept = std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "ckpt")
            })
            .collect::<Vec<_>>();
        kept.sort();
        assert!(kept.len() >= 2, "{label} kept {kept:?}");
        for path in kept {
            let resume_at: u64 = path.file_name().unwrap().to_str().unwrap()[..12]
                .parse()
                .unwrap();
            let (resumed, resumed_progress) = run_with_checkpoints(
                &config,
                &CampaignOrigin::SearchCheckpoint {
                    path,
                    expected_sha256: None,
                },
                None,
            );
            assert_eq!(
                progress_lines_after(&resumed_progress, resume_at),
                progress_lines_after(&original_progress, resume_at),
                "{label} resumed at {resume_at} diverged"
            );
            assert_eq!(resumed.0.archive, original.0.archive);
            assert_eq!(
                resumed.0.executions_completed,
                original.0.executions_completed
            );
            assert_eq!(resumed.0.execution_work, original.0.execution_work);
            assert_eq!(resumed.0.origin.kind, "search_checkpoint");
        }
        std::fs::remove_dir_all(&directory).unwrap();
    }
}

#[test]
fn a_campaign_and_checkpoint_reject_another_loaded_executable() {
    let directory = checkpoint_directory("loaded-executable");
    let mut config = continuation_config(1, DrawMixture::Energy { scale: 6 }, 12);
    config.stop_campaign_on_objective = false;
    config.execution_budget = 8;
    TEST_EXECUTABLE_IDENTITY.with(|identity| identity.set(Some(Ok("build-A"))));
    let mut stream = Vec::new();
    let source = run_campaign_checkpointed_with_options(
        &TestWorkload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
        CampaignExecutionOptions {
            checkpoints: Some(CheckpointPlan {
                directory: directory.clone(),
                every: NonZeroU64::new(1),
                on_marks: false,
                on_top_progress: false,
                on_stall: false,
                checkpoint_owner: None,
                max_image_bytes: 64 << 20,
                before_write: None,
            }),
            ..CampaignExecutionOptions::default()
        },
    )
    .unwrap();
    let checkpoint = directory.join("000000000008-final.ckpt");
    assert_eq!(source.0.executable_identity, "build-A");
    assert_eq!(
        CheckpointReader::open(&checkpoint)
            .unwrap()
            .header
            .executable_identity,
        "build-A"
    );
    assert_eq!(
        replay_campaign_checkpointed(&TestWorkload, &stream, None, None).unwrap(),
        source
    );
    config.execution_budget = 12;
    let origin = CampaignOrigin::SearchCheckpoint {
        path: checkpoint,
        expected_sha256: None,
    };
    let mut matching_stream = Vec::new();
    run_campaign_checkpointed(&TestWorkload, &config, &origin, &mut matching_stream, None).unwrap();
    TEST_EXECUTABLE_IDENTITY.with(|identity| identity.set(Some(Ok("build-B"))));
    let replay = replay_campaign_checkpointed(&TestWorkload, &stream, None, None)
        .err()
        .map(|error| error.to_string());
    let mut resumed_stream = Vec::new();
    let resume =
        run_campaign_checkpointed(&TestWorkload, &config, &origin, &mut resumed_stream, None)
            .err()
            .map(|error| error.to_string());
    TEST_EXECUTABLE_IDENTITY.with(|identity| identity.set(None));
    assert!(
        replay
            .as_deref()
            .is_some_and(|error| error.contains("executable identity")),
        "another executable replayed a completed campaign: {replay:?}"
    );
    assert!(
        resume
            .as_deref()
            .is_some_and(|error| error.contains("different executable")),
        "another executable resumed a search checkpoint: {resume:?}"
    );
    assert!(
        resumed_stream.is_empty(),
        "identity mismatch wrote a new stream"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn saved_states_from_another_executable_or_genesis_are_rebuilt_from_their_inputs() {
    let directory = checkpoint_directory("saved-states-executable");
    let mut config = continuation_config(1, DrawMixture::Energy { scale: 6 }, 12);
    config.stop_campaign_on_objective = false;
    config.execution_budget = 8;
    TEST_EXECUTABLE_IDENTITY.with(|identity| identity.set(Some(Ok("build-A"))));
    run_campaign_checkpointed_with_options(
        &TestWorkload,
        &config,
        &CampaignOrigin::Genesis,
        &mut Vec::new(),
        None,
        CampaignExecutionOptions {
            checkpoints: Some(CheckpointPlan {
                directory: directory.clone(),
                every: None,
                on_marks: false,
                on_top_progress: false,
                on_stall: false,
                checkpoint_owner: None,
                max_image_bytes: 64 << 20,
                before_write: None,
            }),
            ..CampaignExecutionOptions::default()
        },
    )
    .unwrap();
    let checkpoint = directory.join("000000000008-final.ckpt");
    config.execution_budget = 1;
    let import = |build: &'static str, genesis: u8| {
        TEST_EXECUTABLE_IDENTITY.with(|identity| identity.set(Some(Ok(build))));
        TEST_GENESIS.with(|value| value.set(genesis));
        let states = SavedStates::open(&TestWorkload, &checkpoint).unwrap();
        let (report, _) = run_campaign_checkpointed(
            &TestWorkload,
            &config,
            &CampaignOrigin::SavedStates { states },
            &mut Vec::new(),
            None,
        )
        .unwrap();
        let entries = report
            .archive
            .entries
            .iter()
            .filter(|entry| entry.created_execution == 0)
            .map(|entry| entry.input.actions.clone())
            .collect::<Vec<_>>();
        (report.tree_import.unwrap(), entries)
    };
    let (same, same_entries) = import("build-A", 0);
    let (other, other_entries) = import("build-B", 0);
    let (moved, _) = import("build-A", 7);
    TEST_EXECUTABLE_IDENTITY.with(|identity| identity.set(None));
    TEST_GENESIS.with(|value| value.set(0));
    std::fs::remove_dir_all(directory).unwrap();
    assert!(same.checkpointed > 0);
    assert_eq!(other.checkpointed, 0);
    assert_eq!(other.imported, same.imported);
    assert_eq!(other_entries, same_entries);
    assert_eq!(moved.checkpointed, 0);
    assert!(moved.imported > 0);
}

#[test]
fn checkpoint_archive_entries_match_the_saved_state_records() {
    let directory = checkpoint_directory("archive-entries");
    let mut config = continuation_config(1, DrawMixture::Energy { scale: 6 }, 12);
    config.stop_campaign_on_objective = false;
    config.execution_budget = 8;
    run_campaign_checkpointed_with_options(
        &TestWorkload,
        &config,
        &CampaignOrigin::Genesis,
        &mut Vec::new(),
        None,
        CampaignExecutionOptions {
            checkpoints: Some(CheckpointPlan {
                directory: directory.clone(),
                every: None,
                on_marks: false,
                on_top_progress: false,
                on_stall: false,
                checkpoint_owner: None,
                max_image_bytes: 64 << 20,
                before_write: None,
            }),
            ..CampaignExecutionOptions::default()
        },
    )
    .unwrap();
    let checkpoint = directory.join("000000000008-final.ckpt");
    let before = std::fs::read(&checkpoint).unwrap();
    let entries = checkpoint_archive_entries::<TestWorkload>(&checkpoint, None).unwrap();
    assert_eq!(std::fs::read(&checkpoint).unwrap(), before);
    let sha256 = format!("{:x}", Sha256::digest(&before));
    assert_eq!(
        checkpoint_archive_entries::<TestWorkload>(&checkpoint, Some(&sha256)).unwrap(),
        entries
    );
    assert!(checkpoint_archive_entries::<TestWorkload>(&checkpoint, Some("0")).is_err());
    let saved = SavedStates::open(&TestWorkload, &checkpoint).unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|(entry, _)| (entry.id, entry.created_execution, &entry.input.actions))
            .collect::<Vec<_>>(),
        saved
            .entries
            .iter()
            .map(|entry| (entry.id, entry.created_execution, &entry.input.actions))
            .collect::<Vec<_>>()
    );
    let by_id = entries
        .iter()
        .map(|(entry, _)| (entry.id, &entry.input.actions))
        .collect::<BTreeMap<_, _>>();
    let children = entries
        .iter()
        .filter_map(|(entry, _)| Some((entry, by_id.get(&entry.parent_id?)?)))
        .collect::<Vec<_>>();
    assert!(!children.is_empty());
    for (child, parent) in children {
        assert!(child.input.actions.len() > parent.len());
        assert!(child.input.actions.starts_with(parent));
    }
    assert!(entries.iter().any(|(_, active)| *active));
    std::fs::write(&checkpoint, &before[..before.len() - 1]).unwrap();
    assert!(checkpoint_archive_entries::<TestWorkload>(&checkpoint, None).is_err());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn unavailable_loaded_executable_fails_before_campaign_output() {
    let directory = checkpoint_directory("unavailable-executable");
    let mut config = continuation_config(1, DrawMixture::Energy { scale: 6 }, 12);
    config.execution_budget = 8;
    TEST_EXECUTABLE_IDENTITY.with(|identity| identity.set(Some(Err("unavailable executable"))));
    let mut stream = Vec::new();
    let error = run_campaign_checkpointed_with_options(
        &TestWorkload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
        CampaignExecutionOptions {
            checkpoints: Some(CheckpointPlan {
                directory: directory.clone(),
                every: NonZeroU64::new(1),
                on_marks: false,
                on_top_progress: false,
                on_stall: false,
                checkpoint_owner: None,
                max_image_bytes: 64 << 20,
                before_write: None,
            }),
            ..CampaignExecutionOptions::default()
        },
    )
    .err()
    .map(|error| error.to_string());
    TEST_EXECUTABLE_IDENTITY.with(|identity| identity.set(None));
    assert_eq!(error.as_deref(), Some("unavailable executable"));
    assert!(stream.is_empty());
    assert!(!directory.exists());
}

#[test]
fn a_pinned_checkpoint_refuses_a_valid_swap_after_selection() {
    let directory = checkpoint_directory("pinned-swap");
    let mut config = continuation_config(1, DrawMixture::Energy { scale: 6 }, 12);
    config.stop_campaign_on_objective = false;
    config.execution_budget = 320;
    run_with_checkpoints(
        &config,
        &CampaignOrigin::Genesis,
        Some(CheckpointPlan {
            directory: directory.clone(),
            every: NonZeroU64::new(100),
            on_marks: false,
            on_top_progress: false,
            on_stall: false,
            checkpoint_owner: Some("saved-run-a".into()),
            max_image_bytes: 64 << 20,
            before_write: None,
        }),
    );
    let selected = directory.join("000000000320-final.ckpt");
    let older = directory.join("000000000300-interval.ckpt");
    assert!(selected.is_file());
    assert!(older.is_file());
    let selected_sha256 = format!("{:x}", Sha256::digest(std::fs::read(&selected).unwrap()));
    config.execution_budget = 360;
    let mut stream = Vec::new();
    assert!(
        run_campaign_checkpointed_with_options(
            &TestWorkload,
            &config,
            &CampaignOrigin::SearchCheckpoint {
                path: selected.clone(),
                expected_sha256: Some(selected_sha256.clone()),
            },
            &mut stream,
            None,
            CampaignExecutionOptions::default(),
        )
        .is_ok()
    );
    std::fs::copy(&older, &selected).unwrap();
    assert_ne!(
        selected_sha256,
        format!("{:x}", Sha256::digest(std::fs::read(&selected).unwrap()))
    );
    stream.clear();
    assert!(
        run_campaign_checkpointed_with_options(
            &TestWorkload,
            &config,
            &CampaignOrigin::SearchCheckpoint {
                path: selected,
                expected_sha256: Some(selected_sha256),
            },
            &mut stream,
            None,
            CampaignExecutionOptions::default(),
        )
        .is_err()
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_fixed_window_writes_the_same_stream_at_any_worker_count() {
    let streams = [1, 4, 16, 64].map(|workers| {
        let mut config = continuation_config(workers, DrawMixture::EnergySplice { scale: 6 }, 24);
        config.window = 16;
        config.execution_budget = 1600;
        config.stop_campaign_on_objective = false;
        let mut stream = Vec::new();
        let (report, _) = run_campaign_checkpointed_with_options(
            &TestWorkload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
            CampaignExecutionOptions::default(),
        )
        .unwrap();
        assert!(
            report.duplicates_skipped > 0,
            "{workers} workers skipped no duplicate"
        );
        assert!(
            report.snapshot_evictions > 0,
            "{workers} workers evicted no snapshot"
        );
        assert!(
            std::str::from_utf8(&stream)
                .unwrap()
                .contains("\"path\":\"continuation\""),
            "{workers} workers dispatched no continuation"
        );
        stream
    });
    for (workers, stream) in [4, 16, 64].iter().zip(&streams[1..]) {
        assert!(
            stream == &streams[0],
            "{workers} workers changed the stream"
        );
    }
}

#[test]
fn a_checkpoint_resumes_with_another_worker_count_and_repeats_the_run() {
    let directory = checkpoint_directory("worker-count");
    let mut config = continuation_config(4, DrawMixture::EnergySplice { scale: 6 }, 12);
    config.window = 8;
    config.stop_campaign_on_objective = false;
    let (original, original_progress) = run_with_checkpoints(
        &config,
        &CampaignOrigin::Genesis,
        Some(CheckpointPlan {
            directory: directory.clone(),
            every: NonZeroU64::new(170),
            on_marks: false,
            on_top_progress: false,
            on_stall: false,
            checkpoint_owner: None,
            max_image_bytes: 64 << 20,
            before_write: None,
        }),
    );
    assert!(
        original.0.telemetry.admission.results_held > 0,
        "no result waited for admission"
    );
    let buffered = original.0.telemetry.completed_buffer;
    assert!(
        buffered.results_peak >= 2,
        "held results were not counted together"
    );
    assert!(buffered.actions_peak >= 2);
    assert_eq!(buffered.candidates_peak, buffered.actions_peak);
    assert_eq!(buffered.results_live, 0);
    assert_eq!(buffered.actions_live, 0);
    assert_eq!(buffered.candidates_live, 0);

    let freed: u64 = std::fs::read_to_string(directory.join("checkpoints.jsonl"))
        .unwrap()
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["freed_snapshots"]
                .as_u64()
                .unwrap()
        })
        .sum();
    assert!(freed > 0, "pruning interval checkpoints freed no snapshot");
    let mut kept = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "ckpt")
        })
        .collect::<Vec<_>>();
    kept.sort();
    assert_eq!(kept.len(), 3, "kept {kept:?}");
    assert!(kept.last().unwrap().ends_with("000000000800-final.ckpt"));
    for path in kept {
        let resume_at: u64 = path.file_name().unwrap().to_str().unwrap()[..12]
            .parse()
            .unwrap();
        for workers in [1, 3, 8] {
            config.workers = workers;
            let (resumed, resumed_progress) = run_with_checkpoints(
                &config,
                &CampaignOrigin::SearchCheckpoint {
                    path: path.clone(),
                    expected_sha256: None,
                },
                None,
            );
            assert_eq!(
                progress_lines_after(&resumed_progress, resume_at),
                progress_lines_after(&original_progress, resume_at),
                "{workers} workers resumed at {resume_at} diverged"
            );
            assert_eq!(resumed.0.archive, original.0.archive);
            assert_eq!(resumed.0.execution_work, original.0.execution_work);
        }
    }
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn a_short_campaign_finishes_with_a_checkpoint_that_can_extend_its_budget() {
    let directory = checkpoint_directory("short-final");
    let mut config = continuation_config(1, DrawMixture::EnergySplice { scale: 6 }, 12);
    config.execution_budget = 7;
    config.stop_campaign_on_objective = false;
    let (first, _) = run_with_checkpoints(
        &config,
        &CampaignOrigin::Genesis,
        Some(CheckpointPlan {
            directory: directory.clone(),
            every: NonZeroU64::new(100),
            on_marks: false,
            on_top_progress: false,
            on_stall: false,
            checkpoint_owner: None,
            max_image_bytes: 64 << 20,
            before_write: None,
        }),
    );
    let files = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "ckpt")
        })
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 1);
    assert!(
        files[0]
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with("-final.ckpt")
    );
    config.execution_budget = 20;
    let origin = CampaignOrigin::SearchCheckpoint {
        path: files[0].clone(),
        expected_sha256: None,
    };
    let (second, _) = run_with_checkpoints(&config, &origin, None);
    let (repeated, _) = run_with_checkpoints(&config, &origin, None);
    assert!(second.0.executions_completed > first.0.executions_completed);
    assert_eq!(second.0.archive, repeated.0.archive);
    assert_eq!(second.0.execution_work, repeated.0.execution_work);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_reseeded_resume_continues_from_the_checkpoint_with_new_draws() {
    let directory = checkpoint_directory("reseed");
    let mut config = continuation_config(4, DrawMixture::EnergySplice { scale: 6 }, 12);
    config.stop_campaign_on_objective = false;
    let (_, original_progress) = run_with_checkpoints(
        &config,
        &CampaignOrigin::Genesis,
        Some(CheckpointPlan {
            directory: directory.clone(),
            every: NonZeroU64::new(300),
            on_marks: false,
            on_top_progress: false,
            on_stall: false,
            checkpoint_owner: None,
            max_image_bytes: 64 << 20,
            before_write: None,
        }),
    );
    config.campaign_seed = 948;
    let (resumed, resumed_progress) = run_with_checkpoints(
        &config,
        &CampaignOrigin::SearchCheckpoint {
            path: directory.join("000000000300-interval.ckpt"),
            expected_sha256: None,
        },
        None,
    );
    assert_eq!(resumed.0.executions_completed, 800);
    assert_eq!(resumed.0.campaign_seed, 948);
    assert_eq!(
        progress_lines_after(&resumed_progress, 300)[0]["executions"],
        400
    );
    assert_ne!(
        progress_lines_after(&resumed_progress, 300),
        progress_lines_after(&original_progress, 300)
    );
    config.workers = 2;
    let (fewer_workers, fewer_workers_progress) = run_with_checkpoints(
        &config,
        &CampaignOrigin::SearchCheckpoint {
            path: directory.join("000000000300-interval.ckpt"),
            expected_sha256: None,
        },
        None,
    );
    assert_eq!(fewer_workers.0.stream_sha256, resumed.0.stream_sha256);
    assert_eq!(
        progress_lines_after(&fewer_workers_progress, 300),
        progress_lines_after(&resumed_progress, 300)
    );
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn a_resume_under_a_changed_search_policy_records_the_change() {
    let directory = checkpoint_directory("policy-change");
    let mut config = continuation_config(4, DrawMixture::EnergySplice { scale: 6 }, 12);
    config.stop_campaign_on_objective = false;
    run_with_checkpoints(
        &config,
        &CampaignOrigin::Genesis,
        Some(CheckpointPlan {
            directory: directory.clone(),
            every: NonZeroU64::new(300),
            on_marks: false,
            on_top_progress: false,
            on_stall: false,
            checkpoint_owner: None,
            max_image_bytes: 64 << 20,
            before_write: None,
        }),
    );
    config.mixture = DrawMixture::Energy { scale: 6 };
    let (resumed, _) = run_with_checkpoints(
        &config,
        &CampaignOrigin::SearchCheckpoint {
            path: directory.join("000000000300-interval.ckpt"),
            expected_sha256: None,
        },
        None,
    );
    assert_eq!(resumed.0.executions_completed, 800);
    let changes = &resumed.0.origin.checkpoint_policy_changes;
    assert_eq!(changes.keys().collect::<Vec<_>>(), ["mixture_policy"]);
    assert_ne!(
        changes["mixture_policy"].checkpoint,
        changes["mixture_policy"].run
    );
    std::fs::remove_dir_all(&directory).unwrap();
}

fn rewrite_checkpoint_policy(path: &std::path::Path, field: &str, value: &str) {
    let bytes = std::fs::read(path).unwrap();
    let payload = &bytes[..bytes.len() - 32];
    let (mut header, rest): (CheckpointHeader, _) = postcard::take_from_bytes(payload).unwrap();
    header.policies.insert(field.to_owned(), value.to_owned());
    let mut rewritten = postcard::to_allocvec(&header).unwrap();
    rewritten.extend_from_slice(rest);
    let checksum = sha2::Sha256::digest(&rewritten);
    rewritten.extend_from_slice(&checksum);
    std::fs::write(path, rewritten).unwrap();
}

#[test]
fn a_resume_accepts_and_records_each_rebuildable_policy_change() {
    let directory = checkpoint_directory("policy-fields");
    let mut config = continuation_config(4, DrawMixture::EnergySplice { scale: 6 }, 64);
    config.stop_campaign_on_objective = false;
    config.archive_entry_limit = 4096;
    let (_, original_progress) = run_with_checkpoints(
        &config,
        &CampaignOrigin::Genesis,
        Some(CheckpointPlan {
            directory: directory.clone(),
            every: NonZeroU64::new(300),
            on_marks: false,
            on_top_progress: false,
            on_stall: false,
            checkpoint_owner: None,
            max_image_bytes: 64 << 20,
            before_write: None,
        }),
    );
    let checkpoint = directory.join("000000000300-interval.ckpt");
    let recorded = std::fs::read(&checkpoint).unwrap();
    for field in [
        "parent_scheduler",
        "continuation_policy",
        PREFERENCE_PORTFOLIO_FIELD,
        PREFERENCE_POLICY_FIELD,
        DRAW_TABLE_POLICY_FIELD,
    ] {
        std::fs::write(&checkpoint, &recorded).unwrap();
        rewrite_checkpoint_policy(&checkpoint, field, "older");
        let (resumed, resumed_progress) = run_with_checkpoints(
            &config,
            &CampaignOrigin::SearchCheckpoint {
                path: checkpoint.clone(),
                expected_sha256: None,
            },
            None,
        );
        let changes = &resumed.0.origin.checkpoint_policy_changes;
        assert_eq!(changes.keys().collect::<Vec<_>>(), [field]);
        assert_eq!(changes[field].checkpoint, "older");
        assert_eq!(resumed.0.executions_completed, 800);
        assert_eq!(
            progress_lines_after(&resumed_progress, 300),
            progress_lines_after(&original_progress, 300),
            "a resume recording a {field} change diverged"
        );
    }
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn a_checkpoint_with_no_jobs_in_flight_resumes_under_a_larger_budget() {
    let directory = checkpoint_directory("drained");
    let mut config = continuation_config(4, DrawMixture::EnergySplice { scale: 6 }, 12);
    config.stop_campaign_on_objective = false;
    run_with_checkpoints(
        &config,
        &CampaignOrigin::Genesis,
        Some(CheckpointPlan {
            directory: directory.clone(),
            every: NonZeroU64::new(400),
            on_marks: false,
            on_top_progress: false,
            on_stall: false,
            checkpoint_owner: None,
            max_image_bytes: 64 << 20,
            before_write: None,
        }),
    );
    config.execution_budget = 1200;
    config.stop_campaign_on_objective = true;
    let (resumed, _) = run_with_checkpoints(
        &config,
        &CampaignOrigin::SearchCheckpoint {
            path: directory.join("000000000800-interval.ckpt"),
            expected_sha256: None,
        },
        None,
    );
    assert_eq!(resumed.0.executions_completed, 1200);
    assert_eq!(
        resumed
            .0
            .origin
            .checkpoint_policy_changes
            .keys()
            .collect::<Vec<_>>(),
        ["stop_campaign_on_objective"]
    );
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn a_drain_checkpoint_refills_the_original_admission_window_on_resume() {
    let source = checkpoint_directory("drain-window-source");
    let resumed_directory = checkpoint_directory("drain-window-resumed");
    let mut config = continuation_config(2, DrawMixture::EnergySplice { scale: 6 }, 12);
    config.execution_budget = 15;
    config.stop_campaign_on_objective = false;
    run_with_checkpoints(
        &config,
        &CampaignOrigin::Genesis,
        Some(CheckpointPlan {
            directory: source.clone(),
            every: NonZeroU64::new(2),
            on_marks: false,
            on_top_progress: false,
            on_stall: false,
            checkpoint_owner: None,
            max_image_bytes: 64 << 20,
            before_write: None,
        }),
    );
    let checkpoint = source.join("000000000014-interval.ckpt");
    assert!(checkpoint.exists());
    config.execution_budget = 25;
    let (resumed, _) = run_with_checkpoints(
        &config,
        &CampaignOrigin::SearchCheckpoint {
            path: checkpoint,
            expected_sha256: None,
        },
        Some(CheckpointPlan {
            directory: resumed_directory.clone(),
            every: NonZeroU64::new(1),
            on_marks: false,
            on_top_progress: false,
            on_stall: false,
            checkpoint_owner: None,
            max_image_bytes: 64 << 20,
            before_write: None,
        }),
    );
    assert_eq!(resumed.0.executions_completed, 25);
    let first: serde_json::Value = serde_json::from_str(
        std::fs::read_to_string(resumed_directory.join("checkpoints.jsonl"))
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first["executions"], 15);
    assert_eq!(
        first["reserved"].as_u64().unwrap() - 15,
        config.window as u64
    );
    std::fs::remove_dir_all(source).unwrap();
    std::fs::remove_dir_all(resumed_directory).unwrap();
}

#[test]
fn campaign_counters_with_an_import_round_trip_through_postcard() {
    let counters = CampaignCounters {
        bootstrap_execution_work: 3,
        tree_import: Some(TreeImportCounts {
            imported: 5,
            ..TreeImportCounts::default()
        }),
        job_execution_work: 7,
        work_to_first_objective: None,
        executions_to_first_objective: Some(9),
        duplicates_skipped: 11,
        draw_state_memory_bytes: 13,
    };
    let decoded: CampaignCounters =
        postcard::from_bytes(&postcard::to_allocvec(&counters).unwrap()).unwrap();
    assert_eq!(decoded.tree_import, counters.tree_import);
    assert_eq!(decoded.job_execution_work, 7);
    assert_eq!(decoded.duplicates_skipped, 11);
}

#[test]
fn a_resume_refuses_a_changed_workload_policy() {
    let header = |key: &str| CheckpointHeader {
        format: SEARCH_CHECKPOINT_FORMAT.to_owned(),
        checkpoint_owner: None,
        reason: "interval".to_owned(),
        workload_identity_sha256: String::new(),
        executable_identity: String::new(),
        campaign_seed: 1,
        window: 1,
        archive_entry_limit: 8,
        memory_budget_mib: None,
        policies: BTreeMap::from([
            ("key_policy".to_owned(), key.to_owned()),
            ("parent_scheduler".to_owned(), key.to_owned()),
        ]),
        executions: 0,
        reserved: 0,
        admitted: 0,
        planned_clock: 0,
    };
    let error = search_checkpoint_policy_changes(&header("a"), &header("b"))
        .expect_err("a changed key policy");
    assert!(error.to_string().contains("key_policy"), "{error}");
    assert!(!error.to_string().contains("parent_scheduler"), "{error}");
}

#[test]
fn doubled_suffix_limits_are_recorded_replayed_and_checked() {
    let config = continuation_config(4, DrawMixture::EnergySplice { scale: 6 }, 12);
    let mut bytes = Vec::new();
    let (live, checkpoint) = run_campaign_checkpointed(
        &TestWorkload,
        &config,
        &CampaignOrigin::Genesis,
        &mut bytes,
        None,
    )
    .unwrap();
    let text = std::str::from_utf8(&bytes).unwrap().to_owned();
    let records = text
        .lines()
        .skip(1)
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|value| value["event"] == "job" || value["event"] == "skip")
        .collect::<Vec<_>>();
    let limits = records
        .iter()
        .filter_map(|value| value["suffix_limit"].as_u64())
        .collect::<Vec<_>>();
    assert!(
        limits.iter().any(|limit| *limit > 1),
        "no parent doubled its suffix"
    );
    assert!(
        records
            .iter()
            .filter(|value| value["splice"]["tail_postcard"].is_array())
            .all(|value| value["suffix_limit"].is_null()),
        "a spliced or continued job carries a suffix limit"
    );
    let (replayed, replay_checkpoint) =
        replay_campaign_checkpointed(&TestWorkload, &bytes, None, None).unwrap();
    assert_eq!(live, replayed);
    assert_eq!(checkpoint, replay_checkpoint);
    for tamper in [
        serde_json::Value::Null,
        serde_json::json!(0),
        serde_json::json!(SUFFIX_DOUBLING_LIMIT + 1),
    ] {
        let mut lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
        let line = lines
            .iter_mut()
            .find(|line| line.contains("\"suffix_limit\""))
            .expect("a limited record");
        let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
        value["suffix_limit"] = tamper;
        *line = serde_json::to_string(&value).unwrap();
        let error = replay_error(&lines);
        assert!(
            error.contains("recorded suffix limit does not match the drawn suffix")
                || error.contains("replayed job suffix limit diverged at reservation"),
            "a tampered limit failed for another reason: {error}"
        );
    }
}

#[test]
fn replay_rejects_forged_post_admission_skip_suffix_limits() {
    let mut config = continuation_config(1, DrawMixture::EnergySplice { scale: 6 }, 24);
    config.execution_budget = 1600;
    config.campaign_seed = 3;
    let mut stream = Vec::new();
    run_campaign_checkpointed(
        &TestWorkload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
    )
    .unwrap();
    let mut lines = std::str::from_utf8(&stream)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let first_job = lines
        .iter()
        .position(|line| line.contains("\"event\":\"job\""))
        .unwrap();
    let (index, mut skip) = lines
        .iter()
        .enumerate()
        .skip(first_job)
        .map(|(index, line)| {
            (
                index,
                serde_json::from_str::<serde_json::Value>(line).unwrap(),
            )
        })
        .find(|(_, record)| record["event"] == "skip" && record["suffix_limit"] == 1)
        .expect("a one-action skip after the first admission");
    skip["suffix_limit"] = serde_json::json!(2);
    lines[index] = serde_json::to_string(&skip).unwrap();
    let error =
        replay_campaign_checkpointed(&TestWorkload, lines.join("\n").as_bytes(), None, None)
            .expect_err("replay certified a skip limit no live selector could have drawn")
            .to_string();
    assert!(error.contains("skip suffix limit diverged"), "{error}");
}

#[test]
fn replay_rejects_a_limit_overridden_by_an_in_flight_sibling() {
    let config = continuation_config(4, DrawMixture::EnergySplice { scale: 6 }, 12);
    let mut stream = Vec::new();
    run_campaign_checkpointed(
        &TestWorkload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
    )
    .unwrap();
    let mut lines = std::str::from_utf8(&stream)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let jobs = lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            (
                index,
                serde_json::from_str::<serde_json::Value>(line).unwrap(),
            )
        })
        .filter(|(_, record)| record["event"] == "job")
        .collect::<Vec<_>>();
    let window = config.window as u64;
    let (index, mut selected, sibling) = jobs
        .iter()
        .find_map(|(index, selected)| {
            let reservation = selected["reservation"].as_u64()?;
            (selected["suffix_limit"] == 1).then_some(())?;
            jobs.iter()
                .find(|(_, sibling)| {
                    let held = sibling["reservation"].as_u64().unwrap();
                    sibling["parent_id"] == selected["parent_id"]
                        && sibling["suffix_limit"].as_u64().unwrap_or(0) >= 2
                        && held < reservation
                        && held + window > reservation
                        && sibling["sequence"].as_u64().unwrap() + window > reservation
                })
                .map(|(_, sibling)| (*index, selected.clone(), sibling.clone()))
        })
        .expect("a one-action job reserved while a longer sibling was in flight");
    selected["suffix_limit"] = sibling["suffix_limit"].clone();
    lines[index] = serde_json::to_string(&selected).unwrap();
    let error =
        replay_campaign_checkpointed(&TestWorkload, lines.join("\n").as_bytes(), None, None)
            .expect_err("replay accepted a long sibling still in flight")
            .to_string();
    assert!(
        error.contains("suffix limit diverged at reservation"),
        "{error}"
    );
}

fn choice_config(
    workers: u32,
    mixture: DrawMixture,
    budget_mib: usize,
) -> CampaignConfig<ChoiceTestWorkload<true>> {
    let plain = continuation_config(workers, mixture, budget_mib);
    CampaignConfig {
        campaign_seed: plain.campaign_seed,
        workers: plain.workers,
        execution_budget: plain.execution_budget,
        host: plain.host,
        wall_budget: plain.wall_budget,
        stop_rollout_on_objective: plain.stop_rollout_on_objective,
        stop_campaign_on_objective: plain.stop_campaign_on_objective,
        archive_entry_limit: plain.archive_entry_limit,
        window: plain.window,
        memory_budget_mib: plain.memory_budget_mib,
        materialize_final_artifacts: plain.materialize_final_artifacts,
        run: (),
        mixture: plain.mixture,
        retention: plain.retention,
        objective_witness_path: plain.objective_witness_path,
        minimize: plain.minimize,
    }
}

fn run_choices_with_checkpoints(
    config: &CampaignConfig<ChoiceTestWorkload<true>>,
    origin: &CampaignOrigin<ChoiceTestWorkload<true>>,
    checkpoints: Option<CheckpointPlan>,
) -> (CampaignOutcome<ChoiceTestWorkload<true>>, Vec<u8>) {
    let mut stream = Vec::new();
    let mut progress = Vec::new();
    let outcome = run_campaign_checkpointed_with_options(
        &ChoiceTestWorkload::<true>,
        config,
        origin,
        &mut stream,
        Some(&mut progress),
        CampaignExecutionOptions {
            checkpoints,
            ..CampaignExecutionOptions::default()
        },
    )
    .unwrap();
    (outcome, progress)
}

#[test]
fn choice_plans_replay_and_resume_through_continuations_splices_and_snapshot_pressure() {
    for (workers, mixture, budget_mib) in [
        (1, DrawMixture::EnergySplice { scale: 6 }, 12),
        (4, DrawMixture::EnergySplice { scale: 6 }, 12),
        (4, DrawMixture::BiasedHalf, 12),
    ] {
        let config = choice_config(workers, mixture, budget_mib);
        let mut bytes = Vec::new();
        let (live, checkpoint) = run_campaign_checkpointed(
            &ChoiceTestWorkload::<true>,
            &config,
            &CampaignOrigin::Genesis,
            &mut bytes,
            None,
        )
        .unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(
            text.lines()
                .next()
                .unwrap()
                .contains("\"choice_policy\":\"choices_v1:inherited,untried_first\"")
        );
        let jobs: Vec<serde_json::Value> = text
            .lines()
            .skip(1)
            .map(|line| serde_json::from_str(line).unwrap())
            .filter(|record: &serde_json::Value| record["event"] == "job")
            .collect();
        let continuation = |job: &&serde_json::Value| job["selector"]["path"] == "continuation";
        assert!(
            jobs.iter().any(|job| continuation(&job)),
            "{workers} workers never dispatched a continuation"
        );
        assert!(
            jobs.iter()
                .filter(continuation)
                .all(|job| job.get("choices").is_none())
        );
        assert!(
            jobs.iter()
                .filter(|job| !continuation(job))
                .all(|job| job.get("choices").is_some())
        );
        assert!(jobs.iter().any(|job| job["choices"]["redrawn"] == true));
        assert!(
            live.snapshot_evictions > 0,
            "{workers} workers never met memory pressure"
        );
        let (replayed, replay_checkpoint) =
            replay_campaign_checkpointed(&ChoiceTestWorkload::<true>, &bytes, None, None).unwrap();
        assert_eq!(live, replayed, "{workers} workers diverged on replay");
        assert_eq!(checkpoint, replay_checkpoint);
    }
    let label = "choices-resume";
    let directory = checkpoint_directory(label);
    let mut config = choice_config(4, DrawMixture::EnergySplice { scale: 6 }, 12);
    config.stop_campaign_on_objective = false;
    let (original, original_progress) = run_choices_with_checkpoints(
        &config,
        &CampaignOrigin::Genesis,
        Some(CheckpointPlan {
            directory: directory.clone(),
            every: NonZeroU64::new(300),
            on_marks: true,
            on_top_progress: true,
            on_stall: false,
            checkpoint_owner: None,
            max_image_bytes: 64 << 20,
            before_write: None,
        }),
    );
    let mut kept = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "ckpt")
        })
        .collect::<Vec<_>>();
    kept.sort();
    assert!(kept.len() >= 2, "{label} kept {kept:?}");
    for path in kept {
        let resume_at: u64 = path.file_name().unwrap().to_str().unwrap()[..12]
            .parse()
            .unwrap();
        let (resumed, resumed_progress) = run_choices_with_checkpoints(
            &config,
            &CampaignOrigin::SearchCheckpoint {
                path,
                expected_sha256: None,
            },
            None,
        );
        assert_eq!(
            progress_lines_after(&resumed_progress, resume_at),
            progress_lines_after(&original_progress, resume_at),
            "resumed at {resume_at} diverged"
        );
        assert_eq!(resumed.0.archive, original.0.archive);
    }
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn a_continuation_passes_its_parent_profile_to_the_entries_it_keeps() {
    let config = choice_config(4, DrawMixture::EnergySplice { scale: 6 }, 12);
    let mut bytes = Vec::new();
    run_campaign_checkpointed(
        &ChoiceTestWorkload::<true>,
        &config,
        &CampaignOrigin::Genesis,
        &mut bytes,
        None,
    )
    .unwrap();
    let jobs: Vec<serde_json::Value> = std::str::from_utf8(&bytes)
        .unwrap()
        .lines()
        .skip(1)
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|record: &serde_json::Value| record["event"] == "job")
        .collect();
    let root = jobs[0]["parent_id"].clone();
    let root_profile = jobs
        .iter()
        .find(|job| job["parent_id"] == root && job["choices"]["redrawn"] != true)
        .map(|job| job["choices"]["profile"].clone())
        .unwrap();
    let mut profiles = BTreeMap::new();
    let mut from_continuations = BTreeSet::new();
    let mut checked = 0;
    for job in &jobs {
        let parent = job["parent_id"].as_u64().unwrap();
        let continuation = job["selector"]["path"] == "continuation";
        let inherited = profiles.get(&parent).cloned();
        if !continuation
            && job["choices"]["redrawn"] != true
            && from_continuations.contains(&parent)
        {
            let expected = inherited.clone().unwrap_or_else(|| root_profile.clone());
            assert_eq!(job["choices"]["profile"], expected, "job {job}");
            if expected != root_profile {
                checked += 1;
            }
        }
        let profile = if continuation {
            inherited
        } else {
            Some(job["choices"]["profile"].clone())
        };
        for decision in job["decisions"].as_array().unwrap() {
            if decision["decision"] == "retained" {
                let id = decision["id"].as_u64().unwrap();
                if let Some(profile) = &profile {
                    profiles.insert(id, profile.clone());
                }
                if continuation {
                    from_continuations.insert(id);
                }
            }
        }
    }
    assert!(
        checked > 0,
        "no job drew from a continuation's entry with an evolved profile"
    );
}

#[test]
fn a_search_checkpoint_does_not_resume_under_another_choice_policy() {
    let label = "choices-policy-change";
    let directory = checkpoint_directory(label);
    let mut config = choice_config(4, DrawMixture::EnergySplice { scale: 6 }, 12);
    config.stop_campaign_on_objective = false;
    run_choices_with_checkpoints(
        &config,
        &CampaignOrigin::Genesis,
        Some(CheckpointPlan {
            directory: directory.clone(),
            every: NonZeroU64::new(300),
            on_marks: false,
            on_top_progress: false,
            on_stall: false,
            checkpoint_owner: None,
            max_image_bytes: 64 << 20,
            before_write: None,
        }),
    );
    let path = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "ckpt")
        })
        .unwrap();
    TEST_CHOICE_POLICY.with(|policy| {
        policy.set(Some(ChoicePolicy {
            profiles: ChoiceProfiles::Fixed(ChoiceProfile::new(CERTAIN, 0)),
            untried_first: true,
        }));
    });
    let error = run_campaign_checkpointed_with_options(
        &ChoiceTestWorkload::<true>,
        &config,
        &CampaignOrigin::SearchCheckpoint {
            path,
            expected_sha256: None,
        },
        &mut Vec::new(),
        None,
        CampaignExecutionOptions::default(),
    )
    .expect_err("a checkpoint resumed under another choice policy")
    .to_string();
    TEST_CHOICE_POLICY.with(|policy| policy.set(None));
    assert!(
        error.contains("search checkpoint policies differ: choice_policy"),
        "{error}"
    );
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn each_untried_site_counts_its_own_jobs_and_replay_checks_the_site() {
    let site = |record: &serde_json::Value| {
        record
            .get("untried_site")
            .map_or(0, |site| site.as_u64().unwrap())
    };
    let config = choice_config(4, DrawMixture::EnergySplice { scale: 6 }, 12);
    let mut plain = Vec::new();
    run_campaign_checkpointed(
        &ChoiceTestWorkload::<true>,
        &config,
        &CampaignOrigin::Genesis,
        &mut plain,
        None,
    )
    .unwrap();
    assert!(
        !std::str::from_utf8(&plain)
            .unwrap()
            .contains("untried_site")
    );
    TEST_UNTRIED_SITES.with(|sites| sites.set(true));
    let mut bytes = Vec::new();
    let live = run_campaign_checkpointed(
        &ChoiceTestWorkload::<true>,
        &config,
        &CampaignOrigin::Genesis,
        &mut bytes,
        None,
    )
    .unwrap();
    let records: Vec<serde_json::Value> = std::str::from_utf8(&bytes)
        .unwrap()
        .lines()
        .skip(1)
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let cancels = records
        .iter()
        .filter(|record| record["event"] == "cancel")
        .count() as u64;
    let mut walks = BTreeMap::<(u64, u64), Vec<(u64, u64)>>::new();
    for job in records
        .iter()
        .filter(|record| record["event"] == "job" && record.get("choices").is_some())
    {
        walks
            .entry((job["parent_id"].as_u64().unwrap(), site(&job["choices"])))
            .or_default()
            .push((
                job["reservation"].as_u64().unwrap(),
                job["choices"]["untried"].as_u64().unwrap(),
            ));
    }
    for walk in walks.values_mut() {
        walk.sort_unstable();
        for (position, (_, ordinal)) in walk.iter().enumerate() {
            assert!(*ordinal >= position as u64 && *ordinal <= position as u64 + cancels);
        }
        assert!(walk.windows(2).all(|pair| pair[0].1 < pair[1].1));
    }
    assert!(
        walks.iter().any(|((parent, site), walk)| *site == 1
            && walk[0].1 == 0
            && walks
                .get(&(*parent, 0))
                .is_some_and(|other| other[0].1 == 0)),
        "no parent started a walk at both sites"
    );
    let (replayed, _) =
        replay_campaign_checkpointed(&ChoiceTestWorkload::<true>, &bytes, None, None).unwrap();
    assert_eq!(live.0, replayed);
    TEST_UNTRIED_SITES.with(|sites| sites.set(false));
    let error = replay_campaign_checkpointed(&ChoiceTestWorkload::<true>, &bytes, None, None)
        .expect_err("a stream whose suffix asks at another untried site replayed")
        .to_string();
    assert!(
        error.contains("asks its first choice at another untried site"),
        "{error}"
    );
}
