// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque},
    error::Error,
    fmt::Debug,
    hash::Hash,
    io::{BufReader, Read, Write},
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::search::archive::{
    Archive, ArchiveCandidate, ArchiveEntryReport, ArchiveKey, ContinuationAccounting, Edge, Input,
    Position, ProgressPoint, RetentionPolicy, SELECTOR_IDENTIFIER, SelectorAccounting,
    SelectorDraw, SelectorPath, SnapshotRetention, retention_policy_from_identifier,
    retention_policy_identifier,
};
use crate::search::checkpoint::{
    CheckpointHeader, CheckpointPlan, CheckpointReader, SEARCH_CHECKPOINT_FORMAT,
};
use crate::search::continuation::CONTINUATION_IDENTIFIER;
use crate::search::draw::{
    DrawMixture, EnergyStrategy, MixtureDraw, MixtureEnergy, SUFFIX_DOUBLING_LIMIT,
    draw_mixture_from_identifier, draw_mixture_identifier, draw_suffix, energy_strategy,
};
use crate::search::draw_tables::{
    DEFAULT_DRAW_TABLE_PARAMETERS, DRAW_TABLE_POLICY_FIELD, DrawTableHeader, DrawTables,
    DrawVersionSchedule, biased_step, draw_table_from_identifier, draw_table_identifier,
};
use crate::search::duration::{
    DurationCheckpoint, DurationContextCheckpoint, DurationDraw, DurationPolicies, DurationPolicy,
    DurationRequest,
};
use crate::search::empirical_steps::{EmpiricalStepCheckpoint, EmpiricalStepParameters};

pub const LONGEST_SPLICE_TAIL: usize = 128;
use crate::search::parallel::{WorkerPoolError, with_worker_pool_routed};
use crate::search::rand::RomuDuoJrRand;
use crate::search::telemetry::{
    CampaignTelemetry, CompletedBufferTelemetry, HostTimes, Moment, TargetCounters, now,
    unix_seconds,
};

#[path = "campaign_coordinator.rs"]
mod coordinator;

pub use coordinator::{CampaignAdmission, CampaignCoordinator, JobOutcome, JobSpec, run_job};

#[path = "campaign_expansion.rs"]
mod expansion_campaign;

pub use expansion_campaign::{
    EXPANSION_SCHEDULE, EXPANSION_STREAM_FORMAT, ExpansionAccountingRecord, ExpansionAdmission,
    ExpansionCoordinator, ExpansionCounters, ExpansionDecision, ExpansionDuplicateRecord,
    ExpansionEdge, ExpansionEdgeAdmission, ExpansionEdgeId, ExpansionEvidence,
    ExpansionEvidenceData, ExpansionEvidenceRecord, ExpansionFacts, ExpansionNodeRecord,
    ExpansionObservations, ExpansionOutcome, ExpansionReservationRecord, ExpansionRunReport,
    ExpansionStopReason, ExpansionStreamHeader, ExpansionStreamRecord, ExpansionTaskOutcome,
    ExpansionTaskRecord, ExpansionTaskSpec, ExpansionTrace, ExpansionWatch, ExpansionWorkload,
    WorkloadExpansionFacts, replay_expansion, run_expansion_task, run_expansion_watched,
};

#[path = "campaign_view.rs"]
mod view;

pub use view::CampaignView;

pub type WatchedJob<G, N> = Result<(JobOutcome<G>, N), Box<dyn Error>>;

pub trait CampaignWatch<G: Workload>: Sync {
    type Note: Send;

    fn run(
        &self,
        job: &JobSpec<G>,
        run: impl FnOnce() -> Result<JobOutcome<G>, Box<dyn Error>>,
    ) -> WatchedJob<G, Self::Note>;

    fn prepare_admission(&self, _note: &Self::Note, _campaign: &mut CampaignCoordinator<'_, G>) {}

    fn admitted(&self, note: Self::Note, campaign: &mut CampaignCoordinator<'_, G>);

    fn start(&self, _campaign: &mut CampaignCoordinator<'_, G>) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
}

impl<G: Workload> CampaignWatch<G> for () {
    type Note = ();

    fn run(
        &self,
        _job: &JobSpec<G>,
        run: impl FnOnce() -> Result<JobOutcome<G>, Box<dyn Error>>,
    ) -> WatchedJob<G, ()> {
        Ok((run()?, ()))
    }

    fn admitted(&self, _note: (), _campaign: &mut CampaignCoordinator<'_, G>) {}
}

pub type CampaignOutcome<G> = (
    CampaignModeReport<<G as CampaignTypes>::Action, <G as CampaignTypes>::ArchiveReport>,
    SnapshotCheckpoint<<G as CampaignTypes>::Snapshot>,
);

pub type InitialDrawState<G> = (
    DrawTables<<G as CampaignTypes>::Action>,
    Option<DrawTableHeader>,
);

pub const CAMPAIGN_SCHEMA_VERSION: u32 = 15;

pub const CAMPAIGN_SCHEDULE_IDENTITY: &str = "jobs are selected into a deterministic sliding \
     window; each job's planned finish is the planned finish of the admission before its \
     selection plus its planned action cost, and jobs are admitted in planned-finish order; \
     physical workers run queued jobs in that order, but host completion order cannot reach \
     campaign state; the same seed, configuration, origin, and workload bytes produce the same \
     recorded stream";

const CAMPAIGN_PROGRESS_POLICY: &str = "mechanical_watermark_bounded_1024_v2";
const EXECUTOR_MODE: &str = "snapshot_resume_archive";
const SELECTION_SEED_DERIVATION: &str = "sha256(campaign_seed_le)[0..8] as u64 le";
const ORIGIN_GENESIS: &str = "genesis";
const ORIGIN_SNAPSHOT_ROOT: &str = "snapshot_root";
const ORIGIN_ARCHIVE: &str = "archive";
const ORIGIN_SEARCH_CHECKPOINT: &str = "search_checkpoint";

#[derive(Clone, Debug, Default)]
pub struct CampaignExecutionOptions {
    pub work_budget: Option<u64>,
    pub checkpoints: Option<CheckpointPlan>,
    pub placement: Option<ThreadPlacement>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlacedThread {
    Coordinator,
    StreamEncoder,
    Worker(u32),
}

type PlaceThread = dyn Fn(PlacedThread) -> Result<(), String> + Send + Sync;

#[derive(Clone)]
pub struct ThreadPlacement(Arc<PlaceThread>);

impl ThreadPlacement {
    pub fn new(place: impl Fn(PlacedThread) -> Result<(), String> + Send + Sync + 'static) -> Self {
        Self(Arc::new(place))
    }

    fn place(placement: Option<&Self>, thread: PlacedThread) -> Result<(), String> {
        placement.map_or(Ok(()), |placement| (placement.0)(thread))
    }
}

impl Debug for ThreadPlacement {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ThreadPlacement")
    }
}

#[must_use]
pub const fn default_window(workers: u32) -> usize {
    workers as usize
}

const SCHEDULE_POLICY_WINDOW_SUFFIX: &str = "_v6";
const SCHEDULE_POLICY_WINDOW_PREFIX: &str = "planned_finish_window_";
const DEFAULT_MIXTURE_WEIGHT: u8 = 128;

fn schedule_policy_identifier(window: usize) -> String {
    format!("{SCHEDULE_POLICY_WINDOW_PREFIX}{window}{SCHEDULE_POLICY_WINDOW_SUFFIX}")
}

fn schedule_policy_window(policy: &str) -> Option<usize> {
    let window = policy.strip_prefix(SCHEDULE_POLICY_WINDOW_PREFIX)?;
    let window = window.strip_suffix(SCHEDULE_POLICY_WINDOW_SUFFIX)?;
    window.parse().ok().filter(|window| *window >= 1)
}

fn schedule_policy_is_supported(policy: &str) -> bool {
    schedule_policy_window(policy).is_some()
}

const CONSECUTIVE_SKIP_LIMIT: u64 = 1_024;

const CURVE_INTERVAL: u64 = 100;

const PROGRESS_CHECKPOINT_INTERVAL: u64 = 100;

const MAX_PROGRESS_CURVE_POINTS: usize = 1_024;

fn compact_progress_curve<M, P>(
    curve: &mut Vec<ProgressPoint<M, P>>,
    current_interval: u64,
) -> u64 {
    if curve.len() < MAX_PROGRESS_CURVE_POINTS {
        return current_interval;
    }
    let next_interval = current_interval.saturating_mul(2);
    if next_interval == current_interval {
        return current_interval;
    }
    curve.retain(|point| point.executions.is_multiple_of(next_interval));
    next_interval
}

fn progress_line_interval(executions: u64) -> u64 {
    let span = PROGRESS_CHECKPOINT_INTERVAL.saturating_mul(MAX_PROGRESS_CURVE_POINTS as u64);
    let doublings = (executions / span).checked_ilog2().unwrap_or(0);
    PROGRESS_CHECKPOINT_INTERVAL.saturating_mul(1 << doublings)
}

fn progress_checkpoint_due(executions: u64) -> bool {
    executions > 0
        && (executions == 1 || executions.is_multiple_of(progress_line_interval(executions)))
}

fn full_selector_report_due(executions: u64) -> bool {
    executions > 0
        && executions.is_multiple_of(
            progress_line_interval(executions).saturating_mul(MAX_PROGRESS_CURVE_POINTS as u64),
        )
}

pub const RESUME_IDENTIFIER: &str = "whole_tree";

pub const SNAPSHOT_ROOT_RESUME_IDENTIFIER: &str = "snapshot_root";

pub type WorkloadPolicies = BTreeMap<String, String>;

pub trait CampaignTypes: Sync {
    type JobObservation: Copy + Send + 'static;
    type Target;
    type Action: Copy + Ord + Debug + Eq + Send + Sync + Serialize + DeserializeOwned;
    type Key: ArchiveKey + Debug + Eq + Send + Sync + 'static;
    type Milestones: Copy + Default + Debug + Eq + Send + Sync + Serialize + DeserializeOwned;
    type Progress: Copy + Default + Debug + Eq + Send + Sync + Serialize + DeserializeOwned;
    type Snapshot: Clone
        + Debug
        + Eq
        + Send
        + Sync
        + Serialize
        + DeserializeOwned
        + super::snapshot_codec::CheckpointSnapshot
        + 'static;
    type Observations: Clone + Debug + Eq + Send + Sync + Serialize + DeserializeOwned;
    type Evidence: Clone + Default;
    type ArchiveReport: Clone;
    type Run: Clone + Sync;
}

pub trait Reporting: CampaignTypes {
    fn diagnostics(_evidence: &Self::Evidence) -> Option<serde_json::Value> {
        None
    }
    fn retained_diagnostics<'a>(
        _snapshots: impl Iterator<Item = (Option<&'a Self::Snapshot>, u64)>,
    ) -> Option<serde_json::Value>
    where
        Self::Snapshot: 'a,
    {
        None
    }
    fn merge_witness_diagnostics(
        _evidence: &mut Self::Evidence,
        _observations: &[Self::Observations],
        _sequence: u64,
    ) {
    }
    fn write_evidence_checkpoint(
        _evidence: &Self::Evidence,
        _out: &mut dyn Write,
    ) -> Result<(), Box<dyn Error>> {
        Err("this workload cannot write its evidence into a search checkpoint".into())
    }
    fn evidence_checkpoint(evidence: &Self::Evidence) -> Result<Vec<u8>, Box<dyn Error>> {
        let mut bytes = Vec::new();
        Self::write_evidence_checkpoint(evidence, &mut bytes)?;
        Ok(bytes)
    }
    fn evidence_from_checkpoint(_bytes: &[u8]) -> Result<Self::Evidence, Box<dyn Error>> {
        Err("this workload cannot read its evidence from a search checkpoint".into())
    }
    fn checkpoint_marks(_evidence: &Self::Evidence) -> usize {
        0
    }
    fn stream_format(&self) -> &'static str;
    fn checkpoint_format(&self) -> &'static str;
    fn workload_identity_sha256(&self) -> String;
    fn executable_identity(&self) -> Result<String, Box<dyn Error>> {
        Ok(crate::search::executable_identity::loaded_executable_identity_self()?)
    }
    fn action_cost_unit(&self) -> &'static str;
    fn execution_work_unit(&self) -> &'static str;
    fn result_sha256(&self, result: &CampaignJobResult<Self>) -> Result<String, Box<dyn Error>>;
    fn certified_result_sha256(
        &self,
        result: &CampaignJobResult<Self>,
        _target: &Self::Target,
    ) -> Result<String, Box<dyn Error>> {
        self.result_sha256(result)
    }
    fn snapshot_identity_sha256(&self, snapshot: &Self::Snapshot)
    -> Result<String, Box<dyn Error>>;
    fn archive_report(
        &self,
        evidence: &Self::Evidence,
        state: ArchiveReportState<Self>,
    ) -> Self::ArchiveReport;
}

pub trait InputPolicy: CampaignTypes {
    fn policies(&self, run: &Self::Run) -> WorkloadPolicies;
    fn resolve_recorded(&self, policies: &WorkloadPolicies) -> Result<Self::Run, Box<dyn Error>>;
    fn sample_alphabet(
        &self,
        run: &Self::Run,
        previous: Option<&Self::Action>,
        rand: &mut RomuDuoJrRand,
    ) -> Result<Self::Action, Box<dyn Error>>;

    fn draw_table_parameters(&self, run: &Self::Run) -> EmpiricalStepParameters {
        let _ = run;
        DEFAULT_DRAW_TABLE_PARAMETERS
    }
    fn draw_state_memory_reserve_bytes(&self, run: &Self::Run) -> usize {
        let _ = run;
        DrawTables::<Self::Action>::memory_reserve_bytes()
    }
    fn draw_state_memory_bytes(&self, state: &DrawTables<Self::Action>) -> usize {
        state.memory_bytes()
    }
    fn duration_request(
        &self,
        run: &Self::Run,
        parent: Self::Key,
        remaining_work: Option<NonZeroU64>,
    ) -> Option<DurationRequest<Self::Key>> {
        let _ = (run, parent, remaining_work);
        None
    }
    fn expand_suffix_duration(
        &self,
        run: &Self::Run,
        state: &DrawTables<Self::Action>,
        mixture: MixtureDraw,
        mutation_seed: u64,
        previous: Option<&Self::Action>,
        draw: DurationDraw<Self::Key>,
    ) -> Result<Vec<Self::Action>, Box<dyn Error>> {
        self.expand_duration_recorded_or_live(
            run,
            state,
            mixture,
            None,
            mutation_seed,
            previous,
            draw,
            false,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn expand_suffix_recorded_duration(
        &self,
        run: &Self::Run,
        state: &DrawTables<Self::Action>,
        mixture: MixtureDraw,
        before: Option<&EmpiricalStepCheckpoint>,
        mutation_seed: u64,
        previous: Option<&Self::Action>,
        draw: Option<DurationDraw<Self::Key>>,
    ) -> Result<Vec<Self::Action>, Box<dyn Error>> {
        match draw {
            Some(draw) => self.expand_duration_recorded_or_live(
                run,
                state,
                mixture,
                before,
                mutation_seed,
                previous,
                draw,
                true,
            ),
            None => {
                self.expand_suffix_recorded(run, state, mixture, before, mutation_seed, previous)
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn expand_duration_recorded_or_live(
        &self,
        run: &Self::Run,
        state: &DrawTables<Self::Action>,
        mixture: MixtureDraw,
        before: Option<&EmpiricalStepCheckpoint>,
        mutation_seed: u64,
        previous: Option<&Self::Action>,
        draw: DurationDraw<Self::Key>,
        replay: bool,
    ) -> Result<Vec<Self::Action>, Box<dyn Error>> {
        let _ = draw;
        self.expand_recorded_or_live(run, state, mixture, before, mutation_seed, previous, replay)
    }
    fn duration_of_action(&self, run: &Self::Run, action: &Self::Action) -> Option<NonZeroU64> {
        let _ = (run, action);
        None
    }
    fn draw_checkpoint(
        &self,
        state: &DrawTables<Self::Action>,
    ) -> Result<Option<EmpiricalStepCheckpoint>, Box<dyn Error>> {
        state.checkpoint().map(Some)
    }
    fn draw_checkpoint_version(&self, checkpoint: &EmpiricalStepCheckpoint) -> u64 {
        checkpoint.records
    }
    fn expand_suffix(
        &self,
        run: &Self::Run,
        state: &DrawTables<Self::Action>,
        mixture: MixtureDraw,
        mutation_seed: u64,
        previous: Option<&Self::Action>,
    ) -> Result<Vec<Self::Action>, Box<dyn Error>> {
        self.expand_recorded_or_live(run, state, mixture, None, mutation_seed, previous, false)
    }
    fn expand_suffix_prefix(
        &self,
        run: &Self::Run,
        state: &DrawTables<Self::Action>,
        mixture: MixtureDraw,
        mutation_seed: u64,
        previous: Option<&Self::Action>,
        limit: u8,
    ) -> Result<Vec<Self::Action>, Box<dyn Error>> {
        let mut suffix = self.expand_suffix(run, state, mixture, mutation_seed, previous)?;
        suffix.truncate(usize::from(limit));
        Ok(suffix)
    }
    fn expand_suffix_recorded(
        &self,
        run: &Self::Run,
        state: &DrawTables<Self::Action>,
        mixture: MixtureDraw,
        before: Option<&EmpiricalStepCheckpoint>,
        mutation_seed: u64,
        previous: Option<&Self::Action>,
    ) -> Result<Vec<Self::Action>, Box<dyn Error>> {
        self.expand_recorded_or_live(run, state, mixture, before, mutation_seed, previous, true)
    }
    #[allow(clippy::too_many_arguments)]
    fn expand_recorded_or_live(
        &self,
        run: &Self::Run,
        state: &DrawTables<Self::Action>,
        mixture: MixtureDraw,
        before: Option<&EmpiricalStepCheckpoint>,
        mutation_seed: u64,
        previous: Option<&Self::Action>,
        replay: bool,
    ) -> Result<Vec<Self::Action>, Box<dyn Error>> {
        state.draw(before, replay, |view| {
            draw_suffix(
                mixture.mixture,
                mixture.weight,
                mutation_seed,
                previous,
                |rand| biased_step(view, rand),
                |previous, rand| self.sample_alphabet(run, previous, rand),
            )
        })
    }
    fn finish_stream_record(
        &self,
        run: &Self::Run,
        state: &mut DrawTables<Self::Action>,
        retained: &[(usize, &[Self::Action])],
        evidence: &Self::Evidence,
    ) -> Result<Option<EmpiricalStepCheckpoint>, Box<dyn Error>> {
        let _ = (run, evidence);
        state.finish_record(retained)
    }
    fn remember_draw_version(
        &self,
        state: &mut DrawTables<Self::Action>,
        schedule: &DrawVersionSchedule,
    ) -> Result<(), Box<dyn Error>> {
        state.remember_version(schedule)
    }
}

pub trait TargetExecution: CampaignTypes {
    fn worker_locality(&self, _target: &Self::Target) -> crate::search::locality::WorkerLocality {
        crate::search::locality::WorkerLocality::default()
    }
    fn snapshot_locality(
        &self,
        _snapshot: &Self::Snapshot,
    ) -> Option<crate::search::locality::LocalityKey> {
        None
    }
    fn begin_job_observation(&self) {}
    fn take_job_observation(&self) -> Option<Self::JobObservation> {
        None
    }
    fn verify_integrity(&self) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    fn new_target(&self) -> Result<Self::Target, String>;
    fn begin_coordinator_session(&self) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn finish_coordinator_session(&self) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn reset(&self, target: &mut Self::Target);
    fn restore(
        &self,
        target: &mut Self::Target,
        snapshot: &Self::Snapshot,
    ) -> Result<(), Box<dyn Error>>;
    fn execution_work(&self, target: &Self::Target) -> u64;
    fn telemetry(&self, _target: &Self::Target) -> TargetCounters {
        TargetCounters::new()
    }
    fn action_cost_fn(&self) -> fn(&Self::Action) -> u64;
    fn snapshot_memory_charge(snapshot: &Self::Snapshot) -> usize;
    fn snapshot_retention() -> Option<Box<dyn SnapshotRetention<Self::Snapshot>>> {
        None
    }
    fn apply_action(
        &self,
        target: &mut Self::Target,
        action: &Self::Action,
        milestones: &mut Self::Milestones,
    ) -> Result<(), Box<dyn Error>>;
    fn apply_action_and_snapshot(
        &self,
        target: &mut Self::Target,
        action: &Self::Action,
        milestones: &mut Self::Milestones,
    ) -> Result<Option<Self::Snapshot>, Box<dyn Error>> {
        self.apply_action(target, action, milestones)?;
        Ok(None)
    }
    fn rollout_observations(&self, _target: &Self::Target) -> Vec<Self::Observations> {
        Vec::new()
    }
    fn rollout_probe(
        &self,
        _run: &Self::Run,
        target: &mut Self::Target,
        snapshot: &Self::Snapshot,
    ) -> Result<bool, Box<dyn Error>> {
        self.restore(target, snapshot)?;
        Ok(true)
    }
    fn snapshot(&self, target: &mut Self::Target) -> Result<Self::Snapshot, Box<dyn Error>>;
    fn candidate_replay_certificate(
        &self,
        _snapshot: &Self::Snapshot,
    ) -> Result<Option<[u8; 32]>, Box<dyn Error>> {
        Ok(None)
    }
    fn current_replay_certificate(
        &self,
        _target: &Self::Target,
    ) -> Result<Option<[u8; 32]>, Box<dyn Error>> {
        Ok(None)
    }
    #[allow(clippy::too_many_arguments)]
    fn execute_job_certified(
        &self,
        run: &Self::Run,
        target: &mut Self::Target,
        origin_snapshot: &Self::Snapshot,
        replay: &[Self::Action],
        replay_certificates: &[Option<[u8; 32]>],
        parent_milestones: Self::Milestones,
        suffix: &[Self::Action],
        retention: RetentionPolicy,
        stop_rollout_on_objective: bool,
    ) -> Result<CampaignJobResult<Self>, Box<dyn Error>>
    where
        Self: Workload + Sized,
    {
        if replay.len() != replay_certificates.len() {
            return Err("replay certificate count does not match the action path".into());
        }
        self.execute_job(
            run,
            target,
            origin_snapshot,
            replay,
            parent_milestones,
            suffix,
            retention,
            stop_rollout_on_objective,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn execute_job(
        &self,
        run: &Self::Run,
        target: &mut Self::Target,
        origin_snapshot: &Self::Snapshot,
        replay: &[Self::Action],
        parent_milestones: Self::Milestones,
        suffix: &[Self::Action],
        retention: RetentionPolicy,
        stop_rollout_on_objective: bool,
    ) -> Result<CampaignJobResult<Self>, Box<dyn Error>>
    where
        Self: Workload + Sized,
    {
        crate::search::rollout::execute_job(
            self,
            run,
            target,
            origin_snapshot,
            replay,
            parent_milestones,
            suffix,
            retention,
            stop_rollout_on_objective,
        )
    }
}

pub trait Evaluation: CampaignTypes {
    fn execution_disposition(
        &self,
        target: &Self::Target,
    ) -> crate::search::rollout::ExecutionDisposition;
    fn objective_reached(
        &self,
        run: &Self::Run,
        target: &Self::Target,
    ) -> Result<bool, Box<dyn Error>>;
    fn rollout_outcome(
        &self,
        run: &Self::Run,
        target: &Self::Target,
    ) -> Result<crate::search::rollout::Outcome, Box<dyn Error>> {
        Ok(crate::search::rollout::Outcome {
            objective_reached: self.objective_reached(run, target)?,
            disposition: self.execution_disposition(target),
        })
    }
    fn merge_preparation_failure(
        &self,
        _evidence: &mut Self::Evidence,
        _observations: &[Self::Observations],
    ) {
    }
    fn current_key(&self, target: &Self::Target) -> Result<Self::Key, Box<dyn Error>>;
    fn rollout_key(&self, target: &Self::Target) -> Result<Self::Key, Box<dyn Error>> {
        self.current_key(target)
    }
    fn complete_candidate_key(
        &self,
        key: Self::Key,
        snapshot: &Self::Snapshot,
    ) -> Result<Self::Key, Box<dyn Error>>;
    fn merge_milestones(&self, into: &mut Self::Milestones, from: Self::Milestones);
    fn aggregate_milestones(evidence: &Self::Evidence) -> Self::Milestones;
    fn aggregate_progress(evidence: &Self::Evidence) -> Self::Progress;
    fn merge_origin_evidence(&self, evidence: &mut Self::Evidence, source: &Self::ArchiveReport);
    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut Self::Evidence,
        target: &Self::Target,
    ) -> Result<(), Box<dyn Error>>;
    fn merge_import_evidence(
        &self,
        evidence: &mut Self::Evidence,
        milestones: Self::Milestones,
        input: &Input<Self::Action>,
    );
    fn merge_action_evidence<F>(
        &self,
        evidence: &mut Self::Evidence,
        action: &CampaignActionResult<Self>,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<Input<Self::Action>, Box<dyn Error>>;
    fn source_entries<'a>(
        &self,
        source: &'a Self::ArchiveReport,
    ) -> &'a [ArchiveEntryReport<Self::Action, Self::Key, Self::Milestones>];
    fn resume_input(
        &self,
        source: &Self::ArchiveReport,
    ) -> Result<Input<Self::Action>, Box<dyn Error>>;
}

pub trait Workload: CampaignTypes + TargetExecution + InputPolicy + Evaluation + Reporting {}

impl<T> Workload for T where
    T: CampaignTypes + TargetExecution + InputPolicy + Evaluation + Reporting
{
}

pub struct ArchiveReportState<G: CampaignTypes + ?Sized> {
    pub seed: u64,
    pub executions: u64,
    pub entries: Vec<ArchiveEntryReport<G::Action, G::Key, G::Milestones>>,
    pub progress_curve: Vec<ProgressPoint<G::Milestones, G::Progress>>,
    pub retained: u64,
    pub rejected: u64,
    pub terminal_endpoints: u64,
    pub terminal_objectives: u64,
    pub execution_failures: u64,
    pub selector: SelectorAccounting,
}

pub enum CampaignOrigin<G: Workload + ?Sized> {
    Genesis,
    SnapshotRoot {
        checkpoint: CampaignCheckpoint<G::Snapshot>,
    },
    Archive {
        path: String,
        source: ArchiveSource<G::ArchiveReport>,
        checkpoint: Option<CampaignCheckpoint<G::Snapshot>>,
    },
    SearchCheckpoint {
        path: PathBuf,
        expected_sha256: Option<String>,
    },
}

#[derive(Clone)]
pub struct ArchiveSource<R> {
    file_sha256: String,
    report: R,
}

struct ArchiveHashReader<R> {
    inner: R,
    digest: Sha256,
}

impl<R: Read> Read for ArchiveHashReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(bytes)?;
        self.digest.update(&bytes[..read]);
        Ok(read)
    }
}

impl<R: DeserializeOwned> ArchiveSource<R> {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Box<dyn Error>> {
        Self::from_reader(bytes)
    }

    pub fn from_reader(source: impl Read) -> Result<Self, Box<dyn Error>> {
        let mut reader = BufReader::new(ArchiveHashReader {
            inner: source,
            digest: Sha256::new(),
        });
        let report = serde_json::from_reader(&mut reader)?;
        std::io::copy(&mut reader, &mut std::io::sink())?;
        Ok(Self {
            file_sha256: format!("{:x}", reader.into_inner().digest.finalize()),
            report,
        })
    }
}

impl<R> ArchiveSource<R> {
    pub fn file_sha256(&self) -> &str {
        &self.file_sha256
    }

    pub fn report(&self) -> &R {
        &self.report
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CampaignCheckpoint<S> {
    pub path: String,
    pub file_sha256: String,
    pub snapshots: SnapshotCheckpoint<S>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotCheckpoint<S> {
    pub format: String,
    pub entries: Vec<SnapshotCheckpointEntry<S>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "S: Serialize + DeserializeOwned")]
pub struct SnapshotCheckpointEntry<S> {
    pub id: u64,
    pub snapshot: S,
}

type ArchiveMaterialization<G> = (
    <G as CampaignTypes>::ArchiveReport,
    Vec<(u64, <G as CampaignTypes>::Snapshot)>,
);

impl<S: super::snapshot_codec::CheckpointSnapshot> SnapshotCheckpoint<S> {
    pub fn to_bytes(&self) -> Result<Vec<u8>, Box<dyn Error>> {
        let mut bytes = Vec::new();
        self.write_to(&mut bytes)?;
        Ok(bytes)
    }
    pub fn sha256(&self) -> Result<String, Box<dyn Error>> {
        let mut writer = CheckpointHashWriter(Sha256::new());
        self.write_to(&mut writer)?;
        Ok(format!("{:x}", writer.0.finalize()))
    }
    pub fn write_to(&self, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
        super::snapshot_codec::write_checkpoint(self, out)
    }
    pub fn read_from(
        input: &mut dyn std::io::Read,
        expected_format: &str,
    ) -> Result<Self, Box<dyn Error>> {
        super::snapshot_codec::read_checkpoint(input, expected_format)
    }
    pub fn from_bytes(bytes: &[u8], expected_format: &str) -> Result<Self, Box<dyn Error>> {
        Self::read_from(&mut &*bytes, expected_format)
    }
}

pub struct CampaignConfig<G: Workload + ?Sized> {
    pub campaign_seed: u64,
    pub workers: u32,
    pub execution_budget: u64,
    pub host: String,
    pub wall_budget: Option<Duration>,
    pub stop_rollout_on_objective: bool,
    pub stop_campaign_on_objective: bool,
    pub archive_entry_limit: usize,
    pub window: usize,
    pub memory_budget_mib: Option<usize>,
    pub materialize_final_artifacts: bool,
    pub run: G::Run,
    pub mixture: DrawMixture,
    pub retention: RetentionPolicy,
    pub objective_witness_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "T: Serialize + DeserializeOwned")]
pub struct CampaignStreamHeader<T> {
    pub schema_version: u32,
    pub format: String,
    pub campaign_seed: u64,
    pub schedule_policy: String,
    pub progress_policy: String,
    pub host: String,
    pub origin_kind: String,
    pub origin_path: Option<String>,
    pub origin_archive_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_checkpoint_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_checkpoint_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub origin_checkpoint_policy_changes: BTreeMap<String, PolicyChange>,
    pub resume_input_sha256: String,
    pub resume_actions: usize,
    pub execution_budget: u64,
    pub stop_rollout_on_objective: bool,
    pub stop_campaign_on_objective: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_budget: Option<u64>,
    pub wall_budget_seconds: Option<u64>,
    pub archive_entry_limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_budget_mib: Option<usize>,
    pub resume_policy: String,
    pub mixture_policy: String,
    #[serde(flatten)]
    pub workload_policies: WorkloadPolicies,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draw_header: Option<T>,
    pub retention_policy: String,
    pub parent_scheduler: String,
    pub preference_portfolio: String,
    pub executor_mode: String,
    pub selection_seed_derivation: String,
    pub workload_identity_sha256: String,
    pub executable_identity: String,
    pub action_cost_unit: String,
    pub execution_work_unit: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CampaignSpliceRecord {
    Unavailable,
    Tail {
        donor_id: u64,
        leaf_id: u64,
        tail_postcard: Vec<u8>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum CampaignAdmissionDecision {
    Retained { id: u64 },
    Duplicate { id: u64 },
    Rejected,
    ProbeRefused,
    Objective,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "C: Serialize + DeserializeOwned, K: Serialize + DeserializeOwned")]
pub struct CampaignJobRecord<C, K = ()> {
    pub sequence: u64,
    pub reservation: u64,
    pub parent_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_energy: Option<u16>,
    pub mutation_seed: u64,
    pub execution_work: u64,
    pub result_sha256: String,
    pub decisions: Vec<CampaignAdmissionDecision>,
    pub mixture_weight: u8,
    pub splice_weight: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub splice: Option<CampaignSpliceRecord>,

    pub selector: SelectorDraw,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draw_checkpoint_before: Option<C>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draw_checkpoint_after: Option<C>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_draw: Option<DurationDraw<K>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_admission_sequence_at_draw: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_remaining_work: Option<NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_checkpoint_at_draw: Option<DurationCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_checkpoint_before: Option<DurationCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_checkpoint_after: Option<DurationCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix_limit: Option<u8>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "C: Serialize + DeserializeOwned, K: Serialize + DeserializeOwned")]
pub struct CampaignSkipRecord<C, K = ()> {
    pub reservation: u64,
    pub parent_id: u64,
    pub mutation_seed: u64,
    pub mixture_weight: u8,
    pub splice_weight: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub splice: Option<CampaignSpliceRecord>,

    pub selector: SelectorDraw,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draw_checkpoint_before: Option<C>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draw_checkpoint_after: Option<C>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_draw: Option<DurationDraw<K>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_admission_sequence_at_draw: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_remaining_work: Option<NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_checkpoint_at_draw: Option<DurationCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_checkpoint_before: Option<DurationCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_checkpoint_after: Option<DurationCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix_limit: Option<u8>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CampaignEndRecord {
    pub jobs: u64,
    pub skips: u64,
    pub executions_completed: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "C: Serialize + DeserializeOwned, K: Serialize + DeserializeOwned")]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CampaignStreamRecord<C, K = ()> {
    Job(CampaignJobRecord<C, K>),
    Skip(CampaignSkipRecord<C, K>),
    End(CampaignEndRecord),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CampaignOriginRecord {
    pub kind: String,
    pub path: Option<String>,
    pub archive_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub checkpoint_policy_changes: BTreeMap<String, PolicyChange>,
    pub resume_input_sha256: String,
    pub resume_actions: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PolicyChange {
    pub checkpoint: String,
    pub run: String,
}

pub const PREFERENCE_POLICY_FIELD: &str = "preference_policy";

const PREFERENCE_PORTFOLIO_FIELD: &str = "preference_portfolio";

const RESUMABLE_POLICY_FIELDS: [&str; 9] = [
    "mixture_policy",
    "retention_policy",
    "parent_scheduler",
    "continuation_policy",
    "stop_rollout_on_objective",
    "stop_campaign_on_objective",
    DRAW_TABLE_POLICY_FIELD,
    PREFERENCE_PORTFOLIO_FIELD,
    PREFERENCE_POLICY_FIELD,
];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "A: Serialize + DeserializeOwned + Ord + Clone, R: Serialize + DeserializeOwned")]
pub struct CampaignModeReport<A: Ord, R> {
    pub mode: String,
    pub campaign_seed: u64,
    pub window: usize,
    pub host: String,
    pub schedule_identity: String,
    pub origin: CampaignOriginRecord,
    pub execution_budget: u64,
    pub stop_rollout_on_objective: bool,
    pub stop_campaign_on_objective: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_budget: Option<u64>,
    pub executions_completed: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executions_to_first_objective: Option<u64>,
    pub wall_budget_seconds: Option<u64>,
    pub archive_entry_limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_budget_mib: Option<usize>,
    pub resume_policy: String,
    pub mixture_policy: String,
    #[serde(flatten)]
    pub workload_policies: WorkloadPolicies,
    pub retention_policy: String,
    pub parent_scheduler: String,
    pub executor_mode: String,
    pub selection_seed_derivation: String,
    pub workload_identity_sha256: String,
    pub executable_identity: String,
    pub action_cost_unit: String,
    pub execution_work_unit: String,
    pub bootstrap_execution_work: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_import: Option<TreeImportCounts>,
    pub execution_work: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_to_first_objective: Option<u64>,
    pub duplicates_skipped: u64,
    pub probe_refused: u64,
    pub terminal_endpoints: u64,
    pub terminal_objectives: u64,
    pub execution_failures: u64,
    pub objectives_reached: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective_witness: Option<Input<A>>,
    pub replacement_cost_displaced: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub snapshot_evictions: u64,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub resident_snapshot_bytes: usize,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub history_memory_bytes: usize,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub draw_state_memory_bytes: usize,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub resident_memory_bytes: usize,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub live_entries: usize,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub history_compactions: u64,
    #[cfg(test)]
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub(crate) liveness_anchor_reactivations: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub historical_entries_dropped: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub input_reconstructions: u64,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub input_index_nodes: usize,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub historical_cells: usize,
    pub stream_sha256: String,
    pub archive: R,
    #[serde(skip)]
    pub telemetry: CampaignTelemetry,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TreeImportCounts {
    pub imported: u64,
    pub duplicate: u64,
    pub rejected: u64,
    pub terminal: u64,
    pub rerooted: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub checkpointed: u64,
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

fn archive_entry_limit_is_valid(limit: usize) -> bool {
    (1..=crate::search::archive::MAX_ARCHIVE_ENTRIES).contains(&limit)
}

fn memory_is_within_reserve(bytes: usize, reserve: usize) -> bool {
    bytes <= reserve
}

fn resident_memory_is_within_budget(bytes: usize, memory_budget_mib: Option<usize>) -> bool {
    memory_budget_mib.is_none_or(|budget_mib| bytes <= budget_mib.saturating_mul(1024 * 1024))
}

struct ContinuationReservation<G: Workload + ?Sized> {
    parent_index: usize,
    parent_id: u64,
    donor: u64,
    leaf: u64,
    source: Position<G::Key>,
    destination: Position<G::Key>,
    wave: u32,
    suffix: Vec<G::Action>,
}

fn take_continuation<G: Workload + ?Sized>(
    core: &mut CoordinatorCore<G>,
    from_highest_preference: bool,
) -> Option<ContinuationReservation<G>> {
    let mut examined = 0_usize;
    let mut gaining_fallback = None;
    while examined < CONTINUATION_EXITS_PER_RESERVATION
        && let Some(continuation) = core.archive.pop_continuation(from_highest_preference)
    {
        examined = examined.saturating_add(1);
        let Some(parent_index) = core.archive.index_of_id(continuation.parent) else {
            continue;
        };
        if !core.archive.is_active(parent_index) {
            continue;
        }
        let outranks = core.archive.outranks_slot_holders(
            parent_index,
            continuation.destination,
            continuation.preference,
        );
        let gaining = continuation.gains != 0;
        if !outranks && (!gaining || gaining_fallback.is_some()) {
            continue;
        }
        let actions = continuation.actions;
        if core.all_prefixes_archived(parent_index, &actions) {
            continue;
        }
        let reservation = ContinuationReservation {
            parent_index,
            parent_id: continuation.parent,
            donor: continuation.donor,
            leaf: continuation.leaf,
            source: continuation.source,
            destination: continuation.destination,
            wave: continuation.wave,
            suffix: actions,
        };
        if outranks {
            core.archive.record_continuation_dispatch(false);
            return Some(reservation);
        }
        gaining_fallback = Some(reservation);
    }
    if gaining_fallback.is_some() {
        core.archive.record_continuation_dispatch(true);
    }
    gaining_fallback
}

fn continuation_attempt<G: Workload + ?Sized>(
    core: &mut CoordinatorCore<G>,
    campaign_seed: u64,
    reservation: u64,
) -> (Option<u16>, Option<ContinuationReservation<G>>) {
    if core.archive.continuation_pending() == 0 {
        return (None, None);
    }
    let energy = CONTINUATION_ATTEMPT_SHARE;
    let takes = draws_continuation(campaign_seed, reservation);
    core.archive.record_continuation_reservation(energy, takes);
    let taken = takes
        .then(|| {
            let from_highest_preference = draws_highest_preference(campaign_seed, reservation);
            take_continuation(core, from_highest_preference)
        })
        .flatten();
    (Some(energy), taken)
}

fn continuation_reservation_matches<G: Workload + ?Sized>(
    taken: Option<&ContinuationReservation<G>>,
    energy: Option<u16>,
    selector: &SelectorDraw,
    splice: Option<&CampaignSpliceRecord>,
    recorded_energy: Option<u16>,
    sequence: u64,
) -> Result<(), Box<dyn Error>> {
    if recorded_energy != energy {
        return Err(
            format!("record {sequence} disagrees with the rebuilt continuation share").into(),
        );
    }
    let recorded = selector.path == SelectorPath::Continuation;
    match (taken, recorded) {
        (None, false) => Ok(()),
        (None, true) => Err(format!(
            "record {sequence} is a continuation job the replayed queue did not produce"
        )
        .into()),
        (Some(_), false) => Err(format!(
            "the replayed queue produced a continuation at record {sequence}, which is an ordinary job"
        )
        .into()),
        (Some(taken), true) => {
            let Some(CampaignSpliceRecord::Tail {
                donor_id,
                leaf_id,
                tail_postcard,
            }) = splice
            else {
                return Err(
                    format!("continuation record {sequence} lacks its complete tail").into(),
                );
            };
            let tail: Vec<G::Action> = postcard::from_bytes(tail_postcard)?;
            if *donor_id != taken.donor || *leaf_id != taken.leaf || tail != taken.suffix {
                return Err(format!(
                    "continuation record {sequence} disagrees with the replayed queue entry"
                )
                .into());
            }
            Ok(())
        }
    }
}

fn continuation_arrival<G: Workload>(
    archive: &Archive<G::Action, G::Key, G::Milestones, G::Snapshot>,
    edge: Option<Edge<G::Key>>,
    decisions: &[CampaignAdmissionDecision],
) -> (Option<usize>, bool) {
    let Some((source, destination)) = edge else {
        return (None, false);
    };
    decisions
        .iter()
        .fold((None, false), |(landed, replaced), decision| {
            let (id, retained) = match decision {
                CampaignAdmissionDecision::Retained { id } => (*id, true),
                CampaignAdmissionDecision::Duplicate { id } => (*id, false),
                _ => return (landed, replaced),
            };
            let arrived = archive
                .index_of_id(id)
                .filter(|index| archive.lands_on_edge(*index, source, destination));
            (
                landed.or(arrived),
                replaced || (arrived.is_some() && retained),
            )
        })
}

fn retained_archive_indexes<'a, G: Workload>(
    core: &'a CoordinatorCore<G>,
    decisions: &'a [CampaignAdmissionDecision],
) -> impl Iterator<Item = usize> + 'a {
    decisions.iter().filter_map(|decision| match decision {
        CampaignAdmissionDecision::Retained { id } => core.archive.index_of_id(*id),
        _ => None,
    })
}

fn progress_policy_is_supported(policy: &str) -> bool {
    policy == CAMPAIGN_PROGRESS_POLICY
}

fn record_mixture_outcome(
    energy: &mut MixtureEnergy,
    mixture: DrawMixture,
    path: SelectorPath,
    mutation_seed: u64,
    (mixture_weight, splice_weight): (u8, u8),
    new_slot: bool,
    work: u64,
) -> Result<(), Box<dyn Error>> {
    if path == SelectorPath::Continuation {
        return Ok(());
    }
    if matches!(
        mixture,
        DrawMixture::Energy { .. } | DrawMixture::EnergySplice { .. }
    ) {
        energy.record_outcome(
            energy_strategy(mutation_seed, mixture_weight, splice_weight)?,
            new_slot,
            work,
        );
    }
    Ok(())
}

pub(super) fn mixture_weights(energy: &MixtureEnergy, mixture: DrawMixture) -> (u8, u8) {
    match mixture {
        DrawMixture::Energy { scale } => (energy.biased_weight(scale), 0),
        DrawMixture::EnergySplice { scale } => energy.splice_weights(scale),
        _ => (DEFAULT_MIXTURE_WEIGHT, 0),
    }
}

pub(crate) const CONTINUATION_EXITS_PER_RESERVATION: usize = 8;

pub(crate) const CONTINUATION_ATTEMPT_IN: usize = 4;

pub(crate) const CONTINUATION_HIGHEST_TIER_IN: usize = 4;

const CONTINUATION_ATTEMPT_SHARE: u16 = 64;

const CONTINUATION_TIER_SALT: u64 = 0x9e37_79b9_7f4a_7c15;

fn draws_one_in(seed: u64, one_in: usize) -> bool {
    let Some(bound) = NonZeroUsize::new(one_in) else {
        return false;
    };
    RomuDuoJrRand::with_seed(seed).below(bound) == 0
}

#[must_use]
pub(crate) fn draws_continuation(campaign_seed: u64, reservation: u64) -> bool {
    draws_one_in(campaign_seed ^ reservation, CONTINUATION_ATTEMPT_IN)
}

#[must_use]
pub(crate) fn draws_highest_preference(campaign_seed: u64, reservation: u64) -> bool {
    draws_one_in(
        campaign_seed ^ reservation ^ CONTINUATION_TIER_SALT,
        CONTINUATION_HIGHEST_TIER_IN,
    )
}

fn stop_reservations_after_objective(
    stop_campaign_on_objective: bool,
    objective_found: bool,
) -> bool {
    stop_campaign_on_objective && objective_found
}

fn stop_reservations_after_work_budget(
    work_budget: Option<u64>,
    bootstrap_execution_work: u64,
    job_execution_work: u64,
) -> bool {
    work_budget
        .is_some_and(|budget| bootstrap_execution_work.saturating_add(job_execution_work) >= budget)
}

pub type CampaignCandidate<G> =
    CampaignCandidateData<<G as CampaignTypes>::Key, <G as CampaignTypes>::Snapshot>;
pub type CampaignActionResult<G> = CampaignActionResultData<
    <G as CampaignTypes>::Action,
    <G as CampaignTypes>::Observations,
    <G as CampaignTypes>::Milestones,
    CampaignCandidate<G>,
>;
pub type CampaignJobResult<G> =
    CampaignJobResultData<<G as CampaignTypes>::Observations, CampaignActionResult<G>>;

#[derive(Deserialize, Serialize)]
#[serde(rename = "CampaignCandidate")]
pub struct CampaignCandidateData<K, S> {
    pub key: K,
    pub viable: bool,
    pub snapshot: S,
}

impl<K: PartialEq, S: PartialEq> PartialEq for CampaignCandidateData<K, S> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.viable == other.viable && self.snapshot == other.snapshot
    }
}
impl<K: Eq, S: Eq> Eq for CampaignCandidateData<K, S> {}

impl<K: Copy, S: Clone> Clone for CampaignCandidateData<K, S> {
    fn clone(&self) -> Self {
        Self {
            key: self.key,
            viable: self.viable,
            snapshot: self.snapshot.clone(),
        }
    }
}

impl<K: Debug, S: Debug> Debug for CampaignCandidateData<K, S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CampaignCandidate")
            .field("key", &self.key)
            .field("viable", &self.viable)
            .field("snapshot", &self.snapshot)
            .finish()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename = "CampaignActionResult")]
pub struct CampaignActionResultData<A, O, M, C> {
    pub action: A,
    pub observations: Vec<O>,
    pub milestones: M,
    pub outcome: crate::search::rollout::Outcome,
    pub candidate: Option<C>,
}

impl<A: PartialEq, O: PartialEq, M: PartialEq, C: PartialEq> PartialEq
    for CampaignActionResultData<A, O, M, C>
{
    fn eq(&self, other: &Self) -> bool {
        self.action == other.action
            && self.observations == other.observations
            && self.milestones == other.milestones
            && self.outcome == other.outcome
            && self.candidate == other.candidate
    }
}
impl<A: Eq, O: Eq, M: Eq, C: Eq> Eq for CampaignActionResultData<A, O, M, C> {}

impl<A: Copy, O: Clone, M: Copy, C: Clone> Clone for CampaignActionResultData<A, O, M, C> {
    fn clone(&self) -> Self {
        Self {
            action: self.action,
            observations: self.observations.clone(),
            milestones: self.milestones,
            outcome: self.outcome,
            candidate: self.candidate.clone(),
        }
    }
}

impl<A: Debug, O: Debug, M: Debug, C: Debug> Debug for CampaignActionResultData<A, O, M, C> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CampaignActionResult")
            .field("action", &self.action)
            .field("observations", &self.observations)
            .field("milestones", &self.milestones)
            .field("outcome", &self.outcome)
            .field("candidate", &self.candidate)
            .finish()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename = "CampaignJobResult")]
pub struct CampaignJobResultData<O, R> {
    pub preparation_failure: Option<Vec<O>>,
    pub actions: Vec<R>,
}

impl<O: PartialEq, R: PartialEq> PartialEq for CampaignJobResultData<O, R> {
    fn eq(&self, other: &Self) -> bool {
        self.preparation_failure == other.preparation_failure && self.actions == other.actions
    }
}
impl<O: Eq, R: Eq> Eq for CampaignJobResultData<O, R> {}

impl<O: Clone, R: Clone> Clone for CampaignJobResultData<O, R> {
    fn clone(&self) -> Self {
        Self {
            preparation_failure: self.preparation_failure.clone(),
            actions: self.actions.clone(),
        }
    }
}

impl<O: Debug, R: Debug> Debug for CampaignJobResultData<O, R> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CampaignJobResult")
            .field("preparation_failure", &self.preparation_failure)
            .field("actions", &self.actions)
            .finish()
    }
}

fn verify_selector_annotation(
    draw: &SelectorDraw,
    archive_entry_limit: usize,
) -> Result<(), Box<dyn Error>> {
    match (draw.path, draw.tier_rank) {
        (SelectorPath::Tiers | SelectorPath::Recent, None) => {
            Err("tier draw is missing its rank".into())
        }
        (SelectorPath::Tiers | SelectorPath::Recent, Some(rank))
            if usize::from(rank) >= archive_entry_limit =>
        {
            Err("tier rank exceeds the archive population limit".into())
        }
        (SelectorPath::Continuation, Some(_)) => {
            Err("continuation draw carries a tier rank".into())
        }
        _ => Ok(()),
    }
}

fn verify_selector_observed_tiers(
    draw: &SelectorDraw,
    observed_progress_tiers: usize,
) -> Result<(), Box<dyn Error>> {
    if draw
        .tier_rank
        .is_some_and(|rank| usize::from(rank) >= observed_progress_tiers)
    {
        return Err("tier rank exceeds observed progress tiers at selection".into());
    }
    Ok(())
}

fn verify_selector_preference<K: ArchiveKey>(draw: &SelectorDraw) -> Result<(), Box<dyn Error>> {
    if draw
        .best_preference
        .is_some_and(|preference| usize::from(preference) >= K::preferences())
    {
        return Err("selector best preference is outside the key portfolio".into());
    }
    Ok(())
}

#[must_use]
pub fn derive_selection_seed(campaign_seed: u64) -> u64 {
    let digest = Sha256::digest(campaign_seed.to_le_bytes());
    u64::from_le_bytes(
        digest[..8]
            .try_into()
            .expect("a SHA-256 digest has eight bytes"),
    )
}

pub(crate) struct CoordinatorCore<G: Workload + ?Sized> {
    pub(crate) archive: Archive<G::Action, G::Key, G::Milestones, G::Snapshot>,
    pub(crate) evidence: G::Evidence,
    curve: Vec<ProgressPoint<G::Milestones, G::Progress>>,
    curve_interval: u64,
    bounded_progress_curve: bool,
    record_progress: bool,
    terminal_endpoints: u64,
    terminal_objectives: u64,
    execution_failures: u64,
    pub(crate) objectives_reached: u64,
    pub(crate) objective_witness: Option<Input<G::Action>>,
    sequence: u64,
    probe_refused: u64,
    pub(crate) mixture_energy: MixtureEnergy,
    suffix_jobs_in_flight: BTreeMap<(u64, u8), u32>,
    admission_suffix: Vec<G::Action>,
}

impl<G: Workload + ?Sized> CoordinatorCore<G> {
    pub(crate) fn new(
        workload: &G,
        run: &G::Run,
        archive_entry_limit: usize,
        memory_budget_mib: Option<usize>,
    ) -> Self {
        let mut archive = Archive::new(workload.action_cost_fn());
        archive.max_entries = archive_entry_limit;
        archive.set_snapshot_retention(G::snapshot_retention());
        if let Some(memory_budget_mib) = memory_budget_mib {
            let total = memory_budget_mib.saturating_mul(1024 * 1024);
            let draw_reserve = workload
                .draw_state_memory_reserve_bytes(run)
                .saturating_add(DurationPolicies::<G::Key>::memory_reserve_bytes());
            archive.set_memory_budget(
                total.saturating_sub(draw_reserve),
                G::snapshot_memory_charge,
            );
        }
        Self {
            archive,
            evidence: G::Evidence::default(),
            curve: Vec::new(),
            curve_interval: CURVE_INTERVAL,
            bounded_progress_curve: true,
            record_progress: true,
            terminal_endpoints: 0,
            terminal_objectives: 0,
            execution_failures: 0,
            objectives_reached: 0,
            objective_witness: None,
            sequence: 0,
            probe_refused: 0,
            mixture_energy: MixtureEnergy::default(),
            suffix_jobs_in_flight: BTreeMap::new(),
            admission_suffix: Vec::new(),
        }
    }

    fn suffix_limit(&self, parent_id: u64, parent_index: usize) -> u8 {
        self.suffix_limit_excluding(parent_id, parent_index, &BTreeMap::new())
    }

    fn suffix_limit_excluding(
        &self,
        parent_id: u64,
        parent_index: usize,
        excluded: &BTreeMap<(u64, u8), u32>,
    ) -> u8 {
        let limit = self.archive.suffix_limit(parent_index);
        if self
            .suffix_jobs_in_flight
            .range((parent_id, limit)..=(parent_id, u8::MAX))
            .any(|(&(held_parent, held_limit), &count)| {
                count
                    > excluded
                        .get(&(held_parent, held_limit))
                        .copied()
                        .unwrap_or(0)
            })
        {
            1
        } else {
            limit
        }
    }

    fn start_suffix_job(&mut self, parent_id: u64, limit: Option<u8>) {
        if let Some(limit) = limit {
            *self
                .suffix_jobs_in_flight
                .entry((parent_id, limit))
                .or_insert(0) += 1;
        }
    }

    fn finish_suffix_job(&mut self, parent_id: u64, limit: Option<u8>) {
        if let Some(limit) = limit
            && let Some(count) = self.suffix_jobs_in_flight.get_mut(&(parent_id, limit))
        {
            *count -= 1;
            if *count == 0 {
                self.suffix_jobs_in_flight.remove(&(parent_id, limit));
            }
        }
    }

    fn write_checkpoint_state(&self, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
        use super::checkpoint::image::StreamedBytes;
        postcard::to_io(
            &(
                StreamedBytes(|out: &mut dyn Write| {
                    serde_json::to_writer(out, &self.curve)?;
                    Ok(())
                }),
                self.curve_interval,
                self.bounded_progress_curve,
                self.record_progress,
                self.terminal_endpoints,
                self.terminal_objectives,
                self.execution_failures,
                self.objectives_reached,
                &self.objective_witness,
                self.sequence,
                self.probe_refused,
                self.mixture_energy,
            ),
            out,
        )?;
        Ok(())
    }

    fn restore_state(&mut self, state: CoreCheckpoint<G>) -> Result<(), Box<dyn Error>> {
        self.curve = state.curve;
        self.curve_interval = state.curve_interval;
        self.bounded_progress_curve = state.bounded_progress_curve;
        self.record_progress = state.record_progress;
        self.terminal_endpoints = state.terminal_endpoints;
        self.terminal_objectives = state.terminal_objectives;
        self.execution_failures = state.execution_failures;
        self.objectives_reached = state.objectives_reached;
        self.objective_witness = state.objective_witness;
        self.sequence = state.sequence;
        self.probe_refused = state.probe_refused;
        self.mixture_energy = state.mixture_energy;
        Ok(())
    }

    pub(crate) fn bootstrap(
        &mut self,
        workload: &G,
        run: &G::Run,
        target: &mut G::Target,
    ) -> Result<(), Box<dyn Error>> {
        workload.reset(target);
        let outcome = workload.rollout_outcome(run, target)?;
        if outcome.disposition.is_failed() {
            return Err("campaign genesis target failed before execution".into());
        }
        if matches!(
            outcome.disposition,
            crate::search::rollout::ExecutionDisposition::Terminal
        ) {
            self.terminal_endpoints = self.terminal_endpoints.saturating_add(1);
            if outcome.objective_reached {
                self.terminal_objectives = self.terminal_objectives.saturating_add(1);
            }
        }
        if outcome.objective_reached {
            self.objectives_reached = self.objectives_reached.saturating_add(1);
            self.objective_witness = Some(Input::default());
        }
        let genesis_key = workload.current_key(target)?;
        let genesis_snapshot = workload.snapshot(target)?;
        self.archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: Vec::new(),
                    key: genesis_key,
                    milestones: G::Milestones::default(),
                },
                genesis_snapshot,
            )?
            .ok_or("failed to retain campaign genesis")?;
        Ok(())
    }

    pub(crate) fn bootstrap_snapshot_root(
        &mut self,
        workload: &G,
        run: &G::Run,
        target: &mut G::Target,
        checkpoint: &CampaignCheckpoint<G::Snapshot>,
    ) -> Result<(), Box<dyn Error>> {
        let snapshot = validate_snapshot_root_checkpoint(workload, checkpoint)?;
        workload.restore(target, snapshot)?;
        let outcome = workload.rollout_outcome(run, target)?;
        if outcome.disposition.is_failed() {
            return Err("snapshot-root checkpoint restores a failed target".into());
        }
        if matches!(
            outcome.disposition,
            crate::search::rollout::ExecutionDisposition::Terminal
        ) {
            self.terminal_endpoints = self.terminal_endpoints.saturating_add(1);
            if outcome.objective_reached {
                self.terminal_objectives = self.terminal_objectives.saturating_add(1);
            }
        }
        if outcome.disposition.is_terminal() && !outcome.objective_reached {
            return Err("snapshot-root checkpoint restores a terminal target".into());
        }
        if outcome.objective_reached {
            self.objectives_reached = self.objectives_reached.saturating_add(1);
            self.objective_witness = Some(Input::default());
        }
        workload.merge_snapshot_root_evidence(&mut self.evidence, target)?;
        let key = workload.current_key(target)?;
        let retained = self.archive.insert(
            None,
            0,
            ArchiveCandidate {
                suffix: Vec::new(),
                key,
                milestones: G::Milestones::default(),
            },
            snapshot.clone(),
        )?;
        if retained != Some(0) || self.archive.entries.len() != 1 {
            return Err("failed to retain snapshot-root checkpoint as archive id zero".into());
        }
        Ok(())
    }

    pub(crate) fn import_tree(
        &mut self,
        workload: &G,
        run: &G::Run,
        target: &mut G::Target,
        source: &G::ArchiveReport,
        checkpoint: Option<&SnapshotCheckpoint<G::Snapshot>>,
    ) -> Result<TreeImportCounts, Box<dyn Error>> {
        if let Some(checkpoint) = checkpoint {
            validate_archive_checkpoint_entries(workload, source, checkpoint)?;
        }
        let preserve_inactive_snapshots = self.archive.preserves_inactive_snapshots();
        self.archive.preserve_inactive_snapshots(true)?;
        self.bootstrap(workload, run, target)?;
        let genesis_id = 0;
        let checkpointed = checkpoint
            .map(|checkpoint| {
                checkpoint
                    .entries
                    .iter()
                    .map(|entry| (entry.id, entry.snapshot.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let checkpointed = checkpointed.into_iter().collect::<BTreeMap<_, _>>();
        let mut counts = TreeImportCounts::default();
        let mut index_of: BTreeMap<u64, usize> = BTreeMap::new();
        let source_entries = workload.source_entries(source);
        let mut imported: Vec<Option<usize>> = Vec::with_capacity(source_entries.len());
        for (index, entry) in source_entries.iter().enumerate() {
            index_of.insert(entry.id, index);
            if entry.input.actions.is_empty() {
                if let Some(expected) = entry.snapshot_identity_sha256.as_deref() {
                    let genesis = self.archive.entries[genesis_id]
                        .snapshot
                        .as_deref()
                        .ok_or("whole-tree import genesis snapshot was released early")?;
                    if workload.snapshot_identity_sha256(genesis)? != expected {
                        return Err("imported genesis differs from its source entry digest".into());
                    }
                }
                imported.push(Some(genesis_id));
                continue;
            }
            let mut ancestor = entry.parent_id;
            let mut parent = None;
            while let Some(id) = ancestor {
                let Some(ancestor_index) = index_of.get(&id).copied().filter(|i| *i < index) else {
                    break;
                };
                if let Some(new_id) = imported[ancestor_index] {
                    parent = Some((ancestor_index, new_id));
                    break;
                }
                ancestor = source_entries[ancestor_index].parent_id;
            }
            let (parent_input_len, parent_id) = match parent {
                Some((ancestor_index, new_id)) => {
                    let parent_input = &source_entries[ancestor_index].input.actions;
                    if entry.input.actions.get(..parent_input.len())
                        != Some(parent_input.as_slice())
                    {
                        return Err("source archive entry does not extend its parent".into());
                    }
                    if ancestor_index != index_of[&entry.parent_id.unwrap_or(u64::MAX)] {
                        counts.rerooted = counts.rerooted.saturating_add(1);
                    }
                    (parent_input.len(), new_id)
                }
                None => {
                    counts.rerooted = counts.rerooted.saturating_add(1);
                    (0, genesis_id)
                }
            };
            let parent_entry = &self.archive.entries[parent_id];
            let mut milestones = parent_entry.milestones;
            let prefix = entry.input.clone();
            let suffix = prefix
                .actions
                .get(parent_input_len..)
                .ok_or("source archive input is shorter than its imported parent")?
                .to_vec();
            let checkpoint_snapshot = checkpointed.get(&entry.id);
            let checkpoint_certificate = checkpoint_snapshot
                .map(|snapshot| workload.candidate_replay_certificate(snapshot))
                .transpose()?
                .flatten();
            let mut replay_certificates = vec![None; suffix.len()];
            let reconstructed = if checkpoint_snapshot.is_none() || checkpoint_certificate.is_some()
            {
                workload.restore(
                    target,
                    parent_entry
                        .snapshot
                        .as_deref()
                        .ok_or("whole-tree import parent snapshot was released early")?,
                )?;
                let mut terminal = false;
                let mut captured_endpoint = None;
                for (index, action) in suffix.iter().enumerate() {
                    captured_endpoint =
                        workload.apply_action_and_snapshot(target, action, &mut milestones)?;
                    if workload
                        .rollout_outcome(run, target)?
                        .disposition
                        .is_terminal()
                    {
                        terminal = true;
                        break;
                    }
                    replay_certificates[index] = match captured_endpoint.as_ref() {
                        Some(snapshot) => workload.candidate_replay_certificate(snapshot)?,
                        None => workload.current_replay_certificate(target)?,
                    };
                }
                if terminal {
                    None
                } else {
                    let rebuilt = match captured_endpoint {
                        Some(snapshot) => snapshot,
                        None => workload.snapshot(target)?,
                    };
                    if workload.candidate_replay_certificate(&rebuilt)?.is_some()
                        && replay_certificates.iter().any(Option::is_none)
                    {
                        return Err(
                            "certified imported path lacks an action-prefix certificate".into()
                        );
                    }
                    Some(rebuilt)
                }
            } else {
                None
            };
            let snapshot = if let Some(snapshot) = checkpoint_snapshot {
                if checkpoint_certificate.is_some() {
                    let rebuilt = reconstructed.as_ref().ok_or(
                        "checkpointed source prefix stopped during certificate reconstruction",
                    )?;
                    if workload.candidate_replay_certificate(rebuilt)? != checkpoint_certificate
                        || workload.snapshot_identity_sha256(rebuilt)?
                            != workload.snapshot_identity_sha256(snapshot)?
                    {
                        return Err(
                            "checkpointed source prefix differs from its reconstructed endpoint"
                                .into(),
                        );
                    }
                }
                workload.restore(target, snapshot)?;
                milestones = merge_max(workload, milestones, entry.milestones);
                counts.checkpointed = counts.checkpointed.saturating_add(1);
                if workload
                    .rollout_outcome(run, target)?
                    .disposition
                    .is_terminal()
                {
                    None
                } else {
                    Some(snapshot.clone())
                }
            } else {
                reconstructed
            };
            let Some(snapshot) = snapshot else {
                if entry.snapshot_identity_sha256.is_some() {
                    return Err(
                        "imported endpoint with a source entry digest did not reconstruct".into(),
                    );
                }
                counts.terminal = counts.terminal.saturating_add(1);
                imported.push(None);
                continue;
            };
            if let Some(expected) = entry.snapshot_identity_sha256.as_deref()
                && workload.snapshot_identity_sha256(&snapshot)? != expected
            {
                return Err("imported endpoint differs from its source entry digest".into());
            }
            workload.merge_import_evidence(&mut self.evidence, milestones, &prefix);
            let key = workload.current_key(target)?;
            let inserted_before = self.archive.entries.len();
            match self
                .archive
                .insert_after_certified(
                    Some(parent_id),
                    None,
                    0,
                    ArchiveCandidate {
                        suffix,
                        key,
                        milestones,
                    },
                    snapshot,
                    Some(replay_certificates.as_slice()),
                )?
                .0
            {
                Some(id) if id == inserted_before => {
                    counts.imported = counts.imported.saturating_add(1);
                    imported.push(Some(id));
                }
                Some(id) => {
                    counts.duplicate = counts.duplicate.saturating_add(1);
                    imported.push(Some(id));
                }
                None => {
                    counts.rejected = counts.rejected.saturating_add(1);
                    imported.push(None);
                }
            }
        }
        if self.archive.entries.len() == 1 {
            return Err("whole-tree import retained no entry past genesis".into());
        }
        self.archive
            .preserve_inactive_snapshots(preserve_inactive_snapshots)?;
        workload.merge_origin_evidence(&mut self.evidence, source);
        Ok(counts)
    }

    #[cfg(test)]
    pub(crate) fn admit_job(
        &mut self,
        workload: &G,
        parent_id: u64,
        mut result: CampaignJobResult<G>,
    ) -> Result<(u64, Vec<CampaignAdmissionDecision>), Box<dyn Error>> {
        let (sequence, decisions, _) =
            self.admit_job_tracking(workload, parent_id, &mut result, false, |_| false)?;
        Ok((sequence, decisions))
    }

    fn admit_job_tracking(
        &mut self,
        workload: &G,
        parent_id: u64,
        result: &mut CampaignJobResult<G>,
        limited: bool,
        mut tracked_action: impl FnMut(&G::Action) -> bool,
    ) -> Result<(u64, Vec<CampaignAdmissionDecision>, DurationAdmission), Box<dyn Error>> {
        workload.verify_integrity()?;
        let CampaignJobResult::<G> {
            preparation_failure,
            actions,
        } = result;
        if preparation_failure.is_some() && !actions.is_empty() {
            return Err("preparation failure result also carries actions".into());
        }
        self.sequence = self.sequence.saturating_add(1);
        let sequence = self.sequence;
        let parent_index = self
            .archive
            .index_of_id(parent_id)
            .ok_or("campaign job parent is no longer resident")?;
        if let Some(observations) = preparation_failure {
            self.execution_failures = self.execution_failures.saturating_add(1);
            workload.merge_preparation_failure(&mut self.evidence, observations);
            return Ok((sequence, Vec::new(), DurationAdmission::default()));
        }
        let mut current_parent = parent_index;
        let mut pending_suffix = std::mem::take(&mut self.admission_suffix);
        let mut pending_certificates = Vec::new();
        let mut previous_key = None;
        let mut decisions = Vec::new();
        let mut duration = DurationAdmission::default();
        let actions_run = actions.len();
        let terminal = actions
            .last()
            .is_some_and(|action| action.outcome.disposition.is_terminal());
        let parent_place = self.archive.entries[parent_index].key.place();
        let retained_at_start = self.archive.retained;
        let mut left_place = false;
        for action in actions {
            let tracked = tracked_action(&action.action) && !action.outcome.disposition.is_failed();
            duration.applied |= tracked;
            let certificate = action
                .candidate
                .as_ref()
                .map(|candidate| workload.candidate_replay_certificate(&candidate.snapshot))
                .transpose()?
                .flatten();
            pending_suffix.push(action.action);
            if certificate.is_some() || !pending_certificates.is_empty() {
                pending_certificates.resize(pending_suffix.len() - 1, None);
                pending_certificates.push(certificate);
            }
            workload.merge_action_evidence(&mut self.evidence, action, sequence, || {
                let mut input = self
                    .archive
                    .materialize_input(current_parent)
                    .map_err(|error| -> Box<dyn Error> { error.into() })?;
                input.actions.extend_from_slice(&pending_suffix);
                Ok(input)
            })?;
            match action.outcome.disposition {
                crate::search::rollout::ExecutionDisposition::Terminal => {
                    self.terminal_endpoints = self.terminal_endpoints.saturating_add(1);
                    if action.outcome.objective_reached {
                        self.terminal_objectives = self.terminal_objectives.saturating_add(1);
                    }
                }
                crate::search::rollout::ExecutionDisposition::Failed => {
                    self.execution_failures = self.execution_failures.saturating_add(1);
                }
                crate::search::rollout::ExecutionDisposition::Runnable => {}
            }
            if action.outcome.objective_reached {
                duration.useful |= tracked;
                self.objectives_reached = self.objectives_reached.saturating_add(1);
                if self.objective_witness.is_none() {
                    let mut input = self
                        .archive
                        .materialize_input(current_parent)
                        .map_err(|error| -> Box<dyn Error> { error.into() })?;
                    input.actions.extend_from_slice(&pending_suffix);
                    self.objective_witness = Some(input);
                }
                decisions.push(CampaignAdmissionDecision::Objective);
            }
            if let Some(candidate) = action.candidate.as_ref() {
                let completed =
                    workload.complete_candidate_key(candidate.key, &candidate.snapshot)?;
                if !candidate.viable {
                    let key = self.archive.complete_after(
                        Some(current_parent),
                        previous_key,
                        completed,
                    )?;
                    left_place |= key.place() != parent_place;
                    self.probe_refused = self.probe_refused.saturating_add(1);
                    decisions.push(CampaignAdmissionDecision::ProbeRefused);
                    continue;
                }
                if certificate.is_some() && pending_certificates.iter().any(Option::is_none) {
                    return Err("certified campaign path lacks an action-prefix certificate".into());
                }
                let candidate = action
                    .candidate
                    .take()
                    .expect("validated candidate is present");
                let retained_before = self.archive.retained;
                let (admitted, key, unused) =
                    self.archive.insert_after_certified_returning_unused(
                        Some(current_parent),
                        previous_key,
                        sequence,
                        ArchiveCandidate {
                            suffix: pending_suffix.as_slice(),
                            key: completed,
                            milestones: action.milestones,
                        },
                        candidate.snapshot,
                        (!pending_certificates.is_empty())
                            .then_some(pending_certificates.as_slice()),
                    )?;
                if let Some(snapshot) = unused {
                    action.candidate = Some(CampaignCandidate::<G> {
                        key: candidate.key,
                        viable: candidate.viable,
                        snapshot,
                    });
                }
                left_place |= key.place() != parent_place;
                match admitted {
                    Some(id) if self.archive.retained > retained_before => {
                        duration.useful |= tracked;
                        decisions.push(CampaignAdmissionDecision::Retained {
                            id: self
                                .archive
                                .stable_id(id)
                                .ok_or("retained archive slot is missing")?,
                        });
                        current_parent = id;
                        pending_suffix.clear();
                        pending_certificates.clear();
                        previous_key = None;
                    }
                    Some(id) => {
                        decisions.push(CampaignAdmissionDecision::Duplicate {
                            id: self
                                .archive
                                .stable_id(id)
                                .ok_or("duplicate archive slot is missing")?,
                        });
                        current_parent = id;
                        pending_suffix.clear();
                        pending_certificates.clear();
                        previous_key = None;
                    }
                    None => {
                        decisions.push(CampaignAdmissionDecision::Rejected);
                        previous_key = Some(key);
                    }
                }
            }
        }
        if limited {
            self.archive.record_suffix_outcome(
                parent_index,
                actions_run,
                self.archive.retained > retained_at_start,
                left_place,
                terminal,
            );
        }
        if sequence.is_multiple_of(self.curve_interval) {
            self.push_curve_point();
            self.compact_progress_curve_if_needed();
        }
        pending_suffix.clear();
        if pending_suffix.capacity() <= usize::from(SUFFIX_DOUBLING_LIMIT) {
            self.admission_suffix = pending_suffix;
        }
        Ok((sequence, decisions, duration))
    }

    fn push_curve_point(&mut self) {
        self.curve.push(ProgressPoint {
            executions: self.sequence,
            milestones: self.aggregate_milestones(),
            progress: self
                .record_progress
                .then(|| G::aggregate_progress(&self.evidence)),
            active_entries: self.archive.active_count(),
            occupied_cells: self.archive.occupied_cell_count(),
            terminal_endpoints: self.terminal_endpoints,
            execution_failures: self.execution_failures,
        });
    }

    fn compact_progress_curve_if_needed(&mut self) {
        if !self.bounded_progress_curve {
            return;
        }
        self.curve_interval = compact_progress_curve(&mut self.curve, self.curve_interval);
    }

    fn aggregate_milestones(&self) -> G::Milestones {
        G::aggregate_milestones(&self.evidence)
    }

    fn finish_curve(&mut self) {
        if self.sequence > 0
            && self.curve.last().map(|point| point.executions) != Some(self.sequence)
        {
            self.push_curve_point();
        }
    }

    fn all_prefixes_archived(&self, parent_index: usize, suffix: &[G::Action]) -> bool {
        !suffix.is_empty() && self.archive.all_extensions_retained(parent_index, suffix)
    }

    pub(crate) fn into_archive_report_and_snapshots(
        mut self,
        workload: &G,
        campaign_seed: u64,
        materialize_final_artifacts: bool,
    ) -> Result<ArchiveMaterialization<G>, Box<dyn Error>> {
        let selector = self.archive.selector_report();
        let (mut entries, snapshots) = if materialize_final_artifacts {
            self.archive.take_entry_reports_and_snapshots()
        } else {
            (Vec::new(), Vec::new())
        };
        let entry_indexes = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id, index))
            .collect::<BTreeMap<_, _>>();
        if entry_indexes.len() != entries.len() {
            return Err("archive report contains duplicate entry IDs".into());
        }
        for (id, snapshot) in &snapshots {
            let index = entry_indexes
                .get(id)
                .ok_or("archive snapshot has no matching entry")?;
            let entry = &mut entries[*index];
            if entry.snapshot_identity_sha256.is_some() {
                return Err("archive report contains duplicate snapshot IDs".into());
            }
            entry.snapshot_identity_sha256 = Some(workload.snapshot_identity_sha256(snapshot)?);
        }
        let report = workload.archive_report(
            &self.evidence,
            ArchiveReportState {
                seed: campaign_seed,
                executions: self.sequence,
                entries,
                progress_curve: self.curve,
                retained: self.archive.retained,
                rejected: self.archive.rejected,
                terminal_endpoints: self.terminal_endpoints,
                terminal_objectives: self.terminal_objectives,
                execution_failures: self.execution_failures,
                selector,
            },
        );
        Ok((report, snapshots))
    }
}

fn merge_max<G: Workload + ?Sized>(
    workload: &G,
    mut base: G::Milestones,
    other: G::Milestones,
) -> G::Milestones {
    workload.merge_milestones(&mut base, other);
    base
}

fn resolve_origin<G: Workload>(
    workload: &G,
    config: &CampaignConfig<G>,
    origin: &CampaignOrigin<G>,
    search_checkpoint: Option<&CheckpointReader>,
    executable_identity: &str,
) -> Result<CampaignOriginRecord, Box<dyn Error>> {
    if let CampaignOrigin::SearchCheckpoint { path, .. } = origin {
        let reader = search_checkpoint.ok_or("a search checkpoint origin was not opened")?;
        if reader.header.executable_identity != executable_identity {
            return Err("search checkpoint belongs to a different executable".into());
        }
        let checkpoint_policy_changes = search_checkpoint_policy_changes(
            &reader.header,
            &search_checkpoint_header(
                workload,
                config,
                executable_identity,
                &reader.header.reason,
                0,
                0,
                0,
                0,
            ),
        )?;
        let path = path.display().to_string();
        let resume_input_sha256 = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&Input::<G::Action>::default())?)
        );
        return Ok(CampaignOriginRecord {
            kind: ORIGIN_SEARCH_CHECKPOINT.to_owned(),
            path: None,
            archive_sha256: None,
            checkpoint_path: Some(path),
            checkpoint_sha256: Some(reader.file_sha256.clone()),
            checkpoint_policy_changes,
            resume_input_sha256,
            resume_actions: 0,
        });
    }
    let (kind, path, archive_sha256, checkpoint, resume_input) = match origin {
        CampaignOrigin::SearchCheckpoint { .. } => {
            return Err("a search checkpoint origin was not opened".into());
        }
        CampaignOrigin::Genesis => (
            ORIGIN_GENESIS.to_owned(),
            None,
            None,
            None,
            Input::default(),
        ),
        CampaignOrigin::SnapshotRoot { checkpoint } => (
            ORIGIN_SNAPSHOT_ROOT.to_owned(),
            None,
            None,
            Some(checkpoint),
            Input::default(),
        ),
        CampaignOrigin::Archive {
            path,
            source,
            checkpoint,
        } => {
            if let Some(checkpoint) = checkpoint {
                validate_archive_checkpoint(workload, checkpoint)?;
            }
            (
                ORIGIN_ARCHIVE.to_owned(),
                Some(path.clone()),
                Some(source.file_sha256().to_owned()),
                checkpoint.as_ref(),
                workload.resume_input(source.report())?,
            )
        }
    };
    let resume_input_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&resume_input)?));
    Ok(CampaignOriginRecord {
        kind,
        path,
        archive_sha256,
        checkpoint_path: checkpoint.map(|checkpoint| checkpoint.path.clone()),
        checkpoint_sha256: checkpoint.map(|checkpoint| checkpoint.file_sha256.clone()),
        checkpoint_policy_changes: BTreeMap::new(),
        resume_input_sha256,
        resume_actions: resume_input.actions.len(),
    })
}

pub const PREFERENCE_PORTFOLIO_PREFIX: &str = "preference_portfolio_v1:";

#[must_use]
pub fn preference_portfolio_identifier<K: ArchiveKey>() -> String {
    format!(
        "{PREFERENCE_PORTFOLIO_PREFIX}{},{}",
        K::preferences(),
        K::capacity().max(1)
    )
}

fn stream_header<G: Workload>(
    workload: &G,
    config: &CampaignConfig<G>,
    origin: &CampaignOriginRecord,
    draw_header: Option<DrawTableHeader>,
    executable_identity: &str,
) -> CampaignStreamHeader<DrawTableHeader> {
    CampaignStreamHeader {
        schema_version: CAMPAIGN_SCHEMA_VERSION,
        format: workload.stream_format().to_owned(),
        campaign_seed: config.campaign_seed,
        schedule_policy: schedule_policy_identifier(config.window),
        progress_policy: CAMPAIGN_PROGRESS_POLICY.to_owned(),
        host: config.host.clone(),
        origin_kind: origin.kind.clone(),
        origin_path: origin.path.clone(),
        origin_archive_sha256: origin.archive_sha256.clone(),
        origin_checkpoint_path: origin.checkpoint_path.clone(),
        origin_checkpoint_sha256: origin.checkpoint_sha256.clone(),
        origin_checkpoint_policy_changes: origin.checkpoint_policy_changes.clone(),
        resume_input_sha256: origin.resume_input_sha256.clone(),
        resume_actions: origin.resume_actions,
        execution_budget: config.execution_budget,
        stop_rollout_on_objective: config.stop_rollout_on_objective,
        stop_campaign_on_objective: config.stop_campaign_on_objective,
        work_budget: None,
        wall_budget_seconds: config.wall_budget.map(|budget| budget.as_secs()),
        archive_entry_limit: config.archive_entry_limit,
        memory_budget_mib: config.memory_budget_mib,
        resume_policy: if origin.kind == ORIGIN_SNAPSHOT_ROOT {
            SNAPSHOT_ROOT_RESUME_IDENTIFIER.to_owned()
        } else {
            RESUME_IDENTIFIER.to_owned()
        },
        mixture_policy: draw_mixture_identifier(config.mixture),
        workload_policies: recorded_policies(workload, &config.run),
        draw_header,
        retention_policy: retention_policy_identifier(config.retention).to_owned(),
        parent_scheduler: SELECTOR_IDENTIFIER.to_owned(),
        preference_portfolio: preference_portfolio_identifier::<G::Key>(),
        executor_mode: EXECUTOR_MODE.to_owned(),
        selection_seed_derivation: SELECTION_SEED_DERIVATION.to_owned(),
        workload_identity_sha256: workload.workload_identity_sha256(),
        executable_identity: executable_identity.to_owned(),
        action_cost_unit: workload.action_cost_unit().to_owned(),
        execution_work_unit: workload.execution_work_unit().to_owned(),
    }
}

#[path = "campaign_stream.rs"]
mod campaign_stream;
use campaign_stream::StreamWriter;

#[derive(Deserialize, Serialize)]
struct CampaignCounters {
    bootstrap_execution_work: u64,
    #[serde(with = "crate::search::checkpoint::json_bytes")]
    tree_import: Option<TreeImportCounts>,
    job_execution_work: u64,
    work_to_first_objective: Option<u64>,
    executions_to_first_objective: Option<u64>,
    duplicates_skipped: u64,
    draw_state_memory_bytes: usize,
}

fn execution_work_delta(before: u64, after: u64, scope: &str) -> Result<u64, Box<dyn Error>> {
    after
        .checked_sub(before)
        .ok_or_else(|| format!("{scope} execution-work counter rewound").into())
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct DurationAdmission {
    applied: bool,
    useful: bool,
}

struct DurationAdmissionRecord<C> {
    sequence: u64,
    context: C,
    before: Option<DurationCheckpoint>,
    evicted: Option<DurationContextCheckpoint<C>>,
}

fn duration_checkpoint_at_reservation<C: Copy + Ord>(
    policies: &DurationPolicies<C>,
    admissions: &VecDeque<DurationAdmissionRecord<C>>,
    context: C,
    sequence: u64,
) -> Option<DurationCheckpoint> {
    for admission in admissions
        .iter()
        .filter(|admission| admission.sequence > sequence)
    {
        if admission.context == context {
            return admission.before.clone();
        }
        if admission
            .evicted
            .as_ref()
            .is_some_and(|evicted| evicted.context == context)
        {
            return admission
                .evicted
                .as_ref()
                .map(|evicted| evicted.history.clone());
        }
    }
    policies.context_checkpoint(context)
}

#[allow(clippy::too_many_arguments)]
fn validate_duration_reservation<C>(
    draw: Option<&DurationDraw<C>>,
    admission_sequence: Option<u64>,
    recorded_remaining_work: Option<NonZeroU64>,
    current_sequence: u64,
    selection_sequence: u64,
    campaign_work_budget: Option<u64>,
    current_work: u64,
    work_history: &VecDeque<(u64, u64)>,
) -> Result<Option<NonZeroU64>, Box<dyn Error>> {
    let Some(_draw) = draw else {
        if admission_sequence.is_some() {
            return Err("duration reservation sequence is missing its duration choice".into());
        }
        if recorded_remaining_work.is_some() {
            return Err("recorded stream carries remaining duration work without a choice".into());
        }
        return Ok(None);
    };
    let Some(admission_sequence) = admission_sequence else {
        return Err("recorded duration choice is missing its admission sequence".into());
    };
    if admission_sequence > current_sequence {
        return Err("duration choice claims a future admission sequence".into());
    }
    if admission_sequence != selection_sequence {
        return Err("duration choice was not drawn at its selection point".into());
    }
    let work_at_draw = if admission_sequence == current_sequence {
        Some(current_work)
    } else {
        work_history
            .iter()
            .find(|(sequence, _)| *sequence == admission_sequence)
            .map(|(_, work)| *work)
    }
    .ok_or("duration choice has no reconstructed reservation work")?;
    let expected_remaining_work = campaign_work_budget
        .and_then(|budget| NonZeroU64::new(budget.saturating_sub(work_at_draw)));
    if expected_remaining_work != recorded_remaining_work {
        return Err("recorded duration work diverges from its reservation sequence".into());
    }
    Ok(expected_remaining_work)
}

fn verify_duration_draw(
    draw: &DurationDraw<impl Copy>,
    checkpoint: Option<&DurationCheckpoint>,
    mutation_seed: u64,
) -> Result<(), Box<dyn Error>> {
    let policy = match checkpoint {
        Some(checkpoint) => DurationPolicy::from_checkpoint(checkpoint.clone())?,
        None => DurationPolicy::new(),
    };
    let mut rand = RomuDuoJrRand::with_seed(mutation_seed);
    let expected = policy.draw(&mut rand, draw.max_duration);
    if expected != draw.duration.get() {
        return Err("recorded duration choice diverges from its draw-time policy state".into());
    }
    Ok(())
}

fn validate_duration_draw<G: Workload + ?Sized>(
    workload: &G,
    run: &G::Run,
    parent: G::Key,
    campaign_work_budget: Option<u64>,
    remaining_work: Option<NonZeroU64>,
    recorded: Option<DurationDraw<G::Key>>,
) -> Result<Option<DurationDraw<G::Key>>, Box<dyn Error>> {
    if let Some(remaining_work) = remaining_work {
        let Some(campaign_work_budget) = campaign_work_budget else {
            return Err("recorded duration work has no campaign budget".into());
        };
        if remaining_work.get() > campaign_work_budget {
            return Err("recorded duration work exceeds its campaign budget".into());
        }
    }
    let request = workload.duration_request(run, parent, remaining_work);
    match (request, recorded) {
        (None, None) => Ok(None),
        (Some(_), None) => Err("recorded stream is missing its duration choice".into()),
        (None, Some(_)) => Err("recorded stream carries an unexpected duration choice".into()),
        (Some(request), Some(draw)) => {
            if draw.context != request.context {
                return Err("recorded duration context diverges from the workload request".into());
            }
            if draw.max_duration != request.max_duration {
                return Err("recorded duration maximum diverges from the workload request".into());
            }
            if draw.duration > draw.max_duration || !draw.duration.get().is_power_of_two() {
                return Err("recorded duration choice is outside its bounded alphabet".into());
            }
            Ok(Some(draw))
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
struct LiveCoordinatorProfile {
    #[serde(default)]
    timing_complete: bool,
    receive_wait_ns: u128,
    admission_ns: u128,
    bookkeeping_ns: u128,
    history_compaction_ns: u128,
    stream_write_ns: u128,
    selection_ns: u128,
    receives: u64,
    admissions: u64,
    selections: u64,
    replay_jobs: u64,
    replay_actions: u64,
    replay_cost: u64,
    suffix_actions: u64,
    suffix_cost: u64,
    splice_jobs: u64,
    splice_actions: u64,
    splice_cost: u64,
}

impl LiveCoordinatorProfile {
    fn new() -> Self {
        Self {
            timing_complete: profile_clock_enabled(),
            ..Self::default()
        }
    }

    fn telemetry_json(&self) -> Result<serde_json::Value, serde_json::Error> {
        let mut value = serde_json::to_value(self)?;
        if !self.timing_complete {
            for field in [
                "admission_ns",
                "bookkeeping_ns",
                "history_compaction_ns",
                "stream_write_ns",
                "selection_ns",
            ] {
                value[field] = serde_json::Value::Null;
            }
        }
        Ok(value)
    }

    fn note_dispatch<G: Workload + ?Sized>(
        &mut self,
        spec: &JobSpec<G>,
        spliced: bool,
        cost: fn(&G::Action) -> u64,
    ) {
        let total = |actions: &[G::Action]| actions.iter().map(cost).sum::<u64>();
        if !spec.replay.is_empty() {
            self.replay_jobs = self.replay_jobs.saturating_add(1);
        }
        self.replay_actions = self
            .replay_actions
            .saturating_add(u64::try_from(spec.replay.len()).unwrap_or(u64::MAX));
        self.replay_cost = self.replay_cost.saturating_add(total(&spec.replay));
        self.suffix_actions = self
            .suffix_actions
            .saturating_add(u64::try_from(spec.suffix.len()).unwrap_or(u64::MAX));
        self.suffix_cost = self.suffix_cost.saturating_add(total(&spec.suffix));
        if spliced {
            self.splice_jobs = self.splice_jobs.saturating_add(1);
            self.splice_actions = self
                .splice_actions
                .saturating_add(u64::try_from(spec.suffix.len()).unwrap_or(u64::MAX));
            self.splice_cost = self.splice_cost.saturating_add(total(&spec.suffix));
        }
    }
}

fn record_compaction_elapsed(
    profile: &mut LiveCoordinatorProfile,
    before: u64,
    after: u64,
    elapsed_ns: u128,
) {
    if after > before {
        profile.history_compaction_ns = profile.history_compaction_ns.saturating_add(elapsed_ns);
    }
}

fn profile_clock_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("HARMONY_COORDINATOR_PROFILE").is_some())
}

fn profile_now() -> Option<Moment> {
    profile_clock_enabled().then(now)
}

fn profile_elapsed(started: Option<Moment>) -> u128 {
    started.map_or(0, |started| started.elapsed().as_nanos())
}

impl CampaignCounters {
    fn note_first_objective(&mut self, sequence: u64) {
        if self.work_to_first_objective.is_none() {
            self.work_to_first_objective = Some(
                self.bootstrap_execution_work
                    .saturating_add(self.job_execution_work),
            );
            self.executions_to_first_objective = Some(sequence);
        }
    }

    fn new() -> Self {
        Self {
            bootstrap_execution_work: 0,
            tree_import: None,
            job_execution_work: 0,
            work_to_first_objective: None,
            executions_to_first_objective: None,
            duplicates_skipped: 0,
            draw_state_memory_bytes: 0,
        }
    }
}

fn build_report<G: Workload>(
    workload: &G,
    header: &CampaignStreamHeader<DrawTableHeader>,
    origin: CampaignOriginRecord,
    core: CoordinatorCore<G>,
    counters: &CampaignCounters,
    stream_sha256: String,
    materialize_final_artifacts: bool,
) -> Result<CampaignOutcome<G>, Box<dyn Error>> {
    workload.verify_integrity()?;
    let executions_completed = core.sequence;
    let probe_refused = core.probe_refused;
    let terminal_endpoints = core.terminal_endpoints;
    let terminal_objectives = core.terminal_objectives;
    let execution_failures = core.execution_failures;
    let objectives_reached = core.objectives_reached;
    let objective_witness = core.objective_witness.clone();
    let replacement_cost_displaced = core.archive.replacement_cost_displaced();
    let snapshot_evictions = core.archive.snapshot_evictions();
    let resident_snapshot_bytes = core.archive.resident_snapshot_bytes();
    let history_memory_bytes = core.archive.history_memory_bytes();
    let draw_state_memory_bytes = counters.draw_state_memory_bytes;
    let resident_memory_bytes = core
        .archive
        .resident_memory_bytes()
        .saturating_add(draw_state_memory_bytes);
    let live_entries = core.archive.live_entry_count();
    let history_compactions = core.archive.history_compactions();
    #[cfg(test)]
    let liveness_anchor_reactivations = core.archive.liveness_anchor_reactivations();
    let historical_entries_dropped = core.archive.historical_entries_dropped();
    let input_reconstructions = core.archive.input_reconstructions();
    let input_index_nodes = core.archive.input_index_nodes();
    let historical_cells = core.archive.historical_cell_count();
    let (archive, snapshots) = core.into_archive_report_and_snapshots(
        workload,
        header.campaign_seed,
        materialize_final_artifacts,
    )?;
    let checkpoint = SnapshotCheckpoint {
        format: workload.checkpoint_format().to_owned(),
        entries: snapshots
            .into_iter()
            .map(|(id, snapshot)| SnapshotCheckpointEntry { id, snapshot })
            .collect(),
    };
    let report = CampaignModeReport {
        mode: "campaign".to_owned(),
        campaign_seed: header.campaign_seed,
        window: schedule_policy_window(&header.schedule_policy).unwrap_or_default(),
        host: header.host.clone(),
        schedule_identity: CAMPAIGN_SCHEDULE_IDENTITY.to_owned(),
        origin,
        execution_budget: header.execution_budget,
        stop_rollout_on_objective: header.stop_rollout_on_objective,
        stop_campaign_on_objective: header.stop_campaign_on_objective,
        work_budget: header.work_budget,
        executions_completed,
        executions_to_first_objective: counters.executions_to_first_objective,
        wall_budget_seconds: header.wall_budget_seconds,
        archive_entry_limit: header.archive_entry_limit,
        memory_budget_mib: header.memory_budget_mib,
        resume_policy: header.resume_policy.clone(),
        mixture_policy: header.mixture_policy.clone(),
        workload_policies: header.workload_policies.clone(),
        retention_policy: header.retention_policy.clone(),
        parent_scheduler: header.parent_scheduler.clone(),
        executor_mode: header.executor_mode.clone(),
        selection_seed_derivation: header.selection_seed_derivation.clone(),
        workload_identity_sha256: header.workload_identity_sha256.clone(),
        executable_identity: header.executable_identity.clone(),
        action_cost_unit: header.action_cost_unit.clone(),
        execution_work_unit: header.execution_work_unit.clone(),
        bootstrap_execution_work: counters.bootstrap_execution_work,
        tree_import: counters.tree_import,
        execution_work: counters
            .bootstrap_execution_work
            .saturating_add(counters.job_execution_work),
        work_to_first_objective: counters.work_to_first_objective,
        duplicates_skipped: counters.duplicates_skipped,
        probe_refused,
        terminal_endpoints,
        terminal_objectives,
        execution_failures,
        objectives_reached,
        objective_witness,
        replacement_cost_displaced,
        snapshot_evictions,
        resident_snapshot_bytes,
        history_memory_bytes,
        draw_state_memory_bytes,
        resident_memory_bytes,
        live_entries,
        history_compactions,
        #[cfg(test)]
        liveness_anchor_reactivations,
        historical_entries_dropped,
        input_reconstructions,
        input_index_nodes,
        historical_cells,
        stream_sha256,
        archive,
        telemetry: CampaignTelemetry::default(),
    };
    Ok((report, checkpoint))
}

struct PostcardSha256(Sha256);

impl postcard::ser_flavors::Flavor for PostcardSha256 {
    type Output = sha2::digest::Output<Sha256>;

    fn try_extend(&mut self, data: &[u8]) -> postcard::Result<()> {
        self.0.update(data);
        Ok(())
    }

    fn try_push(&mut self, data: u8) -> postcard::Result<()> {
        self.0.update([data]);
        Ok(())
    }

    fn finalize(self) -> postcard::Result<Self::Output> {
        Ok(self.0.finalize())
    }
}

pub fn postcard_result_sha256<G: Workload + ?Sized>(
    result: &CampaignJobResult<G>,
) -> Result<String, Box<dyn Error>> {
    postcard_value_sha256(result)
}

#[derive(Serialize)]
struct SemanticCandidate<'a, K> {
    key: &'a K,
    viable: bool,
}

#[derive(Serialize)]
struct SemanticAction<'a, A, O, M, K> {
    action: &'a A,
    observations: &'a [O],
    milestones: &'a M,
    outcome: crate::search::rollout::Outcome,
    candidate: Option<SemanticCandidate<'a, K>>,
}

#[derive(Serialize)]
struct SemanticResult<'a, A, O, M, K> {
    preparation_failure: &'a Option<Vec<O>>,
    actions: Vec<SemanticAction<'a, A, O, M, K>>,
}

pub fn postcard_semantic_result_sha256<G: CampaignTypes + ?Sized>(
    result: &CampaignJobResult<G>,
) -> Result<String, Box<dyn Error>> {
    postcard_value_sha256(&SemanticResult {
        preparation_failure: &result.preparation_failure,
        actions: result
            .actions
            .iter()
            .map(|action| SemanticAction {
                action: &action.action,
                observations: &action.observations,
                milestones: &action.milestones,
                outcome: action.outcome,
                candidate: action
                    .candidate
                    .as_ref()
                    .map(|candidate| SemanticCandidate {
                        key: &candidate.key,
                        viable: candidate.viable,
                    }),
            })
            .collect(),
    })
}

pub fn postcard_value_sha256<T: Serialize + ?Sized>(value: &T) -> Result<String, Box<dyn Error>> {
    let digest = postcard::serialize_with_flavor::<_, PostcardSha256, sha2::digest::Output<Sha256>>(
        value,
        PostcardSha256(Sha256::new()),
    )?;
    Ok(format!("{digest:x}"))
}

type SelectedJob<G> = (JobSpec<G>, PendingJob<G>);

fn planned_finish<G: Workload + ?Sized>(
    clock: u64,
    spec: &JobSpec<G>,
    cost: fn(&G::Action) -> u64,
) -> u64 {
    spec.replay
        .iter()
        .chain(&spec.suffix)
        .map(cost)
        .fold(clock, u64::saturating_add)
}

#[derive(Deserialize, Serialize)]
#[serde(bound = "")]
struct JobSpecRecord<G: Workload + ?Sized> {
    replay: Vec<G::Action>,
    replay_certificates: Vec<Option<[u8; 32]>>,
    parent_milestones: G::Milestones,
    suffix: Vec<G::Action>,
}

impl<G: Workload + ?Sized> JobSpecRecord<G> {
    fn of(spec: &JobSpec<G>) -> Self {
        Self {
            replay: spec.replay.clone(),
            replay_certificates: spec.replay_certificates.clone(),
            parent_milestones: spec.parent_milestones,
            suffix: spec.suffix.clone(),
        }
    }
}

#[derive(Serialize)]
#[serde(bound = "")]
struct InFlightJobRef<'a, G: Workload + ?Sized> {
    reservation: usize,
    spec: &'a JobSpecRecord<G>,
    pending: &'a PendingJob<G>,
}

#[derive(Deserialize)]
#[serde(bound = "")]
struct InFlightJob<G: Workload + ?Sized> {
    reservation: usize,
    spec: JobSpecRecord<G>,
    pending: PendingJob<G>,
}

#[derive(Deserialize, Serialize)]
#[serde(bound = "")]
struct CoreCheckpoint<G: Workload + ?Sized> {
    #[serde(with = "crate::search::checkpoint::json_bytes")]
    curve: Vec<ProgressPoint<G::Milestones, G::Progress>>,
    curve_interval: u64,
    bounded_progress_curve: bool,
    record_progress: bool,
    terminal_endpoints: u64,
    terminal_objectives: u64,
    execution_failures: u64,
    objectives_reached: u64,
    objective_witness: Option<Input<G::Action>>,
    sequence: u64,
    probe_refused: u64,
    mixture_energy: MixtureEnergy,
}

struct SearchResume<G: Workload + ?Sized> {
    rand: RomuDuoJrRand,
    reserved: u64,
    admitted: usize,
    planned_clock: u64,
    profile: LiveCoordinatorProfile,
    in_flight: Vec<InFlightJob<G>>,
}

#[derive(Deserialize, Serialize)]
#[serde(bound = "")]
struct PendingJob<G: Workload + ?Sized> {
    snapshot_id: u64,
    parent_id: u64,
    continuation_energy: Option<u16>,
    continuation_wave: u32,
    continuation_edge: Option<Edge<G::Key>>,
    mutation_seed: u64,
    mixture_weight: u8,
    splice_weight: u8,
    #[serde(with = "crate::search::checkpoint::json_bytes")]
    splice: Option<CampaignSpliceRecord>,
    #[serde(with = "crate::search::checkpoint::json_bytes")]
    selector: SelectorDraw,
    draw_checkpoint_before: Option<EmpiricalStepCheckpoint>,
    duration_draw: Option<DurationDraw<G::Key>>,
    duration_admission_sequence_at_draw: Option<u64>,
    duration_remaining_work: Option<NonZeroU64>,
    duration_checkpoint_at_draw: Option<DurationCheckpoint>,
    suffix_limit: Option<u8>,
    planned_finish: u64,
}

struct CompletedJob<G: Workload + ?Sized> {
    held_since: Option<Moment>,
    token: ReservationToken,
    outcome: Result<(CampaignJobResult<G>, u64, String), String>,
}

#[allow(clippy::disallowed_types)]
type HostLookupMap<K, V> = std::collections::HashMap<K, V>;

struct ReservationLookup<K, V> {
    map: HostLookupMap<K, V>,
}

impl<K: Eq + Hash, V> ReservationLookup<K, V> {
    #[cfg(test)]
    fn with_capacity_and_hasher(
        capacity: usize,
        state: std::collections::hash_map::RandomState,
    ) -> Self {
        Self {
            map: HostLookupMap::with_capacity_and_hasher(capacity, state),
        }
    }

    fn get(&self, key: &K) -> Option<&V> {
        self.map.get(key)
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        self.map.remove(key)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ReservationToken {
    slot: usize,
    generation: u64,
}

struct AdmissionSchedule {
    heap: BinaryHeap<Reverse<((u64, usize), ReservationToken)>>,
}

impl AdmissionSchedule {
    fn try_new(capacity: usize) -> Result<Self, std::collections::TryReserveError> {
        let mut heap = BinaryHeap::new();
        heap.try_reserve(capacity)?;
        Ok(Self { heap })
    }
    fn insert(&mut self, key: (u64, usize), token: ReservationToken) {
        self.heap.push(Reverse((key, token)));
    }
    fn first(&self) -> Option<&(u64, usize)> {
        self.heap.peek().map(|Reverse((key, _))| key)
    }
    fn first_token(&self) -> Option<ReservationToken> {
        self.heap.peek().map(|Reverse((_, token))| *token)
    }
    fn pop_first(&mut self) {
        self.heap.pop();
    }
    fn validate<P, V>(
        &self,
        reservations: &ReservationSlots<usize, P, V>,
        finish: impl Fn(&P) -> u64,
    ) -> Result<(), &'static str> {
        if reservations.len() != self.len() {
            return Err("search checkpoint reservation count differs");
        }
        let words = reservations.slots.len().div_ceil(64);
        let mut seen = Vec::new();
        seen.try_reserve_exact(words)
            .map_err(|_| "search checkpoint validation capacity cannot be allocated")?;
        seen.resize(words, 0_u64);
        for Reverse(((planned, reservation), token)) in &self.heap {
            let slot = reservations
                .slot(*token)
                .ok_or("search checkpoint lost an in-flight reservation")?;
            let bit = 1_u64 << (token.slot % 64);
            let word = &mut seen[token.slot / 64];
            if slot.key != *reservation || finish(&slot.pending) != *planned || *word & bit != 0 {
                return Err("search checkpoint in-flight schedule differs");
            }
            *word |= bit;
        }
        Ok(())
    }

    fn len(&self) -> usize {
        self.heap.len()
    }
    fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }
}

struct ReservationSlots<K: Eq + Hash + Copy, P, V> {
    entries: ReservationLookup<K, ReservationToken>,
    slots: Vec<ReservationCell<K, P, V>>,
    free: Vec<usize>,
    pending: usize,
    admitting: usize,
    inserted: Option<ReservationToken>,
    telemetry: CompletedBufferTelemetry,
    records: fn(&V) -> (u64, u64),
}

struct ReservationCell<K, P, V> {
    generation: u64,
    entry: Option<ReservationSlot<K, P, V>>,
}

struct ReservationSlot<K, P, V> {
    key: K,
    pending: P,
    completed: Option<BufferedResult<V>>,
    admitting: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CompletedRecords {
    actions: u64,
    candidates: u64,
}

impl CompletedRecords {
    fn of_result<G: CampaignTypes + ?Sized>(result: &CampaignJobResult<G>) -> Self {
        Self {
            actions: result.actions.len() as u64,
            candidates: result
                .actions
                .iter()
                .filter(|action| action.candidate.is_some())
                .count() as u64,
        }
    }

    fn after_watch<G: CampaignTypes + ?Sized, N>(
        outcome: &Result<(JobOutcome<G>, N), String>,
    ) -> Self {
        outcome.as_ref().map_or_else(
            |_| Self::default(),
            |(outcome, _)| Self::of_result::<G>(&outcome.result),
        )
    }
}

struct BufferedResult<V> {
    value: V,
    actions: u64,
    candidates: u64,
}

struct CompletedOutcome<G: Workload + ?Sized> {
    held_since: Option<Moment>,
    outcome: Result<(CampaignJobResult<G>, u64, String), String>,
}

impl<K: Eq + Hash + Copy, P, V> ReservationSlots<K, P, V> {
    #[cfg(test)]
    fn new(records: fn(&V) -> (u64, u64), capacity: usize) -> Self {
        Self::try_new(records, capacity).expect("test reservation capacity")
    }

    fn try_new(
        records: fn(&V) -> (u64, u64),
        capacity: usize,
    ) -> Result<Self, std::collections::TryReserveError> {
        let mut entries = ReservationLookup {
            map: HostLookupMap::new(),
        };
        entries.map.try_reserve(capacity)?;
        let mut slots = Vec::new();
        slots.try_reserve_exact(capacity)?;
        slots.resize_with(capacity, || ReservationCell {
            generation: 0,
            entry: None,
        });
        let mut free = Vec::new();
        free.try_reserve_exact(capacity)?;
        free.extend((0..capacity).rev());
        Ok(Self {
            entries,
            slots,
            free,
            pending: 0,
            admitting: 0,
            inserted: None,
            telemetry: CompletedBufferTelemetry::default(),
            records,
        })
    }

    fn token(&self, key: &K) -> Option<ReservationToken> {
        self.entries.get(key).copied()
    }

    fn len(&self) -> usize {
        self.slots.len() - self.free.len()
    }

    fn slot(&self, token: ReservationToken) -> Option<&ReservationSlot<K, P, V>> {
        self.slots
            .get(token.slot)
            .filter(|cell| cell.generation == token.generation)
            .and_then(|cell| cell.entry.as_ref())
    }

    fn slot_mut(&mut self, token: ReservationToken) -> Option<&mut ReservationSlot<K, P, V>> {
        self.slots
            .get_mut(token.slot)
            .filter(|cell| cell.generation == token.generation)
            .and_then(|cell| cell.entry.as_mut())
    }

    fn validate_pending(&self, token: ReservationToken, key: K) -> Result<(), &'static str> {
        let slot = self
            .slot(token)
            .ok_or("campaign worker replied with a stale reservation token")?;
        if slot.key != key {
            return Err("campaign worker replied for another reservation");
        }
        if slot.completed.is_some() || slot.admitting {
            return Err("campaign worker completed one reservation twice");
        }
        Ok(())
    }

    fn insert(&mut self, key: K, pending: P) -> Option<P> {
        let entry = match self.entries.map.entry(key) {
            std::collections::hash_map::Entry::Occupied(_) => return Some(pending),
            std::collections::hash_map::Entry::Vacant(entry) => entry,
        };
        let index = self.free.pop().unwrap_or_else(|| {
            let index = self.slots.len();
            self.slots.push(ReservationCell {
                generation: 0,
                entry: None,
            });
            index
        });
        let cell = &mut self.slots[index];
        assert!(cell.entry.is_none());
        let token = ReservationToken {
            slot: index,
            generation: cell.generation,
        };
        cell.entry = Some(ReservationSlot {
            key,
            pending,
            completed: None,
            admitting: false,
        });
        entry.insert(token);
        self.inserted = Some(token);
        self.pending += 1;
        None
    }

    fn pending(&self, key: &K) -> Option<&P> {
        self.slot(self.token(key)?)
            .filter(|slot| slot.completed.is_none() && !slot.admitting)
            .map(|slot| &slot.pending)
    }

    #[cfg(test)]
    fn metadata(&self, key: &K) -> Option<&P> {
        self.slot(self.token(key)?).map(|slot| &slot.pending)
    }

    fn metadata_token_mut(&mut self, token: ReservationToken) -> Option<&mut P> {
        self.slot_mut(token).map(|slot| &mut slot.pending)
    }

    #[cfg(test)]
    fn complete(
        &mut self,
        key: K,
        value: V,
        validate: impl FnOnce(&P) -> Result<(), &'static str>,
    ) -> Result<(), &'static str> {
        self.complete_with_records(key, value, None, validate)
    }

    #[cfg(test)]
    fn complete_with_records(
        &mut self,
        key: K,
        value: V,
        records: Option<CompletedRecords>,
        validate: impl FnOnce(&P) -> Result<(), &'static str>,
    ) -> Result<(), &'static str> {
        let token = self
            .token(&key)
            .ok_or("campaign worker replied for an unknown reservation")?;
        self.complete_token(token, key, value, records, validate)
    }

    fn complete_token(
        &mut self,
        token: ReservationToken,
        key: K,
        value: V,
        records: Option<CompletedRecords>,
        validate: impl FnOnce(&P) -> Result<(), &'static str>,
    ) -> Result<(), &'static str> {
        self.validate_pending(token, key)?;
        let slot = self.slot(token).expect("validated reservation token");
        validate(&slot.pending)?;
        let CompletedRecords {
            actions,
            candidates,
        } = records.unwrap_or_else(|| {
            let (actions, candidates) = (self.records)(&value);
            CompletedRecords {
                actions,
                candidates,
            }
        });
        self.slot_mut(token)
            .expect("validated reservation token")
            .completed = Some(BufferedResult {
            value,
            actions,
            candidates,
        });
        self.pending -= 1;
        let stats = &mut self.telemetry;
        stats.results_live = stats.results_live.saturating_add(1);
        stats.actions_live = stats.actions_live.saturating_add(actions);
        stats.candidates_live = stats.candidates_live.saturating_add(candidates);
        stats.results_peak = stats.results_peak.max(stats.results_live);
        stats.actions_peak = stats.actions_peak.max(stats.actions_live);
        stats.candidates_peak = stats.candidates_peak.max(stats.candidates_live);
        Ok(())
    }

    fn begin_admission(&mut self, token: ReservationToken) -> Option<V> {
        let slot = self.slot_mut(token)?;
        let BufferedResult {
            value,
            actions,
            candidates,
        } = slot.completed.take()?;
        slot.admitting = true;
        self.admitting += 1;
        let stats = &mut self.telemetry;
        stats.results_live = stats
            .results_live
            .checked_sub(1)
            .expect("completed result accounting");
        stats.actions_live = stats
            .actions_live
            .checked_sub(actions)
            .expect("completed action accounting");
        stats.candidates_live = stats
            .candidates_live
            .checked_sub(candidates)
            .expect("completed candidate accounting");
        Some(value)
    }

    fn finish_admission(&mut self, token: ReservationToken) -> Result<(), &'static str> {
        let cell = self
            .slots
            .get_mut(token.slot)
            .ok_or("campaign admission lost its reservation")?;
        if cell.generation != token.generation
            || cell.entry.as_ref().is_none_or(|slot| !slot.admitting)
        {
            return Err("campaign admission lost its reservation");
        }
        let generation = cell
            .generation
            .checked_add(1)
            .ok_or("campaign reservation generation overflow")?;
        let key = cell
            .entry
            .as_ref()
            .expect("validated admitting reservation")
            .key;
        cell.entry = None;
        cell.generation = generation;
        self.entries.remove(&key);
        self.free.push(token.slot);
        self.admitting -= 1;
        Ok(())
    }

    #[cfg(test)]
    fn take_completed(&mut self, key: K) -> Option<(P, V)>
    where
        P: Clone,
    {
        let token = self.token(&key)?;
        let pending = self.slot(token)?.pending.clone();
        let value = self.begin_admission(token)?;
        self.finish_admission(token)
            .expect("reservation generation available");
        Some((pending, value))
    }

    fn completed_len(&self) -> usize {
        self.len() - self.pending - self.admitting
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn completed_job_records<G: Workload + ?Sized>(job: &CompletedOutcome<G>) -> (u64, u64) {
    completed_result_records::<G>(&job.outcome)
}

fn completed_result_records<G: CampaignTypes + ?Sized>(
    outcome: &Result<(CampaignJobResult<G>, u64, String), String>,
) -> (u64, u64) {
    let counts = outcome.as_ref().map_or_else(
        |_| CompletedRecords::default(),
        |(result, _, _)| CompletedRecords::of_result::<G>(result),
    );
    (counts.actions, counts.candidates)
}

fn replay_suffix_limit(drawn: bool, recorded: Option<u8>) -> Result<Option<u8>, Box<dyn Error>> {
    match (drawn, recorded) {
        (true, Some(limit)) if (1..=SUFFIX_DOUBLING_LIMIT).contains(&limit) => Ok(Some(limit)),
        (false, None) => Ok(None),
        _ => Err("recorded suffix limit does not match the drawn suffix".into()),
    }
}

fn replay_splice<G: CampaignTypes>(
    strategy: EnergyStrategy,
    recorded: Option<CampaignSpliceRecord>,
) -> Result<Option<Vec<G::Action>>, Box<dyn Error>> {
    if strategy != EnergyStrategy::Splice {
        if recorded.is_some() {
            return Err("non-splice draw carries splice resolution evidence".into());
        }
        return Ok(None);
    }
    match recorded {
        Some(CampaignSpliceRecord::Unavailable) => Ok(None),
        Some(CampaignSpliceRecord::Tail { tail_postcard, .. }) => {
            let tail: Vec<G::Action> = postcard::from_bytes(&tail_postcard)?;
            if tail.is_empty() || tail.len() > LONGEST_SPLICE_TAIL {
                return Err("recorded splice tail length is outside the bound".into());
            }
            Ok(Some(tail))
        }
        None => Err("splice draw lacks resolution evidence".into()),
    }
}

fn verify_splice_selection<G: Workload>(
    core: &mut CoordinatorCore<G>,
    parent_index: usize,
    mutation_seed: u64,
    weights: (u8, u8),
    selector_path: SelectorPath,
    actual_continuation: bool,
    recorded: Option<&CampaignSpliceRecord>,
) -> Result<(), Box<dyn Error>> {
    if selector_path == SelectorPath::Continuation || actual_continuation {
        return Ok(());
    }
    let expected =
        if energy_strategy(mutation_seed, weights.0, weights.1)? == EnergyStrategy::Splice {
            match core
                .archive
                .splice_tail_for_campaign(parent_index, LONGEST_SPLICE_TAIL)
            {
                Some(tail) => Some(CampaignSpliceRecord::Tail {
                    donor_id: core
                        .archive
                        .stable_id(tail.donor_id)
                        .ok_or("replayed splice donor is missing")?,
                    leaf_id: core
                        .archive
                        .stable_id(tail.leaf_id)
                        .ok_or("replayed splice leaf is missing")?,
                    tail_postcard: postcard::to_allocvec(&tail.actions)?,
                }),
                None => Some(CampaignSpliceRecord::Unavailable),
            }
        } else {
            None
        };
    if expected.as_ref() != recorded {
        return Err("recorded splice provenance diverged from selection".into());
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "K: Serialize + DeserializeOwned")]
pub struct CampaignProgressRecord<K> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_diagnostics: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_diagnostics: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestones: Option<serde_json::Value>,
    #[serde(default)]
    pub objectives_reached: u64,
    #[serde(default)]
    pub terminal_endpoints: u64,
    #[serde(default)]
    pub terminal_objectives: u64,
    #[serde(default)]
    pub execution_failures: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_elapsed_millis: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_times: Option<HostTimes>,
    pub unix_time: u64,
    pub executions: u64,
    #[serde(default)]
    pub execution_work: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deepest_key: Option<K>,
    pub cheapest_cost_in_group: u64,
    pub retained: u64,
    #[serde(default)]
    pub active_entries: usize,
    #[serde(default)]
    pub resident_snapshots: usize,
    #[serde(default)]
    pub resident_snapshot_bytes: usize,
    #[serde(default)]
    pub history_memory_bytes: usize,
    #[serde(default)]
    pub draw_state_memory_bytes: usize,
    #[serde(default)]
    pub entry_metadata_memory_bytes: usize,
    #[serde(default)]
    pub input_index_memory_bytes: usize,
    #[serde(default)]
    pub cell_memory_bytes: usize,
    #[serde(default)]
    pub resident_memory_bytes: usize,
    #[serde(default)]
    pub snapshot_evictions: u64,
    #[serde(default)]
    pub entry_drops: u64,
    #[serde(default)]
    pub live_entries: usize,
    #[serde(default)]
    pub history_compactions: u64,
    #[serde(default)]
    pub historical_entries_dropped: u64,
    #[serde(default)]
    pub input_reconstructions: u64,
    #[serde(default)]
    pub input_index_nodes: usize,
    #[serde(default)]
    pub historical_cells: usize,
    #[serde(default)]
    pub selector: SelectorAccounting,
    #[serde(default)]
    pub continuations: ContinuationAccounting,
}

fn write_live_progress<G: Workload>(
    core: &CoordinatorCore<G>,
    counters: &CampaignCounters,
    profile: Option<(&LiveCoordinatorProfile, HostTimes)>,
    draw_state_memory_bytes: usize,
    telemetry_started: Moment,
    final_census: bool,
    sink: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let line = serde_json::to_string(&progress_record(
        core,
        counters,
        profile,
        draw_state_memory_bytes,
        telemetry_started,
        final_census,
    )?)?;
    sink.write_all(line.as_bytes())?;
    sink.write_all(b"\n")?;
    sink.flush()?;
    Ok(())
}

fn progress_record<G: Workload>(
    core: &CoordinatorCore<G>,
    counters: &CampaignCounters,
    profile: Option<(&LiveCoordinatorProfile, HostTimes)>,
    draw_state_memory_bytes: usize,
    telemetry_started: Moment,
    final_census: bool,
) -> Result<CampaignProgressRecord<G::Key>, Box<dyn Error>> {
    let sequence = core.sequence;
    let unix_time = unix_seconds();
    let (deepest_key, cheapest, retained) = core
        .archive
        .live_progress()
        .map(|(key, cheapest, retained)| (Some(key), cheapest, retained))
        .unwrap_or((None, 0, 0));
    Ok(CampaignProgressRecord {
        workload_diagnostics: G::diagnostics(&core.evidence),
        retained_diagnostics: final_census
            .then(|| G::retained_diagnostics(core.archive.retained_snapshots()))
            .flatten(),
        coordinator: profile
            .map(|(coordinator, _)| coordinator.telemetry_json())
            .transpose()?,
        host_times: profile.map(|(_, host_times)| host_times),
        progress: Some(serde_json::to_value(G::aggregate_progress(&core.evidence))?),
        milestones: Some(serde_json::to_value(core.aggregate_milestones())?),
        objectives_reached: core.objectives_reached,
        terminal_endpoints: core.terminal_endpoints,
        terminal_objectives: core.terminal_objectives,
        execution_failures: core.execution_failures,
        search_elapsed_millis: Some(telemetry_started)
            .map(|at| u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX)),
        unix_time,
        executions: sequence,
        execution_work: counters
            .bootstrap_execution_work
            .saturating_add(counters.job_execution_work),
        deepest_key,
        cheapest_cost_in_group: cheapest,
        retained,
        active_entries: core.archive.active_count(),
        resident_snapshots: core.archive.resident_snapshot_count(),
        resident_snapshot_bytes: core.archive.resident_snapshot_bytes(),
        history_memory_bytes: core.archive.history_memory_bytes(),
        draw_state_memory_bytes,
        entry_metadata_memory_bytes: core.archive.entry_metadata_memory_bytes(),
        input_index_memory_bytes: core.archive.input_index_memory_bytes(),
        cell_memory_bytes: core.archive.cell_memory_bytes(),
        resident_memory_bytes: core
            .archive
            .resident_memory_bytes()
            .saturating_add(draw_state_memory_bytes),
        snapshot_evictions: core.archive.snapshot_evictions(),
        entry_drops: core.archive.entry_drops(),
        live_entries: core.archive.live_entry_count(),
        history_compactions: core.archive.history_compactions(),
        historical_entries_dropped: core.archive.historical_entries_dropped(),
        input_reconstructions: core.archive.input_reconstructions(),
        input_index_nodes: core.archive.input_index_nodes(),
        historical_cells: core.archive.historical_cell_count(),
        selector: if final_census || full_selector_report_due(sequence) {
            core.archive.selector_report()
        } else {
            core.archive.selector_counters()
        },
        continuations: core.archive.continuation_report(),
    })
}

fn bootstrap_core<G: Workload>(
    workload: &G,
    run: &G::Run,
    core: &mut CoordinatorCore<G>,
    target: &mut G::Target,
    origin: &CampaignOrigin<G>,
) -> Result<Option<TreeImportCounts>, Box<dyn Error>> {
    match origin {
        CampaignOrigin::Archive {
            source, checkpoint, ..
        } => Ok(Some(core.import_tree(
            workload,
            run,
            target,
            source.report(),
            checkpoint.as_ref().map(|checkpoint| &checkpoint.snapshots),
        )?)),
        CampaignOrigin::Genesis => {
            core.bootstrap(workload, run, target)?;
            Ok(None)
        }
        CampaignOrigin::SnapshotRoot { checkpoint } => {
            core.bootstrap_snapshot_root(workload, run, target, checkpoint)?;
            Ok(None)
        }
        CampaignOrigin::SearchCheckpoint { .. } => {
            Err("a search checkpoint restores its archive instead of bootstrapping one".into())
        }
    }
}

struct CheckpointHashWriter(Sha256);

impl Write for CheckpointHashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn validate_archive_checkpoint_entries<G: Workload + ?Sized>(
    workload: &G,
    source: &G::ArchiveReport,
    checkpoint: &SnapshotCheckpoint<G::Snapshot>,
) -> Result<(), Box<dyn Error>> {
    let mut expected = BTreeMap::new();
    for entry in workload.source_entries(source) {
        if expected
            .insert(entry.id, entry.snapshot_identity_sha256.as_deref())
            .is_some()
        {
            return Err("source archive has duplicate entry IDs".into());
        }
    }
    let mut seen = BTreeSet::new();
    for entry in &checkpoint.entries {
        if !seen.insert(entry.id) {
            return Err("archive snapshot checkpoint has duplicate entry IDs".into());
        }
        let bound = expected
            .get(&entry.id)
            .ok_or("archive snapshot checkpoint has an unknown entry ID")?
            .ok_or("source archive entry lacks a snapshot digest")?;
        if workload.snapshot_identity_sha256(&entry.snapshot)? != *bound {
            return Err("archive snapshot checkpoint differs from its source entry digest".into());
        }
    }
    Ok(())
}

fn validate_archive_checkpoint<G: Workload + ?Sized>(
    workload: &G,
    checkpoint: &CampaignCheckpoint<G::Snapshot>,
) -> Result<(), Box<dyn Error>> {
    if checkpoint.snapshots.format != workload.checkpoint_format() {
        return Err("archive snapshot checkpoint format is not recognized".into());
    }
    let mut writer = CheckpointHashWriter(Sha256::new());
    checkpoint.snapshots.write_to(&mut writer)?;
    let actual_sha256 = format!("{:x}", writer.0.finalize());
    if checkpoint.file_sha256 != actual_sha256 {
        return Err("archive snapshot checkpoint bytes do not match their SHA-256".into());
    }
    Ok(())
}

fn validate_snapshot_root_checkpoint<'a, G: Workload + ?Sized>(
    workload: &G,
    checkpoint: &'a CampaignCheckpoint<G::Snapshot>,
) -> Result<&'a G::Snapshot, Box<dyn Error>> {
    if checkpoint.path.is_empty() {
        return Err("snapshot-root checkpoint logical path is empty".into());
    }
    if checkpoint.file_sha256.len() != 64
        || !checkpoint
            .file_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("snapshot-root checkpoint SHA-256 is malformed".into());
    }
    if checkpoint.snapshots.format != workload.checkpoint_format() {
        return Err("snapshot-root checkpoint format is not recognized".into());
    }
    let [entry] = checkpoint.snapshots.entries.as_slice() else {
        return Err("snapshot-root checkpoint must contain exactly one entry".into());
    };
    if entry.id != 0 {
        return Err("snapshot-root checkpoint entry id is not zero".into());
    }
    let actual_sha256 = format!("{:x}", Sha256::digest(checkpoint.snapshots.to_bytes()?));
    if checkpoint.file_sha256 != actual_sha256 {
        return Err("snapshot-root checkpoint bytes do not match their SHA-256".into());
    }
    Ok(&entry.snapshot)
}

pub fn run_campaign_checkpointed<G: Workload>(
    workload: &G,
    config: &CampaignConfig<G>,
    origin: &CampaignOrigin<G>,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<CampaignOutcome<G>, Box<dyn Error>>
where
    G::ArchiveReport: Serialize,
    G::Action: 'static,
    G::Milestones: 'static,
    G::Observations: 'static,
    G::Snapshot: 'static,
{
    run_campaign_checkpointed_with_options(
        workload,
        config,
        origin,
        stream,
        progress,
        CampaignExecutionOptions::default(),
    )
}

pub fn run_campaign_checkpointed_with_options<G: Workload>(
    workload: &G,
    config: &CampaignConfig<G>,
    origin: &CampaignOrigin<G>,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
    options: CampaignExecutionOptions,
) -> Result<CampaignOutcome<G>, Box<dyn Error>>
where
    G::ArchiveReport: Serialize,
    G::Action: 'static,
    G::Milestones: 'static,
    G::Observations: 'static,
    G::Snapshot: 'static,
{
    run_campaign_checkpointed_observed(workload, config, origin, stream, progress, options, None)
}

pub fn run_campaign_checkpointed_observed<G: Workload>(
    workload: &G,
    config: &CampaignConfig<G>,
    origin: &CampaignOrigin<G>,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
    options: CampaignExecutionOptions,
    observer: Option<crate::search::job_observation::JobObserver<'_, G::JobObservation>>,
) -> Result<CampaignOutcome<G>, Box<dyn Error>>
where
    G::ArchiveReport: Serialize,
    G::Action: 'static,
    G::Milestones: 'static,
    G::Observations: 'static,
    G::Snapshot: 'static,
{
    run_campaign(
        workload,
        config,
        origin,
        stream,
        progress,
        options,
        &(),
        observer,
    )
}

pub fn run_campaign_watched<G: Workload, W: CampaignWatch<G>>(
    workload: &G,
    config: &CampaignConfig<G>,
    origin: &CampaignOrigin<G>,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
    options: CampaignExecutionOptions,
    watch: &W,
) -> Result<CampaignOutcome<G>, Box<dyn Error>>
where
    G::ArchiveReport: Serialize,
    G::Action: 'static,
    G::Milestones: 'static,
    G::Observations: 'static,
    G::Snapshot: 'static,
{
    run_campaign(
        workload, config, origin, stream, progress, options, watch, None,
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn run_campaign<G: Workload, W: CampaignWatch<G>>(
    workload: &G,
    config: &CampaignConfig<G>,
    origin: &CampaignOrigin<G>,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
    options: CampaignExecutionOptions,
    watch: &W,
    mut observer: Option<crate::search::job_observation::JobObserver<'_, G::JobObservation>>,
) -> Result<CampaignOutcome<G>, Box<dyn Error>>
where
    G::ArchiveReport: Serialize,
    G::Action: 'static,
    G::Milestones: 'static,
    G::Observations: 'static,
    G::Snapshot: 'static,
{
    crate::search::job_observation::bounded::<G::JobObservation>();
    let mut sink_disabled = false;
    workload.verify_integrity()?;
    let placement = options.placement.clone();
    let placement = placement.as_ref();
    let (bootstrapped, first_worker_start) = mpsc::channel::<()>();
    let first_worker_start = Mutex::new(first_worker_start);
    let mut coordinator = None;
    let pool_outcome = with_worker_pool_routed(
        config.workers,
        |worker| {
            if worker == 0 {
                first_worker_start
                    .lock()
                    .map_err(|_| "the first worker's start signal lock was poisoned".to_owned())?
                    .recv()
                    .map_err(|_| "the campaign stopped before its bootstrap finished".to_owned())?;
            }
            ThreadPlacement::place(placement, PlacedThread::Worker(worker))?;
            workload.new_target()
        },
        |_| workload.new_target(),
        |target, job: JobSpec<G>| {
            let reservation = job.reservation;
            let outcome = watch
                .run(&job, || run_job(workload, config, target, &job))
                .map_err(|error| error.to_string());
            let records = CompletedRecords::after_watch::<G, W::Note>(&outcome);
            Ok((reservation, outcome, records))
        },
        |target| workload.telemetry(target),
        |target| workload.worker_locality(target),
        || workload.begin_job_observation(),
        || workload.take_job_observation(),
        observer
            .as_ref()
            .map(|observer| observer.diagnostics.clone()),
        |pool| -> Result<(), Box<dyn Error>> {
            ThreadPlacement::place(placement, PlacedThread::Coordinator)?;
            let campaign = coordinator.insert(CampaignCoordinator::start(
                workload,
                config,
                origin,
                stream,
                progress.map(|sink| -> &mut dyn Write { sink }),
                options,
            )?);
            pool.configure_indexed(
                campaign.reservation_capacity(),
                |key: &(u64, usize, ReservationToken)| key.2.slot,
            )?;
            campaign.enable_background_stream(placement.cloned())?;
            watch.start(campaign)?;
            bootstrapped.send(())?;
            drop(bootstrapped);
            pool.wait_ready(config.workers)?;
            campaign.begin_dispatch_phase();
            while campaign.reserve_next_dispatch(&mut |job, token| {
                #[cfg(test)]
                tests::before_locality_dispatch(pool, job.reservation, true)?;
                let locality = workload.snapshot_locality(&job.snapshot);
                pool.dispatch_local((job.planned_finish, job.reservation, token), job, locality)
            })? {}
            loop {
                campaign.dispatch_queued(&mut |job, token| {
                    #[cfg(test)]
                    tests::before_locality_dispatch(pool, job.reservation, false)?;
                    let locality = workload.snapshot_locality(&job.snapshot);
                    pool.dispatch_local((job.planned_finish, job.reservation, token), job, locality)
                })?;
                if campaign.finished() {
                    break;
                }
                let reply = loop {
                    campaign.drain_background_stream()?;
                    let receive_started = now();
                    let received = pool.receive_timeout(Duration::from_millis(5));
                    campaign.note_receive_wait(receive_started.elapsed().as_nanos());
                    match received {
                        Ok(Some(reply)) => break reply,
                        Ok(None) => continue,
                        Err(WorkerPoolError::RepliesClosed) => {
                            let reservation = campaign
                                .next_admission()
                                .ok_or("campaign lost its planned admission while waiting")?;
                            return Err(format!(
                                "campaign reservation {reservation} failed: every worker exited before reporting its result"
                            )
                            .into());
                        }
                        Err(error) => return Err(error.into()),
                    }
                };
                let mut ready_reply = Some(reply);
                while let Some(mut reply) = ready_reply {
                    let physical_worker = reply.worker;
                    if physical_worker >= config.workers {
                        return Err(
                            "campaign worker replied with an unknown physical identifier".into(),
                        );
                    }
                    let (planned_finish, reservation, token) = reply.key;
                    campaign.validate_reply_token(token, reservation, planned_finish)?;
                    let (outcome, records) = match reply.outcome {
                        Ok((claimed, outcome, records)) if claimed == reservation => {
                            (outcome, records)
                        }
                        Ok(_) => {
                            return Err("campaign worker replied for another reservation".into());
                        }
                        Err(error) => (Err(error), CompletedRecords::default()),
                    };
                    if let Ok((outcome, _)) = &outcome
                        && outcome.reservation != reservation
                    {
                        return Err("campaign worker replied for another reservation".into());
                    }
                    if let Some(observer) = observer.as_mut()
                        && !sink_disabled
                        && let Some((parent_id, snapshot_id)) = campaign.pending_origin_token(token)
                    {
                        let receipt = reply.observation.take();
                        let has_payload = receipt.payload.is_some();
                        let completion = crate::search::job_observation::CompletionObservation {
                            worker: physical_worker,
                            reservation,
                            planned_finish,
                            parent_id,
                            snapshot_id,
                            job_failed: outcome.is_err(),
                            receipt,
                        };
                        observer
                            .diagnostics
                            .add(crate::search::job_observation::Counter::SinkAttempts);
                        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            (observer.sink)(completion)
                        })) {
                            Ok(Ok(())) => {
                                if has_payload {
                                    observer
                                        .diagnostics
                                        .add(crate::search::job_observation::Counter::Delivered);
                                }
                            }
                            failure => {
                                observer.diagnostics.add(if failure.is_err() {
                                    crate::search::job_observation::Counter::SinkPanics
                                } else {
                                    crate::search::job_observation::Counter::SinkErrors
                                });
                                if has_payload {
                                    observer
                                        .diagnostics
                                        .add(crate::search::job_observation::Counter::Discarded);
                                }
                                sink_disabled = true;
                            }
                        }
                    }
                    match outcome {
                        Ok((outcome, note)) => {
                            watch.prepare_admission(&note, campaign);
                            campaign.admit_dispatch(
                                outcome,
                                records,
                                token,
                                &mut |job, token| {
                                    #[cfg(test)]
                                    tests::before_locality_dispatch(pool, job.reservation, false)?;
                                    let locality = workload.snapshot_locality(&job.snapshot);
                                    pool.dispatch_local(
                                        (job.planned_finish, job.reservation, token),
                                        job,
                                        locality,
                                    )
                                },
                            )?;
                            watch.admitted(note, campaign);
                        }
                        Err(error) => campaign.fail_dispatch(reservation, token, error)?,
                    }
                    campaign.dispatch_queued(&mut |job, token| {
                        #[cfg(test)]
                        tests::before_locality_dispatch(pool, job.reservation, false)?;
                        let locality = workload.snapshot_locality(&job.snapshot);
                        pool.dispatch_local(
                            (job.planned_finish, job.reservation, token),
                            job,
                            locality,
                        )
                    })?;
                    ready_reply = match pool.try_receive() {
                        Ok(reply) => reply,
                        Err(WorkerPoolError::RepliesClosed) => None,
                        Err(error) => return Err(error.into()),
                    };
                }
            }
            campaign.end_dispatch_phase();
            pool.close();
            Ok(())
        },
    );
    let ((), worker_telemetry) = match pool_outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            return match coordinator.as_mut() {
                Some(campaign) => campaign.finish_persistence(Err(error)),
                None => Err(error),
            };
        }
    };
    coordinator
        .ok_or("campaign coordinator never started")?
        .finish(worker_telemetry)
}

struct CoordinatorSession<'a, G: TargetExecution> {
    workload: &'a G,
    finished: bool,
    begun: bool,
}
impl<G: TargetExecution> CoordinatorSession<'_, G> {
    fn finish(&mut self) -> Result<(), Box<dyn Error>> {
        self.finished = true;
        self.workload.finish_coordinator_session()
    }
}
impl<G: TargetExecution> Drop for CoordinatorSession<'_, G> {
    fn drop(&mut self) {
        if self.begun && !self.finished {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = self.finish();
            }));
        }
    }
}
fn with_coordinator_session<G: TargetExecution, T>(
    workload: &G,
    operation: impl FnOnce() -> Result<T, Box<dyn Error>>,
) -> Result<T, Box<dyn Error>> {
    let mut scope = CoordinatorSession {
        workload,
        finished: false,
        begun: false,
    };
    workload.begin_coordinator_session()?;
    scope.begun = true;
    let result = operation();
    let cleanup = scope.finish();
    coordinator_session_result(result, cleanup, &mut std::io::stderr())
}

fn coordinator_session_result<T>(
    result: Result<T, Box<dyn Error>>,
    cleanup: Result<(), Box<dyn Error>>,
    diagnostics: &mut dyn Write,
) -> Result<T, Box<dyn Error>> {
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup)) => {
            let _ = writeln!(
                diagnostics,
                "coordinator session finalization also failed: {cleanup}"
            );
            Err(error)
        }
    }
}

fn bootstrap_campaign<G: Workload>(
    workload: &G,
    config: &CampaignConfig<G>,
    origin: &CampaignOrigin<G>,
    core: &mut CoordinatorCore<G>,
    draw_state: &DrawTables<G::Action>,
    duration_policies: &DurationPolicies<G::Key>,
    counters: &mut CampaignCounters,
) -> Result<(), Box<dyn Error>> {
    core.archive
        .enable_continuations(usize::from(SUFFIX_DOUBLING_LIMIT));
    let mut bootstrap_target = workload.new_target().map_err(|error| -> Box<dyn Error> {
        format!("failed to build the bootstrap target: {error}").into()
    })?;
    let work_before = workload.execution_work(&bootstrap_target);
    counters.tree_import =
        bootstrap_core(workload, &config.run, core, &mut bootstrap_target, origin)?;
    counters.bootstrap_execution_work = execution_work_delta(
        work_before,
        workload.execution_work(&bootstrap_target),
        "bootstrap",
    )?;
    if core.objectives_reached > 0 {
        counters.note_first_objective(0);
    }
    drop(bootstrap_target);
    let bootstrap_memory_bytes = core
        .archive
        .resident_memory_bytes()
        .saturating_add(workload.draw_state_memory_bytes(draw_state))
        .saturating_add(duration_policies.memory_bytes());
    if !resident_memory_is_within_budget(bootstrap_memory_bytes, config.memory_budget_mib) {
        return Err("campaign bootstrap state exceeds its deterministic memory budget".into());
    }
    core.archive.prepare_selection();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn search_checkpoint_header<G: Workload>(
    workload: &G,
    config: &CampaignConfig<G>,
    executable_identity: &str,
    reason: &str,
    executions: u64,
    reserved: u64,
    admitted: usize,
    planned_clock: u64,
) -> CheckpointHeader {
    let mut policies = recorded_policies(workload, &config.run);
    for (field, value) in [
        ("schedule_policy", schedule_policy_identifier(config.window)),
        ("mixture_policy", draw_mixture_identifier(config.mixture)),
        (
            "retention_policy",
            retention_policy_identifier(config.retention).to_owned(),
        ),
        ("parent_scheduler", SELECTOR_IDENTIFIER.to_owned()),
        ("continuation_policy", CONTINUATION_IDENTIFIER.to_owned()),
        (
            PREFERENCE_PORTFOLIO_FIELD,
            preference_portfolio_identifier::<G::Key>(),
        ),
        (
            "stop_rollout_on_objective",
            config.stop_rollout_on_objective.to_string(),
        ),
        (
            "stop_campaign_on_objective",
            config.stop_campaign_on_objective.to_string(),
        ),
    ] {
        policies.insert(field.to_owned(), value);
    }
    CheckpointHeader {
        format: SEARCH_CHECKPOINT_FORMAT.to_owned(),
        checkpoint_owner: None,
        reason: reason.to_owned(),
        workload_identity_sha256: workload.workload_identity_sha256(),
        executable_identity: executable_identity.to_owned(),
        campaign_seed: config.campaign_seed,
        window: config.window,
        archive_entry_limit: config.archive_entry_limit,
        memory_budget_mib: config.memory_budget_mib,
        policies,
        executions,
        reserved,
        admitted,
        planned_clock,
    }
}

#[allow(clippy::too_many_arguments)]
fn search_checkpoint_policy_changes(
    header: &CheckpointHeader,
    expected: &CheckpointHeader,
) -> Result<BTreeMap<String, PolicyChange>, Box<dyn Error>> {
    let fields = header
        .policies
        .keys()
        .chain(expected.policies.keys())
        .collect::<BTreeSet<_>>();
    let mut changes = BTreeMap::new();
    let mut fixed = Vec::new();
    for field in fields {
        let checkpoint = header.policies.get(field);
        let run = expected.policies.get(field);
        if checkpoint == run {
            continue;
        }
        match (checkpoint, run) {
            (Some(checkpoint), Some(run)) if RESUMABLE_POLICY_FIELDS.contains(&field.as_str()) => {
                changes.insert(
                    field.clone(),
                    PolicyChange {
                        checkpoint: checkpoint.clone(),
                        run: run.clone(),
                    },
                );
            }
            _ => fixed.push(field.as_str()),
        }
    }
    if !fixed.is_empty() {
        return Err(format!("search checkpoint policies differ: {}", fixed.join(", ")).into());
    }
    Ok(changes)
}

#[allow(clippy::too_many_arguments)]
fn restore_search_checkpoint<G: Workload>(
    workload: &G,
    config: &CampaignConfig<G>,
    executable_identity: &str,
    mut reader: CheckpointReader,
    core: &mut CoordinatorCore<G>,
    draw_state: &mut DrawTables<G::Action>,
    duration_policies: &mut DurationPolicies<G::Key>,
    counters: &mut CampaignCounters,
) -> Result<SearchResume<G>, Box<dyn Error>> {
    let header = reader.header.clone();
    let expected = search_checkpoint_header(
        workload,
        config,
        executable_identity,
        &header.reason,
        0,
        0,
        0,
        0,
    );
    if header.executable_identity != expected.executable_identity {
        return Err("search checkpoint belongs to a different executable".into());
    }
    if header.workload_identity_sha256 != expected.workload_identity_sha256 {
        return Err("search checkpoint belongs to a different workload".into());
    }
    if header.window != expected.window {
        return Err("search checkpoint admission window must match the campaign".into());
    }
    if header.archive_entry_limit != expected.archive_entry_limit
        || header.memory_budget_mib != expected.memory_budget_mib
    {
        return Err("search checkpoint limits differ from the campaign".into());
    }
    let changes = search_checkpoint_policy_changes(&header, &expected)?;
    let state: CoreCheckpoint<G> = reader.next()?;
    let archive: Archive<G::Action, G::Key, G::Milestones, G::Snapshot> = reader.next()?;
    let evidence: Vec<u8> = reader.next()?;
    let draw: Vec<u8> = reader.next()?;
    let durations = reader.next()?;
    let mut rand: RomuDuoJrRand = reader.next()?;
    let restored_counters: CampaignCounters = reader.next()?;
    let profile: LiveCoordinatorProfile = reader.next()?;
    let in_flight: Vec<InFlightJob<G>> = reader.next()?;
    let snapshots = reader.snapshots::<G::Snapshot>()?;
    reader.finish()?;
    core.restore_state(state)?;
    core.archive = archive;
    core.archive.set_snapshot_retention(G::snapshot_retention());
    let charge: fn(&G::Snapshot) -> usize = G::snapshot_memory_charge;
    core.archive
        .restore_runtime(workload.action_cost_fn(), Some(charge), snapshots)?;
    if changes.contains_key(PREFERENCE_PORTFOLIO_FIELD)
        || changes.contains_key(PREFERENCE_POLICY_FIELD)
    {
        core.archive.rerank_slot_holders();
    }
    core.archive
        .resume_continuations(usize::from(SUFFIX_DOUBLING_LIMIT));
    core.evidence = G::evidence_from_checkpoint(&evidence)?;
    *draw_state = if changes.contains_key(DRAW_TABLE_POLICY_FIELD) {
        let parameters = workload.draw_table_parameters(&config.run);
        let mut tables = DrawTables::new(parameters)?;
        fold_draw_sources(
            &mut tables,
            parameters,
            core.archive
                .entries
                .iter()
                .map(|entry| entry.input_suffix.as_slice()),
        )?;
        tables.flush()?;
        tables
    } else {
        DrawTables::from_resume_bytes(&draw)?
    };
    *duration_policies = DurationPolicies::from_checkpoint(durations)?;
    *counters = restored_counters;
    if header.campaign_seed != config.campaign_seed {
        rand = RomuDuoJrRand::with_seed(derive_selection_seed(config.campaign_seed));
    }
    let reserved = usize::try_from(header.reserved)?;
    let ordered = in_flight
        .windows(2)
        .all(|pair| pair[0].reservation < pair[1].reservation);
    if !ordered
        || in_flight
            .last()
            .is_some_and(|job| job.reservation >= reserved)
        || reserved.checked_sub(header.admitted) != Some(in_flight.len())
        || in_flight
            .iter()
            .any(|job| job.pending.planned_finish < header.planned_clock)
    {
        return Err("search checkpoint in-flight jobs do not match its admission count".into());
    }
    if core.sequence != u64::try_from(header.admitted)? || core.sequence != header.executions {
        return Err("search checkpoint admission count disagrees with its archive".into());
    }
    Ok(SearchResume {
        rand,
        reserved: header.reserved,
        admitted: header.admitted,
        planned_clock: header.planned_clock,
        profile,
        in_flight,
    })
}

#[allow(clippy::too_many_arguments)]
fn write_search_checkpoint<G: Workload>(
    workload: &G,
    checkpoints: &mut super::checkpoint::publisher::CheckpointPublisher<
        G::Snapshot,
        <G::Key as ArchiveKey>::Progress,
    >,
    reason: &str,
    core: &CoordinatorCore<G>,
    draw_state: &DrawTables<G::Action>,
    duration_policies: &DurationPolicies<G::Key>,
    rand: &RomuDuoJrRand,
    counters: &CampaignCounters,
    profile: &LiveCoordinatorProfile,
    reserved: u64,
    admitted: usize,
    planned_clock: u64,
    schedule: &AdmissionSchedule,
    reservations: &ReservationSlots<usize, PendingJob<G>, CompletedOutcome<G>>,
    spec_records: &BTreeMap<usize, (ReservationToken, JobSpecRecord<G>)>,
) -> Result<(), Box<dyn Error>> {
    workload.verify_integrity()?;
    let in_flight = InFlightCheckpointRef {
        schedule,
        reservations,
        spec_records,
    };
    checkpoints.publish(
        super::checkpoint::publisher::AdmissionVersion {
            executions: core.sequence,
            reserved,
            admitted,
            planned_clock,
        },
        reason,
        core.archive.resident_snapshot_entries(),
        |out| {
            core.write_checkpoint_state(&mut *out)?;
            postcard::to_io(&core.archive, &mut *out)?;
            super::checkpoint::image::write_streamed_bytes(&mut *out, |out| {
                G::write_evidence_checkpoint(&core.evidence, out)
            })?;
            super::checkpoint::image::write_streamed_bytes(&mut *out, |out| {
                draw_state.write_resume(out)
            })?;
            duration_policies.write_checkpoint(&mut *out)?;
            postcard::to_io(rand, &mut *out)?;
            postcard::to_io(counters, &mut *out)?;
            postcard::to_io(profile, &mut *out)?;
            postcard::to_io(&in_flight, &mut *out)?;
            Ok(())
        },
    )
}

struct InFlightCheckpointRef<'a, G: Workload> {
    schedule: &'a AdmissionSchedule,
    reservations: &'a ReservationSlots<usize, PendingJob<G>, CompletedOutcome<G>>,
    spec_records: &'a BTreeMap<usize, (ReservationToken, JobSpecRecord<G>)>,
}
impl<G: Workload> Serialize for InFlightCheckpointRef<'_, G> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        if self.spec_records.len() != self.schedule.len() {
            return Err(serde::ser::Error::custom(
                "search checkpoint in-flight input count differs",
            ));
        }
        self.schedule
            .validate(self.reservations, |pending| pending.planned_finish)
            .map_err(serde::ser::Error::custom)?;
        let mut sequence = serializer.serialize_seq(Some(self.schedule.len()))?;
        for (reservation, (token, spec)) in self.spec_records {
            let pending = self
                .reservations
                .slot(*token)
                .filter(|slot| slot.key == *reservation)
                .map(|slot| &slot.pending)
                .ok_or_else(|| {
                    serde::ser::Error::custom("search checkpoint lost an in-flight reservation")
                })?;
            sequence.serialize_element(&InFlightJobRef {
                reservation: *reservation,
                spec,
                pending,
            })?;
        }
        sequence.end()
    }
}

fn finish_record<G: Workload>(
    workload: &G,
    run: &G::Run,
    draw_state: &mut DrawTables<G::Action>,
    core: &CoordinatorCore<G>,
    decisions: &[CampaignAdmissionDecision],
) -> Result<Option<EmpiricalStepCheckpoint>, Box<dyn Error>> {
    let mut retained = Vec::new();
    for decision in decisions {
        let CampaignAdmissionDecision::Retained { id } = decision else {
            continue;
        };
        let index = core
            .archive
            .index_of_id(*id)
            .ok_or("retained draw-table entry is missing from the run archive")?;
        let entry = core
            .archive
            .entries
            .get(index)
            .ok_or("retained draw-table entry is missing from the run archive")?;
        retained.push((0, entry.input_suffix.as_slice()));
    }
    workload.finish_stream_record(run, draw_state, &retained, &core.evidence)
}

fn recorded_policies<G: Workload>(workload: &G, run: &G::Run) -> WorkloadPolicies {
    let mut policies = workload.policies(run);
    policies.insert(
        DRAW_TABLE_POLICY_FIELD.to_owned(),
        draw_table_identifier(workload.draw_table_parameters(run)),
    );
    policies
}

fn resolve_recorded_policies<G: Workload>(
    workload: &G,
    policies: &WorkloadPolicies,
) -> Result<G::Run, Box<dyn Error>> {
    let mut workload_policies = policies.clone();
    let recorded = workload_policies
        .remove(DRAW_TABLE_POLICY_FIELD)
        .ok_or("campaign stream is missing its draw table policy")?;
    let parameters = draw_table_from_identifier(&recorded)?;
    let run = workload.resolve_recorded(&workload_policies)?;
    if workload.draw_table_parameters(&run) != parameters {
        return Err("campaign stream draw table policy does not match this build".into());
    }
    Ok(run)
}

fn fold_draw_sources<'a, A: Copy + Ord + Serialize + 'a>(
    tables: &mut DrawTables<A>,
    parameters: EmpiricalStepParameters,
    suffixes: impl Iterator<Item = &'a [A]>,
) -> Result<(), Box<dyn Error>> {
    let mut pending = 0_u64;
    for suffix in suffixes {
        tables.fold_source(suffix)?;
        pending = pending.saturating_add(1);
        if pending >= parameters.update_every_records {
            tables.flush()?;
            pending = 0;
        }
    }
    Ok(())
}

fn initial_draw_state<G: Workload>(
    workload: &G,
    run: &G::Run,
    origin: Option<(&str, &G::ArchiveReport)>,
) -> Result<InitialDrawState<G>, Box<dyn Error>> {
    let parameters = workload.draw_table_parameters(run);
    let mut tables = DrawTables::new(parameters)?;
    let source_sha256 = match origin {
        None => format!("{:x}", Sha256::digest([])),
        Some((file_sha256, report)) => {
            let entries = workload.source_entries(report);
            let parent_len: BTreeMap<u64, usize> = entries
                .iter()
                .map(|entry| (entry.id, entry.input.actions.len()))
                .collect();
            fold_draw_sources(
                &mut tables,
                parameters,
                entries.iter().map(|entry| {
                    let prefix = entry
                        .parent_id
                        .and_then(|parent| parent_len.get(&parent).copied())
                        .unwrap_or(0);
                    entry.input.actions.get(prefix..).unwrap_or(&[])
                }),
            )?;
            file_sha256.to_owned()
        }
    };
    tables.flush()?;
    let initial = tables.checkpoint()?;
    let header = DrawTableHeader {
        source_sha256,
        parameters,
        initial,
    };
    Ok((tables, Some(header)))
}

#[derive(Default)]
struct ReplaySkipSchedule {
    last_reservation: Option<u64>,
    skips_for_reservation: u64,
    admitted: BTreeSet<u64>,
}

impl ReplaySkipSchedule {
    fn accept_skip(
        &mut self,
        reservation: u64,
        admissions: usize,
        window: usize,
        total: usize,
    ) -> Result<(), Box<dyn Error>> {
        let slot = usize::try_from(reservation)?;
        if slot >= admissions.saturating_add(window).min(total) || slot >= total {
            return Err("recorded skip names a reservation outside the selection window".into());
        }
        let selection_admission = if slot < window { 0 } else { slot - window + 1 };
        if admissions != selection_admission {
            return Err("recorded skip was not drawn at its selection boundary".into());
        }
        if self.admitted.contains(&reservation) {
            return Err("recorded skip follows its job admission".into());
        }
        if self.last_reservation.is_some_and(|last| reservation < last) {
            return Err("recorded skips are out of reservation order".into());
        }
        if self.last_reservation == Some(reservation) {
            self.skips_for_reservation = self.skips_for_reservation.saturating_add(1);
        } else {
            self.last_reservation = Some(reservation);
            self.skips_for_reservation = 1;
        }
        if self.skips_for_reservation > CONSECUTIVE_SKIP_LIMIT {
            return Err("recorded selection exceeds the consecutive skip limit".into());
        }
        Ok(())
    }

    fn admit(&mut self, reservation: u64) {
        self.admitted.insert(reservation);
    }
}

pub fn replay_campaign_checkpointed<G: Workload>(
    workload: &G,
    stream_bytes: &[u8],
    origin_source: Option<&ArchiveSource<G::ArchiveReport>>,
    origin_checkpoint: Option<&CampaignCheckpoint<G::Snapshot>>,
) -> Result<CampaignOutcome<G>, Box<dyn Error>>
where
    G::ArchiveReport: Serialize,
{
    replay_campaign_checkpointed_inner(
        workload,
        stream_bytes,
        origin_source,
        origin_checkpoint,
        true,
    )
}

pub fn replay_campaign_prefix_checkpointed<G: Workload>(
    workload: &G,
    stream_bytes: &[u8],
    origin_source: Option<&ArchiveSource<G::ArchiveReport>>,
    origin_checkpoint: Option<&CampaignCheckpoint<G::Snapshot>>,
) -> Result<CampaignOutcome<G>, Box<dyn Error>>
where
    G::ArchiveReport: Serialize,
{
    replay_campaign_checkpointed_inner(
        workload,
        stream_bytes,
        origin_source,
        origin_checkpoint,
        false,
    )
}

fn replay_campaign_checkpointed_inner<G: Workload>(
    workload: &G,
    stream_bytes: &[u8],
    origin_source: Option<&ArchiveSource<G::ArchiveReport>>,
    origin_checkpoint: Option<&CampaignCheckpoint<G::Snapshot>>,
    require_terminal: bool,
) -> Result<CampaignOutcome<G>, Box<dyn Error>>
where
    G::ArchiveReport: Serialize,
{
    with_coordinator_session(workload, || {
        replay_campaign_checkpointed_in_session(
            workload,
            stream_bytes,
            origin_source,
            origin_checkpoint,
            require_terminal,
        )
    })
}

#[allow(clippy::too_many_lines)]
fn replay_campaign_checkpointed_in_session<G: Workload>(
    workload: &G,
    stream_bytes: &[u8],
    origin_source: Option<&ArchiveSource<G::ArchiveReport>>,
    origin_checkpoint: Option<&CampaignCheckpoint<G::Snapshot>>,
    require_terminal: bool,
) -> Result<CampaignOutcome<G>, Box<dyn Error>>
where
    G::ArchiveReport: Serialize,
{
    workload.verify_integrity()?;
    let executable_identity = workload.executable_identity()?;
    let stream_sha256 = format!("{:x}", Sha256::digest(stream_bytes));
    let duration_memory_reserve = DurationPolicies::<G::Key>::memory_reserve_bytes();
    let text = std::str::from_utf8(stream_bytes)?;
    let mut lines = text.lines();
    let header: CampaignStreamHeader<DrawTableHeader> =
        serde_json::from_str(lines.next().ok_or("campaign stream is empty")?)?;
    if header.schema_version != CAMPAIGN_SCHEMA_VERSION {
        return Err("campaign stream schema version is not recognized".into());
    }
    if header.preference_portfolio != preference_portfolio_identifier::<G::Key>() {
        return Err("campaign stream preference portfolio does not match the key".into());
    }
    if !archive_entry_limit_is_valid(header.archive_entry_limit) {
        return Err("recorded archive entry limit is outside the compiled bound".into());
    }
    if header.memory_budget_mib == Some(0) {
        return Err("recorded memory budget must be greater than zero".into());
    }
    if header.work_budget == Some(0) {
        return Err("recorded work budget must be nonzero".into());
    }
    if header.format != workload.stream_format() {
        return Err("campaign stream format is not recognized".into());
    }
    let origin_fields_match = header.origin_checkpoint_policy_changes.is_empty()
        && match header.origin_kind.as_str() {
            ORIGIN_GENESIS => {
                header.origin_path.is_none()
                    && header.origin_archive_sha256.is_none()
                    && header.origin_checkpoint_path.is_none()
                    && header.origin_checkpoint_sha256.is_none()
            }
            ORIGIN_SNAPSHOT_ROOT => {
                header.origin_path.is_none()
                    && header.origin_archive_sha256.is_none()
                    && header.origin_checkpoint_path.is_some()
                    && header.origin_checkpoint_sha256.is_some()
            }
            ORIGIN_ARCHIVE => {
                header.origin_path.is_some()
                    && header.origin_archive_sha256.is_some()
                    && (header.origin_checkpoint_path.is_some()
                        == header.origin_checkpoint_sha256.is_some())
            }
            _ => true,
        };
    if !origin_fields_match {
        return Err("campaign stream origin provenance is inconsistent".into());
    }
    if !schedule_policy_is_supported(&header.schedule_policy) {
        return Err("campaign stream schedule policy is not recognized".into());
    }
    if !progress_policy_is_supported(&header.progress_policy) {
        return Err("campaign stream progress policy is not recognized".into());
    }
    if header.executor_mode != EXECUTOR_MODE
        || header.selection_seed_derivation != SELECTION_SEED_DERIVATION
    {
        return Err("campaign stream execution provenance is not recognized".into());
    }
    if header.workload_identity_sha256 != workload.workload_identity_sha256() {
        return Err("campaign replay workload identity does not match the recorded stream".into());
    }
    if header.executable_identity != executable_identity {
        return Err(
            "campaign replay executable identity does not match the recorded stream".into(),
        );
    }
    if header.action_cost_unit != workload.action_cost_unit() {
        return Err("campaign replay action-cost unit does not match the recorded stream".into());
    }
    if header.execution_work_unit != workload.execution_work_unit() {
        return Err(
            "campaign replay execution-work unit does not match the recorded stream".into(),
        );
    }
    let mut record_lines = lines.collect::<Vec<_>>();
    let end = if require_terminal {
        let end_line = record_lines
            .pop()
            .ok_or("campaign stream lacks a terminal record")?;
        let end_record: CampaignStreamRecord<EmpiricalStepCheckpoint, G::Key> =
            serde_json::from_str(end_line)?;
        let CampaignStreamRecord::End(end) = end_record else {
            return Err("campaign stream lacks a terminal record".into());
        };
        Some(end)
    } else {
        None
    };
    let mut required_draw_versions = DrawVersionSchedule::default();
    let mut replay_job_reservations = BTreeMap::<
        u64,
        (
            u64,
            Option<u8>,
            u64,
            SelectorPath,
            Option<CampaignSpliceRecord>,
        ),
    >::new();
    for (index, line) in record_lines.iter().enumerate() {
        let record: CampaignStreamRecord<EmpiricalStepCheckpoint, G::Key> =
            serde_json::from_str(line)?;
        let before = match record {
            CampaignStreamRecord::Job(job) => {
                if replay_job_reservations
                    .insert(
                        job.reservation,
                        (
                            job.parent_id,
                            job.suffix_limit,
                            job.mutation_seed,
                            job.selector.path,
                            job.splice.clone(),
                        ),
                    )
                    .is_some()
                {
                    return Err("campaign stream admits one reservation twice".into());
                }
                job.draw_checkpoint_before
            }
            CampaignStreamRecord::Skip(skip) => skip.draw_checkpoint_before,
            CampaignStreamRecord::End(_) => {
                return Err("campaign stream has a premature terminal record".into());
            }
        };
        if let Some(before) = before {
            required_draw_versions.require(workload.draw_checkpoint_version(&before), index);
        }
    }
    if !replay_job_reservations
        .keys()
        .copied()
        .eq(0..u64::try_from(replay_job_reservations.len())?)
    {
        return Err("campaign stream job reservations are not contiguous".into());
    }
    if u64::try_from(replay_job_reservations.len())? > header.execution_budget {
        return Err("campaign stream exceeds its recorded execution budget".into());
    }
    let replay_job_parents = replay_job_reservations
        .values()
        .map(|record| record.0)
        .collect::<Vec<_>>();
    let replay_job_suffix_limits = replay_job_reservations
        .values()
        .map(|record| record.1)
        .collect::<Vec<_>>();
    let replay_job_splices = replay_job_reservations
        .into_values()
        .map(|(_, _, seed, path, splice)| (seed, path, splice))
        .collect::<Vec<_>>();
    let replay_job_metadata = replay_job_parents
        .iter()
        .map(|parent_id| vec![*parent_id])
        .collect::<Vec<_>>();
    let resume_input = match header.origin_kind.as_str() {
        ORIGIN_GENESIS => {
            if origin_source.is_some() {
                return Err("genesis campaign replay does not take a source archive".into());
            }
            if origin_checkpoint.is_some() {
                return Err("genesis campaign replay does not take a snapshot checkpoint".into());
            }
            Input::default()
        }
        ORIGIN_SNAPSHOT_ROOT => {
            if origin_source.is_some() {
                return Err("snapshot-root replay does not take a source archive".into());
            }
            if origin_checkpoint.is_none() {
                return Err("snapshot-root replay requires its snapshot checkpoint".into());
            }
            Input::default()
        }
        ORIGIN_ARCHIVE => {
            let source =
                origin_source.ok_or("archive campaign replay requires the source archive")?;
            if header.origin_archive_sha256.as_deref() != Some(source.file_sha256()) {
                return Err(
                    "campaign replay source archive bytes do not match the recorded digest".into(),
                );
            }
            if header.origin_checkpoint_sha256.is_some() != origin_checkpoint.is_some() {
                return Err(
                    "archive campaign replay checkpoint does not match the recorded stream".into(),
                );
            }
            workload.resume_input(source.report())?
        }
        ORIGIN_SEARCH_CHECKPOINT => {
            return Err("a stream that resumes from a search checkpoint cannot be replayed".into());
        }
        _ => return Err("campaign stream origin kind is not recognized".into()),
    };
    let resume_input_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&resume_input)?));
    if resume_input_sha256 != header.resume_input_sha256
        || resume_input.actions.len() != header.resume_actions
    {
        return Err("campaign replay resume input does not match the recorded stream".into());
    }

    let replay_retention = retention_policy_from_identifier(&header.retention_policy)?;
    if header.parent_scheduler != SELECTOR_IDENTIFIER {
        return Err("campaign stream parent scheduler is not recognized".into());
    }
    let expected_resume = if header.origin_kind == ORIGIN_SNAPSHOT_ROOT {
        SNAPSHOT_ROOT_RESUME_IDENTIFIER
    } else {
        RESUME_IDENTIFIER
    };
    if header.resume_policy != expected_resume {
        return Err("campaign stream resume policy is not recognized".into());
    }
    let replay_mixture = draw_mixture_from_identifier(&header.mixture_policy)?;
    let replay_run = resolve_recorded_policies(workload, &header.workload_policies)?;
    let action_cost = workload.action_cost_fn();
    if let Some(checkpoint) = origin_checkpoint {
        if header.origin_kind == ORIGIN_ARCHIVE {
            validate_archive_checkpoint(workload, checkpoint)?;
        }
        let sha256_matches =
            header.origin_checkpoint_sha256.as_deref() == Some(checkpoint.file_sha256.as_str());
        let logical_path_matches =
            header.origin_checkpoint_path.as_deref() == Some(checkpoint.path.as_str());
        if !sha256_matches || !logical_path_matches {
            return Err("campaign replay checkpoint does not match the recorded stream".into());
        }
    }
    let draw_origin = match header.origin_kind.as_str() {
        ORIGIN_ARCHIVE => Some((
            origin_source
                .ok_or("archive campaign replay requires the source archive")?
                .file_sha256(),
            origin_source
                .ok_or("archive campaign replay requires the source archive")?
                .report(),
        )),
        ORIGIN_GENESIS | ORIGIN_SNAPSHOT_ROOT => None,
        _ => return Err("campaign stream origin kind is not recognized".into()),
    };
    let (mut draw_state, replay_draw_header) =
        initial_draw_state(workload, &replay_run, draw_origin)?;
    let mut duration_policies = DurationPolicies::<G::Key>::new();
    if let Some(memory_budget_mib) = header.memory_budget_mib {
        let budget = memory_budget_mib.saturating_mul(1024 * 1024);
        if budget
            <= workload
                .draw_state_memory_reserve_bytes(&replay_run)
                .saturating_add(duration_memory_reserve)
        {
            return Err("recorded memory budget is too small for the bounded draw state".into());
        }
    }
    if !memory_is_within_reserve(
        workload.draw_state_memory_bytes(&draw_state),
        workload.draw_state_memory_reserve_bytes(&replay_run),
    ) {
        return Err("replay draw state exceeds its deterministic memory reserve".into());
    }
    if replay_draw_header != header.draw_header {
        return Err("re-derived draw table does not match the recorded header".into());
    }
    workload.remember_draw_version(&mut draw_state, &required_draw_versions)?;
    let mut core = CoordinatorCore::new(
        workload,
        &replay_run,
        header.archive_entry_limit,
        header.memory_budget_mib,
    );
    core.record_progress = true;
    core.bounded_progress_curve = true;
    core.archive
        .enable_continuations(usize::from(SUFFIX_DOUBLING_LIMIT));
    let mut counters = CampaignCounters::new();
    let mut target = workload.new_target().map_err(|error| -> Box<dyn Error> {
        format!("failed to build the replay target: {error}").into()
    })?;
    let work_before = workload.execution_work(&target);
    counters.tree_import = match header.origin_kind.as_str() {
        ORIGIN_GENESIS => {
            core.bootstrap(workload, &replay_run, &mut target)?;
            None
        }
        ORIGIN_SNAPSHOT_ROOT => {
            core.bootstrap_snapshot_root(
                workload,
                &replay_run,
                &mut target,
                origin_checkpoint.ok_or("snapshot-root replay requires its snapshot checkpoint")?,
            )?;
            None
        }
        ORIGIN_ARCHIVE => Some(
            core.import_tree(
                workload,
                &replay_run,
                &mut target,
                origin_source
                    .ok_or("archive campaign replay requires the source archive")?
                    .report(),
                origin_checkpoint.map(|checkpoint| &checkpoint.snapshots),
            )?,
        ),
        _ => return Err("campaign stream origin kind is not recognized".into()),
    };
    counters.bootstrap_execution_work = execution_work_delta(
        work_before,
        workload.execution_work(&target),
        "replay bootstrap",
    )?;
    if core.objectives_reached > 0 {
        counters.note_first_objective(0);
    }
    let bootstrap_memory_bytes = core
        .archive
        .resident_memory_bytes()
        .saturating_add(workload.draw_state_memory_bytes(&draw_state))
        .saturating_add(duration_policies.memory_bytes());
    if !resident_memory_is_within_budget(bootstrap_memory_bytes, header.memory_budget_mib) {
        return Err(
            "campaign replay bootstrap state exceeds its deterministic memory budget".into(),
        );
    }

    core.archive.prepare_selection();
    let mut observed_progress_tiers = BTreeSet::new();
    for entry in &core.archive.entries {
        observed_progress_tiers.insert(entry.key.progress());
        if observed_progress_tiers.len() > usize::from(u8::MAX) {
            break;
        }
    }

    let replay_window_depth = schedule_policy_window(&header.schedule_policy)
        .ok_or("campaign stream schedule policy is not recognized")?;
    if stop_reservations_after_objective(
        header.stop_campaign_on_objective,
        core.objective_witness.is_some(),
    ) && !record_lines.is_empty()
    {
        return Err("campaign replay selected work after its objective stop".into());
    }
    if stop_reservations_after_work_budget(
        header.work_budget,
        counters.bootstrap_execution_work,
        counters.job_execution_work,
    ) && !replay_job_parents.is_empty()
    {
        return Err("campaign replay selected work after its work budget".into());
    }
    if require_terminal
        && header.wall_budget_seconds.is_none()
        && !stop_reservations_after_objective(
            header.stop_campaign_on_objective,
            core.objective_witness.is_some(),
        )
        && !stop_reservations_after_work_budget(
            header.work_budget,
            counters.bootstrap_execution_work,
            counters.job_execution_work,
        )
        && replay_job_parents.len()
            < usize::try_from(
                header
                    .execution_budget
                    .min(u64::try_from(replay_window_depth)?),
            )?
    {
        return Err("campaign stream ended before required reservations completed".into());
    }
    let mut duration_admissions = VecDeque::new();
    let mut duration_work = VecDeque::from([(0, counters.bootstrap_execution_work)]);
    let mut replay_metadata_uses = BTreeMap::<u64, u32>::new();
    let mut replay_job_snapshots =
        BTreeMap::<usize, (Arc<G::Snapshot>, Vec<G::Action>, Vec<Option<[u8; 32]>>, u64)>::new();
    let mut replay_continuations = BTreeMap::<usize, ContinuationReservation<G>>::new();
    let mut replay_continuation_energy = BTreeMap::<usize, Option<u16>>::new();
    let mut replay_job_weights = BTreeMap::<usize, (u8, u8)>::new();
    let mut replay_job_tier_limits = BTreeMap::<usize, usize>::new();
    let mut replay_selections = BTreeMap::<usize, (u64, u64)>::new();
    let mut replay_planned_clock = 0_u64;
    let mut last_admitted = None::<(u64, usize)>;
    core.archive.preserve_inactive_snapshots(false)?;
    for (job_slot, parent_id) in replay_job_parents
        .iter()
        .take(replay_window_depth)
        .enumerate()
    {
        replay_job_tier_limits.insert(job_slot, observed_progress_tiers.len());
        let (energy, continuation) =
            continuation_attempt(&mut core, header.campaign_seed, u64::try_from(job_slot)?);
        let actual_continuation = continuation.is_some();
        replay_continuation_energy.insert(job_slot, energy);
        if let Some(continuation) = continuation {
            replay_continuations.insert(job_slot, continuation);
            replay_job_weights.insert(job_slot, (0, u8::MAX));
        } else {
            replay_job_weights.insert(
                job_slot,
                mixture_weights(&core.mixture_energy, replay_mixture),
            );
        }
        let parent_index = core
            .archive
            .index_of_id(*parent_id)
            .ok_or("initial replay job names a parent the archive does not hold")?;
        if replay_job_suffix_limits[job_slot]
            .is_some_and(|recorded| core.suffix_limit(*parent_id, parent_index) != recorded)
        {
            return Err("replayed job suffix limit diverged at reservation".into());
        }
        let (mutation_seed, selector_path, splice) = &replay_job_splices[job_slot];
        verify_splice_selection(
            &mut core,
            parent_index,
            *mutation_seed,
            replay_job_weights[&job_slot],
            *selector_path,
            actual_continuation,
            splice.as_ref(),
        )?;
        core.start_suffix_job(*parent_id, replay_job_suffix_limits[job_slot]);
        replay_job_snapshots.insert(job_slot, core.archive.pin_job_origin(parent_index)?);
        replay_selections.insert(job_slot, (replay_planned_clock, core.sequence));
        for metadata_id in &replay_job_metadata[job_slot] {
            let uses = replay_metadata_uses.entry(*metadata_id).or_default();
            *uses = uses
                .checked_add(1)
                .ok_or("recorded metadata use count overflow")?;
        }
    }
    core.archive
        .preserve_recorded_metadata_uses(replay_metadata_uses.clone());

    let mut replay_admissions = 0_usize;
    let mut replay_skips = ReplaySkipSchedule::default();
    for (replay_record_index, line) in record_lines.into_iter().enumerate() {
        draw_state.release_versions(&required_draw_versions, replay_record_index);
        let record: CampaignStreamRecord<EmpiricalStepCheckpoint, G::Key> =
            serde_json::from_str(line)?;
        match record {
            CampaignStreamRecord::Skip(skip) => {
                replay_skips.accept_skip(
                    skip.reservation,
                    replay_admissions,
                    replay_window_depth,
                    replay_job_parents.len(),
                )?;
                if replay_continuations.contains_key(&usize::try_from(skip.reservation)?) {
                    return Err(
                        "campaign replay recorded a skip before a taken continuation".into(),
                    );
                }
                if stop_reservations_after_objective(
                    header.stop_campaign_on_objective,
                    core.objective_witness.is_some(),
                ) {
                    return Err("campaign replay selected work after its objective stop".into());
                }
                if stop_reservations_after_work_budget(
                    header.work_budget,
                    counters.bootstrap_execution_work,
                    counters.job_execution_work,
                ) {
                    return Err("campaign replay selected work after its work budget".into());
                }
                if (skip.mixture_weight, skip.splice_weight)
                    != mixture_weights(&core.mixture_energy, replay_mixture)
                {
                    return Err(
                        "recorded skip mixture weights diverge from the rebuilt policy".into(),
                    );
                }
                let parent_index = core
                    .archive
                    .index_of_id(skip.parent_id)
                    .ok_or("recorded skip names a parent the archive does not hold")?;
                verify_splice_selection(
                    &mut core,
                    parent_index,
                    skip.mutation_seed,
                    (skip.mixture_weight, skip.splice_weight),
                    skip.selector.path,
                    false,
                    skip.splice.as_ref(),
                )?;
                let strategy =
                    energy_strategy(skip.mutation_seed, skip.mixture_weight, skip.splice_weight)?;
                let spliced = replay_splice::<G>(strategy, skip.splice)?;
                let draw_checkpoint_before = skip.draw_checkpoint_before;
                if spliced.is_some()
                    && (skip.duration_draw.is_some()
                        || skip.duration_admission_sequence_at_draw.is_some()
                        || skip.duration_remaining_work.is_some()
                        || skip.duration_checkpoint_at_draw.is_some())
                {
                    return Err("recorded splice carries unexpected duration evidence".into());
                }
                let recorded_duration_draw =
                    spliced.is_none().then_some(skip.duration_draw).flatten();
                let duration_remaining_work = validate_duration_reservation(
                    recorded_duration_draw.as_ref(),
                    skip.duration_admission_sequence_at_draw,
                    skip.duration_remaining_work,
                    core.sequence,
                    core.sequence,
                    header.work_budget,
                    counters
                        .bootstrap_execution_work
                        .saturating_add(counters.job_execution_work),
                    &duration_work,
                )?;
                let duration_draw = if spliced.is_none() {
                    validate_duration_draw(
                        workload,
                        &replay_run,
                        core.archive.entries[parent_index].key,
                        header.work_budget,
                        duration_remaining_work,
                        recorded_duration_draw,
                    )?
                } else {
                    None
                };
                if let Some(draw) = duration_draw {
                    let admission_sequence = skip
                        .duration_admission_sequence_at_draw
                        .ok_or("recorded duration choice is missing its admission sequence")?;
                    if admission_sequence != core.sequence {
                        return Err(
                            "recorded skip duration was not drawn at its selection point".into(),
                        );
                    }
                    let expected_checkpoint = duration_checkpoint_at_reservation(
                        &duration_policies,
                        &duration_admissions,
                        draw.context,
                        admission_sequence,
                    );
                    if expected_checkpoint != skip.duration_checkpoint_at_draw {
                        return Err("recorded skip duration state diverged at selection".into());
                    }
                    verify_duration_draw(&draw, expected_checkpoint.as_ref(), skip.mutation_seed)?;
                } else if skip.duration_checkpoint_at_draw.is_some() {
                    return Err("recorded stream carries duration state without a choice".into());
                }
                let duration_checkpoint_before = duration_draw
                    .and_then(|draw| duration_policies.context_checkpoint(draw.context));
                if duration_checkpoint_before != skip.duration_checkpoint_before {
                    return Err("replayed skip duration state diverged before expansion".into());
                }
                let suffix_limit = replay_suffix_limit(spliced.is_none(), skip.suffix_limit)?;
                if spliced.is_none() {
                    let mut not_yet_selected = BTreeMap::<(u64, u8), u32>::new();
                    for (&slot, _) in
                        replay_job_snapshots.range(usize::try_from(skip.reservation)?..)
                    {
                        if let Some(limit) = replay_job_suffix_limits[slot] {
                            let entry = not_yet_selected
                                .entry((replay_job_parents[slot], limit))
                                .or_default();
                            *entry = entry
                                .checked_add(1)
                                .ok_or("suffix reservation count overflow")?;
                        }
                    }
                    if suffix_limit
                        != Some(core.suffix_limit_excluding(
                            skip.parent_id,
                            parent_index,
                            &not_yet_selected,
                        ))
                    {
                        return Err("replayed skip suffix limit diverged at selection".into());
                    }
                }
                let mut suffix = match (spliced, duration_draw) {
                    (Some(tail), _) => tail,
                    (None, draw) => workload.expand_suffix_recorded_duration(
                        &replay_run,
                        &draw_state,
                        MixtureDraw {
                            mixture: replay_mixture,
                            weight: skip.mixture_weight,
                            splice_weight: skip.splice_weight,
                        },
                        draw_checkpoint_before.as_ref(),
                        skip.mutation_seed,
                        core.archive.last_action(parent_index),
                        draw,
                    )?,
                };
                if let Some(limit) = suffix_limit {
                    suffix.truncate(usize::from(limit));
                }
                if !core.all_prefixes_archived(parent_index, &suffix) {
                    return Err("recorded skip is not a duplicate at its stream position".into());
                }
                if skip.selector.path == SelectorPath::Continuation {
                    return Err("continuations cannot be recorded skips".into());
                }
                verify_selector_annotation(&skip.selector, header.archive_entry_limit)?;
                verify_selector_observed_tiers(&skip.selector, observed_progress_tiers.len())?;
                verify_selector_preference::<G::Key>(&skip.selector)?;
                core.archive.record_selection(parent_index, &skip.selector);
                core.archive.maintain_memory_budget()?;
                core.archive.prepare_selection();
                counters.duplicates_skipped = counters.duplicates_skipped.saturating_add(1);
                let draw_checkpoint_after = workload.finish_stream_record(
                    &replay_run,
                    &mut draw_state,
                    &[],
                    &core.evidence,
                )?;
                if draw_checkpoint_after != skip.draw_checkpoint_after {
                    return Err("replayed skip draw-table checkpoint diverged".into());
                }
                let duration_checkpoint_after = duration_draw
                    .and_then(|draw| duration_policies.context_checkpoint(draw.context));
                if duration_checkpoint_after != skip.duration_checkpoint_after {
                    return Err("replayed skip duration state diverged after expansion".into());
                }
                workload.remember_draw_version(&mut draw_state, &required_draw_versions)?;
            }
            CampaignStreamRecord::Job(job) => {
                if job.selector.path == SelectorPath::Continuation
                    && (job.mixture_weight != 0
                        || job.splice_weight != u8::MAX
                        || !matches!(&job.splice, Some(CampaignSpliceRecord::Tail { .. })))
                {
                    return Err(
                        "continuation job lacks its registered schedule or complete tail".into(),
                    );
                }
                let replay_job_slot = usize::try_from(job.reservation)?;
                replay_skips.admit(job.reservation);
                let selector_tier_limit = replay_job_tier_limits
                    .remove(&replay_job_slot)
                    .ok_or("recorded job has no rebuilt selection tier bound")?;
                if (job.mixture_weight, job.splice_weight)
                    != replay_job_weights
                        .remove(&replay_job_slot)
                        .ok_or("recorded job has no rebuilt mixture weights")?
                {
                    return Err(format!(
                        "replayed job {} mixture weights diverge from the rebuilt policy",
                        job.sequence
                    )
                    .into());
                }
                let admission_index = replay_admissions;
                replay_admissions = replay_admissions.saturating_add(1);
                let (planned_start, selection_sequence) = replay_selections
                    .remove(&replay_job_slot)
                    .ok_or("recorded job was admitted before its selection")?;
                let taken_continuation = replay_continuations.remove(&replay_job_slot);
                continuation_reservation_matches::<G>(
                    taken_continuation.as_ref(),
                    replay_continuation_energy
                        .remove(&replay_job_slot)
                        .ok_or("recorded job has no reconstructed reservation")?,
                    &job.selector,
                    job.splice.as_ref(),
                    job.continuation_energy,
                    job.sequence,
                )?;
                let parent_index = core
                    .archive
                    .index_of_id(job.parent_id)
                    .ok_or("recorded job names a parent the archive does not hold")?;
                let ((snapshot, replay, replay_certificates, snapshot_id), parent_milestones) = {
                    let entry = core
                        .archive
                        .entries
                        .get(parent_index)
                        .ok_or("recorded job names a parent the archive does not hold")?;
                    (
                        replay_job_snapshots
                            .remove(&replay_job_slot)
                            .ok_or("recorded job has no replayed in-flight snapshot")?,
                        entry.milestones,
                    )
                };
                let strategy =
                    energy_strategy(job.mutation_seed, job.mixture_weight, job.splice_weight)?;
                let spliced = replay_splice::<G>(strategy, job.splice.clone())?;
                let draw_checkpoint_before = job.draw_checkpoint_before;
                if spliced.is_some()
                    && (job.duration_draw.is_some()
                        || job.duration_admission_sequence_at_draw.is_some()
                        || job.duration_remaining_work.is_some()
                        || job.duration_checkpoint_at_draw.is_some())
                {
                    return Err("recorded splice carries unexpected duration evidence".into());
                }
                let recorded_duration_draw =
                    spliced.is_none().then_some(job.duration_draw).flatten();
                let duration_remaining_work = validate_duration_reservation(
                    recorded_duration_draw.as_ref(),
                    job.duration_admission_sequence_at_draw,
                    job.duration_remaining_work,
                    core.sequence,
                    selection_sequence,
                    header.work_budget,
                    counters
                        .bootstrap_execution_work
                        .saturating_add(counters.job_execution_work),
                    &duration_work,
                )?;
                let duration_draw = if spliced.is_none() {
                    validate_duration_draw(
                        workload,
                        &replay_run,
                        core.archive.entries[parent_index].key,
                        header.work_budget,
                        duration_remaining_work,
                        recorded_duration_draw,
                    )?
                } else {
                    None
                };
                if let Some(draw) = duration_draw {
                    let admission_sequence = job
                        .duration_admission_sequence_at_draw
                        .ok_or("recorded duration choice is missing its admission sequence")?;
                    let expected_checkpoint = duration_checkpoint_at_reservation(
                        &duration_policies,
                        &duration_admissions,
                        draw.context,
                        admission_sequence,
                    );
                    if expected_checkpoint != job.duration_checkpoint_at_draw {
                        return Err("recorded job duration state diverged at selection".into());
                    }
                    verify_duration_draw(&draw, expected_checkpoint.as_ref(), job.mutation_seed)?;
                } else if job.duration_checkpoint_at_draw.is_some() {
                    return Err("recorded stream carries duration state without a choice".into());
                }
                let duration_checkpoint_before = duration_draw
                    .and_then(|draw| duration_policies.context_checkpoint(draw.context));
                if duration_checkpoint_before != job.duration_checkpoint_before {
                    return Err("replayed job duration state diverged before expansion".into());
                }
                let suffix_limit = replay_suffix_limit(spliced.is_none(), job.suffix_limit)?;
                let mut suffix = match (spliced, duration_draw) {
                    (Some(tail), _) => tail,
                    (None, draw) => workload.expand_suffix_recorded_duration(
                        &replay_run,
                        &draw_state,
                        MixtureDraw {
                            mixture: replay_mixture,
                            weight: job.mixture_weight,
                            splice_weight: job.splice_weight,
                        },
                        draw_checkpoint_before.as_ref(),
                        job.mutation_seed,
                        core.archive.last_action(parent_index),
                        draw,
                    )?,
                };
                if let Some(limit) = suffix_limit {
                    suffix.truncate(usize::from(limit));
                }
                let finish = replay
                    .iter()
                    .chain(&suffix)
                    .map(action_cost)
                    .fold(planned_start, u64::saturating_add);
                if last_admitted.is_some_and(|last| last >= (finish, replay_job_slot)) {
                    return Err(format!(
                        "replayed job {} was admitted out of planned-finish order",
                        job.sequence
                    )
                    .into());
                }
                last_admitted = Some((finish, replay_job_slot));
                replay_planned_clock = finish;
                let job_execution_work_before = workload.execution_work(&target);
                let result = workload.execute_job_certified(
                    &replay_run,
                    &mut target,
                    &snapshot,
                    &replay,
                    &replay_certificates,
                    parent_milestones,
                    &suffix,
                    replay_retention,
                    header.stop_rollout_on_objective,
                )?;
                let execution_work = execution_work_delta(
                    job_execution_work_before,
                    workload.execution_work(&target),
                    "replayed job",
                )?;
                if execution_work != job.execution_work {
                    return Err(format!(
                        "replayed job {} measured {execution_work} execution-work units against recorded {}",
                        job.sequence, job.execution_work
                    )
                    .into());
                }
                let digest = workload.certified_result_sha256(&result, &target)?;
                if digest != job.result_sha256 {
                    return Err(format!(
                        "replayed job {} result digest {digest} diverged from recorded {}",
                        job.sequence, job.result_sha256
                    )
                    .into());
                }
                let tracked_duration = duration_draw
                    .filter(|_| execution_work > 0)
                    .map(|draw| draw.duration);
                drop(snapshot);
                let objectives_before = core.objectives_reached;
                let continuation_wave = taken_continuation
                    .as_ref()
                    .map_or(0, |taken| taken.wave.saturating_add(1));
                core.archive.set_admission_wave(continuation_wave);
                core.finish_suffix_job(job.parent_id, suffix_limit);
                let (sequence, decisions, duration_admission) = core.admit_job_tracking(
                    workload,
                    job.parent_id,
                    &mut { result },
                    suffix_limit.is_some(),
                    |action| {
                        tracked_duration.is_some_and(|duration| {
                            workload.duration_of_action(&replay_run, action) == Some(duration)
                        })
                    },
                )?;
                if sequence != job.sequence {
                    return Err(format!(
                        "replayed admission order {sequence} diverged from recorded {}",
                        job.sequence
                    )
                    .into());
                }
                if decisions != job.decisions {
                    return Err(format!(
                        "replayed job {} admission decisions diverged from the recorded stream",
                        job.sequence
                    )
                    .into());
                }
                if observed_progress_tiers.len() <= usize::from(u8::MAX) {
                    for index in retained_archive_indexes(&core, &decisions) {
                        observed_progress_tiers.insert(core.archive.entries[index].key.progress());
                        if observed_progress_tiers.len() > usize::from(u8::MAX) {
                            break;
                        }
                    }
                }
                if duration_admission.applied {
                    let draw = duration_draw
                        .ok_or("duration application was reported without a duration draw")?;
                    let evicted = duration_policies.eviction_checkpoint(draw.context);
                    duration_policies.observe(
                        draw.context,
                        draw.duration,
                        duration_admission.useful,
                        NonZeroU64::new(execution_work)
                            .ok_or("duration application requires positive execution work")?,
                    )?;
                    duration_admissions.push_back(DurationAdmissionRecord {
                        sequence,
                        context: draw.context,
                        before: duration_checkpoint_before.clone(),
                        evicted,
                    });
                }
                let duration_checkpoint_after = duration_draw
                    .and_then(|draw| duration_policies.context_checkpoint(draw.context));
                if duration_checkpoint_after != job.duration_checkpoint_after {
                    return Err("replayed job duration state diverged after admission".into());
                }
                let draw_checkpoint_after =
                    finish_record(workload, &replay_run, &mut draw_state, &core, &decisions)?;
                if draw_checkpoint_after != job.draw_checkpoint_after {
                    return Err(format!(
                        "replayed job {} draw-table checkpoint diverged",
                        job.sequence
                    )
                    .into());
                }
                let draw_state_memory_bytes = workload.draw_state_memory_bytes(&draw_state);
                if !memory_is_within_reserve(
                    draw_state_memory_bytes,
                    workload.draw_state_memory_reserve_bytes(&replay_run),
                ) {
                    return Err("replay draw state exceeds its deterministic memory reserve".into());
                }
                if !memory_is_within_reserve(
                    duration_policies.memory_bytes(),
                    duration_memory_reserve,
                ) {
                    return Err("replay adaptive duration state exceeds its memory reserve".into());
                }
                workload.remember_draw_version(&mut draw_state, &required_draw_versions)?;
                verify_selector_annotation(&job.selector, header.archive_entry_limit)?;
                verify_selector_observed_tiers(&job.selector, selector_tier_limit)?;
                verify_selector_preference::<G::Key>(&job.selector)?;
                if job.selector.path == SelectorPath::Continuation {
                    core.archive.record_isolated_continuation(parent_index);
                    let new_cell_descendant = retained_archive_indexes(&core, &decisions)
                        .any(|id| core.archive.opened_new_cell(id));
                    let (landed, replaced) = continuation_arrival::<G>(
                        &core.archive,
                        taken_continuation
                            .as_ref()
                            .map(|taken| (taken.source, taken.destination)),
                        &decisions,
                    );
                    core.archive.record_continuation_outcome(
                        landed,
                        replaced,
                        new_cell_descendant,
                        continuation_wave,
                    );
                    core.archive.add_continuation_execution_work(execution_work);
                } else {
                    core.archive.record_selection(parent_index, &job.selector);
                    let retained = retained_archive_indexes(&core, &decisions).next().is_some();
                    core.archive
                        .record_selection_outcome(parent_index, retained);
                }
                let new_slot_descendant = retained_archive_indexes(&core, &decisions)
                    .any(|id| core.archive.opened_new_slot(id));
                record_mixture_outcome(
                    &mut core.mixture_energy,
                    replay_mixture,
                    job.selector.path,
                    job.mutation_seed,
                    (job.mixture_weight, job.splice_weight),
                    new_slot_descendant,
                    execution_work,
                )?;
                core.archive.unpin_job_origin(snapshot_id);
                core.archive.unpin_metadata(job.parent_id);
                core.archive.maintain_memory_budget()?;
                core.archive.prepare_selection();
                for metadata_id in &replay_job_metadata[replay_job_slot] {
                    if let Some(uses) = replay_metadata_uses.get_mut(metadata_id) {
                        *uses = uses.saturating_sub(1);
                        if *uses == 0 {
                            replay_metadata_uses.remove(metadata_id);
                        }
                    }
                }
                let next_slot = admission_index.saturating_add(replay_window_depth);
                if stop_reservations_after_objective(
                    header.stop_campaign_on_objective,
                    core.objective_witness.is_some(),
                ) && replay_job_parents.get(next_slot).is_some()
                {
                    return Err("campaign replay selected work after its objective stop".into());
                }
                if stop_reservations_after_work_budget(
                    header.work_budget,
                    counters.bootstrap_execution_work,
                    counters.job_execution_work.saturating_add(execution_work),
                ) && replay_job_parents.get(next_slot).is_some()
                {
                    return Err("campaign replay selected work after its work budget".into());
                }
                if require_terminal
                    && header.wall_budget_seconds.is_none()
                    && u64::try_from(next_slot)? < header.execution_budget
                    && !stop_reservations_after_objective(
                        header.stop_campaign_on_objective,
                        core.objective_witness.is_some(),
                    )
                    && !stop_reservations_after_work_budget(
                        header.work_budget,
                        counters.bootstrap_execution_work,
                        counters.job_execution_work.saturating_add(execution_work),
                    )
                    && replay_job_parents.get(next_slot).is_none()
                {
                    return Err(
                        "campaign stream ended before required reservations completed".into(),
                    );
                }
                if let Some(parent_id) = replay_job_parents.get(next_slot) {
                    replay_job_tier_limits.insert(next_slot, observed_progress_tiers.len());
                    let (energy, continuation) = continuation_attempt(
                        &mut core,
                        header.campaign_seed,
                        u64::try_from(next_slot)?,
                    );
                    let actual_continuation = continuation.is_some();
                    replay_continuation_energy.insert(next_slot, energy);
                    if let Some(continuation) = continuation {
                        replay_continuations.insert(next_slot, continuation);
                        replay_job_weights.insert(next_slot, (0, u8::MAX));
                    } else {
                        replay_job_weights.insert(
                            next_slot,
                            mixture_weights(&core.mixture_energy, replay_mixture),
                        );
                    }
                    let parent_index = core
                        .archive
                        .index_of_id(*parent_id)
                        .ok_or("next replay job names a parent the archive does not hold")?;
                    if replay_job_suffix_limits[next_slot].is_some_and(|recorded| {
                        core.suffix_limit(*parent_id, parent_index) != recorded
                    }) {
                        return Err("replayed job suffix limit diverged at reservation".into());
                    }
                    let (mutation_seed, selector_path, splice) = &replay_job_splices[next_slot];
                    verify_splice_selection(
                        &mut core,
                        parent_index,
                        *mutation_seed,
                        replay_job_weights[&next_slot],
                        *selector_path,
                        actual_continuation,
                        splice.as_ref(),
                    )?;
                    core.start_suffix_job(*parent_id, replay_job_suffix_limits[next_slot]);
                    replay_job_snapshots
                        .insert(next_slot, core.archive.pin_job_origin(parent_index)?);
                    replay_selections.insert(next_slot, (replay_planned_clock, core.sequence));
                    for metadata_id in &replay_job_metadata[next_slot] {
                        let uses = replay_metadata_uses.entry(*metadata_id).or_default();
                        *uses = uses
                            .checked_add(1)
                            .ok_or("recorded metadata use count overflow")?;
                    }
                }
                core.archive
                    .preserve_recorded_metadata_uses(replay_metadata_uses.clone());
                counters.job_execution_work =
                    counters.job_execution_work.saturating_add(execution_work);
                duration_work.push_back((
                    sequence,
                    counters
                        .bootstrap_execution_work
                        .saturating_add(counters.job_execution_work),
                ));
                let oldest_selection = replay_selections
                    .first_key_value()
                    .map_or(core.sequence, |(_, (_, selection))| *selection);
                while duration_work
                    .front()
                    .is_some_and(|(sequence, _)| *sequence < oldest_selection)
                {
                    duration_work.pop_front();
                }
                while duration_admissions
                    .front()
                    .is_some_and(|admission| admission.sequence <= oldest_selection)
                {
                    duration_admissions.pop_front();
                }
                if objectives_before == 0 && core.objectives_reached > 0 {
                    counters.note_first_objective(sequence);
                }
            }
            CampaignStreamRecord::End(_) => {
                return Err("campaign stream has a premature terminal record".into());
            }
        }
    }
    if let Some(end) = end
        && (end.jobs != u64::try_from(replay_job_parents.len())?
            || end.skips != counters.duplicates_skipped
            || end.executions_completed != core.sequence)
    {
        return Err("campaign stream terminal counts diverged".into());
    }
    core.archive.preserve_inactive_snapshots(false)?;
    core.archive.compact_history_for_final_report()?;
    core.finish_curve();

    let origin = CampaignOriginRecord {
        kind: header.origin_kind.clone(),
        path: header.origin_path.clone(),
        archive_sha256: header.origin_archive_sha256.clone(),
        checkpoint_path: header.origin_checkpoint_path.clone(),
        checkpoint_sha256: header.origin_checkpoint_sha256.clone(),
        checkpoint_policy_changes: header.origin_checkpoint_policy_changes.clone(),
        resume_input_sha256: header.resume_input_sha256.clone(),
        resume_actions: header.resume_actions,
    };
    counters.draw_state_memory_bytes = workload
        .draw_state_memory_bytes(&draw_state)
        .saturating_add(duration_policies.memory_bytes());
    let mut outcome = build_report(
        workload,
        &header,
        origin,
        core,
        &counters,
        stream_sha256,
        true,
    )?;
    if !require_terminal {
        outcome.0.mode = "campaign-prefix".to_owned();
    }
    Ok(outcome)
}

#[cfg(test)]
#[path = "campaign_continuation_tests.rs"]
mod continuation_tests;

#[cfg(test)]
mod tests {
    use std::{
        io::Write,
        sync::{Arc, Mutex, mpsc},
    };

    use super::{
        ArchiveReportState, ArchiveSource, CampaignActionResult, CampaignAdmissionDecision,
        CampaignCandidate, CampaignCheckpoint, CampaignConfig, CampaignCoordinator,
        CampaignCounters, CampaignExecutionOptions, CampaignJobRecord, CampaignJobResult,
        CampaignOrigin, CampaignOutcome, CampaignProgressRecord, CampaignSpliceRecord,
        CampaignStreamHeader, CampaignStreamRecord, CampaignTypes, ContinuationAccounting,
        CoordinatorCore, DrawTables, DurationAdmission, EmpiricalStepCheckpoint, EnergyStrategy,
        Evaluation, InputPolicy, JobOutcome, JobSpec, LONGEST_SPLICE_TAIL, LiveCoordinatorProfile,
        MAX_PROGRESS_CURVE_POINTS, PROGRESS_CHECKPOINT_INTERVAL, PlacedThread, Reporting,
        RomuDuoJrRand, SnapshotCheckpoint, SnapshotCheckpointEntry, TargetExecution,
        ThreadPlacement, WorkloadPolicies, archive_entry_limit_is_valid, compact_progress_curve,
        coordinator_session_result, default_window, draws_continuation, draws_highest_preference,
        execution_work_delta, finish_record, full_selector_report_due, is_zero_usize,
        memory_is_within_reserve, postcard_value_sha256, profile_elapsed, progress_checkpoint_due,
        progress_line_interval, progress_policy_is_supported, record_compaction_elapsed,
        record_mixture_outcome, replay_campaign_checkpointed, replay_campaign_prefix_checkpointed,
        replay_splice, resident_memory_is_within_budget, retained_archive_indexes,
        run_campaign_checkpointed, run_campaign_checkpointed_with_options, run_job,
        schedule_policy_identifier, schedule_policy_is_supported, schedule_policy_window,
        stop_reservations_after_objective, with_coordinator_session,
    };
    use crate::search::archive::{
        ArchiveEntryReport, ArchiveKey, Input, ProgressPoint, RetentionPolicy, SelectorDraw,
        SelectorPath, entries_by_suffix,
    };
    use crate::search::draw::{DrawMixture, MixtureDraw, MixtureEnergy};
    use crate::search::rollout::{ExecutionDisposition, Outcome, Rollout, execute_suffix};
    use serde::{Deserialize, Serialize};
    use sha2::{Digest, Sha256};
    use std::{
        collections::BTreeSet,
        error::Error,
        fmt,
        time::{Duration, Instant},
    };

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
            self.0
        }

        fn progress(self) -> Self::Progress {}

        fn identity(self) -> Self::Identity {}

        type Lineage = ();

        fn complete(self, parent: Option<(Self, &Self::Lineage)>) -> Self {
            match parent {
                Some((parent, ())) if self.0 == INHERITS_PARENT_PLACE => parent,
                _ => self,
            }
        }

        fn record(_lineage: &mut Self::Lineage, _key: Self) {}
    }

    const INHERITS_PARENT_PLACE: u8 = u8::MAX;

    type TestActionHook = dyn Fn(TestAction) -> Result<(), String> + Send + Sync;

    struct BootstrapReadiness {
        coordinator: std::thread::ThreadId,
        finalized: std::sync::atomic::AtomicBool,
        worker_starts: std::sync::atomic::AtomicUsize,
    }
    #[derive(Default)]
    struct LiveTargets {
        locality: bool,
        locality_reads: std::sync::atomic::AtomicUsize,
        residency: Option<Arc<ResidencyFixture>>,
        bootstrap_readiness: Option<BootstrapReadiness>,
        refuse: bool,
        integrity_invalidated: std::sync::atomic::AtomicBool,
        integrity_failure_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
        fail_next_target: std::sync::atomic::AtomicBool,
        first_action_started: std::sync::atomic::AtomicBool,
        second_selection_waits_for_first_action: std::sync::atomic::AtomicBool,
        live: std::sync::atomic::AtomicUsize,
        most: std::sync::atomic::AtomicUsize,
        objective_after_work: std::sync::atomic::AtomicU64,
        action_hook: Mutex<Option<Arc<TestActionHook>>>,
        selections: std::sync::atomic::AtomicUsize,
    }

    #[derive(Default)]
    struct TestTarget {
        value: u8,
        execution_work: u64,
        _counted: Option<TargetCount>,
        locality_worker: Option<u32>,
        resident_snapshots: std::collections::BTreeMap<u8, Box<u8>>,
    }

    struct TargetCount(Arc<LiveTargets>);

    impl Drop for TargetCount {
        fn drop(&mut self) {
            self.0
                .live
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
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
        #[serde(with = "entries_by_suffix")]
        entries: Vec<ArchiveEntryReport<TestAction, TestKey, ()>>,
        #[serde(default)]
        marker: u8,
    }

    struct TestWorkload {
        bootstrap_objective: bool,
        targets: Option<Arc<LiveTargets>>,
    }
    std::thread_local! {
        static FINALIZATION: std::cell::Cell<(usize, bool)> = const { std::cell::Cell::new((0, false)) };
        static CERTIFIED_IMPORT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static FUSED_IMPORT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static OMIT_IMPORT_CAPTURE: std::cell::Cell<Option<u8>> = const { std::cell::Cell::new(None) };
        static IMPORT_CALLS: std::cell::Cell<(u64, u64, u64)> = const { std::cell::Cell::new((0, 0, 0)) };
        static OMIT_TEST_PREFIX_CERTIFICATE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static EVIDENCE_WRITE_FAILURE: std::cell::Cell<Option<(usize, usize)>> = const { std::cell::Cell::new(None) };
        static EVIDENCE_PRESSURE_WRITE: std::cell::Cell<Option<(usize, usize)>> = const { std::cell::Cell::new(None) };
    }

    struct EvidenceWriteFailureGuard(Option<(usize, usize)>, Option<(usize, usize)>);

    impl EvidenceWriteFailureGuard {
        fn new(pass: usize) -> Self {
            Self(
                EVIDENCE_WRITE_FAILURE.replace(Some((pass, 0))),
                EVIDENCE_PRESSURE_WRITE.replace(None),
            )
        }

        fn after_overflow() -> Self {
            let guard = Self::new(2);
            EVIDENCE_PRESSURE_WRITE.set(Some((0, 0)));
            guard
        }

        fn calls(&self) -> usize {
            EVIDENCE_WRITE_FAILURE.get().unwrap().1
        }
    }

    impl Drop for EvidenceWriteFailureGuard {
        fn drop(&mut self) {
            EVIDENCE_WRITE_FAILURE.set(self.0);
            EVIDENCE_PRESSURE_WRITE.set(self.1);
        }
    }

    #[derive(Debug)]
    struct TestEvidenceWriteCause;

    impl fmt::Display for TestEvidenceWriteCause {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("typed evidence writer cause")
        }
    }

    impl Error for TestEvidenceWriteCause {}

    #[derive(Debug)]
    struct TestEvidenceWriteError {
        pass: usize,
        cause: TestEvidenceWriteCause,
    }

    impl fmt::Display for TestEvidenceWriteError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "typed evidence writer pass {}", self.pass)
        }
    }

    impl Error for TestEvidenceWriteError {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(&self.cause)
        }
    }

    impl CampaignTypes for TestWorkload {
        type JobObservation = ();
        type Target = TestTarget;
        type Action = TestAction;
        type Key = TestKey;
        type Milestones = ();
        type Progress = ();
        type Snapshot = u8;
        type Observations = ();
        type Evidence = u8;
        type ArchiveReport = TestArchiveReport;
        type Run = ();
    }

    impl Reporting for TestWorkload {
        fn write_evidence_checkpoint(
            evidence: &u8,
            out: &mut dyn Write,
        ) -> Result<(), Box<dyn Error>> {
            if let Some((pass, calls)) = EVIDENCE_WRITE_FAILURE.get() {
                EVIDENCE_WRITE_FAILURE.set(Some((pass, calls + 1)));
                if let Some((attempts, completed)) = EVIDENCE_PRESSURE_WRITE.get() {
                    let result = (|| -> std::io::Result<()> {
                        for _ in 0..128 {
                            out.write_all(&[0; 1024])?;
                        }
                        Ok(())
                    })();
                    EVIDENCE_PRESSURE_WRITE.set(Some((
                        attempts + 1,
                        completed + usize::from(result.is_ok()),
                    )));
                    if calls + 1 == pass {
                        return Err(Box::new(TestEvidenceWriteError {
                            pass,
                            cause: TestEvidenceWriteCause,
                        }));
                    }
                    result?;
                    return Ok(());
                }
                if calls + 1 == pass {
                    return Err(Box::new(TestEvidenceWriteError {
                        pass,
                        cause: TestEvidenceWriteCause,
                    }));
                }
            }
            out.write_all(&[*evidence])?;
            Ok(())
        }
        fn evidence_from_checkpoint(bytes: &[u8]) -> Result<u8, Box<dyn Error>> {
            Ok(bytes.first().copied().unwrap_or_default())
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
        fn result_sha256(
            &self,
            result: &CampaignJobResult<Self>,
        ) -> Result<String, Box<dyn Error>> {
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
            evidence: &Self::Evidence,
            state: ArchiveReportState<Self>,
        ) -> Self::ArchiveReport {
            TestArchiveReport {
                entries: state.entries,
                marker: *evidence,
            }
        }
    }

    impl InputPolicy for TestWorkload {
        fn policies(&self, _run: &Self::Run) -> WorkloadPolicies {
            WorkloadPolicies::new()
        }
        fn resolve_recorded(
            &self,
            _policies: &WorkloadPolicies,
        ) -> Result<Self::Run, Box<dyn Error>> {
            Ok(())
        }
        fn sample_alphabet(
            &self,
            _run: &Self::Run,
            _previous: Option<&Self::Action>,
            rand: &mut RomuDuoJrRand,
        ) -> Result<Self::Action, Box<dyn Error>> {
            Ok(TestAction::new(rand.next_u64() as u8, 1))
        }
        #[allow(clippy::disallowed_methods)]
        fn expand_suffix(
            &self,
            _run: &Self::Run,
            _state: &DrawTables<Self::Action>,
            _mixture: MixtureDraw,
            mutation_seed: u64,
            _previous: Option<&Self::Action>,
        ) -> Result<Vec<Self::Action>, Box<dyn Error>> {
            if let Some(targets) = &self.targets {
                let selection = targets
                    .selections
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1;
                if selection == 2
                    && targets
                        .second_selection_waits_for_first_action
                        .load(std::sync::atomic::Ordering::SeqCst)
                {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while !targets
                        .first_action_started
                        .load(std::sync::atomic::Ordering::SeqCst)
                    {
                        if Instant::now() >= deadline {
                            return Err(
                                "the first job never started before the second selection".into()
                            );
                        }
                        std::thread::yield_now();
                    }
                }
            }
            let long = mutation_seed >> 32 & 1 == 1;
            let length = 1 + usize::from(long);
            Ok(vec![
                TestAction::new(mutation_seed as u8, 1 + u8::from(long));
                length
            ])
        }
        fn expand_suffix_recorded(
            &self,
            run: &Self::Run,
            state: &DrawTables<Self::Action>,
            mixture: MixtureDraw,
            _before: Option<&EmpiricalStepCheckpoint>,
            mutation_seed: u64,
            previous: Option<&Self::Action>,
        ) -> Result<Vec<Self::Action>, Box<dyn Error>> {
            self.expand_suffix(run, state, mixture, mutation_seed, previous)
        }
    }

    #[derive(Debug)]
    struct TestIntegrityViolation;

    impl std::fmt::Display for TestIntegrityViolation {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("injected determinism contradiction")
        }
    }

    impl Error for TestIntegrityViolation {}

    impl TargetExecution for TestWorkload {
        fn worker_locality(&self, target: &TestTarget) -> crate::search::locality::WorkerLocality {
            use crate::search::locality::{LocalityKey, WorkerLocality};
            if let Some(fixture) = self
                .targets
                .as_ref()
                .and_then(|targets| targets.residency.as_ref())
            {
                if target.locality_worker == Some(0) && fixture.case == ResidencyCase::Refill {
                    fixture.wait_for(|state| state.first_admitted);
                }
                return if fixture.hints {
                    WorkerLocality::new(
                        target
                            .resident_snapshots
                            .keys()
                            .map(|value| (LocalityKey([*value; 32]), 1)),
                    )
                } else {
                    WorkerLocality::default()
                };
            }
            if let Some(targets) = self.targets.as_ref().filter(|targets| targets.locality) {
                targets
                    .locality_reads
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                WorkerLocality::new([(LocalityKey([target.value; 32]), 1)])
            } else {
                WorkerLocality::default()
            }
        }

        fn snapshot_locality(&self, snapshot: &u8) -> Option<crate::search::locality::LocalityKey> {
            self.targets
                .as_ref()
                .filter(|targets| targets.locality)
                .map(|_| crate::search::locality::LocalityKey([*snapshot; 32]))
        }

        fn take_job_observation(&self) -> Option<Self::JobObservation> {
            Some(())
        }
        fn finish_coordinator_session(&self) -> Result<(), Box<dyn Error>> {
            FINALIZATION.with(|state| {
                let (calls, fail) = state.get();
                state.set((calls + 1, fail));
                if fail {
                    Err("test shutdown unconfirmed".into())
                } else {
                    if let Some(readiness) = self
                        .targets
                        .as_ref()
                        .and_then(|targets| targets.bootstrap_readiness.as_ref())
                    {
                        readiness
                            .finalized
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    Ok(())
                }
            })
        }
        fn verify_integrity(&self) -> Result<(), Box<dyn Error>> {
            if let Some(targets) = self.targets.as_ref().filter(|targets| {
                targets
                    .integrity_invalidated
                    .load(std::sync::atomic::Ordering::SeqCst)
            }) {
                if let Some(hook) = targets.integrity_failure_hook.lock().unwrap().clone() {
                    hook();
                }
                return Err(Box::new(TestIntegrityViolation));
            }
            Ok(())
        }

        fn new_target(&self) -> Result<Self::Target, String> {
            let Some(targets) = &self.targets else {
                return Ok(TestTarget::default());
            };
            if let Some(readiness) = &targets.bootstrap_readiness
                && std::thread::current().id() != readiness.coordinator
            {
                assert!(
                    readiness
                        .finalized
                        .load(std::sync::atomic::Ordering::SeqCst),
                    "worker started before bootstrap session finalized"
                );
                readiness
                    .worker_starts
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }

            if targets.refuse {
                return Err("the test target refused to start".to_owned());
            }
            if targets
                .fail_next_target
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err("the replacement target refused to start".to_owned());
            }
            let live = targets
                .live
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            targets
                .most
                .fetch_max(live, std::sync::atomic::Ordering::SeqCst);
            let locality_worker = LOCALITY_WORKER.with(std::cell::Cell::get);
            Ok(TestTarget {
                _counted: Some(TargetCount(Arc::clone(targets))),
                locality_worker,
                resident_snapshots: if targets.residency.is_some() && locality_worker == Some(1) {
                    std::collections::BTreeMap::from([(0, Box::new(0))])
                } else {
                    std::collections::BTreeMap::new()
                },
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
            if let Some(fixture) = self
                .targets
                .as_ref()
                .and_then(|targets| targets.residency.as_ref())
                && let Some(worker) = target.locality_worker
            {
                let reconstructed = if let std::collections::btree_map::Entry::Vacant(entry) =
                    target.resident_snapshots.entry(*snapshot)
                {
                    let decoded = postcard::from_bytes(&postcard::to_stdvec(snapshot)?)?;
                    entry.insert(Box::new(decoded));
                    true
                } else {
                    false
                };
                target.value = **target.resident_snapshots.get(snapshot).unwrap();
                fixture
                    .state
                    .lock()
                    .unwrap()
                    .restores
                    .push((worker, *snapshot, reconstructed));
                return Ok(());
            }
            target.value = *snapshot;
            Ok(())
        }
        fn execution_work(&self, target: &Self::Target) -> u64 {
            target.execution_work
        }
        fn telemetry(&self, target: &Self::Target) -> crate::search::telemetry::TargetCounters {
            crate::search::telemetry::TargetCounters::from([(
                "work".to_owned(),
                target.execution_work,
            )])
        }
        fn apply_action(
            &self,
            target: &mut Self::Target,
            action: &Self::Action,
            _milestones: &mut Self::Milestones,
        ) -> Result<(), Box<dyn Error>> {
            let hook = target._counted.as_ref().and_then(|count| {
                count
                    .0
                    .action_hook
                    .lock()
                    .expect("test action hook lock")
                    .clone()
            });
            if let Some(hook) = hook {
                hook(*action)?;
            }
            target.apply(action);
            Ok(())
        }
        fn apply_action_and_snapshot(
            &self,
            target: &mut Self::Target,
            action: &Self::Action,
            milestones: &mut Self::Milestones,
        ) -> Result<Option<Self::Snapshot>, Box<dyn Error>> {
            self.apply_action(target, action, milestones)?;
            if FUSED_IMPORT.with(|enabled| enabled.get())
                && !OMIT_IMPORT_CAPTURE.with(|omitted| omitted.get() == Some(target.value))
            {
                IMPORT_CALLS.with(|calls| {
                    let (captured, current, snapshots) = calls.get();
                    calls.set((captured + 1, current, snapshots));
                });
                Ok(Some(target.value))
            } else {
                Ok(None)
            }
        }
        fn snapshot(&self, target: &mut Self::Target) -> Result<Self::Snapshot, Box<dyn Error>> {
            IMPORT_CALLS.with(|calls| {
                let (captured, current, snapshots) = calls.get();
                calls.set((captured, current, snapshots + 1));
            });
            if self
                .targets
                .as_ref()
                .is_some_and(|targets| targets.residency.is_some())
            {
                target
                    .resident_snapshots
                    .entry(target.value)
                    .or_insert_with(|| Box::new(target.value));
            }
            target.snapshot()
        }
        fn candidate_replay_certificate(
            &self,
            snapshot: &Self::Snapshot,
        ) -> Result<Option<[u8; 32]>, Box<dyn Error>> {
            Ok(CERTIFIED_IMPORT.with(|enabled| enabled.get().then_some([*snapshot; 32])))
        }
        fn current_replay_certificate(
            &self,
            target: &Self::Target,
        ) -> Result<Option<[u8; 32]>, Box<dyn Error>> {
            IMPORT_CALLS.with(|calls| {
                let (captured, current, snapshots) = calls.get();
                calls.set((captured, current + 1, snapshots));
            });
            Ok(CERTIFIED_IMPORT.with(|enabled| {
                (enabled.get()
                    && !OMIT_TEST_PREFIX_CERTIFICATE.with(|omit| omit.get() && target.value == 3))
                .then_some([target.value; 32])
            }))
        }
        fn action_cost_fn(&self) -> fn(&Self::Action) -> u64 {
            test_action_cost
        }

        fn snapshot_memory_charge(_snapshot: &Self::Snapshot) -> usize {
            1
        }
    }

    impl Evaluation for TestWorkload {
        fn execution_disposition(&self, _target: &Self::Target) -> ExecutionDisposition {
            ExecutionDisposition::Runnable
        }

        fn objective_reached(
            &self,
            _run: &Self::Run,
            target: &Self::Target,
        ) -> Result<bool, Box<dyn Error>> {
            let after_work = target._counted.as_ref().is_some_and(|count| {
                let threshold = count
                    .0
                    .objective_after_work
                    .load(std::sync::atomic::Ordering::SeqCst);
                threshold != 0 && target.execution_work >= threshold
            });
            Ok((self.bootstrap_objective && target.execution_work == 0) || after_work)
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
        fn merge_origin_evidence(
            &self,
            evidence: &mut Self::Evidence,
            source: &Self::ArchiveReport,
        ) {
            *evidence = source.marker;
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

    struct TestRollout<'a> {
        target: &'a mut TestTarget,
        initial_terminal: bool,
        terminal_after: Option<u8>,
        objective_after: Option<u8>,
        failed_after: Option<u8>,
        fail_apply: bool,
        fail_probe: bool,
        probe_calls: Vec<u8>,
    }

    impl<'a> TestRollout<'a> {
        fn new(target: &'a mut TestTarget) -> Self {
            Self {
                target,
                initial_terminal: false,
                terminal_after: None,
                objective_after: None,
                failed_after: None,
                fail_apply: false,
                fail_probe: false,
                probe_calls: Vec::new(),
            }
        }
    }

    impl Rollout<TestWorkload> for TestRollout<'_> {
        fn apply(
            &mut self,
            action: &TestAction,
            _milestones: &mut (),
        ) -> Result<(), Box<dyn Error>> {
            if self.fail_apply {
                return Err("test apply failed".into());
            }
            self.target.apply(action);
            Ok(())
        }

        fn observations(&self) -> Vec<()> {
            vec![()]
        }

        fn outcome(&self) -> Result<Outcome, Box<dyn Error>> {
            if self.initial_terminal && self.target.execution_work == 0 {
                return Ok(Outcome {
                    disposition: ExecutionDisposition::Terminal,
                    ..Outcome::default()
                });
            }
            if self
                .failed_after
                .is_some_and(|limit| self.target.value >= limit)
            {
                return Ok(Outcome {
                    disposition: ExecutionDisposition::Failed,
                    ..Outcome::default()
                });
            }
            let objective_reached = self
                .objective_after
                .is_some_and(|limit| self.target.value >= limit);
            let disposition = if self
                .terminal_after
                .is_some_and(|limit| self.target.value >= limit)
            {
                ExecutionDisposition::Terminal
            } else {
                ExecutionDisposition::default()
            };
            Ok(Outcome {
                objective_reached,
                disposition,
            })
        }

        fn snapshot(&mut self) -> Result<u8, Box<dyn Error>> {
            self.target.snapshot()
        }

        fn key(&self) -> Result<TestKey, Box<dyn Error>> {
            Ok(TestKey(self.target.value))
        }

        fn probe(&mut self, snapshot: &u8) -> Result<bool, Box<dyn Error>> {
            if self.fail_probe {
                return Err("test probe failed".into());
            }
            self.probe_calls.push(*snapshot);
            self.target.value = snapshot.wrapping_add(1);
            self.target.value = *snapshot;
            Ok(true)
        }
    }

    #[test]
    fn bootstrap_session_scope_finalizes_success_error_and_unwind_once() {
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        for fail in [false, true] {
            FINALIZATION.with(|state| state.set((0, fail)));
            let result = with_coordinator_session(&workload, || Ok(7));
            assert_eq!(result.is_err(), fail);
            assert_eq!(FINALIZATION.with(|state| state.get().0), 1);
            FINALIZATION.with(|state| state.set((0, fail)));
            let result: Result<(), Box<dyn Error>> =
                with_coordinator_session(&workload, || Err(Box::new(TestIntegrityViolation)));
            assert!(
                result
                    .unwrap_err()
                    .downcast_ref::<TestIntegrityViolation>()
                    .is_some()
            );
            assert_eq!(FINALIZATION.with(|state| state.get().0), 1);
            FINALIZATION.with(|state| state.set((0, fail)));
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _: Result<(), Box<dyn Error>> =
                    with_coordinator_session(&workload, || panic!("bootstrap unwind"));
            }));
            assert!(result.is_err());
            assert_eq!(FINALIZATION.with(|state| state.get().0), 1);
        }
        FINALIZATION.with(|state| state.set((0, false)));
    }

    #[test]
    fn bootstrap_primary_error_survives_failed_secondary_diagnostic_write() {
        struct Broken {
            attempts: usize,
        }
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                self.attempts += 1;
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut diagnostics = Broken { attempts: 0 };
        let result: Result<(), Box<dyn Error>> = coordinator_session_result(
            Err(Box::new(TestIntegrityViolation)),
            Err("shutdown unconfirmed".into()),
            &mut diagnostics,
        );
        assert!(
            result
                .unwrap_err()
                .downcast_ref::<TestIntegrityViolation>()
                .is_some()
        );
        assert_eq!(diagnostics.attempts, 1);
    }

    #[test]
    fn campaign_finalizes_bootstrap_before_worker_readiness_and_refuses_unconfirmed_exit() {
        let mut config = three_worker_config();
        config.workers = 1;
        config.window = 1;
        config.execution_budget = 0;
        for fail in [false, true] {
            let targets = Arc::new(LiveTargets {
                bootstrap_readiness: Some(BootstrapReadiness {
                    coordinator: std::thread::current().id(),
                    finalized: std::sync::atomic::AtomicBool::new(false),
                    worker_starts: std::sync::atomic::AtomicUsize::new(0),
                }),
                ..LiveTargets::default()
            });
            let workload = TestWorkload {
                bootstrap_objective: false,
                targets: Some(targets.clone()),
            };
            FINALIZATION.with(|state| state.set((0, fail)));
            let result = run_campaign_checkpointed(
                &workload,
                &config,
                &CampaignOrigin::Genesis,
                &mut Vec::new(),
                None,
            );
            assert_eq!(result.is_err(), fail);
            if fail {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("shutdown unconfirmed")
                );
            }
            assert_eq!(FINALIZATION.with(|state| state.get().0), 1);
            let readiness = targets.bootstrap_readiness.as_ref().unwrap();
            assert_eq!(
                readiness
                    .worker_starts
                    .load(std::sync::atomic::Ordering::SeqCst),
                usize::from(!fail)
            );
            assert_eq!(
                readiness
                    .finalized
                    .load(std::sync::atomic::Ordering::SeqCst),
                !fail
            );
        }
        FINALIZATION.with(|state| state.set((0, false)));
    }

    fn test_core() -> (TestWorkload, (), CoordinatorCore<TestWorkload>, TestTarget) {
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let run = ();
        let mut core = CoordinatorCore::new(&workload, &run, 1_024, None);
        let mut target = TestTarget::default();
        core.bootstrap(&workload, &run, &mut target)
            .expect("bootstrap generic core");
        (workload, run, core, target)
    }

    #[test]
    fn default_suffix_prefix_preserves_full_expansion_side_effects() {
        let targets = Arc::new(LiveTargets::default());
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: Some(Arc::clone(&targets)),
        };
        let state = DrawTables::new(workload.draw_table_parameters(&())).unwrap();
        let mixture = MixtureDraw {
            mixture: DrawMixture::AlphabetOnly,
            weight: 0,
            splice_weight: 0,
        };
        for limit in [0, 1, 2] {
            let suffix = workload
                .expand_suffix_prefix(&(), &state, mixture, 1 << 32, None, limit)
                .unwrap();
            assert_eq!(suffix.len(), usize::from(limit));
        }
        assert_eq!(
            targets.selections.load(std::sync::atomic::Ordering::SeqCst),
            3
        );
    }

    #[test]
    fn rollout_stops_on_an_already_terminal_parent() {
        let mut target = TestTarget::default();
        let mut rollout = TestRollout::new(&mut target);
        rollout.initial_terminal = true;
        let result = execute_suffix(
            &mut rollout,
            (),
            &[TestAction::new(1, 1)],
            RetentionPolicy::Unprobed,
            false,
        )
        .expect("terminal parent is a valid no-op rollout");
        assert!(result.actions.is_empty());
        assert!(result.preparation_failure.is_none());
        assert_eq!(rollout.target.value, 0);

        let mut target = TestTarget::default();
        let mut failed = TestRollout::new(&mut target);
        failed.failed_after = Some(0);
        let result = execute_suffix(
            &mut failed,
            (),
            &[TestAction::new(1, 1)],
            RetentionPolicy::Unprobed,
            false,
        )
        .expect("failed parent is a reportable preparation failure");
        assert!(result.actions.is_empty());
        assert_eq!(result.preparation_failure, Some(vec![()]));
    }

    #[test]
    fn rollout_runs_every_suffix_action_and_restores_after_each_probe() {
        let mut target = TestTarget::default();
        let mut rollout = TestRollout::new(&mut target);
        let result = execute_suffix(
            &mut rollout,
            (),
            &[
                TestAction::new(1, 1),
                TestAction::new(2, 1),
                TestAction::new(4, 1),
            ],
            RetentionPolicy::ProbeAtAdmission,
            false,
        )
        .expect("rollout");
        assert_eq!(result.actions.len(), 3);
        assert_eq!(
            result.actions[0].candidate.as_ref().map(|c| c.key),
            Some(TestKey(1))
        );
        assert_eq!(
            result.actions[1].candidate.as_ref().map(|c| c.key),
            Some(TestKey(3))
        );
        assert_eq!(
            result.actions[2].candidate.as_ref().map(|c| c.key),
            Some(TestKey(7))
        );
        assert_eq!(rollout.probe_calls, vec![1, 3, 7]);
        assert_eq!(rollout.target.value, 7, "probe restores the live target");
    }

    #[test]
    fn rollout_stops_on_terminal_actions_and_propagates_failures() {
        let mut target = TestTarget::default();
        let mut rollout = TestRollout::new(&mut target);
        rollout.terminal_after = Some(2);
        rollout.objective_after = Some(2);
        let result = execute_suffix(
            &mut rollout,
            (),
            &[
                TestAction::new(1, 1),
                TestAction::new(1, 1),
                TestAction::new(1, 1),
            ],
            RetentionPolicy::Unprobed,
            false,
        )
        .expect("terminal action rollout");
        assert_eq!(result.actions.len(), 2);
        assert!(result.actions[1].outcome.objective_reached);
        assert!(result.actions[1].candidate.is_none());

        let mut target = TestTarget::default();
        let mut apply_failure = TestRollout::new(&mut target);
        apply_failure.fail_apply = true;
        assert!(
            execute_suffix(
                &mut apply_failure,
                (),
                &[TestAction::new(1, 1)],
                RetentionPolicy::Unprobed,
                false,
            )
            .is_err()
        );

        let mut target = TestTarget::default();
        let mut probe_failure = TestRollout::new(&mut target);
        probe_failure.fail_probe = true;
        assert!(
            execute_suffix(
                &mut probe_failure,
                (),
                &[TestAction::new(1, 1)],
                RetentionPolicy::ProbeAtAdmission,
                false,
            )
            .is_err()
        );
    }

    #[test]
    fn runnable_objectives_keep_candidates_and_stop_only_the_current_suffix() {
        let mut target = TestTarget::default();
        let mut rollout = TestRollout::new(&mut target);
        rollout.objective_after = Some(1);
        let result = execute_suffix(
            &mut rollout,
            (),
            &[TestAction::new(1, 1), TestAction::new(1, 1)],
            RetentionPolicy::Unprobed,
            true,
        )
        .expect("runnable objective rollout");
        assert_eq!(result.actions.len(), 1);
        assert!(result.actions[0].outcome.objective_reached);
        assert_eq!(
            result.actions[0].outcome.disposition,
            ExecutionDisposition::Runnable
        );
        assert_eq!(
            result.actions[0].candidate.as_ref().map(|c| c.key),
            Some(TestKey(1))
        );

        let mut target = TestTarget {
            value: 1,
            ..TestTarget::default()
        };
        let mut continuation = TestRollout::new(&mut target);
        continuation.objective_after = Some(1);
        let result = execute_suffix(
            &mut continuation,
            (),
            &[TestAction::new(1, 1)],
            RetentionPolicy::Unprobed,
            true,
        )
        .expect("continuation after a latched objective");
        assert_eq!(result.actions.len(), 1);
        assert!(!result.actions[0].outcome.objective_reached);
        assert_eq!(
            result.actions[0].candidate.as_ref().map(|c| c.key),
            Some(TestKey(2))
        );
    }

    #[test]
    fn terminal_and_failed_outcomes_are_distinct_and_have_no_candidates() {
        let mut target = TestTarget::default();
        let mut terminal = TestRollout::new(&mut target);
        terminal.terminal_after = Some(1);
        let result = execute_suffix(
            &mut terminal,
            (),
            &[TestAction::new(1, 1), TestAction::new(1, 1)],
            RetentionPolicy::Unprobed,
            false,
        )
        .expect("terminal outcome");
        assert_eq!(result.actions.len(), 1);
        assert!(!result.actions[0].outcome.objective_reached);
        assert_eq!(
            result.actions[0].outcome.disposition,
            ExecutionDisposition::Terminal
        );
        assert!(result.actions[0].candidate.is_none());

        let mut target = TestTarget::default();
        let mut failed = TestRollout::new(&mut target);
        failed.failed_after = Some(1);
        let result = execute_suffix(
            &mut failed,
            (),
            &[TestAction::new(1, 1), TestAction::new(1, 1)],
            RetentionPolicy::Unprobed,
            false,
        )
        .expect("failed outcome");
        assert_eq!(result.actions.len(), 1);
        assert_eq!(
            result.actions[0].outcome.disposition,
            ExecutionDisposition::Failed
        );
        assert_eq!(result.actions[0].observations, vec![()]);
        assert!(result.actions[0].candidate.is_none());
    }

    #[test]
    fn coordinator_counts_terminal_objectives_and_execution_failures_separately() {
        let (workload, _run, mut core, _target) = test_core();
        let terminal_objective = TestAction::new(1, 1);
        core.admit_job(
            &workload,
            0,
            CampaignJobResult::<TestWorkload> {
                preparation_failure: None,
                actions: vec![CampaignActionResult::<TestWorkload> {
                    action: terminal_objective,
                    observations: Vec::new(),
                    milestones: (),
                    outcome: Outcome {
                        objective_reached: true,
                        disposition: ExecutionDisposition::Terminal,
                    },
                    candidate: None,
                }],
            },
        )
        .expect("admit terminal objective");
        assert_eq!(core.terminal_endpoints, 1);
        assert_eq!(core.terminal_objectives, 1);
        assert_eq!(core.execution_failures, 0);

        let failed = TestAction::new(2, 1);
        core.admit_job(
            &workload,
            0,
            CampaignJobResult::<TestWorkload> {
                preparation_failure: None,
                actions: vec![CampaignActionResult::<TestWorkload> {
                    action: failed,
                    observations: vec![()],
                    milestones: (),
                    outcome: Outcome {
                        objective_reached: false,
                        disposition: ExecutionDisposition::Failed,
                    },
                    candidate: None,
                }],
            },
        )
        .expect("admit failed execution");
        assert_eq!(core.terminal_endpoints, 1);
        assert_eq!(core.terminal_objectives, 1);
        assert_eq!(core.execution_failures, 1);
    }

    #[test]
    fn coordinator_admits_one_preparation_failure_without_actions() {
        let (workload, _run, mut core, _target) = test_core();
        let (sequence, decisions) = core
            .admit_job(
                &workload,
                0,
                CampaignJobResult::<TestWorkload> {
                    preparation_failure: Some(vec![()]),
                    actions: Vec::new(),
                },
            )
            .expect("admit preparation failure");
        assert_eq!(sequence, 1);
        assert!(decisions.is_empty());
        assert_eq!(core.execution_failures, 1);
        assert_eq!(core.archive.retained, 1);

        let action = TestAction::new(1, 1);
        let error = core
            .admit_job(
                &workload,
                0,
                CampaignJobResult::<TestWorkload> {
                    preparation_failure: Some(vec![()]),
                    actions: vec![CampaignActionResult::<TestWorkload> {
                        action,
                        observations: Vec::new(),
                        milestones: (),
                        outcome: Outcome::default(),
                        candidate: None,
                    }],
                },
            )
            .expect_err("preparation failure cannot fabricate actions");
        assert_eq!(
            error.to_string(),
            "preparation failure result also carries actions"
        );
        assert_eq!(core.execution_failures, 1);
    }

    #[test]
    fn bootstrap_records_an_objective_at_the_empty_input() {
        let workload = TestWorkload {
            bootstrap_objective: true,
            targets: None,
        };
        let run = ();
        let mut core = CoordinatorCore::new(&workload, &run, 1_024, None);
        let mut target = TestTarget::default();
        core.bootstrap(&workload, &run, &mut target)
            .expect("bootstrap root");
        assert_eq!(core.objectives_reached, 1);
        assert_eq!(core.objective_witness, Some(Input::default()));
    }

    #[test]
    fn bootstrap_objective_stops_reservation_and_replays_with_both_stop_policies() {
        for (stop_rollout_on_objective, stop_campaign_on_objective) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let witness_path = std::env::temp_dir().join(format!(
                "harmony-bootstrap-objective-{}-{stop_rollout_on_objective}-{stop_campaign_on_objective}.json",
                std::process::id()
            ));
            let config = CampaignConfig {
                campaign_seed: 7,
                workers: 1,
                execution_budget: 1,
                host: "test".to_owned(),
                wall_budget: None,
                stop_rollout_on_objective,
                stop_campaign_on_objective,
                archive_entry_limit: 16,
                window: 1,
                memory_budget_mib: None,
                materialize_final_artifacts: true,
                run: (),
                mixture: DrawMixture::AlphabetOnly,
                retention: RetentionPolicy::Unprobed,
                objective_witness_path: Some(witness_path.clone()),
            };
            let workload = TestWorkload {
                bootstrap_objective: true,
                targets: None,
            };
            let mut stream = Vec::new();
            let live = run_campaign_checkpointed(
                &workload,
                &config,
                &CampaignOrigin::Genesis,
                &mut stream,
                None,
            )
            .expect("bootstrap objective campaign");
            assert_eq!(live.0.objectives_reached, 1);
            assert_eq!(live.0.objective_witness, Some(Input::default()));
            assert_eq!(
                live.0.executions_completed,
                if stop_campaign_on_objective { 0 } else { 1 }
            );
            assert_eq!(live.0.stop_rollout_on_objective, stop_rollout_on_objective);
            assert_eq!(
                live.0.stop_campaign_on_objective,
                stop_campaign_on_objective
            );
            assert_eq!(
                std::fs::read(&witness_path).expect("bootstrap witness file"),
                serde_json::to_vec_pretty(&Input::<TestAction>::default())
                    .expect("empty witness encodes")
            );

            let replayed = replay_campaign_checkpointed(&workload, &stream, None, None)
                .expect("bootstrap objective replay");
            assert_eq!(replayed, live);
            std::fs::remove_file(witness_path).expect("remove bootstrap witness file");
        }
    }

    #[test]
    fn replay_rejects_work_after_a_bootstrap_objective_stop() {
        let config = CampaignConfig {
            campaign_seed: 7,
            workers: 1,
            execution_budget: 1,
            host: "test".to_owned(),
            wall_budget: None,
            stop_rollout_on_objective: true,
            stop_campaign_on_objective: false,
            archive_entry_limit: 16,
            window: 1,
            memory_budget_mib: None,
            materialize_final_artifacts: true,
            run: (),
            mixture: DrawMixture::AlphabetOnly,
            retention: RetentionPolicy::Unprobed,
            objective_witness_path: None,
        };
        let workload = TestWorkload {
            bootstrap_objective: true,
            targets: None,
        };
        let mut stream = Vec::new();
        let (live, _) = run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .expect("live campaign");
        assert_eq!(live.executions_completed, 1);
        let mut lines = std::str::from_utf8(&stream)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut header: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        header["stop_campaign_on_objective"] = serde_json::json!(true);
        lines[0] = serde_json::to_string(&header).unwrap();
        let error =
            replay_campaign_checkpointed(&workload, lines.join("\n").as_bytes(), None, None)
                .expect_err(
                    "replay certified a campaign that continued after its bootstrap objective",
                )
                .to_string();
        assert!(error.contains("after its objective stop"), "{error}");
    }

    #[test]
    fn replay_rejects_new_reservations_after_an_admitted_objective_stop() {
        let targets = Arc::new(LiveTargets::default());
        targets
            .objective_after_work
            .store(1, std::sync::atomic::Ordering::SeqCst);
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: Some(targets),
        };
        let mut config = CampaignConfig {
            campaign_seed: 7,
            workers: 1,
            execution_budget: 5,
            host: "test".to_owned(),
            wall_budget: None,
            stop_rollout_on_objective: true,
            stop_campaign_on_objective: false,
            archive_entry_limit: 16,
            window: 2,
            memory_budget_mib: None,
            materialize_final_artifacts: true,
            run: (),
            mixture: DrawMixture::AlphabetOnly,
            retention: RetentionPolicy::Unprobed,
            objective_witness_path: None,
        };
        let mut stream = Vec::new();
        let (live, _) = run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .expect("live campaign");
        assert_eq!(live.executions_completed, 5);
        assert_eq!(live.bootstrap_execution_work, 0);
        assert!(live.objectives_reached > 0);
        config.stop_campaign_on_objective = true;
        let mut stopped_stream = Vec::new();
        let (stopped, _) = run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stopped_stream,
            None,
        )
        .expect("live objective-stop campaign");
        assert_eq!(stopped.executions_completed, 2);
        let (replayed_stopped, _) =
            replay_campaign_checkpointed(&workload, &stopped_stream, None, None)
                .expect("in-flight job remains replayable after an objective stop");
        assert_eq!(replayed_stopped.executions_completed, 2);
        let mut lines = std::str::from_utf8(&stream)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut header: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        header["stop_campaign_on_objective"] = serde_json::json!(true);
        lines[0] = serde_json::to_string(&header).unwrap();
        let error =
            replay_campaign_checkpointed(&workload, lines.join("\n").as_bytes(), None, None)
                .expect_err("replay certified reservations made after an objective")
                .to_string();
        assert!(error.contains("after its objective stop"), "{error}");
    }

    fn five_job_campaign_stream() -> (TestWorkload, Vec<String>) {
        let config = CampaignConfig {
            campaign_seed: 7,
            workers: 1,
            execution_budget: 5,
            host: "test".to_owned(),
            wall_budget: None,
            stop_rollout_on_objective: true,
            stop_campaign_on_objective: false,
            archive_entry_limit: 16,
            window: 2,
            memory_budget_mib: None,
            materialize_final_artifacts: true,
            run: (),
            mixture: DrawMixture::AlphabetOnly,
            retention: RetentionPolicy::Unprobed,
            objective_witness_path: None,
        };
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut stream = Vec::new();
        let (live, _) = run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .expect("live campaign");
        assert_eq!(live.executions_completed, 5);
        let lines = std::str::from_utf8(&stream)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        (workload, lines)
    }

    #[test]
    fn standalone_full_and_prefix_replay_finalize_coordinator_sessions_on_all_results() {
        let (workload, lines) = five_job_campaign_stream();
        for prefix in [false, true] {
            let stream = if prefix {
                lines[..lines.len() - 1].join("\n")
            } else {
                lines.join("\n")
            };
            for fail in [false, true] {
                FINALIZATION.with(|state| state.set((0, fail)));
                let result = if prefix {
                    replay_campaign_prefix_checkpointed(&workload, stream.as_bytes(), None, None)
                } else {
                    replay_campaign_checkpointed(&workload, stream.as_bytes(), None, None)
                };
                assert_eq!(
                    result.is_err(),
                    fail,
                    "prefix={prefix} fail={fail}: {:?}",
                    result.as_ref().err().map(ToString::to_string)
                );
                if fail {
                    assert!(
                        result
                            .unwrap_err()
                            .to_string()
                            .contains("shutdown unconfirmed")
                    );
                }
                assert_eq!(FINALIZATION.with(|state| state.get().0), 1);
            }
            FINALIZATION.with(|state| state.set((0, false)));
            let refusing = TestWorkload {
                bootstrap_objective: false,
                targets: Some(Arc::new(LiveTargets {
                    refuse: true,
                    ..LiveTargets::default()
                })),
            };
            let error = if prefix {
                replay_campaign_prefix_checkpointed(&refusing, stream.as_bytes(), None, None)
            } else {
                replay_campaign_checkpointed(&refusing, stream.as_bytes(), None, None)
            }
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("failed to build the replay target")
            );
            assert_eq!(FINALIZATION.with(|state| state.get().0), 1);
            FINALIZATION.with(|state| state.set((0, false)));
            let mut incomplete = lines.clone();
            incomplete.pop();
            let result = replay_campaign_checkpointed(
                &workload,
                incomplete.join("\n").as_bytes(),
                None,
                None,
            );
            assert!(result.is_err());
            assert_eq!(FINALIZATION.with(|state| state.get().0), 1);
        }
        FINALIZATION.with(|state| state.set((0, false)));
    }

    #[test]
    fn replay_rejects_forged_execution_provenance_labels() {
        let (workload, original_lines) = five_job_campaign_stream();
        for (field, forged) in [
            ("executor_mode", "not_snapshot_resume_archive"),
            ("selection_seed_derivation", "not_sha256"),
        ] {
            let mut lines = original_lines.clone();
            let mut header: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
            header[field] = serde_json::json!(forged);
            lines[0] = serde_json::to_string(&header).unwrap();
            let result =
                replay_campaign_checkpointed(&workload, lines.join("\n").as_bytes(), None, None);
            assert!(
                result.is_err(),
                "replay certified forged {field}: {:?}",
                result.unwrap().0
            );
        }
    }

    #[test]
    fn replay_rejects_source_provenance_on_a_genesis_campaign() {
        let (workload, original_lines) = five_job_campaign_stream();
        for field in [
            "origin_path",
            "origin_archive_sha256",
            "origin_checkpoint_path",
            "origin_checkpoint_sha256",
        ] {
            let mut lines = original_lines.clone();
            let mut header: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
            header[field] = serde_json::json!("forged-source");
            lines[0] = serde_json::to_string(&header).unwrap();
            let error =
                replay_campaign_checkpointed(&workload, lines.join("\n").as_bytes(), None, None)
                    .expect_err("genesis replay copied a forged source into its report")
                    .to_string();
            assert!(error.contains("origin provenance"), "{field}: {error}");
        }
    }

    #[test]
    fn replay_rejects_archive_checkpoint_provenance_it_did_not_verify() {
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut config = three_worker_config();
        config.execution_budget = 5;
        config.window = 2;
        let mut source_stream = Vec::new();
        let (source, snapshots) = run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut source_stream,
            None,
        )
        .expect("source campaign");
        let source_archive = source.archive;
        let source_archive =
            ArchiveSource::from_bytes(&serde_json::to_vec(&source_archive).unwrap()).unwrap();
        let source_checkpoint = CampaignCheckpoint {
            path: "source.snapshots".to_owned(),
            file_sha256: format!("{:x}", Sha256::digest(snapshots.to_bytes().unwrap())),
            snapshots,
        };
        config.execution_budget = 2;
        for checkpoint in [None, Some(source_checkpoint)] {
            let mut archive_stream = Vec::new();
            let archive_origin = CampaignOrigin::Archive {
                path: "source.archive".to_owned(),
                source: source_archive.clone(),
                checkpoint: checkpoint.clone(),
            };
            run_campaign_checkpointed(
                &workload,
                &config,
                &archive_origin,
                &mut archive_stream,
                None,
            )
            .expect("archive campaign");
            replay_campaign_checkpointed(
                &workload,
                &archive_stream,
                Some(&source_archive),
                checkpoint.as_ref(),
            )
            .expect("untouched archive replay");
            if let Some(checkpoint) = &checkpoint {
                let mut changed = checkpoint.clone();
                changed.snapshots.entries.push(SnapshotCheckpointEntry {
                    id: u64::MAX,
                    snapshot: 0,
                });
                let replay = replay_campaign_checkpointed(
                    &workload,
                    &archive_stream,
                    Some(&source_archive),
                    Some(&changed),
                );
                assert!(
                    replay.is_err(),
                    "archive replay certified checkpoint bytes unrelated to its SHA-256"
                );
                let mut changed_stream = Vec::new();
                let changed_origin = CampaignOrigin::Archive {
                    path: "source.archive".to_owned(),
                    source: source_archive.clone(),
                    checkpoint: Some(changed),
                };
                assert!(
                    run_campaign_checkpointed(
                        &workload,
                        &config,
                        &changed_origin,
                        &mut changed_stream,
                        None,
                    )
                    .is_err(),
                    "live archive campaign recorded a checkpoint SHA unrelated to its bytes"
                );
            }
            let mut lines = std::str::from_utf8(&archive_stream)
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let mut header: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
            header["origin_checkpoint_path"] = serde_json::json!("forged.snapshots");
            if checkpoint.is_none() {
                header["origin_checkpoint_sha256"] = serde_json::json!("forged-sha256");
            }
            lines[0] = serde_json::to_string(&header).unwrap();
            let result = replay_campaign_checkpointed(
                &workload,
                lines.join("\n").as_bytes(),
                Some(&source_archive),
                checkpoint.as_ref(),
            );
            assert!(
                result.is_err(),
                "archive replay certified forged checkpoint provenance: {:?}",
                result.unwrap().0.origin
            );
        }
    }

    #[test]
    fn archive_checkpoint_cannot_assign_another_sources_state_to_an_input() {
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut config = three_worker_config();
        config.workers = 1;
        config.window = 1;
        config.execution_budget = 1;
        let mut sources = Vec::<(u8, TestArchiveReport, SnapshotCheckpoint<u8>)>::new();
        for seed in 1..=32 {
            config.campaign_seed = seed;
            let (campaign, snapshots) = run_campaign_checkpointed(
                &workload,
                &config,
                &CampaignOrigin::Genesis,
                &mut Vec::new(),
                None,
            )
            .expect("independent source campaign");
            let Some(saved) = snapshots.entries.iter().find(|entry| entry.id == 1) else {
                continue;
            };
            let value = saved.snapshot;
            if value == 0
                || value == INHERITS_PARENT_PLACE
                || sources.iter().any(|(existing, _, _)| *existing == value)
            {
                continue;
            }
            let entry = campaign
                .archive
                .entries
                .iter()
                .find(|entry| entry.id == 1)
                .expect("snapshot belongs to an archive entry");
            assert_eq!(entry.key, TestKey(value));
            assert_eq!(
                entry
                    .input
                    .actions
                    .iter()
                    .fold(0_u8, |sum, action| sum.wrapping_add(action.input)),
                value
            );
            sources.push((value, campaign.archive, snapshots));
            if sources.len() == 2 {
                break;
            }
        }
        assert_eq!(sources.len(), 2, "two distinct saved states are required");
        let (source_value, source_report, source_snapshots) = sources.remove(0);
        let (foreign_value, _, foreign_snapshots) = sources.remove(0);
        let source = ArchiveSource::from_bytes(&serde_json::to_vec(&source_report).unwrap())
            .expect("archive source");
        let checkpoint_for = |path: &str, snapshots: SnapshotCheckpoint<u8>| {
            let bytes = snapshots.to_bytes().expect("encode checkpoint");
            CampaignCheckpoint {
                path: path.to_owned(),
                file_sha256: format!("{:x}", Sha256::digest(&bytes)),
                snapshots: SnapshotCheckpoint::from_bytes(&bytes, workload.checkpoint_format())
                    .expect("decode checkpoint"),
            }
        };
        config.campaign_seed = 101;
        config.execution_budget = 0;
        let matched_checkpoint = checkpoint_for("source.snapshots", source_snapshots.clone());
        let mut matched_stream = Vec::new();
        let (matched_report, _) = run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Archive {
                path: "source.archive".to_owned(),
                source: source.clone(),
                checkpoint: Some(matched_checkpoint.clone()),
            },
            &mut matched_stream,
            None,
        )
        .expect("matching serialized source and checkpoint import");
        assert_eq!(
            replay_campaign_checkpointed(
                &workload,
                &matched_stream,
                Some(&source),
                Some(&matched_checkpoint),
            )
            .expect("matching serialized source and checkpoint replay")
            .0
            .archive,
            matched_report.archive
        );
        let checkpoint = checkpoint_for("foreign.snapshots", foreign_snapshots);
        let mut stream = Vec::new();
        let mixed = run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Archive {
                path: "source.archive".to_owned(),
                source: source.clone(),
                checkpoint: Some(checkpoint.clone()),
            },
            &mut stream,
            None,
        );
        assert_ne!(source_value, foreign_value);
        assert!(
            mixed
                .unwrap_err()
                .to_string()
                .contains("source entry digest")
        );
        let mut changed = source_snapshots.clone();
        changed
            .entries
            .iter_mut()
            .find(|entry| entry.id == 1)
            .expect("saved source entry")
            .snapshot = foreign_value;
        let changed_checkpoint = checkpoint_for("changed.snapshots", changed);
        let mut changed_stream = Vec::new();
        let changed_result = run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Archive {
                path: "source.archive".to_owned(),
                source: source.clone(),
                checkpoint: Some(changed_checkpoint.clone()),
            },
            &mut changed_stream,
            None,
        );
        assert!(
            changed_result
                .unwrap_err()
                .to_string()
                .contains("source entry digest")
        );
        let mut lines = matched_stream
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| String::from_utf8(line.to_vec()).unwrap())
            .collect::<Vec<_>>();
        let mut header: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        header["origin_checkpoint_path"] = serde_json::json!(changed_checkpoint.path);
        header["origin_checkpoint_sha256"] = serde_json::json!(changed_checkpoint.file_sha256);
        lines[0] = serde_json::to_string(&header).unwrap();
        let replay_result = replay_campaign_checkpointed(
            &workload,
            lines.join("\n").as_bytes(),
            Some(&source),
            Some(&changed_checkpoint),
        );
        assert!(
            replay_result
                .unwrap_err()
                .to_string()
                .contains("source entry digest")
        );
        let reject_entries = |report: TestArchiveReport,
                              snapshots: SnapshotCheckpoint<u8>,
                              expected: &str| {
            let source = ArchiveSource::from_bytes(&serde_json::to_vec(&report).unwrap()).unwrap();
            let checkpoint = checkpoint_for("invalid.snapshots", snapshots);
            let result = run_campaign_checkpointed(
                &workload,
                &config,
                &CampaignOrigin::Archive {
                    path: "source.archive".to_owned(),
                    source,
                    checkpoint: Some(checkpoint),
                },
                &mut Vec::new(),
                None,
            );
            assert!(
                result.unwrap_err().to_string().contains(expected),
                "expected {expected}"
            );
        };
        let mut duplicate = source_snapshots.clone();
        duplicate.entries.push(
            duplicate
                .entries
                .iter()
                .find(|entry| entry.id == 1)
                .unwrap()
                .clone(),
        );
        assert!(
            duplicate
                .to_bytes()
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
        assert!(
            super::validate_archive_checkpoint_entries(&workload, &source_report, &duplicate)
                .unwrap_err()
                .to_string()
                .contains("duplicate entry IDs")
        );
        let mut unknown = source_snapshots.clone();
        unknown
            .entries
            .iter_mut()
            .find(|entry| entry.id == 1)
            .unwrap()
            .id = u64::MAX;
        reject_entries(source_report.clone(), unknown, "unknown entry ID");
        let mut unbound = source_report;
        unbound
            .entries
            .iter_mut()
            .find(|entry| entry.id == 1)
            .unwrap()
            .snapshot_identity_sha256 = None;
        reject_entries(unbound, source_snapshots, "lacks a snapshot digest");
    }

    #[test]
    fn replay_rejects_more_reservations_than_its_execution_budget() {
        let (workload, mut lines) = five_job_campaign_stream();
        let mut header: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        header["execution_budget"] = serde_json::json!(1);
        lines[0] = serde_json::to_string(&header).unwrap();
        let error =
            replay_campaign_checkpointed(&workload, lines.join("\n").as_bytes(), None, None)
                .expect_err("replay certified five jobs under a one-job budget")
                .to_string();
        assert!(error.contains("recorded execution budget"), "{error}");
    }

    #[test]
    fn replay_rejects_reservations_after_its_work_budget() {
        let (workload, mut lines) = five_job_campaign_stream();
        let first_job_work = lines
            .iter()
            .find_map(|line| {
                let value: serde_json::Value = serde_json::from_str(line).unwrap();
                (value["event"] == "job").then(|| value["execution_work"].as_u64().unwrap())
            })
            .unwrap();
        assert!(first_job_work > 0);
        let mut header: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        header["work_budget"] = serde_json::json!(first_job_work);
        lines[0] = serde_json::to_string(&header).unwrap();
        let error =
            replay_campaign_checkpointed(&workload, lines.join("\n").as_bytes(), None, None)
                .expect_err("replay certified five jobs under a one-work-unit budget")
                .to_string();
        assert!(error.contains("after its work budget"), "{error}");
    }

    #[test]
    fn replay_accepts_only_the_window_reserved_before_work_budget_exhaustion() {
        let (workload, mut lines) = five_job_campaign_stream();
        let jobs = lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| {
                let value: serde_json::Value = serde_json::from_str(line).unwrap();
                (value["event"] == "job").then_some((index, value))
            })
            .take(2)
            .collect::<Vec<_>>();
        assert_eq!(jobs.len(), 2);
        let first_job_work = jobs[0].1["execution_work"].as_u64().unwrap();
        assert!(first_job_work > 0);
        let mut header: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        header["work_budget"] = serde_json::json!(first_job_work);
        lines[0] = serde_json::to_string(&header).unwrap();
        let second_job = jobs[1].0;
        let skips = lines[1..=second_job]
            .iter()
            .filter(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["event"] == "skip"
            })
            .count();
        let mut end: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
        end["jobs"] = serde_json::json!(2);
        end["skips"] = serde_json::json!(skips);
        end["executions_completed"] = serde_json::json!(2);
        let mut reserved_window = lines[..=second_job].to_vec();
        reserved_window.push(serde_json::to_string(&end).unwrap());
        let (report, _) = replay_campaign_checkpointed(
            &workload,
            reserved_window.join("\n").as_bytes(),
            None,
            None,
        )
        .expect("the two jobs reserved before the work budget was reached may finish");
        assert_eq!(report.executions_completed, 2);
    }

    #[test]
    fn replay_rejects_a_terminal_record_before_required_reservations_finish() {
        let (workload, full_lines) = five_job_campaign_stream();
        for keep_jobs in [1_usize, 3] {
            let mut lines = full_lines.clone();
            let last_job = lines
                .iter()
                .enumerate()
                .filter(|(_, line)| {
                    serde_json::from_str::<serde_json::Value>(line).unwrap()["event"] == "job"
                })
                .nth(keep_jobs - 1)
                .unwrap()
                .0;
            let skips = lines[1..last_job]
                .iter()
                .filter(|line| {
                    serde_json::from_str::<serde_json::Value>(line).unwrap()["event"] == "skip"
                })
                .count();
            let mut end: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
            end["jobs"] = serde_json::json!(keep_jobs);
            end["skips"] = serde_json::json!(skips);
            end["executions_completed"] = serde_json::json!(keep_jobs);
            lines.truncate(last_job + 1);
            let (prefix, _) = replay_campaign_prefix_checkpointed(
                &workload,
                lines.join("\n").as_bytes(),
                None,
                None,
            )
            .expect("intermediate prefix remains replayable");
            assert_eq!(prefix.mode, "campaign-prefix");
            lines.push(serde_json::to_string(&end).unwrap());
            let error =
                replay_campaign_checkpointed(&workload, lines.join("\n").as_bytes(), None, None)
                    .expect_err(
                        "replay certified a completed campaign before required work finished",
                    )
                    .to_string();
            assert!(error.contains("required reservations"), "{error}");
        }
    }

    #[test]
    fn replay_rejects_a_tier_rank_absent_from_the_selection_history() {
        let (workload, mut lines) = five_job_campaign_stream();
        let mut header: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        header["archive_entry_limit"] = serde_json::json!(256);
        lines[0] = serde_json::to_string(&header).unwrap();
        let first_job = lines
            .iter()
            .position(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["event"] == "job"
            })
            .unwrap();
        let mut job: serde_json::Value = serde_json::from_str(&lines[first_job]).unwrap();
        assert_eq!(job["selector"]["tier_rank"], 0);
        job["selector"]["tier_rank"] = serde_json::json!(1);
        lines[first_job] = serde_json::to_string(&job).unwrap();
        let error =
            replay_campaign_checkpointed(&workload, lines.join("\n").as_bytes(), None, None)
                .expect_err("replay certified a nonexistent tier in a one-tier campaign")
                .to_string();
        assert!(error.contains("observed progress tiers"), "{error}");
    }

    #[test]
    fn replay_rejects_a_selector_preference_outside_the_key_portfolio() {
        let (workload, mut lines) = five_job_campaign_stream();
        let first_job = lines
            .iter()
            .position(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["event"] == "job"
            })
            .unwrap();
        let mut job: serde_json::Value = serde_json::from_str(&lines[first_job]).unwrap();
        job["selector"]["best_preference"] = serde_json::json!(0);
        lines[first_job] = serde_json::to_string(&job).unwrap();
        let error =
            replay_campaign_checkpointed(&workload, lines.join("\n").as_bytes(), None, None)
                .expect_err("replay certified a preference the key does not define")
                .to_string();
        assert!(error.contains("preference"), "{error}");
    }

    #[test]
    fn replay_rejects_an_impossible_recorded_suffix_limit() {
        let (workload, mut lines) = five_job_campaign_stream();
        let first_job = lines
            .iter()
            .position(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["event"] == "job"
            })
            .unwrap();
        let mut job: serde_json::Value = serde_json::from_str(&lines[first_job]).unwrap();
        assert_eq!(job["suffix_limit"], 1);
        job["suffix_limit"] = serde_json::json!(2);
        lines[first_job] = serde_json::to_string(&job).unwrap();
        let error =
            replay_campaign_checkpointed(&workload, lines.join("\n").as_bytes(), None, None)
                .expect_err("replay certified a suffix limit inconsistent with the archive")
                .to_string();
        assert!(error.contains("suffix limit"), "{error}");
    }

    const RECORDED_HEADER: &str = r#"{"schema_version":15,"format":"campaign-v1","campaign_seed":7,
"schedule_policy":"planned_finish_window_2_v6","progress_policy":"mechanical_watermark_bounded_1024_v2",
"host":"box","origin_kind":"genesis","origin_path":null,"origin_archive_sha256":null,
"resume_input_sha256":"ab","resume_actions":0,"execution_budget":10,"stop_rollout_on_objective":true,"stop_campaign_on_objective":true,"wall_budget_seconds":null,
"archive_entry_limit":128,"action_vocabulary":"test_inputs",
"key_policy":"test_key","duration_policy":"stratified",
"step_policy":"step_uniform","replacement_policy":"least_cost_per_group",
"resume_policy":"whole_tree","retention_policy":"unprobed",
"parent_scheduler":"tier_progress_cascade_pace_yield_cell_recent_count_decay_v2","preference_portfolio":"preference_portfolio_v1:1,1","executor_mode":"snapshot_resume_archive",
"selection_seed_derivation":"x","mixture_policy":"biased_half","workload_identity_sha256":"cd","executable_identity":"test-executable",
"action_cost_unit":"test_cost","execution_work_unit":"test_work"}"#;

    #[test]
    fn incremental_postcard_digest_matches_encoded_bytes() {
        let value = (17_u64, vec![0_u8, 1, 15, 16, 255], Some("deterministic"));
        let encoded = postcard::to_allocvec(&value).expect("fixture encodes");
        let expected = format!("{:x}", Sha256::digest(encoded));
        let actual = postcard_value_sha256(&value).expect("incremental digest succeeds");

        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 64);
    }

    #[test]
    fn progress_curve_compaction_is_bounded_and_evenly_spaced() {
        let mut curve: Vec<ProgressPoint<(), ()>> = (1..=MAX_PROGRESS_CURVE_POINTS)
            .map(|sample| ProgressPoint {
                executions: u64::try_from(sample).expect("sample fits u64") * 100,
                milestones: (),
                progress: None,
                active_entries: sample,
                occupied_cells: sample,
                terminal_endpoints: 0,
                execution_failures: 0,
            })
            .collect();

        let interval = compact_progress_curve(&mut curve, 100);

        assert_eq!(interval, 200);
        assert_eq!(curve.len(), MAX_PROGRESS_CURVE_POINTS / 2);
        assert!(
            curve
                .iter()
                .all(|point| point.executions.is_multiple_of(interval))
        );
        assert_eq!(curve.first().map(|point| point.executions), Some(200));
        assert_eq!(
            curve.last().map(|point| point.executions),
            Some(u64::try_from(MAX_PROGRESS_CURVE_POINTS).expect("bound fits u64") * 100)
        );
    }

    #[test]
    fn progress_curve_boundaries_and_final_point_are_exact() {
        let mut below: Vec<ProgressPoint<(), ()>> = (1..MAX_PROGRESS_CURVE_POINTS)
            .map(|sample| ProgressPoint {
                executions: u64::try_from(sample).expect("sample fits u64") * 100,
                milestones: (),
                progress: None,
                active_entries: sample,
                occupied_cells: sample,
                terminal_endpoints: 0,
                execution_failures: 0,
            })
            .collect();
        let unchanged = below.clone();
        assert_eq!(compact_progress_curve(&mut below, 100), 100);
        assert_eq!(below, unchanged);
        assert!(is_zero_usize(&0));
        assert!(!is_zero_usize(&1));

        let (_game, _run, mut core, _target) = test_core();
        core.sequence = 0;
        core.finish_curve();
        assert!(core.curve.is_empty());
        core.sequence = 7;
        core.finish_curve();
        core.finish_curve();
        assert_eq!(
            core.curve
                .iter()
                .map(|point| point.executions)
                .collect::<Vec<_>>(),
            vec![7]
        );

        core.curve = (1..=MAX_PROGRESS_CURVE_POINTS)
            .map(|sample| ProgressPoint {
                executions: u64::try_from(sample).expect("sample fits u64") * 100,
                milestones: (),
                progress: Some(()),
                active_entries: sample,
                occupied_cells: sample,
                terminal_endpoints: 0,
                execution_failures: 0,
            })
            .collect();
        core.curve_interval = 100;
        core.bounded_progress_curve = false;
        core.compact_progress_curve_if_needed();
        assert_eq!(core.curve.len(), MAX_PROGRESS_CURVE_POINTS);
        core.curve.pop();
        core.bounded_progress_curve = true;
        core.compact_progress_curve_if_needed();
        assert_eq!(core.curve.len(), MAX_PROGRESS_CURVE_POINTS - 1);
        core.curve.push(ProgressPoint {
            executions: u64::try_from(MAX_PROGRESS_CURVE_POINTS).expect("bound fits u64") * 100,
            milestones: (),
            progress: Some(()),
            active_entries: MAX_PROGRESS_CURVE_POINTS,
            occupied_cells: MAX_PROGRESS_CURVE_POINTS,
            terminal_endpoints: 0,
            execution_failures: 0,
        });
        core.bounded_progress_curve = true;
        core.compact_progress_curve_if_needed();
        assert_eq!(core.curve.len(), MAX_PROGRESS_CURVE_POINTS / 2);
        assert_eq!(core.curve_interval, 200);
    }

    #[test]
    fn sidecar_checkpoint_boundaries_use_completed_execution_count() {
        let checkpoints = (1..=200)
            .filter(|&executions| progress_checkpoint_due(executions))
            .collect::<Vec<_>>();
        assert_eq!(checkpoints, vec![1, 100, 200]);
        for executions in [0, 2, 99, 101] {
            assert!(!progress_checkpoint_due(executions));
        }
    }

    #[test]
    fn progress_lines_thin_to_a_bounded_count_per_doubling() {
        let span = PROGRESS_CHECKPOINT_INTERVAL * MAX_PROGRESS_CURVE_POINTS as u64;
        assert_eq!(
            progress_line_interval(2 * span - 1),
            PROGRESS_CHECKPOINT_INTERVAL
        );
        assert_eq!(
            progress_line_interval(2 * span),
            2 * PROGRESS_CHECKPOINT_INTERVAL
        );
        assert!(progress_checkpoint_due(2 * span + 200));
        assert!(!progress_checkpoint_due(2 * span + 100));
        for doubling in 1..8 {
            let start = span << doubling;
            let lines = (start..2 * start)
                .step_by(PROGRESS_CHECKPOINT_INTERVAL as usize)
                .filter(|&executions| progress_checkpoint_due(executions))
                .count();
            assert_eq!(lines, MAX_PROGRESS_CURVE_POINTS);
            let reports = (start..2 * start)
                .step_by(PROGRESS_CHECKPOINT_INTERVAL as usize)
                .filter(|&executions| full_selector_report_due(executions))
                .collect::<Vec<_>>();
            assert_eq!(reports, vec![start]);
            assert!(progress_checkpoint_due(start));
        }
        assert!(full_selector_report_due(span));
        assert!(!full_selector_report_due(span / 2));
        assert!(progress_line_interval(u64::MAX) > PROGRESS_CHECKPOINT_INTERVAL);
    }

    #[test]
    fn reservations_continue_after_objective_only_when_requested() {
        assert!(stop_reservations_after_objective(true, true));
        assert!(!stop_reservations_after_objective(false, true));
        assert!(!stop_reservations_after_objective(true, false));
    }

    #[test]
    fn coordinator_classifies_new_and_duplicate_boundaries() {
        let (workload, _run, mut core, mut target) = test_core();
        let action = TestAction::new(0x01, 4);
        target.apply(&action);
        let snapshot = target.snapshot().expect("snapshot candidate");
        let result = CampaignJobResult::<TestWorkload> {
            preparation_failure: None,
            actions: vec![CampaignActionResult::<TestWorkload> {
                action,
                observations: Vec::new(),
                milestones: (),
                outcome: Outcome::default(),
                candidate: Some(CampaignCandidate::<TestWorkload> {
                    key: TestKey(target.value),
                    viable: true,
                    snapshot,
                }),
            }],
        };

        let (first_sequence, first) = core
            .admit_job(&workload, 0, result.clone())
            .expect("admit new boundary");
        assert_eq!(first_sequence, 1);
        assert_eq!(first, vec![CampaignAdmissionDecision::Retained { id: 1 }]);
        assert!(core.all_prefixes_archived(0, &[action]));
        assert!(!core.all_prefixes_archived(0, &[]));

        let (second_sequence, second) = core
            .admit_job(&workload, 0, result)
            .expect("admit duplicate boundary");
        assert_eq!(second_sequence, 2);
        assert_eq!(second, vec![CampaignAdmissionDecision::Duplicate { id: 1 }]);
        assert_eq!(core.archive.retained, 2);
    }

    #[test]
    fn duration_credit_follows_the_action_that_created_novelty() {
        let (workload, _run, mut core, _target) = test_core();
        let tracked = TestAction::new(0x01, 4);
        let seed = CampaignJobResult::<TestWorkload> {
            preparation_failure: None,
            actions: vec![CampaignActionResult::<TestWorkload> {
                action: tracked,
                observations: Vec::new(),
                milestones: (),
                outcome: Outcome::default(),
                candidate: Some(CampaignCandidate::<TestWorkload> {
                    key: TestKey(1),
                    viable: true,
                    snapshot: 1,
                }),
            }],
        };
        core.admit_job(&workload, 0, seed)
            .expect("seed tracked boundary");

        let untracked = TestAction::new(0x01, 7);
        let result = CampaignJobResult::<TestWorkload> {
            preparation_failure: None,
            actions: vec![
                CampaignActionResult::<TestWorkload> {
                    action: tracked,
                    observations: Vec::new(),
                    milestones: (),
                    outcome: Outcome::default(),
                    candidate: Some(CampaignCandidate::<TestWorkload> {
                        key: TestKey(1),
                        viable: true,
                        snapshot: 1,
                    }),
                },
                CampaignActionResult::<TestWorkload> {
                    action: untracked,
                    observations: Vec::new(),
                    milestones: (),
                    outcome: Outcome::default(),
                    candidate: Some(CampaignCandidate::<TestWorkload> {
                        key: TestKey(2),
                        viable: true,
                        snapshot: 2,
                    }),
                },
            ],
        };
        let (_, decisions, duration) = core
            .admit_job_tracking(&workload, 0, &mut { result }, false, |action| {
                *action == tracked
            })
            .expect("admit mixed novelty");
        assert_eq!(
            decisions,
            vec![
                CampaignAdmissionDecision::Duplicate { id: 1 },
                CampaignAdmissionDecision::Retained { id: 2 },
            ]
        );
        assert_eq!(
            duration,
            DurationAdmission {
                applied: true,
                useful: false,
            }
        );
    }

    #[test]
    fn field_generic_result_storage_matches_original_json_and_postcard_shapes() {
        #[derive(Deserialize, Serialize)]
        struct OriginalCandidate {
            key: TestKey,
            viable: bool,
            snapshot: u8,
        }
        #[derive(Deserialize, Serialize)]
        struct OriginalAction {
            action: TestAction,
            observations: Vec<()>,
            milestones: (),
            outcome: Outcome,
            candidate: Option<OriginalCandidate>,
        }
        #[derive(Deserialize, Serialize)]
        struct OriginalResult {
            preparation_failure: Option<Vec<()>>,
            actions: Vec<OriginalAction>,
        }
        for preparation in [None, Some(vec![]), Some(vec![()])] {
            for candidate in [None, Some(false), Some(true)] {
                let original = OriginalResult {
                    preparation_failure: preparation.clone(),
                    actions: vec![OriginalAction {
                        action: TestAction::new(1, 2),
                        observations: vec![()],
                        milestones: (),
                        outcome: Outcome::default(),
                        candidate: candidate.map(|viable| OriginalCandidate {
                            key: TestKey(3),
                            viable,
                            snapshot: 4,
                        }),
                    }],
                };
                let json = serde_json::to_vec(&original).unwrap();
                let result: CampaignJobResult<TestWorkload> =
                    serde_json::from_slice(&json).unwrap();
                assert_eq!(serde_json::to_vec(&result).unwrap(), json);
                let postcard = postcard::to_allocvec(&original).unwrap();
                assert_eq!(postcard::to_allocvec(&result).unwrap(), postcard);
                assert_eq!(
                    postcard::from_bytes::<CampaignJobResult<TestWorkload>>(&postcard).unwrap(),
                    result
                );
                assert_eq!(
                    serde_json::to_vec(
                        &serde_json::from_slice::<OriginalResult>(
                            &serde_json::to_vec(&result).unwrap()
                        )
                        .unwrap()
                    )
                    .unwrap(),
                    json
                );
                assert!(format!("{result:?}").starts_with("CampaignJobResult {"));
                assert!(format!("{:?}", result.actions[0]).starts_with("CampaignActionResult {"));
                if let Some(candidate) = result.actions[0].candidate.as_ref() {
                    assert!(format!("{candidate:?}").starts_with("CampaignCandidate {"));
                }
            }
        }
    }

    #[test]
    fn native_encoder_accepts_owned_payloads_from_a_borrowed_type_carrier() {
        struct BorrowedTypes<'a>(&'a str);
        impl CampaignTypes for BorrowedTypes<'_> {
            type JobObservation = ();
            type Target = ();
            type Action = TestAction;
            type Key = TestKey;
            type Milestones = ();
            type Progress = ();
            type Snapshot = u8;
            type Observations = ();
            type Evidence = ();
            type ArchiveReport = ();
            type Run = ();
        }
        fn encode<'a>(carrier: &BorrowedTypes<'a>) -> Vec<u8> {
            assert!(!carrier.0.is_empty());
            let result = CampaignJobResult::<BorrowedTypes<'a>> {
                preparation_failure: Some(vec![()]),
                actions: Vec::new(),
            };
            let mut sink = Vec::new();
            let mut writer =
                super::StreamWriter::<u8, CampaignJobResult<BorrowedTypes<'a>>>::new_with_cleanup(
                    &mut sink,
                );
            writer.enable_background(None).unwrap();
            writer.write_line_with_cleanup(1, result).unwrap();
            writer.finish().unwrap();
            sink
        }
        let local = String::from("borrowed type carrier");
        assert_eq!(encode(&BorrowedTypes(&local)), b"1\n");
    }

    #[test]
    fn borrowed_admission_keeps_unused_candidates_and_result_storage_for_cleanup() {
        let (workload, _, mut core, _) = test_core();
        let make = |input, work_units, viable| CampaignJobResult::<TestWorkload> {
            preparation_failure: None,
            actions: vec![CampaignActionResult::<TestWorkload> {
                action: TestAction::new(input, work_units),
                observations: vec![()],
                milestones: (),
                outcome: Outcome::default(),
                candidate: Some(CampaignCandidate::<TestWorkload> {
                    key: TestKey(1),
                    viable,
                    snapshot: 1,
                }),
            }],
        };
        let mut retained = make(1, 1, true);
        let actions = retained.actions.as_ptr();
        let observations = retained.actions[0].observations.as_ptr();
        let (_, decisions, _) = core
            .admit_job_tracking(&workload, 0, &mut retained, false, |_| false)
            .unwrap();
        assert!(matches!(
            decisions.as_slice(),
            [CampaignAdmissionDecision::Retained { .. }]
        ));
        assert!(retained.actions[0].candidate.is_none());
        assert_eq!(retained.actions.as_ptr(), actions);
        assert_eq!(retained.actions[0].observations.as_ptr(), observations);
        for offset in 1..TestKey::capacity().max(1) {
            let mut filler = make(u8::try_from(10 + offset).unwrap(), 1, true);
            let (_, decisions, _) = core
                .admit_job_tracking(&workload, 0, &mut filler, false, |_| false)
                .unwrap();
            assert!(matches!(
                decisions.as_slice(),
                [CampaignAdmissionDecision::Retained { .. }]
            ));
        }
        for (mut result, expected) in [
            (make(1, 1, true), "duplicate"),
            (make(2, 10, true), "rejected"),
            (make(3, 1, false), "refused"),
        ] {
            let candidate = result.actions[0].candidate.clone();
            let actions = result.actions.as_ptr();
            let observations = result.actions[0].observations.as_ptr();
            let (_, decisions, _) = core
                .admit_job_tracking(&workload, 0, &mut result, false, |_| false)
                .unwrap();
            assert!(matches!(
                (expected, decisions.as_slice()),
                ("duplicate", [CampaignAdmissionDecision::Duplicate { .. }])
                    | ("rejected", [CampaignAdmissionDecision::Rejected])
                    | ("refused", [CampaignAdmissionDecision::ProbeRefused])
            ));
            assert_eq!(result.actions[0].candidate, candidate);
            assert_eq!(result.actions.as_ptr(), actions);
            assert_eq!(result.actions[0].observations.as_ptr(), observations);
            assert_eq!(result.actions[0].observations, vec![()]);
        }
        let mut preparation = CampaignJobResult::<TestWorkload> {
            preparation_failure: Some(vec![()]),
            actions: Vec::new(),
        };
        let observations = preparation.preparation_failure.as_ref().unwrap().as_ptr();
        let failures = core.execution_failures;
        let (_, decisions, _) = core
            .admit_job_tracking(&workload, 0, &mut preparation, false, |_| false)
            .unwrap();
        assert!(decisions.is_empty());
        assert_eq!(core.execution_failures, failures + 1);
        assert_eq!(
            preparation.preparation_failure.as_ref().unwrap().as_ptr(),
            observations
        );
    }

    #[test]
    fn a_probe_refused_state_in_another_place_resets_the_suffix_limit() {
        let (workload, _run, mut core, _target) = test_core();
        let home = core.archive.entries[0].key.0;
        let refused = |value: u8| CampaignJobResult::<TestWorkload> {
            preparation_failure: None,
            actions: vec![CampaignActionResult::<TestWorkload> {
                action: TestAction::new(0x01, 1),
                observations: Vec::new(),
                milestones: (),
                outcome: Outcome::default(),
                candidate: Some(CampaignCandidate::<TestWorkload> {
                    key: TestKey(value),
                    viable: false,
                    snapshot: value,
                }),
            }],
        };
        core.admit_job_tracking(&workload, 0, &mut refused(home), true, |_| false)
            .expect("admit a refused state in the parent's place");
        assert_eq!(core.archive.suffix_limit(0), 2);
        core.admit_job_tracking(
            &workload,
            0,
            &mut refused(INHERITS_PARENT_PLACE),
            true,
            |_| false,
        )
        .expect("admit a refused state whose completed key keeps the parent's place");
        assert_eq!(core.archive.suffix_limit(0), 2);
        core.admit_job_tracking(
            &workload,
            0,
            &mut refused(home.wrapping_add(1)),
            true,
            |_| false,
        )
        .expect("admit a refused state in another place");
        assert_eq!(core.archive.suffix_limit(0), 1);
    }

    #[test]
    fn a_parent_with_a_job_in_flight_at_its_suffix_limit_draws_one_action() {
        let (_workload, _run, mut core, _target) = test_core();
        let parent = core.archive.stable_id(0).expect("the root has an id");
        core.archive
            .record_suffix_outcome(0, 1, false, false, false);
        assert_eq!(core.suffix_limit(parent, 0), 2);
        core.start_suffix_job(parent, Some(1));
        assert_eq!(core.suffix_limit(parent, 0), 2);
        core.start_suffix_job(parent, Some(2));
        assert_eq!(core.suffix_limit(parent, 0), 1);
        core.archive
            .record_suffix_outcome(0, 2, false, false, false);
        assert_eq!(core.suffix_limit(parent, 0), 4);
        core.start_suffix_job(parent, Some(4));
        core.start_suffix_job(parent, Some(4));
        core.finish_suffix_job(parent, Some(4));
        assert_eq!(core.suffix_limit(parent, 0), 1);
        core.finish_suffix_job(parent, Some(4));
        assert_eq!(core.suffix_limit(parent, 0), 4);
        core.start_suffix_job(parent, None);
        assert_eq!(core.suffix_limit(parent, 0), 4);
    }

    #[test]
    fn coordinator_counts_objectives_and_keeps_the_first_witness() {
        let (workload, _run, mut core, _target) = test_core();
        let winning = TestAction::new(0x81, 7);
        let result = CampaignJobResult::<TestWorkload> {
            preparation_failure: None,
            actions: vec![CampaignActionResult::<TestWorkload> {
                action: winning,
                observations: Vec::new(),
                milestones: (),
                outcome: Outcome {
                    objective_reached: true,
                    disposition: ExecutionDisposition::Runnable,
                },
                candidate: Some(CampaignCandidate::<TestWorkload> {
                    key: TestKey(winning.input),
                    viable: true,
                    snapshot: winning.input,
                }),
            }],
        };
        let winning_action = result.actions[0].clone();
        let (sequence, decisions) = core
            .admit_job(&workload, 0, result)
            .expect("admit objective");
        assert_eq!(sequence, 1);
        assert_eq!(
            decisions,
            vec![
                CampaignAdmissionDecision::Objective,
                CampaignAdmissionDecision::Retained { id: 1 },
            ]
        );
        assert_eq!(core.objectives_reached, 1);
        assert_eq!(
            core.objective_witness,
            Some(Input {
                actions: vec![winning]
            })
        );

        let later = CampaignJobResult::<TestWorkload> {
            preparation_failure: None,
            actions: vec![CampaignActionResult::<TestWorkload> {
                action: TestAction::new(0x01, 9),
                ..winning_action
            }],
        };
        core.admit_job(&workload, 0, later)
            .expect("admit a second objective");
        assert_eq!(core.objectives_reached, 2);
        assert_eq!(
            core.objective_witness,
            Some(Input {
                actions: vec![winning]
            })
        );
        let (report, _) = core
            .into_archive_report_and_snapshots(&workload, 0, true)
            .expect("report and snapshots");
        assert_eq!(
            report.entries.len(),
            3,
            "objective retains its runnable candidates"
        );
    }

    #[test]
    fn whole_tree_import_rebuilds_inputs_and_reroots_sparse_parents() {
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let run = ();
        let mut target = TestTarget::default();
        let action = |input: u8| TestAction::new(input, 1);
        let entry =
            |id: u64, parent_id: Option<u64>, actions: Vec<TestAction>| ArchiveEntryReport {
                id,
                parent_id,
                created_execution: 0,
                input: Input { actions },
                key: TestKey(0),
                milestones: (),
                snapshot_identity_sha256: None,
                selector: None,
            };
        let source = TestArchiveReport {
            entries: vec![
                entry(0, None, Vec::new()),
                entry(1, Some(0), vec![action(0x01)]),
                entry(2, Some(1), vec![action(0x01), action(0x02)]),
                entry(3, Some(1), vec![action(0x01), action(0x80)]),
                entry(9, Some(7), vec![action(0x01), action(0x02), action(0x40)]),
            ],
            marker: 0,
        };
        let mut core = CoordinatorCore::new(&workload, &run, 32_768, None);
        let suffix_json = serde_json::to_string(&source).expect("serialize source archive");
        assert!(suffix_json.contains("\"input_suffix\""));
        let rebuilt: TestArchiveReport =
            serde_json::from_str(&suffix_json).expect("load suffix archive");
        assert_eq!(rebuilt, source);
        let counts = core
            .import_tree(&workload, &run, &mut target, &source, None)
            .expect("import source archive");
        assert_eq!(counts.rerooted, 1);
        assert_eq!(counts.terminal, 0);
        assert_eq!(counts.imported + counts.rejected, 4);
        let reports = core.archive.take_entry_reports_and_snapshots().0;
        assert_eq!(reports[0].input.actions.len(), 0);
        for report in &reports[1..] {
            let parent = usize::try_from(report.parent_id.expect("parent")).expect("index");
            let parent_input = &reports[parent].input.actions;
            assert_eq!(
                report.input.actions.get(..parent_input.len()),
                Some(parent_input.as_slice())
            );
            assert_eq!(report.created_execution, 0);
        }
        assert_eq!(
            reports.len(),
            usize::try_from(counts.imported).expect("imported count") + 1
        );
    }

    #[test]
    fn whole_tree_import_reuses_bounded_captures_and_keeps_every_prefix_certificate() {
        for (fused, omitted) in [
            (false, None),
            (true, None),
            (true, Some(3)),
            (true, Some(6)),
        ] {
            let workload = TestWorkload {
                bootstrap_objective: false,
                targets: None,
            };
            let actions = vec![
                TestAction::new(1, 1),
                TestAction::new(2, 1),
                TestAction::new(3, 1),
            ];
            let source = TestArchiveReport {
                entries: vec![
                    ArchiveEntryReport {
                        id: 0,
                        parent_id: None,
                        created_execution: 0,
                        input: Input::default(),
                        key: TestKey(0),
                        milestones: (),
                        snapshot_identity_sha256: None,
                        selector: None,
                    },
                    ArchiveEntryReport {
                        id: 1,
                        parent_id: Some(0),
                        created_execution: 0,
                        input: Input {
                            actions: actions.clone(),
                        },
                        key: TestKey(6),
                        milestones: (),
                        snapshot_identity_sha256: Some(
                            workload.snapshot_identity_sha256(&6).unwrap(),
                        ),
                        selector: None,
                    },
                ],
                marker: 0,
            };
            CERTIFIED_IMPORT.with(|enabled| enabled.set(true));
            FUSED_IMPORT.with(|enabled| enabled.set(fused));
            OMIT_IMPORT_CAPTURE.with(|capture| capture.set(omitted));
            IMPORT_CALLS.with(|calls| calls.set((0, 0, 0)));
            let mut core = CoordinatorCore::new(&workload, &(), 32_768, None);
            core.archive.set_memory_budget(usize::MAX, |_| 1);
            let result =
                core.import_tree(&workload, &(), &mut TestTarget::default(), &source, None);
            FUSED_IMPORT.with(|enabled| enabled.set(false));
            OMIT_IMPORT_CAPTURE.with(|capture| capture.set(None));
            CERTIFIED_IMPORT.with(|enabled| enabled.set(false));
            assert_eq!(result.unwrap().imported, 1);
            assert_eq!(
                IMPORT_CALLS.with(|calls| calls.get()),
                match (fused, omitted) {
                    (false, _) => (0, 3, 2),
                    (true, None) => (3, 0, 1),
                    (true, Some(3)) => (2, 1, 1),
                    (true, Some(6)) => (2, 1, 2),
                    _ => unreachable!(),
                }
            );
            let child = core.archive.index_of_id(1).unwrap();
            core.archive
                .set_memory_budget(core.archive.history_memory_bytes() + 1, |_| 1);
            for _ in 0..8 {
                core.archive.maintain_memory_budget().unwrap();
            }
            assert!(core.archive.entries[child].snapshot.is_none());
            let (_, replay, certificates, pin) = core.archive.pin_job_origin(child).unwrap();
            assert_eq!(replay, actions);
            assert_eq!(
                certificates,
                vec![Some([1; 32]), Some([3; 32]), Some([6; 32])]
            );
            core.archive.unpin_job_origin(pin);
        }
    }

    #[test]
    fn whole_tree_import_certifies_evicted_replay_prefixes_with_or_without_source_snapshots() {
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let actions = vec![
            TestAction::new(1, 1),
            TestAction::new(2, 1),
            TestAction::new(3, 1),
        ];
        let source = TestArchiveReport {
            entries: vec![
                ArchiveEntryReport {
                    id: 0,
                    parent_id: None,
                    created_execution: 0,
                    input: Input::default(),
                    key: TestKey(0),
                    milestones: (),
                    snapshot_identity_sha256: None,
                    selector: None,
                },
                ArchiveEntryReport {
                    id: 1,
                    parent_id: Some(0),
                    created_execution: 1,
                    input: Input {
                        actions: actions.clone(),
                    },
                    key: TestKey(6),
                    milestones: (),
                    snapshot_identity_sha256: Some(workload.snapshot_identity_sha256(&6).unwrap()),
                    selector: None,
                },
            ],
            marker: 0,
        };
        CERTIFIED_IMPORT.with(|enabled| enabled.set(true));
        let result = (|| -> Result<(), Box<dyn Error>> {
            for checkpoint in [
                None,
                Some(SnapshotCheckpoint {
                    format: workload.checkpoint_format().to_owned(),
                    entries: vec![SnapshotCheckpointEntry { id: 1, snapshot: 6 }],
                }),
            ] {
                let mut core = CoordinatorCore::new(&workload, &(), 32_768, None);
                core.archive.set_memory_budget(usize::MAX, |_| 1);
                let mut target = TestTarget::default();
                let counts =
                    core.import_tree(&workload, &(), &mut target, &source, checkpoint.as_ref())?;
                assert_eq!(counts.imported, 1);
                core.archive.set_memory_budget(
                    core.archive.history_memory_bytes().saturating_add(1),
                    |_| 1,
                );
                for _ in 0..8 {
                    core.archive.maintain_memory_budget()?;
                    if core.archive.snapshot_evictions() > 0 {
                        break;
                    }
                }
                let child = core.archive.index_of_id(1).unwrap();
                assert!(
                    core.archive.entries[child].snapshot.is_none(),
                    "evictions={}, resident={}, history={}, snapshots={}",
                    core.archive.snapshot_evictions(),
                    core.archive.resident_memory_bytes(),
                    core.archive.history_memory_bytes(),
                    core.archive.resident_snapshot_count(),
                );
                let (_, replay, certificates, pin) = core.archive.pin_job_origin(child)?;
                assert_eq!(replay, actions);
                assert_eq!(
                    certificates,
                    vec![Some([1; 32]), Some([3; 32]), Some([6; 32])]
                );
                core.archive.unpin_job_origin(pin);
            }
            let mut conflicting_source = source.clone();
            conflicting_source.entries[1].snapshot_identity_sha256 =
                Some(workload.snapshot_identity_sha256(&7)?);
            let conflicting_checkpoint = SnapshotCheckpoint {
                format: workload.checkpoint_format().to_owned(),
                entries: vec![SnapshotCheckpointEntry { id: 1, snapshot: 7 }],
            };
            let mut core = CoordinatorCore::new(&workload, &(), 32_768, None);
            core.archive.set_memory_budget(usize::MAX, |_| 1);
            let mut target = TestTarget::default();
            let error = core
                .import_tree(
                    &workload,
                    &(),
                    &mut target,
                    &conflicting_source,
                    Some(&conflicting_checkpoint),
                )
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("differs from its reconstructed endpoint"),
                "{error}"
            );
            assert_eq!(core.archive.entries.len(), 1);
            assert_eq!(core.archive.input_index_nodes(), 1);
            OMIT_TEST_PREFIX_CERTIFICATE.with(|omit| omit.set(true));
            let mut core = CoordinatorCore::new(&workload, &(), 32_768, None);
            let mut target = TestTarget::default();
            let missing = core
                .import_tree(&workload, &(), &mut target, &source, None)
                .unwrap_err()
                .to_string();
            OMIT_TEST_PREFIX_CERTIFICATE.with(|omit| omit.set(false));
            assert!(
                missing.contains("lacks an action-prefix certificate"),
                "{missing}"
            );
            assert_eq!(core.archive.entries.len(), 1);
            assert_eq!(core.archive.input_index_nodes(), 1);
            Ok(())
        })();
        CERTIFIED_IMPORT.with(|enabled| enabled.set(false));
        result.unwrap();
    }

    #[test]
    fn whole_tree_import_rejects_mismatched_source_snapshot_without_checkpoint() {
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let source = TestArchiveReport {
            entries: vec![
                ArchiveEntryReport {
                    id: 0,
                    parent_id: None,
                    created_execution: 0,
                    input: Input::default(),
                    key: TestKey(0),
                    milestones: (),
                    snapshot_identity_sha256: Some(workload.snapshot_identity_sha256(&0).unwrap()),
                    selector: None,
                },
                ArchiveEntryReport {
                    id: 1,
                    parent_id: Some(0),
                    created_execution: 1,
                    input: Input {
                        actions: vec![TestAction::new(1, 1), TestAction::new(2, 1)],
                    },
                    key: TestKey(3),
                    milestones: (),
                    snapshot_identity_sha256: Some(workload.snapshot_identity_sha256(&3).unwrap()),
                    selector: None,
                },
            ],
            marker: 9,
        };
        for (entry, conflicting_value) in [(1, 4), (0, 1)] {
            let mut conflicting = source.clone();
            conflicting.entries[entry].snapshot_identity_sha256 = Some(
                workload
                    .snapshot_identity_sha256(&conflicting_value)
                    .unwrap(),
            );
            let mut core = CoordinatorCore::new(&workload, &(), 32_768, None);
            let mut target = TestTarget::default();
            let error = core
                .import_tree(&workload, &(), &mut target, &conflicting, None)
                .unwrap_err()
                .to_string();
            assert!(error.contains("source entry digest"), "{error}");
            assert_eq!(core.archive.entries.len(), 1);
            assert_eq!(core.archive.input_index_nodes(), 1);
            assert_eq!(core.evidence, 0);
        }
    }

    #[test]
    fn certified_admission_rejects_a_missing_prefix_before_archiving() {
        let (workload, _, mut core, _) = test_core();
        let first = TestAction::new(1, 1);
        let second = TestAction::new(2, 1);
        let result = CampaignJobResult::<TestWorkload> {
            preparation_failure: None,
            actions: vec![
                CampaignActionResult::<TestWorkload> {
                    action: first,
                    observations: Vec::new(),
                    milestones: (),
                    outcome: Outcome::default(),
                    candidate: None,
                },
                CampaignActionResult::<TestWorkload> {
                    action: second,
                    observations: Vec::new(),
                    milestones: (),
                    outcome: Outcome::default(),
                    candidate: Some(CampaignCandidate::<TestWorkload> {
                        key: TestKey(3),
                        viable: true,
                        snapshot: 3,
                    }),
                },
            ],
        };
        CERTIFIED_IMPORT.with(|enabled| enabled.set(true));
        let error = core
            .admit_job(&workload, 0, result)
            .unwrap_err()
            .to_string();
        CERTIFIED_IMPORT.with(|enabled| enabled.set(false));
        assert!(
            error.contains("lacks an action-prefix certificate"),
            "{error}"
        );
        assert_eq!(core.archive.entries.len(), 1);
        assert_eq!(core.archive.input_index_nodes(), 1);
    }

    #[test]
    fn recorded_splice_tail_validates_strategy_and_bounds() {
        let (_game, _run, _core, _target) = test_core();
        let action = TestAction::new(0x01, 4);
        let encoded = |actions: Vec<TestAction>| CampaignSpliceRecord::Tail {
            donor_id: 90,
            leaf_id: 91,
            tail_postcard: postcard::to_allocvec(&actions).expect("encode splice tail"),
        };

        assert_eq!(
            replay_splice::<TestWorkload>(EnergyStrategy::Splice, Some(encoded(vec![action])))
                .expect("replay explicit tail"),
            Some(vec![action])
        );
        assert_eq!(
            replay_splice::<TestWorkload>(
                EnergyStrategy::Splice,
                Some(CampaignSpliceRecord::Unavailable),
            )
            .expect("replay unavailable tail"),
            None
        );
        assert!(
            replay_splice::<TestWorkload>(EnergyStrategy::Splice, Some(encoded(Vec::new())))
                .is_err()
        );
        assert!(
            replay_splice::<TestWorkload>(
                EnergyStrategy::Splice,
                Some(encoded(vec![action; LONGEST_SPLICE_TAIL + 1])),
            )
            .is_err()
        );
        assert_eq!(
            replay_splice::<TestWorkload>(
                EnergyStrategy::Splice,
                Some(encoded(vec![action; LONGEST_SPLICE_TAIL])),
            )
            .expect("tail at cap is valid")
            .map(|tail| tail.len()),
            Some(LONGEST_SPLICE_TAIL)
        );
        assert!(
            replay_splice::<TestWorkload>(
                EnergyStrategy::Alphabet,
                Some(CampaignSpliceRecord::Unavailable),
            )
            .is_err()
        );
        assert!(replay_splice::<TestWorkload>(EnergyStrategy::Splice, None).is_err());
    }

    #[test]
    fn coordinator_profile_helpers_measure_elapsed_time() {
        #[allow(clippy::disallowed_methods)]
        let started = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("one millisecond before now");
        assert!(profile_elapsed(Some(started)) > 0);
        assert_eq!(profile_elapsed(None), 0);

        let mut profile = LiveCoordinatorProfile::default();
        assert!(profile.telemetry_json().unwrap()["admission_ns"].is_null());
        profile.timing_complete = true;
        assert_eq!(profile.telemetry_json().unwrap()["admission_ns"], 0);
        record_compaction_elapsed(&mut profile, 4, 4, 10);
        record_compaction_elapsed(&mut profile, 4, 3, 20);
        assert_eq!(profile.history_compaction_ns, 0);
        record_compaction_elapsed(&mut profile, 4, 5, 30);
        assert_eq!(profile.history_compaction_ns, 30);
    }

    #[test]
    fn admission_bookkeeping_helpers_preserve_retained_meaning() {
        let (workload, run, mut core, mut target) = test_core();
        let action = TestAction::new(0x01, 4);
        target.apply(&action);
        let snapshot = target.snapshot().expect("snapshot candidate");
        let result = CampaignJobResult::<TestWorkload> {
            preparation_failure: None,
            actions: vec![CampaignActionResult::<TestWorkload> {
                action,
                observations: Vec::new(),
                milestones: (),
                outcome: Outcome::default(),
                candidate: Some(CampaignCandidate::<TestWorkload> {
                    key: TestKey(target.value),
                    viable: true,
                    snapshot,
                }),
            }],
        };
        let (_, admission) = core
            .admit_job(&workload, 0, result)
            .expect("admit second archive entry");
        assert_eq!(
            admission,
            vec![CampaignAdmissionDecision::Retained { id: 1 }]
        );
        let decisions = [
            CampaignAdmissionDecision::Retained { id: 0 },
            CampaignAdmissionDecision::Retained { id: 1 },
            CampaignAdmissionDecision::Duplicate { id: 0 },
            CampaignAdmissionDecision::Rejected,
        ];
        assert_eq!(
            retained_archive_indexes(&core, &decisions).collect::<Vec<_>>(),
            vec![0, 1]
        );

        let (mut draw_state, _header) =
            super::initial_draw_state(&workload, &run, None).expect("initialize draw state");
        assert!(
            finish_record(
                &workload,
                &run,
                &mut draw_state,
                &core,
                &[CampaignAdmissionDecision::Retained { id: u64::MAX }],
            )
            .is_err()
        );
    }

    #[test]
    fn sliding_window_depth_is_the_only_selection_staleness() {
        let depth = default_window(3);
        assert_eq!(depth, 3);

        let mut next_selection = 0_usize;
        let mut selected = Vec::new();
        for next_admission in 0..=depth {
            while next_selection < next_admission + depth {
                selected.push((next_selection, next_admission));
                next_selection += 1;
            }
            assert_eq!(next_selection, next_admission + depth);
        }

        assert_eq!(selected.first(), Some(&(0, 0)));
        assert_eq!(selected.last(), Some(&(depth * 2 - 1, depth)));
        assert!(
            selected
                .iter()
                .all(|(reservation, admitted)| *reservation < admitted + depth)
        );
        assert!(
            selected
                .iter()
                .skip(depth)
                .all(|(reservation, admitted)| *admitted >= reservation - depth)
        );
    }

    #[test]
    fn the_first_objective_counters_exclude_jobs_that_drain_after_the_objective() {
        let mut counters = CampaignCounters::new();
        counters.bootstrap_execution_work = 1_000;
        counters.job_execution_work = 250;
        counters.note_first_objective(11);
        assert_eq!(counters.work_to_first_objective, Some(1_250));
        assert_eq!(counters.executions_to_first_objective, Some(11));

        for (sequence, drained) in [(12, 90), (13, 140), (14, 70)] {
            counters.job_execution_work = counters.job_execution_work.saturating_add(drained);
            counters.note_first_objective(sequence);
        }
        assert_eq!(counters.work_to_first_objective, Some(1_250));
        assert_eq!(counters.executions_to_first_objective, Some(11));
        assert_eq!(
            counters
                .bootstrap_execution_work
                .saturating_add(counters.job_execution_work),
            1_550
        );
    }

    #[test]
    fn a_run_that_never_reaches_its_objective_has_no_first_objective_counters() {
        let mut counters = CampaignCounters::new();
        counters.bootstrap_execution_work = 10;
        counters.job_execution_work = 20;
        assert_eq!(counters.work_to_first_objective, None);
        assert_eq!(counters.executions_to_first_objective, None);
    }

    #[test]
    fn schedule_policy_dispatch_accepts_only_current_windows() {
        let current = schedule_policy_identifier(default_window(1));
        assert_eq!(current, "planned_finish_window_1_v6");
        assert_eq!(schedule_policy_window(&current), Some(1));
        assert!(schedule_policy_is_supported("planned_finish_window_64_v6"));
        assert!(!schedule_policy_is_supported("planned_finish_window_64_v5"));
        assert!(!schedule_policy_is_supported("deterministic_window_64_v4"));
        assert!(!schedule_policy_is_supported("planned_finish_window_0_v6"));
        assert!(!schedule_policy_is_supported("unknown-schedule"));
        assert_eq!(
            schedule_policy_window("planned_finish_window_16_v6"),
            Some(16)
        );
    }

    #[test]
    fn recorded_resource_bounds_and_progress_policies_validate_at_boundaries() {
        let max = crate::search::archive::MAX_ARCHIVE_ENTRIES;
        assert!(!archive_entry_limit_is_valid(0));
        assert!(archive_entry_limit_is_valid(1));
        assert!(archive_entry_limit_is_valid(max));
        assert!(!archive_entry_limit_is_valid(max.saturating_add(1)));

        assert!(memory_is_within_reserve(0, 0));
        assert!(memory_is_within_reserve(1, 2));
        assert!(memory_is_within_reserve(2, 2));
        assert!(!memory_is_within_reserve(2, 1));
        assert!(resident_memory_is_within_budget(usize::MAX, None));
        assert!(resident_memory_is_within_budget(64 * 1024 * 1024, Some(64)));
        assert!(!resident_memory_is_within_budget(
            64 * 1024 * 1024 + 1,
            Some(64)
        ));

        assert!(progress_policy_is_supported(
            super::CAMPAIGN_PROGRESS_POLICY
        ));
        assert!(!progress_policy_is_supported("mechanical_watermark_v1"));
        assert!(!progress_policy_is_supported("unknown-progress"));
    }

    #[test]
    fn execution_work_delta_rejects_a_counter_rewind() {
        assert_eq!(execution_work_delta(7, 11, "worker").unwrap(), 4);
        let error = execution_work_delta(11, 7, "worker").unwrap_err();
        assert_eq!(error.to_string(), "worker execution-work counter rewound");
    }

    #[test]
    fn a_recorded_header_still_reads_and_writes_its_field_names() {
        let compact = RECORDED_HEADER.replace('\n', "");
        let header: CampaignStreamHeader<()> =
            serde_json::from_str(&compact).expect("recorded header parses");
        assert_eq!(header.schedule_policy, "planned_finish_window_2_v6");
        assert_eq!(
            header.progress_policy,
            "mechanical_watermark_bounded_1024_v2"
        );
        assert_eq!(header.mixture_policy, "biased_half");
        assert_eq!(header.resume_policy, "whole_tree");
        assert_eq!(header.schema_version, super::CAMPAIGN_SCHEMA_VERSION);
        assert_eq!(header.draw_header, None);
        let expected: WorkloadPolicies = [
            ("action_vocabulary", "test_inputs"),
            ("key_policy", "test_key"),
            ("duration_policy", "stratified"),
            ("step_policy", "step_uniform"),
            ("replacement_policy", "least_cost_per_group"),
        ]
        .into_iter()
        .map(|(field, value)| (field.to_owned(), value.to_owned()))
        .collect();
        assert_eq!(header.workload_policies, expected);

        let written = serde_json::to_value(&header).expect("header serializes");
        let object = written.as_object().expect("header is an object");
        for field in expected.keys() {
            assert_eq!(
                object.get(field).and_then(serde_json::Value::as_str),
                Some(expected[field].as_str())
            );
        }
        let recorded: serde_json::Value =
            serde_json::from_str(&compact).expect("recorded header parses");
        let recorded_fields: BTreeSet<&String> = recorded
            .as_object()
            .expect("recorded header is an object")
            .keys()
            .collect();
        assert_eq!(object.keys().collect::<BTreeSet<_>>(), recorded_fields);
    }

    #[test]
    fn replay_rejects_missing_or_unknown_campaign_schema_versions() {
        let compact = RECORDED_HEADER.replace('\n', "");
        let mut missing: serde_json::Value =
            serde_json::from_str(&compact).expect("recorded header parses");
        missing
            .as_object_mut()
            .expect("header is an object")
            .remove("schema_version");
        assert!(
            replay_campaign_checkpointed::<TestWorkload>(
                &TestWorkload {
                    bootstrap_objective: false,
                    targets: None,
                },
                serde_json::to_string(&missing)
                    .expect("header re-encodes")
                    .as_bytes(),
                None,
                None,
            )
            .is_err()
        );

        let mut unknown: serde_json::Value =
            serde_json::from_str(&compact).expect("recorded header parses");
        unknown["schema_version"] = serde_json::json!(1);
        assert!(
            replay_campaign_checkpointed::<TestWorkload>(
                &TestWorkload {
                    bootstrap_objective: false,
                    targets: None,
                },
                serde_json::to_string(&unknown)
                    .expect("header re-encodes")
                    .as_bytes(),
                None,
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn a_checkpoint_in_an_older_layout_fails_on_its_format() {
        #[derive(Serialize)]
        struct OlderLayout {
            format: &'static str,
            ids: Vec<u64>,
        }
        let older = postcard::to_allocvec(&OlderLayout {
            format: "test-checkpoint-v0",
            ids: vec![7],
        })
        .expect("older layout encodes");
        let error = SnapshotCheckpoint::<u8>::from_bytes(&older, "test-checkpoint-v1")
            .expect_err("an older checkpoint is rejected")
            .to_string();
        assert_eq!(error, "snapshot checkpoint format is not recognized");

        let current = SnapshotCheckpoint {
            format: "test-checkpoint-v1".to_owned(),
            entries: vec![SnapshotCheckpointEntry {
                id: 7,
                snapshot: 9_u8,
            }],
        };
        let bytes = current.to_bytes().expect("checkpoint encodes");
        assert_eq!(
            SnapshotCheckpoint::<u8>::from_bytes(&bytes, "test-checkpoint-v1")
                .expect("current checkpoint decodes"),
            current
        );
        assert_eq!(
            SnapshotCheckpoint::<u8>::from_bytes(&bytes, "test-checkpoint-v2")
                .expect_err("a different format is rejected")
                .to_string(),
            "snapshot checkpoint format is not recognized"
        );
    }

    #[test]
    fn a_recorded_job_keeps_its_draw_checkpoint_field_names() {
        let line = r#"{"event":"job","sequence":1,"reservation":1,"parent_id":0,"mutation_seed":9,
"execution_work":12,"result_sha256":"ef","decisions":[],"mixture_weight":128,"splice_weight":128,
"selector":{"path":"tiers","tier_rank":0},
"draw_checkpoint_before":{"records":3,"retained_successes":1,"table_sha256":"aa"},
"draw_checkpoint_after":{"records":4,"retained_successes":2,"table_sha256":"bb"}}"#
            .replace('\n', "");
        let record: CampaignStreamRecord<EmpiricalStepCheckpoint> =
            serde_json::from_str(&line).expect("record parses");
        let CampaignStreamRecord::Job(job) = record else {
            panic!("expected a job record");
        };
        assert_eq!(
            job.splice, None,
            "jobs without a splice carry no splice evidence"
        );
        assert_eq!(job.selector.path, SelectorPath::Tiers);
        assert_eq!(
            job.draw_checkpoint_before,
            Some(EmpiricalStepCheckpoint {
                records: 3,
                retained_successes: 1,
                table_sha256: "aa".to_owned(),
            })
        );
        let written = serde_json::to_value(&job).expect("job serializes");
        let object = written.as_object().expect("job is an object");
        let recorded: serde_json::Value = serde_json::from_str(&line).expect("record parses");
        let recorded_fields: BTreeSet<&String> = recorded
            .as_object()
            .expect("recorded job is an object")
            .keys()
            .filter(|field| field.as_str() != "event")
            .collect();
        assert_eq!(object.keys().collect::<BTreeSet<_>>(), recorded_fields);
        let round_trip: CampaignJobRecord<EmpiricalStepCheckpoint> =
            serde_json::from_value(written).expect("job round-trips");
        assert_eq!(round_trip, job);
        let _ = SelectorDraw {
            path: SelectorPath::Tiers,
            tier_rank: Some(0),
            best_preference: None,
        };
    }

    #[test]
    fn a_job_records_its_dispatch_time_splice_resolution() {
        let line = r#"{"event":"job","sequence":1,"reservation":1,"worker":0,"parent_id":3,"mutation_seed":9,
"execution_work":12,"result_sha256":"ef","decisions":[],"mixture_weight":85,"splice_weight":85,
"splice":{"outcome":"tail","donor_id":4,"leaf_id":9,"tail_postcard":[1,2]},
"selector":{"path":"tiers","tier_rank":1}}"#
            .replace('\n', "");
        let record: CampaignStreamRecord<EmpiricalStepCheckpoint> =
            serde_json::from_str(&line).expect("record parses");
        let CampaignStreamRecord::Job(job) = record else {
            panic!("expected a job record");
        };
        assert_eq!(
            job.splice,
            Some(super::CampaignSpliceRecord::Tail {
                donor_id: 4,
                leaf_id: 9,
                tail_postcard: vec![1, 2],
            })
        );
        let written = serde_json::to_vec(&job).expect("job serializes");
        let round_trip: CampaignJobRecord<EmpiricalStepCheckpoint> =
            serde_json::from_slice(&written).expect("job round-trips");
        assert_eq!(round_trip, job);
    }
    #[test]
    fn a_key_without_a_preference_runs_and_replays_with_no_continuation_bank() {
        let config = CampaignConfig {
            campaign_seed: 31,
            workers: 2,
            execution_budget: 200,
            host: "test".to_owned(),
            wall_budget: None,
            stop_rollout_on_objective: false,
            stop_campaign_on_objective: false,
            archive_entry_limit: 32,
            window: 4,
            memory_budget_mib: Some(8),
            materialize_final_artifacts: true,
            run: (),
            mixture: DrawMixture::EnergySplice { scale: 6 },
            retention: RetentionPolicy::Unprobed,
            objective_witness_path: None,
        };
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut stream = Vec::new();
        let mut progress = Vec::new();
        let live = run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            Some(&mut progress),
        )
        .expect("campaign without a preference");
        let text = std::str::from_utf8(&stream).expect("stream is utf-8");
        assert!(!text.contains("\"path\":\"continuation\""));
        assert!(!text.contains("continuation_energy"));
        let progress = String::from_utf8(progress).expect("progress is utf-8");
        let last: CampaignProgressRecord<serde_json::Value> =
            serde_json::from_str(progress.lines().last().expect("a progress line"))
                .expect("progress record parses");
        assert_eq!(last.continuations, ContinuationAccounting::default());
        assert_eq!(
            replay_campaign_checkpointed(&workload, &stream, None, None)
                .expect("replay without a preference"),
            live
        );
    }

    #[test]
    fn arena_tokens_reject_reuse_and_keep_pending_metadata_in_place() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct Pending {
            drops: Arc<AtomicUsize>,
            value: u64,
        }
        impl Drop for Pending {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::SeqCst);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let mut slots = super::ReservationSlots::new(|_: &u64| (1, 0), 1);
        assert!(
            slots
                .insert(
                    100,
                    Pending {
                        drops: Arc::clone(&drops),
                        value: 9
                    }
                )
                .is_none()
        );
        let first = slots.token(&100).unwrap();
        let address = std::ptr::from_ref(&slots.slot(first).unwrap().pending);
        slots
            .complete_token(first, 100, 7, None, |_| Ok(()))
            .unwrap();
        assert_eq!(slots.begin_admission(first), Some(7));
        assert_eq!(
            std::ptr::from_ref(&slots.slot(first).unwrap().pending),
            address
        );
        assert_eq!(slots.slot(first).unwrap().pending.value, 9);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        slots.finish_admission(first).unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(
            slots
                .insert(
                    usize::MAX,
                    Pending {
                        drops: Arc::clone(&drops),
                        value: 10
                    }
                )
                .is_none()
        );
        let second = slots.token(&usize::MAX).unwrap();
        assert_eq!(first.slot, second.slot);
        assert_ne!(first.generation, second.generation);
        assert_eq!(slots.slots.len(), 1);
        assert!(
            slots
                .validate_pending(first, usize::MAX)
                .unwrap_err()
                .contains("stale")
        );
        assert!(
            slots
                .validate_pending(second, 100)
                .unwrap_err()
                .contains("another")
        );
        assert_eq!(slots.pending, 1);
        slots
            .complete_token(second, usize::MAX, 8, None, |_| Ok(()))
            .unwrap();
        slots.begin_admission(second).unwrap();
        slots.finish_admission(second).unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn arena_generation_overflow_fails_without_reusing_or_dropping_a_live_slot() {
        let mut slots = super::ReservationSlots::new(|_: &u64| (0, 0), 1);
        slots.insert(2, 5);
        slots.slots[0].generation = u64::MAX;
        let token = super::ReservationToken {
            slot: 0,
            generation: u64::MAX,
        };
        slots.entries.map.insert(2, token);
        slots.complete_token(token, 2, 7, None, |_| Ok(())).unwrap();
        assert_eq!(slots.begin_admission(token), Some(7));
        assert_eq!(
            slots.finish_admission(token),
            Err("campaign reservation generation overflow")
        );
        assert!(slots.free.is_empty());
        assert_eq!(slots.slot(token).unwrap().pending, 5);
    }

    #[test]
    fn arena_window_reuse_and_explicit_stepped_growth_depend_on_live_count_not_identifiers() {
        let mut slots = super::ReservationSlots::new(|_: &u64| (1, 1), 2);
        for id in 100_usize..10_100 {
            slots.insert(id, id);
            let token = slots.token(&id).unwrap();
            slots
                .complete_token(token, id, id as u64, None, |_| Ok(()))
                .unwrap();
            slots.begin_admission(token).unwrap();
            slots.finish_admission(token).unwrap();
            assert_eq!(slots.slots.len(), 2);
        }
        for id in [usize::MAX, 9, 1] {
            assert!(slots.insert(id, id).is_none());
            let token = slots.token(&id).unwrap();
            slots
                .complete_token(token, id, id as u64, None, |_| Ok(()))
                .unwrap();
        }
        assert_eq!(slots.slots.len(), 3);
        assert_eq!(slots.pending, 0);
        assert_eq!(slots.completed_len(), 3);
        assert_eq!(slots.telemetry.results_peak, 3);
    }

    #[test]
    fn arena_checkpoint_schedule_validates_bijection_order_and_reused_generations() {
        let mut slots = super::ReservationSlots::new(|_: &u64| (0, 0), 2);
        slots.insert(99, 2_u64);
        let a = slots.token(&99).unwrap();
        slots.insert(1, 1_u64);
        let b = slots.token(&1).unwrap();
        let mut schedule = super::AdmissionSchedule::try_new(2).unwrap();
        schedule.insert((2, 99), a);
        schedule.insert((1, 1), b);
        assert_eq!(schedule.first(), Some(&(1, 1)));
        assert_eq!(schedule.validate(&slots, |finish| *finish), Ok(()));
        schedule.heap.clear();
        schedule.insert((2, 99), a);
        schedule.insert((2, 99), a);
        assert!(schedule.validate(&slots, |finish| *finish).is_err());
        schedule.heap.clear();
        schedule.insert((2, 99), a);
        schedule.insert((2, 1), b);
        assert!(schedule.validate(&slots, |finish| *finish).is_err());
        schedule.heap.clear();
        schedule.insert((2, 99), a);
        schedule.insert((1, 1), super::ReservationToken { generation: 1, ..b });
        assert!(schedule.validate(&slots, |finish| *finish).is_err());
    }

    #[test]
    fn arena_impossible_initial_capacity_returns_a_normal_error() {
        assert!(
            super::ReservationSlots::<usize, u64, u64>::try_new(|_| (0, 0), usize::MAX).is_err()
        );
        assert!(super::AdmissionSchedule::try_new(usize::MAX).is_err());
        let mut config = three_worker_config();
        config.window = usize::MAX;
        config.execution_budget = u64::MAX;
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut stream = Vec::new();
        assert!(
            CampaignCoordinator::start(
                &workload,
                &config,
                &CampaignOrigin::Genesis,
                &mut stream,
                None,
                CampaignExecutionOptions::default()
            )
            .is_err()
        );
    }

    #[test]
    fn native_resume_preserves_a_stepped_checkpoint_with_more_live_jobs_than_window() {
        let directory = std::env::temp_dir().join(format!(
            "reservation-arena-stepped-overflow-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let mut config = three_worker_config();
        config.window = 2;
        config.execution_budget = 20;
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let options = CampaignExecutionOptions {
            checkpoints: Some(crate::search::checkpoint::CheckpointPlan {
                directory: directory.clone(),
                every: std::num::NonZeroU64::new(1),
                on_marks: false,
                on_top_progress: false,
                checkpoint_owner: None,
                max_image_bytes: 64 << 20,
                before_write: None,
            }),
            ..Default::default()
        };
        {
            let mut initial_stream = Vec::new();
            let mut coordinator = CampaignCoordinator::start(
                &workload,
                &config,
                &CampaignOrigin::Genesis,
                &mut initial_stream,
                None,
                options,
            )
            .unwrap();
            while coordinator.reserve_next().unwrap() {}
            let mut jobs = coordinator.plan(usize::MAX);
            let first = jobs.remove(0);
            let held = jobs.remove(0);
            let mut target = workload.new_target().unwrap();
            coordinator
                .admit(run_job(&workload, &config, &mut target, &held).unwrap())
                .unwrap();
            assert_eq!(coordinator.executions(), 0);
            assert!(coordinator.reserve_next().unwrap());
            coordinator
                .admit(run_job(&workload, &config, &mut target, &first).unwrap())
                .unwrap();
            assert_eq!(coordinator.executions(), 2);
            assert_eq!(coordinator.reservation_capacity(), 3);
        }
        let origin = CampaignOrigin::SearchCheckpoint {
            path: directory.join("000000000002-interval.ckpt"),
            expected_sha256: None,
        };
        struct Watch;
        impl super::CampaignWatch<TestWorkload> for Watch {
            type Note = usize;
            fn run(
                &self,
                job: &JobSpec<TestWorkload>,
                run: impl FnOnce() -> Result<JobOutcome<TestWorkload>, Box<dyn std::error::Error>>,
            ) -> super::WatchedJob<TestWorkload, usize> {
                Ok((run()?, job.reservation))
            }
            fn start(
                &self,
                campaign: &mut CampaignCoordinator<'_, TestWorkload>,
            ) -> Result<(), Box<dyn std::error::Error>> {
                assert_eq!(campaign.reservation_capacity(), 3);
                campaign.watch_selected_admissions();
                Ok(())
            }
            fn prepare_admission(
                &self,
                note: &usize,
                campaign: &mut CampaignCoordinator<'_, TestWorkload>,
            ) {
                assert!(campaign.request_admission(*note));
            }
            fn admitted(&self, _: usize, campaign: &mut CampaignCoordinator<'_, TestWorkload>) {
                assert_eq!(campaign.reservation_capacity(), 3);
                campaign.take_admissions();
            }
        }
        let mut native_stream = Vec::new();
        let native = super::run_campaign(
            &workload,
            &config,
            &origin,
            &mut native_stream,
            None,
            Default::default(),
            &Watch,
            None,
        )
        .unwrap();
        let mut stepped_stream = Vec::new();
        let mut coordinator = CampaignCoordinator::start(
            &workload,
            &config,
            &origin,
            &mut stepped_stream,
            None,
            Default::default(),
        )
        .unwrap();
        while coordinator.reserve_next().unwrap() {}
        let mut target = workload.new_target().unwrap();
        while !coordinator.finished() {
            let jobs = coordinator.plan(2);
            assert!(!jobs.is_empty());
            for job in jobs.into_iter().rev() {
                coordinator
                    .admit(run_job(&workload, &config, &mut target, &job).unwrap())
                    .unwrap();
            }
        }
        let stepped = coordinator.finish(Vec::new()).unwrap();
        assert_eq!(native_stream, stepped_stream);
        assert_eq!(native, stepped);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn native_reply_token_validates_the_full_logical_key_before_admission() {
        let config = three_worker_config();
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut stream = Vec::new();
        let mut coordinator = CampaignCoordinator::start(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
            CampaignExecutionOptions::default(),
        )
        .unwrap();
        coordinator.reserve_next().unwrap();
        let job = coordinator.plan(1).pop().unwrap();
        let token = coordinator.reservation_token(job.reservation).unwrap();
        assert!(
            coordinator
                .validate_reply_token(token, job.reservation, job.planned_finish + 1)
                .unwrap_err()
                .to_string()
                .contains("planned finish")
        );
        assert!(
            coordinator
                .validate_reply_token(token, job.reservation + 1, job.planned_finish)
                .unwrap_err()
                .to_string()
                .contains("another reservation")
        );
        assert!(
            coordinator
                .validate_reply_token(
                    super::ReservationToken {
                        generation: token.generation + 1,
                        ..token
                    },
                    job.reservation,
                    job.planned_finish
                )
                .unwrap_err()
                .to_string()
                .contains("stale")
        );
        assert!(
            coordinator
                .validate_reply_token(token, job.reservation, job.planned_finish)
                .is_ok()
        );
        assert_eq!(coordinator.executions(), 0);
    }

    #[test]
    fn native_invalid_inner_reservation_never_reaches_the_observer_or_admission_watch() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Invalid {
            admitted: AtomicUsize,
        }
        impl super::CampaignWatch<TestWorkload> for Invalid {
            type Note = ();
            fn run(
                &self,
                _: &JobSpec<TestWorkload>,
                execute: impl FnOnce() -> Result<JobOutcome<TestWorkload>, Box<dyn std::error::Error>>,
            ) -> Result<(JobOutcome<TestWorkload>, ()), Box<dyn std::error::Error>> {
                let mut outcome = execute()?;
                outcome.reservation = usize::MAX;
                Ok((outcome, ()))
            }
            fn admitted(&self, _: (), _: &mut CampaignCoordinator<'_, TestWorkload>) {
                self.admitted.fetch_add(1, Ordering::SeqCst);
            }
        }
        let mut config = three_worker_config();
        config.execution_budget = 6;
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let watch = Invalid {
            admitted: AtomicUsize::new(0),
        };
        let mut observed = 0;
        let mut sink = |_| {
            observed += 1;
            Ok(())
        };
        let mut stream = Vec::new();
        let result = super::run_campaign(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
            CampaignExecutionOptions::default(),
            &watch,
            Some(crate::search::job_observation::JobObserver {
                diagnostics: Default::default(),
                sink: &mut sink,
            }),
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("another reservation")
        );
        assert_eq!(observed, 0);
        assert_eq!(watch.admitted.load(Ordering::SeqCst), 0);
        assert!(job_lines(&stream).is_empty());
    }

    #[test]
    fn reservation_slots_count_owned_records_and_balance_removal() {
        let result = |flags: &[bool]| {
            Ok((
                CampaignJobResult::<TestWorkload> {
                    preparation_failure: None,
                    actions: flags
                        .iter()
                        .map(|&candidate| CampaignActionResult::<TestWorkload> {
                            action: TestAction::new(1, 1),
                            observations: Vec::new(),
                            milestones: (),
                            outcome: Outcome::default(),
                            candidate: candidate.then_some(CampaignCandidate::<TestWorkload> {
                                key: TestKey(1),
                                viable: false,
                                snapshot: 1,
                            }),
                        })
                        .collect(),
                },
                0,
                String::new(),
            ))
        };
        let mut slots =
            super::ReservationSlots::new(super::completed_result_records::<TestWorkload>, 3);
        for key in 2..=4 {
            assert!(slots.insert(key, key * 10).is_none());
        }
        assert_eq!(slots.pending, 3);
        assert!(slots.take_completed(2).is_none());
        slots
            .complete(2, result(&[true, false, true]), |_| Ok(()))
            .unwrap();
        slots
            .complete(3, Err("failed worker result".into()), |_| Ok(()))
            .unwrap();
        slots.complete(4, result(&[]), |_| Ok(())).unwrap();
        assert_eq!(slots.pending, 0);
        assert_eq!(slots.metadata(&2), Some(&20));
        assert!(slots.pending(&2).is_none());
        assert_eq!(slots.telemetry.results_live, 3);
        assert_eq!(slots.telemetry.actions_live, 3);
        assert_eq!(slots.telemetry.candidates_live, 2);
        let before = slots.telemetry;
        assert_eq!(
            slots.complete(2, result(&[true]), |_| Ok(())),
            Err("campaign worker completed one reservation twice")
        );
        assert_eq!(
            slots.complete(99, result(&[]), |_| Ok(())),
            Err("campaign worker replied for an unknown reservation")
        );
        assert_eq!(slots.insert(2, 999), Some(999));
        assert_eq!(slots.metadata(&2), Some(&20));
        assert_eq!(slots.telemetry, before);
        assert!(slots.take_completed(99).is_none());
        let (pending, outcome) = slots.take_completed(2).unwrap();
        assert_eq!(pending, 20);
        assert!(outcome.is_ok());
        assert_eq!(slots.telemetry.results_live, 2);
        assert_eq!(slots.telemetry.actions_live, 0);
        assert_eq!(slots.telemetry.candidates_live, 0);
        assert!(slots.take_completed(3).unwrap().1.is_err());
        slots.take_completed(4).unwrap().1.unwrap();
        assert!(slots.is_empty());
        assert_eq!(slots.telemetry.results_live, 0);
        assert_eq!(slots.telemetry.results_peak, 3);
        assert_eq!(slots.telemetry.actions_peak, 3);
        assert_eq!(slots.telemetry.candidates_peak, 2);
    }

    #[test]
    fn native_campaign_counts_the_actual_post_watch_result() {
        struct MutatingWatch<'a> {
            workload: &'a TestWorkload,
            recorded: Mutex<Option<(super::CompletedRecords, std::thread::ThreadId)>>,
        }
        impl super::CampaignWatch<TestWorkload> for MutatingWatch<'_> {
            type Note = ();
            fn run(
                &self,
                _job: &JobSpec<TestWorkload>,
                run: impl FnOnce() -> Result<JobOutcome<TestWorkload>, Box<dyn Error>>,
            ) -> super::WatchedJob<TestWorkload, Self::Note> {
                let mut outcome = run()?;
                let before = super::CompletedRecords::of_result::<TestWorkload>(&outcome.result);
                *self.recorded.lock().unwrap() = Some((before, std::thread::current().id()));
                outcome.result.actions.extend([
                    CampaignActionResult::<TestWorkload> {
                        action: TestAction::new(2, 1),
                        observations: Vec::new(),
                        milestones: (),
                        outcome: Outcome::default(),
                        candidate: Some(CampaignCandidate::<TestWorkload> {
                            key: TestKey(0),
                            viable: false,
                            snapshot: 0,
                        }),
                    },
                    CampaignActionResult::<TestWorkload> {
                        action: TestAction::new(3, 1),
                        observations: Vec::new(),
                        milestones: (),
                        outcome: Outcome::default(),
                        candidate: None,
                    },
                ]);
                outcome.result_sha256 = self.workload.result_sha256(&outcome.result)?;
                Ok((outcome, ()))
            }
            fn admitted(&self, _: Self::Note, _: &mut CampaignCoordinator<'_, TestWorkload>) {}
        }
        let coordinator = std::thread::current().id();
        let mut config = three_worker_config();
        config.workers = 1;
        config.window = 1;
        config.execution_budget = 1;
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let watch = MutatingWatch {
            workload: &workload,
            recorded: Mutex::new(None),
        };
        let mut stream = Vec::new();
        let (report, _) = super::run_campaign_watched(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
            CampaignExecutionOptions::default(),
            &watch,
        )
        .unwrap();
        let (before, worker) = watch.recorded.lock().unwrap().unwrap();
        assert_ne!(worker, coordinator);
        let telemetry = report.telemetry.completed_buffer;
        assert_eq!(telemetry.results_peak, 1);
        assert_eq!(telemetry.actions_peak, before.actions + 2);
        assert_eq!(telemetry.candidates_peak, before.candidates + 1);
        assert_eq!(telemetry.results_live, 0);
        assert_eq!(telemetry.actions_live, 0);
        assert_eq!(telemetry.candidates_live, 0);
        let failure: Result<(JobOutcome<TestWorkload>, ()), String> = Err("worker failed".into());
        assert_eq!(
            super::CompletedRecords::after_watch::<TestWorkload, _>(&failure),
            super::CompletedRecords::default()
        );
        let preparation = Ok((
            JobOutcome::<TestWorkload> {
                reservation: 0,
                result: CampaignJobResult::<TestWorkload> {
                    preparation_failure: Some(vec![()]),
                    actions: Vec::new(),
                },
                execution_work: 0,
                result_sha256: String::new(),
            },
            (),
        ));
        assert_eq!(
            super::CompletedRecords::after_watch::<TestWorkload, _>(&preparation),
            super::CompletedRecords::default()
        );
    }

    #[test]
    fn counted_completions_validate_and_cache_without_scanning_on_admission_or_removal() {
        let mut slots = super::ReservationSlots::new(
            |_: &u64| panic!("coordinator scanned a counted result"),
            1,
        );
        slots.insert(7, "queued");
        let counts = Some(super::CompletedRecords {
            actions: 5,
            candidates: 2,
        });
        let before = slots.telemetry;
        assert_eq!(
            slots.complete_with_records(8, 1, counts, |_| panic!("unknown validation")),
            Err("campaign worker replied for an unknown reservation")
        );
        assert_eq!(
            slots.complete_with_records(7, 1, counts, |_| Err("never planned")),
            Err("never planned")
        );
        assert_eq!(slots.telemetry, before);
        assert_eq!(slots.pending, 1);
        slots
            .complete_with_records(7, 1, counts, |_| Ok(()))
            .unwrap();
        let completed = slots.telemetry;
        assert_eq!(completed.actions_live, 5);
        assert_eq!(completed.candidates_live, 2);
        assert_eq!(
            slots.complete_with_records(
                7,
                9,
                Some(super::CompletedRecords::default()),
                |_| panic!("duplicate validation")
            ),
            Err("campaign worker completed one reservation twice")
        );
        assert_eq!(slots.telemetry, completed);
        assert_eq!(slots.pending, 0);
        assert_eq!(slots.take_completed(7), Some(("queued", 1)));
        assert_eq!(slots.take_completed(7), None);
        assert_eq!(slots.telemetry.results_live, 0);
        assert_eq!(slots.telemetry.actions_live, 0);
        assert_eq!(slots.telemetry.candidates_live, 0);
        assert_eq!(slots.telemetry.actions_peak, 5);
        assert_eq!(slots.telemetry.candidates_peak, 2);
    }

    #[test]
    fn worker_and_public_counts_preserve_reordered_results_and_buffer_gauges() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let run = |worker_counted: bool| {
            let scans = Arc::new(AtomicUsize::new(0));
            let mut slots = super::ReservationSlots::new(
                |value: &(Arc<AtomicUsize>, u64)| {
                    value.0.fetch_add(1, Ordering::Relaxed);
                    (value.1, u64::from(value.1 > 0))
                },
                3,
            );
            for reservation in 0..3 {
                slots.insert(reservation, reservation + 10);
            }
            let mut next = 0;
            let mut admitted = Vec::new();
            for reservation in [2, 0, 1] {
                let counts = worker_counted.then_some(super::CompletedRecords {
                    actions: reservation,
                    candidates: u64::from(reservation > 0),
                });
                slots
                    .complete_with_records(
                        reservation,
                        (Arc::clone(&scans), reservation),
                        counts,
                        |_| Ok(()),
                    )
                    .unwrap();
                while let Some((pending, (_, value))) = slots.take_completed(next) {
                    admitted.push((next, pending, value));
                    next += 1;
                }
            }
            assert!(slots.is_empty());
            (admitted, slots.telemetry, scans.load(Ordering::Relaxed))
        };
        let counted = run(true);
        let public = run(false);
        assert_eq!(counted.0, public.0);
        assert_eq!(counted.1, public.1);
        assert_eq!(counted.2, 0);
        assert_eq!(public.2, 3);
    }

    #[test]
    fn reservation_slots_validate_before_scanning_and_count_each_result_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let scans = Arc::new(AtomicUsize::new(0));
        let mut slots = super::ReservationSlots::new(
            |value: &(Arc<AtomicUsize>, (u64, u64))| {
                value.0.fetch_add(1, Ordering::Relaxed);
                value.1
            },
            2,
        );
        assert!(slots.insert(7, "queued").is_none());
        assert!(slots.insert(9, "running").is_none());
        assert_eq!(
            slots.complete(7, (Arc::clone(&scans), (5, 2)), |_| Err("never planned")),
            Err("never planned")
        );
        assert_eq!(slots.pending, 2);
        assert_eq!(scans.load(Ordering::Relaxed), 0);
        slots
            .complete(7, (Arc::clone(&scans), (5, 2)), |_| Ok(()))
            .unwrap();
        slots
            .complete(9, (Arc::clone(&scans), (3, 1)), |_| Ok(()))
            .unwrap();
        assert!(
            slots
                .complete(7, (Arc::clone(&scans), (8, 4)), |_| panic!(
                    "duplicate validation"
                ))
                .is_err()
        );
        assert_eq!(scans.load(Ordering::Relaxed), 2);
        assert_eq!(slots.take_completed(9).unwrap().1.1, (3, 1));
        assert_eq!(slots.take_completed(7).unwrap().1.1, (5, 2));
        assert!(slots.take_completed(7).is_none());
        assert_eq!(scans.load(Ordering::Relaxed), 2);
        assert_eq!(slots.telemetry.results_live, 0);
        assert_eq!(slots.telemetry.actions_live, 0);
        assert_eq!(slots.telemetry.candidates_live, 0);
        assert_eq!(slots.telemetry.actions_peak, 8);
        assert_eq!(slots.telemetry.candidates_peak, 3);
    }

    #[test]
    fn reservation_slots_reordering_changes_only_occupancy_not_admission_content() {
        use std::hash::BuildHasher;
        let first = std::collections::hash_map::RandomState::new();
        let second = std::collections::hash_map::RandomState::new();
        assert_ne!(first.hash_one(23_u64), second.hash_one(23_u64));
        let run = |order: &[u64], state: std::collections::hash_map::RandomState| {
            let mut slots =
                super::ReservationSlots::new(|value: &u64| (*value, u64::from(*value > 0)), 3);
            slots.entries = super::ReservationLookup::with_capacity_and_hasher(3, state);
            for reservation in 0..3 {
                slots.insert(reservation, reservation + 10);
            }
            let mut next = 0;
            let mut admitted = Vec::new();
            for &reservation in order {
                slots
                    .complete(reservation, reservation, |_| Ok(()))
                    .unwrap();
                while let Some((pending, value)) = slots.take_completed(next) {
                    admitted.push((next, pending, value));
                    next += 1;
                }
            }
            assert!(slots.is_empty());
            assert_eq!(slots.pending, 0);
            assert_eq!(slots.telemetry.results_live, 0);
            assert_eq!(slots.telemetry.actions_live, 0);
            assert_eq!(slots.telemetry.candidates_live, 0);
            (admitted, slots.telemetry)
        };
        let ordered = run(&[0, 1, 2], first);
        let reordered = run(&[2, 1, 0], second);
        assert_eq!(ordered.0, reordered.0);
        assert_eq!(ordered.1.results_peak, 1);
        assert_eq!(reordered.1.results_peak, 3);
        assert_eq!(reordered.1.actions_peak, 3);
        assert_eq!(reordered.1.candidates_peak, 2);
    }

    #[test]
    fn the_report_carries_worker_telemetry_that_replay_equality_ignores() {
        let config = three_worker_config();
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut stream = Vec::new();
        let live = run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .expect("campaign");
        let telemetry = &live.0.telemetry;
        assert_eq!(telemetry.workers.len(), 3);
        assert_eq!(
            telemetry
                .workers
                .iter()
                .map(|worker| worker.jobs)
                .sum::<u64>(),
            live.0.executions_completed
        );
        assert_eq!(
            telemetry.target["work"],
            live.0.execution_work - live.0.bootstrap_execution_work
        );
        assert!(telemetry.search_ns > 0);
        assert!(telemetry.completed_buffer.results_peak > 0);
        assert!(telemetry.completed_buffer.actions_peak > 0);
        assert!(telemetry.completed_buffer.candidates_peak > 0);
        assert_eq!(telemetry.completed_buffer.results_live, 0);
        assert_eq!(telemetry.completed_buffer.actions_live, 0);
        assert_eq!(telemetry.completed_buffer.candidates_live, 0);
        let serialized = serde_json::to_value(telemetry).unwrap();
        assert_eq!(
            serialized["completed_buffer"]["results_peak"],
            telemetry.completed_buffer.results_peak
        );
        assert!(telemetry.coordinator["admissions"].as_u64() == Some(live.0.executions_completed));
        let replayed =
            replay_campaign_checkpointed(&workload, &stream, None, None).expect("replay");
        assert!(replayed.0.telemetry.workers.is_empty());
        assert_eq!(
            serde_json::to_vec(&replayed.0).unwrap(),
            serde_json::to_vec(&live.0).unwrap()
        );
        assert_eq!(replayed, live);
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ResidencyCase {
        Initial,
        Refill,
        BusyOwner,
    }

    #[derive(Debug, Default)]
    struct ResidencyState {
        first_admitted: bool,
        starts: Vec<(usize, u32)>,
        restores: Vec<(u32, u8, bool)>,
        dispatches: Vec<(usize, bool, Vec<u32>)>,
    }

    struct ResidencyFixture {
        case: ResidencyCase,
        hints: bool,
        state: Mutex<ResidencyState>,
        changed: std::sync::Condvar,
    }

    impl ResidencyFixture {
        #[allow(clippy::disallowed_methods)]
        fn wait_for(&self, ready: impl Fn(&ResidencyState) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut state = self.state.lock().unwrap();
            while !ready(&state) {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "residency fixture stalled: {state:?}");
                state = self.changed.wait_timeout(state, remaining).unwrap().0;
            }
        }
    }

    std::thread_local! {
        static LOCALITY_WORKER: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
        static LOCALITY_DISPATCH: std::cell::RefCell<Option<Arc<ResidencyFixture>>> = const { std::cell::RefCell::new(None) };
    }

    struct ResidencyScope {
        previous: Option<Arc<ResidencyFixture>>,
        worker: Option<u32>,
    }

    impl ResidencyScope {
        fn install(fixture: Arc<ResidencyFixture>) -> Self {
            Self {
                previous: LOCALITY_DISPATCH.with(|slot| slot.replace(Some(fixture))),
                worker: LOCALITY_WORKER.with(std::cell::Cell::get),
            }
        }
    }

    impl Drop for ResidencyScope {
        fn drop(&mut self) {
            LOCALITY_DISPATCH.with(|slot| slot.replace(self.previous.take()));
            LOCALITY_WORKER.with(|slot| slot.set(self.worker));
        }
    }

    pub(super) fn before_locality_dispatch<Key: Ord + Clone, Job, Output, O>(
        pool: &crate::search::parallel::WorkerPool<Key, Job, Output, O>,
        reservation: usize,
        initial: bool,
    ) -> Result<(), Box<dyn Error>> {
        let fixture = LOCALITY_DISPATCH.with(|slot| slot.borrow().clone());
        if let Some(fixture) = fixture {
            if fixture.case == ResidencyCase::Refill && !initial {
                fixture.state.lock().unwrap().first_admitted = true;
                fixture.changed.notify_all();
            }
            let idle = match (fixture.case, reservation) {
                (ResidencyCase::Refill, 0) => vec![1],
                (ResidencyCase::BusyOwner, 1) => {
                    fixture.wait_for(|state| !state.starts.is_empty());
                    let first = fixture.state.lock().unwrap().starts[0];
                    assert_eq!(first.0, 0);
                    vec![1 - first.1]
                }
                _ => vec![0, 1],
            };
            pool.wait_idle(&idle)?;
            fixture
                .state
                .lock()
                .unwrap()
                .dispatches
                .push((reservation, initial, idle));
        }
        Ok(())
    }

    impl super::CampaignWatch<TestWorkload> for ResidencyFixture {
        type Note = usize;

        fn run(
            &self,
            job: &JobSpec<TestWorkload>,
            run: impl FnOnce() -> Result<JobOutcome<TestWorkload>, Box<dyn Error>>,
        ) -> super::WatchedJob<TestWorkload, Self::Note> {
            let worker = LOCALITY_WORKER.with(std::cell::Cell::get).unwrap();
            self.state
                .lock()
                .unwrap()
                .starts
                .push((job.reservation, worker));
            self.changed.notify_all();
            if self.case == ResidencyCase::BusyOwner && job.reservation == 0 {
                self.wait_for(|state| {
                    state
                        .starts
                        .iter()
                        .any(|(reservation, _)| *reservation == 1)
                });
            }
            Ok((run()?, job.reservation))
        }

        fn admitted(&self, note: usize, _campaign: &mut CampaignCoordinator<'_, TestWorkload>) {
            if note == 0 {
                self.state.lock().unwrap().first_admitted = true;
                self.changed.notify_all();
            }
        }
    }

    #[test]
    fn campaign_locality_avoids_reconstruction_in_both_drains_and_falls_back_when_owner_busy() {
        let mut reconstructions = Vec::new();
        let mut assignments = Vec::new();
        for case in [
            ResidencyCase::Initial,
            ResidencyCase::Refill,
            ResidencyCase::BusyOwner,
        ] {
            let mut reference = None;
            for hints in [false, true] {
                let fixture = Arc::new(ResidencyFixture {
                    case,
                    hints,
                    state: Mutex::new(ResidencyState::default()),
                    changed: std::sync::Condvar::new(),
                });
                let _scope = ResidencyScope::install(Arc::clone(&fixture));
                let workload = TestWorkload {
                    bootstrap_objective: false,
                    targets: Some(Arc::new(LiveTargets {
                        locality: true,
                        residency: Some(Arc::clone(&fixture)),
                        ..LiveTargets::default()
                    })),
                };
                let mut config = three_worker_config();
                config.workers = 2;
                config.window = if case == ResidencyCase::BusyOwner {
                    2
                } else {
                    1
                };
                config.execution_budget = if case == ResidencyCase::Initial { 1 } else { 2 };
                config.materialize_final_artifacts = false;
                let mut stream = Vec::new();
                let outcome = super::run_campaign_watched(
                    &workload,
                    &config,
                    &CampaignOrigin::Genesis,
                    &mut stream,
                    None,
                    CampaignExecutionOptions {
                        placement: Some(ThreadPlacement::new(|thread| {
                            LOCALITY_WORKER.with(|slot| {
                                slot.set(match thread {
                                    PlacedThread::Worker(worker) => Some(worker),
                                    PlacedThread::Coordinator | PlacedThread::StreamEncoder => None,
                                })
                            });
                            Ok(())
                        })),
                        ..CampaignExecutionOptions::default()
                    },
                    fixture.as_ref(),
                )
                .unwrap();
                assert!(outcome.1.entries.is_empty());
                let logical = (
                    stream,
                    serde_json::to_vec(&outcome.0).unwrap(),
                    outcome.1.to_bytes().unwrap(),
                );
                if let Some(expected) = &reference {
                    assert_eq!(&logical, expected, "case={case:?}");
                } else {
                    reference = Some(logical);
                }
                let state = fixture.state.lock().unwrap();
                assert_eq!(state.restores.len(), config.execution_budget as usize);
                assert_eq!(state.dispatches.len(), config.execution_budget as usize);
                assert!(state.dispatches[0].1);
                if case == ResidencyCase::Refill {
                    assert_eq!(state.dispatches[0].2, [1]);
                    assert!(!state.dispatches[1].1);
                    assert_eq!(state.dispatches[1].2, [0, 1]);
                }
                if case == ResidencyCase::Initial {
                    assert_eq!(state.dispatches[0].2, [0, 1]);
                }
                if case == ResidencyCase::BusyOwner {
                    assert!(state.dispatches[1].1);
                    assert_eq!(state.dispatches[1].2.len(), 1);
                }
                reconstructions.push(state.restores.iter().filter(|(_, _, cold)| *cold).count());
                let mut starts = state.starts.clone();
                starts.sort_unstable();
                assignments.push(starts);
            }
        }
        assert_eq!(reconstructions, [1, 0, 1, 0, 1, 1]);
        assert_eq!(
            assignments,
            [
                vec![(0, 0)],
                vec![(0, 1)],
                vec![(0, 1), (1, 0)],
                vec![(0, 1), (1, 1)],
                vec![(0, 0), (1, 1)],
                vec![(0, 1), (1, 0)],
            ]
        );
    }

    #[test]
    fn locality_hints_preserve_complete_logical_campaign_streams_across_worker_counts() {
        for seed in [5, 11, 19] {
            let mut reference = None;
            for workers in [1, 3] {
                for locality in [false, true] {
                    let targets = Arc::new(LiveTargets {
                        locality,
                        ..LiveTargets::default()
                    });
                    let workload = TestWorkload {
                        bootstrap_objective: false,
                        targets: Some(Arc::clone(&targets)),
                    };
                    let mut config = three_worker_config();
                    config.campaign_seed = seed;
                    config.workers = workers;
                    config.window = 6;
                    let mut stream = Vec::new();
                    let outcome = run_campaign_checkpointed(
                        &workload,
                        &config,
                        &CampaignOrigin::Genesis,
                        &mut stream,
                        None,
                    )
                    .unwrap();
                    if locality {
                        assert!(
                            targets
                                .locality_reads
                                .load(std::sync::atomic::Ordering::Relaxed)
                                > 0
                        );
                    }
                    let logical = (stream, serde_json::to_vec(&outcome.0).unwrap());
                    if let Some(expected) = &reference {
                        assert_eq!(
                            &logical, expected,
                            "seed={seed}, workers={workers}, locality={locality}"
                        );
                    } else {
                        reference = Some(logical);
                    }
                }
            }
        }
    }

    fn job_lines(stream: &[u8]) -> Vec<(usize, CampaignJobRecord<EmpiricalStepCheckpoint>)> {
        std::str::from_utf8(stream)
            .expect("stream is text")
            .lines()
            .enumerate()
            .skip(1)
            .filter_map(|(index, line)| {
                match serde_json::from_str::<CampaignStreamRecord<EmpiricalStepCheckpoint>>(line)
                    .expect("stream record parses")
                {
                    CampaignStreamRecord::Job(job) => Some((index, job)),
                    CampaignStreamRecord::Skip(_) => None,
                    CampaignStreamRecord::End(_) => None,
                }
            })
            .collect()
    }

    #[test]
    fn jobs_are_admitted_in_planned_finish_order_and_replay() {
        let mut config = three_worker_config();
        config.window = 6;
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut stream = Vec::new();
        let live = run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .expect("campaign");
        let reservations = job_lines(&stream)
            .into_iter()
            .map(|(_, job)| job.reservation)
            .collect::<Vec<_>>();
        let mut ordered = reservations.clone();
        ordered.sort_unstable();
        assert_eq!(
            ordered,
            (0..u64::try_from(reservations.len()).unwrap()).collect::<Vec<_>>()
        );
        assert_ne!(
            reservations, ordered,
            "a cheaper later reservation is admitted first"
        );
        let replayed =
            replay_campaign_checkpointed(&workload, &stream, None, None).expect("replay");
        assert_eq!(replayed, live);
    }

    #[test]
    fn replacement_dispatch_precedes_the_next_buffered_admission() {
        let mut config = three_worker_config();
        config.window = 6;
        config.execution_budget = 100;
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut stream = Vec::new();
        let mut coordinator = CampaignCoordinator::start(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
            CampaignExecutionOptions::default(),
        )
        .unwrap();
        while coordinator.reserve_next().unwrap() {}
        let mut target = workload.new_target().unwrap();
        let jobs = coordinator.plan(usize::MAX);
        assert_eq!(jobs.len(), config.window);
        let mut outcomes = jobs
            .iter()
            .map(|job| run_job(&workload, &config, &mut target, job).unwrap())
            .collect::<Vec<_>>();
        let head = outcomes.remove(0);
        let before = coordinator.executions();
        for outcome in outcomes.into_iter().rev() {
            coordinator.admit(outcome).unwrap();
        }
        assert_eq!(coordinator.executions(), before);
        let records = super::CompletedRecords::of_result::<TestWorkload>(&head.result);
        let token = coordinator.reservation_token(head.reservation).unwrap();
        let error = coordinator
            .admit_dispatch(head, records, token, &mut |_, _| {
                Err("dispatch witness".into())
            })
            .unwrap_err();
        assert_eq!(error.to_string(), "dispatch witness");
        assert_eq!(coordinator.executions(), before + 1);
    }

    #[test]
    fn invalid_worker_replies_fail_before_any_ordered_admission() {
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut config = three_worker_config();
        config.window = 6;
        config.execution_budget = 60;
        for case in 0..4 {
            let mut stream = Vec::new();
            {
                let mut coordinator = CampaignCoordinator::start(
                    &workload,
                    &config,
                    &CampaignOrigin::Genesis,
                    &mut stream,
                    None,
                    CampaignExecutionOptions::default(),
                )
                .unwrap();
                while coordinator.reserve_next().unwrap() {}
                let error = match case {
                    0 => {
                        coordinator.plan(usize::MAX);
                        coordinator
                            .fail(usize::MAX, "unknown worker".into())
                            .unwrap_err()
                    }
                    1 => {
                        let jobs = coordinator.plan(usize::MAX);
                        let head = coordinator.next_admission().unwrap();
                        let held = jobs
                            .iter()
                            .find(|job| job.reservation != head)
                            .unwrap()
                            .reservation;
                        coordinator.fail(held, "held failure".into()).unwrap();
                        assert_eq!(coordinator.executions(), 0);
                        assert!(coordinator.reserve_next().unwrap());
                        assert!(!coordinator.reserve_next().unwrap());
                        coordinator
                            .fail(held, "duplicate failure".into())
                            .unwrap_err()
                    }
                    2 => {
                        let planned = coordinator.plan(1).pop().unwrap().reservation;
                        let queued = (0..config.window)
                            .find(|&reservation| reservation != planned)
                            .unwrap();
                        coordinator
                            .fail(queued, "unplanned worker".into())
                            .unwrap_err()
                    }
                    _ => coordinator
                        .fail(0, "worker before plan".into())
                        .unwrap_err(),
                };
                let expected = match case {
                    0 => "campaign worker replied for an unknown reservation",
                    1 => "campaign worker completed one reservation twice",
                    2 => "campaign worker replied for a job that was never planned",
                    _ => "campaign worker replied without queued work",
                };
                assert_eq!(error.to_string(), expected);
                assert_eq!(coordinator.executions(), 0);
            }
            assert!(job_lines(&stream).is_empty());
            assert!(
                !std::str::from_utf8(&stream)
                    .unwrap()
                    .contains("\"record\":\"end\"")
            );
        }
    }

    #[test]
    fn a_single_threaded_caller_writes_the_same_campaign_as_the_worker_pool() {
        let mut config = three_worker_config();
        config.window = 6;
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut pooled_stream = Vec::new();
        let pooled = run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut pooled_stream,
            None,
        )
        .expect("pooled campaign");
        let mut stream = Vec::new();
        let mut coordinator = CampaignCoordinator::start(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
            CampaignExecutionOptions::default(),
        )
        .expect("coordinator");
        while coordinator.reserve_next().expect("reserve window") {}
        let mut target = workload.new_target().expect("target");
        while !coordinator.finished() {
            let jobs = coordinator.plan(2);
            assert!(!jobs.is_empty());
            for job in jobs.into_iter().rev() {
                let job: JobSpec<TestWorkload> =
                    postcard::from_bytes(&postcard::to_allocvec(&job).expect("job bytes"))
                        .expect("job");
                let outcome = run_job(&workload, &config, &mut target, &job).expect("job runs");
                let outcome: JobOutcome<TestWorkload> =
                    postcard::from_bytes(&postcard::to_allocvec(&outcome).expect("outcome bytes"))
                        .expect("outcome");
                coordinator.admit(outcome).expect("admission");
            }
        }
        assert_eq!(coordinator.executions(), config.execution_budget);
        let stepped = coordinator.finish(Vec::new()).expect("finish");
        assert_eq!(stream, pooled_stream);
        assert_eq!(stepped, pooled);
    }

    #[test]
    fn determinism_contradiction_invalidates_live_campaign_without_a_completed_report() {
        let targets = Arc::new(LiveTargets::default());
        let invalidate = Arc::clone(&targets);
        *targets.action_hook.lock().unwrap() = Some(Arc::new(move |_| {
            invalidate
                .integrity_invalidated
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }));
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: Some(Arc::clone(&targets)),
        };
        let mut stream = Vec::new();
        let result = run_campaign_checkpointed(
            &workload,
            &three_worker_config(),
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
        );
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("a disputed campaign must not return a report"),
        };
        assert!(
            error.downcast_ref::<TestIntegrityViolation>().is_some(),
            "{error}"
        );
        assert!(
            targets
                .integrity_invalidated
                .load(std::sync::atomic::Ordering::SeqCst)
        );
        assert!(job_lines(&stream).is_empty());
        assert!(
            !std::str::from_utf8(&stream)
                .unwrap()
                .lines()
                .skip(1)
                .any(|line| matches!(
                    serde_json::from_str::<CampaignStreamRecord<EmpiricalStepCheckpoint>>(line)
                        .unwrap(),
                    CampaignStreamRecord::End(_)
                ))
        );
        assert_eq!(targets.live.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn execution_failure_waits_for_earlier_planned_admission() {
        struct JobLineStream {
            bytes: Vec<u8>,
            first_job: mpsc::Sender<()>,
        }

        impl Write for JobLineStream {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                if self.bytes.iter().filter(|&&byte| byte == b'\n').count() >= 2 {
                    let _ = self.first_job.send(());
                }
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut config = three_worker_config();
        config.workers = 2;
        config.window = 2;
        config.execution_budget = 2;
        config.mixture = DrawMixture::Energy { scale: 6 };
        let (baseline, earlier, later) = (0..32)
            .find_map(|seed| {
                config.campaign_seed = seed;
                let mut baseline = Vec::new();
                run_campaign_checkpointed(
                    &TestWorkload {
                        bootstrap_objective: false,
                        targets: None,
                    },
                    &config,
                    &CampaignOrigin::Genesis,
                    &mut baseline,
                    None,
                )
                .ok()?;
                let jobs = job_lines(&baseline);
                (jobs.len() == 2
                    && jobs[0].0 == 1
                    && jobs[1].0 == 2
                    && jobs[0].1.mutation_seed as u8 != jobs[1].1.mutation_seed as u8)
                    .then(|| {
                        (
                            baseline,
                            (jobs[0].1.reservation, jobs[0].1.mutation_seed as u8),
                            (jobs[1].1.reservation, jobs[1].1.mutation_seed as u8),
                        )
                    })
            })
            .expect("two distinct jobs in planned-finish order");
        let expected_prefix = format!(
            "{}\n",
            std::str::from_utf8(&baseline)
                .unwrap()
                .lines()
                .take(2)
                .collect::<Vec<_>>()
                .join("\n")
        );
        let run_order = |held_input: u8, label: &str, panicking: bool| {
            let directory = std::env::temp_dir().join(format!(
                "dissonance-failure-prefix-{}-{label}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&directory);
            let (entered_sender, entered_receiver) = mpsc::channel();
            let (release_sender, release_receiver) = mpsc::channel();
            let (job_sender, job_receiver) = mpsc::channel();
            let release_receiver = Arc::new(Mutex::new(Some(release_receiver)));
            let targets = Arc::new(LiveTargets::default());
            *targets.action_hook.lock().unwrap() = Some(Arc::new(move |action| {
                if action.input == earlier.1 || action.input == later.1 {
                    entered_sender.send(action.input).unwrap();
                }
                if action.input == held_input
                    && let Some(release) = release_receiver.lock().unwrap().take()
                {
                    release.recv().map_err(|error| error.to_string())?;
                }
                if action.input == later.1 {
                    if panicking {
                        panic!("injected execution panic");
                    }
                    return Err("planned execution failure".to_owned());
                }
                Ok(())
            }));
            let (error, stream, observed, job_before_release) = std::thread::scope(|scope| {
                let directory_for_run = directory.clone();
                let run = scope.spawn(|| {
                    let mut stream = JobLineStream {
                        bytes: Vec::new(),
                        first_job: job_sender,
                    };
                    let error = run_campaign_checkpointed_with_options(
                        &TestWorkload {
                            bootstrap_objective: false,
                            targets: Some(targets),
                        },
                        &config,
                        &CampaignOrigin::Genesis,
                        &mut stream,
                        None,
                        CampaignExecutionOptions {
                            checkpoints: Some(crate::search::checkpoint::CheckpointPlan {
                                directory: directory_for_run,
                                every: std::num::NonZeroU64::new(1),
                                on_marks: false,
                                on_top_progress: false,
                                checkpoint_owner: None,
                                max_image_bytes: 64 << 20,
                                before_write: None,
                            }),
                            ..CampaignExecutionOptions::default()
                        },
                    )
                    .expect_err("later job fails")
                    .to_string();
                    (error, stream.bytes)
                });
                let mut observed = BTreeSet::new();
                while observed.len() < 2 {
                    let Ok(input) = entered_receiver.recv_timeout(Duration::from_secs(5)) else {
                        break;
                    };
                    observed.insert(input);
                }
                let job_before_release = held_input == later.1
                    && job_receiver.recv_timeout(Duration::from_secs(5)).is_ok();
                release_sender.send(()).unwrap();
                let (error, stream) = run.join().unwrap();
                (error, stream, observed, job_before_release)
            });
            assert_eq!(observed, BTreeSet::from([earlier.1, later.1]));
            if held_input == later.1 {
                assert!(
                    job_before_release,
                    "earlier job did not admit while failure waited"
                );
            }
            (error, stream, directory)
        };
        let (failure_first, first_stream, first_directory) =
            run_order(earlier.1, "failure-first", false);
        let (success_first, second_stream, second_directory) =
            run_order(later.1, "success-first", false);
        let expected_error = format!(
            "campaign reservation {} failed: planned execution failure",
            later.0
        );
        assert_eq!(failure_first, expected_error);
        assert_eq!(success_first, expected_error);
        assert_eq!(first_stream, second_stream);
        assert_eq!(std::str::from_utf8(&first_stream).unwrap(), expected_prefix);
        let checkpoint_path =
            |directory: &std::path::Path| directory.join("000000000001-interval.ckpt");
        let stable_checkpoint = |directory: &std::path::Path| {
            let mut reader =
                crate::search::checkpoint::CheckpointReader::open(&checkpoint_path(directory))
                    .expect("the earlier planned admission wrote a checkpoint");
            let header = reader.header.clone();
            assert_eq!(header.executions, 1);
            assert_eq!(header.admitted, 1);
            assert_eq!(header.reserved, 2);
            let core: super::CoreCheckpoint<TestWorkload> = reader.next().unwrap();
            let archive: crate::search::archive::Archive<TestAction, TestKey, (), u8> =
                reader.next().unwrap();
            let evidence: Vec<u8> = reader.next().unwrap();
            let draw: Vec<u8> = reader.next().unwrap();
            let durations: crate::search::duration::DurationPoliciesCheckpoint<TestKey> =
                reader.next().unwrap();
            let rand: RomuDuoJrRand = reader.next().unwrap();
            let counters: CampaignCounters = reader.next().unwrap();
            let _profile: LiveCoordinatorProfile = reader.next().unwrap();
            let in_flight: Vec<super::InFlightJob<TestWorkload>> = reader.next().unwrap();
            let snapshots = reader.snapshots::<u8>().unwrap();
            reader.finish().unwrap();
            let in_flight = in_flight
                .iter()
                .map(|job| super::InFlightJobRef {
                    reservation: job.reservation,
                    spec: &job.spec,
                    pending: &job.pending,
                })
                .collect::<Vec<_>>();
            [
                postcard::to_allocvec(&header).unwrap(),
                postcard::to_allocvec(&core).unwrap(),
                postcard::to_allocvec(&archive).unwrap(),
                postcard::to_allocvec(&evidence).unwrap(),
                postcard::to_allocvec(&draw).unwrap(),
                postcard::to_allocvec(&durations).unwrap(),
                postcard::to_allocvec(&rand).unwrap(),
                postcard::to_allocvec(&counters).unwrap(),
                postcard::to_allocvec(&in_flight).unwrap(),
                postcard::to_allocvec(&snapshots).unwrap(),
            ]
        };
        assert_eq!(
            stable_checkpoint(&first_directory),
            stable_checkpoint(&second_directory)
        );
        let resume = |directory: &std::path::Path| {
            FINALIZATION.with(|state| state.set((0, true)));
            let error = run_campaign_checkpointed(
                &TestWorkload {
                    bootstrap_objective: false,
                    targets: None,
                },
                &config,
                &CampaignOrigin::SearchCheckpoint {
                    path: checkpoint_path(directory),
                    expected_sha256: None,
                },
                &mut Vec::new(),
                None,
            )
            .unwrap_err();
            assert!(error.to_string().contains("shutdown unconfirmed"));
            assert_eq!(FINALIZATION.with(|state| state.get().0), 1);
            FINALIZATION.with(|state| state.set((0, false)));
            let mut stream = Vec::new();
            let outcome = run_campaign_checkpointed(
                &TestWorkload {
                    bootstrap_objective: false,
                    targets: None,
                },
                &config,
                &CampaignOrigin::SearchCheckpoint {
                    path: checkpoint_path(directory),
                    expected_sha256: None,
                },
                &mut stream,
                None,
            )
            .expect("checkpoint resumes without the injected failure");
            assert_eq!(FINALIZATION.with(|state| state.get().0), 1);
            (outcome, stream)
        };
        let (first_resume, first_resume_stream) = resume(&first_directory);
        let (second_resume, second_resume_stream) = resume(&second_directory);
        assert_eq!(first_resume.0.archive, second_resume.0.archive);
        assert_eq!(first_resume.1, second_resume.1);
        assert_eq!(
            job_lines(&first_resume_stream),
            job_lines(&second_resume_stream)
        );
        let (panic_first, panic_stream, panic_directory) =
            run_order(earlier.1, "panic-first", true);
        let (success_before_panic, later_panic_stream, later_panic_directory) =
            run_order(later.1, "success-before-panic", true);
        let expected_panic = format!(
            "campaign reservation {} failed: worker panicked during execution",
            later.0
        );
        assert_eq!(panic_first, expected_panic);
        assert_eq!(success_before_panic, expected_panic);
        assert_eq!(panic_stream, first_stream);
        assert_eq!(later_panic_stream, first_stream);
        assert_eq!(
            stable_checkpoint(&panic_directory),
            stable_checkpoint(&first_directory)
        );
        assert_eq!(
            stable_checkpoint(&later_panic_directory),
            stable_checkpoint(&first_directory)
        );
        std::fs::remove_dir_all(first_directory).unwrap();
        std::fs::remove_dir_all(second_directory).unwrap();
        std::fs::remove_dir_all(panic_directory).unwrap();
        std::fs::remove_dir_all(later_panic_directory).unwrap();
    }

    #[test]
    fn sole_worker_panic_reports_its_reservation_after_reply_disconnect() {
        let mut config = three_worker_config();
        config.workers = 1;
        config.window = 1;
        config.execution_budget = 1;
        let (baseline, first_job) = (0..32)
            .find_map(|seed| {
                config.campaign_seed = seed;
                let mut stream = Vec::new();
                run_campaign_checkpointed(
                    &TestWorkload {
                        bootstrap_objective: false,
                        targets: None,
                    },
                    &config,
                    &CampaignOrigin::Genesis,
                    &mut stream,
                    None,
                )
                .ok()?;
                let jobs = job_lines(&stream);
                (jobs.len() == 1).then(|| (stream, jobs[0].clone()))
            })
            .expect("single-worker campaign executes a job");
        let panic_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let targets = Arc::new(LiveTargets::default());
        let panic_count_for_hook = Arc::clone(&panic_count);
        *targets.action_hook.lock().unwrap() = Some(Arc::new(move |_| {
            panic_count_for_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            panic!("injected sole-worker panic");
        }));
        let mut stream = Vec::new();
        let error = run_campaign_checkpointed(
            &TestWorkload {
                bootstrap_objective: false,
                targets: Some(targets),
            },
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .expect_err("sole worker panic fails the campaign")
        .to_string();
        assert_eq!(panic_count.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            error,
            format!(
                "campaign reservation {} failed: worker panicked during execution",
                first_job.1.reservation
            )
        );
        let expected_prefix = format!(
            "{}\n",
            std::str::from_utf8(&baseline)
                .unwrap()
                .lines()
                .take(first_job.0)
                .collect::<Vec<_>>()
                .join("\n")
        );
        assert_eq!(std::str::from_utf8(&stream).unwrap(), expected_prefix);
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn sole_worker_recovers_for_an_earlier_planned_job_after_panic() {
        let mut config = three_worker_config();
        config.workers = 1;
        config.window = 2;
        config.execution_budget = 3;
        let (baseline, failed_reservation, failed_input, failed_index) = (0..128)
            .find_map(|seed| {
                config.campaign_seed = seed;
                let mut stream = Vec::new();
                run_campaign_checkpointed(
                    &TestWorkload {
                        bootstrap_objective: false,
                        targets: None,
                    },
                    &config,
                    &CampaignOrigin::Genesis,
                    &mut stream,
                    None,
                )
                .ok()?;
                let jobs = job_lines(&stream);
                let failed = jobs.iter().find(|(_, job)| job.reservation == 0)?;
                (jobs.len() == 3
                    && jobs[0].0 == 1
                    && jobs[0].1.reservation == 1
                    && failed.0 > jobs[0].0
                    && jobs.iter().all(|(_, job)| {
                        job.reservation == 0
                            || job.mutation_seed as u8 != failed.1.mutation_seed as u8
                    }))
                .then_some((
                    stream,
                    failed.1.reservation,
                    failed.1.mutation_seed as u8,
                    failed.0,
                ))
            })
            .expect("the first dispatched job can have a later planned finish");
        let targets = Arc::new(LiveTargets::default());
        targets
            .second_selection_waits_for_first_action
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let weak_targets = Arc::downgrade(&targets);
        *targets.action_hook.lock().unwrap() = Some(Arc::new(move |action| {
            if action.input == failed_input {
                let targets = weak_targets.upgrade().unwrap();
                targets
                    .first_action_started
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                let deadline = Instant::now() + Duration::from_secs(5);
                while targets.selections.load(std::sync::atomic::Ordering::SeqCst) < 2 {
                    if Instant::now() >= deadline {
                        return Err("the second reservation was never selected".to_owned());
                    }
                    std::thread::yield_now();
                }
                panic!("later-planned reservation panicked");
            }
            Ok(())
        }));
        let directory = std::env::temp_dir().join(format!(
            "dissonance-single-worker-panic-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let mut stream = Vec::new();
        let error = run_campaign_checkpointed_with_options(
            &TestWorkload {
                bootstrap_objective: false,
                targets: Some(targets),
            },
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
            CampaignExecutionOptions {
                checkpoints: Some(crate::search::checkpoint::CheckpointPlan {
                    directory: directory.clone(),
                    every: std::num::NonZeroU64::new(1),
                    on_marks: false,
                    on_top_progress: false,
                    checkpoint_owner: None,
                    max_image_bytes: 64 << 20,
                    before_write: None,
                }),
                ..CampaignExecutionOptions::default()
            },
        )
        .expect_err("later-planned panic fails at its scheduled turn")
        .to_string();
        assert_eq!(
            error,
            format!(
                "campaign reservation {failed_reservation} failed: worker panicked during execution"
            )
        );
        let expected_prefix = format!(
            "{}\n",
            std::str::from_utf8(&baseline)
                .unwrap()
                .lines()
                .take(failed_index)
                .collect::<Vec<_>>()
                .join("\n")
        );
        assert_eq!(std::str::from_utf8(&stream).unwrap(), expected_prefix);
        assert!(directory.join("000000000001-interval.ckpt").is_file());
        std::fs::remove_dir_all(directory).unwrap();

        let targets = Arc::new(LiveTargets::default());
        targets
            .second_selection_waits_for_first_action
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let weak_targets = Arc::downgrade(&targets);
        *targets.action_hook.lock().unwrap() = Some(Arc::new(move |action| {
            if action.input == failed_input {
                let targets = weak_targets.upgrade().unwrap();
                targets
                    .first_action_started
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                let deadline = Instant::now() + Duration::from_secs(5);
                while targets.selections.load(std::sync::atomic::Ordering::SeqCst) < 2 {
                    if Instant::now() >= deadline {
                        return Err("the second reservation was never selected".to_owned());
                    }
                    std::thread::yield_now();
                }
                targets
                    .fail_next_target
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                panic!("later-planned reservation panicked before failed recovery");
            }
            Ok(())
        }));
        let mut failed_stream = Vec::new();
        let error = run_campaign_checkpointed(
            &TestWorkload {
                bootstrap_objective: false,
                targets: Some(targets),
            },
            &config,
            &CampaignOrigin::Genesis,
            &mut failed_stream,
            None,
        )
        .expect_err("missing earlier job fails with its own reservation")
        .to_string();
        assert_eq!(
            error,
            "campaign reservation 1 failed: every worker exited before reporting its result"
        );
        assert_eq!(
            std::str::from_utf8(&failed_stream).unwrap(),
            format!(
                "{}\n",
                std::str::from_utf8(&baseline)
                    .unwrap()
                    .lines()
                    .next()
                    .unwrap()
            )
        );
    }

    #[test]
    fn failed_recovery_preserves_an_admitted_success_before_the_next_failure() {
        let mut config = three_worker_config();
        config.workers = 1;
        config.window = 2;
        config.execution_budget = 3;
        let (baseline, failed_input) = (0..128)
            .find_map(|seed| {
                config.campaign_seed = seed;
                let mut stream = Vec::new();
                run_campaign_checkpointed(
                    &TestWorkload {
                        bootstrap_objective: false,
                        targets: None,
                    },
                    &config,
                    &CampaignOrigin::Genesis,
                    &mut stream,
                    None,
                )
                .ok()?;
                let jobs = job_lines(&stream);
                let failed = jobs.iter().find(|(_, job)| job.reservation == 1)?;
                (jobs.len() == 3
                    && jobs[0].0 == 1
                    && jobs[0].1.reservation == 0
                    && jobs[1].1.reservation == 1
                    && jobs.iter().all(|(_, job)| {
                        job.reservation == 1
                            || job.mutation_seed as u8 != failed.1.mutation_seed as u8
                    }))
                .then_some((stream, failed.1.mutation_seed as u8))
            })
            .expect("the first dispatched job can finish first");
        let targets = Arc::new(LiveTargets::default());
        let weak_targets = Arc::downgrade(&targets);
        *targets.action_hook.lock().unwrap() = Some(Arc::new(move |action| {
            if action.input == failed_input {
                let targets = weak_targets.upgrade().unwrap();
                targets
                    .fail_next_target
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                panic!("second reservation panicked before failed recovery");
            }
            Ok(())
        }));
        let mut stream = Vec::new();
        let error = run_campaign_checkpointed(
            &TestWorkload {
                bootstrap_objective: false,
                targets: Some(Arc::clone(&targets)),
            },
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .expect_err("campaign must fail after reporting its successful prefix")
        .to_string();
        assert_eq!(
            targets.selections.load(std::sync::atomic::Ordering::SeqCst),
            3
        );
        assert_eq!(
            error,
            "campaign reservation 1 failed: worker panicked during execution"
        );
        assert_eq!(
            std::str::from_utf8(&stream).unwrap(),
            format!(
                "{}\n",
                std::str::from_utf8(&baseline)
                    .unwrap()
                    .lines()
                    .take(2)
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        );
    }

    #[test]
    fn replay_rejects_forged_ordinary_splice_provenance() {
        let mut config = three_worker_config();
        config.execution_budget = 2_000;
        config.mixture = DrawMixture::EnergySplice { scale: 6 };
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut stream = Vec::new();
        let live = run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .expect("campaign");
        assert_eq!(
            replay_campaign_checkpointed(&workload, &stream, None, None)
                .expect("untampered splice replays"),
            live
        );
        let lines = std::str::from_utf8(&stream)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let (index, _) = job_lines(&stream)
            .into_iter()
            .find(|(_, job)| {
                job.selector.path != SelectorPath::Continuation
                    && matches!(job.splice, Some(CampaignSpliceRecord::Tail { .. }))
            })
            .expect("ordinary splice job");
        for field in ["donor_id", "leaf_id"] {
            let mut tampered = lines.clone();
            let mut record: serde_json::Value = serde_json::from_str(&tampered[index]).unwrap();
            record["splice"][field] = serde_json::json!(u64::MAX);
            tampered[index] = serde_json::to_string(&record).unwrap();
            let mutated = format!("{}\n", tampered.join("\n"));
            assert!(
                replay_campaign_checkpointed(&workload, mutated.as_bytes(), None, None).is_err(),
                "replay accepted an impossible ordinary splice {field}"
            );
        }
    }

    #[test]
    fn replay_rejects_more_consecutive_skips_than_live_selection_allows() {
        let mut config = three_worker_config();
        config.execution_budget = 2_000;
        config.window = 1;
        config.workers = 1;
        config.mixture = DrawMixture::AlphabetOnly;
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let (live, mut records, skip_index, original_stream) = (0..128)
            .find_map(|seed| {
                config.campaign_seed = seed;
                let mut stream = Vec::new();
                let (live, _) = run_campaign_checkpointed(
                    &workload,
                    &config,
                    &CampaignOrigin::Genesis,
                    &mut stream,
                    None,
                )
                .expect("campaign");
                let records = std::str::from_utf8(&stream)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                    .collect::<Vec<_>>();
                let skip_index = records.iter().position(|record| {
                    record["event"] == "skip"
                        && record["draw_checkpoint_before"]["records"]
                            .as_u64()
                            .unwrap()
                            .is_multiple_of(64)
                })?;
                Some((live, records, skip_index, stream))
            })
            .expect("campaign with a skip on an empty draw-table update boundary");
        replay_campaign_checkpointed(&workload, &original_stream, None, None)
            .expect("unmodified campaign replays");
        let original = records[skip_index].clone();
        let before = original["draw_checkpoint_before"].clone();
        let first_record = before["records"].as_u64().unwrap();
        let mut inserted = Vec::new();
        for offset in 0..super::CONSECUTIVE_SKIP_LIMIT {
            let mut skip = original.clone();
            skip["draw_checkpoint_before"]["records"] = serde_json::json!(first_record + offset);
            let after_record = first_record + offset + 1;
            skip["draw_checkpoint_after"] = if after_record.is_multiple_of(1_024) {
                let mut checkpoint = before.clone();
                checkpoint["records"] = serde_json::json!(after_record);
                checkpoint
            } else {
                serde_json::Value::Null
            };
            inserted.push(skip);
        }
        for record in records.iter_mut().skip(skip_index) {
            for field in ["draw_checkpoint_before", "draw_checkpoint_after"] {
                if let Some(value) = record[field]["records"].as_u64() {
                    record[field]["records"] =
                        serde_json::json!(value + super::CONSECUTIVE_SKIP_LIMIT);
                }
            }
        }
        records.splice(skip_index..skip_index, inserted);
        records.last_mut().unwrap()["skips"] =
            serde_json::json!(live.duplicates_skipped + super::CONSECUTIVE_SKIP_LIMIT);
        let lines = records
            .iter()
            .map(|record| serde_json::to_string(record).unwrap())
            .collect::<Vec<_>>();
        let error =
            replay_campaign_checkpointed(&workload, lines.join("\n").as_bytes(), None, None)
                .expect_err(
                    "replay certified more consecutive skips than live selection can record",
                )
                .to_string();
        assert!(error.contains("consecutive skip limit"), "{error}");
    }

    #[test]
    fn multi_window_reservations_each_allow_the_full_skip_limit() {
        let mut schedule = super::ReplaySkipSchedule::default();
        for _ in 0..super::CONSECUTIVE_SKIP_LIMIT {
            schedule.accept_skip(0, 0, 2, 2).unwrap();
        }
        for _ in 0..super::CONSECUTIVE_SKIP_LIMIT {
            schedule.accept_skip(1, 0, 2, 2).unwrap();
        }
        assert!(schedule.accept_skip(1, 0, 2, 2).is_err());
    }

    #[test]
    fn skips_follow_the_admission_that_opens_their_window_slot() {
        let mut schedule = super::ReplaySkipSchedule::default();
        schedule.accept_skip(0, 0, 2, 4).unwrap();
        schedule.accept_skip(1, 0, 2, 4).unwrap();
        schedule.admit(1);
        assert!(schedule.accept_skip(1, 1, 2, 4).is_err());
        schedule.accept_skip(2, 1, 2, 4).unwrap();
        schedule.admit(0);
        schedule.accept_skip(3, 2, 2, 4).unwrap();

        let mut curtailed = super::ReplaySkipSchedule::default();
        curtailed.accept_skip(0, 0, 2, 1).unwrap();
        assert!(curtailed.accept_skip(0, 1, 2, 1).is_err());

        for (reservation, admissions) in [(0, 1), (1, 1), (2, 0), (2, 2), (3, 1)] {
            let mut schedule = super::ReplaySkipSchedule::default();
            assert!(schedule.accept_skip(reservation, admissions, 2, 4).is_err());
        }
    }

    #[test]
    fn replay_rejects_a_skip_moved_past_a_later_reservations_admission() {
        let mut config = three_worker_config();
        config.campaign_seed = 1;
        config.workers = 1;
        config.window = 2;
        config.execution_budget = 2;
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut stream = Vec::new();
        run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .expect("source campaign");
        let jobs = job_lines(&stream);
        let [(first_index, first), (second_index, second)] = jobs.as_slice() else {
            panic!("source campaign did not admit exactly two jobs");
        };
        assert_eq!((first.reservation, second.reservation), (1, 0));
        assert_eq!((first.parent_id, second.parent_id), (0, 0));
        assert!(
            first
                .decisions
                .iter()
                .any(|decision| matches!(decision, CampaignAdmissionDecision::Retained { .. }))
        );
        replay_campaign_checkpointed(&workload, &stream, None, None)
            .expect("original campaign replays");
        let mut lines = std::str::from_utf8(&stream)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let first_value: serde_json::Value = serde_json::from_str(&lines[*first_index]).unwrap();
        let skip = serde_json::json!({
            "event": "skip",
            "reservation": 0,
            "parent_id": 0,
            "mutation_seed": first.mutation_seed,
            "mixture_weight": first.mixture_weight,
            "splice_weight": first.splice_weight,
            "selector": first_value["selector"],
            "suffix_limit": 1,
        });
        lines.insert(*second_index, serde_json::to_string(&skip).unwrap());
        let mut end: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
        end["skips"] = serde_json::json!(1);
        *lines.last_mut().unwrap() = serde_json::to_string(&end).unwrap();
        let changed = format!("{}\n", lines.join("\n"));
        let error = replay_campaign_checkpointed(&workload, changed.as_bytes(), None, None)
            .expect_err("replay accepted impossible skip after later reservation admission")
            .to_string();
        assert!(error.contains("selection boundary"), "{error}");
    }

    #[test]
    fn replay_rejects_stream_truncated_after_a_complete_record() {
        let mut config = three_worker_config();
        config.workers = 1;
        config.window = 1;
        config.execution_budget = 4;
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut stream = Vec::new();
        let live = run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .expect("campaign");
        assert_eq!(live.0.executions_completed, 4);
        let lines = std::str::from_utf8(&stream)
            .unwrap()
            .lines()
            .collect::<Vec<_>>();
        assert!(lines.len() > 2);
        let truncated = format!("{}\n", lines[..lines.len() - 1].join("\n"));
        assert!(replay_campaign_checkpointed(&workload, truncated.as_bytes(), None, None).is_err());

        let mut tampered = lines
            .iter()
            .map(|line| (*line).to_owned())
            .collect::<Vec<_>>();
        let mut end: serde_json::Value = serde_json::from_str(tampered.last().unwrap()).unwrap();
        end["jobs"] = serde_json::json!(3);
        *tampered.last_mut().unwrap() = serde_json::to_string(&end).unwrap();
        let error = replay_campaign_checkpointed(
            &workload,
            format!("{}\n", tampered.join("\n")).as_bytes(),
            None,
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("terminal counts diverged"));
    }

    #[test]
    fn replay_refuses_jobs_admitted_out_of_planned_finish_order() {
        let mut config = three_worker_config();
        config.window = 6;
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut stream = Vec::new();
        run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .expect("campaign");
        let lines = std::str::from_utf8(&stream)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let jobs = job_lines(&stream);
        let refused = jobs.windows(2).filter(|pair| {
            let mut tampered = lines.clone();
            tampered[pair[0].0] = lines[pair[0].0].replacen(
                &format!("\"reservation\":{},", pair[0].1.reservation),
                &format!("\"reservation\":{},", pair[1].1.reservation),
                1,
            );
            tampered[pair[1].0] = lines[pair[1].0].replacen(
                &format!("\"reservation\":{},", pair[1].1.reservation),
                &format!("\"reservation\":{},", pair[0].1.reservation),
                1,
            );
            replay_campaign_checkpointed(&workload, tampered.join("\n").as_bytes(), None, None)
                .is_err_and(|error| error.to_string().contains("out of planned-finish order"))
        });
        assert!(refused.count() > jobs.len() / 2);
    }

    fn run_placed(placement: ThreadPlacement) -> Result<CampaignOutcome<TestWorkload>, String> {
        run_campaign_checkpointed_with_options(
            &TestWorkload {
                bootstrap_objective: false,
                targets: None,
            },
            &three_worker_config(),
            &CampaignOrigin::Genesis,
            &mut Vec::new(),
            None,
            CampaignExecutionOptions {
                placement: Some(placement),
                ..CampaignExecutionOptions::default()
            },
        )
        .map_err(|error| error.to_string())
    }

    #[test]
    fn a_campaign_never_holds_more_targets_than_workers() {
        let targets = Arc::new(LiveTargets::default());
        let config = three_worker_config();
        run_campaign_checkpointed(
            &TestWorkload {
                bootstrap_objective: false,
                targets: Some(Arc::clone(&targets)),
            },
            &config,
            &CampaignOrigin::Genesis,
            &mut Vec::new(),
            None,
        )
        .expect("campaign");
        let peak = targets.most.load(std::sync::atomic::Ordering::SeqCst);
        assert!(peak > 0);
        assert!(peak <= config.workers as usize);
        assert_eq!(targets.live.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[derive(Default)]
    struct StaggeredWatch {
        offset: usize,
        notes: std::sync::Mutex<Vec<usize>>,
        admitted: std::sync::Mutex<Vec<usize>>,
        refuse_start: bool,
        selected: bool,
        requested: Mutex<Vec<usize>>,
    }

    impl super::CampaignWatch<TestWorkload> for StaggeredWatch {
        type Note = usize;

        fn run(
            &self,
            job: &JobSpec<TestWorkload>,
            run: impl FnOnce() -> Result<JobOutcome<TestWorkload>, Box<dyn Error>>,
        ) -> super::WatchedJob<TestWorkload, usize> {
            let delay = (job.reservation * 7 + self.offset) % 4;
            std::thread::sleep(Duration::from_micros(250 * delay as u64));
            Ok((run()?, job.reservation))
        }

        fn prepare_admission(
            &self,
            note: &usize,
            campaign: &mut CampaignCoordinator<'_, TestWorkload>,
        ) {
            if self.selected {
                campaign.watch_selected_admissions();
                if note % 3 == 1 {
                    assert!(campaign.request_admission(*note));
                    self.requested.lock().unwrap().push(*note);
                }
            }
        }

        fn admitted(&self, note: usize, campaign: &mut CampaignCoordinator<'_, TestWorkload>) {
            self.notes.lock().unwrap().push(note);
            self.admitted.lock().unwrap().extend(
                campaign
                    .take_admissions()
                    .into_iter()
                    .map(|admission| admission.reservation),
            );
        }

        fn start(
            &self,
            campaign: &mut CampaignCoordinator<'_, TestWorkload>,
        ) -> Result<(), Box<dyn Error>> {
            if self.refuse_start {
                return Err("the watch refused to start".into());
            }
            if !self.selected {
                campaign.watch_admissions();
            }
            Ok(())
        }
    }

    fn watched_stream(watch: &StaggeredWatch) -> Result<Vec<u8>, Box<dyn Error>> {
        let mut stream = Vec::new();
        super::run_campaign_watched(
            &TestWorkload {
                bootstrap_objective: false,
                targets: None,
            },
            &three_worker_config(),
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
            CampaignExecutionOptions::default(),
            watch,
        )?;
        Ok(stream)
    }

    #[test]
    fn a_watched_campaign_writes_the_unwatched_stream_and_sees_every_admission() {
        let mut plain = Vec::new();
        let plain_outcome = run_campaign_checkpointed(
            &TestWorkload {
                bootstrap_objective: false,
                targets: None,
            },
            &three_worker_config(),
            &CampaignOrigin::Genesis,
            &mut plain,
            None,
        )
        .expect("campaign");
        for offset in 0..3 {
            let watch = StaggeredWatch {
                offset,
                ..StaggeredWatch::default()
            };
            let watched = watched_stream(&watch).expect("watched campaign");
            assert_eq!(watched, plain);
            let replayed = super::replay_campaign_checkpointed(
                &TestWorkload {
                    bootstrap_objective: false,
                    targets: None,
                },
                &watched,
                None,
                None,
            )
            .expect("reordered campaign replay");
            assert_eq!(
                serde_json::to_value(&replayed.0).unwrap(),
                serde_json::to_value(&plain_outcome.0).unwrap(),
            );
            assert_eq!(replayed.1, plain_outcome.1);
            let mut notes = watch.notes.into_inner().unwrap();
            let mut admitted = watch.admitted.into_inner().unwrap();
            notes.sort_unstable();
            admitted.sort_unstable();
            assert!(!notes.is_empty());
            assert_eq!(notes, admitted);
        }
    }

    #[derive(Default)]
    struct FirstFinishWatch {
        admitted: Mutex<Vec<super::CampaignAdmission>>,
    }

    impl super::CampaignWatch<TestWorkload> for FirstFinishWatch {
        type Note = usize;

        fn run(
            &self,
            job: &JobSpec<TestWorkload>,
            run: impl FnOnce() -> Result<JobOutcome<TestWorkload>, Box<dyn Error>>,
        ) -> super::WatchedJob<TestWorkload, usize> {
            let outcome = run()?;
            assert!(
                outcome
                    .result
                    .actions
                    .iter()
                    .any(|action| action.outcome.objective_reached)
            );
            Ok((outcome, job.reservation))
        }

        fn prepare_admission(
            &self,
            note: &usize,
            campaign: &mut CampaignCoordinator<'_, TestWorkload>,
        ) {
            campaign.watch_selected_admissions();
            assert!(campaign.request_admission(*note));
        }

        fn admitted(&self, note: usize, campaign: &mut CampaignCoordinator<'_, TestWorkload>) {
            let admissions = campaign.take_admissions();
            assert_eq!(admissions.len(), 1);
            assert_eq!(admissions[0].reservation, note);
            assert!(
                admissions[0]
                    .decisions
                    .contains(&CampaignAdmissionDecision::Objective)
            );
            self.admitted.lock().unwrap().extend(admissions);
        }
    }

    #[test]
    fn the_first_watched_job_keeps_its_admission_when_it_finishes_the_campaign() {
        let targets = Arc::new(LiveTargets::default());
        targets
            .objective_after_work
            .store(1, std::sync::atomic::Ordering::SeqCst);
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: Some(targets),
        };
        let mut config = three_worker_config();
        config.workers = 1;
        config.window = 1;
        config.execution_budget = 1;
        config.stop_campaign_on_objective = true;
        let mut plain = Vec::new();
        let expected = run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut plain,
            None,
        )
        .unwrap();
        let mut stream = Vec::new();
        let watch = FirstFinishWatch::default();
        let outcome = super::run_campaign_watched(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
            CampaignExecutionOptions::default(),
            &watch,
        )
        .unwrap();
        assert_eq!(outcome.0.executions_completed, 1);
        assert_eq!(stream, plain);
        assert_eq!(outcome, expected);
        assert_eq!(outcome.1, expected.1);
        assert_eq!(watch.admitted.into_inner().unwrap().len(), 1);
    }

    #[test]
    fn selected_admission_requests_survive_reordered_completions_without_changing_the_campaign() {
        let mut config = three_worker_config();
        config.window = 6;
        config.execution_budget = 60;
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut plain = Vec::new();
        let expected = run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut plain,
            None,
        )
        .unwrap();
        for reverse in [false, true] {
            let mut stream = Vec::new();
            let mut coordinator = CampaignCoordinator::start(
                &workload,
                &config,
                &CampaignOrigin::Genesis,
                &mut stream,
                None,
                CampaignExecutionOptions::default(),
            )
            .unwrap();
            while coordinator.reserve_next().unwrap() {}
            let mut target = workload.new_target().unwrap();
            let mut requested = BTreeSet::new();
            let mut observed = BTreeSet::new();
            assert!(!coordinator.request_admission(0));
            coordinator.watch_selected_admissions();
            assert!(!coordinator.request_admission(usize::MAX));
            assert!(!coordinator.request_admission(config.execution_budget as usize));
            while !coordinator.finished() {
                let jobs = coordinator.plan(usize::MAX);
                assert!(!jobs.is_empty());
                let mut outcomes = jobs
                    .iter()
                    .map(|job| run_job(&workload, &config, &mut target, job).unwrap())
                    .collect::<Vec<_>>();
                if reverse {
                    outcomes.reverse();
                }
                for outcome in outcomes {
                    let reservation = outcome.reservation;
                    if reservation % 3 == 1 {
                        assert!(coordinator.request_admission(reservation));
                        assert!(coordinator.request_admission(reservation));
                        requested.insert(reservation);
                    }
                    coordinator.admit(outcome).unwrap();
                    for admission in coordinator.take_admissions() {
                        assert!(observed.insert(admission.reservation));
                        assert!(!coordinator.request_admission(admission.reservation));
                    }
                }
            }
            assert!(!requested.is_empty());
            assert!(requested.len() < config.execution_budget as usize);
            assert_eq!(observed, requested);
            assert!(coordinator.take_admissions().is_empty());
            let actual = coordinator.finish(Vec::new()).unwrap();
            assert_eq!(stream, plain);
            assert_eq!(actual, expected);
            assert_eq!(
                serde_json::to_value(&actual.0).unwrap(),
                serde_json::to_value(&expected.0).unwrap()
            );
            assert_eq!(actual.1, expected.1);
        }
    }

    #[test]
    fn disabling_admission_watching_drops_pending_requests_and_collected_records() {
        let mut config = three_worker_config();
        config.execution_budget = 3;
        config.window = 3;
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut stream = Vec::new();
        let mut coordinator = CampaignCoordinator::start(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
            CampaignExecutionOptions::default(),
        )
        .unwrap();
        while coordinator.reserve_next().unwrap() {}
        let mut target = workload.new_target().unwrap();
        let mut jobs = coordinator.plan(usize::MAX);
        coordinator.watch_selected_admissions();
        for job in &jobs {
            assert!(coordinator.request_admission(job.reservation));
        }
        let first = jobs.remove(0);
        coordinator
            .admit(run_job(&workload, &config, &mut target, &first).unwrap())
            .unwrap();
        coordinator.unwatch_admissions();
        assert!(!coordinator.request_admission(jobs[0].reservation));
        coordinator.watch_selected_admissions();
        for job in jobs {
            coordinator
                .admit(run_job(&workload, &config, &mut target, &job).unwrap())
                .unwrap();
        }
        assert!(coordinator.take_admissions().is_empty());
    }

    #[test]
    fn a_sampled_watch_matches_requested_reservations_across_worker_completion_orders() {
        let config = three_worker_config();
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut plain = Vec::new();
        let expected = run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut plain,
            None,
        )
        .unwrap();
        for offset in 0..3 {
            let watch = StaggeredWatch {
                offset,
                selected: true,
                ..StaggeredWatch::default()
            };
            let mut stream = Vec::new();
            let outcome = super::run_campaign_watched(
                &workload,
                &config,
                &CampaignOrigin::Genesis,
                &mut stream,
                None,
                CampaignExecutionOptions::default(),
                &watch,
            )
            .unwrap();
            assert_eq!(stream, plain);
            assert_eq!(outcome, expected);
            assert_eq!(outcome.1, expected.1);
            let mut requested = watch.requested.into_inner().unwrap();
            let mut admitted = watch.admitted.into_inner().unwrap();
            requested.sort_unstable();
            admitted.sort_unstable();
            assert!(!requested.is_empty());
            assert!(requested.len() < watch.notes.lock().unwrap().len());
            assert_eq!(admitted, requested);
        }
    }

    #[test]
    fn a_watch_that_fails_to_start_stops_the_campaign() {
        let watch = StaggeredWatch {
            refuse_start: true,
            ..StaggeredWatch::default()
        };
        let error = watched_stream(&watch).expect_err("the campaign must stop");
        assert!(error.to_string().contains("the watch refused to start"));
        assert!(watch.notes.into_inner().unwrap().is_empty());
    }

    #[test]
    fn a_failed_bootstrap_returns_its_error_with_workers_waiting() {
        FINALIZATION.with(|state| state.set((0, false)));
        let error = run_campaign_checkpointed(
            &TestWorkload {
                bootstrap_objective: false,
                targets: Some(Arc::new(LiveTargets {
                    refuse: true,
                    ..LiveTargets::default()
                })),
            },
            &three_worker_config(),
            &CampaignOrigin::Genesis,
            &mut Vec::new(),
            None,
        )
        .expect_err("a refused bootstrap target fails the campaign");
        assert!(
            error
                .to_string()
                .contains("failed to build the bootstrap target"),
            "{error}"
        );
        assert_eq!(FINALIZATION.with(|state| state.get().0), 1);
    }

    #[test]
    fn placement_runs_once_on_the_coordinator_and_each_worker_thread() {
        let placed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = std::sync::Arc::clone(&placed);
        let placement = ThreadPlacement::new(move |thread| {
            record
                .lock()
                .expect("placement record")
                .push((thread, std::thread::current().id()));
            Ok(())
        });
        let placed_run = run_placed(placement).expect("placed campaign");
        let phase = placed_run
            .0
            .telemetry
            .dispatch_phase
            .as_ref()
            .expect("native dispatch phase");
        assert!(phase.wall_ns <= placed_run.0.telemetry.search_ns);
        if let Some(host) = phase.host {
            let total = placed_run
                .0
                .telemetry
                .coordinator_host
                .expect("coordinator host counters");
            assert!(host.coordinator_cpu_ns <= total.coordinator_cpu_ns);
            assert!(host.coordinator_run_wait_ns <= total.coordinator_run_wait_ns);
            assert!(host.idle_admission_order_ns <= total.idle_admission_order_ns);
            assert!(host.idle_no_job_ns <= total.idle_no_job_ns);
        }
        let unplaced = run_campaign_checkpointed(
            &TestWorkload {
                bootstrap_objective: false,
                targets: None,
            },
            &three_worker_config(),
            &CampaignOrigin::Genesis,
            &mut Vec::new(),
            None,
        )
        .expect("campaign");
        assert_eq!(placed_run, unplaced);
        let mut placed = placed.lock().expect("placement record").clone();
        placed.sort_by_key(|(thread, _)| match thread {
            PlacedThread::Coordinator => 0,
            PlacedThread::StreamEncoder => 1,
            PlacedThread::Worker(worker) => worker + 2,
        });
        assert_eq!(
            placed.iter().map(|(thread, _)| *thread).collect::<Vec<_>>(),
            vec![
                PlacedThread::Coordinator,
                PlacedThread::StreamEncoder,
                PlacedThread::Worker(0),
                PlacedThread::Worker(1),
                PlacedThread::Worker(2),
            ]
        );
        let threads = placed
            .iter()
            .map(|(_, id)| format!("{id:?}"))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(threads.len(), 5);
    }

    #[test]
    fn a_placement_failure_fails_the_campaign() {
        for refused in [
            PlacedThread::Coordinator,
            PlacedThread::StreamEncoder,
            PlacedThread::Worker(1),
        ] {
            let placement = ThreadPlacement::new(move |thread| {
                if thread == refused {
                    Err(format!("{thread:?} refused"))
                } else {
                    Ok(())
                }
            });
            let error = run_placed(placement).expect_err("refused placement");
            assert!(error.contains("refused"), "{error}");
        }
    }

    #[test]
    fn startup_failures_are_ordered_before_campaign_selection() {
        let mut config = three_worker_config();
        config.workers = 2;
        config.window = 2;
        config.execution_budget = 2;
        let targets = Arc::new(LiveTargets::default());
        let (one_failed_sender, one_failed_receiver) = mpsc::channel();
        let (release_zero_sender, release_zero_receiver) = mpsc::channel();
        let release_zero_receiver = Mutex::new(release_zero_receiver);
        let placement = ThreadPlacement::new(move |thread| match thread {
            PlacedThread::Worker(0) => {
                release_zero_receiver
                    .lock()
                    .unwrap()
                    .recv()
                    .map_err(|error| error.to_string())?;
                Err("worker zero refused startup".to_owned())
            }
            PlacedThread::Worker(1) => {
                one_failed_sender.send(()).unwrap();
                Err("worker one refused startup".to_owned())
            }
            PlacedThread::Coordinator | PlacedThread::StreamEncoder => Ok(()),
            _ => unreachable!(),
        });
        let (error, stream) = std::thread::scope(|scope| {
            let targets_for_run = Arc::clone(&targets);
            let run = scope.spawn(|| {
                let mut stream = Vec::new();
                let error = run_campaign_checkpointed_with_options(
                    &TestWorkload {
                        bootstrap_objective: false,
                        targets: Some(targets_for_run),
                    },
                    &config,
                    &CampaignOrigin::Genesis,
                    &mut stream,
                    None,
                    CampaignExecutionOptions {
                        placement: Some(placement),
                        ..CampaignExecutionOptions::default()
                    },
                )
                .expect_err("both workers refuse startup")
                .to_string();
                (error, stream)
            });
            let one_started = one_failed_receiver
                .recv_timeout(Duration::from_secs(5))
                .is_ok();
            release_zero_sender.send(()).unwrap();
            let outcome = run.join().unwrap();
            assert!(one_started, "worker one never attempted startup");
            outcome
        });
        assert_eq!(
            error,
            "campaign worker 0 failed to start: worker zero refused startup"
        );
        assert_eq!(
            targets.selections.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "selection ran before startup failures were resolved"
        );
        assert_eq!(job_lines(&stream).len(), 0);
    }

    fn three_worker_config() -> CampaignConfig<TestWorkload> {
        CampaignConfig {
            campaign_seed: 5,
            workers: 3,
            execution_budget: 60,
            host: "test".to_owned(),
            wall_budget: None,
            stop_rollout_on_objective: false,
            stop_campaign_on_objective: false,
            archive_entry_limit: 32,
            window: 3,
            memory_budget_mib: None,
            materialize_final_artifacts: true,
            run: (),
            mixture: DrawMixture::AlphabetOnly,
            retention: RetentionPolicy::Unprobed,
            objective_witness_path: None,
        }
    }

    #[test]
    fn borrowed_core_checkpoint_preserves_current_owned_reference_bytes() {
        let (_, _, mut core, _) = test_core();
        core.objective_witness = Some(Input {
            actions: vec![TestAction::new(13, 2)],
        });
        core.sequence = 17;
        core.probe_refused = 19;
        core.terminal_endpoints = 23;
        let reference = super::CoreCheckpoint::<TestWorkload> {
            curve: core.curve.clone(),
            curve_interval: core.curve_interval,
            bounded_progress_curve: core.bounded_progress_curve,
            record_progress: core.record_progress,
            terminal_endpoints: core.terminal_endpoints,
            terminal_objectives: core.terminal_objectives,
            execution_failures: core.execution_failures,
            objectives_reached: core.objectives_reached,
            objective_witness: core.objective_witness.clone(),
            sequence: core.sequence,
            probe_refused: core.probe_refused,
            mixture_energy: core.mixture_energy,
        };
        let expected = postcard::to_allocvec(&reference).unwrap();
        let mut streamed = Vec::new();
        core.write_checkpoint_state(&mut streamed).unwrap();
        assert_eq!(streamed, expected);
        let restored: super::CoreCheckpoint<TestWorkload> =
            postcard::from_bytes(&streamed).unwrap();
        assert_eq!(restored.sequence, 17);
        assert_eq!(restored.probe_refused, 19);
        assert_eq!(restored.objective_witness, core.objective_witness);
    }

    #[allow(clippy::disallowed_methods)]
    fn wait_for_checkpoint_worker(coordinator: &CampaignCoordinator<'_, TestWorkload>) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !coordinator.checkpoint_worker_finished() {
            assert!(
                Instant::now() < deadline,
                "checkpoint worker did not finish"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn coordinator_preserves_typed_evidence_writer_errors_in_both_streaming_passes() {
        for max_image_bytes in [1, 64 << 20] {
            for pass in [1, 2] {
                let directory = std::env::temp_dir().join(format!(
                    "coordinator-evidence-error-{}-{max_image_bytes}-{pass}",
                    std::process::id()
                ));
                let _ = std::fs::remove_dir_all(&directory);
                let mut config = three_worker_config();
                config.window = 1;
                config.execution_budget = 1;
                let workload = TestWorkload {
                    bootstrap_objective: false,
                    targets: None,
                };
                let mut stream = Vec::new();
                let mut coordinator = CampaignCoordinator::start(
                    &workload,
                    &config,
                    &CampaignOrigin::Genesis,
                    &mut stream,
                    None,
                    CampaignExecutionOptions {
                        checkpoints: Some(crate::search::checkpoint::CheckpointPlan {
                            directory: directory.clone(),
                            every: std::num::NonZeroU64::new(1),
                            on_marks: false,
                            on_top_progress: false,
                            checkpoint_owner: None,
                            max_image_bytes,
                            before_write: None,
                        }),
                        ..CampaignExecutionOptions::default()
                    },
                )
                .unwrap();
                assert!(coordinator.reserve_next().unwrap());
                let job = coordinator.plan(1).pop().unwrap();
                let mut target = workload.new_target().unwrap();
                let guard = EvidenceWriteFailureGuard::new(pass);
                let error = coordinator
                    .admit(run_job(&workload, &config, &mut target, &job).unwrap())
                    .unwrap_err();
                assert!(
                    error.is::<super::super::checkpoint::publisher::CampaignPersistenceFailure>()
                );
                let primary = error.source().unwrap();
                assert_eq!(
                    primary
                        .downcast_ref::<TestEvidenceWriteError>()
                        .unwrap()
                        .pass,
                    pass
                );
                assert!(primary.source().unwrap().is::<TestEvidenceWriteCause>());
                assert_eq!(guard.calls(), pass);
                assert!(coordinator.reserve_next().is_err());
                assert!(coordinator.plan(usize::MAX).is_empty());
                assert!(coordinator.finish(Vec::new()).is_err());
                assert_eq!(guard.calls(), pass);
                assert!(!directory.join("000000000001-interval.ckpt").exists());
                assert!(!directory.join("000000000001-final.ckpt").exists());
                assert!(!directory.join("checkpoints.jsonl").exists());
                assert!(std::str::from_utf8(&stream).unwrap().lines().all(|line| {
                    serde_json::from_str::<serde_json::Value>(line).unwrap()["event"] != "end"
                }));
                std::fs::remove_dir_all(directory).unwrap();
            }
        }
    }

    #[test]
    fn coordinator_preserves_a_one_shot_typed_error_after_evidence_capture_overflows() {
        let directory = std::env::temp_dir().join(format!(
            "coordinator-evidence-overflow-error-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let mut config = three_worker_config();
        config.window = 1;
        config.execution_budget = 1;
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut stream = Vec::new();
        let mut coordinator = CampaignCoordinator::start(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
            CampaignExecutionOptions {
                checkpoints: Some(crate::search::checkpoint::CheckpointPlan {
                    directory: directory.clone(),
                    every: std::num::NonZeroU64::new(1),
                    on_marks: false,
                    on_top_progress: false,
                    checkpoint_owner: None,
                    max_image_bytes: 64 << 10,
                    before_write: None,
                }),
                ..CampaignExecutionOptions::default()
            },
        )
        .unwrap();
        assert!(coordinator.reserve_next().unwrap());
        let job = coordinator.plan(1).pop().unwrap();
        let mut target = workload.new_target().unwrap();
        let guard = EvidenceWriteFailureGuard::after_overflow();
        let error = coordinator
            .admit(run_job(&workload, &config, &mut target, &job).unwrap())
            .unwrap_err();
        assert!(error.is::<super::super::checkpoint::publisher::CampaignPersistenceFailure>());
        let primary = error.source().unwrap();
        assert_eq!(
            primary
                .downcast_ref::<TestEvidenceWriteError>()
                .unwrap()
                .pass,
            2
        );
        assert!(primary.source().unwrap().is::<TestEvidenceWriteCause>());
        assert_eq!(coordinator.checkpoint_capture_overflows(), 1);
        assert_eq!(guard.calls(), 2);
        assert_eq!(EVIDENCE_PRESSURE_WRITE.get(), Some((2, 2)));
        assert!(coordinator.reserve_next().is_err());
        assert!(coordinator.plan(usize::MAX).is_empty());
        assert!(coordinator.finish(Vec::new()).is_err());
        assert_eq!(guard.calls(), 2);
        assert!(!directory.join("000000000001-interval.ckpt").exists());
        assert!(!directory.join("000000000001-final.ckpt").exists());
        assert!(!directory.join("checkpoints.jsonl").exists());
        assert!(std::str::from_utf8(&stream).unwrap().lines().all(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["event"] != "end"
        }));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn typed_integrity_failure_precedes_both_ready_and_pending_persistence_failure() {
        for ready in [false, true] {
            for operation in [
                "reserve",
                "dispatch",
                "fail",
                "admit-dispatch",
                "drain-stream",
                "dispatch-queued",
            ] {
                let directory = std::env::temp_dir().join(format!(
                    "direct-integrity-persistence-{}-{ready}-{operation}",
                    std::process::id()
                ));
                let _ = std::fs::remove_dir_all(&directory);
                std::fs::create_dir_all(directory.join("000000000001-interval.ckpt")).unwrap();
                let (entered_sender, entered) = mpsc::channel();
                let (released, release) = mpsc::channel();
                let release = Mutex::new(release);
                let mut config = three_worker_config();
                config.window = 1;
                config.execution_budget = 2;
                let targets = Arc::new(LiveTargets::default());
                let workload = TestWorkload {
                    bootstrap_objective: false,
                    targets: Some(targets.clone()),
                };
                let mut stream = Vec::new();
                let mut coordinator = CampaignCoordinator::start(
                    &workload,
                    &config,
                    &CampaignOrigin::Genesis,
                    &mut stream,
                    None,
                    CampaignExecutionOptions {
                        checkpoints: Some(crate::search::checkpoint::CheckpointPlan {
                            directory: directory.clone(),
                            every: std::num::NonZeroU64::new(1),
                            on_marks: false,
                            on_top_progress: false,
                            checkpoint_owner: None,
                            max_image_bytes: 64 << 20,
                            before_write: Some(super::super::checkpoint::publisher::TestWriteHook(
                                Arc::new(move |_| {
                                    entered_sender.send(()).unwrap();
                                    release.lock().unwrap().recv().unwrap();
                                }),
                            )),
                        }),
                        ..CampaignExecutionOptions::default()
                    },
                )
                .unwrap();
                assert!(coordinator.reserve_next().unwrap());
                let job = coordinator.plan(1).pop().unwrap();
                let mut target = workload.new_target().unwrap();
                coordinator
                    .admit(run_job(&workload, &config, &mut target, &job).unwrap())
                    .unwrap();
                entered.recv_timeout(Duration::from_secs(10)).unwrap();
                let next = matches!(operation, "fail" | "admit-dispatch")
                    .then(|| coordinator.plan(1).pop().unwrap());
                let counted = if operation == "admit-dispatch" {
                    let outcome =
                        run_job(&workload, &config, &mut target, next.as_ref().unwrap()).unwrap();
                    let records =
                        super::CompletedRecords::of_result::<TestWorkload>(&outcome.result);
                    Some((outcome, records))
                } else {
                    None
                };
                if ready {
                    released.send(()).unwrap();
                    wait_for_checkpoint_worker(&coordinator);
                } else {
                    assert!(!coordinator.checkpoint_worker_finished());
                    let released = Mutex::new(Some(released));
                    *targets.integrity_failure_hook.lock().unwrap() = Some(Arc::new(move || {
                        if let Some(released) = released.lock().unwrap().take() {
                            released.send(()).unwrap();
                        }
                    }));
                }
                targets
                    .integrity_invalidated
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                let error = match operation {
                    "reserve" => coordinator.reserve_next().unwrap_err(),
                    "dispatch" => coordinator
                        .reserve_next_dispatch(&mut |_, _| {
                            panic!("dispatch after integrity failure")
                        })
                        .unwrap_err(),
                    "fail" => coordinator
                        .fail(next.unwrap().reservation, "worker error".into())
                        .unwrap_err(),
                    "drain-stream" => coordinator.drain_background_stream().unwrap_err(),
                    "dispatch-queued" => coordinator
                        .dispatch_queued(&mut |_, _| panic!("dispatch after integrity failure"))
                        .unwrap_err(),
                    "admit-dispatch" => {
                        let (outcome, records) = counted.unwrap();
                        let token = coordinator.reservation_token(outcome.reservation).unwrap();
                        coordinator
                            .admit_dispatch(outcome, records, token, &mut |_, _| {
                                panic!("dispatch after integrity failure")
                            })
                            .unwrap_err()
                    }
                    _ => unreachable!(),
                };
                assert!(
                    error.is::<super::super::checkpoint::publisher::CampaignPersistenceFailure>()
                );
                assert!(error.source().unwrap().is::<TestIntegrityViolation>());
                assert!(coordinator.plan(usize::MAX).is_empty());
                let final_error = coordinator.finish(Vec::new()).unwrap_err();
                assert!(
                    final_error
                        .is::<super::super::checkpoint::publisher::CampaignPersistenceFailure>()
                );
                assert!(final_error.source().unwrap().is::<TestIntegrityViolation>());
                assert!(std::str::from_utf8(&stream).unwrap().lines().all(|line| {
                    serde_json::from_str::<serde_json::Value>(line).unwrap()["event"] != "end"
                }));
                std::fs::remove_dir_all(directory).unwrap();
            }
        }
    }

    #[test]
    fn a_direct_coordinator_cannot_complete_after_handing_back_a_publication_failure() {
        let directory =
            std::env::temp_dir().join(format!("direct-checkpoint-poison-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(directory.join("000000000001-interval.ckpt")).unwrap();
        let mut config = three_worker_config();
        config.window = 1;
        config.execution_budget = 1;
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut stream = Vec::new();
        let mut coordinator = CampaignCoordinator::start(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
            CampaignExecutionOptions {
                checkpoints: Some(crate::search::checkpoint::CheckpointPlan {
                    directory: directory.clone(),
                    every: std::num::NonZeroU64::new(1),
                    on_marks: false,
                    on_top_progress: false,
                    checkpoint_owner: None,
                    max_image_bytes: 64 << 20,
                    before_write: None,
                }),
                ..CampaignExecutionOptions::default()
            },
        )
        .unwrap();
        assert!(coordinator.reserve_next().unwrap());
        let job = coordinator.plan(1).pop().unwrap();
        let mut target = workload.new_target().unwrap();
        coordinator
            .admit(run_job(&workload, &config, &mut target, &job).unwrap())
            .unwrap();
        wait_for_checkpoint_worker(&coordinator);
        assert!(
            coordinator
                .reserve_next()
                .unwrap_err()
                .is::<super::super::checkpoint::publisher::PersistenceFailure>()
        );
        coordinator.watch_admissions();
        coordinator.unwatch_admissions();
        assert!(coordinator.plan(usize::MAX).is_empty());
        assert!(coordinator.reserve_next().is_err());
        assert!(
            coordinator
                .fail(job.reservation, "another worker error".into())
                .is_err()
        );
        assert!(coordinator.finish(Vec::new()).is_err());
        assert!(!directory.join("000000000001-final.ckpt").exists());
        assert!(std::str::from_utf8(&stream).unwrap().lines().all(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["event"] != "end"
        }));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn direct_coordinator_gap_finalization_keeps_primary_and_pending_writer_failure() {
        let directory =
            std::env::temp_dir().join(format!("direct-checkpoint-gap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(directory.join("000000000001-interval.ckpt")).unwrap();
        let (entered_sender, entered) = mpsc::channel();
        let (released, release) = mpsc::channel();
        let release = Mutex::new(release);
        let mut config = three_worker_config();
        config.window = 1;
        config.execution_budget = 3;
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut stream = Vec::new();
        let mut coordinator = CampaignCoordinator::start(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
            CampaignExecutionOptions {
                checkpoints: Some(crate::search::checkpoint::CheckpointPlan {
                    directory: directory.clone(),
                    every: std::num::NonZeroU64::new(1),
                    on_marks: false,
                    on_top_progress: false,
                    checkpoint_owner: None,
                    max_image_bytes: 64 << 20,
                    before_write: Some(super::super::checkpoint::publisher::TestWriteHook(
                        Arc::new(move |_| {
                            entered_sender.send(()).unwrap();
                            release.lock().unwrap().recv().unwrap();
                        }),
                    )),
                }),
                ..CampaignExecutionOptions::default()
            },
        )
        .unwrap();
        assert!(coordinator.reserve_next().unwrap());
        let job = coordinator.plan(1).pop().unwrap();
        let mut target = workload.new_target().unwrap();
        coordinator
            .admit(run_job(&workload, &config, &mut target, &job).unwrap())
            .unwrap();
        entered.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(!coordinator.finished());
        released.send(()).unwrap();
        let error = coordinator.finish(Vec::new()).unwrap_err();
        assert!(error.is::<super::super::checkpoint::publisher::CampaignPersistenceFailure>());
        assert!(
            error
                .source()
                .unwrap()
                .to_string()
                .contains("admission gap")
        );
        assert!(
            error
                .to_string()
                .contains("checkpoint finalization also failed")
        );
        assert!(std::str::from_utf8(&stream).unwrap().lines().all(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["event"] != "end"
        }));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn dropping_direct_coordinator_joins_its_last_admission_image_without_writing_end() {
        let directory =
            std::env::temp_dir().join(format!("direct-checkpoint-drop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let mut config = three_worker_config();
        config.window = 1;
        config.execution_budget = 3;
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut stream = Vec::new();
        let mut coordinator = CampaignCoordinator::start(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
            CampaignExecutionOptions {
                checkpoints: Some(crate::search::checkpoint::CheckpointPlan {
                    directory: directory.clone(),
                    every: std::num::NonZeroU64::new(1),
                    on_marks: false,
                    on_top_progress: false,
                    checkpoint_owner: None,
                    max_image_bytes: 64 << 20,
                    before_write: None,
                }),
                ..CampaignExecutionOptions::default()
            },
        )
        .unwrap();
        assert!(coordinator.reserve_next().unwrap());
        let job = coordinator.plan(1).pop().unwrap();
        let mut target = workload.new_target().unwrap();
        coordinator
            .admit(run_job(&workload, &config, &mut target, &job).unwrap())
            .unwrap();
        drop(coordinator);
        let reader = crate::search::checkpoint::CheckpointReader::open(
            &directory.join("000000000001-interval.ckpt"),
        )
        .unwrap();
        assert_eq!(reader.header.executions, 1);
        assert_eq!(reader.header.admitted, 1);
        assert!(std::str::from_utf8(&stream).unwrap().lines().all(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["event"] != "end"
        }));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_delayed_checkpoint_writer_preserves_the_admission_version_replay_and_resume() {
        for direct in [false, true] {
            struct AdmissionSignal(mpsc::Sender<u64>);
            impl AdmissionSignal {
                fn observe(&self, campaign: &CampaignCoordinator<'_, TestWorkload>) {
                    let executions = campaign.progress().unwrap().executions;
                    if executions >= 6 {
                        let _ = self.0.send(executions);
                    }
                }
            }
            impl super::CampaignWatch<TestWorkload> for AdmissionSignal {
                type Note = ();

                fn run(
                    &self,
                    _job: &JobSpec<TestWorkload>,
                    run: impl FnOnce() -> Result<JobOutcome<TestWorkload>, Box<dyn Error>>,
                ) -> super::WatchedJob<TestWorkload, ()> {
                    Ok((run()?, ()))
                }

                fn admitted(&self, _: (), campaign: &mut CampaignCoordinator<'_, TestWorkload>) {
                    self.observe(campaign);
                }
            }
            let asynchronous = std::env::temp_dir().join(format!(
                "campaign-delayed-checkpoint-{}-{direct}",
                std::process::id()
            ));
            let synchronous = std::env::temp_dir().join(format!(
                "campaign-sync-checkpoint-{}-{direct}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&asynchronous);
            let _ = std::fs::remove_dir_all(&synchronous);
            let mut config = three_worker_config();
            config.execution_budget = 12;
            let workload = TestWorkload {
                bootstrap_objective: false,
                targets: None,
            };
            let make_plan =
                |directory, max_image_bytes| crate::search::checkpoint::CheckpointPlan {
                    directory,
                    every: std::num::NonZeroU64::new(5),
                    on_marks: false,
                    on_top_progress: false,
                    checkpoint_owner: None,
                    max_image_bytes,
                    before_write: None,
                };
            let mut control_stream = Vec::new();
            let control = run_campaign_checkpointed_with_options(
                &workload,
                &config,
                &CampaignOrigin::Genesis,
                &mut control_stream,
                None,
                CampaignExecutionOptions {
                    checkpoints: Some(make_plan(synchronous.clone(), 1)),
                    ..CampaignExecutionOptions::default()
                },
            )
            .unwrap();
            let (entered_sender, entered) = mpsc::channel();
            let (released, release_receiver) = mpsc::channel();
            let (later_sender, later) = mpsc::channel();
            let release_receiver = Mutex::new(release_receiver);
            let mut delayed_plan = make_plan(asynchronous.clone(), 64 << 20);
            delayed_plan.before_write = Some(super::super::checkpoint::publisher::TestWriteHook(
                Arc::new(move |ordinal| {
                    if ordinal == 1 {
                        entered_sender.send(()).unwrap();
                        release_receiver.lock().unwrap().recv().unwrap();
                    }
                }),
            ));
            let (live, stream) = std::thread::scope(|scope| {
                let run = scope.spawn(|| {
                    let mut stream = Vec::new();
                    let signal = AdmissionSignal(later_sender);
                    let options = CampaignExecutionOptions {
                        checkpoints: Some(delayed_plan),
                        ..CampaignExecutionOptions::default()
                    };
                    let result = (|| {
                        if direct {
                            let mut campaign = CampaignCoordinator::start(
                                &workload,
                                &config,
                                &CampaignOrigin::Genesis,
                                &mut stream,
                                None,
                                options,
                            )?;
                            while campaign.reserve_next()? {}
                            let mut target = workload.new_target()?;
                            while !campaign.finished() {
                                let jobs = campaign.plan(2);
                                if jobs.is_empty() {
                                    return Err("direct coordinator has no runnable jobs".into());
                                }
                                for job in jobs.into_iter().rev() {
                                    campaign.admit(run_job(
                                        &workload,
                                        &config,
                                        &mut target,
                                        &job,
                                    )?)?;
                                    signal.observe(&campaign);
                                }
                            }
                            campaign.finish(Vec::new())
                        } else {
                            super::run_campaign_watched(
                                &workload,
                                &config,
                                &CampaignOrigin::Genesis,
                                &mut stream,
                                None,
                                options,
                                &signal,
                            )
                        }
                    })();
                    (
                        result.map_err(|error: Box<dyn Error>| error.to_string()),
                        stream,
                    )
                });
                let entered = entered.recv_timeout(Duration::from_secs(10));
                let later = later.recv_timeout(Duration::from_secs(10));
                let _ = released.send(());
                let (result, stream) = run.join().unwrap();
                entered.expect("the first immutable image reached the writer");
                let executions = later.expect(
                    "another admission changed live state while its preceding image was held",
                );
                assert!((6..10).contains(&executions));
                (result.unwrap(), stream)
            });
            assert_eq!(stream, control_stream);
            assert_eq!(
                serde_json::to_vec(&live.0.archive).unwrap(),
                serde_json::to_vec(&control.0.archive).unwrap()
            );
            assert_eq!(live.1.to_bytes().unwrap(), control.1.to_bytes().unwrap());
            let replayed = replay_campaign_checkpointed(&workload, &stream, None, None).unwrap();
            assert_eq!(replayed.1.to_bytes().unwrap(), live.1.to_bytes().unwrap());
            let saved = asynchronous.join("000000000005-interval.ckpt");
            let mut resumed_stream = Vec::new();
            let resumed = run_campaign_checkpointed(
                &workload,
                &config,
                &CampaignOrigin::SearchCheckpoint {
                    path: saved,
                    expected_sha256: None,
                },
                &mut resumed_stream,
                None,
            )
            .unwrap();
            assert_eq!(
                serde_json::to_vec(&resumed.0.archive).unwrap(),
                serde_json::to_vec(&control.0.archive).unwrap()
            );
            assert_eq!(resumed.1.to_bytes().unwrap(), control.1.to_bytes().unwrap());
            std::fs::remove_dir_all(asynchronous).unwrap();
            std::fs::remove_dir_all(synchronous).unwrap();
        }
    }

    #[test]
    fn a_failed_final_checkpoint_cannot_write_end_or_return_a_completed_campaign() {
        let directory = std::env::temp_dir().join(format!(
            "campaign-final-checkpoint-refusal-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(directory.join("000000000001-final.ckpt")).unwrap();
        let mut config = three_worker_config();
        config.execution_budget = 1;
        let mut stream = Vec::new();
        let error = run_campaign_checkpointed_with_options(
            &TestWorkload {
                bootstrap_objective: false,
                targets: None,
            },
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
            CampaignExecutionOptions {
                checkpoints: Some(crate::search::checkpoint::CheckpointPlan {
                    directory: directory.clone(),
                    every: None,
                    on_marks: false,
                    on_top_progress: false,
                    checkpoint_owner: None,
                    max_image_bytes: 64 << 20,
                    before_write: None,
                }),
                ..CampaignExecutionOptions::default()
            },
        )
        .unwrap_err();
        assert!(error.is::<super::super::checkpoint::publisher::PersistenceFailure>());
        assert!(std::str::from_utf8(&stream).unwrap().lines().all(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["event"] != "end"
        }));
        assert_eq!(job_lines(&stream).len(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn the_continuation_draw_takes_a_fixed_one_reservation_in_four() {
        let taken = (0..4096)
            .filter(|reservation| draws_continuation(11, *reservation))
            .count();
        assert!(
            (896..1152).contains(&taken),
            "one in four of 4096 reservations, got {taken}"
        );
    }

    #[test]
    fn the_continuation_draw_is_seeded_by_the_campaign_and_the_reservation() {
        assert_eq!(draws_continuation(11, 3), draws_continuation(11, 3));
        assert!(
            (0..512).any(|reservation| draws_continuation(11, reservation)
                != draws_continuation(12, reservation))
        );
    }

    #[test]
    fn the_highest_preference_draw_is_one_pop_in_four_and_independent_of_the_attempt() {
        let taken = (0..4096)
            .filter(|reservation| draws_highest_preference(11, *reservation))
            .count();
        assert!(
            (896..1152).contains(&taken),
            "one in four of 4096 reservations, got {taken}"
        );
        assert!(
            (0..512).any(|reservation| draws_highest_preference(11, reservation)
                != draws_continuation(11, reservation))
        );
    }

    #[test]
    fn a_continuation_outcome_leaves_the_draw_weights_alone() {
        let mut energy = MixtureEnergy::default();
        let mixture = DrawMixture::EnergySplice { scale: 4 };
        let before = energy.splice_weights(4);
        for seed in 0..64_u64 {
            record_mixture_outcome(
                &mut energy,
                mixture,
                SelectorPath::Continuation,
                seed,
                (128, 128),
                seed % 3 == 0,
                10,
            )
            .expect("continuation outcome");
        }
        assert_eq!(energy.splice_weights(4), before);
        assert_eq!(
            energy.biased_weight(4),
            MixtureEnergy::default().biased_weight(4)
        );
        for _ in 0..16 {
            record_mixture_outcome(
                &mut energy,
                mixture,
                SelectorPath::Tiers,
                1,
                (128, 128),
                false,
                10,
            )
            .expect("ordinary outcome");
        }
        assert_ne!(energy.splice_weights(4), before);
    }
    #[test]
    fn replay_rejects_a_forged_initial_archive_skip_suffix_limit() {
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut base = three_worker_config();
        base.execution_budget = 20;
        base.window = 3;
        let mut source_stream = Vec::new();
        let (source, _) = run_campaign_checkpointed(
            &workload,
            &base,
            &CampaignOrigin::Genesis,
            &mut source_stream,
            None,
        )
        .unwrap();
        let source_archive =
            ArchiveSource::from_bytes(&serde_json::to_vec(&source.archive).unwrap()).unwrap();
        let origin = CampaignOrigin::Archive {
            path: "source.archive".to_owned(),
            source: source_archive.clone(),
            checkpoint: None,
        };
        let config = CampaignConfig {
            campaign_seed: 196,
            workers: 3,
            execution_budget: 8,
            host: "test".to_owned(),
            wall_budget: None,
            stop_rollout_on_objective: true,
            stop_campaign_on_objective: false,
            archive_entry_limit: 16,
            window: 3,
            memory_budget_mib: None,
            materialize_final_artifacts: true,
            run: (),
            mixture: DrawMixture::AlphabetOnly,
            retention: RetentionPolicy::Unprobed,
            objective_witness_path: None,
        };
        let mut stream = Vec::new();
        run_campaign_checkpointed(&workload, &config, &origin, &mut stream, None).unwrap();
        replay_campaign_checkpointed(&workload, &stream, Some(&source_archive), None).unwrap();
        let mut records = std::str::from_utf8(&stream)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records[1]["event"], "skip");
        assert_eq!(records[1]["reservation"], 1);
        assert_eq!(records[1]["suffix_limit"], 1);
        assert_eq!(records[2]["event"], "job");
        records[1]["suffix_limit"] = serde_json::json!(2);
        let forged = records
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n");
        let error =
            replay_campaign_checkpointed(&workload, forged.as_bytes(), Some(&source_archive), None)
                .expect_err("replay certified a skip limit live selection did not record")
                .to_string();
        assert!(error.contains("skip suffix limit"), "{error}");
    }

    #[test]
    fn archive_replay_rejects_changed_source_evidence_under_the_original_digest() {
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut config = three_worker_config();
        config.execution_budget = 5;
        config.window = 2;
        let mut source_stream = Vec::new();
        let (source, _) = run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut source_stream,
            None,
        )
        .unwrap();
        let source_report = source.archive;
        let source_bytes = serde_json::to_vec(&source_report).unwrap();
        config.execution_budget = 0;
        let source = ArchiveSource::from_bytes(&source_bytes).unwrap();
        let origin = CampaignOrigin::Archive {
            path: "source.archive".to_owned(),
            source: source.clone(),
            checkpoint: None,
        };
        let mut stream = Vec::new();
        let (live, _) = run_campaign_checkpointed(&workload, &config, &origin, &mut stream, None)
            .expect("record archive-origin campaign");
        assert_eq!(live.archive.marker, 0);
        assert_eq!(
            live.origin.archive_sha256.as_deref(),
            Some(source.file_sha256())
        );
        replay_campaign_checkpointed(&workload, &stream, Some(&source), None)
            .expect("unchanged source replays");
        let mut prefix_lines = std::str::from_utf8(&stream)
            .unwrap()
            .lines()
            .collect::<Vec<_>>();
        let end: serde_json::Value = serde_json::from_str(prefix_lines.pop().unwrap()).unwrap();
        assert_eq!(end["event"], "end");
        let prefix = prefix_lines.join("\n");
        let (prefix_replay, _) =
            replay_campaign_prefix_checkpointed(&workload, prefix.as_bytes(), Some(&source), None)
                .expect("unchanged source prefix replays");
        assert_eq!(prefix_replay.archive.marker, 0);
        let mut changed = source_report;
        changed.marker = 37;
        assert_eq!(changed.entries, source.report().entries);
        assert_eq!(
            workload.resume_input(&changed).unwrap(),
            workload.resume_input(source.report()).unwrap()
        );
        let changed_source =
            ArchiveSource::from_bytes(&serde_json::to_vec(&changed).unwrap()).unwrap();
        assert_ne!(source.file_sha256(), changed_source.file_sha256());
        let error = replay_campaign_checkpointed(&workload, &stream, Some(&changed_source), None)
            .expect_err("replay accepted changed source evidence under the recorded digest")
            .to_string();
        assert!(error.contains("source archive bytes"), "{error}");
        let error = replay_campaign_prefix_checkpointed(
            &workload,
            prefix.as_bytes(),
            Some(&changed_source),
            None,
        )
        .expect_err("prefix replay accepted changed source evidence")
        .to_string();
        assert!(error.contains("source archive bytes"), "{error}");
        let mut whitespace_changed = source_bytes;
        whitespace_changed.extend_from_slice(b" \n\t");
        let same_report_source = ArchiveSource::from_bytes(&whitespace_changed).unwrap();
        assert_eq!(same_report_source.report(), source.report());
        assert_ne!(same_report_source.file_sha256(), source.file_sha256());
        let error =
            replay_campaign_checkpointed(&workload, &stream, Some(&same_report_source), None)
                .expect_err("replay accepted semantically equal but byte-distinct source archive")
                .to_string();
        assert!(error.contains("source archive bytes"), "{error}");
    }
    #[test]
    fn job_observer_delivery_failures_preserve_logical_bytes_and_complete_counts() {
        use crate::search::job_observation::{
            JobObserver, ObservationDiagnostics, ObservationSinkError,
        };
        let mut config = three_worker_config();
        config.execution_budget = 8;
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: None,
        };
        let mut baseline = Vec::new();
        let expected = run_campaign_checkpointed(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut baseline,
            None,
        )
        .unwrap();
        let records = job_lines(&baseline);
        let by_reservation = records
            .iter()
            .map(|(_, job)| (job.reservation as usize, job))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(expected.0.telemetry.coordinator["replay_actions"], 0);
        assert!(by_reservation.values().any(|job| job.parent_id != 0));
        let mut expected_finish = std::collections::BTreeMap::new();
        for reservation in 0..config.window {
            expected_finish.insert(reservation, by_reservation[&reservation].execution_work * 2);
        }
        let mut next = config.window;
        for (_, job) in &records {
            let finish = expected_finish[&(job.reservation as usize)];
            if next < config.execution_budget as usize {
                expected_finish.insert(next, finish + by_reservation[&next].execution_work * 2);
                next += 1;
            }
        }
        for fail in [0, 1, 2] {
            let diagnostics = ObservationDiagnostics::default();
            let mut seen = Vec::new();
            let mut sink = |row: crate::search::job_observation::CompletionObservation<()>| {
                seen.push(row);
                if fail == 1 {
                    return Err(ObservationSinkError);
                }
                if fail == 2 {
                    panic!("sink panic");
                }
                Ok(())
            };
            let mut stream = Vec::new();
            let actual = super::run_campaign_checkpointed_observed(
                &workload,
                &config,
                &CampaignOrigin::Genesis,
                &mut stream,
                None,
                CampaignExecutionOptions::default(),
                Some(JobObserver {
                    diagnostics: diagnostics.clone(),
                    sink: &mut sink,
                }),
            )
            .unwrap();
            assert_eq!(stream, baseline);
            assert_eq!(actual, expected);
            let counts = diagnostics.snapshot();
            assert_eq!(counts.eligible, 8);
            assert_eq!(counts.collected, 8);
            assert_eq!(counts.collected, counts.delivered + counts.discarded);
            if fail == 0 {
                assert_eq!(seen.len(), 8);
                assert_eq!(counts.delivered, 8);
                assert_eq!(counts.discarded, 0);
            } else {
                assert_eq!(seen.len(), 1);
                assert_eq!(counts.discarded, 8);
                assert_eq!(counts.sink_errors, u64::from(fail == 1));
                assert_eq!(counts.sink_panics, u64::from(fail == 2));
            }
            for row in &seen {
                let job = by_reservation[&row.reservation];
                assert!(row.worker < config.workers);
                assert_eq!(row.parent_id, job.parent_id);
                assert_eq!(row.snapshot_id, job.parent_id);
                assert_eq!(row.planned_finish, expected_finish[&row.reservation]);
                assert!(!row.job_failed);
                assert!(row.receipt.payload.is_some());
            }
            if fail == 0 {
                let mut per_worker = vec![0u64; config.workers as usize];
                for row in &seen {
                    per_worker[row.worker as usize] += 1;
                }
                assert_eq!(
                    per_worker,
                    actual
                        .0
                        .telemetry
                        .workers
                        .iter()
                        .map(|worker| worker.jobs)
                        .collect::<Vec<_>>()
                );
                assert_eq!(
                    seen.iter()
                        .map(|row| row.reservation)
                        .collect::<BTreeSet<_>>(),
                    (0..config.execution_budget as usize).collect()
                );
            }
        }
    }
    #[test]
    fn job_observer_panic_does_not_mask_the_primary_execution_error() {
        use crate::search::job_observation::{JobObserver, ObservationDiagnostics};
        let mut config = three_worker_config();
        config.workers = 1;
        config.window = 1;
        config.execution_budget = 1;
        let targets = Arc::new(LiveTargets::default());
        *targets.action_hook.lock().unwrap() =
            Some(Arc::new(|_| Err("primary execution refused".to_owned())));
        let workload = TestWorkload {
            bootstrap_objective: false,
            targets: Some(targets),
        };
        let diagnostics = ObservationDiagnostics::default();
        let mut sink = |row: crate::search::job_observation::CompletionObservation<()>| {
            assert!(row.job_failed);
            panic!("observer failure")
        };
        let error = super::run_campaign_checkpointed_observed(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut Vec::new(),
            None,
            CampaignExecutionOptions::default(),
            Some(JobObserver {
                diagnostics: diagnostics.clone(),
                sink: &mut sink,
            }),
        )
        .unwrap_err();
        assert!(error.to_string().contains("primary execution refused"));
        let counts = diagnostics.snapshot();
        assert_eq!(counts.eligible, 1);
        assert_eq!(counts.collected, 1);
        assert_eq!(counts.sink_panics, 1);
        assert_eq!(counts.discarded, 1);
        assert_eq!(counts.delivered, 0);
    }

    mod expansion_tests {
        include!("campaign_expansion_tests.rs");
    }
}
