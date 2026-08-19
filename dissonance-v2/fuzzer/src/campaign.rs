// SPDX-License-Identifier: AGPL-3.0-or-later

//! Campaign mode: asynchronous shared-archive search with a recorded job stream.
//!
//! A campaign runs W workers on one machine against one shared archive built
//! from the promoted SMB completion stack. A job is a pure function of
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
    sync::mpsc,
    thread,
    time::Duration,
};

use libafl::executors::ExitKind;
use libafl_bolts::rands::{Rand, StdRand};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    chord_table::{ChordTableCheckpoint, ChordTableParameters, ChordTables},
    phase4b::{ButtonChord, SmbInput, SmbMilestones, SmbObservations, SmbSnapshot, SmbTarget},
    phase4c::{
        Archive, ArchiveCandidate, SmbArchiveDurationPolicy, SmbArchiveKey, SmbArchiveKeyPolicy,
        SmbArchiveProgressPoint, SmbArchiveReplacementPolicy, SmbArchiveReport,
        SmbArchiveRetentionPolicy, SmbArchiveSelectorPolicy, SmbArchiveWaypointPolicy,
        SmbSelectorDraw, SmbSelectorPath, admission_is_viable, archive_key,
        merge_action_milestones, merge_milestones, merge_progress_watermark, milestone_key,
        update_first_inputs,
    },
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

/// Curve sampling interval in admitted executions, matching the serial engine.
const CURVE_INTERVAL: u64 = 100;

/// Where a campaign starts: clean genesis or a recorded source archive.
pub enum SmbCampaignOrigin {
    /// Start from gameplay genesis with a single empty input.
    Genesis,
    /// Resume a recorded archive at its single shortest mechanical frontier input.
    Archive {
        /// Path string recorded verbatim in the stream header.
        path: String,
        /// SHA-256 of the source archive file bytes.
        file_sha256: String,
        /// The parsed source archive report.
        report: Box<SmbArchiveReport>,
    },
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
    /// Parent-selector policy, frozen unless the campaign explicitly asks.
    pub selector_policy: SmbArchiveSelectorPolicy,
    /// Admission retention policy, recorded in the header and report.
    pub retention_policy: SmbArchiveRetentionPolicy,
    /// Archive entry bound for this run, recorded in the header and report.
    pub archive_entry_limit: usize,
    /// Controller vocabulary for this run, recorded in the header and report.
    pub vocabulary: SmbCampaignVocabulary,
    /// Archive key policy for this run, recorded in the header and report.
    pub key_policy: SmbArchiveKeyPolicy,
    /// Waypoint policy for this run, recorded in the header and report.
    pub waypoint_policy: SmbArchiveWaypointPolicy,
    /// Suffix policy for this run, recorded in the header and report.
    pub suffix: SmbCampaignSuffixPolicy,
    /// Chord policy for this run, recorded in the header and report.
    pub chord: SmbCampaignChordPolicy,
    /// Cell-replacement rule for this run, recorded in the header and report.
    pub replacement_policy: SmbArchiveReplacementPolicy,
    /// Resume rule for this run, recorded in the header and report.
    pub resume_policy: SmbCampaignResumePolicy,
}

/// Archive entry bound of every stream recorded before the bound was a
/// header field.
const LEGACY_ARCHIVE_ENTRY_LIMIT: usize = 32_768;

fn legacy_archive_entry_limit() -> usize {
    LEGACY_ARCHIVE_ENTRY_LIMIT
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_legacy_archive_entry_limit(limit: &usize) -> bool {
    *limit == LEGACY_ARCHIVE_ENTRY_LIMIT
}

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
    /// Archive entry bound the run retained under; streams recorded before
    /// this field existed ran at, and replay under, 32,768.
    #[serde(
        default = "legacy_archive_entry_limit",
        skip_serializing_if = "is_legacy_archive_entry_limit"
    )]
    pub archive_entry_limit: usize,
    /// Controller vocabulary identifier; streams recorded before this field
    /// existed derived suffixes from, and replay under, the frozen nine masks.
    #[serde(
        default = "legacy_vocabulary_identifier",
        skip_serializing_if = "is_legacy_vocabulary_identifier"
    )]
    pub controller_vocabulary: String,
    /// Archive key policy identifier; streams and reports recorded before
    /// this field existed keyed under, and replay under, the frozen key.
    #[serde(
        default = "legacy_key_policy_identifier",
        skip_serializing_if = "is_legacy_key_policy_identifier"
    )]
    pub key_policy: String,
    /// Waypoint policy identifier; streams recorded before this field
    /// existed ran without a waypoint and replay under `absent`.
    #[serde(
        default = "legacy_waypoint_identifier",
        skip_serializing_if = "is_legacy_waypoint_identifier"
    )]
    pub waypoint_policy: String,
    /// Frozen duration policy identifier.
    pub duration_policy: String,
    /// Frozen suffix policy identifier.
    pub suffix_policy: String,
    /// Chord policy identifier; streams recorded before this field existed
    /// drew uniformly and replay that way.
    #[serde(
        default = "legacy_chord_policy_identifier",
        skip_serializing_if = "is_legacy_chord_policy_identifier"
    )]
    pub chord_policy: String,
    /// Cell-replacement rule identifier; streams recorded before this field
    /// existed replaced on controller actions and replay that way.
    #[serde(
        default = "legacy_replacement_identifier",
        skip_serializing_if = "is_legacy_replacement_identifier"
    )]
    pub replacement_policy: String,
    /// Resume rule identifier; streams recorded before this field existed
    /// resumed from the shortest deepest input and replay that way.
    #[serde(
        default = "legacy_resume_identifier",
        skip_serializing_if = "is_legacy_resume_identifier"
    )]
    pub resume_policy: String,
    /// Derived chord-table provenance; absent for uniform and legacy compiled tables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chord_table: Option<SmbChordTableHeader>,
    /// Promoted retention policy identifier.
    pub retention_policy: String,
    /// Frozen parent scheduler identifier.
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
    /// The candidate's progress snapped below its parent's beyond the
    /// registered threshold; loop-trap retention is refused.
    SnapRefused,
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
    /// Selector draw record, present only under the corrected selector policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<SmbSelectorDraw>,
    /// Derived table version used to draw this job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chord_table_before: Option<ChordTableCheckpoint>,
    /// Periodic derived table hash after admitting this stream record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chord_table_after: Option<ChordTableCheckpoint>,
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
    /// Selector draw record, present only under the corrected selector policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<SmbSelectorDraw>,
    /// Derived table version used to draw this skipped job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chord_table_before: Option<ChordTableCheckpoint>,
    /// Periodic derived table hash after this stream record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chord_table_after: Option<ChordTableCheckpoint>,
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
    /// Archive entry bound the run retained under; reports recorded before
    /// this field existed ran at 32,768.
    #[serde(
        default = "legacy_archive_entry_limit",
        skip_serializing_if = "is_legacy_archive_entry_limit"
    )]
    pub archive_entry_limit: usize,
    /// Controller vocabulary identifier; reports recorded before this field
    /// existed ran under the frozen nine masks.
    #[serde(
        default = "legacy_vocabulary_identifier",
        skip_serializing_if = "is_legacy_vocabulary_identifier"
    )]
    pub controller_vocabulary: String,
    /// Archive key policy identifier; streams and reports recorded before
    /// this field existed keyed under, and replay under, the frozen key.
    #[serde(
        default = "legacy_key_policy_identifier",
        skip_serializing_if = "is_legacy_key_policy_identifier"
    )]
    pub key_policy: String,
    /// Waypoint policy identifier; reports recorded before this field
    /// existed ran without a waypoint.
    #[serde(
        default = "legacy_waypoint_identifier",
        skip_serializing_if = "is_legacy_waypoint_identifier"
    )]
    pub waypoint_policy: String,
    /// Frozen duration policy identifier.
    pub duration_policy: String,
    /// Frozen suffix policy identifier.
    pub suffix_policy: String,
    /// Chord policy identifier; streams recorded before this field existed
    /// drew uniformly and replay that way.
    #[serde(
        default = "legacy_chord_policy_identifier",
        skip_serializing_if = "is_legacy_chord_policy_identifier"
    )]
    pub chord_policy: String,
    /// Cell-replacement rule identifier; streams recorded before this field
    /// existed replaced on controller actions and replay that way.
    #[serde(
        default = "legacy_replacement_identifier",
        skip_serializing_if = "is_legacy_replacement_identifier"
    )]
    pub replacement_policy: String,
    /// Resume rule identifier; streams recorded before this field existed
    /// resumed from the shortest deepest input and replay that way.
    #[serde(
        default = "legacy_resume_identifier",
        skip_serializing_if = "is_legacy_resume_identifier"
    )]
    pub resume_policy: String,
    /// Promoted retention policy identifier.
    pub retention_policy: String,
    /// Frozen parent scheduler identifier.
    pub parent_scheduler: String,
    /// Executor mode identifier.
    pub executor_mode: String,
    /// How per-worker stream seeds derive from (campaign seed, worker index).
    pub worker_seed_derivation: String,
    /// SHA-256 of the ROM bytes.
    pub rom_sha256: String,
    /// Frames emulated by the origin bootstrap walk, probes included.
    pub bootstrap_frames: u64,
    /// Bootstrap frames plus every job's frames, probes included.
    pub frames_emulated: u64,
    /// Jobs skipped before execution as known duplicates.
    pub duplicates_skipped: u64,
    /// Candidates refused by the admission probe.
    pub probe_refused: u64,
    /// Candidates refused by the snapback rule; zero and omitted for runs
    /// recorded before the rule existed.
    #[serde(default, skip_serializing_if = "snap_refused_is_absent")]
    pub snap_refused: u64,
    /// Candidates retained through the waypoint auxiliary cell capacity;
    /// zero and omitted for runs without a registered waypoint.
    #[serde(default, skip_serializing_if = "waypoint_count_is_absent")]
    pub waypoint_retained: u64,
    /// Snapback refusals waived inside the waypoint region; zero and
    /// omitted likewise.
    #[serde(default, skip_serializing_if = "waypoint_count_is_absent")]
    pub waypoint_snap_exempt: u64,
    /// Cell collisions the frames-in-level replacement rule decided; zero
    /// and omitted for runs replacing on controller actions.
    #[serde(default, skip_serializing_if = "waypoint_count_is_absent")]
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

/// Select a recorded archive's single shortest mechanical frontier input.
///
/// This is the C49 resume selection: among all recorded entries at the maximum
/// `(world, level, progress)` tuple, take the fewest actions, then the
/// earliest id.
///
/// # Errors
///
/// Returns an error when the source archive has no retained entries.
/// Buckets behind the frontier a clock-aware resume may reach back, named in
/// the policy identifier per the numeric-constant convention. A campaign
/// re-crosses this much ground routinely, so depth given up here is depth the
/// next link buys back while carrying the cheaper route forward.
const RESUME_FASTEST_BUCKET_REACH: u16 = 32;

/// How a link chooses the one input it resumes from.
///
/// A campaign inherits exactly one lineage from its origin archive; every
/// other entry is discarded at the boundary. Which lineage that is therefore
/// decides what the chain can accumulate.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SmbCampaignResumePolicy {
    /// Frozen behaviour: the shortest input among the entries at the deepest
    /// recorded tuple. Depth, then brevity; cost is not consulted.
    #[default]
    FrontierShortest,
    /// The fewest frames spent in the frontier pair, among entries standing no
    /// more than [`RESUME_FASTEST_BUCKET_REACH`] buckets behind the frontier.
    /// Ties fall back to the frozen rule's order, so the choice stays total.
    FastestInLevelWithin32,
}

/// Identifier a run records for its resume rule.
#[must_use]
pub fn resume_identifier(policy: SmbCampaignResumePolicy) -> &'static str {
    match policy {
        SmbCampaignResumePolicy::FrontierShortest => "frontier_shortest",
        SmbCampaignResumePolicy::FastestInLevelWithin32 => "fastest_in_level_32",
    }
}

/// Recover a resume rule from its recorded identifier.
///
/// # Errors
/// Returns an error when the identifier names no known rule.
pub fn resume_from_identifier(identifier: &str) -> Result<SmbCampaignResumePolicy, Box<dyn Error>> {
    match identifier {
        "frontier_shortest" => Ok(SmbCampaignResumePolicy::FrontierShortest),
        "fastest_in_level_32" => Ok(SmbCampaignResumePolicy::FastestInLevelWithin32),
        _ => Err("unknown campaign resume policy identifier".into()),
    }
}

/// Frames each recorded entry spent inside its own pair, in entry order.
///
/// Same derivation the archive's replacement rule uses, recomputed here from a
/// serialized report: an entry extends its parent's input, so the frames it
/// added are the held frames past the parent's length, and an entry whose
/// parent stands in a different pair started the count there. Parents always
/// carry lower identifiers than their children, so one forward pass suffices.
fn report_frames_in_level(source: &SmbArchiveReport) -> Vec<u64> {
    let frames_of = |actions: &[ButtonChord]| -> u64 {
        actions
            .iter()
            .map(|action| u64::from(action.bounded_hold_frames()))
            .sum()
    };
    let mut index_of: std::collections::BTreeMap<u64, usize> = std::collections::BTreeMap::new();
    for (index, entry) in source.entries.iter().enumerate() {
        index_of.insert(entry.id, index);
    }
    let mut frames = vec![0_u64; source.entries.len()];
    for (index, entry) in source.entries.iter().enumerate() {
        let parent = entry
            .parent_id
            .and_then(|id| index_of.get(&id).copied())
            .filter(|parent| *parent < index)
            .map(|parent| &source.entries[parent]);
        let Some(parent) = parent else {
            frames[index] = frames_of(&entry.input.actions);
            continue;
        };
        let added = frames_of(
            entry
                .input
                .actions
                .get(parent.input.actions.len()..)
                .unwrap_or(&[]),
        );
        let same_pair = (parent.key.world, parent.key.level) == (entry.key.world, entry.key.level);
        let parent_index = index_of[&parent.id];
        frames[index] = if same_pair {
            frames[parent_index].saturating_add(added)
        } else {
            added
        };
    }
    frames
}

/// Choose the one input a link resumes from, under the recorded resume rule.
///
/// # Errors
///
/// Returns an error when the archive holds no retained entry.
pub fn select_frontier_resume_input(
    source: &SmbArchiveReport,
    policy: SmbCampaignResumePolicy,
) -> Result<SmbInput, Box<dyn Error>> {
    let frontier = source
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level, entry.key.progress))
        .max()
        .ok_or("source archive contains no retained entries")?;
    if policy == SmbCampaignResumePolicy::FrontierShortest {
        return source
            .entries
            .iter()
            .filter(|entry| (entry.key.world, entry.key.level, entry.key.progress) == frontier)
            .min_by_key(|entry| (entry.input.actions.len(), entry.id))
            .map(|entry| entry.input.clone())
            .ok_or_else(|| "source archive contains no frontier entries".into());
    }
    // Clock-aware resume: the frontier pair only, no deeper than the frontier
    // and no further back than the registered reach, cheapest in frames first.
    let frames = report_frames_in_level(source);
    let (world, level, progress) = frontier;
    let floor = progress.saturating_sub(RESUME_FASTEST_BUCKET_REACH);
    source
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            (entry.key.world, entry.key.level) == (world, level)
                && entry.key.progress >= floor
                && entry.key.progress <= progress
        })
        .min_by_key(|(index, entry)| (frames[*index], entry.input.actions.len(), entry.id))
        .map(|(_, entry)| entry.input.clone())
        .ok_or_else(|| "source archive contains no resume candidate in the frontier pair".into())
}

/// Header identifier for a parent-selector policy.
#[must_use]
pub fn selector_identifier(policy: SmbArchiveSelectorPolicy) -> String {
    match policy {
        SmbArchiveSelectorPolicy::ConcentratedRecency => "concentrated_recency_128".to_owned(),
        SmbArchiveSelectorPolicy::PinnedWindow {
            world,
            level,
            low,
            high,
        } => format!("pinned_window_128:{world},{level},{low},{high}"),
        SmbArchiveSelectorPolicy::YieldBudgeted(parameters) => format!(
            "yield_budgeted_128:{},{},{},{}",
            parameters.history_window,
            parameters.exploration_floor,
            parameters.maximum_draws,
            parameters.success_cost_scale
        ),
    }
}

/// Parent-selector policy named by a recorded header identifier.
///
/// # Errors
///
/// Returns an error when the identifier names no known selector policy.
pub fn selector_from_identifier(
    identifier: &str,
) -> Result<SmbArchiveSelectorPolicy, Box<dyn Error>> {
    if identifier == "concentrated_recency_128" {
        return Ok(SmbArchiveSelectorPolicy::ConcentratedRecency);
    }
    if let Some(window) = identifier.strip_prefix("pinned_window_128:") {
        let mut parts = window.split(',');
        let world = parts
            .next()
            .ok_or("pinned selector identifier is missing its world")?
            .parse()?;
        let level = parts
            .next()
            .ok_or("pinned selector identifier is missing its level")?
            .parse()?;
        let low = parts
            .next()
            .ok_or("pinned selector identifier is missing its low bucket")?
            .parse()?;
        let high = parts
            .next()
            .ok_or("pinned selector identifier is missing its high bucket")?
            .parse()?;
        if parts.next().is_some() {
            return Err("pinned selector identifier carries extra fields".into());
        }
        return Ok(SmbArchiveSelectorPolicy::PinnedWindow {
            world,
            level,
            low,
            high,
        });
    }
    if let Some(configuration) = identifier.strip_prefix("yield_budgeted_128:") {
        let mut parts = configuration.split(',');
        let parameters = crate::draw_budget::DrawBudgetParameters {
            history_window: parse_selector_field(&mut parts, "history window")?,
            exploration_floor: parse_selector_field(&mut parts, "exploration floor")?,
            maximum_draws: parse_selector_field(&mut parts, "maximum draws")?,
            success_cost_scale: parse_selector_field(&mut parts, "success cost scale")?,
        };
        if parts.next().is_some() {
            return Err("yield-budget selector identifier carries extra fields".into());
        }
        parameters.validate()?;
        return Ok(SmbArchiveSelectorPolicy::YieldBudgeted(parameters));
    }
    // The frozen and uncapped-corrected selectors were deleted on promotion.
    // A stream recorded under either replays only at the commit that
    // recorded it.
    Err("campaign stream parent scheduler is not recognized".into())
}

fn parse_selector_field<'a, T>(
    fields: &mut impl Iterator<Item = &'a str>,
    name: &str,
) -> Result<T, Box<dyn Error>>
where
    T: std::str::FromStr,
    T::Err: Error + 'static,
{
    Ok(fields
        .next()
        .ok_or_else(|| format!("yield-budget selector is missing {name}"))?
        .parse()?)
}

/// Header identifier for an admission retention policy.
#[must_use]
pub fn retention_identifier(policy: SmbArchiveRetentionPolicy) -> &'static str {
    match policy {
        SmbArchiveRetentionPolicy::Frozen => "frozen",
        SmbArchiveRetentionPolicy::ProbeAtAdmission => "probe_at_admission",
        SmbArchiveRetentionPolicy::ProbeAtAdmission45 => "probe_at_admission_45",
        SmbArchiveRetentionPolicy::ProbeAtAdmission45Snapback16 => {
            "probe_at_admission_45_snapback_16"
        }
    }
}

/// Admission retention policy named by a recorded header identifier.
///
/// # Errors
///
/// Returns an error when the identifier names no known retention policy.
pub fn retention_from_identifier(
    identifier: &str,
) -> Result<SmbArchiveRetentionPolicy, Box<dyn Error>> {
    match identifier {
        "frozen" => Ok(SmbArchiveRetentionPolicy::Frozen),
        "probe_at_admission" => Ok(SmbArchiveRetentionPolicy::ProbeAtAdmission),
        "probe_at_admission_45" => Ok(SmbArchiveRetentionPolicy::ProbeAtAdmission45),
        "probe_at_admission_45_snapback_16" => {
            Ok(SmbArchiveRetentionPolicy::ProbeAtAdmission45Snapback16)
        }
        _ => Err("campaign stream retention policy is not recognized".into()),
    }
}

/// Reject streams whose selector annotations disagree with the header policies.
fn verify_selector_annotation(
    policy: SmbArchiveSelectorPolicy,
    waypoint_policy: SmbArchiveWaypointPolicy,
    annotation: Option<&SmbSelectorDraw>,
) -> Result<(), Box<dyn Error>> {
    match (policy, annotation) {
        (
            SmbArchiveSelectorPolicy::ConcentratedRecency
            | SmbArchiveSelectorPolicy::PinnedWindow { .. }
            | SmbArchiveSelectorPolicy::YieldBudgeted(_),
            None,
        ) => Err("concentrated-selector stream is missing a selector annotation".into()),
        (
            SmbArchiveSelectorPolicy::ConcentratedRecency
            | SmbArchiveSelectorPolicy::PinnedWindow { .. }
            | SmbArchiveSelectorPolicy::YieldBudgeted(_),
            Some(draw),
        ) => {
            if draw.waypoint && waypoint_policy == SmbArchiveWaypointPolicy::Absent {
                return Err(
                    "waypoint draw is recorded without a registered waypoint policy".into(),
                );
            }
            if draw.waypoint && draw.path == SmbSelectorPath::Uniform {
                return Err("waypoint draw claims the uniform path".into());
            }
            match (draw.path, draw.concentration) {
                (SmbSelectorPath::TieClass, None) => {
                    Err("concentrated tie-class draw is missing its concentration record".into())
                }
                (SmbSelectorPath::Uniform, Some(_)) => {
                    Err("concentrated uniform draw carries a concentration record".into())
                }
                _ => Ok(()),
            }
        }
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
/// The frozen one-or-two suffix policy and stratified duration policy are
/// sampled by the same shared code the serial engine uses, from a fresh RNG
/// seeded with the mutation seed alone. This is what makes a job a pure
/// function of (parent snapshot, mutation seed).
fn derive_suffix(
    mutation_seed: u64,
    vocabulary: SmbCampaignVocabulary,
) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
    derive_suffix_sized(
        mutation_seed,
        vocabulary,
        false,
        SmbCampaignChordPolicy::Uniform,
        None,
    )
}

/// Region-conditional suffix derivation: ordinary draws keep the frozen
/// one-or-two shape; a long draw — taken when the selected parent sits
/// inside the registered waypoint region under the long-suffix policy —
/// samples its length uniformly up to [`REGION_LONG_SUFFIX_CAP`], so one
/// job can traverse the whole registered section in a single trajectory.
/// Single-trajectory traversal is immune to cross-lineage poisoning by
/// construction: every page crossing inside the job shares one history.
fn derive_suffix_sized(
    mutation_seed: u64,
    vocabulary: SmbCampaignVocabulary,
    long: bool,
    chord_policy: SmbCampaignChordPolicy,
    chord_tables: Option<&ChordTables<ButtonChord>>,
) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
    let mut rand = StdRand::with_seed(mutation_seed);
    let suffix_len = if long {
        1 + rand.below(NonZeroUsize::new(REGION_LONG_SUFFIX_CAP).ok_or("invalid long cap")?)
    } else if rand.below(NonZeroUsize::new(4).ok_or("invalid suffix odds")?) == 0 {
        2
    } else {
        1
    };
    let mut suffix = Vec::with_capacity(suffix_len);
    for _ in 0..suffix_len {
        let recorded = long
            && chord_policy != SmbCampaignChordPolicy::Uniform
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
        suffix.push(crate::phase4c::sample_chord_from_masks(
            &mut rand,
            SmbArchiveDurationPolicy::Stratified,
            vocabulary.masks(),
        )?);
    }
    Ok(suffix)
}

/// C95 ruling: every post-resume chord from C89's crossing lineages — the
/// machine's own recorded sample of maneuvers this castle's checks reward.
/// Drawing uniformly from the list reproduces the empirical distribution;
/// duplicates carry the frequencies. Provenance: the crossing-chord census
/// over the C89 archive, entries at the frontier pair past bucket 73.
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
    pub parameters: ChordTableParameters,
}

/// Header provenance for a derived chord-table policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbChordTableHeader {
    /// SHA-256 of the named resume archive, or SHA-256 of empty bytes at genesis.
    pub source_sha256: String,
    /// Registered source filter and game-neutral fold parameters.
    pub derivation: SmbChordTableDerivation,
    /// Hash after folding the named source and before the first campaign draw.
    pub initial: ChordTableCheckpoint,
}

/// Chord policy a campaign draws region chords from, recorded in the
/// stream header; recorded variants bind only to region long draws.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbCampaignChordPolicy {
    /// The frozen shape: every chord drawn uniformly from the vocabulary.
    #[default]
    Uniform,
    /// C95 ruling: region long draws take each chord from the recorded
    /// table at even odds with the uniform draw, so exploration mass stays
    /// honest while the sampled shape follows the machine's own successes.
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
        let parameters = ChordTableParameters {
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

fn legacy_chord_policy_identifier() -> String {
    "chord_uniform".to_owned()
}

#[allow(clippy::ptr_arg)]
fn is_legacy_chord_policy_identifier(identifier: &String) -> bool {
    identifier == "chord_uniform"
}

fn legacy_resume_identifier() -> String {
    "frontier_shortest".to_owned()
}

#[allow(clippy::ptr_arg)]
fn is_legacy_resume_identifier(identifier: &String) -> bool {
    identifier == "frontier_shortest"
}

fn legacy_replacement_identifier() -> String {
    "fewest_actions".to_owned()
}

#[allow(clippy::ptr_arg)]
fn is_legacy_replacement_identifier(identifier: &String) -> bool {
    identifier == "fewest_actions"
}

/// Identifier a run records for its cell-replacement rule.
#[must_use]
pub fn replacement_identifier(policy: SmbArchiveReplacementPolicy) -> &'static str {
    match policy {
        SmbArchiveReplacementPolicy::FewestActions => "fewest_actions",
        SmbArchiveReplacementPolicy::FewestFramesInLevel => "fewest_frames_in_level",
    }
}

/// Recover a replacement policy from its recorded identifier.
///
/// # Errors
/// Returns an error when the identifier names no known policy.
pub fn replacement_from_identifier(
    identifier: &str,
) -> Result<SmbArchiveReplacementPolicy, Box<dyn Error>> {
    match identifier {
        "fewest_actions" => Ok(SmbArchiveReplacementPolicy::FewestActions),
        "fewest_frames_in_level" => Ok(SmbArchiveReplacementPolicy::FewestFramesInLevel),
        _ => Err("unknown campaign replacement policy identifier".into()),
    }
}

/// Length cap for region-conditional long suffixes, named in the policy
/// identifier per the numeric-constant convention.
const REGION_LONG_SUFFIX_CAP: usize = 48;

/// Suffix policy a campaign derives suffix lengths from, recorded in the
/// stream header; the long variant binds to the registered waypoint region.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbCampaignSuffixPolicy {
    /// The program's frozen shape: one action, or two at one-in-four odds.
    #[default]
    OneOrTwo,
    /// C93 ruling: parents inside the registered waypoint region draw long
    /// suffixes with length uniform up to the cap; all other parents keep
    /// the frozen shape. Requires a registered waypoint region to bind.
    OneOrTwoRegionLong48,
}

/// Header identifier for a suffix policy.
#[must_use]
pub fn suffix_policy_identifier(policy: SmbCampaignSuffixPolicy) -> &'static str {
    match policy {
        SmbCampaignSuffixPolicy::OneOrTwo => "one_or_two",
        SmbCampaignSuffixPolicy::OneOrTwoRegionLong48 => "one_or_two_region_long_48",
    }
}

/// Suffix policy named by a recorded header identifier.
///
/// # Errors
///
/// Returns an error when the identifier names no known suffix policy.
pub fn suffix_policy_from_identifier(
    identifier: &str,
) -> Result<SmbCampaignSuffixPolicy, Box<dyn Error>> {
    match identifier {
        "one_or_two" => Ok(SmbCampaignSuffixPolicy::OneOrTwo),
        "one_or_two_region_long_48" => Ok(SmbCampaignSuffixPolicy::OneOrTwoRegionLong48),
        _ => Err("campaign stream suffix policy is not recognized".into()),
    }
}

/// Controller vocabulary a campaign derives suffixes from, recorded in the
/// stream header; the table's length is index-visible to suffix derivation,
/// so legacy streams replay only under the frozen nine masks.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbCampaignVocabulary {
    /// The program's historical nine masks; every stream recorded before the
    /// vocabulary was a header field ran under this.
    #[default]
    FrozenNineMask,
    /// D71 ruling: the nine masks plus Down, for down-entry pipes.
    DownTenMask,
}

impl SmbCampaignVocabulary {
    fn masks(self) -> &'static [u8] {
        match self {
            Self::FrozenNineMask => &crate::phase4c::FROZEN_BUTTON_MASKS,
            Self::DownTenMask => &crate::phase4c::DOWN_TEN_BUTTON_MASKS,
        }
    }
}

/// Header identifier for a controller vocabulary.
#[must_use]
pub fn vocabulary_identifier(vocabulary: SmbCampaignVocabulary) -> &'static str {
    match vocabulary {
        SmbCampaignVocabulary::FrozenNineMask => "frozen_nine_mask",
        SmbCampaignVocabulary::DownTenMask => "down_ten_mask",
    }
}

/// Controller vocabulary named by a recorded header identifier.
///
/// # Errors
///
/// Returns an error when the identifier names no known vocabulary.
pub fn vocabulary_from_identifier(
    identifier: &str,
) -> Result<SmbCampaignVocabulary, Box<dyn Error>> {
    match identifier {
        "frozen_nine_mask" => Ok(SmbCampaignVocabulary::FrozenNineMask),
        "down_ten_mask" => Ok(SmbCampaignVocabulary::DownTenMask),
        _ => Err("campaign stream controller vocabulary is not recognized".into()),
    }
}

/// Header identifier for an archive key policy.
#[must_use]
pub fn key_policy_identifier(policy: SmbArchiveKeyPolicy) -> String {
    match policy {
        SmbArchiveKeyPolicy::Frozen => "frozen".to_owned(),
        SmbArchiveKeyPolicy::VerticalPage => "vertical_page".to_owned(),
        SmbArchiveKeyPolicy::FrozenRoomX16 {
            world,
            level,
            progress,
        } => format!("frozen_room_x_16:{world},{level},{progress}"),
    }
}

/// Archive key policy named by a recorded header identifier.
///
/// # Errors
///
/// Returns an error when the identifier names no known key policy.
pub fn key_policy_from_identifier(identifier: &str) -> Result<SmbArchiveKeyPolicy, Box<dyn Error>> {
    if identifier == "frozen" {
        return Ok(SmbArchiveKeyPolicy::Frozen);
    }
    if identifier == "vertical_page" {
        return Ok(SmbArchiveKeyPolicy::VerticalPage);
    }
    if let Some(room) = identifier.strip_prefix("frozen_room_x_16:") {
        let mut parts = room.split(',');
        let world = parts
            .next()
            .ok_or("room key identifier is missing its world")?
            .parse()?;
        let level = parts
            .next()
            .ok_or("room key identifier is missing its level")?
            .parse()?;
        let progress = parts
            .next()
            .ok_or("room key identifier is missing its progress")?
            .parse()?;
        if parts.next().is_some() {
            return Err("room key identifier carries extra fields".into());
        }
        return Ok(SmbArchiveKeyPolicy::FrozenRoomX16 {
            world,
            level,
            progress,
        });
    }
    Err("campaign stream key policy is not recognized".into())
}

/// Header identifier for a waypoint policy.
#[must_use]
pub fn waypoint_identifier(policy: SmbArchiveWaypointPolicy) -> String {
    match policy {
        SmbArchiveWaypointPolicy::Absent => "absent".to_owned(),
        SmbArchiveWaypointPolicy::Region {
            world,
            level,
            low,
            high,
            band_low,
            band_high,
        } => format!("waypoint_4:{world},{level},{low},{high},{band_low},{band_high}"),
        SmbArchiveWaypointPolicy::RegionBucketUniform {
            world,
            level,
            low,
            high,
            band_low,
            band_high,
        } => {
            format!("waypoint_4_bucket_uniform:{world},{level},{low},{high},{band_low},{band_high}")
        }
    }
}

/// Waypoint policy named by a recorded header identifier.
///
/// # Errors
///
/// Returns an error when the identifier names no known waypoint policy or
/// declares an inverted window.
pub fn waypoint_from_identifier(
    identifier: &str,
) -> Result<SmbArchiveWaypointPolicy, Box<dyn Error>> {
    if identifier == "absent" {
        return Ok(SmbArchiveWaypointPolicy::Absent);
    }
    let bucket_uniform = identifier.strip_prefix("waypoint_4_bucket_uniform:");
    if let Some(region) = bucket_uniform.or_else(|| identifier.strip_prefix("waypoint_4:")) {
        let mut parts = region.split(',');
        let world = parts
            .next()
            .ok_or("waypoint identifier is missing its world")?
            .parse()?;
        let level = parts
            .next()
            .ok_or("waypoint identifier is missing its level")?
            .parse()?;
        let low = parts
            .next()
            .ok_or("waypoint identifier is missing its low bucket")?
            .parse()?;
        let high = parts
            .next()
            .ok_or("waypoint identifier is missing its high bucket")?
            .parse()?;
        let band_low = parts
            .next()
            .ok_or("waypoint identifier is missing its band low bucket")?
            .parse()?;
        let band_high = parts
            .next()
            .ok_or("waypoint identifier is missing its band high bucket")?
            .parse()?;
        if parts.next().is_some() {
            return Err("waypoint identifier carries extra fields".into());
        }
        if low > high || band_low > band_high {
            return Err("waypoint identifier declares an inverted window".into());
        }
        if bucket_uniform.is_some() {
            return Ok(SmbArchiveWaypointPolicy::RegionBucketUniform {
                world,
                level,
                low,
                high,
                band_low,
                band_high,
            });
        }
        return Ok(SmbArchiveWaypointPolicy::Region {
            world,
            level,
            low,
            high,
            band_low,
            band_high,
        });
    }
    Err("campaign stream waypoint policy is not recognized".into())
}

fn snap_refused_is_absent(count: &u64) -> bool {
    *count == 0
}

fn waypoint_count_is_absent(count: &u64) -> bool {
    *count == 0
}

fn legacy_waypoint_identifier() -> String {
    "absent".to_owned()
}

#[allow(clippy::ptr_arg)]
fn is_legacy_waypoint_identifier(identifier: &String) -> bool {
    identifier == "absent"
}

fn legacy_key_policy_identifier() -> String {
    "frozen".to_owned()
}

#[allow(clippy::ptr_arg)]
fn is_legacy_key_policy_identifier(identifier: &String) -> bool {
    identifier == "frozen"
}

fn legacy_vocabulary_identifier() -> String {
    "frozen_nine_mask".to_owned()
}

#[allow(clippy::ptr_arg)]
fn is_legacy_vocabulary_identifier(identifier: &String) -> bool {
    identifier == "frozen_nine_mask"
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
/// the serial engine's suffix loop does, collecting per-boundary candidates
/// with worker-side probe verdicts.
/// Per-run execution policies a worker applies to every job.
#[derive(Clone, Copy, Debug)]
struct SmbJobPolicies {
    max_actions: usize,
    retention_policy: SmbArchiveRetentionPolicy,
    key_policy: SmbArchiveKeyPolicy,
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
        if target.is_dead() || length >= policies.max_actions {
            break;
        }
        length = length.saturating_add(1);
        target.apply(action);
        merge_action_milestones(&mut milestones, target)?;
        let observations = target.last_action_observations().to_vec();
        let dead = target.is_dead();
        let failed = target.exit_kind() != ExitKind::Ok;
        let candidate = if dead || failed {
            None
        } else {
            let snapshot = target
                .snapshot()
                .ok_or("failed to snapshot campaign suffix")?;
            let key = archive_key(target.wram(), policies.key_policy);
            let viable = admission_is_viable(target, &snapshot, policies.retention_policy)?;
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
            failed,
            candidate,
        });
        if dead || failed {
            break;
        }
    }
    Ok(SmbCampaignJobResult { actions })
}

/// Serial archive-and-accumulator state shared by the live coordinator loop and
/// replay. Admission through this struct is the single admission lock: every
/// archive mutation happens here, in stream order, so the archive state at any
/// stream position is identical in the live run and in replay.
struct CoordinatorCore<'a> {
    archive: Archive<'a>,
    aggregate: SmbMilestones,
    watermark: crate::phase4b::SmbProgressWatermark,
    first_reached: crate::phase4b::SmbMilestoneTimes,
    first_inputs: crate::phase4b::SmbMilestoneInputs,
    champion_input: SmbInput,
    champion_milestones: SmbMilestones,
    curve: Vec<SmbArchiveProgressPoint>,
    deaths: u64,
    sequence: u64,
    probe_refused: u64,
    snap_refused: u64,
    max_actions: usize,
    retention_policy: SmbArchiveRetentionPolicy,
    key_policy: SmbArchiveKeyPolicy,
    waypoint_policy: SmbArchiveWaypointPolicy,
    waypoint_snap_exempt: u64,
}

impl CoordinatorCore<'_> {
    fn new(
        max_actions: usize,
        selector_policy: SmbArchiveSelectorPolicy,
        retention_policy: SmbArchiveRetentionPolicy,
        archive_entry_limit: usize,
        key_policy: SmbArchiveKeyPolicy,
        waypoint_policy: SmbArchiveWaypointPolicy,
        replacement_policy: SmbArchiveReplacementPolicy,
    ) -> Self {
        let mut archive = Archive::new(None);
        archive.max_entries = archive_entry_limit;
        archive.set_selector_policy(selector_policy);
        archive.set_waypoint_policy(waypoint_policy);
        archive.set_replacement_policy(replacement_policy);
        Self {
            archive,
            aggregate: SmbMilestones::default(),
            watermark: crate::phase4b::SmbProgressWatermark::default(),
            first_reached: crate::phase4b::SmbMilestoneTimes::default(),
            first_inputs: crate::phase4b::SmbMilestoneInputs::default(),
            champion_input: SmbInput::default(),
            champion_milestones: SmbMilestones::default(),
            curve: Vec::new(),
            deaths: 0,
            sequence: 0,
            probe_refused: 0,
            snap_refused: 0,
            max_actions,
            retention_policy,
            key_policy,
            waypoint_policy,
            waypoint_snap_exempt: 0,
        }
    }

    /// Walk the origin exactly as the serial engine's bootstrap does: retain
    /// genesis, then every viable action boundary of each initial input, all at
    /// execution zero and with the promoted admission probe.
    fn bootstrap(
        &mut self,
        target: &mut SmbTarget,
        initial_inputs: &[SmbInput],
    ) -> Result<(), Box<dyn Error>> {
        if initial_inputs.is_empty() {
            return Err("campaign bootstrap requires a nonempty initial corpus".into());
        }
        if initial_inputs
            .iter()
            .any(|input| input.actions.len() > self.max_actions)
        {
            return Err("campaign bootstrap input exceeds the configured action limit".into());
        }
        target.reset();
        let genesis_key = archive_key(target.wram(), self.key_policy);
        let genesis_snapshot = target
            .snapshot()
            .ok_or("failed to snapshot campaign genesis")?;
        let genesis_id = self
            .archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    input: SmbInput::default(),
                    key: genesis_key,
                    milestones: SmbMilestones::default(),
                },
                genesis_snapshot,
                &[],
            )?
            .ok_or("failed to retain campaign genesis")?;
        for input in initial_inputs {
            target.reset();
            let mut prefix = SmbInput::default();
            let mut milestones = SmbMilestones::default();
            let mut parent_id = genesis_id;
            for action in &input.actions {
                if target.is_dead() {
                    break;
                }
                prefix.actions.push(*action);
                target.apply(action);
                merge_progress_watermark(&mut self.watermark, target.last_action_observations());
                merge_action_milestones(&mut milestones, target)?;
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
                if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                    break;
                }
                let snapshot = target
                    .snapshot()
                    .ok_or("failed to snapshot campaign bootstrap prefix")?;
                let observations = target.last_action_observations().to_vec();
                let key = archive_key(target.wram(), self.key_policy);
                if !admission_is_viable(target, &snapshot, self.retention_policy)? {
                    continue;
                }
                if let Some(id) = self.archive.insert(
                    Some(parent_id),
                    0,
                    ArchiveCandidate {
                        input: prefix.clone(),
                        key,
                        milestones,
                    },
                    snapshot,
                    &observations,
                )? {
                    parent_id = id;
                }
            }
        }
        Ok(())
    }

    /// Admit one executed job at the next sequence position, merging its
    /// per-action evidence in order and applying the promoted retention rules
    /// through the same archive the serial engine uses.
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
            if let Some(candidate) = &action.candidate {
                if !candidate.viable {
                    self.probe_refused = self.probe_refused.saturating_add(1);
                    decisions.push(SmbCampaignAdmissionDecision::ProbeRefused);
                    continue;
                }
                if self.retention_policy == SmbArchiveRetentionPolicy::ProbeAtAdmission45Snapback16
                {
                    let parent_key = self
                        .archive
                        .entries
                        .get(current_parent)
                        .ok_or("campaign snapback check lost its parent")?
                        .report
                        .key;
                    if (parent_key.world, parent_key.level)
                        == (candidate.key.world, candidate.key.level)
                        && parent_key.progress > candidate.key.progress.saturating_add(16)
                    {
                        // Waypoint composition with the snapback refusal: a
                        // candidate inside the registered region is exempt.
                        // The snapback rule starves accidental loop traps,
                        // while the waypoint declares backward motion into
                        // its region intentional; the region's auxiliary
                        // retention stays capacity-capped, so the exemption
                        // cannot flood the archive. Outside the region the
                        // refusal is unchanged.
                        if self.waypoint_policy.contains(&candidate.key) {
                            self.waypoint_snap_exempt = self.waypoint_snap_exempt.saturating_add(1);
                        } else {
                            self.snap_refused = self.snap_refused.saturating_add(1);
                            decisions.push(SmbCampaignAdmissionDecision::SnapRefused);
                            continue;
                        }
                    }
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
                    &action.observations,
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

    /// Push the final curve point exactly as the serial engine does at its
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
            ranking: Default::default(),
            generated_mutator: Default::default(),
            // Frozen ladder policy: absent, so campaign archives keep their
            // recorded byte shape.
            ladder: Default::default(),
            // Absent under the frozen selector for the same reason.
            selector: self.archive.selector_report(),
        }
    }
}

/// The resume input and origin record derived from a campaign origin.
struct ResolvedOrigin {
    record: SmbCampaignOriginRecord,
    resume_input: SmbInput,
}

type InitialChordTables = (
    Option<ChordTables<ButtonChord>>,
    Option<SmbChordTableHeader>,
);

fn initial_chord_tables(
    policy: SmbCampaignChordPolicy,
    origin: &SmbCampaignOrigin,
) -> Result<InitialChordTables, Box<dyn Error>> {
    let SmbCampaignChordPolicy::DerivedHalf(derivation) = policy else {
        return Ok((None, None));
    };
    let mut tables = ChordTables::new(derivation.parameters)?;
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
    entry: &crate::phase4c::SmbArchiveEntryReport,
) -> bool {
    (entry.key.world, entry.key.level) == (filter.world, filter.level)
        && entry.key.progress >= filter.minimum_progress
}

fn current_chord_checkpoint(
    tables: Option<&ChordTables<ButtonChord>>,
) -> Result<Option<ChordTableCheckpoint>, Box<dyn Error>> {
    tables
        .map(ChordTables::checkpoint)
        .transpose()
        .map_err(Into::into)
}

fn recorded_chord_tables<'a>(
    policy: SmbCampaignChordPolicy,
    before: Option<&ChordTableCheckpoint>,
    versions: &'a BTreeMap<u64, ChordTables<ButtonChord>>,
) -> Result<Option<&'a ChordTables<ButtonChord>>, Box<dyn Error>> {
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
    tables: Option<&ChordTables<ButtonChord>>,
    required: &BTreeSet<u64>,
    versions: &mut BTreeMap<u64, ChordTables<ButtonChord>>,
) {
    if let Some(tables) = tables
        && required.contains(&tables.records())
    {
        versions.insert(tables.records(), tables.clone());
    }
}

fn finish_chord_stream_record(
    policy: SmbCampaignChordPolicy,
    tables: &mut Option<ChordTables<ButtonChord>>,
    core: &CoordinatorCore<'_>,
    decisions: &[SmbCampaignAdmissionDecision],
) -> Result<Option<ChordTableCheckpoint>, Box<dyn Error>> {
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

fn resolve_origin(
    origin: &SmbCampaignOrigin,
    resume_policy: SmbCampaignResumePolicy,
) -> Result<ResolvedOrigin, Box<dyn Error>> {
    let (kind, path, archive_sha256, resume_input) = match origin {
        SmbCampaignOrigin::Genesis => ("genesis".to_owned(), None, None, SmbInput::default()),
        SmbCampaignOrigin::Archive {
            path,
            file_sha256,
            report,
        } => (
            "archive".to_owned(),
            Some(path.clone()),
            Some(file_sha256.clone()),
            select_frontier_resume_input(report, resume_policy)?,
        ),
    };
    let resume_input_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&resume_input)?));
    Ok(ResolvedOrigin {
        record: SmbCampaignOriginRecord {
            kind,
            path,
            archive_sha256,
            resume_input_sha256,
            resume_actions: resume_input.actions.len(),
        },
        resume_input,
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
        resume_input_sha256: origin.resume_input_sha256.clone(),
        resume_actions: origin.resume_actions,
        execution_budget: config.execution_budget,
        wall_budget_seconds: config.wall_budget.map(|budget| budget.as_secs()),
        action_limit: config.action_limit,
        archive_entry_limit: config.archive_entry_limit,
        controller_vocabulary: vocabulary_identifier(config.vocabulary).to_owned(),
        key_policy: key_policy_identifier(config.key_policy),
        waypoint_policy: waypoint_identifier(config.waypoint_policy),
        duration_policy: "stratified".to_owned(),
        suffix_policy: suffix_policy_identifier(config.suffix).to_owned(),
        chord_policy: chord_policy_identifier(config.chord),
        chord_table,
        replacement_policy: replacement_identifier(config.replacement_policy).to_owned(),
        resume_policy: resume_identifier(config.resume_policy).to_owned(),
        retention_policy: retention_identifier(config.retention_policy).to_owned(),
        parent_scheduler: selector_identifier(config.selector_policy),
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
    job_frames: u64,
    duplicates_skipped: u64,
    jobs_per_worker: Vec<u64>,
    skips_per_worker: Vec<u64>,
}

impl CampaignCounters {
    fn new(workers: u32) -> Self {
        Self {
            bootstrap_frames: 0,
            job_frames: 0,
            duplicates_skipped: 0,
            jobs_per_worker: vec![0; workers as usize],
            skips_per_worker: vec![0; workers as usize],
        }
    }
}

fn build_report(
    header: &SmbCampaignStreamHeader,
    origin: SmbCampaignOriginRecord,
    core: CoordinatorCore<'_>,
    counters: &CampaignCounters,
    stream_sha256: String,
) -> SmbCampaignModeReport {
    let executions_completed = core.sequence;
    let probe_refused = core.probe_refused;
    let snap_refused = core.snap_refused;
    let waypoint_retained = core.archive.waypoint_retained();
    let replacement_frames_displaced = core.archive.replacement_frames_displaced();
    let waypoint_snap_exempt = core.waypoint_snap_exempt;
    let archive = core.into_archive_report(header.campaign_seed);
    SmbCampaignModeReport {
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
        waypoint_policy: header.waypoint_policy.clone(),
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
        frames_emulated: counters
            .bootstrap_frames
            .saturating_add(counters.job_frames),
        duplicates_skipped: counters.duplicates_skipped,
        probe_refused,
        snap_refused,
        waypoint_retained,
        waypoint_snap_exempt,
        replacement_frames_displaced,
        jobs_per_worker: counters.jobs_per_worker.clone(),
        skips_per_worker: counters.skips_per_worker.clone(),
        stream_sha256,
        archive,
    }
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
    selector: Option<SmbSelectorDraw>,
    chord_table_before: Option<ChordTableCheckpoint>,
}

struct WorkerReply {
    worker: u32,
    outcome: Result<(SmbCampaignJobResult, u64), String>,
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
pub fn run_smb_campaign(
    rom: &[u8],
    config: &SmbCampaignConfig,
    origin: &SmbCampaignOrigin,
    stream: &mut dyn Write,
) -> Result<SmbCampaignModeReport, Box<dyn Error>> {
    if config.workers == 0 {
        return Err("campaign mode requires at least one worker".into());
    }
    if config.action_limit == 0 || config.action_limit > crate::phase4c::MAX_SMB_COMPLETION_ACTIONS
    {
        return Err("campaign action limit is outside its bounded range".into());
    }
    if config.archive_entry_limit == 0
        || config.archive_entry_limit > crate::phase4c::MAX_ARCHIVE_ENTRIES
    {
        return Err("campaign archive entry limit is outside its bounded range".into());
    }
    if let SmbArchiveWaypointPolicy::Region {
        low,
        high,
        band_low,
        band_high,
        ..
    } = config.waypoint_policy
        && (low > high || band_low > band_high)
    {
        return Err("campaign waypoint region declares an inverted window".into());
    }
    let resolved = resolve_origin(origin, config.resume_policy)?;
    let (mut chord_tables, chord_table_header) = initial_chord_tables(config.chord, origin)?;
    let header = stream_header(config, &resolved.record, chord_table_header, rom);
    let mut writer = StreamWriter::new(stream);
    writer.write_line(&header)?;

    let mut core = CoordinatorCore::new(
        config.action_limit,
        config.selector_policy,
        config.retention_policy,
        config.archive_entry_limit,
        config.key_policy,
        config.waypoint_policy,
        config.replacement_policy,
    );
    let mut counters = CampaignCounters::new(config.workers);
    let mut bootstrap_target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    let frames_before = bootstrap_target.frames_clocked();
    core.bootstrap(
        &mut bootstrap_target,
        std::slice::from_ref(&resolved.resume_input),
    )?;
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

    let mut reserved = 0_u64;
    let mut pending: Vec<Option<PendingJob>> = Vec::new();
    pending.resize_with(workers, || None);

    thread::scope(|scope| -> Result<(), Box<dyn Error>> {
        let (reply_sender, reply_receiver) = mpsc::channel::<WorkerReply>();
        let mut job_senders = Vec::with_capacity(workers);
        for index in 0..config.workers {
            let (job_sender, job_receiver) = mpsc::channel::<JobSpec>();
            let reply_sender = reply_sender.clone();
            let worker_policies = SmbJobPolicies {
                max_actions: config.action_limit,
                retention_policy: config.retention_policy,
                key_policy: config.key_policy,
            };
            scope.spawn(move || {
                let mut target = match SmbTarget::from_smb_rom_bytes_headless(rom) {
                    Ok(target) => target,
                    Err(error) => {
                        let _ = reply_sender.send(WorkerReply {
                            worker: index,
                            outcome: Err(error.to_string()),
                        });
                        return;
                    }
                };
                while let Ok(spec) = job_receiver.recv() {
                    let frames_before = target.frames_clocked();
                    let outcome = execute_job(
                        &mut target,
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
                    .map_err(|error| error.to_string());
                    let failed = outcome.is_err();
                    if reply_sender
                        .send(WorkerReply {
                            worker: index,
                            outcome,
                        })
                        .is_err()
                        || failed
                    {
                        break;
                    }
                }
            });
            job_senders.push(Some(job_sender));
        }
        drop(reply_sender);

        // Select one job for one worker, recording skips, or report exhaustion.
        let select = |core: &mut CoordinatorCore<'_>,
                      rands: &mut [StdRand],
                      chord_tables: &mut Option<ChordTables<ButtonChord>>,
                      writer: &mut StreamWriter<'_>,
                      counters: &mut CampaignCounters,
                      reserved: &mut u64,
                      worker: u32|
         -> Result<Option<(JobSpec, PendingJob)>, Box<dyn Error>> {
            if *reserved >= config.execution_budget {
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
                let long = config.suffix == SmbCampaignSuffixPolicy::OneOrTwoRegionLong48
                    && core
                        .archive
                        .entries
                        .get(parent_index)
                        .is_some_and(|entry| core.archive.waypoint_contains(&entry.report.key));
                let chord_table_before = current_chord_checkpoint(chord_tables.as_ref())?;
                let suffix = derive_suffix_sized(
                    mutation_seed,
                    config.vocabulary,
                    long,
                    config.chord,
                    chord_tables.as_ref(),
                )?;
                if consecutive_skips < CONSECUTIVE_SKIP_LIMIT
                    && core.all_prefixes_archived(parent_index, &suffix)
                {
                    let chord_table_after =
                        finish_chord_stream_record(config.chord, chord_tables, core, &[])?;
                    writer.write_line(&SmbCampaignStreamRecord::Skip(SmbCampaignSkipRecord {
                        worker,
                        parent_id: u64::try_from(parent_index)?,
                        mutation_seed,
                        selector,
                        chord_table_before,
                        chord_table_after,
                    }))?;
                    if let Some(draw) = &selector {
                        core.archive.record_selection(parent_index, draw);
                    }
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
                    let sender = job_senders[worker as usize]
                        .as_ref()
                        .ok_or("campaign worker channel closed early")?;
                    sender
                        .send(spec)
                        .map_err(|_| "campaign worker exited before its first job")?;
                    in_flight += 1;
                }
                None => {
                    job_senders[worker as usize] = None;
                }
            }
        }

        while in_flight > 0 {
            let reply = reply_receiver
                .recv()
                .map_err(|_| "every campaign worker exited while jobs were in flight")?;
            let worker_index = reply.worker as usize;
            let (result, frames) = reply.outcome.map_err(|error| -> Box<dyn Error> {
                format!("campaign worker {} failed: {error}", reply.worker).into()
            })?;
            let pending_job = pending[worker_index]
                .take()
                .ok_or("campaign worker replied without a pending job")?;
            let (sequence, decisions) = core.admit_job(pending_job.parent_id, &result)?;
            if let Some(draw) = &pending_job.selector {
                let parent_index = usize::try_from(pending_job.parent_id)?;
                core.archive.record_selection(parent_index, draw);
                core.archive.record_selection_outcome(
                    parent_index,
                    decisions.iter().any(|decision| {
                        matches!(decision, SmbCampaignAdmissionDecision::Retained { .. })
                    }),
                    frames,
                )?;
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
                    let sender = job_senders[worker_index]
                        .as_ref()
                        .ok_or("campaign worker channel closed early")?;
                    sender
                        .send(spec)
                        .map_err(|_| "campaign worker exited before its next job")?;
                    in_flight += 1;
                }
                None => {
                    job_senders[worker_index] = None;
                }
            }
        }
        Ok(())
    })?;

    core.finish_curve();
    let stream_sha256 = writer.finish()?;
    Ok(build_report(
        &header,
        resolved.record,
        core,
        &counters,
        stream_sha256,
    ))
}

/// Replay a recorded campaign stream serially and rebuild its report.
///
/// Replay re-executes every recorded job from (parent id, mutation seed) on a
/// single target, verifies each result digest and frame count byte for byte,
/// re-applies the promoted retention rules, and verifies every recomputed
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
            select_frontier_resume_input(source, resume_from_identifier(&header.resume_policy)?)?
        }
        _ => return Err("campaign stream origin kind is not recognized".into()),
    };
    let resume_input_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&resume_input)?));
    if resume_input_sha256 != header.resume_input_sha256
        || resume_input.actions.len() != header.resume_actions
    {
        return Err("campaign replay resume input does not match the recorded stream".into());
    }

    let selector_policy = selector_from_identifier(&header.parent_scheduler)?;
    let retention_policy = retention_from_identifier(&header.retention_policy)?;
    let replacement_policy = replacement_from_identifier(&header.replacement_policy)?;
    let vocabulary = vocabulary_from_identifier(&header.controller_vocabulary)?;
    let replay_key_policy = key_policy_from_identifier(&header.key_policy)?;
    let replay_suffix_policy = suffix_policy_from_identifier(&header.suffix_policy)?;
    let replay_chord_policy = chord_policy_from_identifier(&header.chord_policy)?;
    let chord_origin = match origin_report {
        Some(report) => SmbCampaignOrigin::Archive {
            path: header.origin_path.clone().unwrap_or_default(),
            file_sha256: header.origin_archive_sha256.clone().unwrap_or_default(),
            report: Box::new(report.clone()),
        },
        None => SmbCampaignOrigin::Genesis,
    };
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
    let waypoint_policy = waypoint_from_identifier(&header.waypoint_policy)?;
    let mut core = CoordinatorCore::new(
        header.action_limit,
        selector_policy,
        retention_policy,
        header.archive_entry_limit,
        replay_key_policy,
        waypoint_policy,
        replacement_policy,
    );
    let mut counters = CampaignCounters::new(header.workers);
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    let frames_before = target.frames_clocked();
    core.bootstrap(&mut target, &[resume_input])?;
    counters.bootstrap_frames = target.frames_clocked().saturating_sub(frames_before);

    for line in record_lines {
        let record: SmbCampaignStreamRecord = serde_json::from_str(line)?;
        match record {
            SmbCampaignStreamRecord::Skip(skip) => {
                let parent_index = usize::try_from(skip.parent_id)?;
                if parent_index >= core.archive.entries.len() {
                    return Err("recorded skip names a parent the archive does not hold".into());
                }
                let skip_parent = usize::try_from(skip.parent_id)?;
                let skip_long = replay_suffix_policy
                    == SmbCampaignSuffixPolicy::OneOrTwoRegionLong48
                    && core
                        .archive
                        .entries
                        .get(skip_parent)
                        .is_some_and(|entry| core.archive.waypoint_contains(&entry.report.key));
                let draw_tables = recorded_chord_tables(
                    replay_chord_policy,
                    skip.chord_table_before.as_ref(),
                    &chord_versions,
                )?;
                let suffix = derive_suffix_sized(
                    skip.mutation_seed,
                    vocabulary,
                    skip_long,
                    replay_chord_policy,
                    draw_tables,
                )?;
                if !core.all_prefixes_archived(parent_index, &suffix) {
                    return Err("recorded skip is not a duplicate at its stream position".into());
                }
                let worker = usize::try_from(skip.worker)?;
                if worker >= counters.skips_per_worker.len() {
                    return Err("recorded skip names an unknown worker".into());
                }
                verify_selector_annotation(
                    selector_policy,
                    waypoint_policy,
                    skip.selector.as_ref(),
                )?;
                if let Some(draw) = &skip.selector {
                    core.archive.record_selection(parent_index, draw);
                }
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
                let job_long = replay_suffix_policy
                    == SmbCampaignSuffixPolicy::OneOrTwoRegionLong48
                    && core
                        .archive
                        .entries
                        .get(parent_index)
                        .is_some_and(|entry| core.archive.waypoint_contains(&entry.report.key));
                let draw_tables = recorded_chord_tables(
                    replay_chord_policy,
                    job.chord_table_before.as_ref(),
                    &chord_versions,
                )?;
                let suffix = derive_suffix_sized(
                    job.mutation_seed,
                    vocabulary,
                    job_long,
                    replay_chord_policy,
                    draw_tables,
                )?;
                let job_frames_before = target.frames_clocked();
                let result = execute_job(
                    &mut target,
                    &snapshot,
                    parent_actions,
                    parent_milestones,
                    &suffix,
                    SmbJobPolicies {
                        max_actions: header.action_limit,
                        retention_policy,
                        key_policy: replay_key_policy,
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
                verify_selector_annotation(
                    selector_policy,
                    waypoint_policy,
                    job.selector.as_ref(),
                )?;
                if let Some(draw) = &job.selector {
                    core.archive.record_selection(parent_index, draw);
                    core.archive.record_selection_outcome(
                        parent_index,
                        decisions.iter().any(|decision| {
                            matches!(decision, SmbCampaignAdmissionDecision::Retained { .. })
                        }),
                        job.frames,
                    )?;
                }
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

/// Fixed probe horizon shared by the refused-candidate grid and the
/// promoted admission probe.
const GRID_PROBE_FRAMES: u16 = 120;

/// Button mask applied at one probe frame of a grid schedule.
type GridMaskSchedule = fn(u16) -> u8;

/// One mask schedule of the refused-candidate probe grid: a name and the
/// button mask applied at each probe frame.
const GRID_MASK_SCHEDULES: [(&str, GridMaskSchedule); 6] = [
    ("still", |_| 0x00),
    ("held_right", |_| 0x01),
    ("stroke_right", |_| 0x81),
    ("stroke", |_| 0x80),
    ("stroke_left", |_| 0x82),
    // One expressible alternating swim cadence: press for 4 frames, release
    // for 12, period 16.
    ("stroke_alternating", |frame| {
        if frame % 16 < 4 { 0x80 } else { 0x00 }
    }),
];

/// One probe of one mask schedule from one refused candidate.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbGridProbeOutcome {
    /// Mask schedule name from the fixed grid.
    pub mask: String,
    /// Frames survived before the outcome, capped at the grid horizon.
    pub frames: u16,
    /// `survived`, `kill_state`, `below_play_area`, or `emulation_failed`.
    pub outcome: String,
}

/// One refused candidate re-derived from the recorded stream.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbRefusedCandidateProbe {
    /// Stream sequence of the job that produced the candidate.
    pub sequence: u64,
    /// Archive id of the parent the worker extended.
    pub parent_id: u64,
    /// Parent progress bucket at the frontier pair.
    pub parent_progress: u16,
    /// Candidate mechanical key.
    pub world: u8,
    /// Candidate mechanical key.
    pub level: u8,
    /// Candidate mechanical key.
    pub progress: u16,
    /// Camera pixels at the candidate state.
    pub camera: u32,
    /// Player vertical page at the candidate state; 2 is below the play area.
    pub vertical_page: u8,
    /// Player vertical position low byte at the candidate state.
    pub vertical_low: u8,
    /// Grid outcomes, one per mask schedule.
    pub probes: Vec<SmbGridProbeOutcome>,
}

/// Survival fractions for one mask schedule across the probed candidates.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbGridMaskAggregate {
    /// Mask schedule name.
    pub mask: String,
    /// Candidates surviving at least 45, 60, 90 and 120 frames.
    pub survived_at_45: u64,
    /// See `survived_at_45`.
    pub survived_at_60: u64,
    /// See `survived_at_45`.
    pub survived_at_90: u64,
    /// See `survived_at_45`.
    pub survived_at_120: u64,
}

/// Report of the refused-candidate probe grid over one recorded stream.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbRefusedGridReport {
    /// Frontier `(world, level)` pair the sample was drawn against.
    pub frontier_pair: (u8, u8),
    /// Inclusive parent-progress bounds of sampled refusal jobs.
    pub parent_range: (u16, u16),
    /// Inclusive candidate-progress bounds probed by the grid.
    pub candidate_range: (u16, u16),
    /// Maximum refusal jobs re-derived, in stream order.
    pub sample_cap: usize,
    /// Refusal jobs re-derived.
    pub jobs_sampled: usize,
    /// Refused candidates re-derived across the sampled jobs.
    pub refused_candidates: u64,
    /// Refused candidates inside the candidate range, each probed by the grid.
    pub probed_candidates: u64,
    /// Refused candidates whose key fell outside the candidate range, by key.
    pub out_of_range: Vec<((u8, u8, u16), u64)>,
    /// Probed candidates that survived a promoted-probe mask for the full
    /// horizon; any nonzero value means the re-derivation diverged.
    pub derivation_mismatches: u64,
    /// Survival fractions per mask schedule.
    pub aggregate: Vec<SmbGridMaskAggregate>,
    /// Every probed candidate with its grid outcomes.
    pub candidates: Vec<SmbRefusedCandidateProbe>,
}

/// Run one grid mask schedule from the current target state.
fn grid_probe(target: &mut SmbTarget, mask_for_frame: fn(u16) -> u8) -> (u16, String) {
    use crate::phase4b::{PLAYER_BELOW_PLAY_AREA_PAGE, PLAYER_KILLED_STATE, smb_death_bytes};
    for frame in 0..GRID_PROBE_FRAMES {
        target.apply(&ButtonChord::new(mask_for_frame(frame), 1));
        if target.exit_kind() != ExitKind::Ok {
            return (frame, "emulation_failed".to_owned());
        }
        let bytes = smb_death_bytes(target.wram());
        if bytes.engine_state == PLAYER_KILLED_STATE {
            return (frame.saturating_add(1), "kill_state".to_owned());
        }
        if bytes.vertical_page >= PLAYER_BELOW_PLAY_AREA_PAGE {
            return (frame.saturating_add(1), "below_play_area".to_owned());
        }
    }
    (GRID_PROBE_FRAMES, "survived".to_owned())
}

/// Re-derive probe-refused candidates from a recorded stream and probe each
/// under the fixed mask-schedule grid.
///
/// The sample is the first `sample_cap` job records, in stream order, that
/// carry at least one probe-refused decision and whose parent sits at the
/// frontier `(world, level)` pair within `parent_range`. Each sampled job is
/// re-derived exactly as the worker executed it: the parent's recorded input
/// is executed from reset, the suffix is re-derived from the recorded
/// mutation seed, and decision order maps one-to-one onto alive candidates.
/// Candidates whose recorded decision was probe-refused and whose key falls
/// inside `candidate_range` at the frontier pair are probed under every grid
/// schedule; refused candidates outside the range are tallied by key.
///
/// # Errors
///
/// Returns an error when the stream is malformed, the ROM does not match the
/// recorded header, a parent is missing from the source archive, or
/// emulation fails.
pub fn diagnose_refused_grid(
    rom: &[u8],
    stream_text: &str,
    source: &SmbArchiveReport,
    parent_range: (u16, u16),
    candidate_range: (u16, u16),
    sample_cap: usize,
) -> Result<SmbRefusedGridReport, Box<dyn Error>> {
    use crate::phase4b::smb_camera_pixels;
    use std::collections::BTreeMap;
    type EntryIndex<'a> = BTreeMap<u64, &'a crate::phase4c::SmbArchiveEntryReport>;
    let mut lines = stream_text.lines();
    let header: SmbCampaignStreamHeader =
        serde_json::from_str(lines.next().ok_or("campaign stream is empty")?)?;
    if header.format != CAMPAIGN_STREAM_FORMAT {
        return Err("campaign stream format is not recognized".into());
    }
    if header.rom_sha256 != format!("{:x}", Sha256::digest(rom)) {
        return Err("grid diagnosis ROM does not match the recorded stream".into());
    }
    let grid_vocabulary = vocabulary_from_identifier(&header.controller_vocabulary)?;
    let grid_key_policy = key_policy_from_identifier(&header.key_policy)?;
    let frontier_pair = source
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level))
        .max()
        .ok_or("source archive has no entries")?;
    let by_id: EntryIndex<'_> = source
        .entries
        .iter()
        .map(|entry| (entry.id, entry))
        .collect();
    struct SampledJob {
        sequence: u64,
        parent_id: u64,
        parent_progress: u16,
        mutation_seed: u64,
        decisions: Vec<SmbCampaignAdmissionDecision>,
    }
    let mut sample: Vec<SampledJob> = Vec::new();
    for line in lines {
        if sample.len() >= sample_cap {
            break;
        }
        let record: SmbCampaignStreamRecord = serde_json::from_str(line)?;
        let SmbCampaignStreamRecord::Job(job) = record else {
            continue;
        };
        if !job
            .decisions
            .iter()
            .any(|decision| matches!(decision, SmbCampaignAdmissionDecision::ProbeRefused))
        {
            continue;
        }
        let parent = by_id
            .get(&job.parent_id)
            .ok_or("recorded refusal names a parent the source archive does not hold")?;
        if (parent.key.world, parent.key.level) != frontier_pair
            || parent.key.progress < parent_range.0
            || parent.key.progress > parent_range.1
        {
            continue;
        }
        sample.push(SampledJob {
            sequence: job.sequence,
            parent_id: job.parent_id,
            parent_progress: parent.key.progress,
            mutation_seed: job.mutation_seed,
            decisions: job.decisions,
        });
    }
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    let mut parent_snapshots: BTreeMap<u64, SmbSnapshot> = BTreeMap::new();
    let mut out_of_range: BTreeMap<(u8, u8, u16), u64> = BTreeMap::new();
    let mut candidates: Vec<SmbRefusedCandidateProbe> = Vec::new();
    let mut refused_candidates = 0_u64;
    let mut derivation_mismatches = 0_u64;
    let jobs_sampled = sample.len();
    for job in &sample {
        let parent_snapshot = match parent_snapshots.entry(job.parent_id) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.get().clone(),
            std::collections::btree_map::Entry::Vacant(slot) => {
                let parent = by_id
                    .get(&job.parent_id)
                    .ok_or("sampled parent is missing from the source archive")?;
                target.reset();
                for action in &parent.input.actions {
                    target.apply(action);
                }
                if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                    return Err("a sampled parent input replays to a dead state".into());
                }
                let snapshot = target
                    .snapshot()
                    .ok_or("failed to snapshot a sampled parent")?;
                slot.insert(snapshot).clone()
            }
        };
        let parent_actions = by_id
            .get(&job.parent_id)
            .ok_or("sampled parent is missing from the source archive")?
            .input
            .actions
            .len();
        target.restore(&parent_snapshot)?;
        let suffix = derive_suffix(job.mutation_seed, grid_vocabulary)?;
        let mut length = parent_actions;
        let mut candidate_index = 0_usize;
        for action in &suffix {
            if target.is_dead() || length >= header.action_limit {
                break;
            }
            length = length.saturating_add(1);
            target.apply(action);
            let dead = target.is_dead();
            let failed = target.exit_kind() != ExitKind::Ok;
            if dead || failed {
                break;
            }
            let refused = matches!(
                job.decisions.get(candidate_index),
                Some(SmbCampaignAdmissionDecision::ProbeRefused)
            );
            if refused {
                refused_candidates = refused_candidates.saturating_add(1);
                let key = archive_key(target.wram(), grid_key_policy);
                if (key.world, key.level) == frontier_pair
                    && key.progress >= candidate_range.0
                    && key.progress <= candidate_range.1
                {
                    let camera = smb_camera_pixels(target.wram());
                    let death_bytes = crate::phase4b::smb_death_bytes(target.wram());
                    let resume = target
                        .snapshot()
                        .ok_or("failed to snapshot a refused candidate")?;
                    let mut probes = Vec::with_capacity(GRID_MASK_SCHEDULES.len());
                    for (name, schedule) in GRID_MASK_SCHEDULES {
                        target.restore(&resume)?;
                        let (frames, outcome) = grid_probe(&mut target, schedule);
                        probes.push(SmbGridProbeOutcome {
                            mask: (*name).to_owned(),
                            frames,
                            outcome,
                        });
                    }
                    if probes
                        .iter()
                        .take(3)
                        .any(|probe| probe.outcome == "survived")
                    {
                        derivation_mismatches = derivation_mismatches.saturating_add(1);
                    }
                    candidates.push(SmbRefusedCandidateProbe {
                        sequence: job.sequence,
                        parent_id: job.parent_id,
                        parent_progress: job.parent_progress,
                        world: key.world,
                        level: key.level,
                        progress: key.progress,
                        camera,
                        vertical_page: death_bytes.vertical_page,
                        vertical_low: death_bytes.vertical_low,
                        probes,
                    });
                    target.restore(&resume)?;
                } else {
                    *out_of_range
                        .entry((key.world, key.level, key.progress))
                        .or_insert(0) += 1;
                }
            }
            candidate_index = candidate_index.saturating_add(1);
        }
    }
    let aggregate = GRID_MASK_SCHEDULES
        .iter()
        .map(|(name, _)| {
            let survived_past = |horizon: u16| {
                candidates
                    .iter()
                    .filter_map(|candidate| {
                        candidate.probes.iter().find(|probe| probe.mask == *name)
                    })
                    .filter(|probe| probe.outcome == "survived" || probe.frames > horizon)
                    .count() as u64
            };
            SmbGridMaskAggregate {
                mask: (*name).to_owned(),
                survived_at_45: survived_past(45),
                survived_at_60: survived_past(60),
                survived_at_90: survived_past(90),
                survived_at_120: candidates
                    .iter()
                    .filter_map(|candidate| {
                        candidate.probes.iter().find(|probe| probe.mask == *name)
                    })
                    .filter(|probe| probe.outcome == "survived")
                    .count() as u64,
            }
        })
        .collect();
    Ok(SmbRefusedGridReport {
        frontier_pair,
        parent_range,
        candidate_range,
        sample_cap,
        jobs_sampled,
        refused_candidates,
        probed_candidates: candidates.len() as u64,
        out_of_range: out_of_range.into_iter().collect(),
        derivation_mismatches,
        aggregate,
        candidates,
    })
}

/// Candidate-boundary positions and decisions re-derived from a stream.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbXTransitReport {
    /// Frontier `(world, level)` pair sampled.
    pub frontier_pair: (u8, u8),
    /// Inclusive parent-progress bounds sampled.
    pub parent_range: (u16, u16),
    /// Jobs re-derived.
    pub jobs_sampled: usize,
    /// Candidate boundaries recorded.
    pub candidates: u64,
    /// Per 16-pixel level-x band: retained, rejected, probe-refused and
    /// duplicate candidate counts.
    pub bands: Vec<(u32, u64, u64, u64, u64)>,
}

/// Re-derive frontier jobs and histogram candidate-boundary player-x against
/// the recorded admission decisions.
///
/// # Errors
///
/// Returns an error when the stream is malformed, the ROM mismatches, a
/// parent is missing, or emulation fails.
// Wall-clock feeds stderr progress only; nothing timed is serialized.
#[allow(clippy::disallowed_methods)]
pub fn diagnose_x_transit(
    rom: &[u8],
    stream_text: &str,
    source: &SmbArchiveReport,
    origin: Option<&SmbArchiveReport>,
    parent_range: (u16, u16),
    sample_cap: usize,
    vertical_bands: bool,
) -> Result<SmbXTransitReport, Box<dyn Error>> {
    use std::collections::BTreeMap;
    type EntryIndex<'a> = BTreeMap<u64, &'a crate::phase4c::SmbArchiveEntryReport>;
    let mut lines = stream_text.lines();
    let header: SmbCampaignStreamHeader =
        serde_json::from_str(lines.next().ok_or("campaign stream is empty")?)?;
    if header.format != CAMPAIGN_STREAM_FORMAT {
        return Err("campaign stream format is not recognized".into());
    }
    if header.rom_sha256 != format!("{:x}", Sha256::digest(rom)) {
        return Err("x-transit ROM does not match the recorded stream".into());
    }
    let vocabulary = vocabulary_from_identifier(&header.controller_vocabulary)?;
    let frontier_pair = source
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level))
        .max()
        .ok_or("source archive has no entries")?;
    let by_id: EntryIndex<'_> = source
        .entries
        .iter()
        .map(|entry| (entry.id, entry))
        .collect();
    struct SampledJob {
        parent_id: u64,
        mutation_seed: u64,
        decisions: Vec<SmbCampaignAdmissionDecision>,
    }
    let mut sample: Vec<SampledJob> = Vec::new();
    for line in lines {
        if sample.len() >= sample_cap {
            break;
        }
        let record: SmbCampaignStreamRecord = serde_json::from_str(line)?;
        let SmbCampaignStreamRecord::Job(job) = record else {
            continue;
        };
        let Some(parent) = by_id.get(&job.parent_id) else {
            continue;
        };
        if (parent.key.world, parent.key.level) != frontier_pair
            || parent.key.progress < parent_range.0
            || parent.key.progress > parent_range.1
        {
            continue;
        }
        sample.push(SampledJob {
            parent_id: job.parent_id,
            mutation_seed: job.mutation_seed,
            decisions: job.decisions,
        });
    }
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    // The stream's recorded resume input comes from the run's ORIGIN archive,
    // which for derived-origin links is not the produced archive; the caller
    // passes it explicitly in that case.
    let base = select_frontier_resume_input(
        origin.unwrap_or(source),
        resume_from_identifier(&header.resume_policy)?,
    )?;
    let base_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&base)?));
    if base_sha256 != header.resume_input_sha256 {
        return Err("x-transit base input does not match the recorded resume input".into());
    }
    eprintln!(
        "x-transit: emulating shared prefix of {} actions once",
        base.actions.len()
    );
    target.reset();
    let mut boundary_snapshots: Vec<SmbSnapshot> = Vec::with_capacity(base.actions.len() + 1);
    boundary_snapshots.push(
        target
            .snapshot()
            .ok_or("failed to snapshot the transit genesis")?,
    );
    for action in &base.actions {
        target.apply(action);
        boundary_snapshots.push(
            target
                .snapshot()
                .ok_or("failed to snapshot a transit boundary")?,
        );
    }
    let mut parent_snapshots: BTreeMap<u64, SmbSnapshot> = BTreeMap::new();
    let mut bands: BTreeMap<u32, (u64, u64, u64, u64)> = BTreeMap::new();
    let mut candidates = 0_u64;
    let jobs_sampled = sample.len();
    for (index, job) in sample.iter().enumerate() {
        if index % 200 == 0 {
            eprintln!("x-transit: job {}/{}", index + 1, jobs_sampled);
        }
        let parent_snapshot = match parent_snapshots.entry(job.parent_id) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.get().clone(),
            std::collections::btree_map::Entry::Vacant(slot) => {
                let parent = by_id
                    .get(&job.parent_id)
                    .ok_or("sampled parent is missing from the source archive")?;
                let shared = parent
                    .input
                    .actions
                    .iter()
                    .zip(&base.actions)
                    .take_while(|(a, b)| a == b)
                    .count();
                target.restore(&boundary_snapshots[shared])?;
                for action in &parent.input.actions[shared..] {
                    target.apply(action);
                }
                if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                    return Err("a sampled parent input replays to a dead state".into());
                }
                let snapshot = target
                    .snapshot()
                    .ok_or("failed to snapshot a sampled parent")?;
                slot.insert(snapshot).clone()
            }
        };
        target.restore(&parent_snapshot)?;
        let suffix = derive_suffix(job.mutation_seed, vocabulary)?;
        for (candidate_index, chord) in suffix.iter().enumerate() {
            if target.is_dead() {
                break;
            }
            target.apply(chord);
            if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                break;
            }
            let wram = target.wram();
            let band = if vertical_bands {
                let state = crate::phase4b::smb_mechanical_state_from_wram(wram);
                u32::from(state.progress) * 100 + u32::from(state.player_y_bucket)
            } else {
                let x = u32::from(wram[PLAYER_HORIZONTAL_PAGE_OFFSET]) * 256
                    + u32::from(wram[PLAYER_HORIZONTAL_LOW_OFFSET]);
                (x / 16) * 16
            };
            let slot = bands.entry(band).or_insert((0, 0, 0, 0));
            match job.decisions.get(candidate_index) {
                Some(SmbCampaignAdmissionDecision::Retained { .. }) => slot.0 += 1,
                Some(SmbCampaignAdmissionDecision::Rejected) => slot.1 += 1,
                Some(
                    SmbCampaignAdmissionDecision::ProbeRefused
                    | SmbCampaignAdmissionDecision::SnapRefused,
                ) => slot.2 += 1,
                Some(SmbCampaignAdmissionDecision::Duplicate { .. }) | None => slot.3 += 1,
            }
            candidates += 1;
        }
    }
    Ok(SmbXTransitReport {
        frontier_pair,
        parent_range,
        jobs_sampled,
        candidates,
        bands: bands
            .into_iter()
            .map(|(band, (a, b, c, d))| (band, a, b, c, d))
            .collect(),
    })
}

/// One probed frontier entry of the loop differential.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbLoopProbeRecord {
    /// Archive entry id.
    pub entry_id: u64,
    /// Entry progress bucket.
    pub progress: u16,
    /// `advanced`, `looped`, `dead`, or `held`.
    pub outcome: String,
    /// Maximum progress bucket observed during the probe.
    pub max_progress: u16,
    /// Minimum progress bucket observed during the probe.
    pub min_progress: u16,
}

/// One discriminating work-RAM byte between advancing and looping states.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbLoopDiscriminator {
    /// Work-RAM offset.
    pub offset: usize,
    /// Distinct (value, count) pairs among advancing states.
    pub advanced_values: Vec<(u8, u64)>,
    /// Distinct (value, count) pairs among looping states.
    pub looped_values: Vec<(u8, u64)>,
    /// Whether the byte perfectly separates the two classes.
    pub separates: bool,
}

/// Report of the loop-differential probe over one recorded archive.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbLoopDifferentialReport {
    /// Frontier `(world, level)` pair probed.
    pub frontier_pair: (u8, u8),
    /// Inclusive entry-progress bounds sampled.
    pub bucket_range: (u16, u16),
    /// Entries probed.
    pub probed: usize,
    /// Outcome counts: advanced, looped, dead, held.
    pub outcomes: (u64, u64, u64, u64),
    /// Bytes that perfectly separate advancing from looping states, plus the
    /// strongest imperfect discriminators.
    pub discriminators: Vec<SmbLoopDiscriminator>,
    /// Every probed entry.
    pub probes: Vec<SmbLoopProbeRecord>,
}

/// Probe frontier entries forward under held Right and diff the starting
/// work RAM of advancing states against looping ones.
///
/// # Errors
///
/// Returns an error when the archive is empty or emulation fails.
// Wall-clock feeds stderr progress only; nothing timed is serialized.
#[allow(clippy::disallowed_methods)]
/// One work-RAM byte's value distributions across two entry groups.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbWramDiffByte {
    /// Work-RAM offset.
    pub offset: usize,
    /// Distinct (value, count) pairs in group A.
    pub group_a_values: Vec<(u8, u64)>,
    /// Distinct (value, count) pairs in group B.
    pub group_b_values: Vec<(u8, u64)>,
    /// Whether the byte's value sets are disjoint between the groups.
    pub separates: bool,
    /// Distinct value count inside group A, for packed-state hunting.
    pub group_a_modes: usize,
}

/// Report of the two-group work-RAM differential over one archive.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbWramDiffReport {
    /// Frontier `(world, level)` pair sampled.
    pub frontier_pair: (u8, u8),
    /// Group A inclusive bucket range.
    pub group_a: (u16, u16),
    /// Group B inclusive bucket range.
    pub group_b: (u16, u16),
    /// Entries replayed per group.
    pub sampled: (usize, usize),
    /// Bytes that separate the groups, then the strongest non-separators.
    pub bytes: Vec<SmbWramDiffByte>,
}

/// Replay two bucket-range groups of frontier entries and diff their work
/// RAM byte by byte, reporting separators and multi-modal in-group bytes.
///
/// # Errors
///
/// Returns an error when the archive is empty or emulation fails.
// Wall-clock feeds stderr progress only; nothing timed is serialized.
#[allow(clippy::disallowed_methods)]
pub fn diagnose_wram_diff(
    rom: &[u8],
    source: &SmbArchiveReport,
    group_a: (u16, u16),
    group_b: (u16, u16),
    cap_per_group: usize,
    output_bytes: usize,
) -> Result<SmbWramDiffReport, Box<dyn Error>> {
    use crate::phase4b::WRAM_SIZE;
    let frontier_pair = source
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level))
        .max()
        .ok_or("source archive has no entries")?;
    // This diagnostic reads an archive with no stream beside it, so it names
    // the frozen rule explicitly rather than inferring one.
    let base = select_frontier_resume_input(source, SmbCampaignResumePolicy::FrontierShortest)?;
    let collect = |low: u16, high: u16| {
        let mut picks: Vec<&crate::phase4c::SmbArchiveEntryReport> = source
            .entries
            .iter()
            .filter(|entry| {
                (entry.key.world, entry.key.level) == frontier_pair
                    && entry.key.progress >= low
                    && entry.key.progress <= high
            })
            .collect();
        picks.sort_by_key(|entry| entry.id);
        picks.truncate(cap_per_group);
        picks
    };
    let picks_a = collect(group_a.0, group_a.1);
    let picks_b = collect(group_b.0, group_b.1);
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    eprintln!(
        "wram-diff: emulating shared prefix of {} actions once",
        base.actions.len()
    );
    target.reset();
    let mut boundary_snapshots: Vec<SmbSnapshot> = Vec::with_capacity(base.actions.len() + 1);
    boundary_snapshots.push(
        target
            .snapshot()
            .ok_or("failed to snapshot the wram-diff genesis")?,
    );
    for action in &base.actions {
        target.apply(action);
        boundary_snapshots.push(
            target
                .snapshot()
                .ok_or("failed to snapshot a wram-diff boundary")?,
        );
    }
    let mut replay_group = |picks: &[&crate::phase4c::SmbArchiveEntryReport],
                            tag: &str|
     -> Result<Vec<[u8; WRAM_SIZE]>, Box<dyn Error>> {
        let mut wrams = Vec::with_capacity(picks.len());
        for (index, entry) in picks.iter().enumerate() {
            if index % 100 == 0 {
                eprintln!("wram-diff: {tag} {}/{}", index + 1, picks.len());
            }
            let shared = entry
                .input
                .actions
                .iter()
                .zip(&base.actions)
                .take_while(|(a, b)| a == b)
                .count();
            target.restore(&boundary_snapshots[shared])?;
            for action in &entry.input.actions[shared..] {
                target.apply(action);
            }
            if target.exit_kind() != ExitKind::Ok {
                return Err("a sampled entry failed to replay".into());
            }
            wrams.push(*target.wram());
        }
        Ok(wrams)
    };
    let wram_a = replay_group(&picks_a, "group-a")?;
    let wram_b = replay_group(&picks_b, "group-b")?;
    let mut scored: Vec<(usize, bool, usize, f64)> = Vec::new();
    for offset in 0..WRAM_SIZE {
        let mut a_vals = std::collections::BTreeMap::<u8, u64>::new();
        for wram in &wram_a {
            *a_vals.entry(wram[offset]).or_insert(0) += 1;
        }
        let mut b_vals = std::collections::BTreeMap::<u8, u64>::new();
        for wram in &wram_b {
            *b_vals.entry(wram[offset]).or_insert(0) += 1;
        }
        let separates = !wram_a.is_empty()
            && !wram_b.is_empty()
            && a_vals.keys().all(|value| !b_vals.contains_key(value));
        let a_mean =
            wram_a.iter().map(|w| f64::from(w[offset])).sum::<f64>() / wram_a.len().max(1) as f64;
        let b_mean =
            wram_b.iter().map(|w| f64::from(w[offset])).sum::<f64>() / wram_b.len().max(1) as f64;
        let score = (a_mean - b_mean).abs();
        if separates || score > 0.0 || a_vals.len() > 1 {
            scored.push((offset, separates, a_vals.len(), score));
        }
    }
    scored.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then(b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
    });
    scored.truncate(output_bytes);
    let mut bytes = Vec::with_capacity(scored.len());
    for (offset, separates, modes, _) in scored {
        let mut a_vals = std::collections::BTreeMap::<u8, u64>::new();
        for wram in &wram_a {
            *a_vals.entry(wram[offset]).or_insert(0) += 1;
        }
        let mut b_vals = std::collections::BTreeMap::<u8, u64>::new();
        for wram in &wram_b {
            *b_vals.entry(wram[offset]).or_insert(0) += 1;
        }
        bytes.push(SmbWramDiffByte {
            offset,
            group_a_values: a_vals.into_iter().collect(),
            group_b_values: b_vals.into_iter().collect(),
            separates,
            group_a_modes: modes,
        });
    }
    Ok(SmbWramDiffReport {
        frontier_pair,
        group_a,
        group_b,
        sampled: (wram_a.len(), wram_b.len()),
        bytes,
    })
}

pub fn diagnose_loop_differential(
    rom: &[u8],
    source: &SmbArchiveReport,
    bucket_range: (u16, u16),
    advance_threshold: u16,
    sample_cap: usize,
    probe_chords: u16,
    output_discriminators: usize,
) -> Result<SmbLoopDifferentialReport, Box<dyn Error>> {
    use crate::phase4b::WRAM_SIZE;
    let frontier_pair = source
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level))
        .max()
        .ok_or("source archive has no entries")?;
    // Archive-only diagnostic with no stream beside it: the frozen rule is
    // named explicitly rather than inferred.
    let base = select_frontier_resume_input(source, SmbCampaignResumePolicy::FrontierShortest)?;
    let mut sample: Vec<&crate::phase4c::SmbArchiveEntryReport> = source
        .entries
        .iter()
        .filter(|entry| {
            (entry.key.world, entry.key.level) == frontier_pair
                && entry.key.progress >= bucket_range.0
                && entry.key.progress <= bucket_range.1
        })
        .collect();
    sample.sort_by_key(|entry| entry.id);
    sample.truncate(sample_cap);
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    eprintln!(
        "loop-diff: emulating shared prefix of {} actions once",
        base.actions.len()
    );
    target.reset();
    let mut boundary_snapshots: Vec<SmbSnapshot> = Vec::with_capacity(base.actions.len() + 1);
    boundary_snapshots.push(
        target
            .snapshot()
            .ok_or("failed to snapshot the loop-diff genesis")?,
    );
    for action in &base.actions {
        target.apply(action);
        boundary_snapshots.push(
            target
                .snapshot()
                .ok_or("failed to snapshot a loop-diff boundary")?,
        );
    }
    let mut probes: Vec<SmbLoopProbeRecord> = Vec::new();
    let mut advanced_wram: Vec<[u8; WRAM_SIZE]> = Vec::new();
    let mut looped_wram: Vec<[u8; WRAM_SIZE]> = Vec::new();
    for (index, entry) in sample.iter().enumerate() {
        if index % 50 == 0 {
            eprintln!("loop-diff: entry {}/{}", index + 1, sample.len());
        }
        let shared = entry
            .input
            .actions
            .iter()
            .zip(&base.actions)
            .take_while(|(a, b)| a == b)
            .count();
        target.restore(&boundary_snapshots[shared])?;
        for action in &entry.input.actions[shared..] {
            target.apply(action);
        }
        if target.is_dead() || target.exit_kind() != ExitKind::Ok {
            probes.push(SmbLoopProbeRecord {
                entry_id: entry.id,
                progress: entry.key.progress,
                outcome: "dead".to_owned(),
                max_progress: entry.key.progress,
                min_progress: entry.key.progress,
            });
            continue;
        }
        let start_wram = *target.wram();
        let mut max_progress = entry.key.progress;
        let mut min_progress = entry.key.progress;
        let mut died = false;
        for _ in 0..probe_chords {
            target.apply(&ButtonChord::new(0x80, 60));
            let state = crate::phase4b::smb_mechanical_state_from_wram(target.wram());
            if (state.world, state.level) == frontier_pair {
                max_progress = max_progress.max(state.progress);
                min_progress = min_progress.min(state.progress);
            } else if (state.world, state.level) > frontier_pair {
                max_progress = u16::MAX;
            }
            if target.is_dead() {
                died = true;
                break;
            }
        }
        let outcome = if max_progress > advance_threshold {
            advanced_wram.push(start_wram);
            "advanced"
        } else if min_progress + 4 < entry.key.progress {
            looped_wram.push(start_wram);
            "looped"
        } else if died {
            "dead"
        } else {
            "held"
        };
        probes.push(SmbLoopProbeRecord {
            entry_id: entry.id,
            progress: entry.key.progress,
            outcome: outcome.to_owned(),
            max_progress,
            min_progress,
        });
    }
    let mut discriminators: Vec<SmbLoopDiscriminator> = Vec::new();
    if !advanced_wram.is_empty() && !looped_wram.is_empty() {
        let mut scored: Vec<(usize, bool, f64)> = Vec::new();
        for offset in 0..WRAM_SIZE {
            let mut a_vals = std::collections::BTreeMap::<u8, u64>::new();
            for wram in &advanced_wram {
                *a_vals.entry(wram[offset]).or_insert(0) += 1;
            }
            let mut l_vals = std::collections::BTreeMap::<u8, u64>::new();
            for wram in &looped_wram {
                *l_vals.entry(wram[offset]).or_insert(0) += 1;
            }
            let separates = a_vals.keys().all(|value| !l_vals.contains_key(value));
            let a_mean = advanced_wram
                .iter()
                .map(|wram| f64::from(wram[offset]))
                .sum::<f64>()
                / advanced_wram.len() as f64;
            let l_mean = looped_wram
                .iter()
                .map(|wram| f64::from(wram[offset]))
                .sum::<f64>()
                / looped_wram.len() as f64;
            let score = (a_mean - l_mean).abs();
            if separates || score > 0.0 {
                scored.push((offset, separates, score));
            }
        }
        scored.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then(b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
        });
        scored.truncate(output_discriminators);
        for (offset, separates, _) in scored {
            let mut a_vals = std::collections::BTreeMap::<u8, u64>::new();
            for wram in &advanced_wram {
                *a_vals.entry(wram[offset]).or_insert(0) += 1;
            }
            let mut l_vals = std::collections::BTreeMap::<u8, u64>::new();
            for wram in &looped_wram {
                *l_vals.entry(wram[offset]).or_insert(0) += 1;
            }
            discriminators.push(SmbLoopDiscriminator {
                offset,
                advanced_values: a_vals.into_iter().collect(),
                looped_values: l_vals.into_iter().collect(),
                separates,
            });
        }
    }
    let outcomes = (
        probes.iter().filter(|p| p.outcome == "advanced").count() as u64,
        probes.iter().filter(|p| p.outcome == "looped").count() as u64,
        probes.iter().filter(|p| p.outcome == "dead").count() as u64,
        probes.iter().filter(|p| p.outcome == "held").count() as u64,
    );
    Ok(SmbLoopDifferentialReport {
        frontier_pair,
        bucket_range,
        probed: probes.len(),
        outcomes,
        discriminators,
        probes,
    })
}

/// SMB player horizontal page byte, `$006d`.
const PLAYER_HORIZONTAL_PAGE_OFFSET: usize = 0x006d;
/// SMB player horizontal position byte within the page, `$0086`.
const PLAYER_HORIZONTAL_LOW_OFFSET: usize = 0x0086;

/// One Down press re-derived from a recorded stream.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbDownPressRecord {
    /// Stream sequence of the job.
    pub sequence: u64,
    /// Parent archive id.
    pub parent_id: u64,
    /// Player level-x before the press: page times 256 plus low byte.
    pub player_x: u32,
    /// Player screen-x before the press: level-x minus camera pixels.
    pub screen_x: i64,
    /// Player vertical page before the press.
    pub vertical_page: u8,
    /// Player vertical low byte before the press.
    pub vertical_low: u8,
    /// Engine state before the press.
    pub engine_state_before: u8,
    /// Engine state after the held chord.
    pub engine_state_after: u8,
    /// World byte after the held chord.
    pub world_after: u8,
    /// Whether the chord ended dead.
    pub dead: bool,
}

/// Report of the Down-press census over one recorded stream.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbDownCensusReport {
    /// Frontier `(world, level)` pair sampled.
    pub frontier_pair: (u8, u8),
    /// Inclusive parent-progress bounds sampled.
    pub parent_range: (u16, u16),
    /// Maximum Down-carrying jobs re-derived, in stream order.
    pub sample_cap: usize,
    /// Jobs re-derived.
    pub jobs_sampled: usize,
    /// Down presses recorded.
    pub down_presses: u64,
    /// Presses whose engine state changed across the hold.
    pub engine_state_changes: u64,
    /// Presses after which the world byte differed from the frontier world.
    pub world_changes: u64,
    /// Distinct sampled parents with their player level-x and vertical low.
    pub parent_positions: Vec<(u64, u32, u8)>,
    /// Every Down press.
    pub presses: Vec<SmbDownPressRecord>,
}

/// Re-derive jobs whose suffix presses Down from frontier parents and record
/// where the player stood when Down was pressed and what it did.
///
/// # Errors
///
/// Returns an error when the stream is malformed, the ROM mismatches, a
/// parent is missing, or emulation fails.
// Wall-clock here feeds stderr cost diagnostics only; nothing timed is ever
// serialized into an artifact, so determinism is not in play.
#[allow(clippy::disallowed_methods)]
pub fn diagnose_down_census(
    rom: &[u8],
    stream_text: &str,
    source: &SmbArchiveReport,
    parent_range: (u16, u16),
    sample_cap: usize,
) -> Result<SmbDownCensusReport, Box<dyn Error>> {
    use crate::phase4b::smb_camera_pixels;
    use std::collections::BTreeMap;
    type EntryIndex<'a> = BTreeMap<u64, &'a crate::phase4c::SmbArchiveEntryReport>;
    let mut lines = stream_text.lines();
    let header: SmbCampaignStreamHeader =
        serde_json::from_str(lines.next().ok_or("campaign stream is empty")?)?;
    if header.format != CAMPAIGN_STREAM_FORMAT {
        return Err("campaign stream format is not recognized".into());
    }
    if header.rom_sha256 != format!("{:x}", Sha256::digest(rom)) {
        return Err("down census ROM does not match the recorded stream".into());
    }
    let vocabulary = vocabulary_from_identifier(&header.controller_vocabulary)?;
    let frontier_pair = source
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level))
        .max()
        .ok_or("source archive has no entries")?;
    let by_id: EntryIndex<'_> = source
        .entries
        .iter()
        .map(|entry| (entry.id, entry))
        .collect();
    struct SampledJob {
        sequence: u64,
        parent_id: u64,
        mutation_seed: u64,
    }
    let mut sample: Vec<SampledJob> = Vec::new();
    for line in lines {
        if sample.len() >= sample_cap {
            break;
        }
        let record: SmbCampaignStreamRecord = serde_json::from_str(line)?;
        let SmbCampaignStreamRecord::Job(job) = record else {
            continue;
        };
        let Some(parent) = by_id.get(&job.parent_id) else {
            continue;
        };
        if (parent.key.world, parent.key.level) != frontier_pair
            || parent.key.progress < parent_range.0
            || parent.key.progress > parent_range.1
        {
            continue;
        }
        let suffix = derive_suffix(job.mutation_seed, vocabulary)?;
        if !suffix.iter().any(|chord| chord.buttons & 0x20 != 0) {
            continue;
        }
        sample.push(SampledJob {
            sequence: job.sequence,
            parent_id: job.parent_id,
            mutation_seed: job.mutation_seed,
        });
    }
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    // Every frontier parent extends the run's resume input, so the shared
    // prefix is emulated once and each parent replays only its tail. Without
    // this the census re-emulates roughly 146,000 frames per parent — the
    // cost blowup that hung the first D73 launch.
    let base =
        select_frontier_resume_input(source, resume_from_identifier(&header.resume_policy)?)?;
    let base_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&base)?));
    if base_sha256 != header.resume_input_sha256 {
        return Err("census base input does not match the recorded resume input".into());
    }
    eprintln!(
        "down-census: emulating shared prefix of {} actions once, snapshotting every boundary",
        base.actions.len()
    );
    target.reset();
    let mut boundary_snapshots: Vec<SmbSnapshot> = Vec::with_capacity(base.actions.len() + 1);
    boundary_snapshots.push(
        target
            .snapshot()
            .ok_or("failed to snapshot the census genesis")?,
    );
    for action in &base.actions {
        target.apply(action);
        boundary_snapshots.push(
            target
                .snapshot()
                .ok_or("failed to snapshot a census boundary")?,
        );
    }
    if target.is_dead() || target.exit_kind() != ExitKind::Ok {
        return Err("the shared resume prefix replays to a dead state".into());
    }
    const CENSUS_FRAME_BUDGET: u64 = 20_000_000;
    let frames_at_start = target.frames_clocked();
    let mut parent_snapshots: BTreeMap<u64, SmbSnapshot> = BTreeMap::new();
    let mut parent_positions: BTreeMap<u64, (u32, u8)> = BTreeMap::new();
    let mut presses: Vec<SmbDownPressRecord> = Vec::new();
    let jobs_sampled = sample.len();
    for (index, job) in sample.iter().enumerate() {
        if target.frames_clocked().saturating_sub(frames_at_start) > CENSUS_FRAME_BUDGET {
            return Err("down census exceeded its hard frame budget".into());
        }
        let phase_started = std::time::Instant::now();
        let parent_snapshot = match parent_snapshots.entry(job.parent_id) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.get().clone(),
            std::collections::btree_map::Entry::Vacant(slot) => {
                let parent = by_id
                    .get(&job.parent_id)
                    .ok_or("sampled parent is missing from the source archive")?;
                // Restore from the longest common prefix with the base
                // bootstrap: every archive lineage shares its history with
                // some boundary of the resume input, so the replayed tail is
                // short regardless of which bootstrap entry it descends from.
                let shared = parent
                    .input
                    .actions
                    .iter()
                    .zip(&base.actions)
                    .take_while(|(a, b)| a == b)
                    .count();
                let tail = parent.input.actions[shared..].to_vec();
                let restore_started = std::time::Instant::now();
                target.restore(&boundary_snapshots[shared])?;
                let restored = restore_started.elapsed().as_millis();
                let apply_started = std::time::Instant::now();
                for action in &tail {
                    target.apply(action);
                }
                eprintln!(
                    "down-census: parent {} shared {} tail {} restore_ms {} apply_ms {}",
                    job.parent_id,
                    shared,
                    tail.len(),
                    restored,
                    apply_started.elapsed().as_millis()
                );
                if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                    return Err("a sampled parent input replays to a dead state".into());
                }
                let wram = target.wram();
                let x = u32::from(wram[PLAYER_HORIZONTAL_PAGE_OFFSET]) * 256
                    + u32::from(wram[PLAYER_HORIZONTAL_LOW_OFFSET]);
                parent_positions.insert(job.parent_id, (x, wram[0x00ce]));
                let snapshot_started = std::time::Instant::now();
                let snapshot = target
                    .snapshot()
                    .ok_or("failed to snapshot a sampled parent")?;
                eprintln!(
                    "down-census: parent {} snapshot_ms {} snapshot_bytes {}",
                    job.parent_id,
                    snapshot_started.elapsed().as_millis(),
                    snapshot.emulator_state_len()
                );
                slot.insert(snapshot).clone()
            }
        };
        eprintln!(
            "down-census: job {}/{} sequence {} parent {} frames {} job_setup_ms {}",
            index + 1,
            jobs_sampled,
            job.sequence,
            job.parent_id,
            target.frames_clocked().saturating_sub(frames_at_start),
            phase_started.elapsed().as_millis()
        );
        target.restore(&parent_snapshot)?;
        let suffix = derive_suffix(job.mutation_seed, vocabulary)?;
        for chord in &suffix {
            if target.is_dead() {
                break;
            }
            let is_down = chord.buttons & 0x20 != 0;
            let (player_x, screen_x, vertical_page, vertical_low, engine_before) = if is_down {
                let wram = target.wram();
                let x = u32::from(wram[PLAYER_HORIZONTAL_PAGE_OFFSET]) * 256
                    + u32::from(wram[PLAYER_HORIZONTAL_LOW_OFFSET]);
                let camera = i64::from(smb_camera_pixels(wram));
                let bytes = crate::phase4b::smb_death_bytes(wram);
                (
                    x,
                    i64::from(x) - camera,
                    bytes.vertical_page,
                    bytes.vertical_low,
                    bytes.engine_state,
                )
            } else {
                (0, 0, 0, 0, 0)
            };
            target.apply(chord);
            if is_down {
                let wram = target.wram();
                let bytes = crate::phase4b::smb_death_bytes(wram);
                presses.push(SmbDownPressRecord {
                    sequence: job.sequence,
                    parent_id: job.parent_id,
                    player_x,
                    screen_x,
                    vertical_page,
                    vertical_low,
                    engine_state_before: engine_before,
                    engine_state_after: bytes.engine_state,
                    world_after: bytes.world,
                    dead: target.is_dead(),
                });
            }
        }
    }
    let engine_state_changes = presses
        .iter()
        .filter(|press| press.engine_state_after != press.engine_state_before)
        .count() as u64;
    let world_changes = presses
        .iter()
        .filter(|press| press.world_after != frontier_pair.0)
        .count() as u64;
    Ok(SmbDownCensusReport {
        frontier_pair,
        parent_range,
        sample_cap,
        jobs_sampled,
        down_presses: presses.len() as u64,
        engine_state_changes,
        world_changes,
        parent_positions: parent_positions
            .into_iter()
            .map(|(id, (x, y))| (id, x, y))
            .collect(),
        presses,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        CoordinatorCore, SmbCampaignActionResult, SmbCampaignAdmissionDecision,
        SmbCampaignCandidate, SmbCampaignConfig, SmbCampaignJobResult, SmbCampaignOrigin,
        SmbCampaignResumePolicy, SmbCampaignStreamHeader, chord_policy_from_identifier,
        chord_policy_identifier, derive_suffix, derive_worker_seed, execute_job,
        replacement_from_identifier, replacement_identifier, replay_smb_campaign,
        resume_from_identifier, resume_identifier, run_smb_campaign, select_frontier_resume_input,
        waypoint_from_identifier, waypoint_identifier,
    };
    use crate::{
        chord_table::ChordTableParameters,
        draw_budget::DrawBudgetParameters,
        phase4b::{ButtonChord, SmbInput, SmbMilestones, SmbTarget},
        phase4c::{
            ArchiveCandidate, SmbArchiveKey, SmbArchiveReplacementPolicy, SmbArchiveReport,
            SmbArchiveRetentionPolicy, SmbArchiveSelectorPolicy, SmbArchiveWaypointPolicy,
        },
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
            let first = derive_suffix(seed, super::SmbCampaignVocabulary::FrozenNineMask)
                .expect("derive suffix");
            let second = derive_suffix(seed, super::SmbCampaignVocabulary::FrozenNineMask)
                .expect("derive suffix again");
            assert_eq!(first, second);
            assert!((1..=2).contains(&first.len()));
            assert!(
                first
                    .iter()
                    .all(|chord| (1..=120).contains(&chord.hold_frames))
            );
        }
    }

    fn derived_policy() -> super::SmbCampaignChordPolicy {
        super::SmbCampaignChordPolicy::DerivedHalf(super::SmbChordTableDerivation {
            source_filter: super::SmbChordSourceFilter {
                world: 0,
                level: 0,
                minimum_progress: 0,
            },
            parameters: ChordTableParameters {
                prefix_steps: 0,
                recent_successes: 4,
                recent_weight: 3,
                all_history_weight: 1,
                update_every_records: 2,
                hash_every_records: 2,
            },
        })
    }

    fn budgeted_selector() -> SmbArchiveSelectorPolicy {
        SmbArchiveSelectorPolicy::YieldBudgeted(DrawBudgetParameters {
            history_window: 16,
            exploration_floor: 4,
            maximum_draws: 64,
            success_cost_scale: 256,
        })
    }

    #[test]
    fn budgeted_selector_identifier_round_trips() {
        let policy = budgeted_selector();
        let identifier = super::selector_identifier(policy);
        assert_eq!(
            super::selector_from_identifier(&identifier).expect("parse budget selector"),
            policy
        );
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
            chord_policy_identifier(super::SmbCampaignChordPolicy::RecordedHalf),
            "chord_draw_recorded_50"
        );
        assert_eq!(
            chord_policy_from_identifier("chord_draw_recorded_50")
                .expect("parse legacy constant-backed policy"),
            super::SmbCampaignChordPolicy::RecordedHalf
        );
    }

    #[test]
    fn job_execution_is_pure_across_target_instances() {
        let rom = synthetic_nrom();
        let mut first = SmbTarget::from_smb_rom_bytes_headless(&rom).expect("load first target");
        let mut second = SmbTarget::from_smb_rom_bytes_headless(&rom).expect("load second target");
        first.reset();
        first.apply(&ButtonChord::new(0x81, 12));
        let snapshot = first.snapshot().expect("snapshot prefix");
        let suffix = derive_suffix(0x5eed_ca02, super::SmbCampaignVocabulary::FrozenNineMask)
            .expect("derive suffix");
        // Disturb the first instance so the job must depend on the snapshot alone.
        first.apply(&ButtonChord::new(0x02, 30));
        let on_first = execute_job(
            &mut first,
            &snapshot,
            1,
            SmbMilestones::default(),
            &suffix,
            super::SmbJobPolicies {
                max_actions: 96,
                retention_policy: SmbArchiveRetentionPolicy::ProbeAtAdmission,
                key_policy: crate::phase4c::SmbArchiveKeyPolicy::Frozen,
            },
        )
        .expect("execute job on first instance");
        let on_second = execute_job(
            &mut second,
            &snapshot,
            1,
            SmbMilestones::default(),
            &suffix,
            super::SmbJobPolicies {
                max_actions: 96,
                retention_policy: SmbArchiveRetentionPolicy::ProbeAtAdmission,
                key_policy: crate::phase4c::SmbArchiveKeyPolicy::Frozen,
            },
        )
        .expect("execute job on second instance");
        assert_eq!(on_first, on_second);
    }

    #[test]
    fn live_campaign_replays_byte_identically() {
        let rom = synthetic_nrom();
        let config = SmbCampaignConfig {
            campaign_seed: 0x5eed_ca03,
            workers: 4,
            execution_budget: 32,
            action_limit: 96,
            host: "unit-test".to_owned(),
            wall_budget: None,
            selector_policy: SmbArchiveSelectorPolicy::ConcentratedRecency,
            retention_policy: SmbArchiveRetentionPolicy::ProbeAtAdmission,
            archive_entry_limit: 32_768,
            vocabulary: super::SmbCampaignVocabulary::FrozenNineMask,
            key_policy: crate::phase4c::SmbArchiveKeyPolicy::Frozen,
            waypoint_policy: crate::phase4c::SmbArchiveWaypointPolicy::Absent,
            suffix: super::SmbCampaignSuffixPolicy::OneOrTwo,
            chord: super::SmbCampaignChordPolicy::Uniform,
            replacement_policy: crate::phase4c::SmbArchiveReplacementPolicy::FewestActions,
            resume_policy: SmbCampaignResumePolicy::FrontierShortest,
        };
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("live campaign");
        assert_eq!(live.executions_completed, 32);
        assert_eq!(live.jobs_per_worker.iter().sum::<u64>(), 32);
        let replayed = replay_smb_campaign(&rom, &stream, None).expect("replay recorded campaign");
        assert_eq!(live, replayed);
        let live_bytes = serde_json::to_vec_pretty(&live).expect("serialize live report");
        let replay_bytes = serde_json::to_vec_pretty(&replayed).expect("serialize replayed report");
        assert_eq!(live_bytes, replay_bytes);
    }

    #[test]
    fn continuous_chord_tables_replay_with_recorded_versions() {
        let rom = synthetic_nrom();
        let config = SmbCampaignConfig {
            campaign_seed: 0x5eed_ca13,
            workers: 3,
            execution_budget: 12,
            action_limit: 96,
            host: "unit-test".to_owned(),
            wall_budget: None,
            selector_policy: SmbArchiveSelectorPolicy::ConcentratedRecency,
            retention_policy: SmbArchiveRetentionPolicy::Frozen,
            archive_entry_limit: 32_768,
            vocabulary: super::SmbCampaignVocabulary::FrozenNineMask,
            key_policy: crate::phase4c::SmbArchiveKeyPolicy::Frozen,
            waypoint_policy: crate::phase4c::SmbArchiveWaypointPolicy::Region {
                world: 0,
                level: 0,
                low: 0,
                high: 0,
                band_low: 0,
                band_high: 15,
            },
            suffix: super::SmbCampaignSuffixPolicy::OneOrTwoRegionLong48,
            chord: derived_policy(),
            replacement_policy: crate::phase4c::SmbArchiveReplacementPolicy::FewestActions,
            resume_policy: SmbCampaignResumePolicy::FrontierShortest,
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
                serde_json::from_str::<super::SmbCampaignStreamRecord>(line)
                    .expect("parse campaign record")
            })
            .filter_map(|record| match record {
                super::SmbCampaignStreamRecord::Job(job) => job.chord_table_before,
                super::SmbCampaignStreamRecord::Skip(skip) => skip.chord_table_before,
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
    fn yield_budgeted_campaign_replays_from_recorded_costs() {
        let rom = synthetic_nrom();
        let config = SmbCampaignConfig {
            campaign_seed: 0x5eed_b0d6,
            workers: 2,
            execution_budget: 8,
            action_limit: 96,
            host: "unit-test".to_owned(),
            wall_budget: None,
            selector_policy: budgeted_selector(),
            retention_policy: SmbArchiveRetentionPolicy::Frozen,
            archive_entry_limit: 32_768,
            vocabulary: super::SmbCampaignVocabulary::FrozenNineMask,
            key_policy: crate::phase4c::SmbArchiveKeyPolicy::Frozen,
            waypoint_policy: crate::phase4c::SmbArchiveWaypointPolicy::Absent,
            suffix: super::SmbCampaignSuffixPolicy::OneOrTwo,
            chord: super::SmbCampaignChordPolicy::Uniform,
            replacement_policy: crate::phase4c::SmbArchiveReplacementPolicy::FewestActions,
            resume_policy: SmbCampaignResumePolicy::FrontierShortest,
        };
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("yield-budgeted campaign");
        assert!(
            std::str::from_utf8(&stream)
                .expect("stream text")
                .lines()
                .next()
                .expect("stream header")
                .contains("yield_budgeted_128:16,4,64,256")
        );
        let replayed =
            replay_smb_campaign(&rom, &stream, None).expect("replay yield-budgeted campaign");
        assert_eq!(live, replayed);
    }

    #[test]
    fn duplicate_check_requires_every_boundary() {
        let rom = synthetic_nrom();
        let config = SmbCampaignConfig {
            campaign_seed: 0x5eed_ca04,
            workers: 2,
            execution_budget: 16,
            action_limit: 96,
            host: "unit-test".to_owned(),
            wall_budget: None,
            selector_policy: SmbArchiveSelectorPolicy::ConcentratedRecency,
            retention_policy: SmbArchiveRetentionPolicy::ProbeAtAdmission,
            archive_entry_limit: 32_768,
            vocabulary: super::SmbCampaignVocabulary::FrozenNineMask,
            key_policy: crate::phase4c::SmbArchiveKeyPolicy::Frozen,
            waypoint_policy: crate::phase4c::SmbArchiveWaypointPolicy::Absent,
            suffix: super::SmbCampaignSuffixPolicy::OneOrTwo,
            chord: super::SmbCampaignChordPolicy::Uniform,
            replacement_policy: crate::phase4c::SmbArchiveReplacementPolicy::FewestActions,
            resume_policy: SmbCampaignResumePolicy::FrontierShortest,
        };
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
        let rom = synthetic_nrom();
        let seed_config = SmbCampaignConfig {
            campaign_seed: 0x5eed_ca05,
            workers: 2,
            execution_budget: 12,
            action_limit: 96,
            host: "unit-test".to_owned(),
            wall_budget: None,
            selector_policy: SmbArchiveSelectorPolicy::ConcentratedRecency,
            retention_policy: SmbArchiveRetentionPolicy::ProbeAtAdmission,
            archive_entry_limit: 32_768,
            vocabulary: super::SmbCampaignVocabulary::FrozenNineMask,
            key_policy: crate::phase4c::SmbArchiveKeyPolicy::Frozen,
            waypoint_policy: crate::phase4c::SmbArchiveWaypointPolicy::Absent,
            suffix: super::SmbCampaignSuffixPolicy::OneOrTwo,
            chord: super::SmbCampaignChordPolicy::Uniform,
            replacement_policy: crate::phase4c::SmbArchiveReplacementPolicy::FewestActions,
            resume_policy: SmbCampaignResumePolicy::FrontierShortest,
        };
        let mut seed_stream = Vec::new();
        let seed_campaign = run_smb_campaign(
            &rom,
            &seed_config,
            &SmbCampaignOrigin::Genesis,
            &mut seed_stream,
        )
        .expect("seed campaign");
        let source = seed_campaign.archive.clone();
        let source_sha = "0000000000000000000000000000000000000000000000000000000000000000";
        let config = SmbCampaignConfig {
            campaign_seed: 0x5eed_ca06,
            workers: 3,
            execution_budget: 16,
            action_limit: 96,
            host: "unit-test".to_owned(),
            wall_budget: None,
            selector_policy: SmbArchiveSelectorPolicy::ConcentratedRecency,
            retention_policy: SmbArchiveRetentionPolicy::ProbeAtAdmission,
            archive_entry_limit: 32_768,
            vocabulary: super::SmbCampaignVocabulary::FrozenNineMask,
            key_policy: crate::phase4c::SmbArchiveKeyPolicy::Frozen,
            waypoint_policy: crate::phase4c::SmbArchiveWaypointPolicy::Absent,
            suffix: super::SmbCampaignSuffixPolicy::OneOrTwo,
            chord: super::SmbCampaignChordPolicy::Uniform,
            replacement_policy: crate::phase4c::SmbArchiveReplacementPolicy::FewestActions,
            resume_policy: SmbCampaignResumePolicy::FrontierShortest,
        };
        let mut stream = Vec::new();
        let live = run_smb_campaign(
            &rom,
            &config,
            &SmbCampaignOrigin::Archive {
                path: "seed-archive.json".to_owned(),
                file_sha256: source_sha.to_owned(),
                report: Box::new(source.clone()),
            },
            &mut stream,
        )
        .expect("archive-origin campaign");
        let replayed = replay_smb_campaign(&rom, &stream, Some(&source))
            .expect("replay archive-origin campaign");
        assert_eq!(live, replayed);
        assert_eq!(live.origin.kind, "archive");
    }

    #[test]
    fn concentrated_campaign_replays_byte_identically_with_annotations() {
        let rom = synthetic_nrom();
        let config = SmbCampaignConfig {
            campaign_seed: 0x5eed_ca09,
            workers: 4,
            execution_budget: 32,
            action_limit: 96,
            host: "unit-test".to_owned(),
            wall_budget: None,
            selector_policy: SmbArchiveSelectorPolicy::ConcentratedRecency,
            retention_policy: SmbArchiveRetentionPolicy::ProbeAtAdmission,
            archive_entry_limit: 32_768,
            vocabulary: super::SmbCampaignVocabulary::FrozenNineMask,
            key_policy: crate::phase4c::SmbArchiveKeyPolicy::Frozen,
            waypoint_policy: crate::phase4c::SmbArchiveWaypointPolicy::Absent,
            suffix: super::SmbCampaignSuffixPolicy::OneOrTwo,
            chord: super::SmbCampaignChordPolicy::Uniform,
            replacement_policy: crate::phase4c::SmbArchiveReplacementPolicy::FewestActions,
            resume_policy: SmbCampaignResumePolicy::FrontierShortest,
        };
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("concentrated live campaign");
        assert_eq!(live.executions_completed, 32);
        let text = String::from_utf8(stream.clone()).expect("stream is utf-8");
        assert!(
            text.lines()
                .next()
                .expect("header")
                .contains("concentrated_recency_128")
        );
        for line in text.lines().skip(1) {
            assert!(
                line.contains("\"selector\""),
                "every concentrated job and skip record must carry a selector annotation"
            );
            if line.contains("\"tie_class\"") {
                assert!(
                    line.contains("\"concentration\""),
                    "every concentrated tie-class draw must carry its window record"
                );
            } else {
                assert!(
                    !line.contains("\"concentration\""),
                    "uniform draws must not carry a window record"
                );
            }
        }
        let replayed =
            replay_smb_campaign(&rom, &stream, None).expect("replay concentrated campaign");
        assert_eq!(live, replayed);
        let live_bytes = serde_json::to_vec_pretty(&live).expect("serialize live report");
        let replay_bytes = serde_json::to_vec_pretty(&replayed).expect("serialize replayed report");
        assert_eq!(live_bytes, replay_bytes);
        let accounting = live.archive.selector;
        assert_eq!(
            accounting.policy,
            SmbArchiveSelectorPolicy::ConcentratedRecency
        );
        assert_eq!(
            accounting
                .uniform_selections
                .checked_add(accounting.tie_class_selections),
            live.executions_completed
                .checked_add(live.duplicates_skipped)
        );
        let concentration = accounting.concentration.expect("concentration accounting");
        assert_eq!(concentration.window_cap, 128);
        assert_eq!(concentration.window_draws, accounting.tie_class_selections);
        assert!(concentration.distinct_window_parents > 0);
        assert_eq!(
            concentration.draws_per_parent_milli,
            concentration.window_draws * 1000 / concentration.distinct_window_parents
        );
    }

    #[test]
    fn the_clock_aware_resume_prefers_a_faster_shallower_lineage() {
        use crate::phase4c::{SmbArchiveEntryReport, SmbArchiveKey};
        let key = |progress: u16| SmbArchiveKey {
            world: 7,
            level: 0,
            progress,
            player_y_bucket: 0,
            player_engine_state: 0,
            state_fingerprint: 0,
            room_x_bucket: 0,
        };
        // Two lineages through one pair. The deep one walks further on long
        // holds; the shallow one stops twenty buckets back having spent far
        // fewer frames. This is C102's situation exactly.
        let mut entries = Vec::new();
        let mut push = |id: u64, parent: Option<u64>, progress: u16, actions: Vec<ButtonChord>| {
            entries.push(SmbArchiveEntryReport {
                id,
                parent_id: parent,
                created_execution: 0,
                input: SmbInput { actions },
                key: key(progress),
                milestones: SmbMilestones::default(),
                selector: None,
            });
        };
        let slow: Vec<ButtonChord> = (0..8).map(|_| ButtonChord::new(0x01, 120)).collect();
        let fast: Vec<ButtonChord> = (0..8).map(|_| ButtonChord::new(0x02, 20)).collect();
        push(0, None, 0, Vec::new());
        for step in 1..=8_usize {
            push(
                u64::try_from(step).expect("id"),
                Some(u64::try_from(step - 1).expect("parent")),
                u16::try_from(step * 40).expect("progress"),
                slow[..step].to_vec(),
            );
        }
        for step in 1..=7_usize {
            let id = u64::try_from(8 + step).expect("id");
            push(
                id,
                Some(if step == 1 { 0 } else { id - 1 }),
                u16::try_from(step * 43).expect("progress"),
                fast[..step].to_vec(),
            );
        }
        let source = SmbArchiveReport {
            seed: 0,
            executions: 0,
            milestones: SmbMilestones::default(),
            progress_watermark: crate::phase4b::SmbProgressWatermark::default(),
            first_reached: crate::phase4b::SmbMilestoneTimes::default(),
            first_inputs: crate::phase4b::SmbMilestoneInputs::default(),
            champion_input: SmbInput::default(),
            entries,
            progress_curve: Vec::new(),
            retained: 0,
            rejected: 0,
            deaths: 0,
            ranking: crate::phase4c::SmbRankingAccounting::default(),
            generated_mutator: crate::phase4c::SmbGeneratedMutatorAccounting::default(),
            ladder: crate::phase4c::SmbLadder::default(),
            selector: crate::phase4c::SmbSelectorAccounting::default(),
        };
        // The frozen rule takes the deepest entry, bucket 320, 960 frames.
        let frozen =
            select_frontier_resume_input(&source, SmbCampaignResumePolicy::FrontierShortest)
                .expect("frozen resume");
        assert_eq!(frozen.actions.len(), 8);
        assert_eq!(
            frozen
                .actions
                .iter()
                .map(|action| u64::from(action.bounded_hold_frames()))
                .sum::<u64>(),
            960
        );
        // The clock-aware rule reaches back within its registered thirty-two
        // buckets to bucket 301 and takes the 140-frame route instead.
        let fastest =
            select_frontier_resume_input(&source, SmbCampaignResumePolicy::FastestInLevelWithin32)
                .expect("clock-aware resume");
        assert_eq!(
            fastest
                .actions
                .iter()
                .map(|action| u64::from(action.bounded_hold_frames()))
                .sum::<u64>(),
            140
        );
        assert_eq!(
            resume_from_identifier("fastest_in_level_32").expect("parse"),
            SmbCampaignResumePolicy::FastestInLevelWithin32
        );
        assert_eq!(
            resume_identifier(SmbCampaignResumePolicy::FrontierShortest),
            "frontier_shortest"
        );
        assert!(resume_from_identifier("fastest_in_level").is_err());
    }

    #[test]
    fn replacement_identifier_round_trips_and_defaults_stay_off_the_stream() {
        assert_eq!(
            replacement_identifier(SmbArchiveReplacementPolicy::FewestActions),
            "fewest_actions"
        );
        assert_eq!(
            replacement_identifier(SmbArchiveReplacementPolicy::FewestFramesInLevel),
            "fewest_frames_in_level"
        );
        assert_eq!(
            replacement_from_identifier("fewest_frames_in_level").expect("parse frames rule"),
            SmbArchiveReplacementPolicy::FewestFramesInLevel
        );
        assert!(replacement_from_identifier("fewest_frames").is_err());
        // A run on the frozen rule writes no field, so every stream recorded
        // before the rule existed stays byte-identical and replays as itself.
        let rom = synthetic_nrom();
        let config = SmbCampaignConfig {
            campaign_seed: 0x5eed_ca0c,
            workers: 1,
            execution_budget: 4,
            action_limit: 96,
            host: "unit-test".to_owned(),
            wall_budget: None,
            selector_policy: SmbArchiveSelectorPolicy::ConcentratedRecency,
            retention_policy: SmbArchiveRetentionPolicy::ProbeAtAdmission,
            archive_entry_limit: 32_768,
            vocabulary: super::SmbCampaignVocabulary::FrozenNineMask,
            key_policy: crate::phase4c::SmbArchiveKeyPolicy::Frozen,
            waypoint_policy: crate::phase4c::SmbArchiveWaypointPolicy::Absent,
            suffix: super::SmbCampaignSuffixPolicy::OneOrTwo,
            chord: super::SmbCampaignChordPolicy::Uniform,
            replacement_policy: SmbArchiveReplacementPolicy::FewestActions,
            resume_policy: SmbCampaignResumePolicy::FrontierShortest,
        };
        let mut stream = Vec::new();
        run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("frozen-rule campaign");
        let header_line = std::str::from_utf8(&stream)
            .expect("stream is utf-8")
            .lines()
            .next()
            .expect("stream header")
            .to_owned();
        assert!(
            !header_line.contains("replacement_policy"),
            "the frozen rule writes no header field"
        );
        let header: SmbCampaignStreamHeader =
            serde_json::from_str(&header_line).expect("parse header");
        assert_eq!(
            replacement_from_identifier(&header.replacement_policy).expect("legacy default"),
            SmbArchiveReplacementPolicy::FewestActions
        );
        // The registered rule does record itself, so replay reads it back.
        let mut registered_stream = Vec::new();
        run_smb_campaign(
            &rom,
            &SmbCampaignConfig {
                replacement_policy: SmbArchiveReplacementPolicy::FewestFramesInLevel,
                ..config
            },
            &SmbCampaignOrigin::Genesis,
            &mut registered_stream,
        )
        .expect("registered-rule campaign");
        assert!(
            std::str::from_utf8(&registered_stream)
                .expect("stream is utf-8")
                .lines()
                .next()
                .expect("stream header")
                .contains("fewest_frames_in_level")
        );
        let replayed = replay_smb_campaign(&rom, &registered_stream, None)
            .expect("registered-rule campaign replays");
        assert_eq!(replayed.replacement_policy, "fewest_frames_in_level");
    }

    #[test]
    fn waypoint_identifier_round_trips() {
        let region = SmbArchiveWaypointPolicy::Region {
            world: 4,
            level: 2,
            low: 96,
            high: 128,
            band_low: 3,
            band_high: 9,
        };
        assert_eq!(waypoint_identifier(region), "waypoint_4:4,2,96,128,3,9");
        assert_eq!(
            waypoint_from_identifier("waypoint_4:4,2,96,128,3,9").expect("parse region"),
            region
        );
        assert_eq!(
            waypoint_from_identifier("absent").expect("parse absent"),
            SmbArchiveWaypointPolicy::Absent
        );
        assert_eq!(
            waypoint_identifier(SmbArchiveWaypointPolicy::Absent),
            "absent"
        );
        assert!(waypoint_from_identifier("waypoint_4:4,2,96,128,3").is_err());
        assert!(waypoint_from_identifier("waypoint_4:4,2,96,128,3,9,1").is_err());
        assert!(waypoint_from_identifier("waypoint_4:4,2,128,96,3,9").is_err());
        assert!(waypoint_from_identifier("waypoint_4:4,2,96,128,9,3").is_err());
        assert!(waypoint_from_identifier("pinned_window_128:1,0,0,1").is_err());
    }

    /// The key every state of the synthetic NROM target decodes to.
    fn synthetic_genesis_key() -> SmbArchiveKey {
        let rom = synthetic_nrom();
        let mut target = SmbTarget::from_smb_rom_bytes_headless(&rom).expect("load genesis target");
        target.reset();
        crate::phase4c::archive_key(target.wram(), crate::phase4c::SmbArchiveKeyPolicy::Frozen)
    }

    #[test]
    fn stacked_waypoint_campaign_replays_byte_identically() {
        let rom = synthetic_nrom();
        let genesis = synthetic_genesis_key();
        // All three registered policies stacked: the pin, the snapback
        // refusal, and a waypoint covering the genesis pair.
        let config = SmbCampaignConfig {
            campaign_seed: 0x5eed_ca0a,
            workers: 4,
            execution_budget: 32,
            action_limit: 96,
            host: "unit-test".to_owned(),
            wall_budget: None,
            selector_policy: SmbArchiveSelectorPolicy::PinnedWindow {
                world: genesis.world,
                level: genesis.level,
                low: 0,
                high: u16::MAX,
            },
            retention_policy: SmbArchiveRetentionPolicy::ProbeAtAdmission45Snapback16,
            archive_entry_limit: 32_768,
            vocabulary: super::SmbCampaignVocabulary::FrozenNineMask,
            key_policy: crate::phase4c::SmbArchiveKeyPolicy::Frozen,
            waypoint_policy: SmbArchiveWaypointPolicy::Region {
                world: genesis.world,
                level: genesis.level,
                low: 0,
                high: u16::MAX,
                band_low: 0,
                band_high: u8::MAX,
            },
            suffix: super::SmbCampaignSuffixPolicy::OneOrTwo,
            chord: super::SmbCampaignChordPolicy::Uniform,
            replacement_policy: crate::phase4c::SmbArchiveReplacementPolicy::FewestActions,
            resume_policy: SmbCampaignResumePolicy::FrontierShortest,
        };
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("stacked live campaign");
        assert_eq!(live.executions_completed, 32);
        let text = String::from_utf8(stream.clone()).expect("stream is utf-8");
        let header = text.lines().next().expect("header");
        assert!(header.contains("waypoint_4:"));
        assert!(header.contains("pinned_window_128:"));
        assert!(header.contains("probe_at_admission_45_snapback_16"));
        assert!(
            text.contains("\"waypoint\":true"),
            "the waypoint preference must record its draws"
        );
        assert!(live.archive.selector.waypoint_selections > 0);
        assert!(
            live.waypoint_retained > 0,
            "the auxiliary cell capacity must retain past the base bound"
        );
        let replayed = replay_smb_campaign(&rom, &stream, None).expect("replay stacked campaign");
        assert_eq!(live, replayed);
        let live_bytes = serde_json::to_vec_pretty(&live).expect("serialize live report");
        let replay_bytes = serde_json::to_vec_pretty(&replayed).expect("serialize replayed report");
        assert_eq!(live_bytes, replay_bytes);
    }

    #[test]
    fn waypoint_absent_and_unentered_regions_are_inert() {
        let rom = synthetic_nrom();
        let genesis = synthetic_genesis_key();
        // One worker keeps the live schedule serial, so the two live runs
        // are comparable; multi-worker schedules may differ live by design.
        let config = |waypoint_policy| SmbCampaignConfig {
            campaign_seed: 0x5eed_ca0b,
            workers: 1,
            execution_budget: 16,
            action_limit: 96,
            host: "unit-test".to_owned(),
            wall_budget: None,
            selector_policy: SmbArchiveSelectorPolicy::ConcentratedRecency,
            retention_policy: SmbArchiveRetentionPolicy::ProbeAtAdmission,
            archive_entry_limit: 32_768,
            vocabulary: super::SmbCampaignVocabulary::FrozenNineMask,
            key_policy: crate::phase4c::SmbArchiveKeyPolicy::Frozen,
            waypoint_policy,
            suffix: super::SmbCampaignSuffixPolicy::OneOrTwo,
            chord: super::SmbCampaignChordPolicy::Uniform,
            replacement_policy: crate::phase4c::SmbArchiveReplacementPolicy::FewestActions,
            resume_policy: SmbCampaignResumePolicy::FrontierShortest,
        };
        let mut absent_stream = Vec::new();
        let absent = run_smb_campaign(
            &rom,
            &config(SmbArchiveWaypointPolicy::Absent),
            &SmbCampaignOrigin::Genesis,
            &mut absent_stream,
        )
        .expect("absent-waypoint campaign");
        // A registered region no state ever enters changes nothing but the
        // header record.
        let unentered = SmbArchiveWaypointPolicy::Region {
            world: genesis.world.wrapping_add(1),
            level: genesis.level,
            low: 0,
            high: u16::MAX,
            band_low: 0,
            band_high: u8::MAX,
        };
        let mut region_stream = Vec::new();
        let region = run_smb_campaign(
            &rom,
            &config(unentered),
            &SmbCampaignOrigin::Genesis,
            &mut region_stream,
        )
        .expect("unentered-region campaign");
        let absent_text = String::from_utf8(absent_stream.clone()).expect("absent stream utf-8");
        let region_text = String::from_utf8(region_stream.clone()).expect("region stream utf-8");
        assert!(
            !absent_text
                .lines()
                .next()
                .expect("absent header")
                .contains("waypoint"),
            "an absent policy must keep the legacy header byte shape"
        );
        assert!(
            region_text
                .lines()
                .next()
                .expect("region header")
                .contains("waypoint_4:")
        );
        let absent_records: Vec<&str> = absent_text.lines().skip(1).collect();
        let region_records: Vec<&str> = region_text.lines().skip(1).collect();
        assert_eq!(
            absent_records, region_records,
            "an unentered region must record the identical stream after the header"
        );
        assert_eq!(absent.archive, region.archive);
        assert_eq!(absent.waypoint_retained, 0);
        assert_eq!(region.waypoint_retained, 0);
        assert_eq!(region.waypoint_snap_exempt, 0);
        assert_eq!(region.archive.selector.waypoint_selections, 0);
        // Both streams, one with the field and one without, replay exactly.
        let absent_replay =
            replay_smb_campaign(&rom, &absent_stream, None).expect("replay absent stream");
        assert_eq!(absent, absent_replay);
        let region_replay =
            replay_smb_campaign(&rom, &region_stream, None).expect("replay region stream");
        assert_eq!(region, region_replay);
    }

    #[test]
    fn waypoint_exempts_snapback_only_inside_the_region() {
        let rom = synthetic_nrom();
        let mut target = SmbTarget::from_smb_rom_bytes_headless(&rom).expect("load target");
        target.reset();
        let snapshot = target.snapshot().expect("snapshot genesis");
        let parent_key = SmbArchiveKey {
            world: 1,
            level: 0,
            progress: 40,
            player_y_bucket: 5,
            player_engine_state: 0,
            state_fingerprint: 0,
            room_x_bucket: 0,
        };
        let candidate_key = SmbArchiveKey {
            progress: 10,
            ..parent_key
        };
        let admit = |waypoint_policy| -> (Vec<SmbCampaignAdmissionDecision>, u64, u64) {
            let mut core = CoordinatorCore::new(
                96,
                SmbArchiveSelectorPolicy::ConcentratedRecency,
                SmbArchiveRetentionPolicy::ProbeAtAdmission45Snapback16,
                32_768,
                crate::phase4c::SmbArchiveKeyPolicy::Frozen,
                waypoint_policy,
                crate::phase4c::SmbArchiveReplacementPolicy::FewestActions,
            );
            core.archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        input: SmbInput::default(),
                        key: parent_key,
                        milestones: SmbMilestones::default(),
                    },
                    snapshot.clone(),
                    &[],
                )
                .expect("insert snapback parent")
                .expect("retain snapback parent");
            let result = SmbCampaignJobResult {
                actions: vec![SmbCampaignActionResult {
                    action: ButtonChord::new(0x01, 8),
                    observations: Vec::new(),
                    milestones: SmbMilestones::default(),
                    dead: false,
                    failed: false,
                    candidate: Some(SmbCampaignCandidate {
                        key: candidate_key,
                        viable: true,
                        snapshot: snapshot.clone(),
                    }),
                }],
            };
            let (_, decisions) = core.admit_job(0, &result).expect("admit snapback job");
            (decisions, core.snap_refused, core.waypoint_snap_exempt)
        };
        // Without a waypoint the snapback rule refuses the backward candidate.
        let (decisions, snap_refused, exempted) = admit(SmbArchiveWaypointPolicy::Absent);
        assert_eq!(decisions, vec![SmbCampaignAdmissionDecision::SnapRefused]);
        assert_eq!((snap_refused, exempted), (1, 0));
        // A region containing the candidate waives the refusal and counts it.
        let (decisions, snap_refused, exempted) = admit(SmbArchiveWaypointPolicy::Region {
            world: 1,
            level: 0,
            low: 0,
            high: 16,
            band_low: 0,
            band_high: 15,
        });
        assert_eq!(
            decisions,
            vec![SmbCampaignAdmissionDecision::Retained { id: 1 }]
        );
        assert_eq!((snap_refused, exempted), (0, 1));
        // A region elsewhere in the same pair leaves the refusal unchanged.
        let (decisions, snap_refused, exempted) = admit(SmbArchiveWaypointPolicy::Region {
            world: 1,
            level: 0,
            low: 30,
            high: 50,
            band_low: 0,
            band_high: 15,
        });
        assert_eq!(decisions, vec![SmbCampaignAdmissionDecision::SnapRefused]);
        assert_eq!((snap_refused, exempted), (1, 0));
    }
}
