// SPDX-License-Identifier: AGPL-3.0-or-later

//! Game-neutral campaign coordinator with a recorded job stream.
//!
//! A campaign runs W workers on one machine against one shared archive. A job
//! is a pure function of (parent snapshot, mutation seed); the coordinator
//! serializes selection and admission through a deterministic sliding window,
//! and records the complete admission-ordered job stream. Physical workers
//! drain the window dynamically, but host completion order cannot reach
//! campaign state.
//!
//! Everything game-specific arrives through [`Game`]: target construction and
//! stepping, key and milestone decoding, alive/dead/won classification, the
//! draws a suffix is built from, and an opaque [`GamePolicies`] set naming
//! whatever policies the game owns. The search layer keeps the policies it
//! owns — the mutation shape, admission, selection, and the resume rule — and
//! records each under its own header field. A game supplies no name the
//! search layer reads.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt::Debug,
    io::Write,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::search::archive::{
    Archive, ArchiveCandidate, ArchiveEntryReport, ArchiveKey, CampaignSpliceTail, Input,
    ProgressPoint, RetentionPolicy, SelectorAccounting, SelectorDraw, SelectorPath, SelectorPolicy,
    retention_policy_from_identifier, retention_policy_identifier, selector_policy_identifier,
};
use crate::search::draw::{
    DrawMixture, EnergyStrategy, MIXTURE_BIASED_HALF_IDENTIFIER, MixtureDraw, MixtureEnergy,
    SuffixShape, draw_mixture_from_identifier, draw_mixture_identifier, energy_strategy,
    suffix_shape_from_identifier, suffix_shape_identifier,
};

/// Longest stored tail one splice draw appends, bounding a single job.
pub const SPLICE_ACTION_CAP: usize = 128;
use crate::search::empirical_steps::EmpiricalStepCheckpoint;
use crate::search::parallel::with_worker_pool;
use crate::search::rand::RomuDuoJrRand;

/// A campaign's finished report and its whole-tree snapshot checkpoint.
pub type CampaignOutcome<G> = (
    CampaignModeReport<<G as Game>::Action, <G as Game>::ArchiveReport>,
    SnapshotCheckpoint<<G as Game>::Snapshot>,
);

/// A game's initial draw state and the header provenance recorded for it.
pub type InitialDrawState<G> = (<G as Game>::DrawState, Option<<G as Game>::TableHeader>);

/// Fixed statement of the live worker schedule, recorded in new reports.
pub const CAMPAIGN_SCHEDULE_IDENTITY: &str = "jobs are selected into a deterministic sliding \
     window and admitted in reservation order; physical workers drain the window dynamically, \
     but host completion order cannot reach campaign state; the same seed, configuration, \
     origin, and game bytes produce the same recorded stream";

const LEGACY_CAMPAIGN_SCHEDULE_IDENTITY: &str = "the live schedule is not derivable from the seed \
     alone; the recorded stream is this campaign's identity; two live runs at one seed may \
     differ, and each replays exactly";
const CAMPAIGN_SCHEDULE_POLICY: &str = "deterministic_window_64_per_worker_v1";
const CAMPAIGN_PROGRESS_POLICY: &str = "mechanical_watermark_v1";
const ORIGIN_GENESIS: &str = "genesis";
const ORIGIN_SNAPSHOT_ROOT: &str = "snapshot_root";
const ORIGIN_ARCHIVE: &str = "archive";

/// Deterministic reservations kept ahead of admission per worker. The window
/// hides individual long jobs while bounding result memory and wall-budget
/// overshoot to a small number of jobs per core.
const WINDOW_JOBS_PER_WORKER: usize = 64;

/// Already-reserved jobs buffered in each physical executor. This is smaller
/// than the logical reorder window and changes only overlap: logical worker
/// identity, selection, and admission remain coordinator-owned.
const EXECUTOR_PREFETCH_JOBS: usize = 8;

/// Consecutive pre-execution duplicate skips after which a worker executes the
/// next drawn job anyway and lets admission deduplicate, so a saturated archive
/// cannot livelock selection.
const CONSECUTIVE_SKIP_LIMIT: u64 = 1_024;

/// Curve sampling interval in admitted executions.
const CURVE_INTERVAL: u64 = 100;

/// Seconds between sidecar observations.
const PROGRESS_INTERVAL_SECONDS: u64 = 60;

/// Identifier recorded for the resume rule: the source archive's whole
/// retained tree is imported, and the header's resume input is the frontier
/// identity only.
pub const RESUME_IDENTIFIER: &str = "whole_tree";

/// Identifier recorded when a challenge starts from one evaluator-supplied
/// snapshot with an empty challenge-local input.
pub const SNAPSHOT_ROOT_RESUME_IDENTIFIER: &str = "snapshot_root";

/// The recorded identifier strings of one run's game-owned policies, keyed by
/// the stream-header field each value is written to.
///
/// The generic layer never interprets a name or a value: it writes the set
/// into the header and the report, and hands the recorded set back to
/// [`Game::resolve_recorded`], which decides whether it names a run this
/// build can reproduce. A target with no controller and no input alphabet
/// records whatever names its own policies have instead.
pub type GamePolicies = BTreeMap<String, String>;

/// Everything one game supplies to run campaigns over it.
///
/// The trait is implemented on a context value holding the game image, so
/// methods can construct targets and stamp identity hashes. Worker threads
/// share the context, hence `Sync`.
pub trait Game: Sync {
    /// Emulated game instance a worker drives.
    type Target: Send;
    /// One recorded input action.
    type Action: Copy + Ord + Debug + Eq + Send + Sync + Serialize + DeserializeOwned;
    /// Archive key.
    type Key: ArchiveKey + Debug + Eq + Send + Sync;
    /// Milestone summary merged across executions.
    type Milestones: Copy + Default + Debug + Eq + Send + Sync + Serialize + DeserializeOwned;
    /// Route-agnostic mechanical progress merged across executions.
    type Progress: Copy + Default + Debug + Eq + Send + Sync + Serialize + DeserializeOwned;
    /// Restorable machine state.
    type Snapshot: Clone + Debug + Eq + Send + Sync + Serialize + DeserializeOwned;
    /// Per-frame observation recorded inside a job result.
    type Observations: Clone + Debug + Eq + Send + Sync + Serialize;
    /// Game-owned evidence accumulated outside the archive: watermarks,
    /// first-input tables, champions.
    type Evidence: Clone + Default;
    /// The game's archive report shape.
    type ArchiveReport: Clone;
    /// Per-run game policies (input vocabulary and draw policy).
    type Run: Clone + Sync;
    /// Live state of the recorded input-draw policy.
    type DrawState;
    /// Header provenance for a derived draw-table policy.
    type TableHeader: Clone + Debug + Eq + Serialize + DeserializeOwned;

    /// Stream format identifier written as the first line of every stream.
    fn stream_format(&self) -> &'static str;
    /// Format tag of the snapshot checkpoint file.
    fn checkpoint_format(&self) -> &'static str;
    /// SHA-256 of the game image bytes.
    fn image_sha256(&self) -> String;
    /// Ceiling on the per-run action limit.
    fn max_action_limit(&self) -> usize;
    /// Time-accounting function handed to the archive.
    fn action_time_fn(&self) -> fn(&Self::Action) -> u64;

    /// Incrementally digest one complete worker result for stream replay verification.
    fn result_sha256(&self, result: &CampaignJobResult<Self>) -> Result<String, Box<dyn Error>>;

    /// The recorded identifiers of one run's game-owned policies.
    fn policies(&self, run: &Self::Run) -> GamePolicies;
    /// Resolve a recorded policy set back into a run, rejecting any name or
    /// value that names no compiled policy.
    ///
    /// # Errors
    ///
    /// Returns an error for an unrecognized policy set.
    fn resolve_recorded(&self, policies: &GamePolicies) -> Result<Self::Run, Box<dyn Error>>;

    /// Build one worker's target.
    ///
    /// # Errors
    ///
    /// Returns an error when the target cannot be built.
    fn new_target(&self) -> Result<Self::Target, String>;
    /// Reset a target to gameplay genesis.
    fn reset(&self, target: &mut Self::Target);
    /// Restore a snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when the restore fails.
    fn restore(
        &self,
        target: &mut Self::Target,
        snapshot: &Self::Snapshot,
    ) -> Result<(), Box<dyn Error>>;
    /// Frames the target has emulated over its lifetime.
    fn frames_clocked(&self, target: &Self::Target) -> u64;
    /// Apply one action and merge its milestone evidence.
    ///
    /// # Errors
    ///
    /// Returns an error when milestone decoding fails.
    fn apply_action(
        &self,
        target: &mut Self::Target,
        action: &Self::Action,
        milestones: &mut Self::Milestones,
    ) -> Result<(), Box<dyn Error>>;
    /// Whether the target is dead or failed, ending an imported walk.
    fn is_terminal(&self, target: &Self::Target) -> bool;
    /// Whether the target satisfies any terminal condition recorded by this run.
    ///
    /// # Errors
    ///
    /// Returns an error when the terminal state cannot be observed reliably.
    fn is_run_terminal(
        &self,
        run: &Self::Run,
        target: &Self::Target,
    ) -> Result<bool, Box<dyn Error>>;
    /// Snapshot the target.
    ///
    /// # Errors
    ///
    /// Returns an error when snapshotting fails.
    fn snapshot(&self, target: &mut Self::Target) -> Result<Self::Snapshot, Box<dyn Error>>;
    /// Decode the completed archive key of the target's current state.
    ///
    /// # Errors
    ///
    /// Returns an error when decoding fails.
    fn current_key(&self, target: &Self::Target) -> Result<Self::Key, Box<dyn Error>>;
    /// Complete a worker-decoded candidate key against its snapshot. Workers
    /// leave ancestry-dependent fields canonical so result digests stay
    /// independent of them.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot cannot supply the identity.
    fn complete_candidate_key(
        &self,
        key: Self::Key,
        snapshot: &Self::Snapshot,
    ) -> Result<Self::Key, Box<dyn Error>>;

    /// Execute one job: restore the parent snapshot and apply the suffix,
    /// collecting per-boundary candidates with worker-side probe verdicts.
    ///
    /// # Errors
    ///
    /// Returns an error when emulation or snapshotting fails.
    #[allow(clippy::too_many_arguments)]
    fn execute_job(
        &self,
        run: &Self::Run,
        target: &mut Self::Target,
        parent_snapshot: &Self::Snapshot,
        parent_actions: usize,
        parent_milestones: Self::Milestones,
        suffix: &[Self::Action],
        max_actions: usize,
        retention: RetentionPolicy,
    ) -> Result<CampaignJobResult<Self>, Box<dyn Error>>;

    /// Build the run's initial draw state and, for a derived policy, the
    /// header provenance recorded for it. `origin` carries the source archive
    /// and its file hash when the run resumes one.
    ///
    /// # Errors
    ///
    /// Returns an error when the fold fails.
    fn initial_draw_state(
        &self,
        run: &Self::Run,
        origin: Option<(&str, &Self::ArchiveReport)>,
    ) -> Result<InitialDrawState<Self>, Box<dyn Error>>;
    /// The draw state's current checkpoint, when the policy records one.
    ///
    /// # Errors
    ///
    /// Returns an error when the checkpoint cannot be computed.
    fn draw_checkpoint(
        &self,
        state: &Self::DrawState,
    ) -> Result<Option<EmpiricalStepCheckpoint>, Box<dyn Error>>;
    /// Expand one mutation seed into its complete suffix from the live draw
    /// state, under the search layer's mutation `shape`.
    ///
    /// # Errors
    ///
    /// Returns an error when a draw bound is invalid.
    fn expand_suffix(
        &self,
        run: &Self::Run,
        state: &Self::DrawState,
        shape: SuffixShape,
        mixture: MixtureDraw,
        mutation_seed: u64,
    ) -> Result<Vec<Self::Action>, Box<dyn Error>>;
    /// Expand one recorded mutation seed against the recorded draw-state
    /// version, verifying the version exists and matches.
    ///
    /// # Errors
    ///
    /// Returns an error when the recorded version is unknown or mismatched.
    fn expand_suffix_recorded(
        &self,
        run: &Self::Run,
        state: &Self::DrawState,
        shape: SuffixShape,
        mixture: MixtureDraw,
        before: Option<&EmpiricalStepCheckpoint>,
        mutation_seed: u64,
    ) -> Result<Vec<Self::Action>, Box<dyn Error>>;
    /// Fold the record's retained inputs into the draw state and close the
    /// record, returning the periodic checkpoint when one is due. Each
    /// retained input arrives with its parent's action count so the game's
    /// recorded fold rule can consume the full input or only its new suffix.
    ///
    /// # Errors
    ///
    /// Returns an error when the fold fails.
    fn finish_stream_record(
        &self,
        run: &Self::Run,
        state: &mut Self::DrawState,
        retained: &[(usize, &[Self::Action])],
    ) -> Result<Option<EmpiricalStepCheckpoint>, Box<dyn Error>>;
    /// Remember the current draw-state version when a recorded stream will
    /// need it, so replay can re-derive suffixes drawn against it.
    ///
    /// # Errors
    ///
    /// Returns an error when the checkpoint cannot be computed.
    fn remember_draw_version(
        &self,
        state: &mut Self::DrawState,
        required: &BTreeSet<u64>,
    ) -> Result<(), Box<dyn Error>>;

    /// Merge one milestone summary into another, keeping the strongest of
    /// each rung.
    fn merge_milestones(&self, into: &mut Self::Milestones, from: Self::Milestones);
    /// The strongest milestone summary the evidence has accumulated.
    fn aggregate_milestones(evidence: &Self::Evidence) -> Self::Milestones;
    /// The strongest route-agnostic progress the evidence has accumulated.
    fn aggregate_progress(evidence: &Self::Evidence) -> Self::Progress;
    /// Merge a resumed source archive's whole-run evidence.
    fn merge_origin_evidence(&self, evidence: &mut Self::Evidence, source: &Self::ArchiveReport);
    /// Initialize game-owned evidence from an evaluator-supplied snapshot root.
    ///
    /// # Errors
    ///
    /// Returns an error when the restored target cannot be observed.
    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut Self::Evidence,
        target: &Self::Target,
    ) -> Result<(), Box<dyn Error>>;
    /// Merge one imported entry's evidence.
    fn merge_import_evidence(
        &self,
        evidence: &mut Self::Evidence,
        milestones: Self::Milestones,
        input: &Input<Self::Action>,
    );
    /// Merge one executed action's evidence at its admission sequence.
    fn merge_action_evidence(
        &self,
        evidence: &mut Self::Evidence,
        action: &CampaignActionResult<Self>,
        sequence: u64,
        input: &Input<Self::Action>,
    );
    /// The retained entries of a source archive report.
    fn source_entries<'a>(
        &self,
        source: &'a Self::ArchiveReport,
    ) -> &'a [ArchiveEntryReport<Self::Action, Self::Key, Self::Milestones>];
    /// A source archive's frontier resume input.
    ///
    /// # Errors
    ///
    /// Returns an error when the source holds no retained entries.
    fn resume_input(
        &self,
        source: &Self::ArchiveReport,
    ) -> Result<Input<Self::Action>, Box<dyn Error>>;
    /// Assemble the game's archive report from the campaign's final state.
    fn archive_report(
        &self,
        evidence: &Self::Evidence,
        state: ArchiveReportState<Self>,
    ) -> Self::ArchiveReport;
}

/// The generic half of the final archive state, handed to
/// [`Game::archive_report`].
pub struct ArchiveReportState<G: Game + ?Sized> {
    /// Campaign seed.
    pub seed: u64,
    /// Admitted executions.
    pub executions: u64,
    /// Insertion-ordered entry reports.
    pub entries: Vec<ArchiveEntryReport<G::Action, G::Key, G::Milestones>>,
    /// Fixed-interval deterministic progress curve.
    pub progress_curve: Vec<ProgressPoint<G::Milestones, G::Progress>>,
    /// Candidates retained.
    pub retained: u64,
    /// Candidates rejected.
    pub rejected: u64,
    /// Dead endpoints observed.
    pub deaths: u64,
    /// Selector accounting.
    pub selector: SelectorAccounting,
}

/// Where a campaign starts: clean genesis, one evaluator snapshot, or a
/// recorded source archive.
pub enum CampaignOrigin<G: Game + ?Sized> {
    /// Start from gameplay genesis with a single empty input.
    Genesis,
    /// Start from one restorable snapshot with an empty challenge-local input.
    SnapshotRoot {
        /// The one-entry snapshot checkpoint and its recorded logical identity.
        checkpoint: CampaignCheckpoint<G::Snapshot>,
    },
    /// Resume a recorded archive with its whole retained tree.
    Archive {
        /// Path string recorded verbatim in the stream header.
        path: String,
        /// SHA-256 of the source archive file bytes.
        file_sha256: String,
        /// The parsed source archive report.
        report: Box<G::ArchiveReport>,
        /// Snapshot checkpoint of the source archive, when one was supplied.
        checkpoint: Option<CampaignCheckpoint<G::Snapshot>>,
    },
}

/// A loaded snapshot checkpoint and the file identity recorded for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CampaignCheckpoint<S> {
    /// Path string recorded verbatim in the stream header.
    pub path: String,
    /// SHA-256 of the checkpoint file bytes.
    pub file_sha256: String,
    /// The decoded snapshots.
    pub snapshots: SnapshotCheckpoint<S>,
}

/// Every retained entry's snapshot, keyed by archive identifier, so a
/// whole-tree resume can restore the population instead of re-emulating it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "S: Serialize + DeserializeOwned")]
pub struct SnapshotCheckpoint<S> {
    /// The game's checkpoint format tag.
    pub format: String,
    /// Snapshots in archive identifier order.
    pub entries: Vec<SnapshotCheckpointEntry<S>>,
}

/// One archive entry's snapshot.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "S: Serialize + DeserializeOwned")]
pub struct SnapshotCheckpointEntry<S> {
    /// Archive identifier the snapshot belongs to.
    pub id: u64,
    /// The retained snapshot.
    pub snapshot: S,
}

impl<S: Serialize + DeserializeOwned> SnapshotCheckpoint<S> {
    /// Encode the checkpoint as bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the encoder fails.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Box<dyn Error>> {
        Ok(postcard::to_allocvec(self)?)
    }

    /// Decode a checkpoint, refusing any other format tag.
    ///
    /// # Errors
    ///
    /// Returns an error when the bytes do not decode or carry another format.
    pub fn from_bytes(bytes: &[u8], expected_format: &str) -> Result<Self, Box<dyn Error>> {
        let checkpoint: Self = postcard::from_bytes(bytes)?;
        if checkpoint.format != expected_format {
            return Err("snapshot checkpoint format is not recognized".into());
        }
        Ok(checkpoint)
    }
}

/// Fixed configuration for one live campaign run.
pub struct CampaignConfig<G: Game + ?Sized> {
    /// Campaign seed from which every worker stream derives.
    pub campaign_seed: u64,
    /// Number of worker threads.
    pub workers: u32,
    /// Number of executed jobs the campaign admits, unless stopped by wall budget.
    pub execution_budget: u64,
    /// Bounded clean-reset action horizon.
    pub action_limit: usize,
    /// Operator-supplied host name recorded in the header; never probed.
    pub host: String,
    /// Optional live-only wall cutoff that stops issuing new reservations.
    ///
    /// It never enters campaign state: the stream that was recorded up to the
    /// cutoff still replays exactly.
    pub wall_budget: Option<Duration>,
    /// Archive entry bound for this run, recorded in the header and report.
    pub archive_entry_limit: usize,
    /// Game-owned per-run policies, recorded in the header and report.
    pub run: G::Run,
    /// Mutation shape for this run, recorded in the header and report.
    pub suffix: SuffixShape,
    /// Draw mixture for this run, recorded in the header and report.
    pub mixture: DrawMixture,
    /// Admission rule for this run, recorded in the header and report.
    pub retention: RetentionPolicy,
    /// Parent selector for this run, recorded in the header and report.
    pub selector: SelectorPolicy,
    /// Live-only: where the first winning input is written the moment it is
    /// admitted, before the in-flight jobs drain. Never recorded.
    pub victory_input_path: Option<PathBuf>,
}

/// First line of the stream: everything a replay needs to know about the run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "T: Serialize + DeserializeOwned")]
pub struct CampaignStreamHeader<T> {
    /// Stream format identifier.
    pub format: String,
    /// Campaign seed.
    pub campaign_seed: u64,
    /// Worker count W.
    pub workers: u32,
    /// Live admission-order policy. Absent from historical completion-order streams.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_policy: Option<String>,
    /// Deterministic progress-curve payload policy. Absent from streams whose
    /// curve points predate mechanical progress watermarks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_policy: Option<String>,
    /// Operator-supplied host name.
    pub host: String,
    /// Origin kind: `genesis`, `snapshot_root`, or `archive`.
    pub origin_kind: String,
    /// Source archive path for archive origins.
    pub origin_path: Option<String>,
    /// SHA-256 of the source archive file bytes for archive origins.
    pub origin_archive_sha256: Option<String>,
    /// Snapshot checkpoint path when an origin restored from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_checkpoint_path: Option<String>,
    /// SHA-256 of the snapshot checkpoint file bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_checkpoint_sha256: Option<String>,
    /// SHA-256 of the serialized resume input.
    pub resume_input_sha256: String,
    /// Action count of the resume input.
    pub resume_actions: usize,
    /// Execution budget requested for the run.
    pub execution_budget: u64,
    /// Wall budget in seconds when one was set.
    pub wall_budget_seconds: Option<u64>,
    /// Bounded clean-reset action horizon.
    pub action_limit: usize,
    /// Archive entry bound the run retained under.
    pub archive_entry_limit: usize,
    /// Resume rule identifier.
    pub resume_policy: String,
    /// Mutation shape identifier.
    pub suffix_policy: String,
    /// Draw mixture identifier; streams written before the mixture became a
    /// run option carry no field and replay under the biased half.
    #[serde(default = "default_mixture_policy")]
    pub mixture_policy: String,
    /// The game-owned policy identifiers of the run, one header field each.
    #[serde(flatten)]
    pub game_policies: GamePolicies,
    /// Derived draw-table provenance; absent for uniform tables. The field
    /// keeps its recorded name so streams written before the generic
    /// draw-table naming still replay.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "chord_table"
    )]
    pub draw_table: Option<T>,
    /// Admission rule identifier.
    pub retention_policy: String,
    /// Parent selector identifier.
    pub parent_scheduler: String,
    /// Executor mode identifier.
    pub executor_mode: String,
    /// How per-worker stream seeds derive from (campaign seed, worker index).
    pub worker_seed_derivation: String,
    /// SHA-256 of the game image bytes.
    pub rom_sha256: String,
}

/// Mixture recorded implicitly by streams written before the mixture became
/// a run option.
fn default_mixture_policy() -> String {
    MIXTURE_BIASED_HALF_IDENTIFIER.to_owned()
}

/// Weight recorded implicitly by streams written before the mixture adapted.
fn default_mixture_weight() -> u8 {
    128
}

/// Dispatch-time resolution of a splice-strategy draw.
///
/// New streams record both a successful donor/leaf choice and a deterministic
/// fall back to the alphabet. Older streams omit this field and replay by
/// deriving the splice from their serial archive state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CampaignSpliceRecord {
    /// No stored donor advanced beyond the selected parent at dispatch.
    Unavailable,
    /// Append the recorded donor's path to this recorded descendant.
    Tail {
        /// Dispatch-time selection-cell donor archive id.
        donor_id: u64,
        /// Dispatch-time deepest descendant archive id.
        leaf_id: u64,
    },
}

/// One admission decision for one candidate boundary, in candidate order.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum CampaignAdmissionDecision {
    /// The candidate entered the archive with this new id.
    Retained {
        /// Assigned insertion-order archive id.
        id: u64,
    },
    /// The candidate input already had this archive id.
    Duplicate {
        /// Existing archive id resolved by the input hash.
        id: u64,
    },
    /// Bounded quality-diversity retention rejected the candidate.
    Rejected,
    /// No fixed probe mask kept the candidate alive for the horizon.
    ProbeRefused,
    /// The action reached the run's recorded success predicate; the lineage
    /// ends here and its input is the campaign's winning input.
    Victory,
}

/// Stream record for one executed, admitted job.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CampaignJobRecord {
    /// Admission-order sequence number, starting at one.
    pub sequence: u64,
    /// Worker index that executed the job.
    pub worker: u32,
    /// Archive id of the selected parent.
    pub parent_id: u64,
    /// Mutation seed drawn from the worker's stream; it alone determines the suffix.
    pub mutation_seed: u64,
    /// Frames the job emulated, admission probes included.
    pub frames: u64,
    /// SHA-256 of the serialized job result, snapshots included.
    pub result_sha256: String,
    /// Ordered admission decisions for the job's candidates.
    pub decisions: Vec<CampaignAdmissionDecision>,
    /// Biased-strategy weight out of 256 the energy mixture used for this
    /// draw; the recorded half-weight for every other mixture and for
    /// streams written before the mixture adapted.
    #[serde(default = "default_mixture_weight")]
    pub mixture_weight: u8,
    /// Splice-strategy weight out of 256 the splice mixture used for this
    /// draw; zero for every other mixture and for older streams.
    #[serde(default)]
    pub splice_weight: u8,
    /// Dispatch-time splice resolution. Absent for non-splice draws and
    /// historical streams that predate explicit concurrent splice evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub splice: Option<CampaignSpliceRecord>,

    /// Selector draw record.
    pub selector: SelectorDraw,
    /// Derived table version used to draw this job. The field keeps its
    /// recorded name so streams written before the generic draw-table naming
    /// still replay.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "chord_table_before"
    )]
    pub draw_table_before: Option<EmpiricalStepCheckpoint>,
    /// Periodic derived table hash after admitting this stream record.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "chord_table_after"
    )]
    pub draw_table_after: Option<EmpiricalStepCheckpoint>,
}

/// Stream record for one job skipped before execution as a known duplicate.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CampaignSkipRecord {
    /// Worker index that drew the duplicate.
    pub worker: u32,
    /// Archive id of the selected parent.
    pub parent_id: u64,
    /// Mutation seed whose full prefix chain was already archived.
    pub mutation_seed: u64,
    /// Biased-strategy weight out of 256 the energy mixture used for this
    /// draw; the recorded half-weight for every other mixture and for
    /// streams written before the mixture adapted.
    #[serde(default = "default_mixture_weight")]
    pub mixture_weight: u8,
    /// Splice-strategy weight out of 256 the splice mixture used for this
    /// draw; zero for every other mixture and for older streams.
    #[serde(default)]
    pub splice_weight: u8,
    /// Dispatch-time splice resolution. Skips are synchronous, but recording
    /// it keeps every new splice draw self-contained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub splice: Option<CampaignSpliceRecord>,

    /// Selector draw record.
    pub selector: SelectorDraw,
    /// Derived table version used to draw this skipped job. The field keeps
    /// its recorded name so streams written before the generic draw-table
    /// naming still replay.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "chord_table_before"
    )]
    pub draw_table_before: Option<EmpiricalStepCheckpoint>,
    /// Periodic derived table hash after this stream record.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "chord_table_after"
    )]
    pub draw_table_after: Option<EmpiricalStepCheckpoint>,
}

/// One line of the recorded stream after the header.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CampaignStreamRecord {
    /// An executed job admitted at its sequence position.
    Job(CampaignJobRecord),
    /// A pre-execution duplicate skip; consumes no budget and changes no state.
    Skip(CampaignSkipRecord),
}

/// Origin summary recorded in the campaign report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CampaignOriginRecord {
    /// Origin kind: `genesis` or `archive`.
    pub kind: String,
    /// Source archive path for archive origins.
    pub path: Option<String>,
    /// SHA-256 of the source archive file bytes for archive origins.
    pub archive_sha256: Option<String>,
    /// Snapshot checkpoint path when a whole-tree resume restored from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_path: Option<String>,
    /// SHA-256 of the snapshot checkpoint file bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_sha256: Option<String>,
    /// SHA-256 of the serialized resume input.
    pub resume_input_sha256: String,
    /// Action count of the resume input.
    pub resume_actions: usize,
}

/// Complete deterministic report for one campaign, live or replayed.
///
/// Every field derives from the stream header, the recorded stream, and the
/// origin; no field carries wall-clock state, so a replay reproduces the
/// report byte for byte.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "A: Serialize + DeserializeOwned + Ord + Clone, R: Serialize + DeserializeOwned")]
pub struct CampaignModeReport<A: Ord, R> {
    /// Always `campaign`.
    pub mode: String,
    /// Campaign seed.
    pub campaign_seed: u64,
    /// Worker count W of the live run.
    pub workers: u32,
    /// Operator-supplied host name of the live run.
    pub host: String,
    /// Fixed statement of the campaign determinism trade.
    pub schedule_identity: String,
    /// Origin summary.
    pub origin: CampaignOriginRecord,
    /// Execution budget requested for the run.
    pub execution_budget: u64,
    /// Jobs actually executed and admitted.
    pub executions_completed: u64,
    /// Wall budget in seconds when one was set for the live run.
    pub wall_budget_seconds: Option<u64>,
    /// Bounded clean-reset action horizon.
    pub action_limit: usize,
    /// Archive entry bound the run retained under.
    pub archive_entry_limit: usize,
    /// Resume rule identifier.
    pub resume_policy: String,
    /// Mutation shape identifier.
    pub suffix_policy: String,
    /// Draw mixture identifier; reports written before the mixture became a
    /// run option carry no field and describe the biased half.
    #[serde(default = "default_mixture_policy")]
    pub mixture_policy: String,
    /// The game-owned policy identifiers of the run, one report field each.
    #[serde(flatten)]
    pub game_policies: GamePolicies,
    /// Admission rule identifier.
    pub retention_policy: String,
    /// Parent selector identifier.
    pub parent_scheduler: String,
    /// Executor mode identifier.
    pub executor_mode: String,
    /// How per-worker stream seeds derive from (campaign seed, worker index).
    pub worker_seed_derivation: String,
    /// SHA-256 of the game image bytes.
    pub rom_sha256: String,
    /// Frames emulated by the origin bootstrap walk, probes included.
    pub bootstrap_frames: u64,
    /// Outcome counts of the whole-tree import; absent at genesis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_import: Option<TreeImportCounts>,
    /// Bootstrap frames plus every job's frames, probes included.
    pub frames_emulated: u64,
    /// Jobs skipped before execution as known duplicates.
    pub duplicates_skipped: u64,
    /// Candidates refused by the admission probe.
    pub probe_refused: u64,
    /// Actions that reached the run's recorded success predicate.
    pub victories: u64,
    /// The first input that reached the success predicate, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub victory_input: Option<Input<A>>,
    /// Cell collisions the time-in-group replacement rule decided.
    pub replacement_frames_displaced: u64,
    /// Executed jobs per worker index.
    pub jobs_per_worker: Vec<u64>,
    /// Pre-execution duplicate skips per worker index.
    pub skips_per_worker: Vec<u64>,
    /// SHA-256 of the complete stream file bytes.
    pub stream_sha256: String,
    /// The archive in the standard report shape used by film and audits.
    pub archive: R,
}

/// Outcome counts of a whole-tree import, recorded in the campaign report.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TreeImportCounts {
    /// Source entries retained as new entries.
    pub imported: u64,
    /// Source entries whose rebuilt input was already retained.
    pub duplicate: u64,
    /// Source entries the archive refused under this run's replacement rules.
    pub rejected: u64,
    /// Source entries whose rebuilt walk ended dead or failed.
    pub terminal: u64,
    /// Source entries longer than this run's action limit.
    pub over_limit: u64,
    /// Source entries attached to an ancestor other than their recorded parent.
    pub rerooted: u64,
    /// Source entries restored from the snapshot checkpoint instead of emulated.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub checkpointed: u64,
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

/// One candidate boundary inside a job result.
#[derive(Serialize)]
#[serde(bound = "")]
pub struct CampaignCandidate<G: Game + ?Sized> {
    /// Worker-decoded archive key, ancestry fields canonical.
    pub key: G::Key,
    /// Worker-side probe verdict under the run's admission rule.
    pub viable: bool,
    /// The boundary's snapshot.
    pub snapshot: G::Snapshot,
}

impl<G: Game + ?Sized> PartialEq for CampaignCandidate<G> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.viable == other.viable && self.snapshot == other.snapshot
    }
}
impl<G: Game + ?Sized> Eq for CampaignCandidate<G> {}

impl<G: Game + ?Sized> Clone for CampaignCandidate<G> {
    fn clone(&self) -> Self {
        Self {
            key: self.key,
            viable: self.viable,
            snapshot: self.snapshot.clone(),
        }
    }
}

impl<G: Game + ?Sized> Debug for CampaignCandidate<G> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CampaignCandidate")
            .field("key", &self.key)
            .field("viable", &self.viable)
            .field("snapshot", &self.snapshot)
            .finish()
    }
}

/// One executed action inside a job result.
#[derive(Serialize)]
#[serde(bound = "")]
pub struct CampaignActionResult<G: Game + ?Sized> {
    /// The executed action.
    pub action: G::Action,
    /// Per-frame observations across the action.
    pub observations: Vec<G::Observations>,
    /// Milestones after the action.
    pub milestones: G::Milestones,
    /// Whether the action ended dead.
    pub dead: bool,
    /// Whether the action reached the victory mode.
    pub victory: bool,
    /// Whether emulation failed.
    pub failed: bool,
    /// Admission candidate at this boundary, absent for terminal actions.
    pub candidate: Option<CampaignCandidate<G>>,
}

impl<G: Game + ?Sized> PartialEq for CampaignActionResult<G> {
    fn eq(&self, other: &Self) -> bool {
        self.action == other.action
            && self.observations == other.observations
            && self.milestones == other.milestones
            && self.dead == other.dead
            && self.victory == other.victory
            && self.failed == other.failed
            && self.candidate == other.candidate
    }
}
impl<G: Game + ?Sized> Eq for CampaignActionResult<G> {}

impl<G: Game + ?Sized> Clone for CampaignActionResult<G> {
    fn clone(&self) -> Self {
        Self {
            action: self.action,
            observations: self.observations.clone(),
            milestones: self.milestones,
            dead: self.dead,
            victory: self.victory,
            failed: self.failed,
            candidate: self.candidate.clone(),
        }
    }
}

impl<G: Game + ?Sized> Debug for CampaignActionResult<G> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CampaignActionResult")
            .field("action", &self.action)
            .field("observations", &self.observations)
            .field("milestones", &self.milestones)
            .field("dead", &self.dead)
            .field("victory", &self.victory)
            .field("failed", &self.failed)
            .field("candidate", &self.candidate)
            .finish()
    }
}

/// Complete result of one executed job; its serialization is digested into the
/// stream so replay verifies byte-exact re-execution, snapshots included.
#[derive(Serialize)]
#[serde(bound = "")]
pub struct CampaignJobResult<G: Game + ?Sized> {
    /// Executed actions in order.
    pub actions: Vec<CampaignActionResult<G>>,
}

impl<G: Game + ?Sized> PartialEq for CampaignJobResult<G> {
    fn eq(&self, other: &Self) -> bool {
        self.actions == other.actions
    }
}
impl<G: Game + ?Sized> Eq for CampaignJobResult<G> {}

impl<G: Game + ?Sized> Clone for CampaignJobResult<G> {
    fn clone(&self) -> Self {
        Self {
            actions: self.actions.clone(),
        }
    }
}

impl<G: Game + ?Sized> Debug for CampaignJobResult<G> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CampaignJobResult")
            .field("actions", &self.actions)
            .finish()
    }
}

/// Reject a stream whose selector annotations disagree with the selector.
fn verify_selector_annotation(draw: &SelectorDraw) -> Result<(), Box<dyn Error>> {
    match (draw.path, draw.concentration) {
        (SelectorPath::GroupWalk, None) => {
            Err("cell draw is missing its concentration record".into())
        }
        (SelectorPath::Uniform, Some(_)) => {
            Err("uniform draw carries a concentration record".into())
        }
        _ => Ok(()),
    }
}

/// Derive one worker's stream seed from the campaign seed and worker index.
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

/// Serial archive-and-accumulator state shared by the live coordinator loop and
/// replay. Admission through this struct is the single admission lock: every
/// archive mutation happens here, in stream order, so the archive state at any
/// stream position is identical in the live run and in replay.
pub(crate) struct CoordinatorCore<G: Game + ?Sized> {
    pub(crate) archive: Archive<G::Action, G::Key, G::Milestones, G::Snapshot>,
    pub(crate) evidence: G::Evidence,
    curve: Vec<ProgressPoint<G::Milestones, G::Progress>>,
    record_progress: bool,
    deaths: u64,
    pub(crate) victories: u64,
    pub(crate) victory_input: Option<Input<G::Action>>,
    sequence: u64,
    probe_refused: u64,
    max_actions: usize,
    pub(crate) mixture_energy: MixtureEnergy,
}

impl<G: Game + ?Sized> CoordinatorCore<G> {
    pub(crate) fn new(game: &G, max_actions: usize, archive_entry_limit: usize) -> Self {
        let mut archive = Archive::new(game.action_time_fn());
        archive.max_entries = archive_entry_limit;
        Self {
            archive,
            evidence: G::Evidence::default(),
            curve: Vec::new(),
            record_progress: true,
            deaths: 0,
            victories: 0,
            victory_input: None,
            sequence: 0,
            probe_refused: 0,
            max_actions,
            mixture_energy: MixtureEnergy::default(),
        }
    }

    /// Retain genesis at execution zero.
    pub(crate) fn bootstrap(
        &mut self,
        game: &G,
        target: &mut G::Target,
    ) -> Result<(), Box<dyn Error>> {
        game.reset(target);
        let genesis_key = game.current_key(target)?;
        let genesis_snapshot = game.snapshot(target)?;
        self.archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    input: Input::default(),
                    key: genesis_key,
                    milestones: G::Milestones::default(),
                },
                genesis_snapshot,
            )?
            .ok_or("failed to retain campaign genesis")?;
        Ok(())
    }

    /// Retain one evaluator-supplied snapshot as archive id zero with an empty
    /// challenge-local input.
    pub(crate) fn bootstrap_snapshot_root(
        &mut self,
        game: &G,
        run: &G::Run,
        target: &mut G::Target,
        checkpoint: &CampaignCheckpoint<G::Snapshot>,
    ) -> Result<(), Box<dyn Error>> {
        let snapshot = validate_snapshot_root_checkpoint(game, checkpoint)?;
        game.restore(target, snapshot)?;
        if game.is_run_terminal(run, target)? {
            return Err("snapshot-root checkpoint restores a terminal target".into());
        }
        game.merge_snapshot_root_evidence(&mut self.evidence, target)?;
        let key = game.current_key(target)?;
        let retained = self.archive.insert(
            None,
            0,
            ArchiveCandidate {
                input: Input::default(),
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

    /// Import a source archive's whole retained tree at execution zero, after
    /// retaining genesis.
    ///
    /// Entries are walked in source order. Each one restores its imported
    /// parent's snapshot, applies the actions past the parent's input, and is
    /// inserted under this run's policies, so liveness, cells, lineages, and
    /// replacement decisions are re-derived rather than copied. The admission
    /// probe is not repeated: the source already admitted every entry, and
    /// probing tens of thousands of imports would cost more frames than the
    /// search they seed. An entry whose parent was not imported is re-rooted
    /// at its nearest imported ancestor; an entry that dies or exceeds the
    /// action limit is skipped and counted.
    pub(crate) fn import_tree(
        &mut self,
        game: &G,
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
        // The source evidence already covers every action interior the source
        // run observed, so both import paths merge it whole.
        game.merge_origin_evidence(&mut self.evidence, source);
        self.bootstrap(game, target)?;
        let genesis_id = 0;
        let mut counts = TreeImportCounts::default();
        let mut index_of: BTreeMap<u64, usize> = BTreeMap::new();
        let source_entries = game.source_entries(source);
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
            // Nearest imported ancestor; the walk stays within earlier source
            // entries, which are the only ones that can be imported already.
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
            let mut milestones = parent_entry.report.milestones;
            let prefix = entry.input.clone();
            let snapshot = if let Some(snapshot) = checkpointed.get(&entry.id) {
                // The source recorded the strongest milestones along this
                // input, which is what replaying its actions would merge.
                game.restore(target, snapshot)?;
                milestones = merge_max(game, milestones, entry.milestones);
                counts.checkpointed = counts.checkpointed.saturating_add(1);
                if game.is_terminal(target) {
                    None
                } else {
                    Some((*snapshot).clone())
                }
            } else {
                game.restore(target, &parent_entry.snapshot)?;
                let mut terminal = false;
                for action in &entry.input.actions[parent_input_len..] {
                    game.apply_action(target, action, &mut milestones)?;
                    if game.is_terminal(target) {
                        terminal = true;
                        break;
                    }
                }
                if terminal {
                    None
                } else {
                    Some(game.snapshot(target)?)
                }
            };
            let Some(snapshot) = snapshot else {
                counts.terminal = counts.terminal.saturating_add(1);
                imported.push(None);
                continue;
            };
            game.merge_import_evidence(&mut self.evidence, milestones, &prefix);
            let key = game.current_key(target)?;
            let inserted_before = self.archive.entries.len();
            match self.archive.insert(
                Some(parent_id),
                0,
                ArchiveCandidate {
                    input: prefix,
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
        Ok(counts)
    }

    /// Admit one executed job at the next sequence position, merging its
    /// per-action evidence in order and applying the retention rules
    /// through the campaign's sole archive implementation.
    pub(crate) fn admit_job(
        &mut self,
        game: &G,
        parent_id: u64,
        result: CampaignJobResult<G>,
    ) -> Result<(u64, Vec<CampaignAdmissionDecision>), Box<dyn Error>> {
        self.sequence = self.sequence.saturating_add(1);
        let sequence = self.sequence;
        let parent_index = usize::try_from(parent_id)?;
        // Entries are append-only and immutable once inserted, so the parent
        // read here is identical to the parent the worker saw at selection.
        let mut input = self
            .archive
            .entries
            .get(parent_index)
            .ok_or("campaign job parent is missing from the archive")?
            .report
            .input
            .clone();
        let mut current_parent = parent_index;
        let mut decisions = Vec::new();
        for action in result.actions {
            input.actions.push(action.action);
            game.merge_action_evidence(&mut self.evidence, &action, sequence, &input);
            if action.dead {
                self.deaths = self.deaths.saturating_add(1);
            }
            if action.victory {
                self.victories = self.victories.saturating_add(1);
                self.victory_input.get_or_insert_with(|| input.clone());
                decisions.push(CampaignAdmissionDecision::Victory);
            }
            if let Some(candidate) = action.candidate {
                if !candidate.viable {
                    self.probe_refused = self.probe_refused.saturating_add(1);
                    decisions.push(CampaignAdmissionDecision::ProbeRefused);
                    continue;
                }
                let inserted_before = self.archive.entries.len();
                match self.archive.insert(
                    Some(current_parent),
                    sequence,
                    ArchiveCandidate {
                        input: input.clone(),
                        key: game.complete_candidate_key(candidate.key, &candidate.snapshot)?,
                        milestones: action.milestones,
                    },
                    candidate.snapshot,
                )? {
                    Some(id) if id == inserted_before => {
                        decisions.push(CampaignAdmissionDecision::Retained {
                            id: u64::try_from(id)?,
                        });
                        current_parent = id;
                    }
                    Some(id) => {
                        decisions.push(CampaignAdmissionDecision::Duplicate {
                            id: u64::try_from(id)?,
                        });
                        current_parent = id;
                    }
                    None => decisions.push(CampaignAdmissionDecision::Rejected),
                }
            }
        }
        if sequence.is_multiple_of(CURVE_INTERVAL) {
            self.push_curve_point();
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
            active_entries: self.archive.active.iter().filter(|active| **active).count(),
            occupied_cells: self.archive.slots.len(),
            deaths: self.deaths,
        });
    }

    /// The strongest milestone summary so far; delegated through the game via
    /// the evidence, see [`Game::aggregate_milestones`].
    fn aggregate_milestones(&self) -> G::Milestones {
        G::aggregate_milestones(&self.evidence)
    }

    /// Push the final curve point at the campaign's
    /// last execution, without duplicating an interval point.
    fn finish_curve(&mut self) {
        if self.sequence > 0 && !self.sequence.is_multiple_of(CURVE_INTERVAL) {
            self.push_curve_point();
        }
    }

    /// Report whether every executable boundary of this drawn job is already
    /// archived, in which case executing it cannot change the archive, any
    /// maximum, or the death count.
    fn all_prefixes_archived(&self, parent_index: usize, suffix: &[G::Action]) -> bool {
        let parent = &self.archive.entries[parent_index].report.input;
        let executable = suffix
            .len()
            .min(self.max_actions.saturating_sub(parent.actions.len()));
        if executable == 0 {
            return false;
        }
        let mut input = parent.clone();
        for action in &suffix[..executable] {
            input.actions.push(*action);
            if !self.archive.input_ids.contains_key(&input) {
                return false;
            }
        }
        true
    }

    pub(crate) fn into_archive_report_and_snapshots(
        mut self,
        game: &G,
        campaign_seed: u64,
    ) -> (G::ArchiveReport, Vec<(u64, G::Snapshot)>) {
        let (entries, snapshots) = self.archive.take_entry_reports_and_snapshots();
        let report = game.archive_report(
            &self.evidence,
            ArchiveReportState {
                seed: campaign_seed,
                executions: self.sequence,
                entries,
                progress_curve: self.curve,
                retained: self.archive.retained,
                rejected: self.archive.rejected,
                deaths: self.deaths,
                selector: self.archive.selector_report(),
            },
        );
        (report, snapshots)
    }
}

/// Merge two milestone summaries through the game's evidence rules.
fn merge_max<G: Game + ?Sized>(
    game: &G,
    mut base: G::Milestones,
    other: G::Milestones,
) -> G::Milestones {
    game.merge_milestones(&mut base, other);
    base
}

/// The origin record for the stream header; its resume input is the source
/// archive's frontier identity.
fn resolve_origin<G: Game>(
    game: &G,
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
            game.resume_input(report)?,
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

fn stream_header<G: Game>(
    game: &G,
    config: &CampaignConfig<G>,
    origin: &CampaignOriginRecord,
    draw_table: Option<G::TableHeader>,
) -> CampaignStreamHeader<G::TableHeader> {
    CampaignStreamHeader {
        format: game.stream_format().to_owned(),
        campaign_seed: config.campaign_seed,
        workers: config.workers,
        schedule_policy: Some(CAMPAIGN_SCHEDULE_POLICY.to_owned()),
        progress_policy: Some(CAMPAIGN_PROGRESS_POLICY.to_owned()),
        host: config.host.clone(),
        origin_kind: origin.kind.clone(),
        origin_path: origin.path.clone(),
        origin_archive_sha256: origin.archive_sha256.clone(),
        origin_checkpoint_path: origin.checkpoint_path.clone(),
        origin_checkpoint_sha256: origin.checkpoint_sha256.clone(),
        resume_input_sha256: origin.resume_input_sha256.clone(),
        resume_actions: origin.resume_actions,
        execution_budget: config.execution_budget,
        wall_budget_seconds: config.wall_budget.map(|budget| budget.as_secs()),
        action_limit: config.action_limit,
        archive_entry_limit: config.archive_entry_limit,
        resume_policy: if origin.kind == ORIGIN_SNAPSHOT_ROOT {
            SNAPSHOT_ROOT_RESUME_IDENTIFIER.to_owned()
        } else {
            RESUME_IDENTIFIER.to_owned()
        },
        suffix_policy: suffix_shape_identifier(config.suffix).to_owned(),
        mixture_policy: draw_mixture_identifier(config.mixture),
        game_policies: game.policies(&config.run),
        draw_table,
        retention_policy: retention_policy_identifier(config.retention).to_owned(),
        parent_scheduler: selector_policy_identifier(&config.selector),
        executor_mode: "snapshot_resume_archive".to_owned(),
        worker_seed_derivation: "sha256(campaign_seed_le || worker_index_le)[0..8] as u64 le"
            .to_owned(),
        rom_sha256: game.image_sha256(),
    }
}

/// Line-oriented stream writer that hashes exactly the bytes it writes.
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

/// Everything the coordinator counts alongside the core, derived from the
/// stream on replay.
struct CampaignCounters {
    bootstrap_frames: u64,
    tree_import: Option<TreeImportCounts>,
    job_frames: u64,
    duplicates_skipped: u64,
    jobs_per_worker: Vec<u64>,
    skips_per_worker: Vec<u64>,
}

impl CampaignCounters {
    fn new(workers: u32) -> Self {
        Self {
            bootstrap_frames: 0,
            tree_import: None,
            job_frames: 0,
            duplicates_skipped: 0,
            jobs_per_worker: vec![0; workers as usize],
            skips_per_worker: vec![0; workers as usize],
        }
    }
}

fn build_report<G: Game>(
    game: &G,
    header: &CampaignStreamHeader<G::TableHeader>,
    origin: CampaignOriginRecord,
    core: CoordinatorCore<G>,
    counters: &CampaignCounters,
    stream_sha256: String,
) -> CampaignOutcome<G> {
    let executions_completed = core.sequence;
    let probe_refused = core.probe_refused;
    let victories = core.victories;
    let victory_input = core.victory_input.clone();
    let replacement_frames_displaced = core.archive.replacement_time_displaced();
    let (archive, snapshots) = core.into_archive_report_and_snapshots(game, header.campaign_seed);
    let checkpoint = SnapshotCheckpoint {
        format: game.checkpoint_format().to_owned(),
        entries: snapshots
            .into_iter()
            .map(|(id, snapshot)| SnapshotCheckpointEntry { id, snapshot })
            .collect(),
    };
    let schedule_identity = match header.schedule_policy.as_deref() {
        Some(CAMPAIGN_SCHEDULE_POLICY) => CAMPAIGN_SCHEDULE_IDENTITY,
        _ => LEGACY_CAMPAIGN_SCHEDULE_IDENTITY,
    };
    let report = CampaignModeReport {
        mode: "campaign".to_owned(),
        campaign_seed: header.campaign_seed,
        workers: header.workers,
        host: header.host.clone(),
        schedule_identity: schedule_identity.to_owned(),
        origin,
        execution_budget: header.execution_budget,
        executions_completed,
        wall_budget_seconds: header.wall_budget_seconds,
        action_limit: header.action_limit,
        archive_entry_limit: header.archive_entry_limit,
        resume_policy: header.resume_policy.clone(),
        suffix_policy: header.suffix_policy.clone(),
        mixture_policy: header.mixture_policy.clone(),
        game_policies: header.game_policies.clone(),
        retention_policy: header.retention_policy.clone(),
        parent_scheduler: header.parent_scheduler.clone(),
        executor_mode: header.executor_mode.clone(),
        worker_seed_derivation: header.worker_seed_derivation.clone(),
        rom_sha256: header.rom_sha256.clone(),
        bootstrap_frames: counters.bootstrap_frames,
        tree_import: counters.tree_import,
        frames_emulated: counters
            .bootstrap_frames
            .saturating_add(counters.job_frames),
        duplicates_skipped: counters.duplicates_skipped,
        probe_refused,
        victories,
        victory_input,
        replacement_frames_displaced,
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

/// Incrementally digest the pinned postcard representation without building
/// a full encoded result buffer.
pub(crate) fn postcard_result_sha256<G: Game + ?Sized>(
    result: &CampaignJobResult<G>,
) -> Result<String, Box<dyn Error>> {
    postcard_sha256(result)
}

fn postcard_sha256<T: Serialize + ?Sized>(value: &T) -> Result<String, Box<dyn Error>> {
    let digest = postcard::serialize_with_flavor::<_, PostcardSha256, sha2::digest::Output<Sha256>>(
        value,
        PostcardSha256(Sha256::new()),
    )?;
    Ok(format!("{digest:x}"))
}

/// Job specification sent to a worker.
struct JobSpec<G: Game + ?Sized> {
    reservation: usize,
    snapshot: Arc<G::Snapshot>,
    parent_actions: usize,
    parent_milestones: G::Milestones,
    suffix: Vec<G::Action>,
}

/// A selected job's worker specification and its coordinator-side record.
type SelectedJob<G> = (JobSpec<G>, PendingJob);

/// What the coordinator remembers about a worker's in-flight job.
struct PendingJob {
    worker: u32,
    parent_id: u64,
    mutation_seed: u64,
    mixture_weight: u8,
    splice_weight: u8,
    splice: Option<CampaignSpliceRecord>,
    selector: SelectorDraw,
    draw_table_before: Option<EmpiricalStepCheckpoint>,
}

/// One finished window slot, held until every earlier reservation can be
/// admitted in deterministic order.
struct CompletedJob<G: Game + ?Sized> {
    pending: PendingJob,
    result: CampaignJobResult<G>,
    frames: u64,
    result_sha256: String,
}

fn replay_splice<G: Game>(
    core: &mut CoordinatorCore<G>,
    parent: usize,
    max_actions: usize,
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
        Some(CampaignSpliceRecord::Tail { donor_id, leaf_id }) => {
            Ok(Some(core.archive.recorded_splice_tail(
                parent,
                usize::try_from(donor_id)?,
                usize::try_from(leaf_id)?,
                SPLICE_ACTION_CAP,
            )?))
        }
        None => Ok(core
            .archive
            .splice_tail_for_campaign(parent, max_actions, SPLICE_ACTION_CAP)
            .map(|splice| splice.actions)),
    }
}

/// One periodic observation of a live run.
///
/// Written to a sidecar file so an operator can see a run advance without
/// waiting for its sentinel. It is not part of the recorded stream and takes no
/// part in replay.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "K: Serialize + DeserializeOwned")]
pub struct CampaignProgressRecord<K> {
    /// Seconds since the Unix epoch when the line was written.
    pub unix_time: u64,
    /// Executions admitted so far.
    pub executions: u64,
    /// Emulator frames completed so far, including bootstrap work.
    #[serde(default)]
    pub frames_emulated: u64,
    /// Deepest retained key so far, absent while the archive is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deepest_key: Option<K>,
    /// Fewest time units any entry at that key spent inside its coarsest
    /// group.
    pub cheapest_time_in_group: u64,
    /// Entries retained so far.
    pub retained: u64,
}

/// Seed the coordinator from the origin: genesis alone, or genesis plus the
/// whole source tree.
fn bootstrap_core<G: Game>(
    game: &G,
    run: &G::Run,
    core: &mut CoordinatorCore<G>,
    target: &mut G::Target,
    origin: &CampaignOrigin<G>,
) -> Result<Option<TreeImportCounts>, Box<dyn Error>> {
    match origin {
        CampaignOrigin::Archive {
            report, checkpoint, ..
        } => Ok(Some(core.import_tree(
            game,
            target,
            report,
            checkpoint.as_ref().map(|checkpoint| &checkpoint.snapshots),
        )?)),
        CampaignOrigin::Genesis => {
            core.bootstrap(game, target)?;
            Ok(None)
        }
        CampaignOrigin::SnapshotRoot { checkpoint } => {
            core.bootstrap_snapshot_root(game, run, target, checkpoint)?;
            Ok(None)
        }
    }
}

fn validate_snapshot_root_checkpoint<'a, G: Game + ?Sized>(
    game: &G,
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
    if checkpoint.snapshots.format != game.checkpoint_format() {
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

/// Run a campaign, also returning every retained entry's snapshot so a later
/// whole-tree resume can restore the population instead of re-emulating it.
///
/// The coordinator thread owns the archive, the accumulators, the per-worker
/// RNG streams, and the stream writer; selection and admission both happen on
/// it, serially, which realizes the single admission lock. Workers only
/// execute jobs. The interleaving of results is the run's only nondeterminism.
///
/// # Errors
///
/// Returns an error when the origin is unusable, a worker fails, emulation or
/// snapshotting fails, or the stream cannot be written.
#[allow(clippy::too_many_lines)]
pub fn run_campaign_checkpointed<G: Game>(
    game: &G,
    config: &CampaignConfig<G>,
    origin: &CampaignOrigin<G>,
    stream: &mut dyn Write,
    mut progress: Option<&mut dyn Write>,
) -> Result<CampaignOutcome<G>, Box<dyn Error>>
where
    G::ArchiveReport: Serialize,
{
    if config.workers == 0 {
        return Err("campaign mode requires at least one worker".into());
    }
    if config.action_limit == 0 || config.action_limit > game.max_action_limit() {
        return Err("campaign action limit is outside its bounded range".into());
    }
    if config.archive_entry_limit == 0
        || config.archive_entry_limit > crate::search::archive::MAX_ARCHIVE_ENTRIES
    {
        return Err("campaign archive entry limit is outside its bounded range".into());
    }
    let origin_record = resolve_origin(game, origin)?;
    let draw_origin = match origin {
        CampaignOrigin::Genesis => None,
        CampaignOrigin::SnapshotRoot { .. } => None,
        CampaignOrigin::Archive {
            file_sha256,
            report,
            ..
        } => Some((file_sha256.as_str(), report.as_ref())),
    };
    let (mut draw_state, draw_table_header) = game.initial_draw_state(&config.run, draw_origin)?;
    let header = stream_header(game, config, &origin_record, draw_table_header);
    let mut writer = StreamWriter::new(stream);
    writer.write_line(&header)?;

    let mut core = CoordinatorCore::new(game, config.action_limit, config.archive_entry_limit);
    core.archive.selector_policy = config.selector.clone();
    let mut counters = CampaignCounters::new(config.workers);
    let mut bootstrap_target = game.new_target().map_err(|error| -> Box<dyn Error> {
        format!("failed to build the bootstrap target: {error}").into()
    })?;
    let frames_before = game.frames_clocked(&bootstrap_target);
    counters.tree_import =
        bootstrap_core(game, &config.run, &mut core, &mut bootstrap_target, origin)?;
    counters.bootstrap_frames = game
        .frames_clocked(&bootstrap_target)
        .saturating_sub(frames_before);
    drop(bootstrap_target);

    let workers = config.workers as usize;
    let mut rands = Vec::with_capacity(workers);
    for index in 0..config.workers {
        rands.push(RomuDuoJrRand::with_seed(derive_worker_seed(
            config.campaign_seed,
            index,
        )?));
    }

    // The wall cutoff is live-schedule input only: it stops issuing new
    // reservations and never enters campaign state.
    #[allow(clippy::disallowed_methods)] // not order-observable: reservation cutoff only.
    let started = config.wall_budget.map(|_| std::time::Instant::now());

    // Sidecar cadence. Held outside the worker scope so one clock covers the run.
    #[allow(clippy::disallowed_methods)]
    let progress_started = std::time::Instant::now();
    let mut next_progress = 0_u64;

    let mut reserved = 0_u64;

    let max_actions = config.action_limit;
    let retention = config.retention;
    with_worker_pool(
        config.workers,
        |_| game.new_target(),
        |target, spec: JobSpec<G>| {
            let reservation = spec.reservation;
            let frames_before = game.frames_clocked(target);
            game.execute_job(
                &config.run,
                target,
                &spec.snapshot,
                spec.parent_actions,
                spec.parent_milestones,
                &spec.suffix,
                max_actions,
                retention,
            )
            .and_then(|result| {
                let result_sha256 = game
                    .result_sha256(&result)
                    .map_err(|error| error.to_string())?;
                Ok((
                    reservation,
                    result,
                    game.frames_clocked(target).saturating_sub(frames_before),
                    result_sha256,
                ))
            })
            .map_err(|error| error.to_string())
        },
        |pool| -> Result<(), Box<dyn Error>> {
            // Select one job for one worker, recording skips, or report exhaustion.
            let select = |core: &mut CoordinatorCore<G>,
                          rands: &mut [RomuDuoJrRand],
                          draw_state: &mut G::DrawState,
                          writer: &mut StreamWriter<'_>,
                          counters: &mut CampaignCounters,
                          reserved: &mut u64,
                          worker: u32|
             -> Result<Option<SelectedJob<G>>, Box<dyn Error>> {
                if *reserved >= config.execution_budget || core.victory_input.is_some() {
                    return Ok(None);
                }
                if let (Some(started), Some(wall_budget)) = (started, config.wall_budget)
                    && started.elapsed() >= wall_budget
                {
                    return Ok(None);
                }
                let rand = &mut rands[worker as usize];
                let max_actions = core.max_actions;
                let mut consecutive_skips = 0_u64;
                loop {
                    let (parent_index, selector) = core.archive.select_parent(rand, max_actions)?;
                    let mutation_seed = rand.next_u64();
                    let (mixture_weight, splice_weight) = match config.mixture {
                        DrawMixture::Energy { scale } => {
                            (core.mixture_energy.biased_weight(scale), 0)
                        }
                        DrawMixture::EnergySplice { scale } => {
                            core.mixture_energy.splice_weights(scale)
                        }
                        _ => (default_mixture_weight(), 0),
                    };
                    let draw_table_before = game.draw_checkpoint(draw_state)?;
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
                                }) => (
                                    Some(actions),
                                    Some(CampaignSpliceRecord::Tail {
                                        donor_id: u64::try_from(donor_id)?,
                                        leaf_id: u64::try_from(leaf_id)?,
                                    }),
                                ),
                                None => (None, Some(CampaignSpliceRecord::Unavailable)),
                            }
                        } else {
                            (None, None)
                        };
                    let suffix = match spliced {
                        Some(tail) => tail,
                        None => game.expand_suffix(
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
                    let all_prefixes_archived = consecutive_skips < CONSECUTIVE_SKIP_LIMIT
                        && core.all_prefixes_archived(parent_index, &suffix);
                    if all_prefixes_archived {
                        let draw_table_after =
                            game.finish_stream_record(&config.run, draw_state, &[])?;
                        writer.write_line(&CampaignStreamRecord::Skip(CampaignSkipRecord {
                            worker,
                            parent_id: u64::try_from(parent_index)?,
                            mutation_seed,
                            mixture_weight,
                            splice_weight,
                            splice,
                            selector,
                            draw_table_before,
                            draw_table_after,
                        }))?;
                        core.archive.record_selection(parent_index, &selector);
                        counters.duplicates_skipped = counters.duplicates_skipped.saturating_add(1);
                        counters.skips_per_worker[worker as usize] =
                            counters.skips_per_worker[worker as usize].saturating_add(1);
                        consecutive_skips = consecutive_skips.saturating_add(1);
                        continue;
                    }
                    *reserved = reserved.saturating_add(1);
                    let entry = &core.archive.entries[parent_index];
                    return Ok(Some((
                        JobSpec {
                            reservation: 0,
                            snapshot: entry.snapshot.clone(),
                            parent_actions: entry.report.input.actions.len(),
                            parent_milestones: entry.report.milestones,
                            suffix,
                        },
                        PendingJob {
                            worker,
                            parent_id: u64::try_from(parent_index)?,
                            mutation_seed,
                            mixture_weight,
                            splice_weight,
                            splice,
                            selector,
                            draw_table_before,
                        },
                    )));
                }
            };

            let window_capacity = workers.saturating_mul(WINDOW_JOBS_PER_WORKER);
            let mut pending = BTreeMap::<usize, PendingJob>::new();
            let mut completed = BTreeMap::<usize, CompletedJob<G>>::new();
            let mut queued_specs = VecDeque::with_capacity(window_capacity);
            for _ in 0..window_capacity {
                let worker_index = usize::try_from(reserved % u64::from(config.workers))?;
                let worker = u32::try_from(worker_index)?;
                let selected = select(
                    &mut core,
                    &mut rands,
                    &mut draw_state,
                    &mut writer,
                    &mut counters,
                    &mut reserved,
                    worker,
                )?;
                let Some((mut spec, pending_job)) = selected else {
                    break;
                };
                let reservation = usize::try_from(reserved.saturating_sub(1))?;
                spec.reservation = reservation;
                if pending.insert(reservation, pending_job).is_some() {
                    return Err("campaign reserved one job twice".into());
                }
                queued_specs.push_back(spec);
            }
            let mut physical_queued = vec![0_usize; workers];
            'prefill: for _ in 0..EXECUTOR_PREFETCH_JOBS {
                for worker in 0..config.workers {
                    let Some(spec) = queued_specs.pop_front() else {
                        break 'prefill;
                    };
                    pool.send(worker, spec)?;
                    physical_queued[usize::try_from(worker)?] =
                        physical_queued[usize::try_from(worker)?].saturating_add(1);
                }
            }
            let mut idle_workers = physical_queued
                .iter()
                .enumerate()
                .filter_map(|(worker, queued)| (*queued == 0).then_some(worker))
                .map(u32::try_from)
                .collect::<Result<VecDeque<_>, _>>()?;

            let mut next_admission = 0_usize;
            while !pending.is_empty() {
                let reply = pool.receive()?;
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
                let (reservation, result, frames, result_sha256) = outcome;
                let pending_job = pending
                    .remove(&reservation)
                    .ok_or("campaign worker replied for an unknown reservation")?;
                if completed
                    .insert(
                        reservation,
                        CompletedJob {
                            pending: pending_job,
                            result,
                            frames,
                            result_sha256,
                        },
                    )
                    .is_some()
                {
                    return Err("campaign worker completed one reservation twice".into());
                }

                // Keep the physical executor busy while the coordinator
                // admits the deterministic contiguous result prefix. The
                // replacement job was already selected and reserved, so this
                // overlap cannot change campaign state or recorded order.
                if let Some(spec) = queued_specs.pop_front() {
                    pool.send(physical_worker, spec)?;
                    physical_queued[physical_index] =
                        physical_queued[physical_index].saturating_add(1);
                } else if physical_queued[physical_index] == 0 {
                    idle_workers.push_back(physical_worker);
                }

                while let Some(completed_job) = completed.remove(&next_admission) {
                    let pending_job = completed_job.pending;
                    let worker_index = usize::try_from(pending_job.worker)?;
                    let result = completed_job.result;
                    let frames = completed_job.frames;
                    let result_sha256 = completed_job.result_sha256;
                    let victories_before = core.victories;
                    let (sequence, decisions) =
                        core.admit_job(game, pending_job.parent_id, result)?;
                    let parent_index = usize::try_from(pending_job.parent_id)?;
                    core.archive
                        .record_selection(parent_index, &pending_job.selector);
                    let retained_ids = decisions
                        .iter()
                        .filter_map(|decision| match decision {
                            CampaignAdmissionDecision::Retained { id } => usize::try_from(*id).ok(),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    let new_slot_descendant = retained_ids
                        .iter()
                        .any(|id| core.archive.opened_new_slot(*id));
                    let new_cell_descendant = retained_ids
                        .iter()
                        .any(|id| core.archive.opened_new_cell(*id));
                    core.archive.record_selection_outcome(
                        parent_index,
                        !retained_ids.is_empty(),
                        new_slot_descendant,
                        new_cell_descendant,
                    );
                    if let DrawMixture::Energy { .. } | DrawMixture::EnergySplice { .. } =
                        config.mixture
                    {
                        let strategy = energy_strategy(
                            pending_job.mutation_seed,
                            pending_job.mixture_weight,
                            pending_job.splice_weight,
                        )?;
                        core.mixture_energy
                            .record_outcome(strategy, new_slot_descendant);
                    }
                    if victories_before == 0
                        && let (Some(path), Some(input)) =
                            (&config.victory_input_path, &core.victory_input)
                    {
                        std::fs::write(path, serde_json::to_vec_pretty(input)?)?;
                    }
                    let draw_table_after =
                        finish_record(game, &config.run, &mut draw_state, &core, &decisions)?;
                    writer.write_line(&CampaignStreamRecord::Job(CampaignJobRecord {
                        sequence,
                        worker: pending_job.worker,
                        parent_id: pending_job.parent_id,
                        mutation_seed: pending_job.mutation_seed,
                        frames,
                        result_sha256,
                        decisions,
                        mixture_weight: pending_job.mixture_weight,
                        splice_weight: pending_job.splice_weight,
                        splice: pending_job.splice,
                        selector: pending_job.selector,
                        draw_table_before: pending_job.draw_table_before,
                        draw_table_after,
                    }))?;
                    counters.jobs_per_worker[worker_index] =
                        counters.jobs_per_worker[worker_index].saturating_add(1);
                    counters.job_frames = counters.job_frames.saturating_add(frames);
                    if let Some(sink) = progress.as_deref_mut() {
                        // Wall-clock controls the sidecar only. It selects nothing and
                        // enters no recorded artifact, so its nondeterminism cannot
                        // reach the stream.
                        #[allow(clippy::disallowed_methods)]
                        let elapsed = progress_started.elapsed().as_secs();
                        if elapsed >= next_progress {
                            next_progress = elapsed
                                .saturating_add(PROGRESS_INTERVAL_SECONDS)
                                .saturating_sub(elapsed % PROGRESS_INTERVAL_SECONDS);
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
                                unix_time,
                                executions: sequence,
                                frames_emulated: counters
                                    .bootstrap_frames
                                    .saturating_add(counters.job_frames),
                                deepest_key,
                                cheapest_time_in_group: cheapest,
                                retained,
                            })?;
                            sink.write_all(line.as_bytes())?;
                            sink.write_all(b"\n")?;
                            sink.flush()?;
                        }
                    }
                    next_admission = next_admission.saturating_add(1);
                    let worker_index = usize::try_from(reserved % u64::from(config.workers))?;
                    let worker = u32::try_from(worker_index)?;
                    let selected = select(
                        &mut core,
                        &mut rands,
                        &mut draw_state,
                        &mut writer,
                        &mut counters,
                        &mut reserved,
                        worker,
                    )?;
                    if let Some((mut spec, pending_job)) = selected {
                        let reservation = usize::try_from(reserved.saturating_sub(1))?;
                        spec.reservation = reservation;
                        if pending.insert(reservation, pending_job).is_some() {
                            return Err("campaign reserved one job twice".into());
                        }
                        queued_specs.push_back(spec);
                    }
                }
                while !queued_specs.is_empty() {
                    let Some(worker) = idle_workers.pop_front() else {
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

    core.finish_curve();
    let stream_sha256 = writer.finish()?;
    Ok(build_report(
        game,
        &header,
        origin_record,
        core,
        &counters,
        stream_sha256,
    ))
}

/// Resolve the record's retained inputs and close the draw-state record.
fn finish_record<G: Game>(
    game: &G,
    run: &G::Run,
    draw_state: &mut G::DrawState,
    core: &CoordinatorCore<G>,
    decisions: &[CampaignAdmissionDecision],
) -> Result<Option<EmpiricalStepCheckpoint>, Box<dyn Error>> {
    let mut retained_inputs = Vec::new();
    for decision in decisions {
        let CampaignAdmissionDecision::Retained { id } = decision else {
            continue;
        };
        let index = usize::try_from(*id)?;
        let entry = core
            .archive
            .entries
            .get(index)
            .ok_or("retained draw-table entry is missing from the run archive")?;
        let parent_actions = entry
            .report
            .parent_id
            .and_then(|parent| usize::try_from(parent).ok())
            .and_then(|parent| core.archive.entries.get(parent))
            .map_or(0, |parent| parent.report.input.actions.len());
        retained_inputs.push((parent_actions, entry.report.input.actions.as_slice()));
    }
    game.finish_stream_record(run, draw_state, &retained_inputs)
}

/// Replay a recorded campaign stream serially and rebuild its report.
///
/// Replay re-executes every recorded job from (parent id, mutation seed) on a
/// single target, verifies each result digest and frame count byte for byte,
/// re-applies the retention rules, and verifies every recomputed
/// admission decision against the recorded one. Any mismatch is an error.
///
/// When the recorded header names a snapshot checkpoint, `origin_checkpoint`
/// must be that file and its hash must match; a replay without it re-emulates
/// the import and must still reach the same archive.
///
/// # Errors
///
/// Returns an error when the stream is malformed, the origin does not match
/// the header, or any recomputed value differs from the recorded one.
#[allow(clippy::too_many_lines)]
pub fn replay_campaign_checkpointed<G: Game>(
    game: &G,
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
    let header: CampaignStreamHeader<G::TableHeader> =
        serde_json::from_str(lines.next().ok_or("campaign stream is empty")?)?;
    let record_lines = lines.collect::<Vec<_>>();
    let mut required_draw_versions = BTreeSet::new();
    for line in &record_lines {
        let record: CampaignStreamRecord = serde_json::from_str(line)?;
        let before = match record {
            CampaignStreamRecord::Job(job) => job.draw_table_before,
            CampaignStreamRecord::Skip(skip) => skip.draw_table_before,
        };
        if let Some(before) = before {
            required_draw_versions.insert(before.records);
        }
    }
    if header.format != game.stream_format() {
        return Err("campaign stream format is not recognized".into());
    }
    if header
        .schedule_policy
        .as_deref()
        .is_some_and(|policy| policy != CAMPAIGN_SCHEDULE_POLICY)
    {
        return Err("campaign stream schedule policy is not recognized".into());
    }
    if header
        .progress_policy
        .as_deref()
        .is_some_and(|policy| policy != CAMPAIGN_PROGRESS_POLICY)
    {
        return Err("campaign stream progress policy is not recognized".into());
    }
    if header.rom_sha256 != game.image_sha256() {
        return Err("campaign replay ROM does not match the recorded stream".into());
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
            game.resume_input(source)?
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
    let replay_run = game.resolve_recorded(&header.game_policies)?;
    if let Some(checkpoint) = origin_checkpoint {
        let sha256_matches =
            header.origin_checkpoint_sha256.as_deref() == Some(checkpoint.file_sha256.as_str());
        let logical_path_matches = header.origin_kind != ORIGIN_SNAPSHOT_ROOT
            || header.origin_checkpoint_path.as_deref() == Some(checkpoint.path.as_str());
        if !sha256_matches || !logical_path_matches {
            return Err("campaign replay checkpoint does not match the recorded stream".into());
        }
    }
    // Replay already borrows the decoded origin from its caller. Do not clone
    // a mature archive and its multi-gigabyte checkpoint merely to satisfy the
    // live-run ownership shape: the clone survives the whole replay and can
    // double peak memory without changing any campaign state.
    let draw_origin = match header.origin_kind.as_str() {
        ORIGIN_ARCHIVE => Some((
            header.origin_archive_sha256.as_deref().unwrap_or_default(),
            origin_report.ok_or("archive campaign replay requires the source archive")?,
        )),
        ORIGIN_GENESIS | ORIGIN_SNAPSHOT_ROOT => None,
        _ => return Err("campaign stream origin kind is not recognized".into()),
    };
    let (mut draw_state, replay_draw_header) = game.initial_draw_state(&replay_run, draw_origin)?;
    if replay_draw_header != header.draw_table {
        return Err("re-derived draw table does not match the recorded header".into());
    }
    game.remember_draw_version(&mut draw_state, &required_draw_versions)?;
    let mut core = CoordinatorCore::new(game, header.action_limit, header.archive_entry_limit);
    core.record_progress = header.progress_policy.is_some();
    core.archive.selector_policy = replay_selector.clone();
    let mut counters = CampaignCounters::new(header.workers);
    let mut target = game.new_target().map_err(|error| -> Box<dyn Error> {
        format!("failed to build the replay target: {error}").into()
    })?;
    let frames_before = game.frames_clocked(&target);
    counters.tree_import = match header.origin_kind.as_str() {
        ORIGIN_GENESIS => {
            core.bootstrap(game, &mut target)?;
            None
        }
        ORIGIN_SNAPSHOT_ROOT => {
            core.bootstrap_snapshot_root(
                game,
                &replay_run,
                &mut target,
                origin_checkpoint.ok_or("snapshot-root replay requires its snapshot checkpoint")?,
            )?;
            None
        }
        ORIGIN_ARCHIVE => Some(core.import_tree(
            game,
            &mut target,
            origin_report.ok_or("archive campaign replay requires the source archive")?,
            origin_checkpoint.map(|checkpoint| &checkpoint.snapshots),
        )?),
        _ => return Err("campaign stream origin kind is not recognized".into()),
    };
    counters.bootstrap_frames = game.frames_clocked(&target).saturating_sub(frames_before);

    for line in record_lines {
        let record: CampaignStreamRecord = serde_json::from_str(line)?;
        match record {
            CampaignStreamRecord::Skip(skip) => {
                let parent_index = usize::try_from(skip.parent_id)?;
                if parent_index >= core.archive.entries.len() {
                    return Err("recorded skip names a parent the archive does not hold".into());
                }
                let strategy =
                    energy_strategy(skip.mutation_seed, skip.mixture_weight, skip.splice_weight)?;
                let spliced = replay_splice(
                    &mut core,
                    parent_index,
                    header.action_limit,
                    strategy,
                    skip.splice,
                )?;
                let suffix = match spliced {
                    Some(tail) => tail,
                    None => game.expand_suffix_recorded(
                        &replay_run,
                        &draw_state,
                        replay_suffix,
                        MixtureDraw {
                            mixture: replay_mixture,
                            weight: skip.mixture_weight,
                            splice_weight: skip.splice_weight,
                        },
                        skip.draw_table_before.as_ref(),
                        skip.mutation_seed,
                    )?,
                };
                if !core.all_prefixes_archived(parent_index, &suffix) {
                    return Err("recorded skip is not a duplicate at its stream position".into());
                }
                let worker = usize::try_from(skip.worker)?;
                if worker >= counters.skips_per_worker.len() {
                    return Err("recorded skip names an unknown worker".into());
                }
                verify_selector_annotation(&skip.selector)?;
                core.archive.record_selection(parent_index, &skip.selector);
                counters.duplicates_skipped = counters.duplicates_skipped.saturating_add(1);
                counters.skips_per_worker[worker] =
                    counters.skips_per_worker[worker].saturating_add(1);
                let draw_table_after =
                    game.finish_stream_record(&replay_run, &mut draw_state, &[])?;
                if draw_table_after != skip.draw_table_after {
                    return Err("replayed skip draw-table checkpoint diverged".into());
                }
                game.remember_draw_version(&mut draw_state, &required_draw_versions)?;
            }
            CampaignStreamRecord::Job(job) => {
                let parent_index = usize::try_from(job.parent_id)?;
                let entry = core
                    .archive
                    .entries
                    .get(parent_index)
                    .ok_or("recorded job names a parent the archive does not hold")?;
                let snapshot = entry.snapshot.clone();
                let parent_actions = entry.report.input.actions.len();
                let parent_milestones = entry.report.milestones;
                let strategy =
                    energy_strategy(job.mutation_seed, job.mixture_weight, job.splice_weight)?;
                let spliced = replay_splice(
                    &mut core,
                    parent_index,
                    header.action_limit,
                    strategy,
                    job.splice,
                )?;
                let suffix = match spliced {
                    Some(tail) => tail,
                    None => game.expand_suffix_recorded(
                        &replay_run,
                        &draw_state,
                        replay_suffix,
                        MixtureDraw {
                            mixture: replay_mixture,
                            weight: job.mixture_weight,
                            splice_weight: job.splice_weight,
                        },
                        job.draw_table_before.as_ref(),
                        job.mutation_seed,
                    )?,
                };
                let job_frames_before = game.frames_clocked(&target);
                let result = game.execute_job(
                    &replay_run,
                    &mut target,
                    &snapshot,
                    parent_actions,
                    parent_milestones,
                    &suffix,
                    header.action_limit,
                    replay_retention,
                )?;
                let frames = game
                    .frames_clocked(&target)
                    .saturating_sub(job_frames_before);
                if frames != job.frames {
                    return Err(format!(
                        "replayed job {} emulated {frames} frames against recorded {}",
                        job.sequence, job.frames
                    )
                    .into());
                }
                let digest = game.result_sha256(&result)?;
                if digest != job.result_sha256 {
                    return Err(format!(
                        "replayed job {} result digest {digest} diverged from recorded {}",
                        job.sequence, job.result_sha256
                    )
                    .into());
                }
                let (sequence, decisions) = core.admit_job(game, job.parent_id, result)?;
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
                let draw_table_after =
                    finish_record(game, &replay_run, &mut draw_state, &core, &decisions)?;
                if draw_table_after != job.draw_table_after {
                    return Err(format!(
                        "replayed job {} draw-table checkpoint diverged",
                        job.sequence
                    )
                    .into());
                }
                game.remember_draw_version(&mut draw_state, &required_draw_versions)?;
                verify_selector_annotation(&job.selector)?;
                core.archive.record_selection(parent_index, &job.selector);
                let retained_ids = decisions
                    .iter()
                    .filter_map(|decision| match decision {
                        CampaignAdmissionDecision::Retained { id } => usize::try_from(*id).ok(),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
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
                let worker = usize::try_from(job.worker)?;
                if worker >= counters.jobs_per_worker.len() {
                    return Err("recorded job names an unknown worker".into());
                }
                counters.jobs_per_worker[worker] =
                    counters.jobs_per_worker[worker].saturating_add(1);
                counters.job_frames = counters.job_frames.saturating_add(frames);
            }
        }
    }
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
    Ok(build_report(
        game,
        &header,
        origin,
        core,
        &counters,
        stream_sha256,
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        CampaignJobRecord, CampaignStreamHeader, CampaignStreamRecord, GamePolicies,
        postcard_sha256,
    };
    use crate::search::archive::{SelectorDraw, SelectorPath};
    use crate::search::empirical_steps::EmpiricalStepCheckpoint;
    use sha2::{Digest, Sha256};

    /// The header a stream recorded before the policy map existed, verbatim.
    /// The generic layer now holds the game's policies in a map and names its
    /// draw-table fields generically; both must still read and write these
    /// exact field names, or recorded streams stop replaying.
    const RECORDED_HEADER: &str = r#"{"format":"smb-campaign-v1","campaign_seed":7,"workers":2,
"host":"box","origin_kind":"genesis","origin_path":null,"origin_archive_sha256":null,
"resume_input_sha256":"ab","resume_actions":0,"execution_budget":10,"wall_budget_seconds":null,
"action_limit":64,"archive_entry_limit":128,"controller_vocabulary":"nes_down_ten",
"key_policy":"frozen_area_span","duration_policy":"stratified","suffix_policy":"one_or_two",
"chord_policy":"chord_uniform","replacement_policy":"fewest_frames_in_level",
"resume_policy":"whole_tree","retention_policy":"admit_alive",
"parent_scheduler":"room_cell_uniform_128","executor_mode":"snapshot_resume_archive",
"worker_seed_derivation":"x","rom_sha256":"cd"}"#;

    #[test]
    fn incremental_postcard_digest_matches_encoded_bytes() {
        let value = (17_u64, vec![0_u8, 1, 15, 16, 255], Some("deterministic"));
        let encoded = postcard::to_allocvec(&value).expect("fixture encodes");
        let expected = format!("{:x}", Sha256::digest(encoded));
        let actual = postcard_sha256(&value).expect("incremental digest succeeds");

        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 64);
    }

    #[test]
    fn a_recorded_header_still_reads_and_writes_its_field_names() {
        let compact = RECORDED_HEADER.replace('\n', "");
        let header: CampaignStreamHeader<()> =
            serde_json::from_str(&compact).expect("recorded header parses");
        assert_eq!(header.suffix_policy, "one_or_two");
        assert_eq!(header.resume_policy, "whole_tree");
        assert_eq!(header.draw_table, None);
        let expected: GamePolicies = [
            ("controller_vocabulary", "nes_down_ten"),
            ("key_policy", "frozen_area_span"),
            ("duration_policy", "stratified"),
            ("chord_policy", "chord_uniform"),
            ("replacement_policy", "fewest_frames_in_level"),
        ]
        .into_iter()
        .map(|(field, value)| (field.to_owned(), value.to_owned()))
        .collect();
        assert_eq!(header.game_policies, expected);

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

    /// A job record's draw-table checkpoints keep their recorded names.
    #[test]
    fn a_recorded_job_keeps_its_draw_table_field_names() {
        let line = r#"{"event":"job","sequence":1,"worker":0,"parent_id":0,"mutation_seed":9,
"frames":12,"result_sha256":"ef","decisions":[],
"selector":{"path":"room_cell_uniform","classes_skipped":0,"counter_reset":false},
"chord_table_before":{"records":3,"retained_successes":1,"table_sha256":"aa"},
"chord_table_after":{"records":4,"retained_successes":2,"table_sha256":"bb"}}"#
            .replace('\n', "");
        let record: CampaignStreamRecord = serde_json::from_str(&line).expect("record parses");
        let CampaignStreamRecord::Job(job) = record else {
            panic!("expected a job record");
        };
        assert_eq!(job.splice, None, "historical jobs carry no splice evidence");
        assert_eq!(job.selector.path, SelectorPath::GroupWalk);
        assert_eq!(
            job.draw_table_before,
            Some(EmpiricalStepCheckpoint {
                records: 3,
                retained_successes: 1,
                table_sha256: "aa".to_owned(),
            })
        );
        let written = serde_json::to_value(&job).expect("job serializes");
        let object = written.as_object().expect("job is an object");
        assert!(object.contains_key("chord_table_before"));
        assert!(object.contains_key("chord_table_after"));
        assert!(!object.contains_key("draw_table_before"));
        let round_trip: CampaignJobRecord =
            serde_json::from_value(written).expect("job round-trips");
        assert_eq!(round_trip, job);
        let _ = SelectorDraw {
            path: SelectorPath::GroupWalk,
            classes_skipped: 0,
            counter_reset: false,
            concentration: None,
        };
    }

    #[test]
    fn a_job_records_its_dispatch_time_splice_resolution() {
        let line = r#"{"event":"job","sequence":1,"worker":0,"parent_id":3,"mutation_seed":9,
"frames":12,"result_sha256":"ef","decisions":[],"mixture_weight":85,"splice_weight":85,
"splice":{"outcome":"tail","donor_id":4,"leaf_id":9},
"selector":{"path":"uniform","classes_skipped":0,"counter_reset":false}}"#
            .replace('\n', "");
        let record: CampaignStreamRecord = serde_json::from_str(&line).expect("record parses");
        let CampaignStreamRecord::Job(job) = record else {
            panic!("expected a job record");
        };
        assert_eq!(
            job.splice,
            Some(super::CampaignSpliceRecord::Tail {
                donor_id: 4,
                leaf_id: 9,
            })
        );
        let written = serde_json::to_vec(&job).expect("job serializes");
        let round_trip: CampaignJobRecord =
            serde_json::from_slice(&written).expect("job round-trips");
        assert_eq!(round_trip, job);
    }
}
