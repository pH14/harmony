// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeSet, error::Error, num::NonZeroU64};

use searcher::search::{
    archive::{
        ArchiveEntryReport, ArchiveKey, Input, RetentionPolicy, SelectorPolicy, entries_by_suffix,
    },
    campaign::{
        ArchiveReportState, CampaignActionResult, CampaignConfig, CampaignExecutionOptions,
        CampaignJobResult, CampaignOrigin, CampaignTypes, Evaluation, InitialDrawState,
        InputPolicy, Reporting, ResultBuffering, TargetExecution, WorkloadPolicies,
        postcard_value_sha256, replay_campaign_checkpointed,
        run_campaign_checkpointed_with_options,
    },
    draw::{DrawMixture, MixtureDraw, SuffixShape},
    duration::{DurationDraw, DurationRequest},
    rollout::ExecutionDisposition,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct TimedAction {
    context: u8,
    duration: NonZeroU64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct TimingKey(u8);

impl ArchiveKey for TimingKey {
    type Group = u8;

    fn groups() -> usize {
        1
    }

    fn group(self, depth: usize) -> Self::Group {
        assert_eq!(depth, 0);
        self.0
    }

    type Lineage = ();

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct TimingArchiveReport {
    #[serde(with = "entries_by_suffix")]
    entries: Vec<ArchiveEntryReport<TimedAction, TimingKey, ()>>,
}

#[derive(Default)]
struct TimingTarget {
    value: u8,
    execution_work: u64,
    failed: bool,
}

struct TimingWorkload {
    fail_timed_action: bool,
}

impl CampaignTypes for TimingWorkload {
    type Target = TimingTarget;
    type Action = TimedAction;
    type Key = TimingKey;
    type Milestones = ();
    type Progress = ();
    type Snapshot = u8;
    type Observations = (u8,);
    type Evidence = ();
    type ArchiveReport = TimingArchiveReport;
    type Run = ();
    type DrawState = ();
    type DrawCheckpoint = ();
    type DrawHeader = ();
}

impl Reporting for TimingWorkload {
    fn stream_format(&self) -> &'static str {
        "adaptive-campaign-fixture-v1"
    }

    fn checkpoint_format(&self) -> &'static str {
        "adaptive-campaign-checkpoint-v1"
    }

    fn workload_identity_sha256(&self) -> String {
        "adaptive-campaign-fixture".to_owned()
    }

    fn action_cost_unit(&self) -> &'static str {
        "fixture_ticks"
    }

    fn execution_work_unit(&self) -> &'static str {
        "fixture_ticks"
    }

    fn result_sha256(&self, result: &CampaignJobResult<Self>) -> Result<String, Box<dyn Error>> {
        postcard_value_sha256(result)
    }

    fn archive_report(
        &self,
        _evidence: &Self::Evidence,
        state: ArchiveReportState<Self>,
    ) -> Self::ArchiveReport {
        TimingArchiveReport {
            entries: state.entries,
        }
    }
}

impl InputPolicy for TimingWorkload {
    fn max_action_limit(&self) -> usize {
        16
    }

    fn max_action_cost(&self) -> u64 {
        64
    }

    fn policies(&self, _run: &Self::Run) -> WorkloadPolicies {
        [("duration_policy".to_owned(), "fixture-v1".to_owned())]
            .into_iter()
            .collect()
    }

    fn resolve_recorded(&self, _policies: &WorkloadPolicies) -> Result<Self::Run, Box<dyn Error>> {
        Ok(())
    }

    fn draw_state_memory_reserve_bytes(&self, _run: &Self::Run, _max_actions: usize) -> usize {
        0
    }

    fn draw_state_memory_bytes(&self, _state: &Self::DrawState) -> usize {
        0
    }

    fn initial_draw_state(
        &self,
        _run: &Self::Run,
        _origin: Option<(&str, &Self::ArchiveReport)>,
    ) -> Result<InitialDrawState<Self>, Box<dyn Error>> {
        Ok(((), None))
    }

    fn duration_request(
        &self,
        _run: &Self::Run,
        parent: Self::Key,
        remaining_work: Option<NonZeroU64>,
    ) -> Option<DurationRequest<Self::Key>> {
        let context = TimingKey(parent.0 % 2);
        let base = if context.0 == 0 { 2 } else { 32 };
        let maximum = remaining_work.map_or(base, |remaining| base.min(remaining.get()));
        Some(DurationRequest {
            context,
            max_duration: NonZeroU64::new(maximum)?,
        })
    }

    fn expand_suffix(
        &self,
        _run: &Self::Run,
        _state: &Self::DrawState,
        _shape: SuffixShape,
        _mixture: MixtureDraw,
        _mutation_seed: u64,
    ) -> Result<Vec<Self::Action>, Box<dyn Error>> {
        Ok(vec![TimedAction {
            context: 0,
            duration: NonZeroU64::MIN,
        }])
    }

    fn expand_suffix_duration(
        &self,
        _run: &Self::Run,
        _state: &Self::DrawState,
        _shape: SuffixShape,
        _mixture: MixtureDraw,
        draw_seed: u64,
        draw: DurationDraw<Self::Key>,
    ) -> Result<Vec<Self::Action>, Box<dyn Error>> {
        let _ = draw_seed;
        if self.fail_timed_action {
            return Ok(vec![
                TimedAction {
                    context: 2,
                    duration: NonZeroU64::MIN,
                },
                TimedAction {
                    context: 3,
                    duration: draw.duration,
                },
            ]);
        }
        Ok(vec![TimedAction {
            context: draw.context.0,
            duration: draw.duration,
        }])
    }

    fn duration_of_action(&self, _run: &Self::Run, action: &Self::Action) -> Option<NonZeroU64> {
        if self.fail_timed_action {
            (action.context == 3).then_some(action.duration)
        } else {
            Some(action.duration)
        }
    }
}

impl TargetExecution for TimingWorkload {
    fn new_target(&self) -> Result<Self::Target, String> {
        Ok(TimingTarget::default())
    }

    fn reset(&self, target: &mut Self::Target) {
        target.value = 0;
        target.failed = false;
    }

    fn restore(
        &self,
        target: &mut Self::Target,
        snapshot: &Self::Snapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.value = *snapshot;
        target.failed = false;
        Ok(())
    }

    fn execution_work(&self, target: &Self::Target) -> u64 {
        target.execution_work
    }

    fn action_cost_fn(&self) -> fn(&Self::Action) -> u64 {
        |action| action.duration.get()
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
        if self.fail_timed_action && action.context == 3 {
            target.failed = true;
            return Ok(());
        }
        target.value = target.value.wrapping_add(1);
        target.execution_work = target.execution_work.saturating_add(action.duration.get());
        Ok(())
    }

    fn rollout_observations(&self, target: &Self::Target) -> Vec<Self::Observations> {
        vec![(target.value,)]
    }

    fn snapshot(&self, target: &mut Self::Target) -> Result<Self::Snapshot, Box<dyn Error>> {
        Ok(target.value)
    }
}

impl Evaluation for TimingWorkload {
    fn execution_disposition(&self, target: &Self::Target) -> ExecutionDisposition {
        if target.failed {
            ExecutionDisposition::Failed
        } else {
            ExecutionDisposition::Runnable
        }
    }

    fn objective_reached(
        &self,
        _run: &Self::Run,
        _target: &Self::Target,
    ) -> Result<bool, Box<dyn Error>> {
        Ok(false)
    }

    fn current_key(&self, target: &Self::Target) -> Result<Self::Key, Box<dyn Error>> {
        Ok(TimingKey(target.value))
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
        Err("fixture archive resume is not used".into())
    }
}

fn fixture_config() -> CampaignConfig<TimingWorkload> {
    CampaignConfig {
        campaign_seed: 0x51a7_e001,
        workers: 2,
        execution_budget: 64,
        action_limit: 8,
        host: "fixture".to_owned(),
        wall_budget: None,
        stop_rollout_on_objective: false,
        stop_campaign_on_objective: false,
        archive_entry_limit: 256,
        reservations_per_worker: 2,
        memory_budget_mib: None,
        materialize_final_artifacts: true,
        run: (),
        suffix: SuffixShape::OneOrTwo,
        mixture: DrawMixture::AlphabetOnly,
        retention: RetentionPolicy::Unprobed,
        selector: SelectorPolicy::GroupUniform,
        objective_witness_path: None,
    }
}

fn duration_job_values(stream: &[u8]) -> Vec<Value> {
    std::str::from_utf8(stream)
        .expect("campaign stream is UTF-8")
        .lines()
        .skip(1)
        .map(|line| serde_json::from_str(line).expect("campaign record JSON"))
        .filter(|record: &Value| {
            record.get("event").and_then(Value::as_str) == Some("job")
                && record.get("duration_draw").is_some()
        })
        .collect()
}

fn mutate_first_duration_job<F>(stream: &[u8], mutate: F) -> Vec<u8>
where
    F: FnOnce(&mut Value),
{
    let mut mutate = Some(mutate);
    let mut changed = false;
    let lines = std::str::from_utf8(stream)
        .expect("campaign stream is UTF-8")
        .lines()
        .enumerate()
        .map(|(line_number, line)| {
            if line_number == 0 {
                return line.to_owned();
            }
            let mut record: Value = serde_json::from_str(line).expect("campaign record JSON");
            if !changed
                && record.get("event").and_then(Value::as_str) == Some("job")
                && record.get("duration_draw").is_some()
            {
                mutate.take().expect("one mutation")(&mut record);
                changed = true;
            }
            serde_json::to_string(&record).expect("campaign record encoding")
        })
        .collect::<Vec<_>>();
    assert!(changed, "fixture campaign did not record an adaptive job");
    format!("{}\n", lines.join("\n")).into_bytes()
}

fn fixture_stream(workload: &TimingWorkload) -> Vec<u8> {
    let mut stream = Vec::new();
    run_campaign_checkpointed_with_options(
        workload,
        &fixture_config(),
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
        CampaignExecutionOptions {
            work_budget: Some(4_096),
            result_buffering: ResultBuffering::TwoPerWorker,
        },
    )
    .expect("adaptive campaign");
    stream
}

#[test]
fn campaign_adaptive_duration_replays_concurrently_and_rejects_tampering() {
    let workload = TimingWorkload {
        fail_timed_action: false,
    };
    let config = fixture_config();
    let mut stream = Vec::new();
    let live = run_campaign_checkpointed_with_options(
        &workload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
        CampaignExecutionOptions {
            work_budget: Some(4_096),
            result_buffering: ResultBuffering::TwoPerWorker,
        },
    )
    .expect("adaptive campaign");

    assert_eq!(live.0.workers, 2);
    assert!(live.0.executions_completed >= 8);
    assert!(live.0.jobs_per_worker.iter().all(|jobs| *jobs > 0));
    let jobs = duration_job_values(&stream);
    assert!(jobs.len() >= 4, "expected several adaptive jobs");
    let contexts: BTreeSet<u8> = jobs
        .iter()
        .filter_map(|job| job["duration_draw"]["context"].as_u64())
        .map(|context| u8::try_from(context).expect("fixture context fits"))
        .collect();
    assert!(contexts.contains(&0));
    assert!(contexts.contains(&1));
    let maximums: BTreeSet<u64> = jobs
        .iter()
        .filter_map(|job| job["duration_draw"]["max_duration"].as_u64())
        .collect();
    assert!(maximums.contains(&2));
    assert!(maximums.contains(&32));
    assert!(jobs.iter().any(|job| {
        job["duration_checkpoint_after"]["recent"]
            .as_array()
            .is_some_and(|history| !history.is_empty())
    }));
    assert!(jobs.iter().any(|job| {
        job["duration_remaining_work"]
            .as_u64()
            .is_some_and(|remaining| remaining > 0)
    }));

    let replayed = replay_campaign_checkpointed(&workload, &stream, None, None)
        .expect("adaptive campaign replay");
    assert_eq!(replayed, live);

    let draw_tampered = mutate_first_duration_job(&stream, |record| {
        let duration = record["duration_draw"]["duration"]
            .as_u64()
            .expect("recorded duration");
        let maximum = record["duration_draw"]["max_duration"]
            .as_u64()
            .expect("recorded maximum");
        let replacement = if duration == 1 { 2 } else { 1 };
        assert!(replacement <= maximum);
        record["duration_draw"]["duration"] = serde_json::json!(replacement);
    });
    assert!(
        replay_campaign_checkpointed(&workload, &draw_tampered, None, None).is_err(),
        "replay accepted a changed duration draw"
    );

    let checkpoint_tampered = mutate_first_duration_job(&stream, |record| {
        assert!(record["duration_checkpoint_after"].is_object());
        record["duration_checkpoint_after"]["policy"] = serde_json::json!("tampered");
    });
    assert!(
        replay_campaign_checkpointed(&workload, &checkpoint_tampered, None, None).is_err(),
        "replay accepted a changed duration checkpoint"
    );

    let remaining_tampered = mutate_first_duration_job(&stream, |record| {
        record["duration_remaining_work"] = serde_json::json!(1);
    });
    assert!(
        replay_campaign_checkpointed(&workload, &remaining_tampered, None, None).is_err(),
        "replay accepted a changed remaining-work input"
    );
}

#[test]
fn replay_rejects_fabricated_duration_history_at_reservation() {
    let workload = TimingWorkload {
        fail_timed_action: false,
    };
    let stream = fixture_stream(&workload);
    let tampered = mutate_first_duration_job(&stream, |record| {
        assert!(record.get("duration_checkpoint_at_draw").is_none());
        record["duration_checkpoint_at_draw"] = serde_json::json!({
            "policy": "recent_useful_work_log_duration_v1",
            "recent": [{"duration": 1, "useful": false, "execution_cost": 1}]
        });
    });
    assert!(
        replay_campaign_checkpointed(&workload, &tampered, None, None).is_err(),
        "replay accepted a fabricated observation before the first job was reserved"
    );
}

#[test]
fn replay_rejects_remaining_work_that_does_not_match_reservation() {
    let workload = TimingWorkload {
        fail_timed_action: false,
    };
    let stream = fixture_stream(&workload);
    let tampered = mutate_first_duration_job(&stream, |record| {
        assert_eq!(record["duration_remaining_work"], serde_json::json!(4_096));
        record["duration_remaining_work"] = serde_json::json!(4_095);
    });
    assert!(
        replay_campaign_checkpointed(&workload, &tampered, None, None).is_err(),
        "replay accepted remaining work that diverged from its reservation"
    );
}

#[test]
fn failed_duration_actions_do_not_train_the_policy() {
    let workload = TimingWorkload {
        fail_timed_action: true,
    };
    let mut stream = Vec::new();
    let live = run_campaign_checkpointed_with_options(
        &workload,
        &fixture_config(),
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
        CampaignExecutionOptions {
            work_budget: Some(4_096),
            result_buffering: ResultBuffering::TwoPerWorker,
        },
    )
    .expect("failed-duration campaign");
    assert!(live.0.execution_failures > 0);
    let jobs = duration_job_values(&stream);
    assert!(
        !jobs.is_empty(),
        "failed fixture did not record adaptive jobs"
    );
    assert!(
        jobs.iter()
            .all(|job| job.get("duration_checkpoint_after").is_none()),
        "failed timed actions generated duration learning observations"
    );
    let replayed = replay_campaign_checkpointed(&workload, &stream, None, None)
        .expect("failed-duration campaign replay");
    assert_eq!(replayed, live);
}
