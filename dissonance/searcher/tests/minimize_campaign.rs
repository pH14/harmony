// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeMap, error::Error, num::NonZeroU64, num::NonZeroUsize, path::PathBuf};

use searcher::search::{
    archive::{
        ArchiveEntryReport, ArchiveKey, Input, MAX_ENTRIES_PER_KEY, RetentionPolicy,
        entries_by_suffix,
    },
    campaign::{
        ArchiveReportState, CampaignActionResult, CampaignConfig, CampaignExecutionOptions,
        CampaignIncumbentRecord, CampaignJobResult, CampaignOrigin, CampaignOutcome, CampaignTypes,
        Evaluation, InputPolicy, MinimizeConfig, Reporting, TargetExecution, WorkloadPolicies,
        postcard_value_sha256, replay_campaign_checkpointed,
        run_campaign_checkpointed_with_options,
    },
    checkpoint::CheckpointPlan,
    cost::ShrinkCost,
    draw::DrawMixture,
    rand::RomuDuoJrRand,
    rollout::{ExecutionDisposition, JobCeiling},
    shrink::{
        JobKind, ShrinkConfig, ShrinkEvent, ShrinkPolicy, ShrinkStop, shrink,
        window::{WindowConfig, WindowStop, search_window},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const GOAL: u8 = 12;
const MAX_STRIDE: u8 = 3;
const DEATH: Step = Step(MAX_STRIDE + 1);
const DEAD: u8 = u8::MAX;
const STAGE_LENGTH: u8 = 4;
const WHOLE: WalkRun = WalkRun {
    goal: GOAL,
    staged: false,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WalkRun {
    goal: u8,
    staged: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct Step(u8);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct WalkKey(u8);

thread_local! {
    static KEY_CAPACITY: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

impl ArchiveKey for WalkKey {
    fn capacity() -> usize {
        KEY_CAPACITY
            .with(std::cell::Cell::get)
            .unwrap_or(MAX_ENTRIES_PER_KEY)
    }

    type Place = u8;
    type Progress = ();
    type Identity = ();

    fn place(self) -> Self::Place {
        self.0
    }

    fn progress(self) -> Self::Progress {}

    fn identity(self) -> Self::Identity {}

    type Lineage = ();

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct WalkArchiveReport {
    #[serde(with = "entries_by_suffix")]
    entries: Vec<ArchiveEntryReport<Step, WalkKey, ()>>,
}

#[derive(Default)]
struct WalkTarget {
    position: u8,
    execution_work: u64,
}

struct Walk;

impl CampaignTypes for Walk {
    type JobObservation = ();
    type Target = WalkTarget;
    type Action = Step;
    type Key = WalkKey;
    type Milestones = ();
    type Progress = ();
    type Snapshot = u8;
    type Observations = ();
    type Evidence = ();
    type ArchiveReport = WalkArchiveReport;
    type Run = WalkRun;
}

impl Reporting for Walk {
    fn stream_format(&self) -> &'static str {
        "minimize-walk-v1"
    }

    fn checkpoint_format(&self) -> &'static str {
        "minimize-walk-checkpoint-v1"
    }

    fn workload_identity_sha256(&self) -> String {
        "minimize-walk".to_owned()
    }

    fn write_evidence_checkpoint(
        _evidence: &Self::Evidence,
        _out: &mut dyn std::io::Write,
    ) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    fn evidence_from_checkpoint(_bytes: &[u8]) -> Result<Self::Evidence, Box<dyn Error>> {
        Ok(())
    }

    fn action_cost_unit(&self) -> &'static str {
        "steps"
    }

    fn execution_work_unit(&self) -> &'static str {
        "steps"
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
        WalkArchiveReport {
            entries: state.entries,
        }
    }
}

impl InputPolicy for Walk {
    fn policies(&self, run: &Self::Run) -> WorkloadPolicies {
        [
            ("step_policy".to_owned(), "walk-v1".to_owned()),
            ("walk_goal".to_owned(), run.goal.to_string()),
            ("walk_staged".to_owned(), run.staged.to_string()),
        ]
        .into_iter()
        .collect()
    }

    fn resolve_recorded(&self, policies: &WorkloadPolicies) -> Result<Self::Run, Box<dyn Error>> {
        let field = |name: &str| {
            policies
                .get(name)
                .ok_or(format!("walk stream has no {name}"))
        };
        Ok(WalkRun {
            goal: field("walk_goal")?.parse()?,
            staged: field("walk_staged")?.parse()?,
        })
    }

    fn sample_alphabet(
        &self,
        _run: &Self::Run,
        _previous: Option<&Self::Action>,
        rand: &mut RomuDuoJrRand,
    ) -> Result<Self::Action, Box<dyn Error>> {
        let strides = NonZeroUsize::new(usize::from(MAX_STRIDE) + 1).expect("stride count");
        Ok(Step(u8::try_from(rand.below(strides))?))
    }

    fn simpler_actions(&self, _run: &Self::Run, action: &Self::Action) -> Vec<Self::Action> {
        (action.0 == 1).then_some(Step(2)).into_iter().collect()
    }
}

impl ShrinkPolicy for Walk {
    fn window_actions(&self, _run: &Self::Run) -> Vec<Self::Action> {
        vec![Step(1), Step(2)]
    }
}

fn advance(position: u8, step: Step) -> u8 {
    if position == DEAD || step == DEATH {
        return DEAD;
    }
    position.saturating_add(step.0).min(GOAL)
}

impl TargetExecution for Walk {
    fn new_target(&self) -> Result<Self::Target, String> {
        Ok(WalkTarget::default())
    }

    fn reset(&self, target: &mut Self::Target) {
        target.position = 0;
    }

    fn restore(
        &self,
        target: &mut Self::Target,
        snapshot: &Self::Snapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.position = *snapshot;
        Ok(())
    }

    fn execution_work(&self, target: &Self::Target) -> u64 {
        target.execution_work
    }

    fn action_cost_fn(&self) -> fn(&Self::Action) -> u64 {
        |_| 1
    }

    fn snapshot_memory_charge(_snapshot: &Self::Snapshot) -> usize {
        std::mem::size_of::<u8>()
    }

    fn apply_action(
        &self,
        target: &mut Self::Target,
        action: &Self::Action,
        _milestones: &mut Self::Milestones,
    ) -> Result<(), Box<dyn Error>> {
        target.position = advance(target.position, *action);
        target.execution_work = target.execution_work.saturating_add(1);
        Ok(())
    }

    fn rollout_observations(&self, _target: &Self::Target) -> Vec<Self::Observations> {
        Vec::new()
    }

    fn snapshot(&self, target: &mut Self::Target) -> Result<Self::Snapshot, Box<dyn Error>> {
        Ok(target.position)
    }
}

fn remaining_steps(position: u8) -> u64 {
    steps_to(GOAL, position)
}

fn steps_to(goal: u8, position: u8) -> u64 {
    u64::from(goal.saturating_sub(position).div_ceil(MAX_STRIDE))
}

impl Evaluation for Walk {
    fn execution_disposition(&self, target: &Self::Target) -> ExecutionDisposition {
        if target.position == DEAD {
            ExecutionDisposition::Terminal
        } else {
            ExecutionDisposition::Runnable
        }
    }

    fn objective_reached(
        &self,
        run: &Self::Run,
        target: &Self::Target,
    ) -> Result<bool, Box<dyn Error>> {
        Ok(target.position >= run.goal && target.position != DEAD)
    }

    fn remaining_cost_bound(&self, run: &Self::Run, target: &Self::Target) -> ShrinkCost {
        ShrinkCost::new(steps_to(run.goal, target.position), 0)
    }

    fn stage(&self, run: &Self::Run, target: &Self::Target) -> Option<u32> {
        run.staged
            .then(|| u32::from(target.position.min(GOAL) / STAGE_LENGTH))
    }

    fn stage_run(&self, run: &Self::Run, stage: u32) -> Option<Self::Run> {
        let goal = u8::try_from(stage).ok()?.checked_mul(STAGE_LENGTH)?;
        (goal < GOAL).then_some(WalkRun { goal, ..*run })
    }

    fn current_key(&self, target: &Self::Target) -> Result<Self::Key, Box<dyn Error>> {
        Ok(WalkKey(target.position))
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
        _source: &Self::ArchiveReport,
    ) -> Result<Input<Self::Action>, Box<dyn Error>> {
        Err("walk archive resume is not used".into())
    }
}

fn walk_config(workers: u32) -> CampaignConfig<Walk> {
    CampaignConfig {
        campaign_seed: 0x3a1d_77e1,
        workers,
        execution_budget: 160,
        host: "fixture".to_owned(),
        wall_budget: None,
        stop_rollout_on_objective: true,
        stop_campaign_on_objective: true,
        archive_entry_limit: 256,
        window: 4,
        memory_budget_mib: None,
        materialize_final_artifacts: true,
        run: WHOLE,
        mixture: DrawMixture::AlphabetOnly,
        retention: RetentionPolicy::Unprobed,
        objective_witness_path: None,
        minimize: Some(MinimizeConfig::default()),
    }
}

fn slow_tape() -> Input<Step> {
    Input {
        actions: vec![Step(1); usize::from(GOAL)],
    }
}

const fn tape_origin(input: Input<Step>, root: usize) -> CampaignOrigin<Walk> {
    CampaignOrigin::Tape { input, root }
}

fn run_walk(
    config: &CampaignConfig<Walk>,
    origin: &CampaignOrigin<Walk>,
    checkpoints: Option<CheckpointPlan>,
) -> (CampaignOutcome<Walk>, Vec<u8>) {
    let mut stream = Vec::new();
    let outcome = run_campaign_checkpointed_with_options(
        &Walk,
        config,
        origin,
        &mut stream,
        None,
        CampaignExecutionOptions {
            checkpoints,
            ..CampaignExecutionOptions::default()
        },
    )
    .expect("minimize campaign");
    (outcome, stream)
}

fn job_records(stream: &[u8]) -> Vec<Value> {
    std::str::from_utf8(stream)
        .expect("campaign stream is UTF-8")
        .lines()
        .skip(1)
        .map(|line| serde_json::from_str::<Value>(line).expect("campaign record JSON"))
        .filter(|record| record["event"] == "job")
        .collect()
}

fn walk_cost(input: &Input<Step>) -> (Option<usize>, ShrinkCost) {
    let mut position = 0;
    let mut cost = ShrinkCost::ZERO;
    for (index, step) in input.actions.iter().enumerate() {
        position = advance(position, *step);
        cost = cost + ShrinkCost::new(1, 0);
        if position >= GOAL && position != DEAD {
            return (Some(index), cost);
        }
    }
    (None, cost)
}

fn checkpoint_directory(label: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "dissonance-minimize-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    directory
}

fn check_stop(outcome: &CampaignOutcome<Walk>, config: &CampaignConfig<Walk>) {
    let report = &outcome.0;
    match report.shrink_stop {
        Some(ShrinkStop::JobBudget) => {
            assert_eq!(report.executions_completed, config.execution_budget);
        }
        Some(ShrinkStop::LowerBound) => {
            assert!(report.executions_completed <= config.execution_budget);
            assert!(
                report.incumbent_cost.expect("incumbent")
                    <= report.minimize_progress.as_ref().expect("progress").floor
            );
        }
        stop => panic!("unexpected minimize stop {stop:?}"),
    }
}

fn check_minimize_campaign(
    outcome: &CampaignOutcome<Walk>,
    stream: &[u8],
    start: Option<ShrinkCost>,
    floor: ShrinkCost,
) -> ShrinkCost {
    let report = &outcome.0;
    assert!(report.minimize.is_some());
    let entries = report
        .archive
        .entries
        .iter()
        .map(|entry| (entry.id, &entry.input))
        .collect::<BTreeMap<_, _>>();
    let mut current = start;
    let mut accepted = 0;
    let mut pruned = 0;
    let mut checked = 0;
    for record in job_records(stream) {
        let ceiling: Option<JobCeiling> =
            serde_json::from_value(record["ceiling"].clone()).expect("job ceiling");
        if let Some(ceiling) = ceiling {
            let incumbent = current.expect("a ceiling needs an incumbent");
            assert!(ceiling.ceiling >= incumbent, "{record}");
            assert!(
                start.is_none_or(|start| ceiling.ceiling <= start),
                "{record}"
            );
            if let Some(parent) = record["parent_id"].as_u64().and_then(|id| entries.get(&id))
                && ceiling.ceiling > floor
            {
                let position = parent.actions.iter().fold(0, |at, step| advance(at, *step));
                assert!(
                    ceiling.start + ShrinkCost::new(remaining_steps(position), 0) < ceiling.ceiling,
                    "a job started from an entry whose cost and bound reach the incumbent: {record}"
                );
            }
        }
        for decision in record["decisions"].as_array().expect("decisions") {
            if decision["decision"] == "pruned" {
                pruned += 1;
            }
            let (Some(incumbent), Some(input)) = (
                current,
                decision["id"].as_u64().and_then(|id| entries.get(&id)),
            ) else {
                continue;
            };
            if decision["decision"] != "retained" {
                continue;
            }
            let position = input.actions.iter().fold(0, |at, step| advance(at, *step));
            let lower = ShrinkCost::new(
                u64::try_from(input.actions.len()).unwrap() + remaining_steps(position),
                0,
            );
            assert!(lower < incumbent, "{record}");
            checked += 1;
        }
        if let Some(incumbent) = record.get("incumbent") {
            let incumbent: CampaignIncumbentRecord =
                serde_json::from_value(incumbent.clone()).expect("incumbent");
            assert!(
                current.is_none_or(|current| incumbent.cost < current),
                "incumbent cost {:?} did not fall below {current:?}",
                incumbent.cost
            );
            current = Some(incumbent.cost);
            accepted += 1;
        }
    }
    assert!(accepted > 0, "no cheaper objective was accepted");
    assert!(
        checked > 0,
        "no retained entry survived to the final archive"
    );
    assert!(pruned > 0, "no candidate was pruned against the incumbent");
    assert!(
        report.doomed_endpoints > 0,
        "no rollout stopped at its ceiling"
    );

    let final_cost = report.incumbent_cost.expect("final incumbent cost");
    assert_eq!(Some(final_cost), current);
    let witness = report
        .objective_witness
        .as_ref()
        .expect("objective witness");
    assert_eq!(
        walk_cost(witness),
        (Some(witness.actions.len() - 1), final_cost)
    );

    let replayed =
        replay_campaign_checkpointed(&Walk, stream, None, None).expect("minimize campaign replay");
    assert_eq!(&replayed, outcome);
    final_cost
}

#[test]
fn a_minimize_campaign_from_a_tape_finds_a_cheaper_objective_and_replays() {
    let config = walk_config(2);
    let tape = slow_tape();
    let (outcome, stream) = run_walk(&config, &tape_origin(tape.clone(), 0), None);
    assert_eq!(outcome.0.origin.kind, "tape");
    check_stop(&outcome, &config);
    let final_cost = check_minimize_campaign(
        &outcome,
        &stream,
        Some(walk_cost(&tape).1),
        ShrinkCost::new(remaining_steps(0), 0),
    );
    assert!(final_cost < ShrinkCost::new(u64::from(GOAL), 0));
}

fn kinds_in(stream: &[u8]) -> BTreeMap<String, usize> {
    let mut kinds = BTreeMap::new();
    for record in job_records(stream) {
        if let Some(kind) = record["shrink"]["kind"].as_str() {
            *kinds.entry(kind.to_owned()).or_default() += 1;
        }
    }
    kinds
}

#[test]
fn a_minimize_campaign_matches_across_worker_counts() {
    for shortcut_snapshots in [false, true] {
        check_worker_counts(MinimizeConfig {
            shortcut_snapshots,
            ..MinimizeConfig::default()
        });
    }
}

fn check_worker_counts(minimize: MinimizeConfig) {
    let origin = tape_origin(padded_tape(), 0);
    let config = |workers| {
        let mut config = walk_config(workers);
        config.minimize = Some(minimize);
        config
    };
    let (single, single_stream) = run_walk(&config(1), &origin, None);
    let (parallel, parallel_stream) = run_walk(&config(4), &origin, None);
    let kinds = kinds_in(&single_stream);
    let expected: &[&str] = if minimize.shortcut_snapshots {
        &["shortcut", "ordinary"]
    } else {
        &["shortcut", "ordinary", "continuation"]
    };
    assert!(
        expected.iter().all(|kind| kinds.contains_key(*kind)),
        "{minimize:?} {kinds:?}"
    );
    let shortcut_retained = job_records(&single_stream).iter().any(|record| {
        record["shrink"]["kind"] == "shortcut"
            && record["decisions"]
                .as_array()
                .is_some_and(|decisions| decisions.iter().any(|d| d["decision"] == "retained"))
    });
    assert_eq!(
        shortcut_retained, minimize.shortcut_snapshots,
        "{minimize:?}"
    );
    assert_eq!(
        String::from_utf8(single_stream.clone()).unwrap(),
        String::from_utf8(parallel_stream.clone()).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&single.0).unwrap(),
        serde_json::to_value(&parallel.0).unwrap()
    );
    assert_eq!(single.1, parallel.1);
    for stream in [&single_stream, &parallel_stream] {
        let replayed = replay_campaign_checkpointed(&Walk, stream, None, None)
            .expect("minimize campaign replay");
        assert_eq!(
            serde_json::to_value(&replayed.0).unwrap(),
            serde_json::to_value(&single.0).unwrap()
        );
    }
}

#[test]
fn a_minimize_campaign_resumed_from_a_search_checkpoint_matches_the_uninterrupted_run() {
    for shortcut_snapshots in [false, true] {
        check_resume(shortcut_snapshots);
    }
}

fn check_resume(shortcut_snapshots: bool) {
    let directory = checkpoint_directory(&format!("resume-{shortcut_snapshots}"));
    let mut config = walk_config(2);
    config.minimize = Some(MinimizeConfig {
        shortcut_snapshots,
        ..MinimizeConfig::default()
    });
    let origin = tape_origin(padded_tape(), 0);
    let (plain, _) = run_walk(&config, &origin, None);
    let (original, _) = run_walk(
        &config,
        &origin,
        Some(CheckpointPlan {
            directory: directory.clone(),
            every: NonZeroU64::new(8),
            on_marks: false,
            on_top_progress: false,
            on_stall: false,
            checkpoint_owner: None,
            max_image_bytes: 64 << 20,
        }),
    );
    assert_eq!(original.0.archive, plain.0.archive);
    assert_eq!(original.0.incumbent_cost, plain.0.incumbent_cost);
    let mut kept = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "ckpt")
        })
        .collect::<Vec<_>>();
    kept.sort();
    assert!(kept.len() >= 2, "kept {kept:?}");
    for path in kept {
        let (resumed, _) = run_walk(
            &config,
            &CampaignOrigin::SearchCheckpoint {
                path: path.clone(),
                expected_sha256: None,
            },
            None,
        );
        assert_eq!(resumed.0.origin.kind, "search_checkpoint", "{path:?}");
        assert_eq!(resumed.0.archive, plain.0.archive, "{path:?}");
        assert_eq!(resumed.0.incumbent_cost, plain.0.incumbent_cost, "{path:?}");
        assert_eq!(
            resumed.0.objective_witness, plain.0.objective_witness,
            "{path:?}"
        );
        assert_eq!(
            resumed.0.executions_completed, plain.0.executions_completed,
            "{path:?}"
        );
        assert_eq!(resumed.0.execution_work, plain.0.execution_work, "{path:?}");
    }
    std::fs::remove_dir_all(&directory).unwrap();
}

fn check_unsolved_tape(tape: &Input<Step>, states: usize, root: usize) {
    let config = walk_config(2);
    let (outcome, stream) = run_walk(&config, &tape_origin(tape.clone(), root), None);
    let report = &outcome.0;
    assert_eq!(report.origin.kind, "tape");
    let inputs = report
        .archive
        .entries
        .iter()
        .map(|entry| entry.input.actions.clone())
        .collect::<Vec<_>>();
    for length in root.max(1)..=states {
        assert!(
            inputs.contains(&tape.actions[..length].to_vec()),
            "the tape state after {length} actions is not in the archive"
        );
    }
    if let Some(past) = tape.actions.get(..=states) {
        assert!(
            !inputs.iter().any(|input| input.starts_with(past)),
            "the archive holds a state past the tape's last live state"
        );
    }
    let first = job_records(&stream)
        .into_iter()
        .position(|record| record.get("incumbent").is_some())
        .expect("an arrival became the incumbent");
    assert!(
        job_records(&stream)[..first]
            .iter()
            .all(|record| record["shrink"].is_null() && record["ceiling"].is_null()),
        "a job before the first arrival ran as a minimize kind or under a ceiling"
    );
    let witness = report.objective_witness.as_ref().expect("witness");
    assert!(witness.actions.starts_with(&tape.actions[..root]));
    let cost = report.incumbent_cost.expect("incumbent cost");
    assert_eq!(walk_cost(witness), (Some(witness.actions.len() - 1), cost));
    let replayed =
        replay_campaign_checkpointed(&Walk, &stream, None, None).expect("unsolved tape replay");
    assert_eq!(&replayed, &outcome);
}

#[test]
fn a_tape_that_stops_short_of_the_objective_searches_from_its_states_until_an_arrival() {
    let tape = Input {
        actions: vec![Step(MAX_STRIDE); 3],
    };
    check_unsolved_tape(&tape, 3, 0);
    check_unsolved_tape(&tape, 3, 2);
}

#[test]
fn a_tape_that_dies_before_the_objective_searches_from_its_live_states_until_an_arrival() {
    let tape = Input {
        actions: vec![
            Step(MAX_STRIDE),
            Step(MAX_STRIDE),
            Step(MAX_STRIDE),
            DEATH,
            Step(1),
        ],
    };
    check_unsolved_tape(&tape, 3, 0);
    check_unsolved_tape(&tape, 3, 3);
}

#[test]
fn a_tape_origin_requires_minimize() {
    let mut config = walk_config(1);
    config.minimize = None;
    let mut stream = Vec::new();
    let error = run_campaign_checkpointed_with_options(
        &Walk,
        &config,
        &tape_origin(slow_tape(), 0),
        &mut stream,
        None,
        CampaignExecutionOptions::default(),
    )
    .expect_err("a tape origin without minimize must be rejected");
    assert!(error.to_string().contains("minimize"), "{error}");
}

#[test]
fn a_minimize_campaign_from_genesis_keeps_its_cheapest_objective() {
    let config = walk_config(2);
    let (outcome, stream) = run_walk(&config, &CampaignOrigin::Genesis, None);
    assert_eq!(outcome.0.origin.kind, "genesis");
    check_stop(&outcome, &config);
    check_minimize_campaign(
        &outcome,
        &stream,
        None,
        ShrinkCost::new(remaining_steps(0), 0),
    );
}

#[test]
fn a_tape_root_keeps_every_entry_and_job_at_or_after_the_root() {
    const ROOT: usize = 5;
    let config = walk_config(2);
    let tape = slow_tape();
    let (outcome, stream) = run_walk(&config, &tape_origin(tape.clone(), ROOT), None);
    let prefix = &tape.actions[..ROOT];
    for entry in &outcome.0.archive.entries {
        if entry.id != 0 {
            assert!(
                entry.input.actions.starts_with(prefix),
                "entry {} lies before the root: {:?}",
                entry.id,
                entry.input
            );
        }
    }
    let mut shortcuts = 0;
    for record in job_records(&stream) {
        assert_ne!(
            record["parent_id"], 0,
            "a job started before the root: {record}"
        );
        if let Some(start) = record["shrink"]["shortcut"]["start"].as_u64() {
            assert!(start >= u64::try_from(ROOT).unwrap(), "{record}");
            shortcuts += 1;
        }
    }
    assert!(shortcuts > 0, "no shortcut job ran");
    let witness = outcome.0.objective_witness.as_ref().expect("witness");
    assert!(witness.actions.starts_with(prefix));
    let root_cost = ShrinkCost::new(u64::try_from(ROOT).unwrap(), 0);
    check_minimize_campaign(
        &outcome,
        &stream,
        Some(walk_cost(&tape).1),
        root_cost + ShrinkCost::new(remaining_steps(u8::try_from(ROOT).unwrap()), 0),
    );
}

#[test]
fn a_tape_root_past_the_objective_is_rejected() {
    let mut stream = Vec::new();
    let error = run_campaign_checkpointed_with_options(
        &Walk,
        &walk_config(1),
        &tape_origin(slow_tape(), usize::from(GOAL)),
        &mut stream,
        None,
        CampaignExecutionOptions::default(),
    )
    .expect_err("a root at the objective must be rejected");
    assert!(error.to_string().contains("root"), "{error}");
}

#[test]
fn a_tape_root_whose_state_loses_its_slot_to_genesis_is_rejected() {
    let mut actions = vec![Step(0), Step(0)];
    actions.extend([Step(3); 4]);
    KEY_CAPACITY.with(|capacity| capacity.set(Some(1)));
    for ordinary in [false, true] {
        let mut stream = Vec::new();
        let error = run_campaign_checkpointed_with_options(
            &Walk,
            &minimize_config(ordinary, 1, 64),
            &tape_origin(
                Input {
                    actions: actions.clone(),
                },
                1,
            ),
            &mut stream,
            None,
            CampaignExecutionOptions::default(),
        )
        .expect_err("a root without an archive slot must be rejected");
        assert!(error.to_string().contains("root"), "{error}");
    }
    KEY_CAPACITY.with(|capacity| capacity.set(None));
}

fn minimize_config(ordinary: bool, window: usize, execution_budget: u64) -> CampaignConfig<Walk> {
    let mut config = walk_config(1);
    config.window = window;
    config.execution_budget = execution_budget;
    config.minimize = Some(MinimizeConfig {
        ordinary,
        ..MinimizeConfig::default()
    });
    config
}

fn padded_tape() -> Input<Step> {
    Input {
        actions: [0, 1, 0, 2, 0, 0, 1]
            .into_iter()
            .cycle()
            .take(21)
            .map(Step)
            .collect(),
    }
}

#[test]
fn shortcut_jobs_reach_the_same_cost_at_each_job_budget() {
    let window_one = [
        (4, 14),
        (8, 14),
        (16, 12),
        (24, 10),
        (32, 6),
        (48, 6),
        (64, 6),
        (160, 6),
    ];
    for window in [1, 4] {
        for (budget, cost) in window_one {
            let config = minimize_config(false, window, budget);
            let (outcome, stream) = run_walk(&config, &tape_origin(padded_tape(), 0), None);
            let report = &outcome.0;
            let progress = report
                .minimize_progress
                .as_ref()
                .expect("minimize progress");
            assert_eq!(progress.jobs.ordinary + progress.jobs.continuation, 0);
            let campaign = report.incumbent_cost.expect("incumbent");
            if window == 1 {
                assert_eq!(campaign, ShrinkCost::new(cost, 0), "budget {budget}");
            }
            let replayed =
                replay_campaign_checkpointed(&Walk, &stream, None, None).expect("replay");
            assert_eq!(&replayed, &outcome);
        }
    }
}

fn shrink_config(label: &str) -> (ShrinkConfig<WalkRun>, PathBuf) {
    let directory = checkpoint_directory(label);
    std::fs::create_dir_all(&directory).unwrap();
    let mut config = ShrinkConfig::new(WHOLE, 0x3a1d_77e1);
    config.workers = 2;
    config.job_budget = Some(160);
    config.stream = Some(directory.join("stream.jsonl"));
    (config, directory)
}

#[test]
fn shrink_is_the_minimize_campaign_from_its_tape() {
    let (config, directory) = shrink_config("shrink-call");
    let mut improved = Vec::new();
    let mut progressed = 0;
    let outcome = shrink(&Walk, &config, &padded_tape(), |event| {
        match event {
            ShrinkEvent::Improved { input, improvement } => {
                assert_eq!(walk_cost(input).1, improvement.cost);
                improved.push(*improvement);
            }
            ShrinkEvent::Progress { .. } => progressed += 1,
        }
        Ok(())
    })
    .expect("shrink");
    let mut campaign = walk_config(2);
    campaign.window = 2;
    campaign.materialize_final_artifacts = false;
    campaign.minimize = Some(MinimizeConfig {
        ordinary_length: config.ordinary_length,
        ..MinimizeConfig::default()
    });
    let (expected, _) = run_walk(&campaign, &tape_origin(padded_tape(), 0), None);
    let report = &expected.0;
    assert_eq!(Some(&outcome.input), report.objective_witness.as_ref());
    assert_eq!(outcome.progress.cost, report.incumbent_cost);
    assert_eq!(
        outcome.progress.initial_cost,
        Some(walk_cost(&padded_tape()).1)
    );
    assert_eq!(Some(outcome.stop), report.shrink_stop);
    assert_eq!(outcome.improvements, improved);
    assert!(!improved.is_empty());
    assert!(progressed > 0);
    let progress = report.minimize_progress.as_ref().expect("progress");
    assert_eq!(outcome.progress.accepted, progress.accepted);
    assert_eq!(outcome.progress.jobs.total(), report.executions_completed);
    assert_eq!(
        u64::try_from(outcome.improvements.len()).unwrap(),
        outcome.progress.accepted.total()
    );
    let stream = std::fs::read(config.stream.as_ref().unwrap()).unwrap();
    replay_campaign_checkpointed(&Walk, &stream, None, None).expect("shrink stream replay");
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn shrink_from_an_unsolved_tape_reports_its_first_arrival_and_replays() {
    let tapes = [
        ("short", vec![Step(1); 4]),
        ("dies", vec![Step(1), Step(1), DEATH, Step(1)]),
    ];
    for (label, actions) in tapes {
        let (config, directory) = shrink_config(&format!("shrink-{label}"));
        let tape = Input { actions };
        let outcome = shrink(&Walk, &config, &tape, |_| Ok(())).expect("shrink");
        assert_eq!(outcome.progress.initial_cost, None, "{label}");
        let cost = outcome.progress.cost.expect("an arrival");
        assert_eq!(
            walk_cost(&outcome.input),
            (Some(outcome.input.actions.len() - 1), cost),
            "{label}"
        );
        assert_ne!(outcome.stop, ShrinkStop::NoArrival, "{label}");
        assert_eq!(
            outcome.improvements.first().map(|first| first.kind),
            Some(JobKind::Ordinary)
        );
        let stream = std::fs::read(config.stream.as_ref().unwrap()).unwrap();
        let replayed =
            replay_campaign_checkpointed(&Walk, &stream, None, None).expect("shrink stream replay");
        assert_eq!(replayed.0.incumbent_cost, Some(cost), "{label}");
        assert_eq!(
            replayed.0.objective_witness.as_ref(),
            Some(&outcome.input),
            "{label}"
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }
}

#[test]
fn a_staged_shrink_runs_one_campaign_per_stage_and_joins_their_tapes() {
    let (mut config, directory) = shrink_config("shrink-staged");
    config.run.staged = true;
    let tape = padded_tape();
    let outcome = shrink(&Walk, &config, &tape, |_| Ok(())).expect("shrink");
    let initial = walk_cost(&tape).1;
    let cost = outcome.progress.cost.expect("an arrival");
    assert_eq!(outcome.progress.initial_cost, Some(initial));
    assert_eq!(
        walk_cost(&outcome.input),
        (Some(outcome.input.actions.len() - 1), cost)
    );
    assert!(cost < initial);
    let stages: Vec<_> = outcome
        .improvements
        .iter()
        .map(|improvement| improvement.stage)
        .collect();
    assert!(stages.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(stages.iter().all(Option::is_some));
    assert!(
        outcome
            .improvements
            .windows(2)
            .all(|pair| pair[0].execution_work <= pair[1].execution_work)
    );
    for stage in 0..u32::from(GOAL / STAGE_LENGTH) {
        let stream = directory.join(format!("stream-stage-{stage}.jsonl"));
        let stream = std::fs::read(&stream).expect("a stream per stage");
        replay_campaign_checkpointed(&Walk, &stream, None, None).expect("stage stream replay");
    }
    assert!(!directory.join("stream.jsonl").exists());
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn a_staged_shrink_from_a_tape_that_dies_in_its_first_stage_runs_a_campaign_per_stage() {
    let (mut config, directory) = shrink_config("shrink-staged-dying");
    config.run.staged = true;
    let tape = Input {
        actions: vec![Step(1), Step(1), DEATH, Step(1)],
    };
    assert_eq!(walk_cost(&tape).0, None);
    let outcome = shrink(&Walk, &config, &tape, |_| Ok(())).expect("shrink");
    assert_eq!(outcome.progress.initial_cost, None);
    let cost = outcome.progress.cost.expect("an arrival");
    assert_eq!(
        walk_cost(&outcome.input),
        (Some(outcome.input.actions.len() - 1), cost)
    );
    let stages: std::collections::BTreeSet<_> = outcome
        .improvements
        .iter()
        .map(|improvement| improvement.stage)
        .collect();
    let all: std::collections::BTreeSet<_> =
        (0..u32::from(GOAL / STAGE_LENGTH)).map(Some).collect();
    assert_eq!(stages, all);
    for stage in 0..u32::from(GOAL / STAGE_LENGTH) {
        let stream = directory.join(format!("stream-stage-{stage}.jsonl"));
        let stream = std::fs::read(&stream).expect("a stream per stage");
        replay_campaign_checkpointed(&Walk, &stream, None, None).expect("stage stream replay");
    }
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn a_staged_shrink_whose_budget_runs_out_between_stages_returns_its_tape() {
    let tape = Input {
        actions: vec![Step(1); usize::from(GOAL)],
    };
    for budget in [1, 12] {
        let mut config = ShrinkConfig::new(
            WalkRun {
                staged: true,
                ..WHOLE
            },
            1,
        );
        config.work_budget = Some(budget);
        let outcome = shrink(&Walk, &config, &tape, |_| Ok(())).expect("shrink");
        assert!(outcome.progress.execution_work >= budget, "{budget}");
        let cost = outcome.progress.cost.expect("an arrival");
        assert!(cost <= walk_cost(&tape).1, "{budget}");
        assert_eq!(walk_cost(&outcome.input).1, cost, "{budget}");
    }
}

#[test]
fn a_staged_shrink_whose_stage_result_reaches_the_objective_ends_there() {
    let mut config = ShrinkConfig::new(
        WalkRun {
            staged: true,
            ..WHOLE
        },
        1,
    );
    config.work_budget = Some(64);
    let tape = Input {
        actions: vec![Step(GOAL)],
    };
    let outcome = shrink(&Walk, &config, &tape, |_| Ok(())).expect("shrink");
    assert_eq!(outcome.input, tape);
    assert_eq!(outcome.progress.cost, Some(walk_cost(&tape).1));
}

#[test]
fn a_staged_shrink_whose_objective_lies_before_its_next_stage_ends_at_the_objective() {
    let run = WalkRun {
        goal: MAX_STRIDE,
        staged: true,
    };
    let mut config = ShrinkConfig::new(run, 1);
    config.work_budget = Some(64);
    let tape = Input {
        actions: vec![Step(MAX_STRIDE), Step(1)],
    };
    let outcome = shrink(&Walk, &config, &tape, |_| Ok(())).expect("shrink");
    assert_eq!(outcome.progress.cost, Some(ShrinkCost::new(1, 0)));
}

#[test]
fn shrink_without_an_arrival_stops_with_no_arrival_and_returns_its_tape() {
    let mut config = ShrinkConfig::new(WHOLE, 1);
    let tape = Input {
        actions: vec![Step(1)],
    };
    config.work_budget = Some(1);
    let outcome = shrink(&Walk, &config, &tape, |_| Ok(())).expect("shrink");
    assert_eq!(outcome.stop, ShrinkStop::NoArrival);
    assert_eq!(outcome.input, tape);
    assert_eq!(outcome.progress.cost, None);
    assert_eq!(outcome.progress.initial_cost, None);
    assert!(outcome.improvements.is_empty());
}

#[test]
fn all_three_kinds_reach_at_least_the_shortcut_cost() {
    for budget in [8, 16, 32, 64, 160] {
        let shortcuts = run_walk(
            &minimize_config(false, 4, budget),
            &tape_origin(padded_tape(), 0),
            None,
        )
        .0;
        let (mixed, stream) = run_walk(
            &minimize_config(true, 4, budget),
            &tape_origin(padded_tape(), 0),
            None,
        );
        let progress = mixed
            .0
            .minimize_progress
            .as_ref()
            .expect("minimize progress");
        eprintln!(
            "budget {budget}: shortcuts {:?} after {} jobs; all kinds {:?} after {} jobs {progress:?}",
            shortcuts.0.incumbent_cost,
            shortcuts.0.executions_completed,
            mixed.0.incumbent_cost,
            mixed.0.executions_completed,
        );
        assert!(
            mixed.0.incumbent_cost <= shortcuts.0.incumbent_cost,
            "budget {budget}"
        );
        let replayed = replay_campaign_checkpointed(&Walk, &stream, None, None).expect("replay");
        assert_eq!(&replayed, &mixed);
    }
}

#[test]
fn an_ordinary_length_floors_ordinary_jobs_once_an_incumbent_exists() {
    let mut config = walk_config(2);
    config.minimize = Some(MinimizeConfig {
        ordinary_length: Some(8),
        ..MinimizeConfig::default()
    });
    let (outcome, stream) = run_walk(&config, &tape_origin(padded_tape(), 0), None);
    let ordinary = job_records(&stream)
        .into_iter()
        .filter(|record| record["shrink"]["kind"] == "ordinary")
        .collect::<Vec<_>>();
    assert!(!ordinary.is_empty());
    for record in &ordinary {
        if let Some(limit) = record["suffix_limit"].as_u64() {
            assert!(limit >= 8, "{record}");
        }
    }
    let replayed = replay_campaign_checkpointed(&Walk, &stream, None, None).expect("replay");
    assert_eq!(&replayed, &outcome);
    config.minimize = Some(MinimizeConfig {
        ordinary_length: Some(0),
        ..MinimizeConfig::default()
    });
    let mut rejected = Vec::new();
    let error = run_campaign_checkpointed_with_options(
        &Walk,
        &config,
        &tape_origin(padded_tape(), 0),
        &mut rejected,
        None,
        CampaignExecutionOptions::default(),
    )
    .expect_err("a zero ordinary length must be rejected");
    assert!(error.to_string().contains("ordinary length"), "{error}");
}

#[test]
fn a_minimize_campaign_stops_when_its_shortcut_passes_are_exhausted() {
    let mut config = walk_config(2);
    config.minimize = Some(MinimizeConfig {
        until_shortcuts_exhausted: true,
        ..MinimizeConfig::default()
    });
    let (outcome, stream) = run_walk(&config, &tape_origin(padded_tape(), 0), None);
    let report = &outcome.0;
    assert_eq!(report.shrink_stop, Some(ShrinkStop::ShortcutsExhausted));
    assert!(report.executions_completed < config.execution_budget);
    let replayed = replay_campaign_checkpointed(&Walk, &stream, None, None).expect("replay");
    assert_eq!(&replayed, &outcome);
}

#[test]
fn a_minimize_campaign_stops_at_its_root_lower_bound() {
    let config = walk_config(2);
    let (outcome, stream) = run_walk(&config, &tape_origin(padded_tape(), 0), None);
    let report = &outcome.0;
    assert_eq!(report.shrink_stop, Some(ShrinkStop::LowerBound));
    assert_eq!(
        report.incumbent_cost,
        Some(ShrinkCost::new(remaining_steps(0), 0))
    );
    assert!(report.executions_completed < config.execution_budget);
    let replayed = replay_campaign_checkpointed(&Walk, &stream, None, None).expect("replay");
    assert_eq!(&replayed, &outcome);
}

#[test]
fn replay_rejects_a_minimize_selection_the_rebuilt_core_did_not_make() {
    let config = walk_config(1);
    let (_, stream) = run_walk(&config, &tape_origin(padded_tape(), 0), None);
    let text = std::str::from_utf8(&stream).unwrap();
    for (field, kind) in [
        ("generation", "shortcut"),
        ("start", "shortcut"),
        ("shares", "ordinary"),
    ] {
        let mut tampered = false;
        let lines = text
            .lines()
            .map(|line| {
                let mut record: Value = serde_json::from_str(line).unwrap();
                if !tampered && record["event"] == "job" && record["shrink"]["kind"] == kind {
                    let shrink = &mut record["shrink"];
                    match field {
                        "generation" => shrink["generation"] = Value::from(99),
                        "start" => {
                            let start = shrink["shortcut"]["start"].as_u64().unwrap();
                            shrink["shortcut"]["start"] = Value::from(start + 1);
                        }
                        _ => shrink["shares"][0] = Value::from(1),
                    }
                    tampered = true;
                }
                serde_json::to_string(&record).unwrap()
            })
            .collect::<Vec<_>>();
        assert!(tampered, "{field}");
        let tampered_stream = format!("{}\n", lines.join("\n"));
        let error = replay_campaign_checkpointed(&Walk, tampered_stream.as_bytes(), None, None)
            .expect_err("a tampered minimize selection must not replay");
        assert!(
            error.to_string().contains("minimize selection"),
            "{field}: {error}"
        );
    }
}

#[test]
fn an_ordinary_campaign_replays_its_report_and_checkpoint_without_minimize_records() {
    let config = |workers| {
        let mut config = walk_config(workers);
        config.minimize = None;
        config.stop_campaign_on_objective = false;
        config
    };
    let (single, single_stream) = run_walk(&config(1), &CampaignOrigin::Genesis, None);
    let (parallel, parallel_stream) = run_walk(&config(4), &CampaignOrigin::Genesis, None);
    assert_eq!(single.0.executions_completed, 160);
    assert!(single.0.minimize.is_none());
    assert!(single.0.minimize_progress.is_none());
    assert!(single.0.shrink_stop.is_none());
    assert!(single.0.incumbent_cost.is_none());
    assert_eq!(
        String::from_utf8(single_stream.clone()).unwrap(),
        String::from_utf8(parallel_stream.clone()).unwrap()
    );
    assert_eq!(single.1, parallel.1);
    let records = job_records(&single_stream);
    assert_eq!(records.len(), 160);
    assert!(records.iter().all(|record| record["shrink"].is_null()
        && record["incumbent"].is_null()
        && record["ceiling"].is_null()
        && record["selector"]["path"] != "shrink"));
    let replayed = replay_campaign_checkpointed(&Walk, &single_stream, None, None)
        .expect("ordinary campaign replay");
    assert_eq!(
        serde_json::to_value(&replayed.0).unwrap(),
        serde_json::to_value(&single.0).unwrap()
    );
    assert_eq!(replayed.1, single.1);
}

#[test]
fn a_window_search_that_drops_states_never_proves_more_than_the_shortest_walk() {
    let shortest = ShrinkCost::new(u64::from(GOAL / 2), 0);
    let incumbent = Input {
        actions: vec![Step(1); usize::from(GOAL)],
    };
    for frontier_limit in [1, 2, 3, 64] {
        let config = WindowConfig {
            start: 0,
            expansion_budget: None,
            frontier_limit,
        };
        let outcome =
            search_window(&Walk, &WHOLE, &incumbent, &config, |_| Ok(())).expect("window search");
        assert!(
            outcome.progress.lower_bound <= shortest,
            "frontier {frontier_limit} proved {:?}",
            outcome.progress.lower_bound
        );
        if outcome.stop == WindowStop::Complete {
            assert_eq!(outcome.progress.cost, shortest, "frontier {frontier_limit}");
        }
        if frontier_limit == 64 {
            assert_eq!(outcome.stop, WindowStop::Complete);
            assert_eq!(outcome.progress.cost, shortest);
        }
    }
}
