// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt::Debug,
    io::Write,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::search::archive::{
    Archive, ArchiveCandidate, ArchiveEntryReport, ArchiveKey, CampaignSpliceTail, Input,
    ProgressPoint, RetentionPolicy, SelectorAccounting, SelectorDraw, SelectorPath, SelectorPolicy,
    retention_policy_from_identifier, retention_policy_identifier, selector_policy_identifier,
};
use crate::search::draw::{
    DrawMixture, EnergyStrategy, MixtureDraw, MixtureEnergy, SuffixShape,
    draw_mixture_from_identifier, draw_mixture_identifier, energy_strategy,
    suffix_shape_from_identifier, suffix_shape_identifier,
};

pub const SPLICE_ACTION_CAP: usize = 128;
use crate::search::parallel::{ResultSlots, with_worker_pool};
use crate::search::rand::RomuDuoJrRand;

pub type CampaignOutcome<G> = (
    CampaignModeReport<<G as CampaignTypes>::Action, <G as CampaignTypes>::ArchiveReport>,
    SnapshotCheckpoint<<G as CampaignTypes>::Snapshot>,
);

pub type InitialDrawState<G> = (
    <G as CampaignTypes>::DrawState,
    Option<<G as CampaignTypes>::DrawHeader>,
);

pub const CAMPAIGN_SCHEMA_VERSION: u32 = 2;

pub const CAMPAIGN_SCHEDULE_IDENTITY: &str = "jobs are selected into a deterministic sliding \
     window and admitted in reservation order; physical workers drain the window dynamically, \
     but host completion order cannot reach campaign state; the same seed, configuration, \
     origin, and workload bytes produce the same recorded stream";

const CAMPAIGN_PROGRESS_POLICY: &str = "mechanical_watermark_bounded_1024_v2";
const ORIGIN_GENESIS: &str = "genesis";
const ORIGIN_SNAPSHOT_ROOT: &str = "snapshot_root";
const ORIGIN_ARCHIVE: &str = "archive";

#[derive(Clone, Copy, Debug, Default)]
pub enum ResultBuffering {
    #[default]
    OnePerWorker,
    TwoPerWorker,
}

impl ResultBuffering {
    const fn capacity(self) -> usize {
        match self {
            Self::OnePerWorker => 1,
            Self::TwoPerWorker => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CampaignExecutionOptions {
    pub work_budget: Option<u64>,
    pub result_buffering: ResultBuffering,
}

pub const DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER: usize = 1;

const fn admission_window_depth(workers: usize, reservations_per_worker: usize) -> usize {
    workers.saturating_mul(reservations_per_worker)
}

const SCHEDULE_POLICY_WINDOW_SUFFIX: &str = "_per_worker_v3";
const SCHEDULE_POLICY_WINDOW_PREFIX: &str = "deterministic_window_";
const DEFAULT_MIXTURE_WEIGHT: u8 = 128;

fn schedule_policy_identifier(reservations_per_worker: usize) -> String {
    format!(
        "{SCHEDULE_POLICY_WINDOW_PREFIX}{reservations_per_worker}{SCHEDULE_POLICY_WINDOW_SUFFIX}"
    )
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

fn progress_checkpoint_due(executions: u64) -> bool {
    executions > 0 && (executions == 1 || executions.is_multiple_of(PROGRESS_CHECKPOINT_INTERVAL))
}

pub const RESUME_IDENTIFIER: &str = "whole_tree";

pub const SNAPSHOT_ROOT_RESUME_IDENTIFIER: &str = "snapshot_root";

pub type WorkloadPolicies = BTreeMap<String, String>;

pub trait CampaignTypes: Sync {
    type Target;
    type Action: Copy + Ord + Debug + Eq + Send + Sync + Serialize + DeserializeOwned;
    type Key: ArchiveKey + Debug + Eq + Send + Sync;
    type Milestones: Copy + Default + Debug + Eq + Send + Sync + Serialize + DeserializeOwned;
    type Progress: Copy + Default + Debug + Eq + Send + Sync + Serialize + DeserializeOwned;
    type Snapshot: Clone + Debug + Eq + Send + Sync + Serialize + DeserializeOwned;
    type Observations: Clone + Debug + Eq + Send + Sync + Serialize;
    type Evidence: Clone + Default;
    type ArchiveReport: Clone;
    type Run: Clone + Sync;
    type DrawState;
    type DrawCheckpoint: Clone + Debug + Eq + Send + Sync + Serialize + DeserializeOwned;
    type DrawHeader: Clone + Debug + Eq + Serialize + DeserializeOwned;
}

pub trait Reporting: CampaignTypes {
    fn diagnostics(_evidence: &Self::Evidence) -> Option<serde_json::Value> {
        None
    }
    fn merge_witness_diagnostics(
        _evidence: &mut Self::Evidence,
        _observations: &[Self::Observations],
        _sequence: u64,
    ) {
    }
    fn stream_format(&self) -> &'static str;
    fn checkpoint_format(&self) -> &'static str;
    fn workload_identity_sha256(&self) -> String;
    fn action_cost_unit(&self) -> &'static str;
    fn execution_work_unit(&self) -> &'static str;
    fn result_sha256(&self, result: &CampaignJobResult<Self>) -> Result<String, Box<dyn Error>>;
    fn archive_report(
        &self,
        evidence: &Self::Evidence,
        state: ArchiveReportState<Self>,
    ) -> Self::ArchiveReport;
}

pub trait InputPolicy: CampaignTypes {
    fn max_action_limit(&self) -> usize;
    fn max_action_cost(&self) -> u64;
    fn policies(&self, run: &Self::Run) -> WorkloadPolicies;
    fn resolve_recorded(&self, policies: &WorkloadPolicies) -> Result<Self::Run, Box<dyn Error>>;
    fn draw_state_memory_reserve_bytes(&self, run: &Self::Run, max_actions: usize) -> usize;
    fn draw_state_memory_bytes(&self, state: &Self::DrawState) -> usize;

    fn initial_draw_state(
        &self,
        run: &Self::Run,
        origin: Option<(&str, &Self::ArchiveReport)>,
    ) -> Result<InitialDrawState<Self>, Box<dyn Error>>;
    fn draw_checkpoint(
        &self,
        state: &Self::DrawState,
    ) -> Result<Option<Self::DrawCheckpoint>, Box<dyn Error>> {
        let _ = state;
        Ok(None)
    }
    fn draw_checkpoint_version(&self, _checkpoint: &Self::DrawCheckpoint) -> u64 {
        0
    }
    fn expand_suffix(
        &self,
        run: &Self::Run,
        state: &Self::DrawState,
        shape: SuffixShape,
        mixture: MixtureDraw,
        mutation_seed: u64,
    ) -> Result<Vec<Self::Action>, Box<dyn Error>>;
    fn expand_suffix_recorded(
        &self,
        run: &Self::Run,
        state: &Self::DrawState,
        shape: SuffixShape,
        mixture: MixtureDraw,
        before: Option<&Self::DrawCheckpoint>,
        mutation_seed: u64,
    ) -> Result<Vec<Self::Action>, Box<dyn Error>> {
        if before.is_some() {
            return Err("recorded stream carries an unsupported draw checkpoint".into());
        }
        self.expand_suffix(run, state, shape, mixture, mutation_seed)
    }
    fn finish_stream_record(
        &self,
        run: &Self::Run,
        state: &mut Self::DrawState,
        retained: &[(usize, &[Self::Action])],
    ) -> Result<Option<Self::DrawCheckpoint>, Box<dyn Error>> {
        let _ = (run, state, retained);
        Ok(None)
    }
    fn retained_inputs_need_full(&self, _run: &Self::Run) -> bool {
        false
    }
    fn remember_draw_version(
        &self,
        state: &mut Self::DrawState,
        required: &BTreeSet<u64>,
    ) -> Result<(), Box<dyn Error>> {
        let _ = (state, required);
        Ok(())
    }
}

pub trait TargetExecution: CampaignTypes {
    fn new_target(&self) -> Result<Self::Target, String>;
    fn reset(&self, target: &mut Self::Target);
    fn restore(
        &self,
        target: &mut Self::Target,
        snapshot: &Self::Snapshot,
    ) -> Result<(), Box<dyn Error>>;
    fn execution_work(&self, target: &Self::Target) -> u64;
    fn action_cost_fn(&self) -> fn(&Self::Action) -> u64;
    fn snapshot_memory_charge(snapshot: &Self::Snapshot) -> usize;
    fn apply_action(
        &self,
        target: &mut Self::Target,
        action: &Self::Action,
        milestones: &mut Self::Milestones,
    ) -> Result<(), Box<dyn Error>>;
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
    #[allow(clippy::too_many_arguments)]
    fn execute_job(
        &self,
        run: &Self::Run,
        target: &mut Self::Target,
        origin_snapshot: &Self::Snapshot,
        replay: &[Self::Action],
        parent_actions: usize,
        parent_milestones: Self::Milestones,
        suffix: &[Self::Action],
        max_actions: usize,
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
            parent_actions,
            parent_milestones,
            suffix,
            max_actions,
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
        file_sha256: String,
        report: Box<G::ArchiveReport>,
        checkpoint: Option<CampaignCheckpoint<G::Snapshot>>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CampaignCheckpoint<S> {
    pub path: String,
    pub file_sha256: String,
    pub snapshots: SnapshotCheckpoint<S>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "S: Serialize + DeserializeOwned")]
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

impl<S: Serialize + DeserializeOwned> SnapshotCheckpoint<S> {
    pub fn to_bytes(&self) -> Result<Vec<u8>, Box<dyn Error>> {
        Ok(postcard::to_allocvec(self)?)
    }

    pub fn from_bytes(bytes: &[u8], expected_format: &str) -> Result<Self, Box<dyn Error>> {
        let checkpoint: Self = postcard::from_bytes(bytes)?;
        if checkpoint.format != expected_format {
            return Err("snapshot checkpoint format is not recognized".into());
        }
        Ok(checkpoint)
    }
}

pub struct CampaignConfig<G: Workload + ?Sized> {
    pub campaign_seed: u64,
    pub workers: u32,
    pub execution_budget: u64,
    pub action_limit: usize,
    pub host: String,
    pub wall_budget: Option<Duration>,
    pub stop_rollout_on_objective: bool,
    pub stop_campaign_on_objective: bool,
    pub archive_entry_limit: usize,
    pub reservations_per_worker: usize,
    pub memory_budget_mib: Option<usize>,
    pub materialize_final_artifacts: bool,
    pub run: G::Run,
    pub suffix: SuffixShape,
    pub mixture: DrawMixture,
    pub retention: RetentionPolicy,
    pub selector: SelectorPolicy,
    pub objective_witness_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "T: Serialize + DeserializeOwned")]
pub struct CampaignStreamHeader<T> {
    pub schema_version: u32,
    pub format: String,
    pub campaign_seed: u64,
    pub workers: u32,
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
    pub resume_input_sha256: String,
    pub resume_actions: usize,
    pub execution_budget: u64,
    pub stop_rollout_on_objective: bool,
    pub stop_campaign_on_objective: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_budget: Option<u64>,
    pub wall_budget_seconds: Option<u64>,
    pub action_limit: usize,
    pub archive_entry_limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_budget_mib: Option<usize>,
    pub resume_policy: String,
    pub suffix_policy: String,
    pub mixture_policy: String,
    #[serde(flatten)]
    pub workload_policies: WorkloadPolicies,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draw_header: Option<T>,
    pub retention_policy: String,
    pub parent_scheduler: String,
    pub executor_mode: String,
    pub worker_seed_derivation: String,
    pub workload_identity_sha256: String,
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
#[serde(bound = "C: Serialize + DeserializeOwned")]
pub struct CampaignJobRecord<C> {
    pub sequence: u64,
    pub worker: u32,
    pub parent_id: u64,
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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "C: Serialize + DeserializeOwned")]
pub struct CampaignSkipRecord<C> {
    pub worker: u32,
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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "C: Serialize + DeserializeOwned")]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CampaignStreamRecord<C> {
    Job(CampaignJobRecord<C>),
    Skip(CampaignSkipRecord<C>),
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
    pub resume_input_sha256: String,
    pub resume_actions: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "A: Serialize + DeserializeOwned + Ord + Clone, R: Serialize + DeserializeOwned")]
pub struct CampaignModeReport<A: Ord, R> {
    pub mode: String,
    pub campaign_seed: u64,
    pub workers: u32,
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
    pub action_limit: usize,
    pub archive_entry_limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_budget_mib: Option<usize>,
    pub resume_policy: String,
    pub suffix_policy: String,
    pub mixture_policy: String,
    #[serde(flatten)]
    pub workload_policies: WorkloadPolicies,
    pub retention_policy: String,
    pub parent_scheduler: String,
    pub executor_mode: String,
    pub worker_seed_derivation: String,
    pub workload_identity_sha256: String,
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
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub barren_groups: usize,
    pub jobs_per_worker: Vec<u64>,
    pub skips_per_worker: Vec<u64>,
    pub stream_sha256: String,
    pub archive: R,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TreeImportCounts {
    pub imported: u64,
    pub duplicate: u64,
    pub rejected: u64,
    pub terminal: u64,
    pub over_limit: u64,
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

fn draw_state_memory_is_within_reserve(bytes: usize, reserve: usize) -> bool {
    bytes <= reserve
}

fn resident_memory_is_within_budget(bytes: usize, memory_budget_mib: Option<usize>) -> bool {
    memory_budget_mib.is_none_or(|budget_mib| bytes <= budget_mib.saturating_mul(1024 * 1024))
}

fn retained_archive_indexes<G: Workload>(
    core: &CoordinatorCore<G>,
    decisions: &[CampaignAdmissionDecision],
) -> Vec<usize> {
    decisions
        .iter()
        .filter_map(|decision| match decision {
            CampaignAdmissionDecision::Retained { id } => core.archive.index_of_id(*id),
            _ => None,
        })
        .collect()
}

fn progress_policy_is_supported(policy: &str) -> bool {
    policy == CAMPAIGN_PROGRESS_POLICY
}

fn record_mixture_outcome(
    energy: &mut MixtureEnergy,
    mixture: DrawMixture,
    path: SelectorPath,
    mutation_seed: u64,
    mixture_weight: u8,
    splice_weight: u8,
    new_slot: bool,
) -> Result<(), Box<dyn Error>> {
    if mixture.isolates_continuations() && path == SelectorPath::Continuation {
        return Ok(());
    }
    if matches!(
        mixture,
        DrawMixture::Energy { .. }
            | DrawMixture::EnergySplice { .. }
            | DrawMixture::EnergySpliceContinuation { .. }
            | DrawMixture::EnergySpliceContinuationIsolated { .. }
    ) {
        energy.record_outcome(
            energy_strategy(mutation_seed, mixture_weight, splice_weight)?,
            new_slot,
        );
    }
    Ok(())
}

fn stop_reservations_after_objective(
    stop_campaign_on_objective: bool,
    objective_found: bool,
) -> bool {
    stop_campaign_on_objective && objective_found
}

#[derive(Serialize)]
#[serde(bound = "")]
pub struct CampaignCandidate<G: CampaignTypes + ?Sized> {
    pub key: G::Key,
    pub viable: bool,
    pub snapshot: G::Snapshot,
}

impl<G: CampaignTypes + ?Sized> PartialEq for CampaignCandidate<G> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.viable == other.viable && self.snapshot == other.snapshot
    }
}
impl<G: CampaignTypes + ?Sized> Eq for CampaignCandidate<G> {}

impl<G: CampaignTypes + ?Sized> Clone for CampaignCandidate<G> {
    fn clone(&self) -> Self {
        Self {
            key: self.key,
            viable: self.viable,
            snapshot: self.snapshot.clone(),
        }
    }
}

impl<G: CampaignTypes + ?Sized> Debug for CampaignCandidate<G> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CampaignCandidate")
            .field("key", &self.key)
            .field("viable", &self.viable)
            .field("snapshot", &self.snapshot)
            .finish()
    }
}

#[derive(Serialize)]
#[serde(bound = "")]
pub struct CampaignActionResult<G: CampaignTypes + ?Sized> {
    pub action: G::Action,
    pub observations: Vec<G::Observations>,
    pub milestones: G::Milestones,
    pub outcome: crate::search::rollout::Outcome,
    pub candidate: Option<CampaignCandidate<G>>,
}

impl<G: CampaignTypes + ?Sized> PartialEq for CampaignActionResult<G> {
    fn eq(&self, other: &Self) -> bool {
        self.action == other.action
            && self.observations == other.observations
            && self.milestones == other.milestones
            && self.outcome == other.outcome
            && self.candidate == other.candidate
    }
}
impl<G: CampaignTypes + ?Sized> Eq for CampaignActionResult<G> {}

impl<G: CampaignTypes + ?Sized> Clone for CampaignActionResult<G> {
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

impl<G: CampaignTypes + ?Sized> Debug for CampaignActionResult<G> {
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

#[derive(Serialize)]
#[serde(bound = "")]
pub struct CampaignJobResult<G: CampaignTypes + ?Sized> {
    pub actions: Vec<CampaignActionResult<G>>,
}

impl<G: CampaignTypes + ?Sized> PartialEq for CampaignJobResult<G> {
    fn eq(&self, other: &Self) -> bool {
        self.actions == other.actions
    }
}
impl<G: CampaignTypes + ?Sized> Eq for CampaignJobResult<G> {}

impl<G: CampaignTypes + ?Sized> Clone for CampaignJobResult<G> {
    fn clone(&self) -> Self {
        Self {
            actions: self.actions.clone(),
        }
    }
}

impl<G: CampaignTypes + ?Sized> Debug for CampaignJobResult<G> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CampaignJobResult")
            .field("actions", &self.actions)
            .finish()
    }
}

fn verify_selector_annotation(draw: &SelectorDraw) -> Result<(), Box<dyn Error>> {
    match (draw.path, draw.concentration) {
        (SelectorPath::HierarchyWalk, None) => {
            Err("cell draw is missing its concentration record".into())
        }
        (SelectorPath::Uniform | SelectorPath::Continuation, Some(_)) => {
            Err("non-cell draw carries a concentration record".into())
        }
        _ => Ok(()),
    }
}

pub fn derive_worker_seed(campaign_seed: u64, worker_index: u32) -> Result<u64, Box<dyn Error>> {
    let mut hasher = Sha256::new();
    hasher.update(campaign_seed.to_le_bytes());
    hasher.update(worker_index.to_le_bytes());
    let digest = hasher.finalize();
    let bytes: [u8; 8] = digest[..8]
        .try_into()
        .map_err(|_| "worker seed digest is too short")?;
    Ok(u64::from_le_bytes(bytes))
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
    max_actions: usize,
    pub(crate) mixture_energy: MixtureEnergy,
}

impl<G: Workload + ?Sized> CoordinatorCore<G> {
    pub(crate) fn new(
        workload: &G,
        run: &G::Run,
        max_actions: usize,
        archive_entry_limit: usize,
        memory_budget_mib: Option<usize>,
    ) -> Self {
        let mut archive = Archive::new(workload.action_cost_fn());
        archive.max_entries = archive_entry_limit;
        if let Some(memory_budget_mib) = memory_budget_mib {
            let total = memory_budget_mib.saturating_mul(1024 * 1024);
            let draw_reserve = workload.draw_state_memory_reserve_bytes(run, max_actions);
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
            max_actions,
            mixture_energy: MixtureEnergy::default(),
        }
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
        let checkpointed: BTreeMap<u64, &G::Snapshot> = checkpoint
            .map(|checkpoint| {
                checkpoint
                    .entries
                    .iter()
                    .map(|entry| (entry.id, &entry.snapshot))
                    .collect()
            })
            .unwrap_or_default();
        workload.merge_origin_evidence(&mut self.evidence, source);
        let preserve_inactive_snapshots = self.archive.preserves_inactive_snapshots();
        self.archive.preserve_inactive_snapshots(true)?;
        self.bootstrap(workload, run, target)?;
        let genesis_id = 0;
        let mut counts = TreeImportCounts::default();
        let mut index_of: BTreeMap<u64, usize> = BTreeMap::new();
        let source_entries = workload.source_entries(source);
        let mut imported: Vec<Option<usize>> = Vec::with_capacity(source_entries.len());
        for (index, entry) in source_entries.iter().enumerate() {
            index_of.insert(entry.id, index);
            if entry.input.actions.is_empty() {
                imported.push(Some(genesis_id));
                continue;
            }
            if entry.input.actions.len() > self.max_actions {
                counts.over_limit = counts.over_limit.saturating_add(1);
                imported.push(None);
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
            let snapshot = if let Some(snapshot) = checkpointed.get(&entry.id) {
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
                    Some((*snapshot).clone())
                }
            } else {
                workload.restore(
                    target,
                    parent_entry
                        .snapshot
                        .as_deref()
                        .ok_or("whole-tree import parent snapshot was released early")?,
                )?;
                let mut terminal = false;
                for action in &entry.input.actions[parent_input_len..] {
                    workload.apply_action(target, action, &mut milestones)?;
                    if workload
                        .rollout_outcome(run, target)?
                        .disposition
                        .is_terminal()
                    {
                        terminal = true;
                        break;
                    }
                }
                if terminal {
                    None
                } else {
                    Some(workload.snapshot(target)?)
                }
            };
            let Some(snapshot) = snapshot else {
                counts.terminal = counts.terminal.saturating_add(1);
                imported.push(None);
                continue;
            };
            workload.merge_import_evidence(&mut self.evidence, milestones, &prefix);
            let key = workload.current_key(target)?;
            let suffix = prefix
                .actions
                .get(parent_input_len..)
                .ok_or("source archive input is shorter than its imported parent")?
                .to_vec();
            let inserted_before = self.archive.entries.len();
            match self.archive.insert(
                Some(parent_id),
                0,
                ArchiveCandidate {
                    suffix,
                    key,
                    milestones,
                },
                snapshot,
            )? {
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
        Ok(counts)
    }

    pub(crate) fn admit_job(
        &mut self,
        workload: &G,
        parent_id: u64,
        result: CampaignJobResult<G>,
    ) -> Result<(u64, Vec<CampaignAdmissionDecision>), Box<dyn Error>> {
        self.sequence = self.sequence.saturating_add(1);
        let sequence = self.sequence;
        let parent_index = self
            .archive
            .index_of_id(parent_id)
            .ok_or("campaign job parent is no longer resident")?;
        let mut current_parent = parent_index;
        let mut pending_suffix = Vec::new();
        let mut previous_key = None;
        let mut decisions = Vec::new();
        for action in result.actions {
            pending_suffix.push(action.action);
            workload.merge_action_evidence(&mut self.evidence, &action, sequence, || {
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
            if let Some(candidate) = action.candidate {
                if !candidate.viable {
                    self.probe_refused = self.probe_refused.saturating_add(1);
                    decisions.push(CampaignAdmissionDecision::ProbeRefused);
                    continue;
                }
                let retained_before = self.archive.retained;
                let (admitted, key) = self.archive.insert_after(
                    Some(current_parent),
                    previous_key,
                    sequence,
                    ArchiveCandidate {
                        suffix: pending_suffix.clone(),
                        key: workload.complete_candidate_key(candidate.key, &candidate.snapshot)?,
                        milestones: action.milestones,
                    },
                    candidate.snapshot,
                )?;
                match admitted {
                    Some(id) if self.archive.retained > retained_before => {
                        decisions.push(CampaignAdmissionDecision::Retained {
                            id: self
                                .archive
                                .stable_id(id)
                                .ok_or("retained archive slot is missing")?,
                        });
                        current_parent = id;
                        pending_suffix.clear();
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
                        previous_key = None;
                    }
                    None => {
                        decisions.push(CampaignAdmissionDecision::Rejected);
                        previous_key = Some(key);
                    }
                }
            }
        }
        if sequence.is_multiple_of(self.curve_interval) {
            self.push_curve_point();
            self.compact_progress_curve_if_needed();
        }
        Ok((sequence, decisions))
    }

    fn push_curve_point(&mut self) {
        self.curve.push(ProgressPoint {
            executions: self.sequence,
            milestones: self.aggregate_milestones(),
            progress: self
                .record_progress
                .then(|| G::aggregate_progress(&self.evidence)),
            active_entries: self.archive.active_count(),
            occupied_cells: self.archive.slots.len(),
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
        let parent_actions = self.archive.entries[parent_index].input_len;
        let executable = suffix
            .len()
            .min(self.max_actions.saturating_sub(parent_actions));
        if executable == 0 {
            return false;
        }
        self.archive
            .all_extensions_retained(parent_index, &suffix[..executable])
    }

    pub(crate) fn into_archive_report_and_snapshots(
        mut self,
        workload: &G,
        campaign_seed: u64,
        materialize_final_artifacts: bool,
    ) -> (G::ArchiveReport, Vec<(u64, G::Snapshot)>) {
        let (entries, snapshots) = if materialize_final_artifacts {
            self.archive.take_entry_reports_and_snapshots()
        } else {
            (Vec::new(), Vec::new())
        };
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
                selector: self.archive.selector_report(),
            },
        );
        (report, snapshots)
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
    origin: &CampaignOrigin<G>,
) -> Result<CampaignOriginRecord, Box<dyn Error>> {
    let (kind, path, archive_sha256, checkpoint, resume_input) = match origin {
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
            file_sha256,
            report,
            checkpoint,
        } => (
            ORIGIN_ARCHIVE.to_owned(),
            Some(path.clone()),
            Some(file_sha256.clone()),
            checkpoint.as_ref(),
            workload.resume_input(report)?,
        ),
    };
    let resume_input_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&resume_input)?));
    Ok(CampaignOriginRecord {
        kind,
        path,
        archive_sha256,
        checkpoint_path: checkpoint.map(|checkpoint| checkpoint.path.clone()),
        checkpoint_sha256: checkpoint.map(|checkpoint| checkpoint.file_sha256.clone()),
        resume_input_sha256,
        resume_actions: resume_input.actions.len(),
    })
}

fn stream_header<G: Workload>(
    workload: &G,
    config: &CampaignConfig<G>,
    origin: &CampaignOriginRecord,
    draw_header: Option<G::DrawHeader>,
) -> CampaignStreamHeader<G::DrawHeader> {
    CampaignStreamHeader {
        schema_version: CAMPAIGN_SCHEMA_VERSION,
        format: workload.stream_format().to_owned(),
        campaign_seed: config.campaign_seed,
        workers: config.workers,
        schedule_policy: schedule_policy_identifier(config.reservations_per_worker),
        progress_policy: CAMPAIGN_PROGRESS_POLICY.to_owned(),
        host: config.host.clone(),
        origin_kind: origin.kind.clone(),
        origin_path: origin.path.clone(),
        origin_archive_sha256: origin.archive_sha256.clone(),
        origin_checkpoint_path: origin.checkpoint_path.clone(),
        origin_checkpoint_sha256: origin.checkpoint_sha256.clone(),
        resume_input_sha256: origin.resume_input_sha256.clone(),
        resume_actions: origin.resume_actions,
        execution_budget: config.execution_budget,
        stop_rollout_on_objective: config.stop_rollout_on_objective,
        stop_campaign_on_objective: config.stop_campaign_on_objective,
        work_budget: None,
        wall_budget_seconds: config.wall_budget.map(|budget| budget.as_secs()),
        action_limit: config.action_limit,
        archive_entry_limit: config.archive_entry_limit,
        memory_budget_mib: config.memory_budget_mib,
        resume_policy: if origin.kind == ORIGIN_SNAPSHOT_ROOT {
            SNAPSHOT_ROOT_RESUME_IDENTIFIER.to_owned()
        } else {
            RESUME_IDENTIFIER.to_owned()
        },
        suffix_policy: suffix_shape_identifier(config.suffix).to_owned(),
        mixture_policy: draw_mixture_identifier(config.mixture),
        workload_policies: workload.policies(&config.run),
        draw_header,
        retention_policy: retention_policy_identifier(config.retention).to_owned(),
        parent_scheduler: selector_policy_identifier(&config.selector),
        executor_mode: "snapshot_resume_archive".to_owned(),
        worker_seed_derivation: "sha256(campaign_seed_le || worker_index_le)[0..8] as u64 le"
            .to_owned(),
        workload_identity_sha256: workload.workload_identity_sha256(),
        action_cost_unit: workload.action_cost_unit().to_owned(),
        execution_work_unit: workload.execution_work_unit().to_owned(),
    }
}

struct StreamWriter<'a> {
    sink: &'a mut dyn Write,
    hasher: Sha256,
}

impl<'a> StreamWriter<'a> {
    fn new(sink: &'a mut dyn Write) -> Self {
        Self {
            sink,
            hasher: Sha256::new(),
        }
    }

    fn write_line<T: Serialize>(&mut self, value: &T) -> Result<(), Box<dyn Error>> {
        let mut line = serde_json::to_vec(value)?;
        line.push(b'\n');
        self.sink.write_all(&line)?;
        self.hasher.update(&line);
        Ok(())
    }

    fn finish(self) -> Result<String, Box<dyn Error>> {
        self.sink.flush()?;
        Ok(format!("{:x}", self.hasher.finalize()))
    }
}

struct CampaignCounters {
    bootstrap_execution_work: u64,
    tree_import: Option<TreeImportCounts>,
    job_execution_work: u64,
    work_to_first_objective: Option<u64>,
    executions_to_first_objective: Option<u64>,
    duplicates_skipped: u64,
    draw_state_memory_bytes: usize,
    jobs_per_worker: Vec<u64>,
    skips_per_worker: Vec<u64>,
}

fn execution_work_delta(before: u64, after: u64, scope: &str) -> Result<u64, Box<dyn Error>> {
    after
        .checked_sub(before)
        .ok_or_else(|| format!("{scope} execution-work counter rewound").into())
}

#[derive(Default, Serialize)]
struct LiveCoordinatorProfile {
    enabled: bool,
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
}

impl LiveCoordinatorProfile {
    fn note_dispatch<G: Workload + ?Sized>(
        &mut self,
        spec: &JobSpec<G>,
        cost: fn(&G::Action) -> u64,
    ) {
        if !self.enabled {
            return;
        }
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
    }
}

fn live_coordinator_profile(enabled: bool) -> LiveCoordinatorProfile {
    LiveCoordinatorProfile {
        enabled,
        ..LiveCoordinatorProfile::default()
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

#[allow(clippy::disallowed_methods)]
fn profile_now(enabled: bool) -> Option<Instant> {
    enabled.then(Instant::now)
}

fn profile_elapsed(started: Option<Instant>) -> u128 {
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

    fn new(workers: u32) -> Self {
        Self {
            bootstrap_execution_work: 0,
            tree_import: None,
            job_execution_work: 0,
            work_to_first_objective: None,
            executions_to_first_objective: None,
            duplicates_skipped: 0,
            draw_state_memory_bytes: 0,
            jobs_per_worker: vec![0; workers as usize],
            skips_per_worker: vec![0; workers as usize],
        }
    }
}

fn build_report<G: Workload>(
    workload: &G,
    header: &CampaignStreamHeader<G::DrawHeader>,
    origin: CampaignOriginRecord,
    core: CoordinatorCore<G>,
    counters: &CampaignCounters,
    stream_sha256: String,
    materialize_final_artifacts: bool,
) -> CampaignOutcome<G> {
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
    let barren_groups = core.archive.barren_group_count();
    let (archive, snapshots) = core.into_archive_report_and_snapshots(
        workload,
        header.campaign_seed,
        materialize_final_artifacts,
    );
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
        workers: header.workers,
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
        action_limit: header.action_limit,
        archive_entry_limit: header.archive_entry_limit,
        memory_budget_mib: header.memory_budget_mib,
        resume_policy: header.resume_policy.clone(),
        suffix_policy: header.suffix_policy.clone(),
        mixture_policy: header.mixture_policy.clone(),
        workload_policies: header.workload_policies.clone(),
        retention_policy: header.retention_policy.clone(),
        parent_scheduler: header.parent_scheduler.clone(),
        executor_mode: header.executor_mode.clone(),
        worker_seed_derivation: header.worker_seed_derivation.clone(),
        workload_identity_sha256: header.workload_identity_sha256.clone(),
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
        barren_groups,
        jobs_per_worker: counters.jobs_per_worker.clone(),
        skips_per_worker: counters.skips_per_worker.clone(),
        stream_sha256,
        archive,
    };
    (report, checkpoint)
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

pub fn postcard_value_sha256<T: Serialize + ?Sized>(value: &T) -> Result<String, Box<dyn Error>> {
    let digest = postcard::serialize_with_flavor::<_, PostcardSha256, sha2::digest::Output<Sha256>>(
        value,
        PostcardSha256(Sha256::new()),
    )?;
    Ok(format!("{digest:x}"))
}

struct JobSpec<G: Workload + ?Sized> {
    reservation: usize,
    snapshot: Arc<G::Snapshot>,
    replay: Vec<G::Action>,
    parent_actions: usize,
    parent_milestones: G::Milestones,
    suffix: Vec<G::Action>,
}

type SelectedJob<G> = (JobSpec<G>, PendingJob<G>);

struct PendingJob<G: Workload + ?Sized> {
    snapshot_id: u64,
    worker: u32,
    parent_id: u64,
    mutation_seed: u64,
    mixture_weight: u8,
    splice_weight: u8,
    splice: Option<CampaignSpliceRecord>,
    selector: SelectorDraw,
    draw_checkpoint_before: Option<G::DrawCheckpoint>,
}

struct CompletedJob<G: Workload + ?Sized> {
    physical_worker: u32,
    pending: PendingJob<G>,
    result: CampaignJobResult<G>,
    execution_work: u64,
    result_sha256: String,
}

fn completed_results_within_bound(completed: usize, workers: usize, per_worker: usize) -> bool {
    completed <= workers.saturating_mul(per_worker)
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
            if tail.is_empty() || tail.len() > SPLICE_ACTION_CAP {
                return Err("recorded splice tail length is outside the bound".into());
            }
            Ok(Some(tail))
        }
        None => Err("splice draw lacks resolution evidence".into()),
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "K: Serialize + DeserializeOwned")]
pub struct CampaignProgressRecord<K> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_diagnostics: Option<serde_json::Value>,
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
    pub novelty_memory_bytes: usize,
    #[serde(default)]
    pub barren_memory_bytes: usize,
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
    pub barren_groups: usize,
}

fn write_live_progress<G: Workload>(
    core: &CoordinatorCore<G>,
    counters: &CampaignCounters,
    coordinator_profile: &LiveCoordinatorProfile,
    draw_state_memory_bytes: usize,
    telemetry_started: Instant,
    sink: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let sequence = core.sequence;
    #[allow(clippy::disallowed_methods)]
    let unix_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let (deepest_key, cheapest, retained) = core
        .archive
        .live_progress()
        .map(|(key, cheapest, retained)| (Some(key), cheapest, retained))
        .unwrap_or((None, 0, 0));
    let line = serde_json::to_string(&CampaignProgressRecord {
        workload_diagnostics: G::diagnostics(&core.evidence),
        coordinator: coordinator_profile
            .enabled
            .then(|| serde_json::to_value(coordinator_profile))
            .transpose()?,
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
        novelty_memory_bytes: core.archive.novelty_memory_bytes(),
        barren_memory_bytes: core.archive.barren_memory_bytes(),
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
        barren_groups: core.archive.barren_group_count(),
    })?;
    sink.write_all(line.as_bytes())?;
    sink.write_all(b"\n")?;
    sink.flush()?;
    Ok(())
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
            report, checkpoint, ..
        } => Ok(Some(core.import_tree(
            workload,
            run,
            target,
            report,
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
    }
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

#[allow(clippy::too_many_lines)]
pub fn run_campaign_checkpointed_with_options<G: Workload>(
    workload: &G,
    config: &CampaignConfig<G>,
    origin: &CampaignOrigin<G>,
    stream: &mut dyn Write,
    mut progress: Option<&mut dyn Write>,
    options: CampaignExecutionOptions,
) -> Result<CampaignOutcome<G>, Box<dyn Error>>
where
    G::ArchiveReport: Serialize,
{
    let work_budget = options.work_budget;
    let result_limit = options.result_buffering.capacity();
    if work_budget == Some(0) {
        return Err("work budget must be nonzero".into());
    }
    if config.workers == 0 {
        return Err("campaign mode requires at least one worker".into());
    }
    if config.action_limit == 0 || config.action_limit > workload.max_action_limit() {
        return Err("campaign action limit is outside its bounded range".into());
    }
    if !archive_entry_limit_is_valid(config.archive_entry_limit) {
        return Err("campaign archive entry limit is outside its bounded range".into());
    }
    if config.reservations_per_worker == 0 {
        return Err("campaign reservations per worker must be at least one".into());
    }
    if config.memory_budget_mib == Some(0) {
        return Err("campaign memory budget must be nonzero".into());
    }
    if let Some(memory_budget_mib) = config.memory_budget_mib {
        let budget = memory_budget_mib.saturating_mul(1024 * 1024);
        if budget <= workload.draw_state_memory_reserve_bytes(&config.run, config.action_limit) {
            return Err("campaign memory budget is too small for the bounded draw state".into());
        }
    }
    let origin_record = resolve_origin(workload, origin)?;
    let draw_origin = match origin {
        CampaignOrigin::Genesis => None,
        CampaignOrigin::SnapshotRoot { .. } => None,
        CampaignOrigin::Archive {
            file_sha256,
            report,
            ..
        } => Some((file_sha256.as_str(), report.as_ref())),
    };
    let (mut draw_state, draw_header) = workload.initial_draw_state(&config.run, draw_origin)?;
    if !draw_state_memory_is_within_reserve(
        workload.draw_state_memory_bytes(&draw_state),
        workload.draw_state_memory_reserve_bytes(&config.run, config.action_limit),
    ) {
        return Err("initial draw state exceeds its deterministic memory reserve".into());
    }
    let mut header = stream_header(workload, config, &origin_record, draw_header);
    header.work_budget = work_budget;
    let mut writer = StreamWriter::new(stream);
    writer.write_line(&header)?;

    let mut core = CoordinatorCore::new(
        workload,
        &config.run,
        config.action_limit,
        config.archive_entry_limit,
        config.memory_budget_mib,
    );
    core.archive.selector_policy = config.selector.clone();
    core.archive
        .enable_continuations(config.mixture.uses_continuations());
    let mut counters = CampaignCounters::new(config.workers);
    let mut bootstrap_target = workload.new_target().map_err(|error| -> Box<dyn Error> {
        format!("failed to build the bootstrap target: {error}").into()
    })?;
    let work_before = workload.execution_work(&bootstrap_target);
    counters.tree_import = bootstrap_core(
        workload,
        &config.run,
        &mut core,
        &mut bootstrap_target,
        origin,
    )?;
    counters.bootstrap_execution_work = execution_work_delta(
        work_before,
        workload.execution_work(&bootstrap_target),
        "bootstrap",
    )?;
    if core.objectives_reached > 0 {
        counters.note_first_objective(0);
    }
    if let (Some(path), Some(input)) = (&config.objective_witness_path, &core.objective_witness) {
        std::fs::write(path, serde_json::to_vec_pretty(input)?)?;
    }
    drop(bootstrap_target);
    let bootstrap_memory_bytes = core
        .archive
        .resident_memory_bytes()
        .saturating_add(workload.draw_state_memory_bytes(&draw_state));
    if !resident_memory_is_within_budget(bootstrap_memory_bytes, config.memory_budget_mib) {
        return Err("campaign bootstrap state exceeds its deterministic memory budget".into());
    }

    let workers = config.workers as usize;
    let mut rands = Vec::with_capacity(workers);
    for index in 0..config.workers {
        rands.push(RomuDuoJrRand::with_seed(derive_worker_seed(
            config.campaign_seed,
            index,
        )?));
    }

    #[allow(clippy::disallowed_methods)]
    let telemetry_started = std::time::Instant::now();
    let started = config.wall_budget.map(|_| telemetry_started);

    let mut reserved = 0_u64;
    let mut coordinator_profile =
        live_coordinator_profile(std::env::var_os("HARMONY_COORDINATOR_PROFILE").is_some());
    let action_cost = workload.action_cost_fn();
    let max_action_cost = workload.max_action_cost();

    let max_actions = config.action_limit;
    let retention = config.retention;
    with_worker_pool(
        config.workers,
        |_| workload.new_target(),
        |target, spec: JobSpec<G>| {
            let reservation = spec.reservation;
            let work_before = workload.execution_work(target);
            workload
                .execute_job(
                    &config.run,
                    target,
                    &spec.snapshot,
                    &spec.replay,
                    spec.parent_actions,
                    spec.parent_milestones,
                    &spec.suffix,
                    max_actions,
                    retention,
                    config.stop_rollout_on_objective,
                )
                .and_then(|result| {
                    let result_sha256 = workload
                        .result_sha256(&result)
                        .map_err(|error| error.to_string())?;
                    let execution_work = execution_work_delta(
                        work_before,
                        workload.execution_work(target),
                        "worker",
                    )?;
                    Ok((reservation, result, execution_work, result_sha256))
                })
                .map_err(|error| error.to_string())
        },
        |pool| -> Result<(), Box<dyn Error>> {
            let select = |core: &mut CoordinatorCore<G>,
                          rands: &mut [RomuDuoJrRand],
                          draw_state: &mut G::DrawState,
                          writer: &mut StreamWriter<'_>,
                          counters: &mut CampaignCounters,
                          reserved: &mut u64,
                          worker: u32|
             -> Result<Option<SelectedJob<G>>, Box<dyn Error>> {
                if *reserved >= config.execution_budget
                    || work_budget.is_some_and(|budget| {
                        counters
                            .bootstrap_execution_work
                            .saturating_add(counters.job_execution_work)
                            >= budget
                    })
                    || stop_reservations_after_objective(
                        config.stop_campaign_on_objective,
                        core.objective_witness.is_some(),
                    )
                {
                    return Ok(None);
                }
                if let (Some(started), Some(wall_budget)) = (started, config.wall_budget)
                    && started.elapsed() >= wall_budget
                {
                    return Ok(None);
                }
                let rand = &mut rands[worker as usize];
                let max_actions = core.max_actions;
                core.archive.establish_liveness_anchor(max_actions);
                let mut consecutive_skips = 0_u64;
                if reserved.wrapping_add(1).is_multiple_of(4) {
                    while let Some(continuation) = core.archive.pop_continuation() {
                        let Some(parent_index) = core.archive.index_of_id(continuation.parent)
                        else {
                            continue;
                        };
                        if !core.archive.active[parent_index]
                            || core.archive.entries[parent_index].input_len >= max_actions
                        {
                            continue;
                        }
                        let mut suffix = continuation.actions;
                        config
                            .suffix
                            .bound_cost(&mut suffix, action_cost, max_action_cost);
                        if core.all_prefixes_archived(parent_index, &suffix) {
                            continue;
                        }
                        let (mixture_weight, splice_weight) = (0, u8::MAX);
                        let mut mutation_seed = rand.next_u64();
                        while energy_strategy(mutation_seed, mixture_weight, splice_weight)?
                            != EnergyStrategy::Splice
                        {
                            mutation_seed = rand.next_u64();
                        }
                        let parent_id = continuation.parent;
                        let selector = SelectorDraw {
                            path: SelectorPath::Continuation,
                            classes_skipped: 0,
                            counter_reset: false,
                            concentration: None,
                        };
                        let draw_checkpoint_before = workload.draw_checkpoint(draw_state)?;
                        let splice = Some(CampaignSpliceRecord::Tail {
                            donor_id: continuation.donor,
                            leaf_id: continuation.leaf,
                            tail_postcard: postcard::to_allocvec(&suffix)?,
                        });
                        *reserved = reserved.saturating_add(1);
                        core.archive.pin_metadata(parent_id)?;
                        let (snapshot, replay, snapshot_id) =
                            core.archive.pin_job_origin(parent_index)?;
                        let entry = &core.archive.entries[parent_index];
                        return Ok(Some((
                            JobSpec {
                                reservation: 0,
                                snapshot,
                                replay,
                                parent_actions: entry.input_len,
                                parent_milestones: entry.milestones,
                                suffix,
                            },
                            PendingJob {
                                snapshot_id,
                                worker,
                                parent_id,
                                mutation_seed,
                                mixture_weight,
                                splice_weight,
                                splice,
                                selector,
                                draw_checkpoint_before,
                            },
                        )));
                    }
                }
                loop {
                    let (parent_index, selector) = core.archive.select_parent(rand, max_actions)?;
                    let parent_id = core
                        .archive
                        .stable_id(parent_index)
                        .ok_or("selected archive slot is missing")?;
                    let mutation_seed = rand.next_u64();
                    let (mixture_weight, splice_weight) = match config.mixture {
                        DrawMixture::Energy { scale } => {
                            (core.mixture_energy.biased_weight(scale), 0)
                        }
                        DrawMixture::EnergySplice { scale }
                        | DrawMixture::EnergySpliceContinuation { scale }
                        | DrawMixture::EnergySpliceContinuationIsolated { scale } => {
                            core.mixture_energy.splice_weights(scale)
                        }
                        _ => (DEFAULT_MIXTURE_WEIGHT, 0),
                    };
                    let draw_checkpoint_before = workload.draw_checkpoint(draw_state)?;
                    let (spliced, splice) =
                        if energy_strategy(mutation_seed, mixture_weight, splice_weight)?
                            == EnergyStrategy::Splice
                        {
                            match core.archive.splice_tail_for_campaign(
                                parent_index,
                                max_actions,
                                SPLICE_ACTION_CAP,
                            ) {
                                Some(CampaignSpliceTail {
                                    donor_id,
                                    leaf_id,
                                    actions,
                                }) => {
                                    let tail_postcard = postcard::to_allocvec(&actions)?;
                                    (
                                        Some(actions),
                                        Some(CampaignSpliceRecord::Tail {
                                            donor_id: core
                                                .archive
                                                .stable_id(donor_id)
                                                .ok_or("splice donor slot is missing")?,
                                            leaf_id: core
                                                .archive
                                                .stable_id(leaf_id)
                                                .ok_or("splice leaf slot is missing")?,
                                            tail_postcard,
                                        }),
                                    )
                                }
                                None => (None, Some(CampaignSpliceRecord::Unavailable)),
                            }
                        } else {
                            (None, None)
                        };
                    let mut suffix = match spliced {
                        Some(tail) => tail,
                        None => workload.expand_suffix(
                            &config.run,
                            draw_state,
                            config.suffix,
                            MixtureDraw {
                                mixture: config.mixture,
                                weight: mixture_weight,
                                splice_weight,
                            },
                            mutation_seed,
                        )?,
                    };
                    config
                        .suffix
                        .bound_cost(&mut suffix, action_cost, max_action_cost);
                    let all_prefixes_archived = consecutive_skips < CONSECUTIVE_SKIP_LIMIT
                        && core.all_prefixes_archived(parent_index, &suffix);
                    if all_prefixes_archived {
                        let draw_checkpoint_after =
                            workload.finish_stream_record(&config.run, draw_state, &[])?;
                        writer.write_line(&CampaignStreamRecord::Skip(CampaignSkipRecord {
                            worker,
                            parent_id,
                            mutation_seed,
                            mixture_weight,
                            splice_weight,
                            splice,
                            selector,
                            draw_checkpoint_before,
                            draw_checkpoint_after,
                        }))?;
                        core.archive.record_selection(parent_index, &selector);
                        core.archive.maintain_memory_budget()?;
                        counters.duplicates_skipped = counters.duplicates_skipped.saturating_add(1);
                        counters.skips_per_worker[worker as usize] =
                            counters.skips_per_worker[worker as usize].saturating_add(1);
                        consecutive_skips = consecutive_skips.saturating_add(1);
                        continue;
                    }
                    *reserved = reserved.saturating_add(1);
                    core.archive.pin_metadata(parent_id)?;
                    let (snapshot, replay, snapshot_id) =
                        core.archive.pin_job_origin(parent_index)?;
                    let entry = &core.archive.entries[parent_index];
                    return Ok(Some((
                        JobSpec {
                            reservation: 0,
                            snapshot,
                            replay,
                            parent_actions: entry.input_len,
                            parent_milestones: entry.milestones,
                            suffix,
                        },
                        PendingJob {
                            snapshot_id,
                            worker,
                            parent_id,
                            mutation_seed,
                            mixture_weight,
                            splice_weight,
                            splice,
                            selector,
                            draw_checkpoint_before,
                        },
                    )));
                }
            };

            let pipeline_depth = admission_window_depth(workers, config.reservations_per_worker);
            let mut pending = BTreeMap::<usize, PendingJob<G>>::new();
            let mut completed = BTreeMap::<usize, CompletedJob<G>>::new();
            let mut queued_specs = VecDeque::with_capacity(
                pipeline_depth.min(usize::try_from(config.execution_budget).unwrap_or(usize::MAX)),
            );
            for _ in 0..pipeline_depth {
                let worker_index = usize::try_from(reserved % u64::from(config.workers))?;
                let worker = u32::try_from(worker_index)?;
                let selection_started = profile_now(coordinator_profile.enabled);
                let selected = select(
                    &mut core,
                    &mut rands,
                    &mut draw_state,
                    &mut writer,
                    &mut counters,
                    &mut reserved,
                    worker,
                )?;
                coordinator_profile.selection_ns = coordinator_profile
                    .selection_ns
                    .saturating_add(profile_elapsed(selection_started));
                coordinator_profile.selections = coordinator_profile.selections.saturating_add(1);
                let Some((mut spec, pending_job)) = selected else {
                    break;
                };
                coordinator_profile.note_dispatch(&spec, action_cost);
                let reservation = usize::try_from(reserved.saturating_sub(1))?;
                spec.reservation = reservation;
                if pending.insert(reservation, pending_job).is_some() {
                    return Err("campaign reserved one job twice".into());
                }
                queued_specs.push_back(spec);
            }
            let mut physical_queued = vec![0_usize; workers];
            let mut result_slots = ResultSlots::new(config.workers, result_limit);
            while !queued_specs.is_empty() {
                let Some(worker) = result_slots.reserve() else {
                    break;
                };
                let spec = queued_specs
                    .pop_front()
                    .ok_or("campaign prefill lost queued job")?;
                pool.send(worker, spec)?;
                physical_queued[usize::try_from(worker)?] += 1;
            }

            let mut next_admission = 0_usize;
            while !pending.is_empty() || !completed.is_empty() {
                let mut ready_reply = if completed.contains_key(&next_admission) {
                    pool.try_receive()?
                } else if pending.is_empty() {
                    return Err("campaign reorder window ended with an admission gap".into());
                } else {
                    let receive_started = profile_now(coordinator_profile.enabled);
                    let reply = pool.receive()?;
                    coordinator_profile.receive_wait_ns = coordinator_profile
                        .receive_wait_ns
                        .saturating_add(profile_elapsed(receive_started));
                    Some(reply)
                };
                while let Some(reply) = ready_reply {
                    coordinator_profile.receives = coordinator_profile.receives.saturating_add(1);
                    let physical_worker = reply.worker;
                    let outcome = reply.outcome.map_err(|error| -> Box<dyn Error> {
                        format!("campaign worker {physical_worker} failed: {error}").into()
                    })?;
                    let physical_index = usize::try_from(physical_worker)?;
                    let queued = physical_queued
                        .get_mut(physical_index)
                        .ok_or("campaign worker replied with an unknown physical identifier")?;
                    *queued = queued
                        .checked_sub(1)
                        .ok_or("campaign worker replied without queued work")?;
                    let (reservation, result, execution_work, result_sha256) = outcome;
                    let pending_job = pending
                        .remove(&reservation)
                        .ok_or("campaign worker replied for an unknown reservation")?;
                    if completed
                        .insert(
                            reservation,
                            CompletedJob {
                                physical_worker,
                                pending: pending_job,
                                result,
                                execution_work,
                                result_sha256,
                            },
                        )
                        .is_some()
                    {
                        return Err("campaign worker completed one reservation twice".into());
                    }
                    if !completed_results_within_bound(completed.len(), workers, result_limit) {
                        return Err("campaign exceeded its bounded result-bearing jobs".into());
                    }
                    ready_reply = pool.try_receive()?;
                }

                while let Some(completed_job) = completed.remove(&next_admission) {
                    let pending_job = completed_job.pending;
                    let worker_index = usize::try_from(pending_job.worker)?;
                    let result = completed_job.result;
                    let execution_work = completed_job.execution_work;
                    let result_sha256 = completed_job.result_sha256;
                    let physical_worker = completed_job.physical_worker;
                    let objectives_before = core.objectives_reached;
                    let admission_started = profile_now(coordinator_profile.enabled);
                    let (sequence, decisions) =
                        core.admit_job(workload, pending_job.parent_id, result)?;
                    coordinator_profile.admission_ns = coordinator_profile
                        .admission_ns
                        .saturating_add(profile_elapsed(admission_started));
                    coordinator_profile.admissions =
                        coordinator_profile.admissions.saturating_add(1);
                    let bookkeeping_started = profile_now(coordinator_profile.enabled);
                    let parent_index = core
                        .archive
                        .index_of_id(pending_job.parent_id)
                        .ok_or("completed job parent is no longer resident")?;
                    let isolated_continuation = config.mixture.isolates_continuations()
                        && pending_job.selector.path == SelectorPath::Continuation;
                    if isolated_continuation {
                        core.archive.record_isolated_continuation(parent_index);
                    } else {
                        core.archive
                            .record_selection(parent_index, &pending_job.selector);
                    }
                    let retained_ids = retained_archive_indexes(&core, &decisions);
                    let new_slot_descendant = retained_ids
                        .iter()
                        .any(|id| core.archive.opened_new_slot(*id));
                    let new_cell_descendant = retained_ids
                        .iter()
                        .any(|id| core.archive.opened_new_cell(*id));
                    if !isolated_continuation {
                        core.archive.record_selection_outcome(
                            parent_index,
                            !retained_ids.is_empty(),
                            new_slot_descendant,
                            new_cell_descendant,
                        );
                    }
                    record_mixture_outcome(
                        &mut core.mixture_energy,
                        config.mixture,
                        pending_job.selector.path,
                        pending_job.mutation_seed,
                        pending_job.mixture_weight,
                        pending_job.splice_weight,
                        new_slot_descendant,
                    )?;
                    if objectives_before == 0
                        && let (Some(path), Some(input)) =
                            (&config.objective_witness_path, &core.objective_witness)
                    {
                        std::fs::write(path, serde_json::to_vec_pretty(input)?)?;
                    }
                    let draw_checkpoint_after =
                        finish_record(workload, &config.run, &mut draw_state, &core, &decisions)?;
                    let draw_state_memory_bytes = workload.draw_state_memory_bytes(&draw_state);
                    if !draw_state_memory_is_within_reserve(
                        draw_state_memory_bytes,
                        workload.draw_state_memory_reserve_bytes(&config.run, config.action_limit),
                    ) {
                        return Err(
                            "live draw state exceeds its deterministic memory reserve".into()
                        );
                    }
                    core.archive.unpin_job_origin(pending_job.snapshot_id);
                    core.archive.unpin_metadata(pending_job.parent_id);
                    let compaction_started = profile_now(coordinator_profile.enabled);
                    let compactions_before = core.archive.history_compactions();
                    core.archive.maintain_memory_budget()?;
                    record_compaction_elapsed(
                        &mut coordinator_profile,
                        compactions_before,
                        core.archive.history_compactions(),
                        profile_elapsed(compaction_started),
                    );
                    coordinator_profile.bookkeeping_ns = coordinator_profile
                        .bookkeeping_ns
                        .saturating_add(profile_elapsed(bookkeeping_started));
                    let stream_started = profile_now(coordinator_profile.enabled);
                    writer.write_line(&CampaignStreamRecord::Job(CampaignJobRecord {
                        sequence,
                        worker: pending_job.worker,
                        parent_id: pending_job.parent_id,
                        mutation_seed: pending_job.mutation_seed,
                        execution_work,
                        result_sha256,
                        decisions,
                        mixture_weight: pending_job.mixture_weight,
                        splice_weight: pending_job.splice_weight,
                        splice: pending_job.splice,
                        selector: pending_job.selector,
                        draw_checkpoint_before: pending_job.draw_checkpoint_before,
                        draw_checkpoint_after,
                    }))?;
                    coordinator_profile.stream_write_ns = coordinator_profile
                        .stream_write_ns
                        .saturating_add(profile_elapsed(stream_started));
                    counters.jobs_per_worker[worker_index] =
                        counters.jobs_per_worker[worker_index].saturating_add(1);
                    counters.job_execution_work =
                        counters.job_execution_work.saturating_add(execution_work);
                    if objectives_before == 0 && core.objectives_reached > 0 {
                        counters.note_first_objective(sequence);
                    }
                    result_slots.admit(physical_worker)?;
                    if let Some(sink) = progress.as_deref_mut()
                        && progress_checkpoint_due(sequence)
                    {
                        write_live_progress(
                            &core,
                            &counters,
                            &coordinator_profile,
                            draw_state_memory_bytes,
                            telemetry_started,
                            sink,
                        )?;
                        if coordinator_profile.enabled {
                            eprintln!(
                                "coordinator-profile executions={sequence} receive_wait_ns={} admission_ns={} bookkeeping_ns={} history_compaction_ns={} stream_write_ns={} selection_ns={} receives={} admissions={} selections={} entries={} active_entries={} historical_input_actions={} stored_input_actions={} input_index_nodes={} resident_snapshots={} resident_snapshot_bytes={} entry_metadata_memory_bytes={} input_index_memory_bytes={} novelty_memory_bytes={} barren_memory_bytes={} history_memory_bytes={} draw_state_memory_bytes={} resident_memory_bytes={} snapshot_evictions={} history_compactions={} historical_entries_dropped={} input_reconstructions={} available_result_slots={} queued_specs={} completed_buffered={} job_execution_work={} replay_jobs={} replay_actions={} replay_cost={} suffix_actions={} suffix_cost={}",
                                coordinator_profile.receive_wait_ns,
                                coordinator_profile.admission_ns,
                                coordinator_profile.bookkeeping_ns,
                                coordinator_profile.history_compaction_ns,
                                coordinator_profile.stream_write_ns,
                                coordinator_profile.selection_ns,
                                coordinator_profile.receives,
                                coordinator_profile.admissions,
                                coordinator_profile.selections,
                                core.archive.entries.len(),
                                core.archive.active_count(),
                                core.archive.historical_input_actions(),
                                core.archive.stored_input_actions(),
                                core.archive.input_index_nodes(),
                                core.archive.resident_snapshot_count(),
                                core.archive.resident_snapshot_bytes(),
                                core.archive.entry_metadata_memory_bytes(),
                                core.archive.input_index_memory_bytes(),
                                core.archive.novelty_memory_bytes(),
                                core.archive.barren_memory_bytes(),
                                core.archive.history_memory_bytes(),
                                draw_state_memory_bytes,
                                core.archive
                                    .resident_memory_bytes()
                                    .saturating_add(draw_state_memory_bytes),
                                core.archive.snapshot_evictions(),
                                core.archive.history_compactions(),
                                core.archive.historical_entries_dropped(),
                                core.archive.input_reconstructions(),
                                result_slots.available(),
                                queued_specs.len(),
                                completed.len(),
                                counters.job_execution_work,
                                coordinator_profile.replay_jobs,
                                coordinator_profile.replay_actions,
                                coordinator_profile.replay_cost,
                                coordinator_profile.suffix_actions,
                                coordinator_profile.suffix_cost,
                            );
                        }
                    }
                    next_admission = next_admission.saturating_add(1);
                    let worker_index = usize::try_from(reserved % u64::from(config.workers))?;
                    let worker = u32::try_from(worker_index)?;
                    let selection_started = profile_now(coordinator_profile.enabled);
                    let selected = select(
                        &mut core,
                        &mut rands,
                        &mut draw_state,
                        &mut writer,
                        &mut counters,
                        &mut reserved,
                        worker,
                    )?;
                    coordinator_profile.selection_ns = coordinator_profile
                        .selection_ns
                        .saturating_add(profile_elapsed(selection_started));
                    coordinator_profile.selections =
                        coordinator_profile.selections.saturating_add(1);
                    if let Some((mut spec, pending_job)) = selected {
                        coordinator_profile.note_dispatch(&spec, action_cost);
                        let reservation = usize::try_from(reserved.saturating_sub(1))?;
                        spec.reservation = reservation;
                        if pending.insert(reservation, pending_job).is_some() {
                            return Err("campaign reserved one job twice".into());
                        }
                        queued_specs.push_back(spec);
                    }

                    while !queued_specs.is_empty() {
                        let Some(worker) = result_slots.reserve() else {
                            break;
                        };
                        let spec = queued_specs
                            .pop_front()
                            .ok_or("campaign queued-job count changed while dispatching")?;
                        pool.send(worker, spec)?;
                        let physical_index = usize::try_from(worker)?;
                        physical_queued[physical_index] =
                            physical_queued[physical_index].saturating_add(1);
                    }
                }
                while !queued_specs.is_empty() {
                    let Some(worker) = result_slots.reserve() else {
                        break;
                    };
                    let spec = queued_specs
                        .pop_front()
                        .ok_or("campaign queued-job count changed while dispatching")?;
                    pool.send(worker, spec)?;
                    let physical_index = usize::try_from(worker)?;
                    physical_queued[physical_index] =
                        physical_queued[physical_index].saturating_add(1);
                }
            }
            if !completed.is_empty() {
                return Err("campaign reorder window ended with an admission gap".into());
            }
            if !queued_specs.is_empty() {
                return Err("campaign reorder window ended with queued jobs".into());
            }
            for worker in 0..config.workers {
                pool.close(worker)?;
            }
            Ok(())
        },
    )?;

    core.archive.preserve_inactive_snapshots(false)?;
    core.archive.compact_history_for_final_report()?;
    core.finish_curve();
    if let Some(sink) = progress {
        write_live_progress(
            &core,
            &counters,
            &coordinator_profile,
            workload.draw_state_memory_bytes(&draw_state),
            telemetry_started,
            sink,
        )?;
    }
    let stream_sha256 = writer.finish()?;
    counters.draw_state_memory_bytes = workload.draw_state_memory_bytes(&draw_state);
    Ok(build_report(
        workload,
        &header,
        origin_record,
        core,
        &counters,
        stream_sha256,
        config.materialize_final_artifacts,
    ))
}

fn finish_record<G: Workload>(
    workload: &G,
    run: &G::Run,
    draw_state: &mut G::DrawState,
    core: &CoordinatorCore<G>,
    decisions: &[CampaignAdmissionDecision],
) -> Result<Option<G::DrawCheckpoint>, Box<dyn Error>> {
    let needs_full = workload.retained_inputs_need_full(run);
    let mut retained_inputs = Vec::new();
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
        let input = if needs_full {
            core.archive
                .materialize_input(index)
                .map_err(|error| -> Box<dyn Error> { error.into() })?
        } else {
            Input {
                actions: entry.input_suffix.clone(),
            }
        };
        let parent_actions = if needs_full {
            entry
                .parent_id
                .and_then(|parent| core.archive.index_of_id(parent))
                .and_then(|parent| core.archive.entries.get(parent))
                .map_or(0, |parent| parent.input_len)
        } else {
            0
        };
        retained_inputs.push((parent_actions, input));
    }
    let retained = retained_inputs
        .iter()
        .map(|(parent_actions, input)| (*parent_actions, input.actions.as_slice()))
        .collect::<Vec<_>>();
    workload.finish_stream_record(run, draw_state, &retained)
}

#[allow(clippy::too_many_lines)]
pub fn replay_campaign_checkpointed<G: Workload>(
    workload: &G,
    stream_bytes: &[u8],
    origin_report: Option<&G::ArchiveReport>,
    origin_checkpoint: Option<&CampaignCheckpoint<G::Snapshot>>,
) -> Result<CampaignOutcome<G>, Box<dyn Error>>
where
    G::ArchiveReport: Serialize,
{
    let stream_sha256 = format!("{:x}", Sha256::digest(stream_bytes));
    let text = std::str::from_utf8(stream_bytes)?;
    let mut lines = text.lines();
    let header: CampaignStreamHeader<G::DrawHeader> =
        serde_json::from_str(lines.next().ok_or("campaign stream is empty")?)?;
    if header.schema_version != CAMPAIGN_SCHEMA_VERSION {
        return Err("campaign stream schema version is not recognized".into());
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
    if !schedule_policy_is_supported(&header.schedule_policy) {
        return Err("campaign stream schedule policy is not recognized".into());
    }
    if !progress_policy_is_supported(&header.progress_policy) {
        return Err("campaign stream progress policy is not recognized".into());
    }
    if header.workload_identity_sha256 != workload.workload_identity_sha256() {
        return Err("campaign replay workload identity does not match the recorded stream".into());
    }
    if header.action_cost_unit != workload.action_cost_unit() {
        return Err("campaign replay action-cost unit does not match the recorded stream".into());
    }
    if header.execution_work_unit != workload.execution_work_unit() {
        return Err(
            "campaign replay execution-work unit does not match the recorded stream".into(),
        );
    }
    let record_lines = lines.collect::<Vec<_>>();
    let mut required_draw_versions = BTreeSet::new();
    let mut replay_job_parents = Vec::<u64>::new();
    let mut replay_job_metadata = Vec::<Vec<u64>>::new();
    for line in &record_lines {
        let record: CampaignStreamRecord<G::DrawCheckpoint> = serde_json::from_str(line)?;
        let before = match record {
            CampaignStreamRecord::Job(job) => {
                replay_job_parents.push(job.parent_id);
                replay_job_metadata.push(vec![job.parent_id]);
                job.draw_checkpoint_before
            }
            CampaignStreamRecord::Skip(skip) => skip.draw_checkpoint_before,
        };
        if let Some(before) = before {
            required_draw_versions.insert(workload.draw_checkpoint_version(&before));
        }
    }
    let resume_input = match header.origin_kind.as_str() {
        ORIGIN_GENESIS => {
            if origin_report.is_some() {
                return Err("genesis campaign replay does not take a source archive".into());
            }
            if origin_checkpoint.is_some() {
                return Err("genesis campaign replay does not take a snapshot checkpoint".into());
            }
            Input::default()
        }
        ORIGIN_SNAPSHOT_ROOT => {
            if origin_report.is_some() {
                return Err("snapshot-root replay does not take a source archive".into());
            }
            if origin_checkpoint.is_none() {
                return Err("snapshot-root replay requires its snapshot checkpoint".into());
            }
            Input::default()
        }
        ORIGIN_ARCHIVE => {
            let source =
                origin_report.ok_or("archive campaign replay requires the source archive")?;
            workload.resume_input(source)?
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
    let replay_selector = crate::search::archive::selector_policy_from_identifier(
        &header.parent_scheduler,
        G::Key::groups().saturating_sub(2),
    )?;
    let expected_resume = if header.origin_kind == ORIGIN_SNAPSHOT_ROOT {
        SNAPSHOT_ROOT_RESUME_IDENTIFIER
    } else {
        RESUME_IDENTIFIER
    };
    if header.resume_policy != expected_resume {
        return Err("campaign stream resume policy is not recognized".into());
    }
    let replay_suffix = suffix_shape_from_identifier(&header.suffix_policy)?;
    let replay_mixture = draw_mixture_from_identifier(&header.mixture_policy)?;
    let replay_run = workload.resolve_recorded(&header.workload_policies)?;
    let action_cost = workload.action_cost_fn();
    let max_action_cost = workload.max_action_cost();
    if let Some(checkpoint) = origin_checkpoint {
        let sha256_matches =
            header.origin_checkpoint_sha256.as_deref() == Some(checkpoint.file_sha256.as_str());
        let logical_path_matches = header.origin_kind != ORIGIN_SNAPSHOT_ROOT
            || header.origin_checkpoint_path.as_deref() == Some(checkpoint.path.as_str());
        if !sha256_matches || !logical_path_matches {
            return Err("campaign replay checkpoint does not match the recorded stream".into());
        }
    }
    let draw_origin = match header.origin_kind.as_str() {
        ORIGIN_ARCHIVE => Some((
            header.origin_archive_sha256.as_deref().unwrap_or_default(),
            origin_report.ok_or("archive campaign replay requires the source archive")?,
        )),
        ORIGIN_GENESIS | ORIGIN_SNAPSHOT_ROOT => None,
        _ => return Err("campaign stream origin kind is not recognized".into()),
    };
    let (mut draw_state, replay_draw_header) =
        workload.initial_draw_state(&replay_run, draw_origin)?;
    if let Some(memory_budget_mib) = header.memory_budget_mib {
        let budget = memory_budget_mib.saturating_mul(1024 * 1024);
        if budget <= workload.draw_state_memory_reserve_bytes(&replay_run, header.action_limit) {
            return Err("recorded memory budget is too small for the bounded draw state".into());
        }
    }
    if !draw_state_memory_is_within_reserve(
        workload.draw_state_memory_bytes(&draw_state),
        workload.draw_state_memory_reserve_bytes(&replay_run, header.action_limit),
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
        header.action_limit,
        header.archive_entry_limit,
        header.memory_budget_mib,
    );
    core.record_progress = true;
    core.bounded_progress_curve = true;
    core.archive.selector_policy = replay_selector.clone();
    core.archive
        .enable_continuations(replay_mixture.uses_continuations());
    let mut counters = CampaignCounters::new(header.workers);
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
        ORIGIN_ARCHIVE => Some(core.import_tree(
            workload,
            &replay_run,
            &mut target,
            origin_report.ok_or("archive campaign replay requires the source archive")?,
            origin_checkpoint.map(|checkpoint| &checkpoint.snapshots),
        )?),
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
        .saturating_add(workload.draw_state_memory_bytes(&draw_state));
    if !resident_memory_is_within_budget(bootstrap_memory_bytes, header.memory_budget_mib) {
        return Err(
            "campaign replay bootstrap state exceeds its deterministic memory budget".into(),
        );
    }

    core.archive.establish_liveness_anchor(header.action_limit);

    let replay_window_depth = admission_window_depth(
        usize::try_from(header.workers)?,
        schedule_policy_window(&header.schedule_policy)
            .ok_or("campaign stream schedule policy is not recognized")?,
    );
    let mut replay_metadata_uses = BTreeMap::<u64, u32>::new();
    let mut replay_job_snapshots =
        BTreeMap::<usize, (Arc<G::Snapshot>, Vec<G::Action>, u64)>::new();
    core.archive.preserve_inactive_snapshots(false)?;
    for (job_slot, parent_id) in replay_job_parents
        .iter()
        .take(replay_window_depth)
        .enumerate()
    {
        let parent_index = core
            .archive
            .index_of_id(*parent_id)
            .ok_or("initial replay job names a parent the archive does not hold")?;
        replay_job_snapshots.insert(job_slot, core.archive.pin_job_origin(parent_index)?);
        for metadata_id in &replay_job_metadata[job_slot] {
            let uses = replay_metadata_uses.entry(*metadata_id).or_default();
            *uses = uses
                .checked_add(1)
                .ok_or("recorded metadata use count overflow")?;
        }
    }
    core.archive
        .preserve_recorded_metadata_uses(replay_metadata_uses.clone());

    let mut replay_job_index = 0_usize;
    for line in record_lines {
        let record: CampaignStreamRecord<G::DrawCheckpoint> = serde_json::from_str(line)?;
        match record {
            CampaignStreamRecord::Skip(skip) => {
                let parent_index = core
                    .archive
                    .index_of_id(skip.parent_id)
                    .ok_or("recorded skip names a parent the archive does not hold")?;
                let strategy =
                    energy_strategy(skip.mutation_seed, skip.mixture_weight, skip.splice_weight)?;
                let spliced = replay_splice::<G>(strategy, skip.splice)?;
                let draw_checkpoint_before = skip.draw_checkpoint_before;
                let mut suffix = match spliced {
                    Some(tail) => tail,
                    None => workload.expand_suffix_recorded(
                        &replay_run,
                        &draw_state,
                        replay_suffix,
                        MixtureDraw {
                            mixture: replay_mixture,
                            weight: skip.mixture_weight,
                            splice_weight: skip.splice_weight,
                        },
                        draw_checkpoint_before.as_ref(),
                        skip.mutation_seed,
                    )?,
                };
                replay_suffix.bound_cost(&mut suffix, action_cost, max_action_cost);
                if !core.all_prefixes_archived(parent_index, &suffix) {
                    return Err("recorded skip is not a duplicate at its stream position".into());
                }
                let worker = usize::try_from(skip.worker)?;
                if worker >= counters.skips_per_worker.len() {
                    return Err("recorded skip names an unknown worker".into());
                }
                if skip.selector.path == SelectorPath::Continuation {
                    return Err("continuations cannot be recorded skips".into());
                }
                verify_selector_annotation(&skip.selector)?;
                core.archive.record_selection(parent_index, &skip.selector);
                core.archive.maintain_memory_budget()?;
                counters.duplicates_skipped = counters.duplicates_skipped.saturating_add(1);
                counters.skips_per_worker[worker] =
                    counters.skips_per_worker[worker].saturating_add(1);
                let draw_checkpoint_after =
                    workload.finish_stream_record(&replay_run, &mut draw_state, &[])?;
                if draw_checkpoint_after != skip.draw_checkpoint_after {
                    return Err("replayed skip draw-table checkpoint diverged".into());
                }
                workload.remember_draw_version(&mut draw_state, &required_draw_versions)?;
            }
            CampaignStreamRecord::Job(job) => {
                if job.selector.path == SelectorPath::Continuation
                    && (!replay_mixture.uses_continuations()
                        || !job.sequence.is_multiple_of(4)
                        || job.mixture_weight != 0
                        || job.splice_weight != u8::MAX
                        || !matches!(&job.splice, Some(CampaignSpliceRecord::Tail { .. })))
                {
                    return Err(
                        "continuation job lacks its registered schedule or complete tail".into(),
                    );
                }
                let replay_job_slot = replay_job_index;
                replay_job_index = replay_job_index.saturating_add(1);
                let parent_index = core
                    .archive
                    .index_of_id(job.parent_id)
                    .ok_or("recorded job names a parent the archive does not hold")?;
                let ((snapshot, replay, snapshot_id), parent_actions, parent_milestones) = {
                    let entry = core
                        .archive
                        .entries
                        .get(parent_index)
                        .ok_or("recorded job names a parent the archive does not hold")?;
                    (
                        replay_job_snapshots
                            .remove(&replay_job_slot)
                            .ok_or("recorded job has no replayed in-flight snapshot")?,
                        entry.input_len,
                        entry.milestones,
                    )
                };
                let strategy =
                    energy_strategy(job.mutation_seed, job.mixture_weight, job.splice_weight)?;
                let spliced = replay_splice::<G>(strategy, job.splice.clone())?;
                let draw_checkpoint_before = job.draw_checkpoint_before;
                let mut suffix = match spliced {
                    Some(tail) => tail,
                    None => workload.expand_suffix_recorded(
                        &replay_run,
                        &draw_state,
                        replay_suffix,
                        MixtureDraw {
                            mixture: replay_mixture,
                            weight: job.mixture_weight,
                            splice_weight: job.splice_weight,
                        },
                        draw_checkpoint_before.as_ref(),
                        job.mutation_seed,
                    )?,
                };
                replay_suffix.bound_cost(&mut suffix, action_cost, max_action_cost);
                let job_execution_work_before = workload.execution_work(&target);
                let result = workload.execute_job(
                    &replay_run,
                    &mut target,
                    &snapshot,
                    &replay,
                    parent_actions,
                    parent_milestones,
                    &suffix,
                    header.action_limit,
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
                let digest = workload.result_sha256(&result)?;
                if digest != job.result_sha256 {
                    return Err(format!(
                        "replayed job {} result digest {digest} diverged from recorded {}",
                        job.sequence, job.result_sha256
                    )
                    .into());
                }
                drop(snapshot);
                let objectives_before = core.objectives_reached;
                let (sequence, decisions) = core.admit_job(workload, job.parent_id, result)?;
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
                let draw_checkpoint_after =
                    finish_record(workload, &replay_run, &mut draw_state, &core, &decisions)?;
                if draw_checkpoint_after != job.draw_checkpoint_after {
                    return Err(format!(
                        "replayed job {} draw-table checkpoint diverged",
                        job.sequence
                    )
                    .into());
                }
                workload.remember_draw_version(&mut draw_state, &required_draw_versions)?;
                verify_selector_annotation(&job.selector)?;
                if replay_mixture.isolates_continuations()
                    && job.selector.path == SelectorPath::Continuation
                {
                    core.archive.record_isolated_continuation(parent_index);
                } else {
                    core.archive.record_selection(parent_index, &job.selector);
                    let retained_ids = retained_archive_indexes(&core, &decisions);
                    core.archive.record_selection_outcome(
                        parent_index,
                        !retained_ids.is_empty(),
                        retained_ids
                            .iter()
                            .any(|id| core.archive.opened_new_slot(*id)),
                        retained_ids
                            .iter()
                            .any(|id| core.archive.opened_new_cell(*id)),
                    );
                }
                core.archive.unpin_job_origin(snapshot_id);
                core.archive.unpin_metadata(job.parent_id);
                core.archive.maintain_memory_budget()?;
                for metadata_id in &replay_job_metadata[replay_job_slot] {
                    if let Some(uses) = replay_metadata_uses.get_mut(metadata_id) {
                        *uses = uses.saturating_sub(1);
                        if *uses == 0 {
                            replay_metadata_uses.remove(metadata_id);
                        }
                    }
                }
                if let Some(parent_id) =
                    replay_job_parents.get(replay_job_slot.saturating_add(replay_window_depth))
                {
                    let next_slot = replay_job_slot.saturating_add(replay_window_depth);
                    let parent_index = core
                        .archive
                        .index_of_id(*parent_id)
                        .ok_or("next replay job names a parent the archive does not hold")?;
                    replay_job_snapshots
                        .insert(next_slot, core.archive.pin_job_origin(parent_index)?);
                    for metadata_id in &replay_job_metadata[next_slot] {
                        let uses = replay_metadata_uses.entry(*metadata_id).or_default();
                        *uses = uses
                            .checked_add(1)
                            .ok_or("recorded metadata use count overflow")?;
                    }
                }
                core.archive
                    .preserve_recorded_metadata_uses(replay_metadata_uses.clone());
                let worker = usize::try_from(job.worker)?;
                if worker >= counters.jobs_per_worker.len() {
                    return Err("recorded job names an unknown worker".into());
                }
                counters.jobs_per_worker[worker] =
                    counters.jobs_per_worker[worker].saturating_add(1);
                counters.job_execution_work =
                    counters.job_execution_work.saturating_add(execution_work);
                if objectives_before == 0 && core.objectives_reached > 0 {
                    counters.note_first_objective(sequence);
                }
            }
        }
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
        resume_input_sha256: header.resume_input_sha256.clone(),
        resume_actions: header.resume_actions,
    };
    counters.draw_state_memory_bytes = workload.draw_state_memory_bytes(&draw_state);
    Ok(build_report(
        workload,
        &header,
        origin,
        core,
        &counters,
        stream_sha256,
        true,
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        ArchiveReportState, CampaignActionResult, CampaignAdmissionDecision, CampaignCandidate,
        CampaignConfig, CampaignCounters, CampaignJobRecord, CampaignJobResult, CampaignOrigin,
        CampaignSpliceRecord, CampaignStreamHeader, CampaignStreamRecord, CampaignTypes,
        CoordinatorCore, DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER, EnergyStrategy, Evaluation,
        InitialDrawState, InputPolicy, LiveCoordinatorProfile, MAX_PROGRESS_CURVE_POINTS,
        Reporting, SPLICE_ACTION_CAP, TargetExecution, WorkloadPolicies, admission_window_depth,
        archive_entry_limit_is_valid, compact_progress_curve, completed_results_within_bound,
        draw_state_memory_is_within_reserve, execution_work_delta, finish_record, is_zero_usize,
        live_coordinator_profile, postcard_value_sha256, profile_elapsed, profile_now,
        progress_checkpoint_due, progress_policy_is_supported, record_compaction_elapsed,
        replay_campaign_checkpointed, replay_splice, resident_memory_is_within_budget,
        retained_archive_indexes, run_campaign_checkpointed, schedule_policy_identifier,
        schedule_policy_is_supported, schedule_policy_window, stop_reservations_after_objective,
    };
    use crate::search::archive::{
        ArchiveEntryReport, ArchiveKey, Input, ProgressPoint, RetentionPolicy, SelectorDraw,
        SelectorPath, SelectorPolicy, entries_by_suffix,
    };
    use crate::search::draw::{DrawMixture, MixtureDraw, SuffixShape};
    use crate::search::empirical_steps::EmpiricalStepCheckpoint;
    use crate::search::rollout::{ExecutionDisposition, Outcome, Rollout, execute_suffix};
    use serde::{Deserialize, Serialize};
    use sha2::{Digest, Sha256};
    use std::{
        error::Error,
        time::{Duration, Instant},
    };

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

    fn test_action_cost(action: &TestAction) -> u64 {
        u64::from(action.hold_frames).saturating_mul(2)
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
            self.0
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
                .saturating_add(u64::from(action.hold_frames));
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

    struct TestWorkload {
        bootstrap_objective: bool,
    }
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
        type DrawState = ();
        type DrawCheckpoint = ();
        type DrawHeader = ();
    }

    impl Reporting for TestWorkload {
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

    impl InputPolicy for TestWorkload {
        fn draw_state_memory_reserve_bytes(&self, _run: &Self::Run, _max_actions: usize) -> usize {
            0
        }
        fn draw_state_memory_bytes(&self, _state: &Self::DrawState) -> usize {
            0
        }
        fn policies(&self, _run: &Self::Run) -> WorkloadPolicies {
            WorkloadPolicies::new()
        }
        fn resolve_recorded(
            &self,
            _policies: &WorkloadPolicies,
        ) -> Result<Self::Run, Box<dyn Error>> {
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
            Ok(self.bootstrap_objective && target.execution_work == 0)
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
            _evidence: &mut Self::Evidence,
            _source: &Self::ArchiveReport,
        ) {
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

    fn test_core() -> (TestWorkload, (), CoordinatorCore<TestWorkload>, TestTarget) {
        let workload = TestWorkload {
            bootstrap_objective: false,
        };
        let run = ();
        let mut core = CoordinatorCore::new(&workload, &run, 16, 1_024, None);
        let mut target = TestTarget::default();
        core.bootstrap(&workload, &run, &mut target)
            .expect("bootstrap generic core");
        (workload, run, core, target)
    }

    #[test]
    fn rollout_stops_on_an_already_terminal_parent() {
        let mut target = TestTarget::default();
        let mut rollout = TestRollout::new(&mut target);
        rollout.initial_terminal = true;
        let result = execute_suffix(
            &mut rollout,
            0,
            (),
            &[TestAction::new(1, 1)],
            8,
            RetentionPolicy::Unprobed,
            false,
        )
        .expect("terminal parent is a valid no-op rollout");
        assert!(result.actions.is_empty());
        assert_eq!(rollout.target.value, 0);
    }

    #[test]
    fn rollout_honors_limits_and_restores_after_each_probe() {
        let mut target = TestTarget::default();
        let mut rollout = TestRollout::new(&mut target);
        let result = execute_suffix(
            &mut rollout,
            1,
            (),
            &[
                TestAction::new(1, 1),
                TestAction::new(2, 1),
                TestAction::new(4, 1),
            ],
            3,
            RetentionPolicy::ProbeAtAdmission,
            false,
        )
        .expect("bounded rollout");
        assert_eq!(result.actions.len(), 2);
        assert_eq!(
            result.actions[0].candidate.as_ref().map(|c| c.key),
            Some(TestKey(1))
        );
        assert_eq!(
            result.actions[1].candidate.as_ref().map(|c| c.key),
            Some(TestKey(3))
        );
        assert_eq!(rollout.probe_calls, vec![1, 3]);
        assert_eq!(rollout.target.value, 3, "probe restores the live target");
    }

    #[test]
    fn rollout_stops_on_terminal_actions_and_propagates_failures() {
        let mut target = TestTarget::default();
        let mut rollout = TestRollout::new(&mut target);
        rollout.terminal_after = Some(2);
        rollout.objective_after = Some(2);
        let result = execute_suffix(
            &mut rollout,
            0,
            (),
            &[
                TestAction::new(1, 1),
                TestAction::new(1, 1),
                TestAction::new(1, 1),
            ],
            8,
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
                0,
                (),
                &[TestAction::new(1, 1)],
                8,
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
                0,
                (),
                &[TestAction::new(1, 1)],
                8,
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
            0,
            (),
            &[TestAction::new(1, 1), TestAction::new(1, 1)],
            8,
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
            1,
            (),
            &[TestAction::new(1, 1)],
            8,
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
            0,
            (),
            &[TestAction::new(1, 1), TestAction::new(1, 1)],
            8,
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
            0,
            (),
            &[TestAction::new(1, 1), TestAction::new(1, 1)],
            8,
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
            CampaignJobResult {
                actions: vec![CampaignActionResult {
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
            CampaignJobResult {
                actions: vec![CampaignActionResult {
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
    fn bootstrap_records_an_objective_at_the_empty_input() {
        let workload = TestWorkload {
            bootstrap_objective: true,
        };
        let run = ();
        let mut core = CoordinatorCore::new(&workload, &run, 16, 1_024, None);
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
                action_limit: 8,
                host: "test".to_owned(),
                wall_budget: None,
                stop_rollout_on_objective,
                stop_campaign_on_objective,
                archive_entry_limit: 16,
                reservations_per_worker: 1,
                memory_budget_mib: None,
                materialize_final_artifacts: true,
                run: (),
                suffix: SuffixShape::OneOrTwo,
                mixture: DrawMixture::AlphabetOnly,
                retention: RetentionPolicy::Unprobed,
                selector: SelectorPolicy::GroupUniform,
                objective_witness_path: Some(witness_path.clone()),
            };
            let workload = TestWorkload {
                bootstrap_objective: true,
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

    const RECORDED_HEADER: &str = r#"{"schema_version":2,"format":"campaign-v1","campaign_seed":7,"workers":2,
"schedule_policy":"deterministic_window_1_per_worker_v3","progress_policy":"mechanical_watermark_bounded_1024_v2",
"host":"box","origin_kind":"genesis","origin_path":null,"origin_archive_sha256":null,
"resume_input_sha256":"ab","resume_actions":0,"execution_budget":10,"stop_rollout_on_objective":true,"stop_campaign_on_objective":true,"wall_budget_seconds":null,
"action_limit":64,"archive_entry_limit":128,"controller_vocabulary":"nes_down_ten",
"key_policy":"frozen_area_span","duration_policy":"stratified","suffix_policy":"one_or_two",
"chord_policy":"chord_uniform","replacement_policy":"fewest_frames_in_level",
"resume_policy":"whole_tree","retention_policy":"unprobed",
"parent_scheduler":"hierarchy_uniform_128","executor_mode":"snapshot_resume_archive",
"worker_seed_derivation":"x","mixture_policy":"biased_half","workload_identity_sha256":"cd",
"action_cost_unit":"frames","execution_work_unit":"frames"}"#;

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
            actions: vec![CampaignActionResult {
                action,
                observations: Vec::new(),
                milestones: (),
                outcome: Outcome::default(),
                candidate: Some(CampaignCandidate {
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
    fn coordinator_counts_objectives_and_keeps_the_first_witness() {
        let (workload, _run, mut core, _target) = test_core();
        let winning = TestAction::new(0x81, 7);
        let result = CampaignJobResult::<TestWorkload> {
            actions: vec![CampaignActionResult {
                action: winning,
                observations: Vec::new(),
                milestones: (),
                outcome: Outcome {
                    objective_reached: true,
                    disposition: ExecutionDisposition::Runnable,
                },
                candidate: Some(CampaignCandidate {
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

        let later = CampaignJobResult {
            actions: vec![CampaignActionResult {
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
        let (report, _) = core.into_archive_report_and_snapshots(&workload, 0, true);
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
                selector: None,
            };
        let source = TestArchiveReport {
            entries: vec![
                entry(0, None, Vec::new()),
                entry(1, Some(0), vec![action(0x01)]),
                entry(2, Some(1), vec![action(0x01), action(0x02)]),
                entry(3, Some(1), vec![action(0x01), action(0x80)]),
                entry(9, Some(7), vec![action(0x01), action(0x02), action(0x40)]),
                entry(10, Some(2), vec![action(0x01); 5]),
            ],
        };
        let mut core = CoordinatorCore::new(&workload, &run, 4, 32_768, None);
        let suffix_json = serde_json::to_string(&source).expect("serialize source archive");
        assert!(suffix_json.contains("\"input_suffix\""));
        let rebuilt: TestArchiveReport =
            serde_json::from_str(&suffix_json).expect("load suffix archive");
        assert_eq!(rebuilt, source);
        let counts = core
            .import_tree(&workload, &run, &mut target, &source, None)
            .expect("import source archive");
        assert_eq!(counts.over_limit, 1);
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
                Some(encoded(vec![action; SPLICE_ACTION_CAP + 1])),
            )
            .is_err()
        );
        assert_eq!(
            replay_splice::<TestWorkload>(
                EnergyStrategy::Splice,
                Some(encoded(vec![action; SPLICE_ACTION_CAP])),
            )
            .expect("tail at cap is valid")
            .map(|tail| tail.len()),
            Some(SPLICE_ACTION_CAP)
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
    fn coordinator_profile_helpers_distinguish_disabled_and_elapsed() {
        assert!(!live_coordinator_profile(false).enabled);
        assert!(live_coordinator_profile(true).enabled);
        assert!(profile_now(false).is_none());
        assert!(profile_now(true).is_some());
        assert_eq!(profile_elapsed(None), 0);
        #[allow(clippy::disallowed_methods)]
        let started = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("one millisecond before now");
        assert!(profile_elapsed(Some(started)) > 0);

        let mut profile = LiveCoordinatorProfile::default();
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
            actions: vec![CampaignActionResult {
                action,
                observations: Vec::new(),
                milestones: (),
                outcome: Outcome::default(),
                candidate: Some(CampaignCandidate {
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
        assert_eq!(retained_archive_indexes(&core, &decisions), vec![0, 1]);

        let (mut draw_state, _header) = workload
            .initial_draw_state(&run, None)
            .expect("initialize draw state");
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
        let workers = 3;
        let depth = admission_window_depth(workers, DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER);
        assert_eq!(depth, workers * DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER);

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
    fn sliding_window_depth_saturates_without_overflow() {
        assert_eq!(
            admission_window_depth(0, DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER),
            0
        );
        assert_eq!(admission_window_depth(usize::MAX, 1), usize::MAX);
    }

    #[test]
    fn the_first_objective_counters_exclude_jobs_that_drain_after_the_objective() {
        let mut counters = CampaignCounters::new(4);
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
        let mut counters = CampaignCounters::new(1);
        counters.bootstrap_execution_work = 10;
        counters.job_execution_work = 20;
        assert_eq!(counters.work_to_first_objective, None);
        assert_eq!(counters.executions_to_first_objective, None);
    }

    #[test]
    fn schedule_policy_dispatch_accepts_only_current_windows() {
        let current = schedule_policy_identifier(DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER);
        assert_eq!(current, "deterministic_window_1_per_worker_v3");
        assert_eq!(
            schedule_policy_window(&current),
            Some(DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER)
        );
        assert!(schedule_policy_is_supported(&current));
        assert!(schedule_policy_is_supported(
            "deterministic_window_4_per_worker_v3"
        ));
        assert!(!schedule_policy_is_supported(
            "deterministic_window_4_per_worker_v2"
        ));
        assert!(!schedule_policy_is_supported(
            "deterministic_window_4_per_worker_v1"
        ));
        assert!(!schedule_policy_is_supported(
            "deterministic_window_64_per_worker_v1"
        ));
        assert!(!schedule_policy_is_supported("unknown-schedule"));
        assert!(!schedule_policy_is_supported(
            "deterministic_window_0_per_worker_v3"
        ));
        assert_eq!(
            schedule_policy_window("deterministic_window_4_per_worker_v3"),
            Some(4)
        );
        assert_eq!(
            schedule_policy_window("deterministic_window_4_per_worker_v1"),
            None
        );
    }

    #[test]
    fn unadmitted_result_jobs_obey_the_explicit_physical_bound() {
        for limit in [1, 2] {
            assert!(completed_results_within_bound(0, 0, limit));
            assert!(completed_results_within_bound(4 * limit, 4, limit));
            assert!(!completed_results_within_bound(4 * limit + 1, 4, limit));
            assert!(!completed_results_within_bound(1, 0, limit));
        }
    }

    #[test]
    fn recorded_resource_bounds_and_progress_policies_validate_at_boundaries() {
        let max = crate::search::archive::MAX_ARCHIVE_ENTRIES;
        assert!(!archive_entry_limit_is_valid(0));
        assert!(archive_entry_limit_is_valid(1));
        assert!(archive_entry_limit_is_valid(max));
        assert!(!archive_entry_limit_is_valid(max.saturating_add(1)));

        assert!(draw_state_memory_is_within_reserve(0, 0));
        assert!(draw_state_memory_is_within_reserve(1, 2));
        assert!(draw_state_memory_is_within_reserve(2, 2));
        assert!(!draw_state_memory_is_within_reserve(2, 1));
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
        assert_eq!(
            header.schedule_policy,
            "deterministic_window_1_per_worker_v3"
        );
        assert_eq!(
            header.progress_policy,
            "mechanical_watermark_bounded_1024_v2"
        );
        assert_eq!(header.mixture_policy, "biased_half");
        assert_eq!(header.suffix_policy, "one_or_two");
        assert_eq!(header.resume_policy, "whole_tree");
        assert_eq!(header.schema_version, super::CAMPAIGN_SCHEMA_VERSION);
        assert_eq!(header.draw_header, None);
        let expected: WorkloadPolicies = [
            ("controller_vocabulary", "nes_down_ten"),
            ("key_policy", "frozen_area_span"),
            ("duration_policy", "stratified"),
            ("chord_policy", "chord_uniform"),
            ("replacement_policy", "fewest_frames_in_level"),
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
        assert!(!object.contains_key("chord_table"));
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
                    bootstrap_objective: false
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
                    bootstrap_objective: false
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
    fn a_recorded_job_keeps_its_draw_checkpoint_field_names() {
        let line = r#"{"event":"job","sequence":1,"worker":0,"parent_id":0,"mutation_seed":9,
"execution_work":12,"result_sha256":"ef","decisions":[],"mixture_weight":128,"splice_weight":128,
"selector":{"path":"hierarchy_uniform","classes_skipped":0,"counter_reset":false},
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
        assert_eq!(job.selector.path, SelectorPath::HierarchyWalk);
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
        assert!(object.contains_key("draw_checkpoint_before"));
        assert!(object.contains_key("draw_checkpoint_after"));
        assert!(!object.contains_key("chord_table_before"));
        let round_trip: CampaignJobRecord<EmpiricalStepCheckpoint> =
            serde_json::from_value(written).expect("job round-trips");
        assert_eq!(round_trip, job);
        let _ = SelectorDraw {
            path: SelectorPath::HierarchyWalk,
            classes_skipped: 0,
            counter_reset: false,
            concentration: None,
        };
    }

    #[test]
    fn a_job_records_its_dispatch_time_splice_resolution() {
        let line = r#"{"event":"job","sequence":1,"worker":0,"parent_id":3,"mutation_seed":9,
"execution_work":12,"result_sha256":"ef","decisions":[],"mixture_weight":85,"splice_weight":85,
"splice":{"outcome":"tail","donor_id":4,"leaf_id":9,"tail_postcard":[1,2]},
"selector":{"path":"uniform","classes_skipped":0,"counter_reset":false}}"#
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
}

#[cfg(test)]
#[path = "campaign_continuation_tests.rs"]
mod continuation_tests;
