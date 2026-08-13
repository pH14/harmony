// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deterministic snapshot-backed quality-diversity search for SMB completion.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    error::Error,
    num::NonZeroUsize,
};

use libafl::executors::ExitKind;
use libafl_bolts::rands::{Rand, StdRand};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    phase4b::{
        ButtonChord, FRAME_HEIGHT, FRAME_WIDTH, MAX_HOLD_FRAMES, MAX_SMB_ACTIONS,
        PLAYER_KILLED_STATE, SmbDeathBytes, SmbInput, SmbMacro, SmbMechanicalState,
        SmbMilestoneInputs, SmbMilestoneTimes, SmbMilestones, SmbObservations,
        SmbProgressWatermark, SmbSnapshot, SmbTarget, smb_camera_pixels, smb_death_bytes,
        smb_mechanical_state_from_wram, smb_milestones_from_wram,
    },
    target::Target,
};

const MAX_ARCHIVE_ENTRIES: usize = 32_768;
const MAX_ENTRIES_PER_KEY: usize = 2;
const FRONTIER_WINDOW: usize = 128;
const RANKING_REBUILD_INTERVAL: u64 = 512;
const RANKING_STALE_EXECUTIONS: u64 = 1_024;
const GENERATED_MUTATOR_RETIRE_AFTER: u64 = 128;
const FRONTIER_PROGRESS_BAND: u16 = 8;
const STATE_FINGERPRINT_MASK: u8 = 0x3f;
const FROZEN_BUTTON_MASKS: [u8; 9] = [0x00, 0x01, 0x02, 0x40, 0x80, 0x81, 0x82, 0x83, 0x10];

/// Largest bounded action horizon accepted by the completion-only archive.
pub const MAX_SMB_COMPLETION_ACTIONS: usize = 512;

/// Pure generated score used only to choose a replacement inside a full archive cell.
pub trait SmbRanking {
    /// Return one comparable deterministic score from one state's recorded observations.
    fn score(&self, observations: &[SmbObservations]) -> i64;
}

/// Mechanical accounting for one installed generated ranking.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbRankingAccounting {
    /// Whether this campaign installed a ranking.
    pub installed: bool,
    /// Whether the ranking remained active at campaign end.
    pub active: bool,
    /// Full-cell replacements selected while the ranking was active.
    pub replacements: u64,
    /// New archive cells reached by descendants of ranking-selected replacements.
    pub descendant_novelty: u64,
    /// Execution-count rebuild at which an ineffective ranking was retired.
    pub retired_at_execution: Option<u64>,
}

/// Mechanical accounting for one installed generated archive mutator.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbGeneratedMutatorAccounting {
    /// Whether this campaign installed a generated mutator.
    pub installed: bool,
    /// Whether the mutator remained active at campaign end.
    pub active: bool,
    /// Times the host selected and invoked the generated mutator.
    pub attempts: u64,
    /// Changed, bounded candidates emitted by the generated mutator.
    pub offspring: u64,
    /// Emitted candidates that retained at least one archive state.
    pub retained_offspring: u64,
    /// Consecutive emitted candidates that retained no archive state.
    pub consecutive_nonretained: u64,
    /// Execution at which mechanical retirement occurred.
    pub retired_at_execution: Option<u64>,
}

/// Duration distribution used by completion suffix mutation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbArchiveDurationPolicy {
    /// Frozen H1 distribution: three quarters short, one quarter full-range.
    Legacy,
    /// Generic two-stratum distribution covering short control and long time horizons.
    Stratified,
}

/// Number of adjacent chords sampled for one archive expansion.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbArchiveSuffixPolicy {
    /// Frozen H1 behavior: one chord with probability 3/4, otherwise two.
    OneOrTwo,
    /// Bounded temporal burst: lengths one through four with geometric tail probabilities.
    BurstUpToFour,
}

/// Frozen search parameters used by a generated-ranking archive campaign.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbRankingSearchConfig {
    /// Maximum clean-reset action count.
    pub max_actions: usize,
    /// Seeded hold-duration distribution.
    pub duration_policy: SmbArchiveDurationPolicy,
    /// Seeded adjacent-chord distribution.
    pub suffix_policy: SmbArchiveSuffixPolicy,
}

/// One bounded quality-diversity cell for an action-boundary snapshot.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SmbArchiveKey {
    /// Mechanical world number.
    pub world: u8,
    /// Mechanical level number.
    pub level: u8,
    /// Current 16-pixel progress bucket.
    pub progress: u16,
    /// Coarse player vertical-position bucket.
    pub player_y_bucket: u8,
    /// Mechanical player engine state.
    pub player_engine_state: u8,
    /// Six-bit deterministic fingerprint of otherwise-hidden work RAM state.
    pub state_fingerprint: u8,
}

/// Serializable lineage and retention record for one archived testcase.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbArchiveEntryReport {
    /// Stable insertion-order archive identifier.
    pub id: u64,
    /// Archive parent selected for the suffix execution.
    pub parent_id: Option<u64>,
    /// Target execution that created the entry; zero denotes bootstrap.
    pub created_execution: u64,
    /// Complete clean-reset input represented by this snapshot.
    pub input: SmbInput,
    /// Route-agnostic quality-diversity key.
    pub key: SmbArchiveKey,
    /// Strongest milestones observed along this input.
    pub milestones: SmbMilestones,
}

/// Deterministic progress sample from one archive campaign.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbArchiveProgressPoint {
    /// Completed target executions.
    pub executions: u64,
    /// Strongest milestone state observed so far.
    pub milestones: SmbMilestones,
    /// Number of active retained archive entries.
    pub active_entries: usize,
    /// Number of occupied quality-diversity cells.
    pub occupied_cells: usize,
    /// Number of terminal death transitions seen so far.
    pub deaths: u64,
}

/// Complete deterministic report for one snapshot-backed suffix-search campaign.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbArchiveReport {
    /// Caller-provided seeded RNG value.
    pub seed: u64,
    /// Number of suffix target executions.
    pub executions: u64,
    /// Strongest milestone values reached.
    pub milestones: SmbMilestones,
    /// Furthest per-frame mechanical position, including action interiors.
    #[serde(default)]
    pub progress_watermark: SmbProgressWatermark,
    /// First execution reaching each frozen milestone rung.
    pub first_reached: SmbMilestoneTimes,
    /// First clean-reset input reaching each rung.
    pub first_inputs: SmbMilestoneInputs,
    /// Current best clean-reset input.
    pub champion_input: SmbInput,
    /// Insertion and replacement records for retained testcases.
    pub entries: Vec<SmbArchiveEntryReport>,
    /// Fixed-interval deterministic progress curve.
    pub progress_curve: Vec<SmbArchiveProgressPoint>,
    /// Candidate snapshots admitted to the active archive.
    pub retained: u64,
    /// Candidate snapshots rejected by bounded quality-diversity retention.
    pub rejected: u64,
    /// Terminal death transitions observed.
    pub deaths: u64,
    /// Execution-count accounting for the optional generated ranking.
    #[serde(default)]
    pub ranking: SmbRankingAccounting,
    /// Execution-count accounting for the optional generated archive mutator.
    #[serde(default)]
    pub generated_mutator: SmbGeneratedMutatorAccounting,
}

/// Mechanical outcome of one fixed frontier-viability continuation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbViabilityClass {
    /// The continuation reached player engine kill state `$0b`.
    KillState,
    /// The continuation ended in vertical bucket 15 without registering kill.
    BelowPlayable,
    /// The continuation ended outside the two doomed classes.
    Controllable,
}

/// Viability result for one active archive representative.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbViabilityEntry {
    /// Stable source archive identifier.
    pub id: u64,
    /// Whether this entry belongs to the maximal progress-39 frontier.
    pub frontier: bool,
    /// Recorded archive key at the audited endpoint.
    pub key: SmbArchiveKey,
    /// No-input continuation followed by the nine frozen controller masks.
    pub continuations: Vec<SmbViabilityClass>,
    /// True only when no continuation remains controllable.
    pub doomed: bool,
}

/// Count summary for one audited archive slice.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbViabilityCounts {
    /// Audited active representatives.
    pub total: u64,
    /// Representatives with no controllable continuation.
    pub doomed: u64,
}

/// Deterministic D27 frontier-viability report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbFrontierViabilityReport {
    /// Frames applied by every fixed continuation.
    pub continuation_frames: u8,
    /// No-input plus frozen mask order used by every entry.
    pub continuation_masks: Vec<Option<u8>>,
    /// Maximal progress-39 counts.
    pub frontier: SmbViabilityCounts,
    /// Inclusive progress-32-through-39 approach-band counts.
    pub approach_band: SmbViabilityCounts,
    /// Stable per-entry evidence in input lexical order.
    pub entries: Vec<SmbViabilityEntry>,
}

#[derive(Clone, Debug)]
struct ArchiveEntry {
    report: SmbArchiveEntryReport,
    snapshot: SmbSnapshot,
    observations: Vec<SmbObservations>,
    ranking_lineage: bool,
}

struct ArchiveCandidate {
    input: SmbInput,
    key: SmbArchiveKey,
    milestones: SmbMilestones,
}

struct Archive<'a> {
    entries: Vec<ArchiveEntry>,
    active: Vec<bool>,
    cells: BTreeMap<SmbArchiveKey, Vec<usize>>,
    input_ids: BTreeMap<SmbInput, usize>,
    retained: u64,
    rejected: u64,
    ranking: Option<&'a dyn SmbRanking>,
    ranking_accounting: SmbRankingAccounting,
    first_ranking_replacement: Option<u64>,
    last_descendant_novelty: Option<u64>,
    experimental_search: bool,
}

impl<'a> Archive<'a> {
    fn new(ranking: Option<&'a dyn SmbRanking>, experimental_search: bool) -> Self {
        Self {
            entries: Vec::new(),
            active: Vec::new(),
            cells: BTreeMap::new(),
            input_ids: BTreeMap::new(),
            retained: 0,
            rejected: 0,
            ranking,
            ranking_accounting: SmbRankingAccounting {
                installed: ranking.is_some(),
                active: ranking.is_some(),
                ..SmbRankingAccounting::default()
            },
            first_ranking_replacement: None,
            last_descendant_novelty: None,
            experimental_search,
        }
    }

    fn insert(
        &mut self,
        parent_id: Option<usize>,
        execution: u64,
        candidate: ArchiveCandidate,
        snapshot: SmbSnapshot,
        observations: &[SmbObservations],
    ) -> Result<Option<usize>, Box<dyn Error>> {
        let ArchiveCandidate {
            input,
            key,
            milestones,
        } = candidate;
        if let Some(existing) = self.input_ids.get(&input) {
            return Ok(Some(*existing));
        }
        let cell = self.cells.entry(key).or_default();
        let new_cell = cell.is_empty();
        let mut ranking_replacement = false;
        let replace = if cell.len() < MAX_ENTRIES_PER_KEY {
            None
        } else if self.ranking_accounting.active {
            let ranking = self.ranking.ok_or("active SMB ranking is missing")?;
            let candidate_quality = (ranking.score(observations), Reverse(input.actions.len()));
            cell.iter()
                .copied()
                .min_by_key(|id| {
                    (
                        ranking.score(&self.entries[*id].observations),
                        Reverse(self.entries[*id].report.input.actions.len()),
                    )
                })
                .filter(|id| {
                    let existing_quality = (
                        ranking.score(&self.entries[*id].observations),
                        Reverse(self.entries[*id].report.input.actions.len()),
                    );
                    candidate_quality > existing_quality
                })
                .inspect(|_| ranking_replacement = true)
        } else {
            cell.iter()
                .copied()
                .max_by_key(|id| entry_cost(&self.entries[*id].report))
                .filter(|id| input.actions.len() < self.entries[*id].report.input.actions.len())
        };
        if cell.len() >= MAX_ENTRIES_PER_KEY && replace.is_none() {
            self.rejected = self.rejected.saturating_add(1);
            return Ok(None);
        }
        if self.entries.len() >= MAX_ARCHIVE_ENTRIES {
            self.rejected = self.rejected.saturating_add(1);
            return Ok(None);
        }
        if let Some(replaced) = replace {
            self.active[replaced] = false;
            cell.retain(|id| *id != replaced);
        }
        let parent_ranking_lineage = parent_id
            .and_then(|id| self.entries.get(id))
            .is_some_and(|entry| entry.ranking_lineage);
        let ranking_lineage = parent_ranking_lineage || ranking_replacement;
        if ranking_replacement {
            self.ranking_accounting.replacements =
                self.ranking_accounting.replacements.saturating_add(1);
            self.first_ranking_replacement.get_or_insert(execution);
        }
        if new_cell && parent_ranking_lineage && execution > 0 {
            self.ranking_accounting.descendant_novelty =
                self.ranking_accounting.descendant_novelty.saturating_add(1);
            self.last_descendant_novelty = Some(execution);
        }
        let id = self.entries.len();
        let report = SmbArchiveEntryReport {
            id: u64::try_from(id)?,
            parent_id: parent_id.map(u64::try_from).transpose()?,
            created_execution: execution,
            input: input.clone(),
            key,
            milestones,
        };
        self.entries.push(ArchiveEntry {
            report,
            snapshot,
            observations: observations.to_vec(),
            ranking_lineage,
        });
        self.active.push(true);
        cell.push(id);
        self.input_ids.insert(input, id);
        self.retained = self.retained.saturating_add(1);
        Ok(Some(id))
    }

    fn finish_execution(&mut self, execution: u64) {
        if !self.ranking_accounting.active || !execution.is_multiple_of(RANKING_REBUILD_INTERVAL) {
            return;
        }
        let Some(first_replacement) = self.first_ranking_replacement else {
            return;
        };
        let last_gain = self.last_descendant_novelty.unwrap_or(first_replacement);
        if execution.saturating_sub(last_gain) >= RANKING_STALE_EXECUTIONS {
            self.ranking_accounting.active = false;
            self.ranking_accounting.retired_at_execution = Some(execution);
        }
    }

    fn active_ids(&self, max_actions: usize) -> Vec<usize> {
        self.active
            .iter()
            .enumerate()
            .filter_map(|(id, active)| {
                (*active && self.entries[id].report.input.actions.len() < max_actions).then_some(id)
            })
            .collect()
    }

    fn choose_parent(
        &self,
        rand: &mut StdRand,
        max_actions: usize,
    ) -> Result<usize, Box<dyn Error>> {
        let active = self.active_ids(max_actions);
        if active.is_empty() {
            return Err("SMB archive has no expandable entry".into());
        }
        let use_frontier = rand.below(NonZeroUsize::new(4).ok_or("invalid frontier odds")?) != 0;
        if !use_frontier {
            return Ok(active[rand.below(NonZeroUsize::new(active.len()).ok_or("empty archive")?)]);
        }
        if !self.experimental_search {
            let mut ordered = active;
            ordered.sort_by_key(|id| {
                (
                    milestone_key(self.entries[*id].report.milestones),
                    self.entries[*id].report.key,
                    self.entries[*id].report.id,
                )
            });
            let start = ordered.len().saturating_sub(FRONTIER_WINDOW);
            let frontier = &ordered[start..];
            return Ok(
                frontier[rand.below(NonZeroUsize::new(frontier.len()).ok_or("empty frontier")?)]
            );
        }
        let best = active
            .iter()
            .map(|id| frontier_quality(&self.entries[*id].report))
            .max()
            .ok_or("empty frontier")?;
        let frontier = active
            .into_iter()
            .filter(|id| {
                let quality = frontier_quality(&self.entries[*id].report);
                quality.0 == best.0
                    && quality.1 == best.1
                    && quality.2 == best.2
                    && quality.3.saturating_add(FRONTIER_PROGRESS_BAND - 1) >= best.3
            })
            .collect::<Vec<_>>();
        Ok(frontier[rand.below(NonZeroUsize::new(frontier.len()).ok_or("empty frontier")?)])
    }
}

/// Run deterministic snapshot-backed short-horizon suffix search.
pub fn run_smb_archive_search(
    rom: &[u8],
    initial_inputs: &[SmbInput],
    seed: u64,
    execution_budget: u64,
) -> Result<SmbArchiveReport, Box<dyn Error>> {
    run_smb_archive_search_with_action_limit(
        rom,
        initial_inputs,
        seed,
        execution_budget,
        MAX_SMB_ACTIONS,
    )
}

/// Audit whether active frontier and approach-band representatives can recover.
pub fn audit_smb_frontier_viability(
    rom: &[u8],
    source: &SmbArchiveReport,
) -> Result<SmbFrontierViabilityReport, Box<dyn Error>> {
    let active = active_source_entries(source);
    let mut selected = active
        .into_iter()
        .filter(|entry| {
            entry.key.world == 0 && entry.key.level == 2 && (32..=39).contains(&entry.key.progress)
        })
        .collect::<Vec<_>>();
    selected.sort_by_key(|entry| (entry.input.clone(), entry.id));
    let continuation_masks = std::iter::once(None)
        .chain(FROZEN_BUTTON_MASKS.into_iter().map(Some))
        .collect::<Vec<_>>();
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    let genesis = target
        .snapshot()
        .ok_or("failed to snapshot audit genesis")?;
    let mut prior_input = SmbInput::default();
    let mut prior_snapshots = vec![genesis];
    let mut entries = Vec::with_capacity(selected.len());
    for entry in selected {
        let common = prior_input
            .actions
            .iter()
            .zip(&entry.input.actions)
            .take_while(|(left, right)| left == right)
            .count();
        target.restore(&prior_snapshots[common])?;
        prior_snapshots.truncate(common + 1);
        for action in &entry.input.actions[common..] {
            target.apply(action);
            let snapshot = target
                .snapshot()
                .ok_or("failed to snapshot audit replay prefix")?;
            prior_snapshots.push(snapshot);
            if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                break;
            }
        }
        let endpoint = target
            .snapshot()
            .ok_or("failed to snapshot audit endpoint")?;
        let mut continuations = Vec::with_capacity(continuation_masks.len());
        for mask in &continuation_masks {
            target.restore(&endpoint)?;
            target.apply(&ButtonChord::new(mask.unwrap_or(0), 120));
            let state = smb_mechanical_state_from_wram(target.wram());
            let reached_kill_state = state.player_engine_state == 0x0b
                || target
                    .last_action_observations()
                    .iter()
                    .any(|observation| observation.decoded.player_engine_state == 0x0b);
            continuations.push(if reached_kill_state {
                SmbViabilityClass::KillState
            } else if state.player_y_bucket == 15 {
                SmbViabilityClass::BelowPlayable
            } else {
                SmbViabilityClass::Controllable
            });
        }
        let doomed = continuations
            .iter()
            .all(|class| *class != SmbViabilityClass::Controllable);
        entries.push(SmbViabilityEntry {
            id: entry.id,
            frontier: entry.key.progress == 39,
            key: entry.key,
            continuations,
            doomed,
        });
        prior_input = entry.input.clone();
    }
    let counts = |frontier: bool| {
        let matching = entries.iter().filter(|entry| entry.frontier == frontier);
        SmbViabilityCounts {
            total: u64::try_from(matching.clone().count()).unwrap_or(u64::MAX),
            doomed: u64::try_from(matching.filter(|entry| entry.doomed).count())
                .unwrap_or(u64::MAX),
        }
    };
    let frontier = counts(true);
    let nonfrontier = counts(false);
    Ok(SmbFrontierViabilityReport {
        continuation_frames: 120,
        continuation_masks,
        frontier,
        approach_band: SmbViabilityCounts {
            total: frontier.total.saturating_add(nonfrontier.total),
            doomed: frontier.doomed.saturating_add(nonfrontier.doomed),
        },
        entries,
    })
}

/// One audited representative in the screen-relative player-column decode audit.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbPlayerColumnAuditedEntry {
    /// Stable source archive identifier.
    pub id: u64,
    /// Recorded progress bucket of the audited slice.
    pub progress: u16,
    /// Whether the entry belongs to the maximal frontier slice.
    pub frontier: bool,
    /// Camera position in pixels at the audited endpoint.
    pub endpoint_camera: u32,
    /// Recorded frame count per continuation, including the endpoint.
    pub recorded_frames: Vec<u16>,
}

/// Film-check evidence for one candidate work-RAM index.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbPlayerColumnFilmEvidence {
    /// Work-RAM index under test.
    pub index: u16,
    /// Smallest offset with at least the required agreeing comparisons.
    pub offset: i16,
    /// Comparisons agreeing with that offset inside the fixed tolerance.
    pub agreeing_comparisons: u64,
    /// Comparisons available for this index.
    pub comparisons: u64,
    /// Largest recorded camera difference among the agreeing comparisons.
    #[serde(default)]
    pub camera_spread: u32,
}

/// Deterministic screen-relative player-column decode report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbPlayerColumnReport {
    /// Frames requested by every continuation.
    pub continuation_frames: u8,
    /// Fixed continuation masks in execution order.
    pub continuation_masks: Vec<u8>,
    /// Audited representatives in selection order.
    pub audited: Vec<SmbPlayerColumnAuditedEntry>,
    /// Ordered active entries examined per slice before auditing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scanned_per_slice: Vec<u64>,
    /// Examined entries the controller steered, per slice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steerable_per_slice: Vec<u64>,
    /// Indices taking at least the required number of distinct values.
    pub distinct_value_survivors: u64,
    /// Indices additionally changing by at most the frame-step bound.
    pub smooth_survivors: u64,
    /// Indices additionally decreasing under the left continuation.
    pub left_direction_survivors: u64,
    /// Indices additionally not decreasing under the right continuation.
    pub right_direction_survivors: u64,
    /// Right continuations whose camera advance qualifies for the relative test.
    pub qualifying_right_continuations: u64,
    /// Indices surviving every mechanical filter, in ascending order.
    pub camera_relative_survivors: Vec<u16>,
    /// Surviving indices that additionally pass the film check.
    pub film_survivors: Vec<SmbPlayerColumnFilmEvidence>,
    /// Film survivors discarded as members of a four-byte-stride group.
    pub stride_rejected: Vec<u16>,
    /// Selected index, if the audit is conclusive.
    pub selected: Option<SmbPlayerColumnFilmEvidence>,
}

/// Control-authority counts for one progress bucket.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbControlCensusBucket {
    /// Corrected progress bucket.
    pub progress: u16,
    /// Active representatives in the bucket.
    pub active: u64,
    /// Representatives whose right continuation advanced the camera.
    pub admitted: u64,
}

/// Deterministic control-authority census over one archive level.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbControlCensusReport {
    /// Frames applied by the single right continuation.
    pub continuation_frames: u8,
    /// Camera pixels an admitted continuation must advance.
    pub camera_advance: u32,
    /// Per-bucket counts in ascending progress order.
    pub buckets: Vec<SmbControlCensusBucket>,
    /// Active representatives examined.
    pub active: u64,
    /// Representatives admitted anywhere.
    pub admitted: u64,
    /// Admitted entry identifiers in descending progress then `(input, id)` order.
    pub admitted_ids: Vec<u64>,
}

/// One rendered audit frame retained for direct visual inspection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SmbAuditFrame {
    /// Stable file name for the rendered frame.
    pub name: String,
    /// Raw RGBA pixels in rendering order.
    pub rgba: Vec<u8>,
}

const PLAYER_COLUMN_MASKS: [u8; 3] = [0x00, 0x01, 0x02];
const PLAYER_COLUMN_FRAMES: u8 = 120;
const PLAYER_COLUMN_SLICES: [u16; 2] = [39, 32];
const PLAYER_COLUMN_SLICE_SIZE: usize = 8;
const PLAYER_COLUMN_MIN_DISTINCT: usize = 8;
const PLAYER_COLUMN_MAX_STEP: i32 = 8;
const PLAYER_COLUMN_LEFT_DECREASE: i32 = 8;
const PLAYER_COLUMN_LEFT_SLACK: i32 = 4;
const PLAYER_COLUMN_LEFT_ENTRIES: usize = 12;
const PLAYER_COLUMN_LEFT_ENTRIES_BASE: usize = 16;
const PLAYER_COLUMN_RIGHT_SLACK: i32 = 16;
const PLAYER_COLUMN_CAMERA_ADVANCE: u32 = 32;
const PLAYER_COLUMN_FILM_GAP: i32 = 8;
const PLAYER_COLUMN_FILM_OFFSETS: i32 = 24;
const PLAYER_COLUMN_FILM_TOLERANCE: i32 = 6;
const PLAYER_COLUMN_FILM_MIN_AGREE: usize = 8;
const PLAYER_COLUMN_FILM_MIN_WIDTH: i32 = 4;
const PLAYER_COLUMN_FILM_MAX_WIDTH: i32 = 40;
const PLAYER_COLUMN_STRIDES: [u16; 3] = [4, 8, 12];
const PLAYER_COLUMN_SCAN_CAP: usize = 64;
const PLAYER_COLUMN_ADVANCING_SCAN_CAP: usize = 128;
const PLAYER_COLUMN_RENDERED_COMPARISONS: usize = 4;
const PLAYER_COLUMN_CAMERA_SPREAD: u32 = 16;
const PLAYER_COLUMN_BUCKET_CAP: usize = 2;

/// One ordered audit candidate: an active entry and its slice progress bucket.
type PlayerColumnCandidate<'a> = (&'a SmbArchiveEntryReport, u16);

/// Which active entries a screen-column audit records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmbPlayerColumnSelection {
    /// D29: the first eight ordered active entries of each slice.
    FirstOrdered,
    /// D30: the first eight ordered active entries the controller steers.
    FirstSteerable,
    /// D31: the first eight ordered active entries whose right continuation
    /// advances the recorded camera.
    FirstCameraAdvancing,
}

/// Which registered filter and truncation rules an audit applies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PlayerColumnRules {
    truncate_on_camera_decrease: bool,
    require_right_direction: bool,
    require_camera_relative: bool,
    require_camera_spread: bool,
}

impl PlayerColumnRules {
    /// D29 through D32: no camera-epoch truncation, with filters C3 and C4.
    const LEGACY: Self = Self {
        truncate_on_camera_decrease: false,
        require_right_direction: true,
        require_camera_relative: true,
        require_camera_spread: false,
    };

    /// D33: one camera epoch per continuation, C3 and C4 replaced by camera spread.
    const SPREAD: Self = Self {
        truncate_on_camera_decrease: true,
        require_right_direction: false,
        require_camera_relative: false,
        require_camera_spread: true,
    };
}

struct ContinuationRecording {
    wram: Vec<[u8; 2_048]>,
    columns: Vec<[u64; 256]>,
    camera: Vec<u32>,
}

struct EntryRecording {
    id: u64,
    progress: u16,
    frontier: bool,
    continuations: Vec<ContinuationRecording>,
}

struct FilmComparison {
    entry: usize,
    left: usize,
    right: usize,
    frame: usize,
    lowest: i32,
    highest: i32,
    camera: u32,
}

/// Identify the work-RAM byte holding the player's horizontal column on screen.
///
/// The audit runs no search and consults no model. It returns its deterministic
/// report together with the rendered frames that support the visual half.
///
/// # Errors
///
/// Returns an error when the source lacks the registered audit slices or when
/// emulation, snapshotting, or rendering fails.
pub fn audit_smb_player_screen_column(
    rom: &[u8],
    source: &SmbArchiveReport,
) -> Result<(SmbPlayerColumnReport, Vec<SmbAuditFrame>), Box<dyn Error>> {
    audit_smb_player_column_with_selection(rom, source, SmbPlayerColumnSelection::FirstOrdered)
}

/// Identify the horizontal-column byte using an explicit audited-entry selection.
///
/// `FirstOrdered` reproduces the frozen D29 audit byte for byte. `FirstSteerable`
/// records D30's control-authority test and its per-slice scan counts.
///
/// # Errors
///
/// Returns an error when the source lacks the registered audit slices or when
/// emulation, snapshotting, or rendering fails.
pub fn audit_smb_player_column_with_selection(
    rom: &[u8],
    source: &SmbArchiveReport,
    selection_mode: SmbPlayerColumnSelection,
) -> Result<(SmbPlayerColumnReport, Vec<SmbAuditFrame>), Box<dyn Error>> {
    let candidates = player_column_candidates(source, selection_mode)?;
    let mut target = SmbTarget::from_smb_rom_bytes(rom)?;
    let mut prefix = PlayerColumnPrefix::new(&mut target)?;
    let mut selected = Vec::new();
    let mut recordings = Vec::new();
    let mut scanned_per_slice = Vec::new();
    let mut steerable_per_slice = Vec::new();
    for slice in &candidates {
        let mut scanned = 0_u64;
        let mut steerable = 0_u64;
        let mut audited = 0_usize;
        for (entry, progress) in slice {
            if audited >= PLAYER_COLUMN_SLICE_SIZE {
                break;
            }
            scanned = scanned.saturating_add(1);
            let recording = record_player_column_entry(
                &mut target,
                &mut prefix,
                entry,
                *progress,
                PlayerColumnRules::LEGACY,
            )?;
            let keep = match selection_mode {
                SmbPlayerColumnSelection::FirstOrdered => true,
                SmbPlayerColumnSelection::FirstSteerable => {
                    let steered = player_column_is_steerable(&recording);
                    if steered {
                        steerable = steerable.saturating_add(1);
                    }
                    steered
                }
                SmbPlayerColumnSelection::FirstCameraAdvancing => {
                    let advanced = player_column_advances_camera(&recording);
                    if advanced {
                        steerable = steerable.saturating_add(1);
                    }
                    advanced
                }
            };
            if keep {
                selected.push((*entry, *progress));
                recordings.push(recording);
                audited = audited.saturating_add(1);
            }
        }
        if selection_mode != SmbPlayerColumnSelection::FirstOrdered {
            scanned_per_slice.push(scanned);
            steerable_per_slice.push(steerable);
        }
    }
    let (mut report, comparisons) = analyze_player_column(&recordings);
    report.scanned_per_slice = scanned_per_slice;
    report.steerable_per_slice = steerable_per_slice;
    let report = report;
    let requests = player_column_frame_requests(&recordings, &comparisons, &report);
    let frames = render_player_column_frames(&mut target, &selected, &requests)?;
    Ok((report, frames))
}

fn player_column_frame_requests(
    recordings: &[EntryRecording],
    comparisons: &[FilmComparison],
    report: &SmbPlayerColumnReport,
) -> BTreeSet<(usize, usize, usize)> {
    let mut requests = BTreeSet::new();
    for entry in first_audited_entry_per_slice(recordings) {
        let Some(recording) = recordings.get(entry) else {
            continue;
        };
        for (continuation, recorded) in recording.continuations.iter().enumerate() {
            let last = recorded.wram.len().saturating_sub(1);
            for frame in [0, last / 2, last] {
                requests.insert((entry, continuation, frame));
            }
        }
    }
    if let Some(selection) = report.selected {
        for comparison in comparisons
            .iter()
            .filter(|comparison| {
                film_offset(recordings, comparison, selection.index).is_some_and(
                    |(offset, width)| {
                        (offset - i32::from(selection.offset)).abs() <= PLAYER_COLUMN_FILM_TOLERANCE
                            && (PLAYER_COLUMN_FILM_MIN_WIDTH..=PLAYER_COLUMN_FILM_MAX_WIDTH)
                                .contains(&width)
                    },
                )
            })
            .take(PLAYER_COLUMN_RENDERED_COMPARISONS)
        {
            requests.insert((comparison.entry, comparison.left, comparison.frame));
            requests.insert((comparison.entry, comparison.right, comparison.frame));
        }
    }
    requests
}

/// Audit the horizontal-column byte over an explicit list of source entries.
///
/// D32 uses this with the highest-progress entries its control-authority census
/// admitted. Recording, filters, film check, selection, and rendering are the
/// audit's own.
///
/// # Errors
///
/// Returns an error when an identifier is absent from the source or when
/// emulation, snapshotting, or rendering fails.
pub fn audit_smb_player_column_from_ids(
    rom: &[u8],
    source: &SmbArchiveReport,
    ids: &[u64],
) -> Result<(SmbPlayerColumnReport, Vec<SmbAuditFrame>), Box<dyn Error>> {
    audit_player_column_from_ids(rom, source, ids, PlayerColumnRules::LEGACY)
}

/// Audit the horizontal-column byte under D33's camera-epoch and camera-spread rules.
///
/// # Errors
///
/// Returns an error when an identifier is absent from the source or when
/// emulation, snapshotting, or rendering fails.
pub fn audit_smb_player_column_spread(
    rom: &[u8],
    source: &SmbArchiveReport,
    ids: &[u64],
) -> Result<(SmbPlayerColumnReport, Vec<SmbAuditFrame>), Box<dyn Error>> {
    audit_player_column_from_ids(rom, source, ids, PlayerColumnRules::SPREAD)
}

fn audit_player_column_from_ids(
    rom: &[u8],
    source: &SmbArchiveReport,
    ids: &[u64],
    rules: PlayerColumnRules,
) -> Result<(SmbPlayerColumnReport, Vec<SmbAuditFrame>), Box<dyn Error>> {
    let active = active_source_entries(source);
    let mut selected = Vec::with_capacity(ids.len());
    for id in ids {
        let entry = active
            .iter()
            .find(|entry| entry.id == *id)
            .ok_or("audit identifier is not an active source entry")?;
        selected.push((*entry, entry.key.progress));
    }
    let mut target = SmbTarget::from_smb_rom_bytes(rom)?;
    let mut prefix = PlayerColumnPrefix::new(&mut target)?;
    let mut recordings = Vec::with_capacity(selected.len());
    for (entry, progress) in &selected {
        recordings.push(record_player_column_entry(
            &mut target,
            &mut prefix,
            entry,
            *progress,
            rules,
        )?);
    }
    let (report, comparisons) = analyze_player_column_with_rules(&recordings, rules);
    let requests = player_column_frame_requests(&recordings, &comparisons, &report);
    let frames = render_player_column_frames(&mut target, &selected, &requests)?;
    Ok((report, frames))
}

/// Per-frame rendered-difference and work-RAM record for one audited entry.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbFilmColumnTrace {
    /// Stable source archive identifier.
    pub id: u64,
    /// Recorded progress bucket.
    pub progress: u16,
    /// Recorded camera per frame of the no-input continuation.
    pub camera: Vec<u32>,
    /// Lowest differing rendered column per frame, or -1 when none differs.
    pub lowest: Vec<i32>,
    /// Highest differing rendered column per frame, or -1 when none differs.
    pub highest: Vec<i32>,
    /// Complete work RAM per frame of the left continuation.
    pub left_wram: Vec<Vec<u8>>,
    /// Complete work RAM per frame of the no-input continuation.
    pub still_wram: Vec<Vec<u8>>,
}

/// Record the rendered difference between the no-input and left continuations.
///
/// # Errors
///
/// Returns an error when an identifier is absent from the source or when
/// emulation or snapshotting fails.
pub fn diagnose_smb_film_columns(
    rom: &[u8],
    source: &SmbArchiveReport,
    ids: &[u64],
) -> Result<Vec<SmbFilmColumnTrace>, Box<dyn Error>> {
    let active = active_source_entries(source);
    let mut target = SmbTarget::from_smb_rom_bytes(rom)?;
    let mut prefix = PlayerColumnPrefix::new(&mut target)?;
    let mut traces = Vec::with_capacity(ids.len());
    for id in ids {
        let entry = active
            .iter()
            .find(|entry| entry.id == *id)
            .ok_or("audit identifier is not an active source entry")?;
        let recording = record_player_column_entry(
            &mut target,
            &mut prefix,
            entry,
            entry.key.progress,
            PlayerColumnRules::SPREAD,
        )?;
        let still = &recording.continuations[0];
        let left = &recording.continuations[2];
        let frames = still.wram.len().min(left.wram.len());
        let mut lowest = Vec::with_capacity(frames);
        let mut highest = Vec::with_capacity(frames);
        for frame in 0..frames {
            let differing = (0..256)
                .filter(|column| still.columns[frame][*column] != left.columns[frame][*column])
                .collect::<Vec<_>>();
            lowest.push(
                differing
                    .first()
                    .map_or(-1, |column| i32::try_from(*column).unwrap_or(i32::MAX)),
            );
            highest.push(
                differing
                    .last()
                    .map_or(-1, |column| i32::try_from(*column).unwrap_or(i32::MAX)),
            );
        }
        traces.push(SmbFilmColumnTrace {
            id: *id,
            progress: entry.key.progress,
            camera: still.camera[..frames].to_vec(),
            lowest,
            highest,
            left_wram: left.wram[..frames]
                .iter()
                .map(|wram| wram.to_vec())
                .collect(),
            still_wram: still.wram[..frames]
                .iter()
                .map(|wram| wram.to_vec())
                .collect(),
        });
    }
    Ok(traces)
}

/// One film-check measurement recorded for diagnosis.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbFilmMeasurement {
    /// Work-RAM index under test.
    pub index: u16,
    /// Audited entry position.
    pub entry: u16,
    /// Recorded camera at the compared frame.
    pub camera: u32,
    /// Recorded frame index.
    pub frame: u16,
    /// Absolute difference of the two candidate values.
    pub difference: i32,
    /// Lowest differing rendered column minus the smaller candidate value.
    pub offset: i32,
    /// Differing span minus the candidate difference.
    pub width: i32,
}

/// Record every film-check measurement for the indices that reach verification.
///
/// # Errors
///
/// Returns an error when an identifier is absent from the source or when
/// emulation or snapshotting fails.
pub fn diagnose_smb_film_measurements(
    rom: &[u8],
    source: &SmbArchiveReport,
    ids: &[u64],
) -> Result<Vec<SmbFilmMeasurement>, Box<dyn Error>> {
    let active = active_source_entries(source);
    let mut selected = Vec::with_capacity(ids.len());
    for id in ids {
        let entry = active
            .iter()
            .find(|entry| entry.id == *id)
            .ok_or("audit identifier is not an active source entry")?;
        selected.push((*entry, entry.key.progress));
    }
    let mut target = SmbTarget::from_smb_rom_bytes(rom)?;
    let mut prefix = PlayerColumnPrefix::new(&mut target)?;
    let mut recordings = Vec::with_capacity(selected.len());
    for (entry, progress) in &selected {
        recordings.push(record_player_column_entry(
            &mut target,
            &mut prefix,
            entry,
            *progress,
            PlayerColumnRules::SPREAD,
        )?);
    }
    let (report, comparisons) =
        analyze_player_column_with_rules(&recordings, PlayerColumnRules::SPREAD);
    let mut measurements = Vec::new();
    for index in &report.camera_relative_survivors {
        for comparison in &comparisons {
            let recording = &recordings[comparison.entry];
            let position = usize::from(*index);
            let left = i32::from(
                recording.continuations[comparison.left].wram[comparison.frame][position],
            );
            let right = i32::from(
                recording.continuations[comparison.right].wram[comparison.frame][position],
            );
            let difference = (left - right).abs();
            if difference < PLAYER_COLUMN_FILM_GAP {
                continue;
            }
            measurements.push(SmbFilmMeasurement {
                index: *index,
                entry: u16::try_from(comparison.entry).unwrap_or(u16::MAX),
                camera: comparison.camera,
                frame: u16::try_from(comparison.frame).unwrap_or(u16::MAX),
                difference,
                offset: comparison.lowest - left.min(right),
                width: comparison.highest - comparison.lowest + 1 - difference,
            });
        }
    }
    Ok(measurements)
}

/// Choose census-admitted entries in descending progress with a per-bucket cap.
///
/// The cap makes the audited endpoints span several camera positions, which the
/// camera-spread verification requires.
///
/// # Errors
///
/// Returns an error when an admitted identifier is absent from the source.
pub fn select_smb_spread_audit_ids(
    source: &SmbArchiveReport,
    admitted: &[u64],
    wanted: usize,
) -> Result<Vec<u64>, Box<dyn Error>> {
    let active = active_source_entries(source);
    let mut per_bucket = BTreeMap::<u16, usize>::new();
    let mut chosen = Vec::with_capacity(wanted);
    for id in admitted {
        if chosen.len() >= wanted {
            break;
        }
        let entry = active
            .iter()
            .find(|entry| entry.id == *id)
            .ok_or("admitted identifier is not an active source entry")?;
        let taken = per_bucket.entry(entry.key.progress).or_insert(0);
        if *taken >= PLAYER_COLUMN_BUCKET_CAP {
            continue;
        }
        *taken = taken.saturating_add(1);
        chosen.push(*id);
    }
    Ok(chosen)
}

fn player_column_candidates(
    source: &SmbArchiveReport,
    selection_mode: SmbPlayerColumnSelection,
) -> Result<Vec<Vec<PlayerColumnCandidate<'_>>>, Box<dyn Error>> {
    let active = active_source_entries(source);
    let cap = match selection_mode {
        SmbPlayerColumnSelection::FirstOrdered => PLAYER_COLUMN_SLICE_SIZE,
        SmbPlayerColumnSelection::FirstSteerable => PLAYER_COLUMN_SCAN_CAP,
        SmbPlayerColumnSelection::FirstCameraAdvancing => PLAYER_COLUMN_ADVANCING_SCAN_CAP,
    };
    let mut slices = Vec::with_capacity(PLAYER_COLUMN_SLICES.len());
    for progress in PLAYER_COLUMN_SLICES {
        let mut slice = active
            .iter()
            .filter(|entry| {
                entry.key.world == 0 && entry.key.level == 2 && entry.key.progress == progress
            })
            .copied()
            .collect::<Vec<_>>();
        slice.sort_by_key(|entry| (entry.input.clone(), entry.id));
        if slice.len() < PLAYER_COLUMN_SLICE_SIZE {
            return Err("audit slice has fewer than eight active entries".into());
        }
        slice.truncate(cap);
        slices.push(
            slice
                .into_iter()
                .map(|entry| (entry, progress))
                .collect::<Vec<_>>(),
        );
    }
    Ok(slices)
}

/// Count how many retained representatives per progress bucket the controller
/// can still move rightwards.
///
/// # Errors
///
/// Returns an error when emulation or snapshotting fails.
pub fn census_smb_control_authority(
    rom: &[u8],
    source: &SmbArchiveReport,
) -> Result<SmbControlCensusReport, Box<dyn Error>> {
    let mut entries = active_source_entries(source)
        .into_iter()
        .filter(|entry| entry.key.world == 0 && entry.key.level == 2)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| (entry.key.progress, entry.input.clone(), entry.id));
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    let mut prefix = PlayerColumnPrefix::new(&mut target)?;
    let mut buckets = BTreeMap::<u16, (u64, u64)>::new();
    let mut admitted_entries = Vec::new();
    for entry in &entries {
        let endpoint = replay_player_column_endpoint(&mut target, &mut prefix, entry)?;
        target.restore(&endpoint)?;
        let first = smb_camera_pixels(target.wram());
        let mut last = first;
        for _ in 0..PLAYER_COLUMN_FRAMES {
            if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                break;
            }
            target.apply(&ButtonChord::new(PLAYER_COLUMN_MASKS[1], 1));
            last = smb_camera_pixels(target.wram());
        }
        let admitted = last.saturating_sub(first) >= PLAYER_COLUMN_CAMERA_ADVANCE;
        let counts = buckets.entry(entry.key.progress).or_insert((0, 0));
        counts.0 = counts.0.saturating_add(1);
        if admitted {
            counts.1 = counts.1.saturating_add(1);
            admitted_entries.push(*entry);
        }
    }
    admitted_entries
        .sort_by_key(|entry| (Reverse(entry.key.progress), entry.input.clone(), entry.id));
    Ok(SmbControlCensusReport {
        continuation_frames: PLAYER_COLUMN_FRAMES,
        camera_advance: PLAYER_COLUMN_CAMERA_ADVANCE,
        buckets: buckets
            .iter()
            .map(|(progress, (active, admitted))| SmbControlCensusBucket {
                progress: *progress,
                active: *active,
                admitted: *admitted,
            })
            .collect(),
        active: u64::try_from(entries.len()).unwrap_or(u64::MAX),
        admitted: u64::try_from(admitted_entries.len()).unwrap_or(u64::MAX),
        admitted_ids: admitted_entries.iter().map(|entry| entry.id).collect(),
    })
}

fn player_column_advances_camera(recording: &EntryRecording) -> bool {
    let camera = &recording.continuations[1].camera;
    camera[camera.len().saturating_sub(1)].saturating_sub(camera[0]) >= PLAYER_COLUMN_CAMERA_ADVANCE
}

fn player_column_is_steerable(recording: &EntryRecording) -> bool {
    let right = &recording.continuations[1].wram;
    let left = &recording.continuations[2].wram;
    right.last() != left.last()
}

fn first_audited_entry_per_slice(recordings: &[EntryRecording]) -> Vec<usize> {
    let mut seen = BTreeSet::new();
    recordings
        .iter()
        .enumerate()
        .filter(|(_, recording)| seen.insert(recording.progress))
        .map(|(entry, _)| entry)
        .collect()
}

/// Reusable genesis-rooted prefix so consecutive ordered candidates share replay work.
struct PlayerColumnPrefix {
    input: SmbInput,
    snapshots: Vec<SmbSnapshot>,
}

impl PlayerColumnPrefix {
    fn new(target: &mut SmbTarget) -> Result<Self, Box<dyn Error>> {
        target.reset();
        let genesis = target
            .snapshot()
            .ok_or("failed to snapshot audit genesis")?;
        Ok(Self {
            input: SmbInput::default(),
            snapshots: vec![genesis],
        })
    }
}

fn replay_player_column_endpoint(
    target: &mut SmbTarget,
    prefix: &mut PlayerColumnPrefix,
    entry: &SmbArchiveEntryReport,
) -> Result<SmbSnapshot, Box<dyn Error>> {
    let common = prefix
        .input
        .actions
        .iter()
        .zip(&entry.input.actions)
        .take_while(|(left, right)| left == right)
        .count();
    target.restore(&prefix.snapshots[common])?;
    prefix.snapshots.truncate(common + 1);
    for action in &entry.input.actions[common..] {
        target.apply(action);
        let snapshot = target
            .snapshot()
            .ok_or("failed to snapshot audit replay prefix")?;
        prefix.snapshots.push(snapshot);
        if target.is_dead() || target.exit_kind() != ExitKind::Ok {
            break;
        }
    }
    prefix.input = entry.input.clone();
    target
        .snapshot()
        .ok_or_else(|| "failed to snapshot audit endpoint".into())
}

fn record_player_column_entry(
    target: &mut SmbTarget,
    prefix: &mut PlayerColumnPrefix,
    entry: &SmbArchiveEntryReport,
    progress: u16,
    rules: PlayerColumnRules,
) -> Result<EntryRecording, Box<dyn Error>> {
    let endpoint = replay_player_column_endpoint(target, prefix, entry)?;
    // The emulator's frame buffer is not part of a restored snapshot, so the endpoint
    // image is captured once here and reused as every continuation's frame zero.
    let endpoint_columns = column_signatures(&target.frame_rgba())?;
    let mut continuations = Vec::with_capacity(PLAYER_COLUMN_MASKS.len());
    for mask in PLAYER_COLUMN_MASKS {
        target.restore(&endpoint)?;
        let mut recording = ContinuationRecording {
            wram: vec![*target.wram()],
            columns: vec![endpoint_columns],
            camera: vec![smb_camera_pixels(target.wram())],
        };
        for _ in 0..PLAYER_COLUMN_FRAMES {
            if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                break;
            }
            target.apply(&ButtonChord::new(mask, 1));
            push_player_column_frame(target, &mut recording)?;
            if rules.truncate_on_camera_decrease {
                let camera = &recording.camera;
                let last = camera.len().saturating_sub(1);
                if last > 0 && camera[last] < camera[last - 1] {
                    break;
                }
            }
        }
        continuations.push(recording);
    }
    Ok(EntryRecording {
        id: entry.id,
        progress,
        frontier: progress == PLAYER_COLUMN_SLICES[0],
        continuations,
    })
}

fn push_player_column_frame(
    target: &mut SmbTarget,
    recording: &mut ContinuationRecording,
) -> Result<(), Box<dyn Error>> {
    recording.wram.push(*target.wram());
    recording.camera.push(smb_camera_pixels(target.wram()));
    recording
        .columns
        .push(column_signatures(&target.frame_rgba())?);
    Ok(())
}

fn column_signatures(rgba: &[u8]) -> Result<[u64; 256], Box<dyn Error>> {
    if rgba.len() != FRAME_WIDTH * FRAME_HEIGHT * 4 {
        return Err("unexpected TetaNES RGBA frame length".into());
    }
    let mut signatures = [0xcbf2_9ce4_8422_2325_u64; 256];
    for row in rgba.chunks_exact(FRAME_WIDTH * 4) {
        for (column, pixel) in row.chunks_exact(4).enumerate() {
            let mut hash = signatures[column];
            for byte in pixel {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            signatures[column] = hash;
        }
    }
    Ok(signatures)
}

fn analyze_player_column(
    recordings: &[EntryRecording],
) -> (SmbPlayerColumnReport, Vec<FilmComparison>) {
    analyze_player_column_with_rules(recordings, PlayerColumnRules::LEGACY)
}

fn analyze_player_column_with_rules(
    recordings: &[EntryRecording],
    rules: PlayerColumnRules,
) -> (SmbPlayerColumnReport, Vec<FilmComparison>) {
    let mut distinct_value_survivors = 0_u64;
    let mut smooth_survivors = 0_u64;
    let mut left_direction_survivors = 0_u64;
    let mut right_direction_survivors = 0_u64;
    let mut camera_relative_survivors = Vec::new();
    let qualifying = qualifying_right_continuations(recordings);
    for index in 0..2_048_usize {
        if !player_column_distinct(recordings, index) {
            continue;
        }
        distinct_value_survivors = distinct_value_survivors.saturating_add(1);
        if !player_column_smooth(recordings, index) {
            continue;
        }
        smooth_survivors = smooth_survivors.saturating_add(1);
        if !player_column_left_direction(recordings, index) {
            continue;
        }
        left_direction_survivors = left_direction_survivors.saturating_add(1);
        if rules.require_right_direction && !player_column_right_direction(recordings, index) {
            continue;
        }
        right_direction_survivors = right_direction_survivors.saturating_add(1);
        if rules.require_camera_relative
            && (qualifying.is_empty()
                || !player_column_camera_relative(recordings, index, &qualifying))
        {
            continue;
        }
        camera_relative_survivors.push(u16::try_from(index).unwrap_or(u16::MAX));
    }
    let comparisons = film_comparisons(recordings);
    let film_survivors = camera_relative_survivors
        .iter()
        .filter_map(|index| film_evidence(recordings, &comparisons, *index, rules))
        .collect::<Vec<_>>();
    let stride_rejected = film_survivors
        .iter()
        .map(|evidence| evidence.index)
        .filter(|index| {
            film_survivors.iter().any(|other| {
                PLAYER_COLUMN_STRIDES.iter().any(|stride| {
                    other.index == index.saturating_add(*stride)
                        || other.index.saturating_add(*stride) == *index
                })
            })
        })
        .collect::<Vec<_>>();
    let selected = film_survivors
        .iter()
        .find(|evidence| !stride_rejected.contains(&evidence.index))
        .copied();
    let audited = recordings
        .iter()
        .map(|recording| SmbPlayerColumnAuditedEntry {
            id: recording.id,
            progress: recording.progress,
            frontier: recording.frontier,
            endpoint_camera: recording.continuations[0].camera[0],
            recorded_frames: recording
                .continuations
                .iter()
                .map(|continuation| u16::try_from(continuation.wram.len()).unwrap_or(u16::MAX))
                .collect(),
        })
        .collect::<Vec<_>>();
    (
        SmbPlayerColumnReport {
            continuation_frames: PLAYER_COLUMN_FRAMES,
            continuation_masks: PLAYER_COLUMN_MASKS.to_vec(),
            audited,
            scanned_per_slice: Vec::new(),
            steerable_per_slice: Vec::new(),
            distinct_value_survivors,
            smooth_survivors,
            left_direction_survivors,
            right_direction_survivors,
            qualifying_right_continuations: u64::try_from(qualifying.len()).unwrap_or(u64::MAX),
            camera_relative_survivors,
            film_survivors,
            stride_rejected,
            selected,
        },
        comparisons,
    )
}

fn player_column_distinct(recordings: &[EntryRecording], index: usize) -> bool {
    let mut seen = [false; 256];
    let mut distinct = 0_usize;
    for recording in recordings {
        for continuation in &recording.continuations {
            for wram in &continuation.wram {
                let value = usize::from(wram[index]);
                if !seen[value] {
                    seen[value] = true;
                    distinct = distinct.saturating_add(1);
                    if distinct >= PLAYER_COLUMN_MIN_DISTINCT {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn player_column_smooth(recordings: &[EntryRecording], index: usize) -> bool {
    recordings.iter().all(|recording| {
        recording.continuations.iter().all(|continuation| {
            continuation.wram.windows(2).all(|pair| {
                (i32::from(pair[1][index]) - i32::from(pair[0][index])).abs()
                    <= PLAYER_COLUMN_MAX_STEP
            })
        })
    })
}

fn player_column_left_direction(recordings: &[EntryRecording], index: usize) -> bool {
    let mut decreasing = 0_usize;
    for recording in recordings {
        let (first, last) = continuation_endpoints(recording, 2, index);
        if last > first + PLAYER_COLUMN_LEFT_SLACK {
            return false;
        }
        if last <= first - PLAYER_COLUMN_LEFT_DECREASE {
            decreasing = decreasing.saturating_add(1);
        }
    }
    decreasing >= player_column_left_threshold(recordings.len())
}

fn player_column_left_threshold(audited: usize) -> usize {
    if audited == PLAYER_COLUMN_LEFT_ENTRIES_BASE {
        return PLAYER_COLUMN_LEFT_ENTRIES;
    }
    audited
        .saturating_mul(3)
        .saturating_add(3)
        .saturating_div(4)
}

fn player_column_right_direction(recordings: &[EntryRecording], index: usize) -> bool {
    recordings.iter().all(|recording| {
        let (first, last) = continuation_endpoints(recording, 1, index);
        last >= first - PLAYER_COLUMN_RIGHT_SLACK
    })
}

fn continuation_endpoints(
    recording: &EntryRecording,
    continuation: usize,
    index: usize,
) -> (i32, i32) {
    let frames = &recording.continuations[continuation].wram;
    let first = i32::from(frames[0][index]);
    let last = i32::from(frames[frames.len().saturating_sub(1)][index]);
    (first, last)
}

fn qualifying_right_continuations(recordings: &[EntryRecording]) -> Vec<usize> {
    recordings
        .iter()
        .enumerate()
        .filter(|(_, recording)| {
            let camera = &recording.continuations[1].camera;
            camera[camera.len().saturating_sub(1)].saturating_sub(camera[0])
                >= PLAYER_COLUMN_CAMERA_ADVANCE
        })
        .map(|(entry, _)| entry)
        .collect()
}

fn player_column_camera_relative(
    recordings: &[EntryRecording],
    index: usize,
    qualifying: &[usize],
) -> bool {
    qualifying.iter().all(|entry| {
        let recording = &recordings[*entry];
        let camera = &recording.continuations[1].camera;
        let advance = camera[camera.len().saturating_sub(1)].saturating_sub(camera[0]);
        let (first, last) = continuation_endpoints(recording, 1, index);
        u32::try_from((last - first).abs()).unwrap_or(u32::MAX) < advance
    })
}

fn film_comparisons(recordings: &[EntryRecording]) -> Vec<FilmComparison> {
    let mut comparisons = Vec::new();
    for (entry, recording) in recordings.iter().enumerate() {
        for left in 0..recording.continuations.len() {
            for right in left.saturating_add(1)..recording.continuations.len() {
                let first = &recording.continuations[left];
                let second = &recording.continuations[right];
                for frame in 0..first.wram.len().min(second.wram.len()) {
                    if first.camera[frame] != second.camera[frame] {
                        continue;
                    }
                    let differing = (0..256)
                        .filter(|column| {
                            first.columns[frame][*column] != second.columns[frame][*column]
                        })
                        .collect::<Vec<_>>();
                    let (Some(lowest), Some(highest)) = (differing.first(), differing.last())
                    else {
                        continue;
                    };
                    comparisons.push(FilmComparison {
                        entry,
                        left,
                        right,
                        frame,
                        lowest: i32::try_from(*lowest).unwrap_or(i32::MAX),
                        highest: i32::try_from(*highest).unwrap_or(i32::MAX),
                        camera: first.camera[frame],
                    });
                }
            }
        }
    }
    comparisons
}

fn film_offset(
    recordings: &[EntryRecording],
    comparison: &FilmComparison,
    index: u16,
) -> Option<(i32, i32)> {
    let recording = &recordings[comparison.entry];
    let index = usize::from(index);
    let left = i32::from(recording.continuations[comparison.left].wram[comparison.frame][index]);
    let right = i32::from(recording.continuations[comparison.right].wram[comparison.frame][index]);
    let difference = (left - right).abs();
    if difference < PLAYER_COLUMN_FILM_GAP {
        return None;
    }
    let offset = comparison.lowest - left.min(right);
    let width = comparison.highest - comparison.lowest + 1 - difference;
    Some((offset, width))
}

fn film_evidence(
    recordings: &[EntryRecording],
    comparisons: &[FilmComparison],
    index: u16,
    rules: PlayerColumnRules,
) -> Option<SmbPlayerColumnFilmEvidence> {
    let measured = comparisons
        .iter()
        .filter_map(|comparison| {
            film_offset(recordings, comparison, index)
                .map(|(offset, width)| (offset, width, comparison.camera))
        })
        .collect::<Vec<_>>();
    // Pass or fail is "some offset agrees at least PLAYER_COLUMN_FILM_MIN_AGREE times";
    // the offset reported is the one the most comparisons agree with, so the recorded
    // number describes the identification rather than the low edge of the tolerance band.
    let best = (-PLAYER_COLUMN_FILM_OFFSETS..=PLAYER_COLUMN_FILM_OFFSETS)
        .map(|offset| {
            let agreeing = measured
                .iter()
                .filter(|(measured_offset, width, _)| {
                    (measured_offset - offset).abs() <= PLAYER_COLUMN_FILM_TOLERANCE
                        && (PLAYER_COLUMN_FILM_MIN_WIDTH..=PLAYER_COLUMN_FILM_MAX_WIDTH)
                            .contains(width)
                })
                .map(|(_, _, camera)| *camera)
                .collect::<Vec<_>>();
            let spread = match (agreeing.iter().min(), agreeing.iter().max()) {
                (Some(low), Some(high)) => high.saturating_sub(*low),
                _ => 0,
            };
            (agreeing.len(), offset, spread)
        })
        .filter(|(agreeing, _, spread)| {
            *agreeing >= PLAYER_COLUMN_FILM_MIN_AGREE
                && (!rules.require_camera_spread || *spread >= PLAYER_COLUMN_CAMERA_SPREAD)
        })
        .max_by_key(|(agreeing, offset, _)| (*agreeing, Reverse(offset.abs()), Reverse(*offset)))?;
    Some(SmbPlayerColumnFilmEvidence {
        index,
        offset: i16::try_from(best.1).unwrap_or(i16::MAX),
        agreeing_comparisons: u64::try_from(best.0).unwrap_or(u64::MAX),
        comparisons: u64::try_from(measured.len()).unwrap_or(u64::MAX),
        camera_spread: best.2,
    })
}

/// One recorded frame of a screen-column diagnosis continuation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbPlayerColumnFrame {
    /// Recorded frame index, with zero the audited endpoint.
    pub frame: u16,
    /// Raw screen-page byte.
    pub camera_page: u8,
    /// Raw screen-x byte.
    pub camera_x: u8,
    /// Raw player vertical-position byte.
    pub player_y: u8,
    /// Raw player engine-state byte.
    pub engine_state: u8,
    /// Program's own decoded mechanical state at this frame.
    pub decoded: SmbMechanicalState,
    /// Program's own decoded milestone ladder at this frame.
    pub milestones: SmbMilestones,
}

/// Per-entry continuation traces recorded for the screen-column diagnosis.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbPlayerColumnTrace {
    /// Stable source archive identifier.
    pub id: u64,
    /// Recorded progress bucket of the audited slice.
    pub progress: u16,
    /// One frame list per fixed continuation mask.
    pub continuations: Vec<Vec<SmbPlayerColumnFrame>>,
    /// Work RAM sampled every tenth recorded frame of every continuation.
    pub raw_wram: Vec<Vec<Vec<u8>>>,
}

/// Record continuation traces and frame strips for the audited screen-column slices.
///
/// This diagnosis runs no search, changes no search behavior, and involves no
/// model. It exposes recorded evidence about the same sixteen entries the audit
/// selected.
///
/// # Errors
///
/// Returns an error when the source lacks the audit slices or when emulation,
/// snapshotting, or rendering fails.
pub fn diagnose_smb_player_column(
    rom: &[u8],
    source: &SmbArchiveReport,
) -> Result<(Vec<SmbPlayerColumnTrace>, Vec<SmbAuditFrame>), Box<dyn Error>> {
    let selected = player_column_candidates(source, SmbPlayerColumnSelection::FirstOrdered)?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut target = SmbTarget::from_smb_rom_bytes(rom)?;
    let mut prefix = PlayerColumnPrefix::new(&mut target)?;
    let mut traces = Vec::with_capacity(selected.len());
    for (source_entry, progress) in &selected {
        let recording = record_player_column_entry(
            &mut target,
            &mut prefix,
            source_entry,
            *progress,
            PlayerColumnRules::LEGACY,
        )?;
        traces.push(SmbPlayerColumnTrace {
            id: source_entry.id,
            progress: *progress,
            continuations: recording
                .continuations
                .iter()
                .map(|continuation| {
                    continuation
                        .wram
                        .iter()
                        .enumerate()
                        .map(|(frame, wram)| SmbPlayerColumnFrame {
                            frame: u16::try_from(frame).unwrap_or(u16::MAX),
                            camera_page: wram[0x071a],
                            camera_x: wram[0x071c],
                            player_y: wram[0x00ce],
                            engine_state: wram[0x000e],
                            decoded: smb_mechanical_state_from_wram(wram),
                            milestones: smb_milestones_from_wram(wram),
                        })
                        .collect()
                })
                .collect(),
            raw_wram: recording
                .continuations
                .iter()
                .map(|continuation| {
                    continuation
                        .wram
                        .iter()
                        .step_by(10)
                        .map(|wram| wram.to_vec())
                        .collect()
                })
                .collect(),
        });
    }
    let mut requests = BTreeSet::new();
    for slice in 0..PLAYER_COLUMN_SLICES.len() {
        let entry = slice.saturating_mul(PLAYER_COLUMN_SLICE_SIZE);
        for continuation in 0..PLAYER_COLUMN_MASKS.len() {
            for frame in (0..=usize::from(PLAYER_COLUMN_FRAMES)).step_by(10) {
                requests.insert((entry, continuation, frame));
            }
        }
    }
    let frames = render_player_column_frames(&mut target, &selected, &requests)?;
    Ok((traces, frames))
}

fn render_player_column_frames(
    target: &mut SmbTarget,
    selected: &[PlayerColumnCandidate<'_>],
    requests: &BTreeSet<(usize, usize, usize)>,
) -> Result<Vec<SmbAuditFrame>, Box<dyn Error>> {
    let mut frames = Vec::new();
    let mut prefix = PlayerColumnPrefix::new(target)?;
    let mut entries = requests
        .iter()
        .map(|(entry, _, _)| *entry)
        .collect::<Vec<_>>();
    entries.dedup();
    for entry in entries {
        let (source, _) = selected[entry];
        let endpoint = replay_player_column_endpoint(target, &mut prefix, source)?;
        let endpoint_rgba = target.frame_rgba();
        for (continuation, mask) in PLAYER_COLUMN_MASKS.into_iter().enumerate() {
            let wanted = requests
                .iter()
                .filter(|(request_entry, request_continuation, _)| {
                    *request_entry == entry && *request_continuation == continuation
                })
                .map(|(_, _, frame)| *frame)
                .collect::<Vec<_>>();
            let Some(last) = wanted.last().copied() else {
                continue;
            };
            target.restore(&endpoint)?;
            for frame in 0..=last {
                if frame > 0 {
                    if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                        break;
                    }
                    target.apply(&ButtonChord::new(mask, 1));
                }
                if wanted.contains(&frame) {
                    frames.push(SmbAuditFrame {
                        name: format!(
                            "entry-{entry:02}-id-{}-mask-{mask:02x}-frame-{frame:03}.png",
                            source.id
                        ),
                        rgba: if frame == 0 {
                            endpoint_rgba.clone()
                        } else {
                            target.frame_rgba()
                        },
                    });
                }
            }
        }
    }
    Ok(frames)
}

const DEATH_AUDIT_ENTRIES: usize = 8;
const DEATH_AUDIT_SCAN_CAP: usize = 128;
const DEATH_AUDIT_BUCKET_CAP: usize = 2;
const DEATH_AUDIT_FRAMES: usize = 240;
const DEATH_AUDIT_THRESHOLDS: [u8; 7] = [1, 2, 3, 4, 5, 6, 7];

/// One candidate terminal condition evaluated by the D34 audit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeathCandidate {
    /// The frozen condition: engine state equals its killed value.
    FrozenKill,
    /// The life counter is strictly below its value at the start of the replay.
    LifeCounterBelowStart,
    /// The vertical page byte is at least a fixed threshold.
    VerticalPageAtLeast(u8),
    /// The combined vertical position is at least a fixed threshold of pages.
    VerticalPositionAtLeast(u8),
}

impl DeathCandidate {
    fn name(self) -> String {
        match self {
            Self::FrozenKill => "K0".to_owned(),
            Self::LifeCounterBelowStart => "K1".to_owned(),
            Self::VerticalPageAtLeast(threshold) => format!("K2({threshold})"),
            Self::VerticalPositionAtLeast(threshold) => format!("K3({threshold})"),
        }
    }

    fn holds(self, bytes: SmbDeathBytes, start_life_counter: u8) -> bool {
        match self {
            Self::FrozenKill => bytes.engine_state == PLAYER_KILLED_STATE,
            Self::LifeCounterBelowStart => bytes.life_counter < start_life_counter,
            Self::VerticalPageAtLeast(threshold) => bytes.vertical_page >= threshold,
            Self::VerticalPositionAtLeast(threshold) => {
                u32::from(bytes.vertical_page) * 256 + u32::from(bytes.vertical_low)
                    >= u32::from(threshold) * 256
            }
        }
    }
}

fn death_candidate_order() -> Vec<DeathCandidate> {
    let mut candidates = vec![
        DeathCandidate::FrozenKill,
        DeathCandidate::LifeCounterBelowStart,
    ];
    candidates.extend(DEATH_AUDIT_THRESHOLDS.map(DeathCandidate::VerticalPageAtLeast));
    candidates.extend(DEATH_AUDIT_THRESHOLDS.map(DeathCandidate::VerticalPositionAtLeast));
    candidates
}

/// One evaluated candidate terminal condition.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbDeathCandidateReport {
    /// Fixed candidate name from the registration.
    pub name: String,
    /// Control frames on which the candidate is true; zero is required to pass.
    pub control_true_frames: u64,
    /// First-trip frame index per uncontrolled continuation, or `-1` when it never trips.
    pub trip_frames: Vec<i32>,
    /// Identifiers of the uncontrolled continuations on which the candidate never trips.
    pub without_trip: Vec<u64>,
    /// Median first-trip frame index, recorded only for a passing candidate.
    pub median_trip_frame: Option<u16>,
    /// Largest first-trip frame index, recorded only for a passing candidate.
    pub max_trip_frame: Option<u16>,
    /// Whether the registered acceptance rule admits this candidate.
    pub passes: bool,
}

/// One recorded uncontrolled continuation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbDeathTrace {
    /// Stable source archive identifier.
    pub id: u64,
    /// Recorded progress bucket of the continued entry.
    pub progress: u16,
    /// Whether the life counter was already below its genesis value at the endpoint.
    pub life_counter_below_genesis_at_endpoint: bool,
    /// Raw recorded bytes per frame, starting at the endpoint.
    pub frames: Vec<SmbDeathBytes>,
}

/// Complete terminal-death decode audit report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbTerminalDeathReport {
    /// Champion actions consumed before the control replay was truncated.
    pub control_actions: usize,
    /// Control frames recorded, including the genesis frame.
    pub control_frames: u64,
    /// Fixed uncontrolled continuation length.
    pub continuation_frames: u16,
    /// Entries whose continuations were run during the qualification scan.
    pub scanned: u64,
    /// Identifiers admitted into the uncontrolled population.
    pub uncontrolled_ids: Vec<u64>,
    /// Every candidate in its fixed registered order.
    pub candidates: Vec<SmbDeathCandidateReport>,
    /// The candidate the registered adoption rule would select, if any.
    pub adoption_rule_selects: Option<String>,
    /// Raw recorded bytes per control frame.
    pub control_trace: Vec<SmbDeathBytes>,
    /// Raw recorded bytes per uncontrolled continuation.
    pub uncontrolled_traces: Vec<SmbDeathTrace>,
}

/// Audit candidate terminal-death conditions against recorded live and uncontrolled play.
///
/// # Errors
///
/// Returns an error when the source has no active entries, when the recorded
/// champion input never reaches the maximum recorded tuple, or when emulation
/// or snapshotting fails.
pub fn audit_smb_terminal_death(
    rom: &[u8],
    source: &SmbArchiveReport,
) -> Result<SmbTerminalDeathReport, Box<dyn Error>> {
    let active = active_source_entries(source);
    let max_tuple = active
        .iter()
        .map(|entry| (entry.key.world, entry.key.level))
        .max()
        .ok_or("source archive has no active entries")?;
    let mut target = SmbTarget::from_smb_rom_bytes(rom)?;
    let (control_trace, control_actions, genesis_life) =
        record_death_audit_control(&mut target, source, max_tuple)?;
    let (uncontrolled_traces, scanned) =
        record_death_audit_uncontrolled(&mut target, &active, max_tuple, genesis_life)?;
    let complete = uncontrolled_traces.len() == DEATH_AUDIT_ENTRIES;
    let candidates = death_candidate_order()
        .into_iter()
        .map(|candidate| {
            evaluate_death_candidate(
                candidate,
                &control_trace,
                &uncontrolled_traces,
                genesis_life,
                complete,
            )
        })
        .collect::<Vec<_>>();
    Ok(SmbTerminalDeathReport {
        control_actions,
        control_frames: u64::try_from(control_trace.len()).unwrap_or(u64::MAX),
        continuation_frames: u16::try_from(DEATH_AUDIT_FRAMES).unwrap_or(u16::MAX),
        scanned,
        uncontrolled_ids: uncontrolled_traces.iter().map(|trace| trace.id).collect(),
        adoption_rule_selects: adopt_death_candidate(&candidates),
        candidates,
        control_trace,
        uncontrolled_traces,
    })
}

fn record_death_audit_control(
    target: &mut SmbTarget,
    source: &SmbArchiveReport,
    max_tuple: (u8, u8),
) -> Result<(Vec<SmbDeathBytes>, usize, u8), Box<dyn Error>> {
    target.reset();
    let genesis_life = smb_death_bytes(target.wram()).life_counter;
    let mut trace = vec![smb_death_bytes(target.wram())];
    let mut actions = 0_usize;
    for action in &source.champion_input.actions {
        actions = actions.saturating_add(1);
        for _ in 0..action.bounded_hold_frames() {
            if target.is_dead() {
                return Err("champion control replay reached the frozen terminal condition".into());
            }
            target.apply(&ButtonChord::new(action.buttons, 1));
            if target.exit_kind() != ExitKind::Ok {
                return Err("champion control replay failed to emulate".into());
            }
            trace.push(smb_death_bytes(target.wram()));
            let decoded = smb_mechanical_state_from_wram(target.wram());
            if (decoded.world, decoded.level) == max_tuple {
                return Ok((trace, actions, genesis_life));
            }
        }
    }
    Err("the recorded champion input never reaches the maximum recorded tuple".into())
}

fn record_death_audit_uncontrolled(
    target: &mut SmbTarget,
    active: &[&SmbArchiveEntryReport],
    max_tuple: (u8, u8),
    genesis_life: u8,
) -> Result<(Vec<SmbDeathTrace>, u64), Box<dyn Error>> {
    let mut entries = active
        .iter()
        .copied()
        .filter(|entry| (entry.key.world, entry.key.level) == max_tuple)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| (Reverse(entry.key.progress), entry.input.clone(), entry.id));
    let mut prefix = PlayerColumnPrefix::new(target)?;
    let mut per_bucket = BTreeMap::<u16, usize>::new();
    let mut traces = Vec::with_capacity(DEATH_AUDIT_ENTRIES);
    let mut scanned = 0_usize;
    for entry in entries {
        if traces.len() >= DEATH_AUDIT_ENTRIES || scanned >= DEATH_AUDIT_SCAN_CAP {
            break;
        }
        let taken = per_bucket.entry(entry.key.progress).or_insert(0);
        if *taken >= DEATH_AUDIT_BUCKET_CAP {
            continue;
        }
        scanned = scanned.saturating_add(1);
        let endpoint = replay_player_column_endpoint(target, &mut prefix, entry)?;
        // The frame buffer survives no restore, so the endpoint image is captured here.
        let endpoint_columns = column_signatures(&target.frame_rgba())?;
        let endpoint_bytes = smb_death_bytes(target.wram());
        if !death_audit_is_uncontrolled(target, &endpoint, endpoint_columns)? {
            continue;
        }
        *per_bucket.entry(entry.key.progress).or_insert(0) += 1;
        target.restore(&endpoint)?;
        let mut frames = vec![endpoint_bytes];
        for _ in 0..DEATH_AUDIT_FRAMES {
            target.apply(&ButtonChord::new(PLAYER_COLUMN_MASKS[0], 1));
            if target.exit_kind() != ExitKind::Ok {
                break;
            }
            frames.push(smb_death_bytes(target.wram()));
            if target.is_dead() {
                break;
            }
        }
        traces.push(SmbDeathTrace {
            id: entry.id,
            progress: entry.key.progress,
            life_counter_below_genesis_at_endpoint: endpoint_bytes.life_counter < genesis_life,
            frames,
        });
    }
    Ok((traces, u64::try_from(scanned).unwrap_or(u64::MAX)))
}

/// Report whether the controller has no rendered effect over the fixed continuation.
fn death_audit_is_uncontrolled(
    target: &mut SmbTarget,
    endpoint: &SmbSnapshot,
    endpoint_columns: [u64; 256],
) -> Result<bool, Box<dyn Error>> {
    let mut recorded = Vec::with_capacity(2);
    for mask in [PLAYER_COLUMN_MASKS[0], PLAYER_COLUMN_MASKS[2]] {
        target.restore(endpoint)?;
        let mut columns = vec![endpoint_columns];
        for _ in 0..PLAYER_COLUMN_FRAMES {
            if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                break;
            }
            target.apply(&ButtonChord::new(mask, 1));
            columns.push(column_signatures(&target.frame_rgba())?);
        }
        recorded.push(columns);
    }
    let frames = recorded[0].len().min(recorded[1].len());
    Ok((0..frames).all(|frame| recorded[0][frame] == recorded[1][frame]))
}

fn evaluate_death_candidate(
    candidate: DeathCandidate,
    control: &[SmbDeathBytes],
    uncontrolled: &[SmbDeathTrace],
    genesis_life: u8,
    complete: bool,
) -> SmbDeathCandidateReport {
    let control_true_frames = control
        .iter()
        .filter(|bytes| candidate.holds(**bytes, genesis_life))
        .count();
    let mut trip_frames = Vec::with_capacity(uncontrolled.len());
    let mut without_trip = Vec::new();
    for trace in uncontrolled {
        match trace
            .frames
            .iter()
            .position(|bytes| candidate.holds(*bytes, genesis_life))
        {
            Some(frame) => trip_frames.push(i32::try_from(frame).unwrap_or(i32::MAX)),
            None => {
                trip_frames.push(-1);
                without_trip.push(trace.id);
            }
        }
    }
    let passes = complete && control_true_frames == 0 && without_trip.is_empty();
    let mut sorted = trip_frames.clone();
    sorted.sort_unstable();
    let median = sorted
        .get(sorted.len() / 2)
        .and_then(|frame| u16::try_from(*frame).ok());
    let largest = sorted.last().and_then(|frame| u16::try_from(*frame).ok());
    SmbDeathCandidateReport {
        name: candidate.name(),
        control_true_frames: u64::try_from(control_true_frames).unwrap_or(u64::MAX),
        trip_frames,
        without_trip,
        median_trip_frame: passes.then_some(median).flatten(),
        max_trip_frame: passes.then_some(largest).flatten(),
        passes,
    }
}

/// Apply the registered adoption rule: the passing candidate that trips earliest.
fn adopt_death_candidate(candidates: &[SmbDeathCandidateReport]) -> Option<String> {
    candidates
        .iter()
        .filter(|candidate| candidate.passes)
        .min_by_key(|candidate| candidate.max_trip_frame.unwrap_or(u16::MAX))
        .map(|candidate| candidate.name.clone())
}

fn active_source_entries(source: &SmbArchiveReport) -> Vec<&SmbArchiveEntryReport> {
    let mut cells = BTreeMap::<SmbArchiveKey, Vec<&SmbArchiveEntryReport>>::new();
    for entry in &source.entries {
        let cell = cells.entry(entry.key).or_default();
        if cell.len() < MAX_ENTRIES_PER_KEY {
            cell.push(entry);
            continue;
        }
        if let Some((index, existing)) = cell
            .iter()
            .enumerate()
            .max_by_key(|(_, existing)| entry_cost(existing))
            && entry.input.actions.len() < existing.input.actions.len()
        {
            cell[index] = entry;
        }
    }
    cells.into_values().flatten().collect()
}

/// Run completion search with an explicit bounded completion-only action horizon.
pub fn run_smb_archive_search_with_action_limit(
    rom: &[u8],
    initial_inputs: &[SmbInput],
    seed: u64,
    execution_budget: u64,
    max_actions: usize,
) -> Result<SmbArchiveReport, Box<dyn Error>> {
    run_smb_archive_search_with_config(
        rom,
        initial_inputs,
        seed,
        execution_budget,
        max_actions,
        SmbArchiveDurationPolicy::Legacy,
    )
}

/// Run completion search with explicit bounded horizon and duration policy.
pub fn run_smb_archive_search_with_config(
    rom: &[u8],
    initial_inputs: &[SmbInput],
    seed: u64,
    execution_budget: u64,
    max_actions: usize,
    duration_policy: SmbArchiveDurationPolicy,
) -> Result<SmbArchiveReport, Box<dyn Error>> {
    run_smb_archive_search_with_config_and_suffix(
        rom,
        initial_inputs,
        seed,
        execution_budget,
        max_actions,
        duration_policy,
        SmbArchiveSuffixPolicy::OneOrTwo,
    )
}

/// Run frozen completion search with explicit bounded duration and suffix policies.
pub fn run_smb_archive_search_with_config_and_suffix(
    rom: &[u8],
    initial_inputs: &[SmbInput],
    seed: u64,
    execution_budget: u64,
    max_actions: usize,
    duration_policy: SmbArchiveDurationPolicy,
    suffix_policy: SmbArchiveSuffixPolicy,
) -> Result<SmbArchiveReport, Box<dyn Error>> {
    run_smb_archive_search_internal(
        rom,
        initial_inputs,
        seed,
        execution_budget,
        max_actions,
        duration_policy,
        suffix_policy,
        None,
        None,
        false,
    )
}

/// Run completion search with explicit bounded duration and suffix policies.
pub fn run_smb_archive_search_with_policies(
    rom: &[u8],
    initial_inputs: &[SmbInput],
    seed: u64,
    execution_budget: u64,
    max_actions: usize,
    duration_policy: SmbArchiveDurationPolicy,
    suffix_policy: SmbArchiveSuffixPolicy,
) -> Result<SmbArchiveReport, Box<dyn Error>> {
    run_smb_archive_search_internal(
        rom,
        initial_inputs,
        seed,
        execution_budget,
        max_actions,
        duration_policy,
        suffix_policy,
        None,
        None,
        true,
    )
}

/// Run completion search with a generated ranking confined to full-cell replacement.
pub fn run_smb_archive_search_with_ranking<R: SmbRanking>(
    rom: &[u8],
    initial_inputs: &[SmbInput],
    seed: u64,
    execution_budget: u64,
    config: SmbRankingSearchConfig,
    ranking: &R,
) -> Result<SmbArchiveReport, Box<dyn Error>> {
    run_smb_archive_search_internal(
        rom,
        initial_inputs,
        seed,
        execution_budget,
        config.max_actions,
        config.duration_policy,
        config.suffix_policy,
        Some(ranking),
        None,
        false,
    )
}

/// Run frozen completion search with one bounded generated semantic mutator choice.
pub fn run_smb_archive_search_with_generated_mutator<M: SmbMacro>(
    rom: &[u8],
    initial_inputs: &[SmbInput],
    seed: u64,
    execution_budget: u64,
    config: SmbRankingSearchConfig,
    generated_mutator: &M,
) -> Result<SmbArchiveReport, Box<dyn Error>> {
    run_smb_archive_search_internal(
        rom,
        initial_inputs,
        seed,
        execution_budget,
        config.max_actions,
        config.duration_policy,
        config.suffix_policy,
        None,
        Some(generated_mutator),
        false,
    )
}

fn record_generated_mutator_result(
    accounting: &mut SmbGeneratedMutatorAccounting,
    retained: bool,
    execution: u64,
) {
    if retained {
        accounting.retained_offspring = accounting.retained_offspring.saturating_add(1);
        accounting.consecutive_nonretained = 0;
    } else {
        accounting.consecutive_nonretained = accounting.consecutive_nonretained.saturating_add(1);
        if accounting.consecutive_nonretained >= GENERATED_MUTATOR_RETIRE_AFTER {
            accounting.active = false;
            accounting.retired_at_execution = Some(execution);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_smb_archive_search_internal(
    rom: &[u8],
    initial_inputs: &[SmbInput],
    seed: u64,
    execution_budget: u64,
    max_actions: usize,
    duration_policy: SmbArchiveDurationPolicy,
    suffix_policy: SmbArchiveSuffixPolicy,
    ranking: Option<&dyn SmbRanking>,
    generated_mutator: Option<&dyn SmbMacro>,
    experimental_search: bool,
) -> Result<SmbArchiveReport, Box<dyn Error>> {
    if initial_inputs.is_empty() {
        return Err("SMB archive search requires a nonempty initial corpus".into());
    }
    if !(1..=MAX_SMB_COMPLETION_ACTIONS).contains(&max_actions) {
        return Err("SMB completion action limit is outside its bounded range".into());
    }
    if initial_inputs
        .iter()
        .any(|input| input.actions.len() > max_actions)
    {
        return Err("SMB archive input exceeds the configured action limit".into());
    }
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    let mut archive = Archive::new(ranking, experimental_search);
    let mut aggregate = SmbMilestones::default();
    let mut progress_watermark = SmbProgressWatermark::default();
    let mut first_reached = SmbMilestoneTimes::default();
    let mut first_inputs = SmbMilestoneInputs::default();
    let mut champion_input = SmbInput::default();
    let mut champion_milestones = SmbMilestones::default();

    target.reset();
    let genesis_key = archive_key(target.wram());
    let genesis_snapshot = target.snapshot().ok_or("failed to snapshot SMB genesis")?;
    let genesis_id = archive
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
        .ok_or("failed to retain SMB genesis")?;
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
            merge_progress_watermark(&mut progress_watermark, target.last_action_observations());
            merge_action_milestones(&mut milestones, &target)?;
            merge_milestones(&mut aggregate, milestones);
            update_first_inputs(
                &mut first_reached,
                &mut first_inputs,
                milestones,
                0,
                &prefix,
            );
            if milestone_key(milestones) > milestone_key(champion_milestones) {
                champion_milestones = milestones;
                champion_input = prefix.clone();
            }
            if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                break;
            }
            let snapshot = target
                .snapshot()
                .ok_or("failed to snapshot SMB bootstrap prefix")?;
            if let Some(id) = archive.insert(
                Some(parent_id),
                0,
                ArchiveCandidate {
                    input: prefix.clone(),
                    key: archive_key(target.wram()),
                    milestones,
                },
                snapshot,
                target.last_action_observations(),
            )? {
                parent_id = id;
            }
        }
    }

    let mut rand = StdRand::with_seed(seed);
    let mut curve = Vec::new();
    let mut deaths = 0_u64;
    let mut generated_mutator_accounting = SmbGeneratedMutatorAccounting {
        installed: generated_mutator.is_some(),
        active: generated_mutator.is_some(),
        ..SmbGeneratedMutatorAccounting::default()
    };
    for execution in 1..=execution_budget {
        let parent_id = archive.choose_parent(&mut rand, max_actions)?;
        let parent = archive.entries[parent_id].clone();
        target.restore(&parent.snapshot)?;
        let mut input = parent.report.input.clone();
        let mut milestones = parent.report.milestones;
        let use_generated_mutator = generated_mutator_accounting.active
            && rand.below(NonZeroUsize::new(5).ok_or("invalid generated mutator odds")?) == 0;
        let (suffix, generated_emitted) = if use_generated_mutator {
            generated_mutator_accounting.attempts =
                generated_mutator_accounting.attempts.saturating_add(1);
            let candidate = generated_mutator
                .ok_or("active generated SMB mutator is missing")?
                .mutate(&input, rand.next());
            if candidate.actions.len() > max_actions
                || !candidate.actions.starts_with(&input.actions)
                || candidate
                    .actions
                    .iter()
                    .any(|action| action.hold_frames == 0 || action.hold_frames > MAX_HOLD_FRAMES)
            {
                return Err("generated SMB archive mutator violated deterministic bounds".into());
            }
            let emitted = candidate.actions.len() > input.actions.len();
            if emitted {
                generated_mutator_accounting.offspring =
                    generated_mutator_accounting.offspring.saturating_add(1);
            }
            (candidate.actions[input.actions.len()..].to_vec(), emitted)
        } else {
            let suffix_len = match suffix_policy {
                SmbArchiveSuffixPolicy::OneOrTwo => {
                    if rand.below(NonZeroUsize::new(4).ok_or("invalid suffix odds")?) == 0 {
                        2
                    } else {
                        1
                    }
                }
                SmbArchiveSuffixPolicy::BurstUpToFour => {
                    match rand.below(NonZeroUsize::new(8).ok_or("invalid burst odds")?) {
                        0 => 4,
                        1 => 3,
                        2 | 3 => 2,
                        _ => 1,
                    }
                }
            };
            let mut suffix = Vec::with_capacity(suffix_len);
            for _ in 0..suffix_len {
                suffix.push(sample_chord(
                    &mut rand,
                    duration_policy,
                    experimental_search,
                )?);
            }
            (suffix, false)
        };
        let mut current_parent = parent_id;
        let retained_before = archive.retained;
        for action in suffix {
            if target.is_dead() || input.actions.len() >= max_actions {
                break;
            }
            input.actions.push(action);
            target.apply(&action);
            merge_progress_watermark(&mut progress_watermark, target.last_action_observations());
            merge_action_milestones(&mut milestones, &target)?;
            merge_milestones(&mut aggregate, milestones);
            update_first_inputs(
                &mut first_reached,
                &mut first_inputs,
                milestones,
                execution,
                &input,
            );
            if milestone_key(milestones) > milestone_key(champion_milestones) {
                champion_milestones = milestones;
                champion_input = input.clone();
            }
            if target.is_dead() {
                deaths = deaths.saturating_add(1);
                break;
            }
            if target.exit_kind() != ExitKind::Ok {
                break;
            }
            let snapshot = target.snapshot().ok_or("failed to snapshot SMB suffix")?;
            if let Some(id) = archive.insert(
                Some(current_parent),
                execution,
                ArchiveCandidate {
                    input: input.clone(),
                    key: archive_key(target.wram()),
                    milestones,
                },
                snapshot,
                target.last_action_observations(),
            )? {
                current_parent = id;
            }
        }
        if generated_emitted {
            record_generated_mutator_result(
                &mut generated_mutator_accounting,
                archive.retained > retained_before,
                execution,
            );
        }
        archive.finish_execution(execution);
        if execution % 100 == 0 || execution == execution_budget {
            curve.push(SmbArchiveProgressPoint {
                executions: execution,
                milestones: aggregate,
                active_entries: archive.active.iter().filter(|active| **active).count(),
                occupied_cells: archive.cells.len(),
                deaths,
            });
        }
    }

    Ok(SmbArchiveReport {
        seed,
        executions: execution_budget,
        milestones: aggregate,
        progress_watermark,
        first_reached,
        first_inputs,
        champion_input,
        entries: archive
            .entries
            .into_iter()
            .map(|entry| entry.report)
            .collect(),
        progress_curve: curve,
        retained: archive.retained,
        rejected: archive.rejected,
        deaths,
        ranking: archive.ranking_accounting,
        generated_mutator: generated_mutator_accounting,
    })
}

fn merge_progress_watermark(
    watermark: &mut SmbProgressWatermark,
    observations: &[SmbObservations],
) {
    for observation in observations {
        let decoded = observation.decoded;
        *watermark = (*watermark).max(SmbProgressWatermark {
            world: decoded.world,
            level: decoded.level,
            progress: decoded.progress,
        });
    }
}

fn archive_key(wram: &[u8; 2_048]) -> SmbArchiveKey {
    let state = smb_mechanical_state_from_wram(wram);
    let digest = Sha256::digest(wram);
    SmbArchiveKey {
        world: state.world,
        level: state.level,
        progress: state.progress,
        player_y_bucket: state.player_y_bucket,
        player_engine_state: state.player_engine_state,
        state_fingerprint: digest[0] & STATE_FINGERPRINT_MASK,
    }
}

fn sample_chord(
    rand: &mut StdRand,
    duration_policy: SmbArchiveDurationPolicy,
    experimental_search: bool,
) -> Result<ButtonChord, Box<dyn Error>> {
    let masks: &[u8] = if experimental_search {
        const EXPERIMENTAL_BUTTON_MASKS: [u8; 14] = [
            0x00, 0x01, 0x02, 0x40, 0x80, 0x81, 0x82, 0x83, 0x10, 0x41, 0x42, 0xc0, 0xc1, 0xc2,
        ];
        &EXPERIMENTAL_BUTTON_MASKS
    } else {
        &FROZEN_BUTTON_MASKS
    };
    let buttons =
        masks[rand.below(NonZeroUsize::new(masks.len()).ok_or("empty SMB button vocabulary")?)];
    let hold_frames = match duration_policy {
        SmbArchiveDurationPolicy::Legacy => {
            if rand.below(NonZeroUsize::new(4).ok_or("invalid hold odds")?) != 0 {
                u8::try_from(
                    2 + rand.below(NonZeroUsize::new(11).ok_or("invalid short hold span")?),
                )?
            } else {
                u8::try_from(1 + rand.below(NonZeroUsize::new(120).ok_or("invalid hold span")?))?
            }
        }
        SmbArchiveDurationPolicy::Stratified => {
            if rand.below(NonZeroUsize::new(2).ok_or("invalid stratum odds")?) == 0 {
                u8::try_from(
                    2 + rand.below(NonZeroUsize::new(11).ok_or("invalid short hold span")?),
                )?
            } else {
                u8::try_from(
                    96 + rand.below(NonZeroUsize::new(25).ok_or("invalid long hold span")?),
                )?
            }
        }
    };
    Ok(ButtonChord::new(buttons, hold_frames))
}

fn merge_action_milestones(
    milestones: &mut SmbMilestones,
    target: &SmbTarget,
) -> Result<(), Box<dyn Error>> {
    for observation in target.last_action_observations() {
        let wram: &[u8; 2_048] = observation
            .wram
            .as_slice()
            .try_into()
            .map_err(|_| "SMB observation WRAM is not exactly 2 KiB")?;
        merge_milestones(milestones, smb_milestones_from_wram(wram));
    }
    Ok(())
}

fn merge_milestones(aggregate: &mut SmbMilestones, current: SmbMilestones) {
    aggregate.max_1_1_scroll_bucket = aggregate
        .max_1_1_scroll_bucket
        .max(current.max_1_1_scroll_bucket);
    aggregate.reached_1_1_flag |= current.reached_1_1_flag;
    aggregate.reached_1_2 |= current.reached_1_2;
    aggregate.reached_onward |= current.reached_onward;
}

fn update_first_inputs(
    times: &mut SmbMilestoneTimes,
    inputs: &mut SmbMilestoneInputs,
    current: SmbMilestones,
    execution: u64,
    input: &SmbInput,
) {
    if current.max_1_1_scroll_bucket > 0 {
        times.progress_into_1_1.get_or_insert(execution);
        inputs
            .progress_into_1_1
            .get_or_insert_with(|| input.clone());
    }
    if current.reached_1_1_flag {
        times.flag_1_1.get_or_insert(execution);
        inputs.flag_1_1.get_or_insert_with(|| input.clone());
    }
    if current.reached_1_2 {
        times.level_1_2.get_or_insert(execution);
        inputs.level_1_2.get_or_insert_with(|| input.clone());
    }
    if current.reached_onward {
        times.onward.get_or_insert(execution);
        inputs.onward.get_or_insert_with(|| input.clone());
    }
}

fn milestone_key(milestones: SmbMilestones) -> (bool, bool, bool, u16) {
    (
        milestones.reached_onward,
        milestones.reached_1_2,
        milestones.reached_1_1_flag,
        milestones.max_1_1_scroll_bucket,
    )
}

fn frontier_quality(entry: &SmbArchiveEntryReport) -> ((bool, bool, bool, u16), u8, u8, u16) {
    (
        milestone_key(entry.milestones),
        entry.key.world,
        entry.key.level,
        entry.key.progress,
    )
}

fn entry_cost(entry: &SmbArchiveEntryReport) -> (usize, u64) {
    (entry.input.actions.len(), entry.id)
}

#[cfg(test)]
mod tests {
    use super::{
        Archive, ContinuationRecording, EntryRecording, SmbArchiveDurationPolicy,
        SmbArchiveSuffixPolicy, SmbDeathBytes, SmbGeneratedMutatorAccounting, SmbProgressWatermark,
        SmbRanking, SmbRankingSearchConfig, analyze_player_column, merge_progress_watermark,
        record_generated_mutator_result, run_smb_archive_search,
        run_smb_archive_search_with_config_and_suffix,
        run_smb_archive_search_with_generated_mutator, run_smb_archive_search_with_ranking,
    };
    use crate::phase4b::{ButtonChord, MAX_SMB_ACTIONS, SmbInput, SmbMacro, SmbObservations};

    const SCREEN_COLUMN_INDEX: usize = 100;
    const ABSOLUTE_INDEX: usize = 200;
    const CONSTANT_INDEX: usize = 150;
    const NOISY_INDEX: usize = 160;
    const RISING_UNDER_LEFT_INDEX: usize = 170;
    const REPLICATED_INDICES: [usize; 4] = [300, 304, 308, 312];

    fn scripted_column(entry: usize, continuation: usize, frame: usize) -> i32 {
        let start = 40 + i32::try_from(entry).expect("entry index");
        let frame = i32::try_from(frame).expect("frame index");
        match continuation {
            1 => start + frame.min(60),
            2 => (start - frame).max(0),
            _ => start,
        }
    }

    fn scripted_camera(continuation: usize, frame: usize) -> u32 {
        let frame = u32::try_from(frame).expect("frame index");
        if continuation == 1 {
            2 * frame.saturating_sub(60)
        } else {
            0
        }
    }

    fn scripted_signatures(column: i32) -> [u64; 256] {
        let mut signatures = [0_u64; 256];
        for offset in 0..16 {
            let lit = usize::try_from(column + offset).expect("lit column");
            if lit < signatures.len() {
                signatures[lit] = 1;
            }
        }
        signatures
    }

    fn scripted_recording(entry: usize) -> EntryRecording {
        let mut continuations = Vec::new();
        for continuation in 0..3 {
            let mut recording = ContinuationRecording {
                wram: Vec::new(),
                columns: Vec::new(),
                camera: Vec::new(),
            };
            for frame in 0..=120 {
                let column = scripted_column(entry, continuation, frame);
                let camera = scripted_camera(continuation, frame);
                let mut wram = [0_u8; 2_048];
                let byte = u8::try_from(column).expect("column byte");
                wram[SCREEN_COLUMN_INDEX] = byte;
                for index in REPLICATED_INDICES {
                    wram[index] = byte;
                }
                wram[ABSOLUTE_INDEX] =
                    u8::try_from(column + i32::try_from(camera).expect("camera"))
                        .unwrap_or(u8::MAX);
                wram[CONSTANT_INDEX] = 7;
                wram[NOISY_INDEX] = if frame % 2 == 0 { 0 } else { 200 };
                wram[RISING_UNDER_LEFT_INDEX] = u8::try_from(frame).unwrap_or(u8::MAX);
                recording.wram.push(wram);
                recording.camera.push(camera);
                recording.columns.push(scripted_signatures(column));
            }
            continuations.push(recording);
        }
        EntryRecording {
            id: u64::try_from(entry).expect("entry id"),
            progress: if entry < 8 { 39 } else { 32 },
            frontier: entry < 8,
            continuations,
        }
    }

    #[test]
    fn player_column_audit_selects_the_screen_relative_byte() {
        let recordings = (0..16).map(scripted_recording).collect::<Vec<_>>();
        let (report, comparisons) = analyze_player_column(&recordings);
        assert!(!comparisons.is_empty());
        assert_eq!(report.qualifying_right_continuations, 16);
        let survivors = report.camera_relative_survivors.clone();
        assert!(survivors.contains(&u16::try_from(SCREEN_COLUMN_INDEX).expect("index")));
        assert!(!survivors.contains(&u16::try_from(ABSOLUTE_INDEX).expect("index")));
        assert!(!survivors.contains(&u16::try_from(CONSTANT_INDEX).expect("index")));
        assert!(!survivors.contains(&u16::try_from(NOISY_INDEX).expect("index")));
        assert!(!survivors.contains(&u16::try_from(RISING_UNDER_LEFT_INDEX).expect("index")));
        for index in REPLICATED_INDICES {
            let index = u16::try_from(index).expect("index");
            assert!(report.stride_rejected.contains(&index));
        }
        let selected = report.selected.expect("conclusive audit");
        assert_eq!(
            selected.index,
            u16::try_from(SCREEN_COLUMN_INDEX).expect("index")
        );
        assert_eq!(selected.offset, 0);
        assert!(selected.agreeing_comparisons >= 8);
    }

    #[test]
    fn player_column_audit_reports_nothing_without_a_camera_advance() {
        let mut recordings = (0..16).map(scripted_recording).collect::<Vec<_>>();
        for recording in &mut recordings {
            for camera in &mut recording.continuations[1].camera {
                *camera = 0;
            }
        }
        let (report, _) = analyze_player_column(&recordings);
        assert_eq!(report.qualifying_right_continuations, 0);
        assert!(report.camera_relative_survivors.is_empty());
        assert!(report.selected.is_none());
    }

    #[test]
    fn player_column_steerability_and_left_threshold_scale_with_the_audited_set() {
        let steerable = scripted_recording(0);
        assert!(super::player_column_is_steerable(&steerable));
        let mut frozen = scripted_recording(1);
        let right = frozen.continuations[1].wram.clone();
        frozen.continuations[2].wram = right;
        assert!(!super::player_column_is_steerable(&frozen));
        assert_eq!(super::player_column_left_threshold(16), 12);
        assert_eq!(super::player_column_left_threshold(8), 6);
        assert_eq!(super::player_column_left_threshold(9), 7);
        assert_eq!(super::player_column_left_threshold(4), 3);
    }

    #[test]
    fn player_column_audit_still_selects_with_a_smaller_audited_set() {
        let recordings = (0..8).map(scripted_recording).collect::<Vec<_>>();
        let (report, _) = analyze_player_column(&recordings);
        let selected = report.selected.expect("conclusive audit");
        assert_eq!(
            selected.index,
            u16::try_from(SCREEN_COLUMN_INDEX).expect("index")
        );
    }

    fn scripted_death_trace(id: u64, frames: &[(u8, u8, u8, u8)]) -> super::SmbDeathTrace {
        super::SmbDeathTrace {
            id,
            progress: 0,
            life_counter_below_genesis_at_endpoint: false,
            frames: frames
                .iter()
                .map(
                    |(engine_state, life_counter, vertical_page, vertical_low)| SmbDeathBytes {
                        engine_state: *engine_state,
                        life_counter: *life_counter,
                        vertical_page: *vertical_page,
                        vertical_low: *vertical_low,
                        ..SmbDeathBytes::default()
                    },
                )
                .collect(),
        }
    }

    #[test]
    fn death_candidates_read_only_the_bytes_they_are_named_for() {
        let falling = SmbDeathBytes {
            engine_state: 0x06,
            life_counter: 1,
            vertical_page: 3,
            vertical_low: 0x20,
            ..SmbDeathBytes::default()
        };
        assert!(!super::DeathCandidate::FrozenKill.holds(falling, 2));
        assert!(super::DeathCandidate::LifeCounterBelowStart.holds(falling, 2));
        assert!(!super::DeathCandidate::LifeCounterBelowStart.holds(falling, 1));
        assert!(super::DeathCandidate::VerticalPageAtLeast(3).holds(falling, 2));
        assert!(!super::DeathCandidate::VerticalPageAtLeast(4).holds(falling, 2));
        assert!(super::DeathCandidate::VerticalPositionAtLeast(3).holds(falling, 2));
        assert!(!super::DeathCandidate::VerticalPositionAtLeast(4).holds(falling, 2));
    }

    #[test]
    fn death_audit_rejects_a_candidate_that_is_true_during_live_play() {
        let control = vec![SmbDeathBytes {
            vertical_page: 1,
            ..SmbDeathBytes::default()
        }];
        let uncontrolled = (0..8)
            .map(|id| scripted_death_trace(id, &[(0x00, 2, 1, 0x00), (0x00, 2, 3, 0x00)]))
            .collect::<Vec<_>>();
        let report = super::evaluate_death_candidate(
            super::DeathCandidate::VerticalPageAtLeast(1),
            &control,
            &uncontrolled,
            2,
            true,
        );
        assert_eq!(report.control_true_frames, 1);
        assert!(!report.passes);
        let later = super::evaluate_death_candidate(
            super::DeathCandidate::VerticalPageAtLeast(3),
            &control,
            &uncontrolled,
            2,
            true,
        );
        assert_eq!(later.control_true_frames, 0);
        assert!(later.passes);
        assert_eq!(later.max_trip_frame, Some(1));
    }

    #[test]
    fn death_audit_requires_a_trip_on_every_uncontrolled_continuation() {
        let mut uncontrolled = (0..7)
            .map(|id| scripted_death_trace(id, &[(0x00, 2, 3, 0x00)]))
            .collect::<Vec<_>>();
        uncontrolled.push(scripted_death_trace(7, &[(0x00, 2, 1, 0x00)]));
        let report = super::evaluate_death_candidate(
            super::DeathCandidate::VerticalPageAtLeast(3),
            &[],
            &uncontrolled,
            2,
            true,
        );
        assert_eq!(report.without_trip, vec![7]);
        assert!(!report.passes);
        assert_eq!(report.max_trip_frame, None);
    }

    #[test]
    fn death_audit_adopts_the_passing_candidate_that_trips_earliest() {
        let candidates = vec![
            super::SmbDeathCandidateReport {
                name: "K0".to_owned(),
                control_true_frames: 0,
                trip_frames: vec![-1],
                without_trip: vec![0],
                median_trip_frame: None,
                max_trip_frame: None,
                passes: false,
            },
            super::SmbDeathCandidateReport {
                name: "K1".to_owned(),
                control_true_frames: 0,
                trip_frames: vec![90],
                without_trip: Vec::new(),
                median_trip_frame: Some(90),
                max_trip_frame: Some(90),
                passes: true,
            },
            super::SmbDeathCandidateReport {
                name: "K2(3)".to_owned(),
                control_true_frames: 0,
                trip_frames: vec![12],
                without_trip: Vec::new(),
                median_trip_frame: Some(12),
                max_trip_frame: Some(12),
                passes: true,
            },
        ];
        assert_eq!(
            super::adopt_death_candidate(&candidates),
            Some("K2(3)".to_owned())
        );
        assert_eq!(super::adopt_death_candidate(&candidates[..1]), None);
    }

    struct ScriptedFrameRanking;

    impl SmbRanking for ScriptedFrameRanking {
        fn score(&self, observations: &[SmbObservations]) -> i64 {
            observations.last().map_or(0, |observation| {
                i64::try_from(observation.frame_count).unwrap_or(i64::MAX)
            })
        }
    }

    struct ScriptedArchiveMutator;

    impl SmbMacro for ScriptedArchiveMutator {
        fn mutate(&self, input: &SmbInput, seed: u64) -> SmbInput {
            let mut candidate = input.clone();
            if candidate.actions.len() < MAX_SMB_ACTIONS {
                candidate
                    .actions
                    .push(ButtonChord::new(if seed & 1 == 0 { 0x81 } else { 0x82 }, 8));
            }
            candidate
        }
    }

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
    fn progress_watermark_uses_action_interiors() {
        let mut watermark = SmbProgressWatermark::default();
        let mut first = SmbObservations {
            frame_count: 1,
            wram: Vec::new(),
            decoded: Default::default(),
            milestones: Default::default(),
            changed_indices: Vec::new(),
            dead: false,
            log_line: String::new(),
        };
        first.decoded.world = 0;
        first.decoded.level = 2;
        first.decoded.progress = 41;
        let mut endpoint = first.clone();
        endpoint.frame_count = 2;
        endpoint.decoded.progress = 39;
        merge_progress_watermark(&mut watermark, &[first, endpoint]);
        assert_eq!(watermark.progress, 41);
    }

    #[test]
    fn same_seed_archive_reports_match_and_stay_bounded() {
        let rom = synthetic_nrom();
        let initial = vec![SmbInput::default()];
        let first = run_smb_archive_search(&rom, &initial, 0x5eed_e000, 32)
            .expect("first archive campaign");
        let second = run_smb_archive_search(&rom, &initial, 0x5eed_e000, 32)
            .expect("second archive campaign");
        assert_eq!(first, second);
        assert_eq!(first.executions, 32);
        assert!(
            first
                .entries
                .iter()
                .all(|entry| entry.input.actions.len() <= MAX_SMB_ACTIONS)
        );
        let burst_first = run_smb_archive_search_with_config_and_suffix(
            &rom,
            &initial,
            0x5eed_e000,
            32,
            MAX_SMB_ACTIONS,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::BurstUpToFour,
        )
        .expect("first frozen burst campaign");
        let burst_second = run_smb_archive_search_with_config_and_suffix(
            &rom,
            &initial,
            0x5eed_e000,
            32,
            MAX_SMB_ACTIONS,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::BurstUpToFour,
        )
        .expect("replayed frozen burst campaign");
        assert_eq!(burst_first, burst_second);
    }

    #[test]
    fn scripted_ranking_replays_and_retirement_uses_execution_rebuilds() {
        let rom = synthetic_nrom();
        let initial = vec![SmbInput::default()];
        let first = run_smb_archive_search_with_ranking(
            &rom,
            &initial,
            0x5eed_e000,
            32,
            SmbRankingSearchConfig {
                max_actions: MAX_SMB_ACTIONS,
                duration_policy: SmbArchiveDurationPolicy::Legacy,
                suffix_policy: SmbArchiveSuffixPolicy::OneOrTwo,
            },
            &ScriptedFrameRanking,
        )
        .expect("first ranked archive campaign");
        let second = run_smb_archive_search_with_ranking(
            &rom,
            &initial,
            0x5eed_e000,
            32,
            SmbRankingSearchConfig {
                max_actions: MAX_SMB_ACTIONS,
                duration_policy: SmbArchiveDurationPolicy::Legacy,
                suffix_policy: SmbArchiveSuffixPolicy::OneOrTwo,
            },
            &ScriptedFrameRanking,
        )
        .expect("replayed ranked archive campaign");
        assert_eq!(first, second);
        assert!(first.ranking.installed);
        let mut archive = Archive::new(Some(&ScriptedFrameRanking), false);
        archive.ranking_accounting.replacements = 1;
        archive.first_ranking_replacement = Some(1);
        archive.finish_execution(1_024);
        assert!(archive.ranking_accounting.active);
        archive.finish_execution(1_536);
        assert_eq!(archive.ranking_accounting.retired_at_execution, Some(1_536));
        assert!(!archive.ranking_accounting.active);
    }

    #[test]
    fn generated_mutator_retirement_counts_emitted_offspring_not_time() {
        let mut accounting = SmbGeneratedMutatorAccounting {
            installed: true,
            active: true,
            ..SmbGeneratedMutatorAccounting::default()
        };
        for execution in 1..128 {
            record_generated_mutator_result(&mut accounting, false, execution);
        }
        assert!(accounting.active);
        assert_eq!(accounting.consecutive_nonretained, 127);
        record_generated_mutator_result(&mut accounting, true, 128);
        assert!(accounting.active);
        assert_eq!(accounting.retained_offspring, 1);
        assert_eq!(accounting.consecutive_nonretained, 0);
        for execution in 129..=256 {
            record_generated_mutator_result(&mut accounting, false, execution);
        }
        assert!(!accounting.active);
        assert_eq!(accounting.retired_at_execution, Some(256));
    }

    #[test]
    fn scripted_generated_archive_mutator_replays_exactly() {
        let rom = synthetic_nrom();
        let initial = vec![SmbInput::default()];
        let config = SmbRankingSearchConfig {
            max_actions: MAX_SMB_ACTIONS,
            duration_policy: SmbArchiveDurationPolicy::Legacy,
            suffix_policy: SmbArchiveSuffixPolicy::OneOrTwo,
        };
        let first = run_smb_archive_search_with_generated_mutator(
            &rom,
            &initial,
            0x5eed_ef14,
            64,
            config,
            &ScriptedArchiveMutator,
        )
        .expect("first generated-mutator archive campaign");
        let replay = run_smb_archive_search_with_generated_mutator(
            &rom,
            &initial,
            0x5eed_ef14,
            64,
            config,
            &ScriptedArchiveMutator,
        )
        .expect("replayed generated-mutator archive campaign");
        assert_eq!(first, replay);
        assert!(first.generated_mutator.installed);
        assert!(first.generated_mutator.attempts > 0);
        assert!(first.generated_mutator.offspring > 0);
    }
}
