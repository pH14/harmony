// SPDX-License-Identifier: AGPL-3.0-or-later

//! SMB adapter for asynchronous shared-archive search with a recorded job stream.
//!
//! A campaign runs W workers on one machine against one shared archive. A
//! job is a pure function of
//! (parent snapshot, mutation seed); the coordinator serializes selection and
//! admission, and records the complete admission-ordered job stream. The live
//! schedule is not derivable from the campaign seed alone: the recorded stream
//! is the campaign's identity, and replaying it serially must reproduce the
//! final archive and report byte for byte.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    io::Write,
    num::NonZeroUsize,
    path::PathBuf,
    time::Duration,
};

use libafl::executors::ExitKind;
use libafl_bolts::rands::{Rand, StdRand};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    search::empirical_steps::{
        EmpiricalStepCheckpoint, EmpiricalStepParameters, EmpiricalStepTables,
    },
    search::parallel::with_worker_pool,
    smb::archive::{
        Archive, ArchiveCandidate, DOWN_TEN_BUTTON_MASKS, KEY_POLICY_IDENTIFIER,
        REPLACEMENT_IDENTIFIER, SmbArchiveKey, SmbArchiveProgressPoint, SmbArchiveReport,
        SmbRetentionPolicy, SmbSelectorDraw, SmbSelectorPath, SmbSelectorPolicy,
        admission_is_viable, archive_key, merge_action_milestones, merge_milestones,
        merge_progress_watermark, milestone_key, retention_policy_from_identifier,
        retention_policy_identifier, selector_policy_from_identifier, selector_policy_identifier,
        update_first_inputs,
    },
    smb::target::{ButtonChord, SmbInput, SmbMilestones, SmbObservations, SmbSnapshot, SmbTarget},
    target::Target,
};

/// Stream format identifier written as the first line of every campaign stream.
pub const CAMPAIGN_STREAM_FORMAT: &str = "smb-campaign-stream-v1";

/// Fixed statement of the campaign determinism trade, recorded in every report.
pub const CAMPAIGN_SCHEDULE_IDENTITY: &str = "the live schedule is not derivable from the seed \
     alone; the recorded stream is this campaign's identity; two live runs at one seed may \
     differ, and each replays exactly";

/// Consecutive pre-execution duplicate skips after which a worker executes the
/// next drawn job anyway and lets admission deduplicate, so a saturated archive
/// cannot livelock selection.
const CONSECUTIVE_SKIP_LIMIT: u64 = 1_024;

/// Curve sampling interval in admitted executions.
const CURVE_INTERVAL: u64 = 100;

/// Where a campaign starts: clean genesis or a recorded source archive.
pub enum SmbCampaignOrigin {
    /// Start from gameplay genesis with a single empty input.
    Genesis,
    /// Resume a recorded archive with its whole retained tree.
    Archive {
        /// Path string recorded verbatim in the stream header.
        path: String,
        /// SHA-256 of the source archive file bytes.
        file_sha256: String,
        /// The parsed source archive report.
        report: Box<SmbArchiveReport>,
        /// Snapshot checkpoint of the source archive, when one was supplied.
        checkpoint: Option<SmbCampaignCheckpoint>,
    },
}

/// A loaded snapshot checkpoint and the file identity recorded for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SmbCampaignCheckpoint {
    /// Path string recorded verbatim in the stream header.
    pub path: String,
    /// SHA-256 of the checkpoint file bytes.
    pub file_sha256: String,
    /// The decoded snapshots.
    pub snapshots: SmbSnapshotCheckpoint,
}

/// Format tag of the snapshot checkpoint file.
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "smb-snapshot-checkpoint-v1";

/// Every retained entry's emulator snapshot, keyed by archive identifier, so a
/// whole-tree resume can restore the population instead of re-emulating it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbSnapshotCheckpoint {
    /// Always [`SNAPSHOT_CHECKPOINT_FORMAT`].
    pub format: String,
    /// Snapshots in archive identifier order.
    pub entries: Vec<SmbSnapshotCheckpointEntry>,
}

/// One archive entry's snapshot.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbSnapshotCheckpointEntry {
    /// Archive identifier the snapshot belongs to.
    pub id: u64,
    /// The retained snapshot.
    pub snapshot: SmbSnapshot,
}

impl SmbSnapshotCheckpoint {
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
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Box<dyn Error>> {
        let checkpoint: Self = postcard::from_bytes(bytes)?;
        if checkpoint.format != SNAPSHOT_CHECKPOINT_FORMAT {
            return Err("snapshot checkpoint format is not recognized".into());
        }
        Ok(checkpoint)
    }
}

/// Fixed configuration for one live campaign run.
pub struct SmbCampaignConfig {
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
    /// Chord policy for this run, recorded in the header and report.
    pub chord: SmbCampaignChordPolicy,
    /// Admission rule for this run, recorded in the header and report.
    pub retention: SmbRetentionPolicy,
    /// Parent selector for this run, recorded in the header and report.
    pub selector: SmbSelectorPolicy,
    /// Live-only: where the first winning input is written the moment it is
    /// admitted, before the in-flight jobs drain. Never recorded.
    pub victory_input_path: Option<PathBuf>,
    /// Live-only: directory receiving a whole-tree checkpoint every
    /// [`LIVE_CHECKPOINT_INTERVAL`] executions, so an interrupted run of this
    /// binary can be resumed instead of restarted. Never recorded.
    pub checkpoint_dir: Option<PathBuf>,
}

/// Executions between live whole-tree checkpoint writes.
pub const LIVE_CHECKPOINT_INTERVAL: u64 = 25_000;

/// First line of the stream: everything a replay needs to know about the run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbCampaignStreamHeader {
    /// Stream format identifier.
    pub format: String,
    /// Campaign seed.
    pub campaign_seed: u64,
    /// Worker count W.
    pub workers: u32,
    /// Operator-supplied host name.
    pub host: String,
    /// Origin kind: `genesis` or `archive`.
    pub origin_kind: String,
    /// Source archive path for archive origins.
    pub origin_path: Option<String>,
    /// SHA-256 of the source archive file bytes for archive origins.
    pub origin_archive_sha256: Option<String>,
    /// Snapshot checkpoint path when a whole-tree resume restored from one.
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
    /// Controller vocabulary identifier.
    pub controller_vocabulary: String,
    /// Archive key policy identifier.
    pub key_policy: String,
    /// Hold distribution identifier.
    pub duration_policy: String,
    /// Suffix shape identifier.
    pub suffix_policy: String,
    /// Chord policy identifier.
    pub chord_policy: String,
    /// Cell-replacement rule identifier.
    pub replacement_policy: String,
    /// Resume rule identifier.
    pub resume_policy: String,
    /// Derived chord-table provenance; absent for uniform and compiled tables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chord_table: Option<SmbChordTableHeader>,
    /// Admission rule identifier.
    pub retention_policy: String,
    /// Parent selector identifier.
    pub parent_scheduler: String,
    /// Executor mode identifier.
    pub executor_mode: String,
    /// How per-worker stream seeds derive from (campaign seed, worker index).
    pub worker_seed_derivation: String,
    /// SHA-256 of the ROM bytes.
    pub rom_sha256: String,
}

/// One admission decision for one candidate boundary, in candidate order.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum SmbCampaignAdmissionDecision {
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
    /// The action reached the game's victory mode; the lineage ends here and
    /// its input is the campaign's winning input.
    Victory,
}

/// Stream record for one executed, admitted job.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbCampaignJobRecord {
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
    pub decisions: Vec<SmbCampaignAdmissionDecision>,
    /// Selector draw record.
    pub selector: SmbSelectorDraw,
    /// Derived table version used to draw this job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chord_table_before: Option<EmpiricalStepCheckpoint>,
    /// Periodic derived table hash after admitting this stream record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chord_table_after: Option<EmpiricalStepCheckpoint>,
}

/// Stream record for one job skipped before execution as a known duplicate.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbCampaignSkipRecord {
    /// Worker index that drew the duplicate.
    pub worker: u32,
    /// Archive id of the selected parent.
    pub parent_id: u64,
    /// Mutation seed whose full prefix chain was already archived.
    pub mutation_seed: u64,
    /// Selector draw record.
    pub selector: SmbSelectorDraw,
    /// Derived table version used to draw this skipped job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chord_table_before: Option<EmpiricalStepCheckpoint>,
    /// Periodic derived table hash after this stream record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chord_table_after: Option<EmpiricalStepCheckpoint>,
}

/// One line of the recorded stream after the header.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SmbCampaignStreamRecord {
    /// An executed job admitted at its sequence position.
    Job(SmbCampaignJobRecord),
    /// A pre-execution duplicate skip; consumes no budget and changes no state.
    Skip(SmbCampaignSkipRecord),
}

/// Origin summary recorded in the campaign report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbCampaignOriginRecord {
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
pub struct SmbCampaignModeReport {
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
    pub origin: SmbCampaignOriginRecord,
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
    /// Controller vocabulary identifier.
    pub controller_vocabulary: String,
    /// Archive key policy identifier.
    pub key_policy: String,
    /// Hold distribution identifier.
    pub duration_policy: String,
    /// Suffix shape identifier.
    pub suffix_policy: String,
    /// Chord policy identifier.
    pub chord_policy: String,
    /// Cell-replacement rule identifier.
    pub replacement_policy: String,
    /// Resume rule identifier.
    pub resume_policy: String,
    /// Admission rule identifier.
    pub retention_policy: String,
    /// Parent selector identifier.
    pub parent_scheduler: String,
    /// Executor mode identifier.
    pub executor_mode: String,
    /// How per-worker stream seeds derive from (campaign seed, worker index).
    pub worker_seed_derivation: String,
    /// SHA-256 of the ROM bytes.
    pub rom_sha256: String,
    /// Frames emulated by the origin bootstrap walk, probes included.
    pub bootstrap_frames: u64,
    /// Outcome counts of the whole-tree import; absent at genesis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_import: Option<SmbTreeImportCounts>,
    /// Bootstrap frames plus every job's frames, probes included.
    pub frames_emulated: u64,
    /// Jobs skipped before execution as known duplicates.
    pub duplicates_skipped: u64,
    /// Candidates refused by the admission probe.
    pub probe_refused: u64,
    /// Actions that reached the game's victory mode.
    pub victories: u64,
    /// The first input that reached the victory mode, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub victory_input: Option<SmbInput>,
    /// Cell collisions the frames-in-level replacement rule decided.
    pub replacement_frames_displaced: u64,
    /// Executed jobs per worker index.
    pub jobs_per_worker: Vec<u64>,
    /// Pre-execution duplicate skips per worker index.
    pub skips_per_worker: Vec<u64>,
    /// SHA-256 of the complete stream file bytes.
    pub stream_sha256: String,
    /// The archive in the standard report shape used by film and audits.
    pub archive: SmbArchiveReport,
}

/// Identifier recorded for the resume rule: the source archive's whole
/// retained tree is imported, and the header's resume input is the frontier
/// identity only.
pub const RESUME_IDENTIFIER: &str = "whole_tree";

/// Identifier recorded for the suffix shape: one action, or two at
/// one-in-four odds.
pub const SUFFIX_IDENTIFIER: &str = "one_or_two";

/// Identifier recorded for the hold distribution, see
/// [`crate::smb::archive::sample_chord_from_masks`].
pub const DURATION_IDENTIFIER: &str = "stratified";

/// Identifier recorded for the controller vocabulary, see
/// [`DOWN_TEN_BUTTON_MASKS`].
pub const VOCABULARY_IDENTIFIER: &str = "down_ten_mask";

/// A source archive's frontier identity: the shortest input among the entries
/// at the deepest recorded `(world, level, progress)`, earliest id on ties.
///
/// # Errors
///
/// Returns an error when the source archive has no retained entries.
pub fn select_frontier_resume_input(source: &SmbArchiveReport) -> Result<SmbInput, Box<dyn Error>> {
    let frontier = source
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level, entry.key.progress))
        .max()
        .ok_or("source archive contains no retained entries")?;
    source
        .entries
        .iter()
        .filter(|entry| (entry.key.world, entry.key.level, entry.key.progress) == frontier)
        .min_by_key(|entry| (entry.input.actions.len(), entry.id))
        .map(|entry| entry.input.clone())
        .ok_or_else(|| "source archive contains no frontier entries".into())
}

/// Reject a header whose fixed-rule identifiers differ from the compiled
/// ones, and resolve its per-run retention and selector values.
fn verify_fixed_rules(
    header: &SmbCampaignStreamHeader,
) -> Result<(SmbRetentionPolicy, SmbSelectorPolicy), Box<dyn Error>> {
    let expected = [
        (
            header.key_policy.as_str(),
            KEY_POLICY_IDENTIFIER,
            "key policy",
        ),
        (
            header.replacement_policy.as_str(),
            REPLACEMENT_IDENTIFIER,
            "replacement policy",
        ),
        (
            header.resume_policy.as_str(),
            RESUME_IDENTIFIER,
            "resume policy",
        ),
        (
            header.suffix_policy.as_str(),
            SUFFIX_IDENTIFIER,
            "suffix policy",
        ),
        (
            header.duration_policy.as_str(),
            DURATION_IDENTIFIER,
            "duration policy",
        ),
        (
            header.controller_vocabulary.as_str(),
            VOCABULARY_IDENTIFIER,
            "controller vocabulary",
        ),
    ];
    for (recorded, compiled, name) in expected {
        if recorded != compiled {
            return Err(format!("campaign stream {name} is not recognized").into());
        }
    }
    Ok((
        retention_policy_from_identifier(&header.retention_policy)?,
        selector_policy_from_identifier(&header.parent_scheduler)?,
    ))
}

/// Reject a stream whose selector annotations disagree with the selector.
fn verify_selector_annotation(draw: &SmbSelectorDraw) -> Result<(), Box<dyn Error>> {
    match (draw.path, draw.concentration) {
        (SmbSelectorPath::RoomCellUniform, None) => {
            Err("cell draw is missing its concentration record".into())
        }
        (SmbSelectorPath::Uniform, Some(_)) => {
            Err("uniform draw carries a concentration record".into())
        }
        _ => Ok(()),
    }
}

/// Derive one worker's stream seed from the campaign seed and worker index.
fn derive_worker_seed(campaign_seed: u64, worker_index: u32) -> Result<u64, Box<dyn Error>> {
    let mut hasher = Sha256::new();
    hasher.update(campaign_seed.to_le_bytes());
    hasher.update(worker_index.to_le_bytes());
    let digest = hasher.finalize();
    let bytes: [u8; 8] = digest[..8]
        .try_into()
        .map_err(|_| "worker seed digest is too short")?;
    Ok(u64::from_le_bytes(bytes))
}

/// Expand one mutation seed into its complete suffix.
///
/// The suffix is sampled from a fresh RNG seeded with the mutation seed
/// alone, so a job is a pure function of (parent snapshot, mutation seed).
/// Under a recorded chord policy each chord comes from the table at even
/// odds with the uniform draw. Public so recorded-artifact diagnostics can
/// re-derive the actions a stream's jobs executed.
///
/// # Errors
///
/// Returns an error when a draw bound is invalid or a recorded chord policy
/// is missing its folded tables.
pub fn derive_suffix(
    mutation_seed: u64,
    chord_policy: SmbCampaignChordPolicy,
    chord_tables: Option<&EmpiricalStepTables<ButtonChord>>,
) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
    let mut rand = StdRand::with_seed(mutation_seed);
    let suffix_len = if rand.below(NonZeroUsize::new(4).ok_or("invalid suffix odds")?) == 0 {
        2
    } else {
        1
    };
    let mut suffix = Vec::with_capacity(suffix_len);
    for _ in 0..suffix_len {
        let recorded = chord_policy != SmbCampaignChordPolicy::Uniform
            && rand.below(NonZeroUsize::new(2).ok_or("invalid chord odds")?) == 0;
        if recorded {
            let mined = match chord_policy {
                SmbCampaignChordPolicy::Uniform => None,
                SmbCampaignChordPolicy::RecordedHalf => {
                    let index = rand.below(
                        NonZeroUsize::new(RECORDED_CHORD_TABLE.len())
                            .ok_or("empty recorded chord table")?,
                    );
                    let (buttons, hold) = RECORDED_CHORD_TABLE[index];
                    Some(ButtonChord::new(buttons, hold))
                }
                SmbCampaignChordPolicy::DerivedHalf(_) => {
                    let tables = chord_tables.ok_or("derived chord policy has no folded tables")?;
                    let length = tables.mixed_len()?;
                    NonZeroUsize::new(length)
                        .and_then(|length| tables.mixed_step(rand.below(length)))
                        .copied()
                }
            };
            if let Some(chord) = mined {
                suffix.push(chord);
                continue;
            }
        }
        suffix.push(crate::smb::archive::sample_chord_from_masks(
            &mut rand,
            &DOWN_TEN_BUTTON_MASKS,
        )?);
    }
    Ok(suffix)
}

/// Chords copied from the lineages that crossed the 7-4 castle checks, the
/// machine's own recorded sample of maneuvers those checks reward. Drawing
/// uniformly from the list reproduces the empirical distribution; duplicates
/// carry the frequencies.
const RECORDED_CHORD_TABLE: [(u8, u8); 328] = [
    (131, 102),
    (130, 4),
    (64, 6),
    (131, 108),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (130, 12),
    (130, 115),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (131, 4),
    (130, 112),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (130, 12),
    (1, 118),
    (130, 9),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (130, 118),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (130, 118),
    (64, 4),
    (128, 104),
    (129, 3),
    (129, 99),
    (0, 96),
    (129, 111),
    (128, 104),
    (129, 3),
    (129, 99),
    (0, 96),
    (129, 111),
    (32, 115),
    (128, 104),
    (129, 3),
    (129, 99),
    (16, 3),
    (129, 107),
    (128, 104),
    (129, 3),
    (129, 99),
    (0, 96),
    (32, 8),
    (129, 113),
    (131, 7),
    (1, 97),
    (128, 119),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (130, 12),
    (1, 118),
    (2, 9),
    (1, 2),
    (131, 103),
    (128, 104),
    (129, 3),
    (129, 99),
    (0, 96),
    (32, 8),
    (128, 108),
    (128, 104),
    (129, 3),
    (129, 99),
    (0, 96),
    (32, 8),
    (128, 108),
    (16, 10),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (131, 4),
    (1, 7),
    (16, 99),
    (2, 115),
    (16, 119),
    (16, 12),
    (130, 108),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (131, 4),
    (1, 7),
    (16, 99),
    (128, 102),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (130, 12),
    (1, 118),
    (16, 108),
    (128, 106),
    (131, 96),
    (64, 99),
    (2, 105),
    (0, 3),
    (16, 4),
    (128, 116),
    (131, 120),
    (2, 11),
    (131, 96),
    (64, 99),
    (2, 105),
    (0, 3),
    (16, 4),
    (128, 116),
    (131, 120),
    (130, 11),
    (16, 5),
    (129, 12),
    (129, 115),
    (1, 105),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (130, 12),
    (1, 118),
    (2, 9),
    (64, 6),
    (128, 119),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (130, 12),
    (1, 118),
    (2, 9),
    (129, 7),
    (131, 100),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (130, 12),
    (1, 118),
    (16, 108),
    (128, 4),
    (128, 97),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (130, 12),
    (1, 118),
    (2, 9),
    (1, 2),
    (130, 96),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (130, 12),
    (1, 118),
    (129, 11),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (130, 12),
    (1, 118),
    (16, 108),
    (128, 4),
    (128, 9),
    (128, 104),
    (129, 3),
    (129, 99),
    (0, 100),
    (1, 107),
    (2, 118),
    (128, 112),
    (128, 104),
    (129, 3),
    (129, 99),
    (16, 3),
    (130, 3),
    (0, 10),
    (128, 105),
    (128, 104),
    (129, 3),
    (129, 99),
    (16, 3),
    (130, 3),
    (0, 10),
    (128, 105),
    (129, 11),
    (32, 112),
    (1, 5),
    (128, 100),
    (2, 9),
    (131, 98),
    (128, 110),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (130, 12),
    (1, 118),
    (2, 9),
    (32, 97),
    (1, 8),
    (131, 97),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (130, 12),
    (1, 118),
    (2, 9),
    (32, 97),
    (1, 8),
    (131, 2),
    (1, 12),
    (0, 6),
    (129, 111),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (130, 12),
    (1, 118),
    (2, 9),
    (32, 97),
    (1, 8),
    (32, 103),
    (32, 112),
    (131, 115),
    (128, 104),
    (129, 3),
    (129, 99),
    (0, 100),
    (1, 107),
    (2, 118),
    (131, 10),
    (129, 115),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (131, 4),
    (1, 7),
    (16, 99),
    (2, 4),
    (32, 115),
    (64, 107),
    (130, 114),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (131, 4),
    (1, 7),
    (16, 99),
    (2, 4),
    (32, 115),
    (32, 4),
    (129, 107),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (130, 12),
    (1, 118),
    (2, 9),
    (32, 97),
    (1, 8),
    (32, 110),
    (131, 107),
    (128, 104),
    (129, 3),
    (129, 99),
    (16, 3),
    (130, 3),
    (0, 10),
    (128, 105),
    (129, 4),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (130, 12),
    (1, 118),
    (2, 9),
    (129, 7),
    (64, 118),
    (16, 12),
    (129, 99),
    (16, 5),
    (2, 5),
    (131, 110),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (130, 12),
    (1, 118),
    (2, 9),
    (129, 7),
    (129, 113),
    (128, 104),
    (129, 3),
    (129, 99),
    (64, 9),
    (130, 12),
    (1, 118),
    (2, 9),
    (32, 97),
    (1, 8),
    (32, 103),
    (32, 112),
    (2, 102),
    (129, 117),
];

/// SMB-only filter preserving the existing deep-lineage extraction semantics.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbChordSourceFilter {
    /// Zero-based source world.
    pub world: u8,
    /// Zero-based source level.
    pub level: u8,
    /// Minimum retained source progress.
    pub minimum_progress: u16,
}

/// Complete registered derivation for one pair of mined chord tables.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbChordTableDerivation {
    /// Thin SMB source filter.
    pub source_filter: SmbChordSourceFilter,
    /// Game-neutral extraction, mixture, update, and hash parameters.
    pub parameters: EmpiricalStepParameters,
}

/// Header provenance for a derived chord-table policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbChordTableHeader {
    /// SHA-256 of the named resume archive, or SHA-256 of empty bytes at genesis.
    pub source_sha256: String,
    /// Registered source filter and game-neutral fold parameters.
    pub derivation: SmbChordTableDerivation,
    /// Hash after folding the named source and before the first campaign draw.
    pub initial: EmpiricalStepCheckpoint,
}

/// Chord policy a campaign draws chords from, recorded in the stream header.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbCampaignChordPolicy {
    /// Every chord drawn uniformly from the vocabulary.
    #[default]
    Uniform,
    /// Each chord comes from the compiled recorded table at even odds with
    /// the uniform draw, so exploration mass stays while the sampled shape
    /// follows the machine's own successes.
    RecordedHalf,
    /// Derive recent and all-history tables from the recorded source, mixing
    /// their registered empirical weights into the biased half of each draw.
    DerivedHalf(SmbChordTableDerivation),
}

/// Header identifier for a chord policy.
#[must_use]
pub fn chord_policy_identifier(policy: SmbCampaignChordPolicy) -> String {
    match policy {
        SmbCampaignChordPolicy::Uniform => "chord_uniform".to_owned(),
        SmbCampaignChordPolicy::RecordedHalf => "chord_draw_recorded_50".to_owned(),
        SmbCampaignChordPolicy::DerivedHalf(derivation) => {
            let source = derivation.source_filter;
            let parameters = derivation.parameters;
            format!(
                "chord_draw_recorded_50:{},{},{},{},{},{},{},{},{}",
                source.world,
                source.level,
                source.minimum_progress,
                parameters.prefix_steps,
                parameters.recent_successes,
                parameters.recent_weight,
                parameters.all_history_weight,
                parameters.update_every_records,
                parameters.hash_every_records
            )
        }
    }
}

/// Chord policy named by a recorded header identifier.
///
/// # Errors
///
/// Returns an error when the identifier names no known chord policy.
pub fn chord_policy_from_identifier(
    identifier: &str,
) -> Result<SmbCampaignChordPolicy, Box<dyn Error>> {
    if identifier == "chord_uniform" {
        return Ok(SmbCampaignChordPolicy::Uniform);
    }
    if identifier == "chord_draw_recorded_50" {
        return Ok(SmbCampaignChordPolicy::RecordedHalf);
    }
    if let Some(fields) = identifier.strip_prefix("chord_draw_recorded_50:") {
        let mut fields = fields.split(',');
        let source_filter = SmbChordSourceFilter {
            world: parse_chord_field(&mut fields, "world")?,
            level: parse_chord_field(&mut fields, "level")?,
            minimum_progress: parse_chord_field(&mut fields, "minimum progress")?,
        };
        let parameters = EmpiricalStepParameters {
            prefix_steps: parse_chord_field(&mut fields, "prefix steps")?,
            recent_successes: parse_chord_field(&mut fields, "recent successes")?,
            recent_weight: parse_chord_field(&mut fields, "recent weight")?,
            all_history_weight: parse_chord_field(&mut fields, "all-history weight")?,
            update_every_records: parse_chord_field(&mut fields, "update interval")?,
            hash_every_records: parse_chord_field(&mut fields, "hash interval")?,
        };
        if fields.next().is_some() {
            return Err("derived chord policy carries extra fields".into());
        }
        parameters.validate()?;
        return Ok(SmbCampaignChordPolicy::DerivedHalf(
            SmbChordTableDerivation {
                source_filter,
                parameters,
            },
        ));
    }
    Err("campaign stream chord policy is not recognized".into())
}

fn parse_chord_field<'a, T>(
    fields: &mut impl Iterator<Item = &'a str>,
    name: &str,
) -> Result<T, Box<dyn Error>>
where
    T: std::str::FromStr,
    T::Err: Error + 'static,
{
    Ok(fields
        .next()
        .ok_or_else(|| format!("derived chord policy is missing {name}"))?
        .parse()?)
}

/// One candidate boundary inside a job result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct SmbCampaignCandidate {
    key: SmbArchiveKey,
    viable: bool,
    snapshot: SmbSnapshot,
}

/// One executed action inside a job result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct SmbCampaignActionResult {
    action: ButtonChord,
    observations: Vec<SmbObservations>,
    milestones: SmbMilestones,
    dead: bool,
    victory: bool,
    failed: bool,
    candidate: Option<SmbCampaignCandidate>,
}

/// Complete result of one executed job; its serialization is digested into the
/// stream so replay verifies byte-exact re-execution, snapshots included.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct SmbCampaignJobResult {
    actions: Vec<SmbCampaignActionResult>,
}

/// Execute one job: restore the parent snapshot and apply the suffix exactly as
/// the campaign suffix loop does, collecting per-boundary candidates
/// with worker-side probe verdicts.
/// Per-run execution policies a worker applies to every job.
#[derive(Clone, Copy, Debug)]
struct SmbJobPolicies {
    max_actions: usize,
    retention: SmbRetentionPolicy,
}

fn execute_job(
    target: &mut SmbTarget,
    parent_snapshot: &SmbSnapshot,
    parent_actions: usize,
    parent_milestones: SmbMilestones,
    suffix: &[ButtonChord],
    policies: SmbJobPolicies,
) -> Result<SmbCampaignJobResult, Box<dyn Error>> {
    target.restore(parent_snapshot)?;
    let mut milestones = parent_milestones;
    let mut length = parent_actions;
    let mut actions = Vec::with_capacity(suffix.len());
    for action in suffix {
        if target.is_dead() || target.is_victory() || length >= policies.max_actions {
            break;
        }
        length = length.saturating_add(1);
        target.apply(action);
        merge_action_milestones(&mut milestones, target)?;
        let observations = target.last_action_observations().to_vec();
        let dead = target.is_dead();
        let victory = target.is_victory();
        let failed = target.exit_kind() != ExitKind::Ok;
        // A won game is terminal: nothing past it is searched, so no
        // candidate is offered.
        let candidate = if dead || victory || failed {
            None
        } else {
            let snapshot = target
                .snapshot()
                .ok_or("failed to snapshot campaign suffix")?;
            let key = archive_key(target.wram());
            let viable = match policies.retention {
                SmbRetentionPolicy::ProbeAtAdmission45 => admission_is_viable(target, &snapshot)?,
                SmbRetentionPolicy::AdmitAlive => true,
            };
            Some(SmbCampaignCandidate {
                key,
                viable,
                snapshot,
            })
        };
        actions.push(SmbCampaignActionResult {
            action: *action,
            observations,
            milestones,
            dead,
            victory,
            failed,
            candidate,
        });
        if dead || victory || failed {
            break;
        }
    }
    Ok(SmbCampaignJobResult { actions })
}

/// Serial archive-and-accumulator state shared by the live coordinator loop and
/// replay. Admission through this struct is the single admission lock: every
/// archive mutation happens here, in stream order, so the archive state at any
/// stream position is identical in the live run and in replay.
struct CoordinatorCore {
    archive: Archive,
    aggregate: SmbMilestones,
    watermark: crate::smb::target::SmbProgressWatermark,
    first_reached: crate::smb::target::SmbMilestoneTimes,
    first_inputs: crate::smb::target::SmbMilestoneInputs,
    champion_input: SmbInput,
    champion_milestones: SmbMilestones,
    curve: Vec<SmbArchiveProgressPoint>,
    deaths: u64,
    victories: u64,
    victory_input: Option<SmbInput>,
    sequence: u64,
    probe_refused: u64,
    max_actions: usize,
}

impl CoordinatorCore {
    fn new(max_actions: usize, archive_entry_limit: usize) -> Self {
        let mut archive = Archive::new();
        archive.max_entries = archive_entry_limit;
        Self {
            archive,
            aggregate: SmbMilestones::default(),
            watermark: crate::smb::target::SmbProgressWatermark::default(),
            first_reached: crate::smb::target::SmbMilestoneTimes::default(),
            first_inputs: crate::smb::target::SmbMilestoneInputs::default(),
            champion_input: SmbInput::default(),
            champion_milestones: SmbMilestones::default(),
            curve: Vec::new(),
            deaths: 0,
            victories: 0,
            victory_input: None,
            sequence: 0,
            probe_refused: 0,
            max_actions,
        }
    }

    /// Retain genesis at execution zero.
    fn bootstrap(&mut self, target: &mut SmbTarget) -> Result<(), Box<dyn Error>> {
        target.reset();
        let genesis_key = archive_key(target.wram());
        let genesis_snapshot = target
            .snapshot()
            .ok_or("failed to snapshot campaign genesis")?;
        self.archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    input: SmbInput::default(),
                    key: genesis_key,
                    milestones: SmbMilestones::default(),
                },
                genesis_snapshot,
            )?
            .ok_or("failed to retain campaign genesis")?;
        Ok(())
    }

    /// Import a source archive's whole retained tree at execution zero, after
    /// retaining genesis.
    ///
    /// Entries are walked in source order. Each one restores its imported
    /// parent's snapshot, applies the actions past the parent's input, and is
    /// inserted under this run's policies, so liveness, cells, room sets, and
    /// replacement decisions are re-derived rather than copied. The admission
    /// probe is not repeated: the source already admitted every entry, and
    /// probing tens of thousands of imports would cost more frames than the
    /// search they seed. An entry whose parent was not imported is re-rooted
    /// at its nearest imported ancestor; an entry that dies or exceeds the
    /// action limit is skipped and counted.
    fn import_tree(
        &mut self,
        target: &mut SmbTarget,
        source: &SmbArchiveReport,
        checkpoint: Option<&SmbSnapshotCheckpoint>,
    ) -> Result<SmbTreeImportCounts, Box<dyn Error>> {
        let checkpointed: BTreeMap<u64, &SmbSnapshot> = checkpoint
            .map(|checkpoint| {
                checkpoint
                    .entries
                    .iter()
                    .map(|entry| (entry.id, &entry.snapshot))
                    .collect()
            })
            .unwrap_or_default();
        // The source watermark already covers every action interior the
        // source run observed, so both import paths merge it whole.
        self.watermark = self.watermark.max(source.progress_watermark);
        self.bootstrap(target)?;
        let genesis_id = 0;
        let mut counts = SmbTreeImportCounts::default();
        let mut index_of: BTreeMap<u64, usize> = BTreeMap::new();
        let mut imported: Vec<Option<usize>> = Vec::with_capacity(source.entries.len());
        for (index, entry) in source.entries.iter().enumerate() {
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
                ancestor = source.entries[ancestor_index].parent_id;
            }
            let (parent_input_len, parent_id) = match parent {
                Some((ancestor_index, new_id)) => {
                    let parent_input = &source.entries[ancestor_index].input.actions;
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
                target.restore(snapshot)?;
                merge_milestones(&mut milestones, entry.milestones);
                counts.checkpointed = counts.checkpointed.saturating_add(1);
                if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                    None
                } else {
                    Some((*snapshot).clone())
                }
            } else {
                target.restore(&parent_entry.snapshot)?;
                let mut terminal = false;
                for action in &entry.input.actions[parent_input_len..] {
                    target.apply(action);
                    merge_action_milestones(&mut milestones, target)?;
                    if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                        terminal = true;
                        break;
                    }
                }
                if terminal {
                    None
                } else {
                    Some(
                        target
                            .snapshot()
                            .ok_or("failed to snapshot campaign tree import")?,
                    )
                }
            };
            let Some(snapshot) = snapshot else {
                counts.terminal = counts.terminal.saturating_add(1);
                imported.push(None);
                continue;
            };
            merge_milestones(&mut self.aggregate, milestones);
            update_first_inputs(
                &mut self.first_reached,
                &mut self.first_inputs,
                milestones,
                0,
                &prefix,
            );
            if milestone_key(milestones) > milestone_key(self.champion_milestones) {
                self.champion_milestones = milestones;
                self.champion_input = prefix.clone();
            }
            let key = archive_key(target.wram());
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
    fn admit_job(
        &mut self,
        parent_id: u64,
        result: &SmbCampaignJobResult,
    ) -> Result<(u64, Vec<SmbCampaignAdmissionDecision>), Box<dyn Error>> {
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
        for action in &result.actions {
            input.actions.push(action.action);
            merge_progress_watermark(&mut self.watermark, &action.observations);
            merge_milestones(&mut self.aggregate, action.milestones);
            update_first_inputs(
                &mut self.first_reached,
                &mut self.first_inputs,
                action.milestones,
                sequence,
                &input,
            );
            if milestone_key(action.milestones) > milestone_key(self.champion_milestones) {
                self.champion_milestones = action.milestones;
                self.champion_input = input.clone();
            }
            if action.dead {
                self.deaths = self.deaths.saturating_add(1);
            }
            if action.victory {
                self.victories = self.victories.saturating_add(1);
                self.victory_input.get_or_insert_with(|| input.clone());
                decisions.push(SmbCampaignAdmissionDecision::Victory);
            }
            if let Some(candidate) = &action.candidate {
                if !candidate.viable {
                    self.probe_refused = self.probe_refused.saturating_add(1);
                    decisions.push(SmbCampaignAdmissionDecision::ProbeRefused);
                    continue;
                }
                let inserted_before = self.archive.entries.len();
                match self.archive.insert(
                    Some(current_parent),
                    sequence,
                    ArchiveCandidate {
                        input: input.clone(),
                        key: candidate.key,
                        milestones: action.milestones,
                    },
                    candidate.snapshot.clone(),
                )? {
                    Some(id) if id == inserted_before => {
                        decisions.push(SmbCampaignAdmissionDecision::Retained {
                            id: u64::try_from(id)?,
                        });
                        current_parent = id;
                    }
                    Some(id) => {
                        decisions.push(SmbCampaignAdmissionDecision::Duplicate {
                            id: u64::try_from(id)?,
                        });
                        current_parent = id;
                    }
                    None => decisions.push(SmbCampaignAdmissionDecision::Rejected),
                }
            }
        }
        if sequence.is_multiple_of(CURVE_INTERVAL) {
            self.push_curve_point();
        }
        Ok((sequence, decisions))
    }

    fn push_curve_point(&mut self) {
        self.curve.push(SmbArchiveProgressPoint {
            executions: self.sequence,
            milestones: self.aggregate,
            active_entries: self.archive.active.iter().filter(|active| **active).count(),
            occupied_cells: self.archive.cells.len(),
            deaths: self.deaths,
        });
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
    fn all_prefixes_archived(&self, parent_index: usize, suffix: &[ButtonChord]) -> bool {
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

    /// Clone the archive into a report without ending the run, for the live
    /// whole-tree checkpoint.
    fn archive_report_snapshot(&self, campaign_seed: u64) -> SmbArchiveReport {
        SmbArchiveReport {
            seed: campaign_seed,
            executions: self.sequence,
            milestones: self.aggregate,
            progress_watermark: self.watermark,
            first_reached: self.first_reached,
            first_inputs: self.first_inputs.clone(),
            champion_input: self.champion_input.clone(),
            entries: self.archive.entry_reports_snapshot(),
            progress_curve: self.curve.clone(),
            retained: self.archive.retained,
            rejected: self.archive.rejected,
            deaths: self.deaths,
            selector: self.archive.selector_report(),
        }
    }

    fn into_archive_report(mut self, campaign_seed: u64) -> SmbArchiveReport {
        let entries = self.archive.take_entry_reports();
        SmbArchiveReport {
            seed: campaign_seed,
            executions: self.sequence,
            milestones: self.aggregate,
            progress_watermark: self.watermark,
            first_reached: self.first_reached,
            first_inputs: self.first_inputs,
            champion_input: self.champion_input,
            entries,
            progress_curve: self.curve,
            retained: self.archive.retained,
            rejected: self.archive.rejected,
            deaths: self.deaths,
            selector: self.archive.selector_report(),
        }
    }
}

/// Outcome counts of a whole-tree import, recorded in the campaign report.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbTreeImportCounts {
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

type InitialChordTables = (
    Option<EmpiricalStepTables<ButtonChord>>,
    Option<SmbChordTableHeader>,
);

fn initial_chord_tables(
    policy: SmbCampaignChordPolicy,
    origin: &SmbCampaignOrigin,
) -> Result<InitialChordTables, Box<dyn Error>> {
    let SmbCampaignChordPolicy::DerivedHalf(derivation) = policy else {
        return Ok((None, None));
    };
    let mut tables = EmpiricalStepTables::new(derivation.parameters)?;
    let source_sha256 = match origin {
        SmbCampaignOrigin::Genesis => format!("{:x}", Sha256::digest([])),
        SmbCampaignOrigin::Archive {
            file_sha256,
            report,
            ..
        } => {
            for entry in &report.entries {
                if source_filter_matches(derivation.source_filter, entry) {
                    tables.fold_retained(&entry.input.actions)?;
                }
            }
            file_sha256.clone()
        }
    };
    tables.flush()?;
    let initial = tables.checkpoint()?;
    let header = SmbChordTableHeader {
        source_sha256,
        derivation,
        initial,
    };
    Ok((Some(tables), Some(header)))
}

fn source_filter_matches(
    filter: SmbChordSourceFilter,
    entry: &crate::smb::archive::SmbArchiveEntryReport,
) -> bool {
    (entry.key.world, entry.key.level) == (filter.world, filter.level)
        && entry.key.progress >= filter.minimum_progress
}

fn current_chord_checkpoint(
    tables: Option<&EmpiricalStepTables<ButtonChord>>,
) -> Result<Option<EmpiricalStepCheckpoint>, Box<dyn Error>> {
    tables
        .map(EmpiricalStepTables::checkpoint)
        .transpose()
        .map_err(Into::into)
}

fn recorded_chord_tables<'a>(
    policy: SmbCampaignChordPolicy,
    before: Option<&EmpiricalStepCheckpoint>,
    versions: &'a BTreeMap<u64, EmpiricalStepTables<ButtonChord>>,
) -> Result<Option<&'a EmpiricalStepTables<ButtonChord>>, Box<dyn Error>> {
    let SmbCampaignChordPolicy::DerivedHalf(_) = policy else {
        if before.is_some() {
            return Err("non-derived chord draw carries a table version".into());
        }
        return Ok(None);
    };
    let before = before.ok_or("derived chord draw is missing its table version")?;
    let tables = versions
        .get(&before.records)
        .ok_or("derived chord draw names an unknown table version")?;
    if tables.checkpoint()? != *before {
        return Err("derived chord draw table hash does not match replay".into());
    }
    Ok(Some(tables))
}

fn remember_chord_version(
    tables: Option<&EmpiricalStepTables<ButtonChord>>,
    required: &BTreeSet<u64>,
    versions: &mut BTreeMap<u64, EmpiricalStepTables<ButtonChord>>,
) {
    if let Some(tables) = tables
        && required.contains(&tables.records())
    {
        versions.insert(tables.records(), tables.clone());
    }
}

fn finish_chord_stream_record(
    policy: SmbCampaignChordPolicy,
    tables: &mut Option<EmpiricalStepTables<ButtonChord>>,
    core: &CoordinatorCore,
    decisions: &[SmbCampaignAdmissionDecision],
) -> Result<Option<EmpiricalStepCheckpoint>, Box<dyn Error>> {
    let SmbCampaignChordPolicy::DerivedHalf(_) = policy else {
        return Ok(None);
    };
    let tables = tables
        .as_mut()
        .ok_or("derived chord policy has no folded tables")?;
    for decision in decisions {
        let SmbCampaignAdmissionDecision::Retained { id } = decision else {
            continue;
        };
        let index = usize::try_from(*id)?;
        let entry = core
            .archive
            .entries
            .get(index)
            .ok_or("retained chord-table entry is missing from the run archive")?;
        tables.fold_retained(&entry.report.input.actions)?;
    }
    Ok(tables.finish_record()?)
}

/// Seed the coordinator from the origin: genesis alone, or genesis plus the
/// whole source tree.
fn bootstrap_core(
    core: &mut CoordinatorCore,
    target: &mut SmbTarget,
    origin: &SmbCampaignOrigin,
) -> Result<Option<SmbTreeImportCounts>, Box<dyn Error>> {
    match origin {
        SmbCampaignOrigin::Archive {
            report, checkpoint, ..
        } => Ok(Some(core.import_tree(
            target,
            report,
            checkpoint.as_ref().map(|checkpoint| &checkpoint.snapshots),
        )?)),
        SmbCampaignOrigin::Genesis => {
            core.bootstrap(target)?;
            Ok(None)
        }
    }
}

/// The origin record for the stream header; its resume input is the source
/// archive's frontier identity.
fn resolve_origin(origin: &SmbCampaignOrigin) -> Result<SmbCampaignOriginRecord, Box<dyn Error>> {
    let (kind, path, archive_sha256, checkpoint, resume_input) = match origin {
        SmbCampaignOrigin::Genesis => ("genesis".to_owned(), None, None, None, SmbInput::default()),
        SmbCampaignOrigin::Archive {
            path,
            file_sha256,
            report,
            checkpoint,
        } => (
            "archive".to_owned(),
            Some(path.clone()),
            Some(file_sha256.clone()),
            checkpoint.as_ref(),
            select_frontier_resume_input(report)?,
        ),
    };
    let resume_input_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&resume_input)?));
    Ok(SmbCampaignOriginRecord {
        kind,
        path,
        archive_sha256,
        checkpoint_path: checkpoint.map(|checkpoint| checkpoint.path.clone()),
        checkpoint_sha256: checkpoint.map(|checkpoint| checkpoint.file_sha256.clone()),
        resume_input_sha256,
        resume_actions: resume_input.actions.len(),
    })
}

fn stream_header(
    config: &SmbCampaignConfig,
    origin: &SmbCampaignOriginRecord,
    chord_table: Option<SmbChordTableHeader>,
    rom: &[u8],
) -> SmbCampaignStreamHeader {
    SmbCampaignStreamHeader {
        format: CAMPAIGN_STREAM_FORMAT.to_owned(),
        campaign_seed: config.campaign_seed,
        workers: config.workers,
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
        controller_vocabulary: VOCABULARY_IDENTIFIER.to_owned(),
        key_policy: KEY_POLICY_IDENTIFIER.to_owned(),
        duration_policy: DURATION_IDENTIFIER.to_owned(),
        suffix_policy: SUFFIX_IDENTIFIER.to_owned(),
        chord_policy: chord_policy_identifier(config.chord),
        chord_table,
        replacement_policy: REPLACEMENT_IDENTIFIER.to_owned(),
        resume_policy: RESUME_IDENTIFIER.to_owned(),
        retention_policy: retention_policy_identifier(config.retention).to_owned(),
        parent_scheduler: selector_policy_identifier(config.selector),
        executor_mode: "snapshot_resume_archive".to_owned(),
        worker_seed_derivation: "sha256(campaign_seed_le || worker_index_le)[0..8] as u64 le"
            .to_owned(),
        rom_sha256: format!("{:x}", Sha256::digest(rom)),
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
    tree_import: Option<SmbTreeImportCounts>,
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

/// Write the whole retained tree beside the run so an interruption of this
/// binary loses at most one interval. Both files land under temporary names
/// first, then rename over the previous checkpoint generation.
fn write_live_checkpoint(
    core: &CoordinatorCore,
    campaign_seed: u64,
    directory: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    /// Borrowed mirror of [`SmbSnapshotCheckpointEntry`]; postcard encodes
    /// both identically, so the multi-gigabyte snapshot set is serialized
    /// without cloning it.
    #[derive(Serialize)]
    struct EntryRef<'a> {
        id: u64,
        snapshot: &'a SmbSnapshot,
    }
    /// Borrowed mirror of [`SmbSnapshotCheckpoint`].
    #[derive(Serialize)]
    struct CheckpointRef<'a> {
        format: &'a str,
        entries: Vec<EntryRef<'a>>,
    }
    let archive_tmp = directory.join("checkpoint-archive.json.tmp");
    std::fs::write(
        &archive_tmp,
        serde_json::to_vec(&core.archive_report_snapshot(campaign_seed))?,
    )?;
    let entries = core
        .archive
        .entries
        .iter()
        .map(|entry| EntryRef {
            id: entry.report.id,
            snapshot: &entry.snapshot,
        })
        .collect();
    let checkpoint = CheckpointRef {
        format: SNAPSHOT_CHECKPOINT_FORMAT,
        entries,
    };
    let snapshots_tmp = directory.join("checkpoint-snapshots.bin.tmp");
    std::fs::write(&snapshots_tmp, postcard::to_allocvec(&checkpoint)?)?;
    std::fs::rename(&archive_tmp, directory.join("checkpoint-archive.json"))?;
    std::fs::rename(&snapshots_tmp, directory.join("checkpoint-snapshots.bin"))?;
    Ok(())
}

fn build_report(
    header: &SmbCampaignStreamHeader,
    origin: SmbCampaignOriginRecord,
    core: CoordinatorCore,
    counters: &CampaignCounters,
    stream_sha256: String,
) -> (SmbCampaignModeReport, SmbSnapshotCheckpoint) {
    let checkpoint = SmbSnapshotCheckpoint {
        format: SNAPSHOT_CHECKPOINT_FORMAT.to_owned(),
        entries: core
            .archive
            .entries
            .iter()
            .map(|entry| SmbSnapshotCheckpointEntry {
                id: entry.report.id,
                snapshot: entry.snapshot.clone(),
            })
            .collect(),
    };
    let executions_completed = core.sequence;
    let probe_refused = core.probe_refused;
    let victories = core.victories;
    let victory_input = core.victory_input.clone();
    let replacement_frames_displaced = core.archive.replacement_frames_displaced();
    let archive = core.into_archive_report(header.campaign_seed);
    let report = SmbCampaignModeReport {
        mode: "campaign".to_owned(),
        campaign_seed: header.campaign_seed,
        workers: header.workers,
        host: header.host.clone(),
        schedule_identity: CAMPAIGN_SCHEDULE_IDENTITY.to_owned(),
        origin,
        execution_budget: header.execution_budget,
        executions_completed,
        wall_budget_seconds: header.wall_budget_seconds,
        action_limit: header.action_limit,
        archive_entry_limit: header.archive_entry_limit,
        controller_vocabulary: header.controller_vocabulary.clone(),
        key_policy: header.key_policy.clone(),
        duration_policy: header.duration_policy.clone(),
        suffix_policy: header.suffix_policy.clone(),
        chord_policy: header.chord_policy.clone(),
        replacement_policy: header.replacement_policy.clone(),
        resume_policy: header.resume_policy.clone(),
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

fn result_sha256(result: &SmbCampaignJobResult) -> Result<String, Box<dyn Error>> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(result)?)))
}

/// Job specification sent to a worker.
struct JobSpec {
    snapshot: SmbSnapshot,
    parent_actions: usize,
    parent_milestones: SmbMilestones,
    suffix: Vec<ButtonChord>,
}

/// What the coordinator remembers about a worker's in-flight job.
struct PendingJob {
    parent_id: u64,
    mutation_seed: u64,
    selector: SmbSelectorDraw,
    chord_table_before: Option<EmpiricalStepCheckpoint>,
}

/// Run one live campaign, writing the stream as it goes.
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
/// One periodic observation of a live run.
///
/// Written to a sidecar file so an operator can see a run advance without
/// waiting for its sentinel. It is not part of the recorded stream and takes no
/// part in replay.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbCampaignProgressRecord {
    /// Seconds since the Unix epoch when the line was written.
    pub unix_time: u64,
    /// Executions admitted so far.
    pub executions: u64,
    /// Deepest world reached so far.
    pub world: u8,
    /// Deepest level reached so far.
    pub level: u8,
    /// Deepest progress bucket reached so far.
    pub progress: u16,
    /// Fewest frames any entry at that bucket spent inside its pair.
    pub cheapest_frames_in_level: u64,
    /// Entries retained so far.
    pub retained: u64,
}

/// Seconds between sidecar observations.
const PROGRESS_INTERVAL_SECONDS: u64 = 60;

pub fn run_smb_campaign(
    rom: &[u8],
    config: &SmbCampaignConfig,
    origin: &SmbCampaignOrigin,
    stream: &mut dyn Write,
) -> Result<SmbCampaignModeReport, Box<dyn Error>> {
    run_smb_campaign_with_progress(rom, config, origin, stream, None)
}

/// Run a campaign, optionally emitting periodic progress lines to a sidecar.
///
/// The sidecar is pure observation: it reads archive state that is already
/// settled, consumes no randomness, and writes to a sink separate from the
/// recorded stream. A run with a sidecar and the same run without one record
/// byte-identical streams and archives.
///
/// # Errors
///
/// Returns an error under the same conditions as [`run_smb_campaign`], or when
/// the sidecar sink cannot be written.
pub fn run_smb_campaign_with_progress(
    rom: &[u8],
    config: &SmbCampaignConfig,
    origin: &SmbCampaignOrigin,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<SmbCampaignModeReport, Box<dyn Error>> {
    run_smb_campaign_checkpointed(rom, config, origin, stream, progress).map(|(report, _)| report)
}

/// Run a campaign, also returning every retained entry's snapshot so a later
/// whole-tree resume can restore the population instead of re-emulating it.
///
/// # Errors
///
/// Returns an error under the same conditions as
/// [`run_smb_campaign_with_progress`].
pub fn run_smb_campaign_checkpointed(
    rom: &[u8],
    config: &SmbCampaignConfig,
    origin: &SmbCampaignOrigin,
    stream: &mut dyn Write,
    mut progress: Option<&mut dyn Write>,
) -> Result<(SmbCampaignModeReport, SmbSnapshotCheckpoint), Box<dyn Error>> {
    if config.workers == 0 {
        return Err("campaign mode requires at least one worker".into());
    }
    if config.action_limit == 0
        || config.action_limit > crate::smb::archive::MAX_SMB_COMPLETION_ACTIONS
    {
        return Err("campaign action limit is outside its bounded range".into());
    }
    if config.archive_entry_limit == 0
        || config.archive_entry_limit > crate::smb::archive::MAX_ARCHIVE_ENTRIES
    {
        return Err("campaign archive entry limit is outside its bounded range".into());
    }
    let origin_record = resolve_origin(origin)?;
    let (mut chord_tables, chord_table_header) = initial_chord_tables(config.chord, origin)?;
    let header = stream_header(config, &origin_record, chord_table_header, rom);
    let mut writer = StreamWriter::new(stream);
    writer.write_line(&header)?;

    let mut core = CoordinatorCore::new(config.action_limit, config.archive_entry_limit);
    core.archive.selector_policy = config.selector;
    let mut counters = CampaignCounters::new(config.workers);
    let mut bootstrap_target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    let frames_before = bootstrap_target.frames_clocked();
    counters.tree_import = bootstrap_core(&mut core, &mut bootstrap_target, origin)?;
    counters.bootstrap_frames = bootstrap_target
        .frames_clocked()
        .saturating_sub(frames_before);
    drop(bootstrap_target);

    let workers = config.workers as usize;
    let mut rands = Vec::with_capacity(workers);
    for index in 0..config.workers {
        rands.push(StdRand::with_seed(derive_worker_seed(
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
    let mut pending: Vec<Option<PendingJob>> = Vec::new();
    pending.resize_with(workers, || None);

    let worker_policies = SmbJobPolicies {
        max_actions: config.action_limit,
        retention: config.retention,
    };
    with_worker_pool(
        config.workers,
        |_| SmbTarget::from_smb_rom_bytes_headless(rom).map_err(|error| error.to_string()),
        |target, spec: JobSpec| {
            let frames_before = target.frames_clocked();
            execute_job(
                target,
                &spec.snapshot,
                spec.parent_actions,
                spec.parent_milestones,
                &spec.suffix,
                worker_policies,
            )
            .map(|result| {
                (
                    result,
                    target.frames_clocked().saturating_sub(frames_before),
                )
            })
            .map_err(|error| error.to_string())
        },
        |pool| -> Result<(), Box<dyn Error>> {
            // Select one job for one worker, recording skips, or report exhaustion.
            let select = |core: &mut CoordinatorCore,
                          rands: &mut [StdRand],
                          chord_tables: &mut Option<EmpiricalStepTables<ButtonChord>>,
                          writer: &mut StreamWriter<'_>,
                          counters: &mut CampaignCounters,
                          reserved: &mut u64,
                          worker: u32|
             -> Result<Option<(JobSpec, PendingJob)>, Box<dyn Error>> {
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
                    let mutation_seed = rand.next();
                    let chord_table_before = current_chord_checkpoint(chord_tables.as_ref())?;
                    let suffix = derive_suffix(mutation_seed, config.chord, chord_tables.as_ref())?;
                    if consecutive_skips < CONSECUTIVE_SKIP_LIMIT
                        && core.all_prefixes_archived(parent_index, &suffix)
                    {
                        let chord_table_after =
                            finish_chord_stream_record(config.chord, chord_tables, core, &[])?;
                        writer.write_line(&SmbCampaignStreamRecord::Skip(
                            SmbCampaignSkipRecord {
                                worker,
                                parent_id: u64::try_from(parent_index)?,
                                mutation_seed,
                                selector,
                                chord_table_before,
                                chord_table_after,
                            },
                        ))?;
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
                            snapshot: entry.snapshot.clone(),
                            parent_actions: entry.report.input.actions.len(),
                            parent_milestones: entry.report.milestones,
                            suffix,
                        },
                        PendingJob {
                            parent_id: u64::try_from(parent_index)?,
                            mutation_seed,
                            selector,
                            chord_table_before,
                        },
                    )));
                }
            };

            let mut in_flight = 0_usize;
            for worker in 0..config.workers {
                match select(
                    &mut core,
                    &mut rands,
                    &mut chord_tables,
                    &mut writer,
                    &mut counters,
                    &mut reserved,
                    worker,
                )? {
                    Some((spec, pending_job)) => {
                        pending[worker as usize] = Some(pending_job);
                        pool.send(worker, spec)?;
                        in_flight += 1;
                    }
                    None => {
                        pool.close(worker)?;
                    }
                }
            }

            while in_flight > 0 {
                let reply = pool.receive()?;
                let worker_index = reply.worker as usize;
                let (result, frames) = reply.outcome.map_err(|error| -> Box<dyn Error> {
                    format!("campaign worker {} failed: {error}", reply.worker).into()
                })?;
                let pending_job = pending[worker_index]
                    .take()
                    .ok_or("campaign worker replied without a pending job")?;
                let victories_before = core.victories;
                let (sequence, decisions) = core.admit_job(pending_job.parent_id, &result)?;
                let parent_index = usize::try_from(pending_job.parent_id)?;
                core.archive
                    .record_selection(parent_index, &pending_job.selector);
                core.archive.record_selection_outcome(
                    parent_index,
                    decisions.iter().any(|decision| {
                        matches!(decision, SmbCampaignAdmissionDecision::Retained { .. })
                    }),
                );
                if victories_before == 0
                    && let (Some(path), Some(input)) =
                        (&config.victory_input_path, &core.victory_input)
                {
                    std::fs::write(path, serde_json::to_vec_pretty(input)?)?;
                }
                let chord_table_after =
                    finish_chord_stream_record(config.chord, &mut chord_tables, &core, &decisions)?;
                writer.write_line(&SmbCampaignStreamRecord::Job(SmbCampaignJobRecord {
                    sequence,
                    worker: reply.worker,
                    parent_id: pending_job.parent_id,
                    mutation_seed: pending_job.mutation_seed,
                    frames,
                    result_sha256: result_sha256(&result)?,
                    decisions,
                    selector: pending_job.selector,
                    chord_table_before: pending_job.chord_table_before,
                    chord_table_after,
                }))?;
                counters.jobs_per_worker[worker_index] =
                    counters.jobs_per_worker[worker_index].saturating_add(1);
                counters.job_frames = counters.job_frames.saturating_add(frames);
                in_flight -= 1;
                if let Some(directory) = &config.checkpoint_dir
                    && sequence > 0
                    && sequence.is_multiple_of(LIVE_CHECKPOINT_INTERVAL)
                {
                    write_live_checkpoint(&core, config.campaign_seed, directory)?;
                }
                if let Some(sink) = progress.as_deref_mut() {
                    // Wall-clock gates the sidecar only. It selects nothing and
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
                        let (world, level, bucket, cheapest, retained) =
                            core.archive.live_progress();
                        let line = serde_json::to_string(&SmbCampaignProgressRecord {
                            unix_time,
                            executions: sequence,
                            world,
                            level,
                            progress: bucket,
                            cheapest_frames_in_level: cheapest,
                            retained,
                        })?;
                        sink.write_all(line.as_bytes())?;
                        sink.write_all(b"\n")?;
                        sink.flush()?;
                    }
                }
                match select(
                    &mut core,
                    &mut rands,
                    &mut chord_tables,
                    &mut writer,
                    &mut counters,
                    &mut reserved,
                    reply.worker,
                )? {
                    Some((spec, pending_job)) => {
                        pending[worker_index] = Some(pending_job);
                        pool.send(reply.worker, spec)?;
                        in_flight += 1;
                    }
                    None => {
                        pool.close(reply.worker)?;
                    }
                }
            }
            Ok(())
        },
    )?;

    core.finish_curve();
    let stream_sha256 = writer.finish()?;
    Ok(build_report(
        &header,
        origin_record,
        core,
        &counters,
        stream_sha256,
    ))
}

/// Replay a recorded campaign stream serially and rebuild its report.
///
/// Replay re-executes every recorded job from (parent id, mutation seed) on a
/// single target, verifies each result digest and frame count byte for byte,
/// re-applies the retention rules, and verifies every recomputed
/// admission decision against the recorded one. Any mismatch is an error.
///
/// # Errors
///
/// Returns an error when the stream is malformed, the origin does not match
/// the header, or any recomputed value differs from the recorded one.
pub fn replay_smb_campaign(
    rom: &[u8],
    stream_bytes: &[u8],
    origin_report: Option<&SmbArchiveReport>,
) -> Result<SmbCampaignModeReport, Box<dyn Error>> {
    replay_smb_campaign_checkpointed(rom, stream_bytes, origin_report, None)
        .map(|(report, _)| report)
}

/// Replay a recorded campaign, also returning the rebuilt snapshot checkpoint.
///
/// When the recorded header names a snapshot checkpoint, `origin_checkpoint`
/// must be that file and its hash must match; a replay without it re-emulates
/// the import and must still reach the same archive.
///
/// # Errors
///
/// Returns an error under the same conditions as [`replay_smb_campaign`], or
/// when the supplied checkpoint is not the recorded one.
pub fn replay_smb_campaign_checkpointed(
    rom: &[u8],
    stream_bytes: &[u8],
    origin_report: Option<&SmbArchiveReport>,
    origin_checkpoint: Option<&SmbCampaignCheckpoint>,
) -> Result<(SmbCampaignModeReport, SmbSnapshotCheckpoint), Box<dyn Error>> {
    let stream_sha256 = format!("{:x}", Sha256::digest(stream_bytes));
    let text = std::str::from_utf8(stream_bytes)?;
    let mut lines = text.lines();
    let header: SmbCampaignStreamHeader =
        serde_json::from_str(lines.next().ok_or("campaign stream is empty")?)?;
    let record_lines = lines.collect::<Vec<_>>();
    let mut required_chord_versions = BTreeSet::new();
    for line in &record_lines {
        let record: SmbCampaignStreamRecord = serde_json::from_str(line)?;
        let before = match record {
            SmbCampaignStreamRecord::Job(job) => job.chord_table_before,
            SmbCampaignStreamRecord::Skip(skip) => skip.chord_table_before,
        };
        if let Some(before) = before {
            required_chord_versions.insert(before.records);
        }
    }
    if header.format != CAMPAIGN_STREAM_FORMAT {
        return Err("campaign stream format is not recognized".into());
    }
    if header.rom_sha256 != format!("{:x}", Sha256::digest(rom)) {
        return Err("campaign replay ROM does not match the recorded stream".into());
    }
    let resume_input = match header.origin_kind.as_str() {
        "genesis" => {
            if origin_report.is_some() {
                return Err("genesis campaign replay does not take a source archive".into());
            }
            SmbInput::default()
        }
        "archive" => {
            let source =
                origin_report.ok_or("archive campaign replay requires the source archive")?;
            select_frontier_resume_input(source)?
        }
        _ => return Err("campaign stream origin kind is not recognized".into()),
    };
    let resume_input_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&resume_input)?));
    if resume_input_sha256 != header.resume_input_sha256
        || resume_input.actions.len() != header.resume_actions
    {
        return Err("campaign replay resume input does not match the recorded stream".into());
    }

    let (replay_retention, replay_selector) = verify_fixed_rules(&header)?;
    let replay_chord_policy = chord_policy_from_identifier(&header.chord_policy)?;
    let chord_origin = match origin_report {
        Some(report) => SmbCampaignOrigin::Archive {
            path: header.origin_path.clone().unwrap_or_default(),
            file_sha256: header.origin_archive_sha256.clone().unwrap_or_default(),
            report: Box::new(report.clone()),
            checkpoint: origin_checkpoint.cloned(),
        },
        None => SmbCampaignOrigin::Genesis,
    };
    if let Some(checkpoint) = origin_checkpoint
        && header.origin_checkpoint_sha256.as_deref() != Some(checkpoint.file_sha256.as_str())
    {
        return Err("campaign replay checkpoint does not match the recorded stream".into());
    }
    let (mut chord_tables, replay_chord_header) =
        initial_chord_tables(replay_chord_policy, &chord_origin)?;
    if replay_chord_header != header.chord_table {
        return Err("re-derived chord table does not match the recorded header".into());
    }
    let mut chord_versions = BTreeMap::new();
    remember_chord_version(
        chord_tables.as_ref(),
        &required_chord_versions,
        &mut chord_versions,
    );
    let mut core = CoordinatorCore::new(header.action_limit, header.archive_entry_limit);
    core.archive.selector_policy = replay_selector;
    let mut counters = CampaignCounters::new(header.workers);
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    let frames_before = target.frames_clocked();
    counters.tree_import = bootstrap_core(&mut core, &mut target, &chord_origin)?;
    counters.bootstrap_frames = target.frames_clocked().saturating_sub(frames_before);

    for line in record_lines {
        let record: SmbCampaignStreamRecord = serde_json::from_str(line)?;
        match record {
            SmbCampaignStreamRecord::Skip(skip) => {
                let parent_index = usize::try_from(skip.parent_id)?;
                if parent_index >= core.archive.entries.len() {
                    return Err("recorded skip names a parent the archive does not hold".into());
                }
                let draw_tables = recorded_chord_tables(
                    replay_chord_policy,
                    skip.chord_table_before.as_ref(),
                    &chord_versions,
                )?;
                let suffix = derive_suffix(skip.mutation_seed, replay_chord_policy, draw_tables)?;
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
                let chord_table_after =
                    finish_chord_stream_record(replay_chord_policy, &mut chord_tables, &core, &[])?;
                if chord_table_after != skip.chord_table_after {
                    return Err("replayed skip chord-table checkpoint diverged".into());
                }
                remember_chord_version(
                    chord_tables.as_ref(),
                    &required_chord_versions,
                    &mut chord_versions,
                );
            }
            SmbCampaignStreamRecord::Job(job) => {
                let parent_index = usize::try_from(job.parent_id)?;
                let entry = core
                    .archive
                    .entries
                    .get(parent_index)
                    .ok_or("recorded job names a parent the archive does not hold")?;
                let snapshot = entry.snapshot.clone();
                let parent_actions = entry.report.input.actions.len();
                let parent_milestones = entry.report.milestones;
                let draw_tables = recorded_chord_tables(
                    replay_chord_policy,
                    job.chord_table_before.as_ref(),
                    &chord_versions,
                )?;
                let suffix = derive_suffix(job.mutation_seed, replay_chord_policy, draw_tables)?;
                let job_frames_before = target.frames_clocked();
                let result = execute_job(
                    &mut target,
                    &snapshot,
                    parent_actions,
                    parent_milestones,
                    &suffix,
                    SmbJobPolicies {
                        max_actions: header.action_limit,
                        retention: replay_retention,
                    },
                )?;
                let frames = target.frames_clocked().saturating_sub(job_frames_before);
                if frames != job.frames {
                    return Err(format!(
                        "replayed job {} emulated {frames} frames against recorded {}",
                        job.sequence, job.frames
                    )
                    .into());
                }
                let digest = result_sha256(&result)?;
                if digest != job.result_sha256 {
                    return Err(format!(
                        "replayed job {} result digest diverged from the recorded stream",
                        job.sequence
                    )
                    .into());
                }
                let (sequence, decisions) = core.admit_job(job.parent_id, &result)?;
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
                let chord_table_after = finish_chord_stream_record(
                    replay_chord_policy,
                    &mut chord_tables,
                    &core,
                    &decisions,
                )?;
                if chord_table_after != job.chord_table_after {
                    return Err(format!(
                        "replayed job {} chord-table checkpoint diverged",
                        job.sequence
                    )
                    .into());
                }
                remember_chord_version(
                    chord_tables.as_ref(),
                    &required_chord_versions,
                    &mut chord_versions,
                );
                verify_selector_annotation(&job.selector)?;
                core.archive.record_selection(parent_index, &job.selector);
                core.archive.record_selection_outcome(
                    parent_index,
                    decisions.iter().any(|decision| {
                        matches!(decision, SmbCampaignAdmissionDecision::Retained { .. })
                    }),
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

    let origin = SmbCampaignOriginRecord {
        kind: header.origin_kind.clone(),
        path: header.origin_path.clone(),
        archive_sha256: header.origin_archive_sha256.clone(),
        checkpoint_path: header.origin_checkpoint_path.clone(),
        checkpoint_sha256: header.origin_checkpoint_sha256.clone(),
        resume_input_sha256: header.resume_input_sha256.clone(),
        resume_actions: header.resume_actions,
    };
    Ok(build_report(
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
        CoordinatorCore, SNAPSHOT_CHECKPOINT_FORMAT, SmbCampaignActionResult,
        SmbCampaignAdmissionDecision, SmbCampaignChordPolicy, SmbCampaignConfig,
        SmbCampaignJobResult, SmbCampaignOrigin, SmbCampaignStreamRecord, SmbSnapshotCheckpoint,
        SmbSnapshotCheckpointEntry, chord_policy_from_identifier, chord_policy_identifier,
        derive_suffix, derive_worker_seed, execute_job, replay_smb_campaign, run_smb_campaign,
        run_smb_campaign_with_progress, write_live_checkpoint,
    };
    use crate::{
        search::empirical_steps::EmpiricalStepParameters,
        smb::archive::{SmbArchiveEntryReport, SmbArchiveKey, SmbArchiveReport},
        smb::target::{ButtonChord, SmbInput, SmbMilestones, SmbTarget},
        target::Target,
    };

    fn synthetic_nrom() -> Vec<u8> {
        let mut rom = vec![0_u8; 16 + (16 * 1024) + (8 * 1024)];
        rom[..16].copy_from_slice(&[b'N', b'E', b'S', 0x1a, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let prg = &mut rom[16..16 + (16 * 1024)];
        prg.fill(0xea);
        prg[..3].copy_from_slice(&[0x4c, 0x00, 0x80]);
        for vector in [0x3ffa, 0x3ffc, 0x3ffe] {
            prg[vector..vector + 2].copy_from_slice(&0x8000_u16.to_le_bytes());
        }
        rom
    }

    fn genesis_config(
        campaign_seed: u64,
        workers: u32,
        execution_budget: u64,
    ) -> SmbCampaignConfig {
        SmbCampaignConfig {
            campaign_seed,
            workers,
            execution_budget,
            action_limit: 96,
            host: "unit-test".to_owned(),
            wall_budget: None,
            archive_entry_limit: 32_768,
            chord: SmbCampaignChordPolicy::Uniform,
            retention: super::SmbRetentionPolicy::ProbeAtAdmission45,
            selector: super::SmbSelectorPolicy::RoomCellUniform128,
            victory_input_path: None,
            checkpoint_dir: None,
        }
    }

    #[test]
    fn worker_seed_derivation_is_stable() {
        let seeds = (0..3)
            .map(|index| derive_worker_seed(0x5eed_ca00, index).expect("derive worker seed"))
            .collect::<Vec<_>>();
        assert_eq!(seeds.len(), 3);
        assert_ne!(seeds[0], seeds[1]);
        assert_ne!(seeds[1], seeds[2]);
        let again = derive_worker_seed(0x5eed_ca00, 0).expect("derive worker seed again");
        assert_eq!(seeds[0], again);
    }

    #[test]
    fn suffix_derivation_is_pure_and_bounded() {
        for seed in [0_u64, 0x5eed_ca01, u64::MAX] {
            let first =
                derive_suffix(seed, SmbCampaignChordPolicy::Uniform, None).expect("derive suffix");
            let second = derive_suffix(seed, SmbCampaignChordPolicy::Uniform, None)
                .expect("derive suffix again");
            assert_eq!(first, second);
            assert!((1..=2).contains(&first.len()));
            assert!(
                first
                    .iter()
                    .all(|chord| (2..=12).contains(&chord.hold_frames)
                        || (96..=120).contains(&chord.hold_frames))
            );
        }
    }

    #[test]
    fn the_recorded_chord_draw_changes_the_suffix() {
        let differing = (0..64_u64)
            .filter(|seed| {
                derive_suffix(*seed, SmbCampaignChordPolicy::Uniform, None).expect("uniform")
                    != derive_suffix(*seed, SmbCampaignChordPolicy::RecordedHalf, None)
                        .expect("recorded")
            })
            .count();
        assert!(differing > 0, "no draw came from the recorded table");
    }

    fn derived_policy() -> SmbCampaignChordPolicy {
        SmbCampaignChordPolicy::DerivedHalf(super::SmbChordTableDerivation {
            source_filter: super::SmbChordSourceFilter {
                world: 0,
                level: 0,
                minimum_progress: 0,
            },
            parameters: EmpiricalStepParameters {
                prefix_steps: 0,
                recent_successes: 4,
                recent_weight: 3,
                all_history_weight: 1,
                update_every_records: 2,
                hash_every_records: 2,
            },
        })
    }

    #[test]
    fn derived_chord_policy_identifier_round_trips() {
        let policy = derived_policy();
        let identifier = chord_policy_identifier(policy);
        assert_eq!(
            chord_policy_from_identifier(&identifier).expect("parse derived policy"),
            policy
        );
        assert_eq!(
            chord_policy_identifier(SmbCampaignChordPolicy::RecordedHalf),
            "chord_draw_recorded_50"
        );
        assert_eq!(
            chord_policy_from_identifier("chord_draw_recorded_50").expect("parse recorded policy"),
            SmbCampaignChordPolicy::RecordedHalf
        );
        assert!(chord_policy_from_identifier("chord_draw_recorded_50:0").is_err());
    }

    #[test]
    fn job_execution_is_pure_across_target_instances() {
        let rom = synthetic_nrom();
        let mut first = SmbTarget::from_smb_rom_bytes_headless(&rom).expect("load first target");
        let mut second = SmbTarget::from_smb_rom_bytes_headless(&rom).expect("load second target");
        first.reset();
        first.apply(&ButtonChord::new(0x81, 12));
        let snapshot = first.snapshot().expect("snapshot prefix");
        let suffix = derive_suffix(0x5eed_ca02, SmbCampaignChordPolicy::Uniform, None)
            .expect("derive suffix");
        // Disturb the first instance so the job must depend on the snapshot alone.
        first.apply(&ButtonChord::new(0x02, 30));
        let policies = super::SmbJobPolicies {
            max_actions: 96,
            retention: super::SmbRetentionPolicy::ProbeAtAdmission45,
        };
        let on_first = execute_job(
            &mut first,
            &snapshot,
            1,
            SmbMilestones::default(),
            &suffix,
            policies,
        )
        .expect("execute job on first instance");
        let on_second = execute_job(
            &mut second,
            &snapshot,
            1,
            SmbMilestones::default(),
            &suffix,
            policies,
        )
        .expect("execute job on second instance");
        assert_eq!(on_first, on_second);
    }

    #[test]
    fn a_job_from_a_won_snapshot_executes_nothing() {
        let rom = synthetic_nrom();
        let mut target = SmbTarget::from_smb_rom_bytes_headless(&rom).expect("load target");
        target.reset();
        target.wram_mut()[0x0770] = 2;
        target.wram_mut()[0x075f] = 7;
        let won = target.snapshot().expect("snapshot won state");
        let result = execute_job(
            &mut target,
            &won,
            0,
            SmbMilestones::default(),
            &[ButtonChord::new(0x01, 4)],
            super::SmbJobPolicies {
                max_actions: 96,
                retention: super::SmbRetentionPolicy::ProbeAtAdmission45,
            },
        )
        .expect("execute job");
        assert!(result.actions.is_empty());
    }

    #[test]
    fn admission_counts_a_victory_and_keeps_the_first_winning_input() {
        let rom = synthetic_nrom();
        let mut target = SmbTarget::from_smb_rom_bytes_headless(&rom).expect("load target");
        let mut core = CoordinatorCore::new(96, 32_768);
        core.bootstrap(&mut target).expect("retain genesis");
        let winning = ButtonChord::new(0x81, 7);
        let result = SmbCampaignJobResult {
            actions: vec![SmbCampaignActionResult {
                action: winning,
                observations: Vec::new(),
                milestones: SmbMilestones::default(),
                dead: false,
                victory: true,
                failed: false,
                candidate: None,
            }],
        };
        let (sequence, decisions) = core.admit_job(0, &result).expect("admit winning job");
        assert_eq!(sequence, 1);
        assert_eq!(decisions, vec![SmbCampaignAdmissionDecision::Victory]);
        assert_eq!(core.victories, 1);
        assert_eq!(
            core.victory_input,
            Some(SmbInput {
                actions: vec![winning]
            })
        );
        let later = SmbCampaignJobResult {
            actions: vec![SmbCampaignActionResult {
                action: ButtonChord::new(0x01, 9),
                ..result.actions[0].clone()
            }],
        };
        core.admit_job(0, &later)
            .expect("admit a second winning job");
        assert_eq!(core.victories, 2);
        assert_eq!(
            core.victory_input,
            Some(SmbInput {
                actions: vec![winning]
            })
        );
        let report = core.into_archive_report(0);
        assert_eq!(report.entries.len(), 1, "a won lineage is not extended");
    }

    #[test]
    fn live_checkpoint_files_round_trip() {
        let rom = synthetic_nrom();
        let mut core = CoordinatorCore::new(96, 32_768);
        let mut target = SmbTarget::from_smb_rom_bytes_headless(&rom).expect("load target");
        core.bootstrap(&mut target).expect("bootstrap genesis");
        let directory =
            std::env::temp_dir().join(format!("smb-live-checkpoint-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create checkpoint directory");
        write_live_checkpoint(&core, 7, &directory).expect("write live checkpoint");
        let report: SmbArchiveReport = serde_json::from_slice(
            &std::fs::read(directory.join("checkpoint-archive.json")).expect("read archive"),
        )
        .expect("parse archive report");
        assert_eq!(report.entries.len(), core.archive.entries.len());
        let decoded = SmbSnapshotCheckpoint::from_bytes(
            &std::fs::read(directory.join("checkpoint-snapshots.bin")).expect("read snapshots"),
        )
        .expect("decode snapshot checkpoint");
        let owned = SmbSnapshotCheckpoint {
            format: SNAPSHOT_CHECKPOINT_FORMAT.to_owned(),
            entries: core
                .archive
                .entries
                .iter()
                .map(|entry| SmbSnapshotCheckpointEntry {
                    id: entry.report.id,
                    snapshot: entry.snapshot.clone(),
                })
                .collect(),
        };
        assert_eq!(decoded, owned, "borrowed encoding matches the owned one");
        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn live_campaign_replays_byte_identically() {
        let rom = synthetic_nrom();
        let config = genesis_config(0x5eed_ca03, 4, 32);
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("live campaign");
        assert_eq!(live.executions_completed, 32);
        assert_eq!(live.jobs_per_worker.iter().sum::<u64>(), 32);
        assert_eq!(live.victories, 0);
        assert_eq!(live.victory_input, None);
        let text = String::from_utf8(stream.clone()).expect("stream is utf-8");
        let header = text.lines().next().expect("header");
        for identifier in [
            "room_cell_uniform_128",
            "probe_at_admission_45",
            "fewest_frames_in_level",
            "whole_tree",
            "down_ten_mask",
            "frozen_area_span",
            "one_or_two",
            "stratified",
        ] {
            assert!(header.contains(identifier), "header lacks {identifier}");
        }
        for line in text.lines().skip(1) {
            assert!(line.contains("\"selector\""));
            assert_eq!(
                line.contains("\"room_cell_uniform\""),
                line.contains("\"concentration\"")
            );
        }
        let replayed = replay_smb_campaign(&rom, &stream, None).expect("replay recorded campaign");
        assert_eq!(live, replayed);
        let live_bytes = serde_json::to_vec_pretty(&live).expect("serialize live report");
        let replay_bytes = serde_json::to_vec_pretty(&replayed).expect("serialize replayed report");
        assert_eq!(live_bytes, replay_bytes);
        let accounting = live.archive.selector;
        assert_eq!(
            accounting
                .uniform_selections
                .checked_add(accounting.cell_selections),
            live.executions_completed
                .checked_add(live.duplicates_skipped)
        );
        assert_eq!(accounting.concentration.window_cap, 128);
        assert_eq!(
            accounting.concentration.window_draws,
            accounting.cell_selections
        );
    }

    #[test]
    fn admit_alive_campaign_probes_nothing_and_replays_byte_identically() {
        let rom = synthetic_nrom();
        let probing = genesis_config(0x5eed_ca20, 4, 32);
        let mut probing_stream = Vec::new();
        let probed = run_smb_campaign(
            &rom,
            &probing,
            &SmbCampaignOrigin::Genesis,
            &mut probing_stream,
        )
        .expect("probing campaign");
        let mut config = genesis_config(0x5eed_ca20, 4, 32);
        config.retention = crate::smb::archive::SmbRetentionPolicy::AdmitAlive;
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("admit-alive campaign");
        let text = String::from_utf8(stream.clone()).expect("stream is utf-8");
        let header = text.lines().next().expect("header");
        assert!(header.contains("\"retention_policy\":\"admit_alive\""));
        assert_eq!(live.probe_refused, 0);
        assert!(
            live.frames_emulated < probed.frames_emulated,
            "skipping the probe must emulate fewer frames: {} against {}",
            live.frames_emulated,
            probed.frames_emulated
        );
        let replayed = replay_smb_campaign(&rom, &stream, None).expect("replay admit-alive");
        assert_eq!(
            serde_json::to_vec_pretty(&live).expect("serialize live"),
            serde_json::to_vec_pretty(&replayed).expect("serialize replayed")
        );
    }

    #[test]
    fn retiring_selector_records_counters_and_replays_byte_identically() {
        let rom = synthetic_nrom();
        let mut config = genesis_config(0x5eed_ca21, 4, 48);
        config.selector = crate::smb::archive::SmbSelectorPolicy::Retire(
            crate::smb::archive::SmbRetireThresholds {
                entry: 2,
                cell: 4,
                band: 8,
                room: 16,
            },
        );
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("retiring campaign");
        let text = String::from_utf8(stream.clone()).expect("stream is utf-8");
        let header = text.lines().next().expect("header");
        assert!(header.contains("room_cell_uniform_128_retire:2,4,8,16"));
        assert!(live.archive.selector.retirement.is_some());
        let replayed = replay_smb_campaign(&rom, &stream, None).expect("replay retiring");
        assert_eq!(
            serde_json::to_vec_pretty(&live).expect("serialize live"),
            serde_json::to_vec_pretty(&replayed).expect("serialize replayed")
        );
    }

    #[test]
    fn retention_and_selector_identifiers_round_trip() {
        use crate::smb::archive::{
            SmbRetentionPolicy, SmbRetireThresholds, SmbSelectorPolicy,
            retention_policy_from_identifier, retention_policy_identifier,
            selector_policy_from_identifier, selector_policy_identifier,
        };
        for policy in [
            SmbRetentionPolicy::ProbeAtAdmission45,
            SmbRetentionPolicy::AdmitAlive,
        ] {
            assert_eq!(
                retention_policy_from_identifier(retention_policy_identifier(policy))
                    .expect("retention round trip"),
                policy
            );
        }
        for policy in [
            SmbSelectorPolicy::RoomCellUniform128,
            SmbSelectorPolicy::Retire(SmbRetireThresholds {
                entry: 3,
                cell: 6,
                band: 12,
                room: 2,
            }),
        ] {
            assert_eq!(
                selector_policy_from_identifier(&selector_policy_identifier(policy))
                    .expect("selector round trip"),
                policy
            );
        }
        assert!(retention_policy_from_identifier("no_probe").is_err());
        assert!(selector_policy_from_identifier("room_cell_uniform_128_retire:3,6,12").is_err());
        assert!(selector_policy_from_identifier("room_cell_uniform_128_retire:3,6,12,0").is_err());
    }

    #[test]
    fn a_stream_recorded_under_another_rule_is_refused() {
        let rom = synthetic_nrom();
        let config = genesis_config(0x5eed_ca10, 1, 4);
        let mut stream = Vec::new();
        run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("live campaign");
        let text = String::from_utf8(stream).expect("stream is utf-8");
        for (from, to) in [
            ("room_cell_uniform_128", "concentrated_recency_128"),
            ("probe_at_admission_45", "probe_at_admission_45_snapback_16"),
            ("fewest_frames_in_level", "fewest_actions"),
            ("\"whole_tree\"", "\"frontier_shortest\""),
            ("down_ten_mask", "frozen_nine_mask"),
        ] {
            let tampered = text.replacen(from, to, 1);
            assert!(
                replay_smb_campaign(&rom, tampered.as_bytes(), None).is_err(),
                "replay accepted {to}"
            );
        }
    }

    #[test]
    fn continuous_chord_tables_replay_with_recorded_versions() {
        let rom = synthetic_nrom();
        let config = SmbCampaignConfig {
            chord: derived_policy(),
            ..genesis_config(0x5eed_ca13, 3, 12)
        };
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("continuous chord-table campaign");
        let text = std::str::from_utf8(&stream).expect("stream text");
        assert!(
            text.lines()
                .next()
                .expect("header")
                .contains("\"chord_table\"")
        );
        assert!(text.contains("\"chord_table_before\""));
        assert!(text.contains("\"chord_table_after\""));
        let retained_successes = text
            .lines()
            .skip(1)
            .map(|line| {
                serde_json::from_str::<SmbCampaignStreamRecord>(line)
                    .expect("parse campaign record")
            })
            .filter_map(|record| match record {
                SmbCampaignStreamRecord::Job(job) => job.chord_table_before,
                SmbCampaignStreamRecord::Skip(skip) => skip.chord_table_before,
            })
            .map(|checkpoint| checkpoint.retained_successes)
            .max()
            .unwrap_or(0);
        assert!(
            retained_successes > 0,
            "from-scratch continuous tables must grow from retained successes"
        );
        let replayed =
            replay_smb_campaign(&rom, &stream, None).expect("replay continuous chord tables");
        assert_eq!(live, replayed);
    }

    #[test]
    fn duplicate_check_requires_every_boundary() {
        let rom = synthetic_nrom();
        let config = genesis_config(0x5eed_ca04, 2, 16);
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("live campaign");
        // Tampering with a recorded skip must fail replay loudly rather than
        // silently reproducing the counters.
        let text = String::from_utf8(stream.clone()).expect("stream is utf-8");
        if let Some(skip_line) = text.lines().find(|line| line.contains("\"skip\"")) {
            let tampered_line = skip_line.replace("\"mutation_seed\":", "\"mutation_seed\":9");
            let tampered = text.replace(skip_line, &tampered_line);
            let outcome = replay_smb_campaign(&rom, tampered.as_bytes(), None);
            assert!(outcome.is_err());
        }
        assert_eq!(
            live.duplicates_skipped,
            live.skips_per_worker.iter().sum::<u64>()
        );
    }

    #[test]
    fn archive_origin_round_trips_through_replay() {
        use super::{
            SmbCampaignCheckpoint, SmbSnapshotCheckpoint, replay_smb_campaign_checkpointed,
            run_smb_campaign_checkpointed,
        };
        use sha2::{Digest, Sha256};
        let rom = synthetic_nrom();
        let seed_config = genesis_config(0x5eed_ca05, 2, 12);
        let mut seed_stream = Vec::new();
        let (seed_campaign, seed_checkpoint) = run_smb_campaign_checkpointed(
            &rom,
            &seed_config,
            &SmbCampaignOrigin::Genesis,
            &mut seed_stream,
            None,
        )
        .expect("seed campaign");
        let source = seed_campaign.archive.clone();
        let source_sha = "0000000000000000000000000000000000000000000000000000000000000000";
        // The re-emulated and checkpoint-restored campaigns below are two
        // independent live runs whose archives are compared for equality;
        // multi-worker draws are schedule-dependent, so one worker keeps
        // the comparison sound.
        let tree_config = genesis_config(0x5eed_ca06, 1, 16);
        let mut tree_stream = Vec::new();
        let tree_live = run_smb_campaign(
            &rom,
            &tree_config,
            &SmbCampaignOrigin::Archive {
                path: "seed-archive.json".to_owned(),
                file_sha256: source_sha.to_owned(),
                report: Box::new(source.clone()),
                checkpoint: None,
            },
            &mut tree_stream,
        )
        .expect("whole-tree campaign");
        let tree_replayed = replay_smb_campaign(&rom, &tree_stream, Some(&source))
            .expect("replay whole-tree campaign");
        assert_eq!(tree_live, tree_replayed);
        assert_eq!(tree_live.origin.kind, "archive");
        assert_eq!(tree_live.resume_policy, "whole_tree");
        let counts = tree_live.tree_import.expect("tree import counts");
        let source_retained = u64::try_from(source.entries.len() - 1).expect("count");
        assert_eq!(
            counts.imported
                + counts.duplicate
                + counts.rejected
                + counts.terminal
                + counts.over_limit,
            source_retained
        );
        assert!(counts.imported >= 1);
        assert_eq!(counts.rerooted, 0);
        assert_eq!(counts.checkpointed, 0);

        // Restoring the source population from its snapshot checkpoint
        // reaches the same archive as re-emulating it, records the
        // checkpoint in the header, and replays with or without the file.
        let checkpoint_bytes = seed_checkpoint.to_bytes().expect("encode checkpoint");
        let checkpoint = SmbCampaignCheckpoint {
            path: "seed-snapshots.bin".to_owned(),
            file_sha256: format!("{:x}", Sha256::digest(&checkpoint_bytes)),
            snapshots: SmbSnapshotCheckpoint::from_bytes(&checkpoint_bytes)
                .expect("decode checkpoint"),
        };
        assert_eq!(checkpoint.snapshots.entries.len(), source.entries.len());
        let mut restored_stream = Vec::new();
        let (restored_live, restored_checkpoint) = run_smb_campaign_checkpointed(
            &rom,
            &tree_config,
            &SmbCampaignOrigin::Archive {
                path: "seed-archive.json".to_owned(),
                file_sha256: source_sha.to_owned(),
                report: Box::new(source.clone()),
                checkpoint: Some(checkpoint.clone()),
            },
            &mut restored_stream,
            None,
        )
        .expect("checkpoint-restored campaign");
        assert_eq!(restored_live.archive, tree_live.archive);
        assert_eq!(restored_live.bootstrap_frames, 0);
        assert_eq!(
            restored_live.origin.checkpoint_sha256.as_deref(),
            Some(checkpoint.file_sha256.as_str())
        );
        let restored_counts = restored_live.tree_import.expect("restored counts");
        assert_eq!(restored_counts.checkpointed, source_retained);
        assert_eq!(
            (
                restored_counts.imported,
                restored_counts.duplicate,
                restored_counts.rejected
            ),
            (counts.imported, counts.duplicate, counts.rejected)
        );
        let (replayed_with, replayed_checkpoint) = replay_smb_campaign_checkpointed(
            &rom,
            &restored_stream,
            Some(&source),
            Some(&checkpoint),
        )
        .expect("replay with checkpoint");
        assert_eq!(replayed_with, restored_live);
        assert_eq!(replayed_checkpoint, restored_checkpoint);
        let (replayed_without, _) =
            replay_smb_campaign_checkpointed(&rom, &restored_stream, Some(&source), None)
                .expect("replay without checkpoint");
        assert_eq!(replayed_without.archive, restored_live.archive);
        let wrong = SmbCampaignCheckpoint {
            file_sha256: "00".to_owned(),
            ..checkpoint.clone()
        };
        assert!(
            replay_smb_campaign_checkpointed(&rom, &restored_stream, Some(&source), Some(&wrong))
                .is_err()
        );
        let imported_inputs: std::collections::BTreeSet<_> = tree_live
            .archive
            .entries
            .iter()
            .filter(|entry| entry.created_execution == 0)
            .map(|entry| entry.input.clone())
            .collect();
        let source_inputs: std::collections::BTreeSet<_> = source
            .entries
            .iter()
            .map(|entry| entry.input.clone())
            .collect();
        assert!(imported_inputs.is_subset(&source_inputs));
        assert!(imported_inputs.len() > 1);
    }

    #[test]
    fn whole_tree_import_rebuilds_the_source_population() {
        let rom = synthetic_nrom();
        let mut target = SmbTarget::from_smb_rom_bytes_headless(&rom).expect("target");
        let chord = |mask: u8| ButtonChord::new(mask, 4);
        let entry =
            |id: u64, parent: Option<u64>, actions: Vec<ButtonChord>| SmbArchiveEntryReport {
                id,
                parent_id: parent,
                created_execution: 0,
                input: SmbInput { actions },
                key: SmbArchiveKey {
                    world: 0,
                    level: 0,
                    progress: 0,
                    player_y_bucket: 0,
                    player_engine_state: 0,
                    state_fingerprint: 0,
                    room_x_bucket: 0,
                    room: [0; 3],
                },
                milestones: SmbMilestones::default(),
                selector: None,
            };
        // A trunk of two, a branch off the trunk, a child whose recorded
        // parent is absent from the report, and one past the action limit.
        let entries = vec![
            entry(0, None, Vec::new()),
            entry(1, Some(0), vec![chord(0x01)]),
            entry(2, Some(1), vec![chord(0x01), chord(0x02)]),
            entry(3, Some(1), vec![chord(0x01), chord(0x80)]),
            entry(9, Some(7), vec![chord(0x01), chord(0x02), chord(0x40)]),
            entry(10, Some(2), vec![chord(0x01); 5]),
        ];
        let source = SmbArchiveReport {
            seed: 0,
            executions: 0,
            milestones: SmbMilestones::default(),
            progress_watermark: crate::smb::target::SmbProgressWatermark::default(),
            first_reached: crate::smb::target::SmbMilestoneTimes::default(),
            first_inputs: crate::smb::target::SmbMilestoneInputs::default(),
            champion_input: SmbInput::default(),
            entries,
            progress_curve: Vec::new(),
            retained: 0,
            rejected: 0,
            deaths: 0,
            selector: crate::smb::archive::SmbSelectorAccounting::default(),
        };
        let mut core = CoordinatorCore::new(4, 32_768);
        // The report stores each entry's actions past its parent and rebuilds
        // the full inputs on load.
        let suffix_json = serde_json::to_string(&source).expect("serialize");
        assert!(suffix_json.contains("\"input_suffix\""));
        let rebuilt: SmbArchiveReport = serde_json::from_str(&suffix_json).expect("load suffix");
        assert_eq!(rebuilt, source);
        let counts = core
            .import_tree(&mut target, &source, None)
            .expect("import");
        assert_eq!(counts.over_limit, 1);
        assert_eq!(counts.rerooted, 1);
        assert_eq!(counts.terminal, 0);
        // The synthetic ROM never leaves one cell, so the two-entry cell
        // keeps genesis and the one-action entry and refuses the rest.
        assert_eq!((counts.imported, counts.rejected), (1, 3));
        let reports: Vec<_> = core.archive.entries.iter().map(|e| &e.report).collect();
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
        assert_eq!(reports.len(), 2);
    }

    #[test]
    fn the_progress_sidecar_changes_no_recorded_bytes() {
        let rom = synthetic_nrom();
        let config = genesis_config(0x5eed_ca0e, 1, 24);
        let mut without = Vec::new();
        run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut without)
            .expect("campaign without a sidecar");
        let mut with = Vec::new();
        let mut sidecar = Vec::new();
        let observed = run_smb_campaign_with_progress(
            &rom,
            &config,
            &SmbCampaignOrigin::Genesis,
            &mut with,
            Some(&mut sidecar),
        )
        .expect("campaign with a sidecar");
        // With one worker the schedule is derivable from the seed, so the
        // sidecar run must record byte-identical stream bytes.
        assert_eq!(without, with);
        assert!(!with.is_empty());
        assert!(
            std::str::from_utf8(&with)
                .expect("stream is utf-8")
                .lines()
                .all(|line| !line.contains("unix_time")),
            "no sidecar field reaches the recorded stream"
        );
        let replayed =
            replay_smb_campaign(&rom, &with, None).expect("sidecar run replays byte-exact");
        assert_eq!(replayed.stream_sha256, observed.stream_sha256);
        assert_eq!(replayed.archive, observed.archive);
    }
}
