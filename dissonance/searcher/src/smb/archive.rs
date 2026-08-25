// SPDX-License-Identifier: AGPL-3.0-or-later

//! SMB snapshot archive: cell key, retention, and parent selection.

use std::{cmp::Reverse, collections::BTreeMap, error::Error, num::NonZeroUsize};

use crate::search::rand::RomuDuoJrRand;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    smb::target::{
        ButtonChord, SmbInput, SmbMilestoneInputs, SmbMilestoneTimes, SmbMilestones,
        SmbObservations, SmbProgressWatermark, SmbSnapshot, SmbTarget, smb_camera_pixels,
        smb_mechanical_state_from_wram, smb_milestones_from_wram,
    },
    target::Target,
};

/// Compiled ceiling on archive entries. A ceiling is not an allocation:
/// memory tracks actual retention (roughly 10–20 KB per entry with its
/// snapshot), and a whole-tree resume inherits the source population in
/// full. At the ceiling the archive rejects every admission, freezing the
/// search, so the ceiling must exceed a full campaign's retention — a
/// genesis-to-victory run retains a few entries per ten executions across
/// millions of executions. Campaign runs register their own per-run bound
/// at or below this.
pub const MAX_ARCHIVE_ENTRIES: usize = 4_194_304;
const MAX_ENTRIES_PER_KEY: usize = 2;
const FRONTIER_PROGRESS_BAND: u16 = 8;
const STATE_FINGERPRINT_MASK: u8 = 0x3f;

/// Largest bounded action horizon accepted by the completion-only archive.
/// A ceiling is not an allocation, and every campaign registers its own
/// explicit per-run action limit that replay retains and validates under.
pub const MAX_SMB_COMPLETION_ACTIONS: usize = 8192;

/// Identifier recorded for the archive key rule.
///
/// Rooms follow the area-span rule: a room opens when the area bytes change
/// or when a lineage moves past every room it knows, and a same-area
/// backward warp lands in the lineage's room of that area whose arrival page
/// is the greatest one not past the landing page. On top of the room, the
/// player's 16-pixel on-screen x bucket joins the key across the full frame
/// width, computed by [`screen_x_bucket`].
pub const KEY_POLICY_IDENTIFIER: &str = "frozen_area_span_screen_x_16";

/// Work RAM addresses whose byte pair identifies the current area (area type
/// and area data offset).
pub const ROOM_IDENTITY_BYTES: [usize; 2] = [0x074e, 0x074f];

/// One room identity: the area bytes at `ROOM_IDENTITY_BYTES` followed by
/// the level page the lineage arrived in that area at. The arrival page is
/// part of the identity because a warp can drop the player back into an area
/// already walked through; the game keeps no settled byte that says so, but
/// the screen never scrolls backward, so a child standing more than a page
/// behind its parent inside one level can only have arrived by warp. Looping
/// through the same warp arrives at the same page and adds no room.
pub type SmbRoomIdentity = [u8; 3];

/// Smallest backward progress step, in buckets, that only a warp can produce
/// within one level: one full screen plus one bucket.
const ROOM_ARRIVAL_SNAP: u16 = 17;

/// Which of a full cell's entries a better candidate displaces.
///
/// The archive key locates a state; it says nothing about what reaching that
/// state cost. Two routes to the same cell therefore collide, and the level
/// clock is denominated in frames, so the candidate displaces the cell's
/// costliest entry when it spent strictly fewer frames inside the current
/// level. Frames-in-level is derived from the recorded action durations and
/// the recorded level transitions alone: an entry whose parent shares its
/// pair carries the parent's count plus its own action's held frames, and an
/// entry whose parent sits in a different pair starts the count at its own
/// action.
pub const REPLACEMENT_IDENTIFIER: &str = "fewest_frames_in_level";

/// Identifier recorded for the parent selector: room uniform, band uniform,
/// cell uniform, then the recency-concentrated draw within the cell.
pub const SELECTOR_IDENTIFIER: &str = "room_cell_uniform_128";

/// Identifier recorded for the admission rule: the 45-frame probe under
/// three masks.
pub const RETENTION_IDENTIFIER: &str = "probe_at_admission_45";

/// Identifier recorded for the no-screening admission rule: an alive
/// endpoint is admitted under the normal cell rules and the probe never
/// runs.
pub const RETENTION_ADMIT_ALIVE_IDENTIFIER: &str = "admit_alive";

/// Per-run admission rule, recorded in the stream header; replay validates
/// under the recorded value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmbRetentionPolicy {
    /// Admit an alive endpoint only if one of three fixed input
    /// continuations survives the probe horizon from its snapshot.
    ProbeAtAdmission45,
    /// Admit every alive endpoint; the probe never runs on this path.
    AdmitAlive,
}

/// The recorded identifier of an admission rule.
#[must_use]
pub fn retention_policy_identifier(policy: SmbRetentionPolicy) -> &'static str {
    match policy {
        SmbRetentionPolicy::ProbeAtAdmission45 => RETENTION_IDENTIFIER,
        SmbRetentionPolicy::AdmitAlive => RETENTION_ADMIT_ALIVE_IDENTIFIER,
    }
}

/// The admission rule a recorded identifier names.
///
/// # Errors
///
/// Returns an error when the identifier names no compiled admission rule.
pub fn retention_policy_from_identifier(
    identifier: &str,
) -> Result<SmbRetentionPolicy, Box<dyn Error>> {
    match identifier {
        RETENTION_IDENTIFIER => Ok(SmbRetentionPolicy::ProbeAtAdmission45),
        RETENTION_ADMIT_ALIVE_IDENTIFIER => Ok(SmbRetentionPolicy::AdmitAlive),
        _ => Err(format!("retention policy {identifier} is not recognized").into()),
    }
}

/// Give-up thresholds for the retiring selector, one per pooling level:
/// consecutive barren draws at which the level's class is skipped in
/// selection exactly as exhausted classes are skipped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SmbRetireThresholds {
    /// One entry's own draws since its last retained descendant.
    pub entry: u64,
    /// Draws pooled over the entry's cell (its key without the fingerprint).
    pub cell: u64,
    /// Draws pooled over the entry's progress band within its room.
    pub band: u64,
    /// Draws pooled over the entry's room.
    pub room: u64,
}

/// Per-run parent selector, recorded in the stream header; replay validates
/// under the recorded value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmbSelectorPolicy {
    /// The compiled room, band, cell walk with the recency window.
    RoomCellUniform128,
    /// The same walk with barren classes retired at the given thresholds.
    /// Retirement is soft: entries stay serialized and replayable, and the
    /// deterministic all-exhausted reset also clears the barren counters,
    /// so the search can never seal itself out.
    Retire(SmbRetireThresholds),
}

/// The recorded identifier of a parent selector.
#[must_use]
pub fn selector_policy_identifier(policy: SmbSelectorPolicy) -> String {
    match policy {
        SmbSelectorPolicy::RoomCellUniform128 => SELECTOR_IDENTIFIER.to_owned(),
        SmbSelectorPolicy::Retire(thresholds) => format!(
            "{SELECTOR_IDENTIFIER}_retire:{},{},{},{}",
            thresholds.entry, thresholds.cell, thresholds.band, thresholds.room
        ),
    }
}

/// The parent selector a recorded identifier names.
///
/// # Errors
///
/// Returns an error when the identifier names no compiled selector or its
/// thresholds do not parse or contain a zero.
pub fn selector_policy_from_identifier(
    identifier: &str,
) -> Result<SmbSelectorPolicy, Box<dyn Error>> {
    if identifier == SELECTOR_IDENTIFIER {
        return Ok(SmbSelectorPolicy::RoomCellUniform128);
    }
    let prefix = format!("{SELECTOR_IDENTIFIER}_retire:");
    let Some(values) = identifier.strip_prefix(&prefix) else {
        return Err(format!("parent selector {identifier} is not recognized").into());
    };
    let parsed = values
        .split(',')
        .map(|value| value.parse::<u64>())
        .collect::<Result<Vec<_>, _>>()?;
    let [entry, cell, band, room] = parsed.as_slice() else {
        return Err("retiring selector needs exactly four thresholds".into());
    };
    if [entry, cell, band, room].iter().any(|value| **value == 0) {
        return Err("retiring selector thresholds must be nonzero".into());
    }
    Ok(SmbSelectorPolicy::Retire(SmbRetireThresholds {
        entry: *entry,
        cell: *cell,
        band: *band,
        room: *room,
    }))
}

/// Selections since the last retained descendant at which a parent is exhausted.
const SELECTION_EXHAUSTION_THRESHOLD: u64 = 64;

/// A concentrated cell draw samples only this many of the cell's
/// greatest-id members.
const CONCENTRATION_WINDOW: usize = 128;

/// Which selection path one recorded draw took.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SmbSelectorPath {
    /// The one-in-four uniform draw over all active entries.
    Uniform,
    /// One room of the deepest pair chosen uniformly, then one of its
    /// unexhausted progress bands uniformly, then one of the band's
    /// unexhausted cells uniformly, then the concentrated recency draw
    /// within it.
    RoomCellUniform,
}

/// One selector draw, recorded so selection-time state is checkable.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbSelectorDraw {
    /// Path this draw took.
    pub path: SmbSelectorPath,
    /// Fully exhausted bands skipped before this draw found its cell.
    pub classes_skipped: u64,
    /// Whether this draw found every active entry exhausted and reset the
    /// exhaustion counters.
    pub counter_reset: bool,
    /// Sampled-set state, present only on cell draws.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concentration: Option<SmbConcentrationDraw>,
}

/// Concentrated sampled-set state at one cell draw.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbConcentrationDraw {
    /// Members of the concentrated sampled set at this draw.
    pub window_size: u64,
    /// Sampled-set members at this draw that were never members before.
    pub entered_window: u64,
}

/// Per-campaign accounting for the selector.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbSelectorAccounting {
    /// Parent selections drawn through the uniform path.
    pub uniform_selections: u64,
    /// Parent selections drawn through the cell path.
    pub cell_selections: u64,
    /// Selections that produced at least one retained descendant.
    pub productive_selections: u64,
    /// Fully exhausted bands skipped across all draws.
    pub classes_skipped: u64,
    /// Deterministic all-exhausted counter resets.
    pub counter_resets: u64,
    /// Concentrated-window accounting.
    pub concentration: SmbConcentrationAccounting,
    /// Retirement accounting, present only under a retiring selector so
    /// reports recorded under the compiled selector keep their exact bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retirement: Option<SmbRetirementAccounting>,
}

/// Retirement state at report time under a retiring selector.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbRetirementAccounting {
    /// Entries whose own barren streak is at or over the entry threshold.
    pub entries_over_threshold: u64,
    /// Cells whose pooled barren streak is at or over the cell threshold.
    pub cells_over_threshold: u64,
    /// Bands whose pooled barren streak is at or over the band threshold.
    pub bands_over_threshold: u64,
    /// Rooms whose pooled barren streak is at or over the room threshold.
    pub rooms_over_threshold: u64,
}

/// Per-campaign accounting for the concentrated recency window.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbConcentrationAccounting {
    /// Fixed cap on the sampled set.
    pub window_cap: u64,
    /// Sampled-set size at the most recent cell draw.
    pub final_window_size: u64,
    /// Cell draws taken through the concentrated window.
    pub window_draws: u64,
    /// Distinct parents that were ever sampled-set members.
    pub distinct_window_parents: u64,
    /// Draws per parent through the window, in thousandths:
    /// `window_draws * 1000 / distinct_window_parents`, floored.
    pub draws_per_parent_milli: u64,
}

/// Per-entry selection counters.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbEntrySelectorCounters {
    /// Times this entry was selected as a parent.
    pub selected: u64,
    /// Selections of this entry that produced at least one retained descendant.
    pub productive: u64,
}

/// Fixed masks the admission probe tries, in order, stopping at the first survivor.
const VIABILITY_PROBE_MASKS: [u8; 3] = [0x00, 0x01, 0x81];
/// Admission-probe horizon in frames.
const VIABILITY_PROBE_FRAMES: u16 = 45;

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
    /// One-based 16-pixel screen-x bucket, present only for states inside
    /// the registered scroll-frozen room; zero and omitted everywhere else.
    #[serde(default, skip_serializing_if = "room_x_bucket_is_absent")]
    pub room_x_bucket: u8,
    /// The room the entry stands in, see [`SmbRoomIdentity`]; all zero and
    /// omitted for an entry inserted without a parent snapshot.
    #[serde(default, skip_serializing_if = "room_is_absent")]
    pub room: SmbRoomIdentity,
}

fn room_is_absent(room: &SmbRoomIdentity) -> bool {
    *room == SmbRoomIdentity::default()
}

fn room_x_bucket_is_absent(bucket: &u8) -> bool {
    *bucket == 0
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
    /// Selection counters, absent on an entry the search has not run over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<SmbEntrySelectorCounters>,
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
    /// First execution reaching each milestone rung.
    pub first_reached: SmbMilestoneTimes,
    /// First clean-reset input reaching each rung.
    pub first_inputs: SmbMilestoneInputs,
    /// Current best clean-reset input.
    pub champion_input: SmbInput,
    /// Insertion and replacement records for retained testcases.
    ///
    /// On disk each entry carries only the actions past its parent's input;
    /// the full inputs are rebuilt on load. Archives written with full inputs
    /// still load.
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<SmbArchiveEntryReport>,
    /// Fixed-interval deterministic progress curve.
    pub progress_curve: Vec<SmbArchiveProgressPoint>,
    /// Candidate snapshots admitted to the active archive.
    pub retained: u64,
    /// Candidate snapshots rejected by bounded quality-diversity retention.
    pub rejected: u64,
    /// Terminal death transitions observed.
    pub deaths: u64,
    /// Selector accounting.
    #[serde(default)]
    pub selector: SmbSelectorAccounting,
}

/// Serialized form of the entry list: every entry extends its parent, so the
/// actions past the parent's length identify the input once the parent is
/// rebuilt, at a small fraction of the size of the full input.
mod entries_by_suffix {
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

    use super::{SmbArchiveEntryReport, SmbArchiveKey, SmbEntrySelectorCounters};
    use crate::smb::target::{ButtonChord, SmbInput, SmbMilestones};

    #[derive(Deserialize, Serialize)]
    struct Wire {
        id: u64,
        parent_id: Option<u64>,
        created_execution: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<SmbInput>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_suffix: Option<Vec<ButtonChord>>,
        key: SmbArchiveKey,
        milestones: SmbMilestones,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<SmbEntrySelectorCounters>,
    }

    pub fn serialize<S: Serializer>(
        entries: &[SmbArchiveEntryReport],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let index_of: std::collections::BTreeMap<u64, usize> = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id, index))
            .collect();
        let wires: Vec<Wire> = entries
            .iter()
            .map(|entry| {
                let parent = entry
                    .parent_id
                    .and_then(|id| index_of.get(&id))
                    .map(|index| &entries[*index].input.actions)
                    .filter(|parent| entry.input.actions.starts_with(parent));
                let (input, input_suffix) = match parent {
                    Some(parent) => (None, Some(entry.input.actions[parent.len()..].to_vec())),
                    None => (Some(entry.input.clone()), None),
                };
                Wire {
                    id: entry.id,
                    parent_id: entry.parent_id,
                    created_execution: entry.created_execution,
                    input,
                    input_suffix,
                    key: entry.key,
                    milestones: entry.milestones,
                    selector: entry.selector,
                }
            })
            .collect();
        wires.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<SmbArchiveEntryReport>, D::Error> {
        let wires = Vec::<Wire>::deserialize(deserializer)?;
        let mut entries: Vec<SmbArchiveEntryReport> = Vec::with_capacity(wires.len());
        let mut index_of = std::collections::BTreeMap::<u64, usize>::new();
        for wire in wires {
            let input = match (wire.input, wire.input_suffix) {
                (Some(input), None) => input,
                (None, Some(suffix)) => {
                    let mut actions = match wire.parent_id.and_then(|id| index_of.get(&id)) {
                        Some(index) => entries[*index].input.actions.clone(),
                        None => {
                            return Err(D::Error::custom(format!(
                                "archive entry {} carries an input suffix without a loaded parent",
                                wire.id
                            )));
                        }
                    };
                    actions.extend(suffix);
                    SmbInput { actions }
                }
                _ => {
                    return Err(D::Error::custom(format!(
                        "archive entry {} must carry exactly one of input and input_suffix",
                        wire.id
                    )));
                }
            };
            index_of.insert(wire.id, entries.len());
            entries.push(SmbArchiveEntryReport {
                id: wire.id,
                parent_id: wire.parent_id,
                created_execution: wire.created_execution,
                input,
                key: wire.key,
                milestones: wire.milestones,
                selector: wire.selector,
            });
        }
        Ok(entries)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ArchiveEntry {
    pub(crate) report: SmbArchiveEntryReport,
    pub(crate) snapshot: SmbSnapshot,
}

pub(crate) struct ArchiveCandidate {
    pub(crate) input: SmbInput,
    pub(crate) key: SmbArchiveKey,
    pub(crate) milestones: SmbMilestones,
}

pub(crate) struct Archive {
    /// Retention stops when the entry count reaches this bound; campaign
    /// runs record their bound in the stream header and replay under it.
    pub(crate) max_entries: usize,
    pub(crate) entries: Vec<ArchiveEntry>,
    pub(crate) active: Vec<bool>,
    pub(crate) cells: BTreeMap<SmbArchiveKey, Vec<usize>>,
    pub(crate) input_ids: BTreeMap<SmbInput, usize>,
    pub(crate) retained: u64,
    pub(crate) rejected: u64,
    selected: Vec<u64>,
    productive: Vec<u64>,
    since_retained: Vec<u64>,
    in_window_ever: Vec<bool>,
    selector_accounting: SmbSelectorAccounting,
    /// Frames each retained entry spent inside its own pair, in entry-id
    /// order. Carried alongside the entries rather than in the serialized
    /// report.
    frames_in_level: Vec<u64>,
    replacement_frames_displaced: u64,
    /// Sorted distinct room identities per entry.
    room_sets: Vec<Vec<SmbRoomIdentity>>,
    /// The room each entry currently stands in, aligned with `room_sets`.
    current_rooms: Vec<SmbRoomIdentity>,
    /// Parent selector this archive selects under.
    pub(crate) selector_policy: SmbSelectorPolicy,
    /// Pooled barren streak per cell (key with the fingerprint zeroed).
    cell_barren: BTreeMap<SmbArchiveKey, u64>,
    /// Pooled barren streak per (pair, room, band).
    band_barren: BTreeMap<(u8, u8, SmbRoomIdentity, u16), u64>,
    /// Pooled barren streak per (pair, room).
    room_barren: BTreeMap<(u8, u8, SmbRoomIdentity), u64>,
}

/// A key's cell identity: the key with its fingerprint zeroed, so all
/// fingerprint siblings pool into one counter.
fn cell_identity(key: SmbArchiveKey) -> SmbArchiveKey {
    SmbArchiveKey {
        state_fingerprint: 0,
        ..key
    }
}

/// A key's (pair, room, band) identity; the pair scopes the counter because
/// two levels can reuse one area's bytes and arrival page.
fn band_identity(key: SmbArchiveKey) -> (u8, u8, SmbRoomIdentity, u16) {
    (
        key.world,
        key.level,
        key.room,
        key.progress / FRONTIER_PROGRESS_BAND,
    )
}

/// A key's (pair, room) identity.
fn room_identity(key: SmbArchiveKey) -> (u8, u8, SmbRoomIdentity) {
    (key.world, key.level, key.room)
}

impl Archive {
    /// Distinct room identities a retained entry's lineage visited inside
    /// its level, sorted.
    #[must_use]
    pub fn room_set(&self, id: usize) -> &[SmbRoomIdentity] {
        self.room_sets.get(id).map_or(&[], Vec::as_slice)
    }

    /// Cell collisions the frames-in-level rule decided, counted for the
    /// report.
    pub(crate) fn replacement_frames_displaced(&self) -> u64 {
        self.replacement_frames_displaced
    }

    /// Frames a retained entry spent inside its own pair.
    #[cfg(test)]
    pub(crate) fn entry_frames_in_level(&self, id: usize) -> u64 {
        self.frames_in_level[id]
    }

    /// Deepest recorded tuple, the fewest frames any entry there spent inside
    /// its pair, and the retained total.
    ///
    /// Read-only. Nothing here consumes randomness or mutates archive state, so
    /// calling it cannot change what a run records.
    pub(crate) fn live_progress(&self) -> (u8, u8, u16, u64, u64) {
        let deepest = self
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.report.key.world,
                    entry.report.key.level,
                    entry.report.key.progress,
                )
            })
            .max()
            .unwrap_or((0, 0, 0));
        let cheapest = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                (
                    entry.report.key.world,
                    entry.report.key.level,
                    entry.report.key.progress,
                ) == deepest
            })
            .map(|(index, _)| self.frames_in_level.get(index).copied().unwrap_or(0))
            .min()
            .unwrap_or(0);
        (deepest.0, deepest.1, deepest.2, cheapest, self.retained)
    }

    pub(crate) fn new() -> Self {
        Self {
            max_entries: MAX_ARCHIVE_ENTRIES,
            entries: Vec::new(),
            active: Vec::new(),
            cells: BTreeMap::new(),
            input_ids: BTreeMap::new(),
            retained: 0,
            rejected: 0,
            selected: Vec::new(),
            productive: Vec::new(),
            since_retained: Vec::new(),
            in_window_ever: Vec::new(),
            selector_accounting: SmbSelectorAccounting {
                concentration: SmbConcentrationAccounting {
                    window_cap: u64::try_from(CONCENTRATION_WINDOW).unwrap_or(u64::MAX),
                    ..SmbConcentrationAccounting::default()
                },
                ..SmbSelectorAccounting::default()
            },
            frames_in_level: Vec::new(),
            replacement_frames_displaced: 0,
            room_sets: Vec::new(),
            current_rooms: Vec::new(),
            selector_policy: SmbSelectorPolicy::RoomCellUniform128,
            cell_barren: BTreeMap::new(),
            band_barren: BTreeMap::new(),
            room_barren: BTreeMap::new(),
        }
    }

    /// Frames a candidate spent inside its own pair.
    ///
    /// An input extends its parent's, so the frames added since the parent are
    /// the held frames of the actions past the parent's length. A candidate
    /// whose parent already sits in the same pair inherits the parent's count;
    /// one whose parent sits elsewhere entered the pair during those actions
    /// and starts the count there. A candidate with no parent — genesis, and
    /// only genesis — counts its whole input.
    fn frames_in_level_of(
        &self,
        parent_id: Option<usize>,
        input: &SmbInput,
        key: SmbArchiveKey,
    ) -> u64 {
        let frames_of = |actions: &[ButtonChord]| -> u64 {
            actions
                .iter()
                .map(|action| u64::from(action.bounded_hold_frames()))
                .sum()
        };
        let Some(parent) = parent_id.and_then(|id| self.entries.get(id)) else {
            return frames_of(&input.actions);
        };
        let parent_actions = parent.report.input.actions.len();
        let added = frames_of(input.actions.get(parent_actions..).unwrap_or(&[]));
        let parent_key = parent.report.key;
        if (parent_key.world, parent_key.level) == (key.world, key.level) {
            self.frames_in_level
                .get(parent_id.unwrap_or_default())
                .copied()
                .unwrap_or(0)
                .saturating_add(added)
        } else {
            added
        }
    }

    pub(crate) fn insert(
        &mut self,
        parent_id: Option<usize>,
        execution: u64,
        candidate: ArchiveCandidate,
        snapshot: SmbSnapshot,
    ) -> Result<Option<usize>, Box<dyn Error>> {
        let ArchiveCandidate {
            input,
            mut key,
            milestones,
        } = candidate;
        if let Some(existing) = self.input_ids.get(&input) {
            return Ok(Some(*existing));
        }
        let (room_set, current_room) = self.room_set_for(parent_id, key, snapshot.wram())?;
        key.room = current_room;
        let candidate_frames_in_level = self.frames_in_level_of(parent_id, &input, key);
        let cell = self.cells.entry(key).or_default();
        // The costliest entry in the level's own currency loses to a
        // candidate that reached the same cell in strictly fewer frames. The
        // entry id breaks ties so the choice stays a total order over the
        // cell.
        let replace = if cell.len() < MAX_ENTRIES_PER_KEY {
            None
        } else {
            cell.iter()
                .copied()
                .max_by_key(|id| (self.frames_in_level[*id], self.entries[*id].report.id))
                .filter(|id| candidate_frames_in_level < self.frames_in_level[*id])
        };
        if cell.len() >= MAX_ENTRIES_PER_KEY && replace.is_none() {
            self.rejected = self.rejected.saturating_add(1);
            return Ok(None);
        }
        if self.entries.len() >= self.max_entries {
            self.rejected = self.rejected.saturating_add(1);
            return Ok(None);
        }
        if let Some(replaced) = replace {
            self.active[replaced] = false;
            cell.retain(|id| *id != replaced);
            self.replacement_frames_displaced = self.replacement_frames_displaced.saturating_add(1);
        }
        let id = self.entries.len();
        let report = SmbArchiveEntryReport {
            id: u64::try_from(id)?,
            parent_id: parent_id.map(u64::try_from).transpose()?,
            created_execution: execution,
            input: input.clone(),
            key,
            milestones,
            selector: None,
        };
        self.entries.push(ArchiveEntry { report, snapshot });
        self.active.push(true);
        self.room_sets.push(room_set);
        self.current_rooms.push(current_room);
        self.frames_in_level.push(candidate_frames_in_level);
        self.selected.push(0);
        self.productive.push(0);
        self.since_retained.push(0);
        self.in_window_ever.push(false);
        cell.push(id);
        self.input_ids.insert(input, id);
        self.retained = self.retained.saturating_add(1);
        Ok(Some(id))
    }

    /// The candidate's sorted room set and the room it stands in: the parent's
    /// set when the candidate stays in the parent's level, otherwise a fresh
    /// set, plus the candidate's room. An area change opens a room; a
    /// backward warp inside one area lands in the lineage's room of that area
    /// with the greatest arrival page not past the landing page, or opens a
    /// room at the landing page when there is none.
    fn room_set_for(
        &self,
        parent_id: Option<usize>,
        key: SmbArchiveKey,
        wram: &[u8],
    ) -> Result<(Vec<SmbRoomIdentity>, SmbRoomIdentity), Box<dyn Error>> {
        let mut area = [0_u8; 2];
        for (slot, offset) in area.iter_mut().zip(ROOM_IDENTITY_BYTES) {
            *slot = *wram
                .get(offset)
                .ok_or("room identity byte outside work RAM")?;
        }
        let arrival_page = u8::try_from(key.progress / 16)?;
        let arrived_here = [area[0], area[1], arrival_page];
        let parent = parent_id
            .map(|parent| {
                self.entries
                    .get(parent)
                    .map(|entry| (entry.report.key, self.current_rooms[parent]))
                    .ok_or("room set parent is missing")
            })
            .transpose()?;
        let (mut set, current) = match parent {
            Some((parent_key, parent_room))
                if (parent_key.world, parent_key.level) == (key.world, key.level) =>
            {
                let same_area = parent_room[..2] == area;
                let warped = parent_key.progress >= key.progress.saturating_add(ROOM_ARRIVAL_SNAP);
                let set = self.room_set(parent_id.unwrap_or_default()).to_vec();
                let current = if !same_area {
                    arrived_here
                } else if warped {
                    set.iter()
                        .copied()
                        .filter(|room| room[..2] == area && room[2] <= arrival_page)
                        .max_by_key(|room| room[2])
                        .unwrap_or(arrived_here)
                } else {
                    parent_room
                };
                (set, current)
            }
            _ => (Vec::new(), arrived_here),
        };
        if let Err(slot) = set.binary_search(&current) {
            set.insert(slot, current);
        }
        Ok((set, current))
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

    fn selector_unexhausted(&self, id: usize, ignore_streaks: bool) -> bool {
        if ignore_streaks {
            return true;
        }
        if self.since_retained[id] >= SELECTION_EXHAUSTION_THRESHOLD {
            return false;
        }
        match self.selector_policy {
            SmbSelectorPolicy::RoomCellUniform128 => true,
            SmbSelectorPolicy::Retire(thresholds) => {
                let key = self.entries[id].report.key;
                self.since_retained[id] < thresholds.entry
                    && self
                        .cell_barren
                        .get(&cell_identity(key))
                        .copied()
                        .unwrap_or(0)
                        < thresholds.cell
                    && self
                        .band_barren
                        .get(&band_identity(key))
                        .copied()
                        .unwrap_or(0)
                        < thresholds.band
                    && self
                        .room_barren
                        .get(&room_identity(key))
                        .copied()
                        .unwrap_or(0)
                        < thresholds.room
            }
        }
    }

    /// Choose a parent: one in four draws is uniform over every expandable
    /// entry; the rest pick a room of the deepest pair uniformly, then one
    /// of its unexhausted bands, then one of the band's unexhausted cells,
    /// then sample the cell's recency window. When every entry is exhausted
    /// the exhaustion counters reset once and the draw repeats.
    pub(crate) fn select_parent(
        &mut self,
        rand: &mut RomuDuoJrRand,
        max_actions: usize,
    ) -> Result<(usize, SmbSelectorDraw), Box<dyn Error>> {
        let active = self.active_ids(max_actions);
        if active.is_empty() {
            return Err("SMB archive has no expandable entry".into());
        }
        let use_frontier = rand.below(NonZeroUsize::new(4).ok_or("invalid frontier odds")?) != 0;
        if !use_frontier {
            let id = active[rand.below(NonZeroUsize::new(active.len()).ok_or("empty archive")?)];
            return Ok((
                id,
                SmbSelectorDraw {
                    path: SmbSelectorPath::Uniform,
                    classes_skipped: 0,
                    counter_reset: false,
                    concentration: None,
                },
            ));
        }
        let mut counter_reset = false;
        let mut classes_skipped = 0_u64;
        loop {
            if let Some(band) =
                self.room_band_uniform_class(rand, &active, &mut classes_skipped, counter_reset)?
            {
                let class = self.cell_uniform_class(rand, &band, counter_reset)?;
                let (id, concentration) = self.draw_from_class(rand, class)?;
                return Ok((
                    id,
                    SmbSelectorDraw {
                        path: SmbSelectorPath::RoomCellUniform,
                        classes_skipped,
                        counter_reset,
                        concentration: Some(concentration),
                    },
                ));
            }
            if counter_reset {
                return Err("selection counter reset freed no entry".into());
            }
            // The reset draw selects as if every streak counter were zero;
            // the durable clear happens when the reset-marked record is
            // applied, so counter state stays a pure function of the record
            // stream and live jobs still in flight at shutdown cannot leave
            // state replay never sees.
            counter_reset = true;
        }
    }

    /// The unexhausted members of every fixed-width progress band of
    /// `members`, deepest band first; exhausted bands are counted as skipped.
    fn unexhausted_bands(
        &self,
        members: &[usize],
        classes_skipped: &mut u64,
        ignore_streaks: bool,
    ) -> Vec<Vec<usize>> {
        let mut bands = BTreeMap::<Reverse<u16>, Vec<usize>>::new();
        for id in members {
            let band = self.entries[*id].report.key.progress / FRONTIER_PROGRESS_BAND;
            bands.entry(Reverse(band)).or_default().push(*id);
        }
        let mut live = Vec::new();
        for (_, band) in bands {
            let unexhausted = band
                .into_iter()
                .filter(|id| self.selector_unexhausted(*id, ignore_streaks))
                .collect::<Vec<_>>();
            if unexhausted.is_empty() {
                *classes_skipped = classes_skipped.saturating_add(1);
            } else {
                live.push(unexhausted);
            }
        }
        live
    }

    /// One unexhausted band of one room: the room chosen uniformly among the
    /// rooms of the deepest `(world, level)` pair with an unexhausted entry,
    /// the band uniformly among that room's unexhausted bands; `None` when
    /// every active entry is exhausted.
    fn room_band_uniform_class(
        &self,
        rand: &mut RomuDuoJrRand,
        active: &[usize],
        classes_skipped: &mut u64,
        ignore_streaks: bool,
    ) -> Result<Option<Vec<usize>>, Box<dyn Error>> {
        let mut pairs = BTreeMap::<(u8, u8), BTreeMap<SmbRoomIdentity, Vec<usize>>>::new();
        for id in active {
            let key = self.entries[*id].report.key;
            pairs
                .entry((key.world, key.level))
                .or_default()
                .entry(key.room)
                .or_default()
                .push(*id);
        }
        for (_, rooms) in pairs.into_iter().rev() {
            let mut live = Vec::new();
            for (_, members) in rooms {
                let mut skipped = 0_u64;
                let bands = self.unexhausted_bands(&members, &mut skipped, ignore_streaks);
                if bands.is_empty() {
                    *classes_skipped = classes_skipped.saturating_add(skipped);
                } else {
                    live.push((bands, skipped));
                }
            }
            let Some(count) = NonZeroUsize::new(live.len()) else {
                continue;
            };
            let (mut bands, skipped) = live.swap_remove(rand.below(count));
            *classes_skipped = classes_skipped.saturating_add(skipped);
            let band = bands
                .swap_remove(rand.below(NonZeroUsize::new(bands.len()).ok_or("empty band list")?));
            return Ok(Some(band));
        }
        Ok(None)
    }

    /// One cell of `members` chosen uniformly among the cells with an
    /// unexhausted member; a cell is the key without its state fingerprint.
    /// `members` must hold at least one unexhausted entry.
    fn cell_uniform_class(
        &self,
        rand: &mut RomuDuoJrRand,
        members: &[usize],
        ignore_streaks: bool,
    ) -> Result<Vec<usize>, Box<dyn Error>> {
        let mut cells = BTreeMap::<(u16, u8, u8, u8), Vec<usize>>::new();
        for id in members {
            if !self.selector_unexhausted(*id, ignore_streaks) {
                continue;
            }
            let key = self.entries[*id].report.key;
            cells
                .entry((
                    key.progress,
                    key.player_y_bucket,
                    key.player_engine_state,
                    key.room_x_bucket,
                ))
                .or_default()
                .push(*id);
        }
        let mut cells = cells.into_values().collect::<Vec<_>>();
        let count = NonZeroUsize::new(cells.len()).ok_or("cell draw over an exhausted band")?;
        Ok(cells.swap_remove(rand.below(count)))
    }

    /// Uniform draw within the chosen cell, narrowed to the cell's
    /// `CONCENTRATION_WINDOW` greatest-id members.
    ///
    /// Entry ids are creation order, so the greatest ids are the cell's most
    /// recently retained members. Membership is recomputed at every draw: a
    /// member leaves when `CONCENTRATION_WINDOW` newer sampleable cell
    /// members exist, or immediately when it exhausts.
    fn draw_from_class(
        &mut self,
        rand: &mut RomuDuoJrRand,
        mut class: Vec<usize>,
    ) -> Result<(usize, SmbConcentrationDraw), Box<dyn Error>> {
        class.sort_unstable();
        let window = &class[class.len().saturating_sub(CONCENTRATION_WINDOW)..];
        let mut entered_window = 0_u64;
        for id in window {
            if !self.in_window_ever[*id] {
                self.in_window_ever[*id] = true;
                entered_window = entered_window.saturating_add(1);
            }
        }
        let id = window[rand.below(NonZeroUsize::new(window.len()).ok_or("empty tie window")?)];
        Ok((
            id,
            SmbConcentrationDraw {
                window_size: u64::try_from(window.len())?,
                entered_window,
            },
        ))
    }

    /// Account one recorded selection of `id`.
    pub(crate) fn record_selection(&mut self, id: usize, draw: &SmbSelectorDraw) {
        // The reset-marked draw is the only place streak counters clear.
        // Applying it here, in stream order, keeps counter state a pure
        // function of the record stream, so live and replay agree at every
        // stream position. Retirement is soft: the reset also clears the
        // pooled barren counters, so the search can never seal itself out.
        if draw.counter_reset {
            for counter in &mut self.since_retained {
                *counter = 0;
            }
            self.cell_barren.clear();
            self.band_barren.clear();
            self.room_barren.clear();
        }
        self.selected[id] = self.selected[id].saturating_add(1);
        self.since_retained[id] = self.since_retained[id].saturating_add(1);
        if matches!(self.selector_policy, SmbSelectorPolicy::Retire(_)) {
            let key = self.entries[id].report.key;
            for counter in [
                self.cell_barren.entry(cell_identity(key)).or_insert(0),
                self.band_barren.entry(band_identity(key)).or_insert(0),
                self.room_barren.entry(room_identity(key)).or_insert(0),
            ] {
                *counter = counter.saturating_add(1);
            }
        }
        match draw.path {
            SmbSelectorPath::Uniform => {
                self.selector_accounting.uniform_selections = self
                    .selector_accounting
                    .uniform_selections
                    .saturating_add(1);
            }
            SmbSelectorPath::RoomCellUniform => {
                self.selector_accounting.cell_selections =
                    self.selector_accounting.cell_selections.saturating_add(1);
            }
        }
        self.selector_accounting.classes_skipped = self
            .selector_accounting
            .classes_skipped
            .saturating_add(draw.classes_skipped);
        self.selector_accounting.counter_resets = self
            .selector_accounting
            .counter_resets
            .saturating_add(u64::from(draw.counter_reset));
        if let Some(concentration) = draw.concentration.as_ref() {
            let accounting = &mut self.selector_accounting.concentration;
            accounting.window_draws = accounting.window_draws.saturating_add(1);
            accounting.final_window_size = concentration.window_size;
            accounting.distinct_window_parents = accounting
                .distinct_window_parents
                .saturating_add(concentration.entered_window);
            accounting.draws_per_parent_milli = accounting
                .window_draws
                .saturating_mul(1000)
                .checked_div(accounting.distinct_window_parents)
                .unwrap_or(0);
        }
    }

    /// Account one selection's discovery outcome.
    pub(crate) fn record_selection_outcome(&mut self, id: usize, retained_descendant: bool) {
        if !retained_descendant {
            return;
        }
        self.productive[id] = self.productive[id].saturating_add(1);
        self.since_retained[id] = 0;
        if matches!(self.selector_policy, SmbSelectorPolicy::Retire(_)) {
            let key = self.entries[id].report.key;
            self.cell_barren.insert(cell_identity(key), 0);
            self.band_barren.insert(band_identity(key), 0);
            self.room_barren.insert(room_identity(key), 0);
        }
        self.selector_accounting.productive_selections = self
            .selector_accounting
            .productive_selections
            .saturating_add(1);
    }

    /// The per-campaign selector accounting for the report.
    pub(crate) fn selector_report(&self) -> SmbSelectorAccounting {
        let mut accounting = self.selector_accounting;
        if let SmbSelectorPolicy::Retire(thresholds) = self.selector_policy {
            fn over<K>(map: &BTreeMap<K, u64>, threshold: u64) -> u64 {
                u64::try_from(map.values().filter(|streak| **streak >= threshold).count())
                    .unwrap_or(u64::MAX)
            }
            accounting.retirement = Some(SmbRetirementAccounting {
                entries_over_threshold: u64::try_from(
                    self.since_retained
                        .iter()
                        .zip(&self.active)
                        .filter(|(streak, active)| **active && **streak >= thresholds.entry)
                        .count(),
                )
                .unwrap_or(u64::MAX),
                cells_over_threshold: over(&self.cell_barren, thresholds.cell),
                bands_over_threshold: over(&self.band_barren, thresholds.band),
                rooms_over_threshold: over(&self.room_barren, thresholds.room),
            });
        }
        accounting
    }

    /// Clone the entry reports, stamping per-entry selection counters.
    pub(crate) fn entry_reports_snapshot(&self) -> Vec<SmbArchiveEntryReport> {
        self.entries
            .iter()
            .enumerate()
            .map(|(id, entry)| {
                let mut report = entry.report.clone();
                report.selector = Some(SmbEntrySelectorCounters {
                    selected: self.selected[id],
                    productive: self.productive[id],
                });
                report
            })
            .collect()
    }

    /// Extract the entry reports, stamping per-entry selection counters.
    pub(crate) fn take_entry_reports(&mut self) -> Vec<SmbArchiveEntryReport> {
        std::mem::take(&mut self.entries)
            .into_iter()
            .enumerate()
            .map(|(id, entry)| {
                let mut report = entry.report;
                report.selector = Some(SmbEntrySelectorCounters {
                    selected: self.selected[id],
                    productive: self.productive[id],
                });
                report
            })
            .collect()
    }
}

/// Probe whether a candidate stays alive for the admission horizon under any
/// of the fixed masks; the target is left restored to the candidate.
pub(crate) fn admission_is_viable(
    target: &mut SmbTarget,
    snapshot: &SmbSnapshot,
) -> Result<bool, Box<dyn Error>> {
    let mut viable = false;
    for mask in VIABILITY_PROBE_MASKS {
        target.restore(snapshot)?;
        if target.survives_probe(mask, VIABILITY_PROBE_FRAMES) {
            viable = true;
            break;
        }
    }
    target.restore(snapshot)?;
    Ok(viable)
}

pub(crate) fn merge_progress_watermark(
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

pub(crate) fn archive_key(wram: &[u8; 2_048]) -> SmbArchiveKey {
    let state = smb_mechanical_state_from_wram(wram);
    let digest = Sha256::digest(wram);
    SmbArchiveKey {
        world: state.world,
        level: state.level,
        progress: state.progress,
        player_y_bucket: state.player_y_bucket,
        player_engine_state: state.player_engine_state,
        state_fingerprint: digest[0] & STATE_FINGERPRINT_MASK,
        room_x_bucket: screen_x_bucket(wram),
        room: [0; 3],
    }
}

/// SMB player horizontal page byte, `$006d`, read by the room-x key term.
const PLAYER_ROOM_X_PAGE_OFFSET: usize = 0x006d;
/// SMB player horizontal position byte within the page, `$0086`.
const PLAYER_ROOM_X_LOW_OFFSET: usize = 0x0086;
/// The player's 16-pixel on-screen x bucket across the full frame width.
///
/// Wherever the camera is pinned — a scroll-locked room, a corridor, or
/// the clamped end of an area — the camera-derived progress coordinate
/// keeps one value while the player moves, and without this term every
/// position in the region shares one cell. The bucket covers the whole
/// width because pinned regions place their exits anywhere on screen,
/// including left of the camera's follow point. While the camera follows
/// the player it holds them near the follow point, so ordinary forward
/// play occupies few buckets and the term stays cheap where the camera
/// moves.
fn screen_x_bucket(wram: &[u8; 2_048]) -> u8 {
    let player_x = u32::from(wram[PLAYER_ROOM_X_PAGE_OFFSET]) * 256
        + u32::from(wram[PLAYER_ROOM_X_LOW_OFFSET]);
    let screen_x = player_x.saturating_sub(smb_camera_pixels(wram));
    u8::try_from(screen_x.min(255) / 16).unwrap_or(15)
}

/// The original controller vocabulary. Its masks were written in the SMB
/// disassembly's bit order, but the emulator reads the reverse order, so the
/// chords it actually presses are: no button, A, B, Left, Right, A+Right,
/// B+Right, A+B+Right, Up, Down — no leftward jump exists. It is kept only so
/// recordings made under its identifier keep replaying byte-exact.
pub const DOWN_TEN_BUTTON_MASKS: [u8; 10] =
    [0x00, 0x01, 0x02, 0x40, 0x80, 0x81, 0x82, 0x83, 0x10, 0x20];

/// The controller vocabulary in the emulator's bit order: no button, Right,
/// Left, B, A, A+Right, A+Left, A+Left+Right, Up, Down. Start and Select are
/// excluded because either pauses or leaves the game.
pub const NES_DOWN_TEN_BUTTON_MASKS: [u8; 10] =
    [0x00, 0x80, 0x40, 0x02, 0x01, 0x81, 0x41, 0xc1, 0x10, 0x20];

/// Draw one chord: a uniform mask and a hold from one of two strata, short
/// (2..=12 frames) for control and long (96..=120 frames) for time.
pub(crate) fn sample_chord_from_masks(
    rand: &mut RomuDuoJrRand,
    masks: &[u8],
) -> Result<ButtonChord, Box<dyn Error>> {
    let buttons =
        masks[rand.below(NonZeroUsize::new(masks.len()).ok_or("empty SMB button vocabulary")?)];
    let hold_frames = if rand.below(NonZeroUsize::new(2).ok_or("invalid stratum odds")?) == 0 {
        u8::try_from(2 + rand.below(NonZeroUsize::new(11).ok_or("invalid short hold span")?))?
    } else {
        u8::try_from(96 + rand.below(NonZeroUsize::new(25).ok_or("invalid long hold span")?))?
    };
    Ok(ButtonChord::new(buttons, hold_frames))
}

pub(crate) fn merge_action_milestones(
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

pub(crate) fn merge_milestones(aggregate: &mut SmbMilestones, current: SmbMilestones) {
    aggregate.max_1_1_scroll_bucket = aggregate
        .max_1_1_scroll_bucket
        .max(current.max_1_1_scroll_bucket);
    aggregate.reached_1_1_flag |= current.reached_1_1_flag;
    aggregate.reached_1_2 |= current.reached_1_2;
    aggregate.reached_onward |= current.reached_onward;
}

pub(crate) fn update_first_inputs(
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

pub(crate) fn milestone_key(milestones: SmbMilestones) -> (bool, bool, bool, u16) {
    (
        milestones.reached_onward,
        milestones.reached_1_2,
        milestones.reached_1_1_flag,
        milestones.max_1_1_scroll_bucket,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        Archive, ArchiveCandidate, MAX_SMB_COMPLETION_ACTIONS, ROOM_IDENTITY_BYTES,
        SELECTION_EXHAUSTION_THRESHOLD, SmbArchiveKey, SmbProgressWatermark, SmbSelectorDraw,
        SmbSelectorPath, merge_progress_watermark,
    };
    use crate::search::rand::RomuDuoJrRand;
    use crate::{
        smb::target::{ButtonChord, SmbInput, SmbObservations, SmbSnapshot, SmbTarget},
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

    fn selector_snapshot() -> SmbSnapshot {
        let rom = synthetic_nrom();
        let mut target =
            SmbTarget::from_smb_rom_bytes_headless(&rom).expect("load selector target");
        target.reset();
        target.snapshot().expect("snapshot selector genesis")
    }

    fn selector_archive(keys: &[(u8, u8, u16)]) -> Archive {
        let snapshot = selector_snapshot();
        let mut archive = Archive::new();
        for (index, (world, level, progress)) in keys.iter().enumerate() {
            let input = SmbInput {
                actions: vec![ButtonChord::new(
                    u8::try_from(index / 120).expect("chord mask"),
                    u8::try_from((index % 120) + 1).expect("hold frames"),
                )],
            };
            let key = SmbArchiveKey {
                world: *world,
                level: *level,
                progress: *progress,
                player_y_bucket: u8::try_from(index / 64).expect("vertical bucket"),
                player_engine_state: 0,
                state_fingerprint: u8::try_from(index % 64).expect("fingerprint"),
                room_x_bucket: 0,
                room: [0; 3],
            };
            archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        input,
                        key,
                        milestones: crate::smb::target::SmbMilestones::default(),
                    },
                    snapshot.clone(),
                )
                .expect("insert selector entry")
                .expect("retain selector entry");
        }
        archive
    }

    #[test]
    fn room_cell_uniform_splits_a_band_over_its_unexhausted_cells() {
        // One band of one 8-4 room: 60 entries at (304, y 11), 3 at
        // (303, y 7), 1 at (300, y 4). The band draw gives the crowded cell
        // nearly every draw; the cell rule gives each cell a third.
        let mut keys: Vec<(u8, u8, u16)> = Vec::new();
        keys.extend(std::iter::repeat_n((7, 3, 304), 60));
        keys.extend(std::iter::repeat_n((7, 3, 303), 3));
        keys.push((7, 3, 300));
        let mut archive = selector_archive(&keys);
        for (index, entry) in archive.entries.iter_mut().enumerate() {
            entry.report.key.room = [3, 5, 16];
            entry.report.key.player_y_bucket = match index {
                0..=59 => 11,
                60..=62 => 7,
                _ => 4,
            };
        }
        let mut rand = RomuDuoJrRand::with_seed(0xce11_0000);
        let mut per_cell = std::collections::BTreeMap::<(u16, u8), u64>::new();
        let mut cell_draws = 0_u64;
        for _ in 0..900 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("room-cell selection");
            if draw.path != SmbSelectorPath::RoomCellUniform {
                continue;
            }
            cell_draws += 1;
            let key = archive.entries[id].report.key;
            *per_cell
                .entry((key.progress, key.player_y_bucket))
                .or_default() += 1;
            archive.record_selection(id, &draw);
            archive.record_selection_outcome(id, true);
        }
        assert!(cell_draws > 600);
        assert_eq!(per_cell.len(), 3, "cells drawn: {per_cell:?}");
        for share in per_cell.values() {
            assert!(
                share * 3 > cell_draws / 2 && share * 3 < cell_draws * 3 / 2,
                "uneven cell shares: {per_cell:?}"
            );
        }
    }

    #[test]
    fn frozen_area_span_lands_same_area_warps_in_the_room_that_covers_the_page() {
        let mut archive = Archive::new();
        let genesis = selector_snapshot();
        let area_snapshot = |area: [u8; 2]| -> SmbSnapshot {
            let mut value = serde_json::to_value(&genesis).expect("serialize snapshot");
            let wram = value["observation"]["wram"]
                .as_array_mut()
                .expect("snapshot work RAM");
            for (offset, byte) in ROOM_IDENTITY_BYTES.into_iter().zip(area) {
                wram[offset] = serde_json::json!(byte);
            }
            serde_json::from_value(value).expect("rebuild snapshot")
        };
        let key = |progress: u16| SmbArchiveKey {
            world: 7,
            level: 3,
            progress,
            ..BASELINE_LIKE_KEY
        };
        let insert = |archive: &mut Archive, parent, actions: usize, key, area| {
            archive
                .insert(
                    parent,
                    0,
                    ArchiveCandidate {
                        input: SmbInput {
                            actions: vec![ButtonChord::new(1, 2); actions],
                        },
                        key,
                        milestones: crate::smb::target::SmbMilestones::default(),
                    },
                    area_snapshot(area),
                )
                .expect("insert")
        };
        let root = insert(&mut archive, None, 1, key(10), [3, 5]).expect("root");
        let deep = insert(&mut archive, Some(root), 2, key(230), [3, 5]).expect("deep");
        let water = insert(&mut archive, Some(deep), 3, key(20), [0, 2]).expect("water");
        let back = insert(&mut archive, Some(water), 4, key(260), [3, 5]).expect("back");
        assert_eq!(archive.entries[back].report.key.room, [3, 5, 16]);
        let tip = insert(&mut archive, Some(back), 5, key(304), [3, 5]).expect("tip");
        // The page-19 loop returns to page 16: the after-water room covers it.
        let looped = insert(&mut archive, Some(tip), 6, key(258), [3, 5]).expect("loop");
        assert_eq!(archive.entries[looped].report.key.room, [3, 5, 16]);
        // A pipe back to page 1 lands in the start room, which covers page 1.
        let restart = insert(&mut archive, Some(looped), 7, key(20), [3, 5]).expect("restart");
        assert_eq!(archive.entries[restart].report.key.room, [3, 5, 0]);
        assert_eq!(
            archive.room_set(restart),
            &[[0, 2, 1], [3, 5, 0], [3, 5, 16]]
        );
    }

    const BASELINE_LIKE_KEY: SmbArchiveKey = SmbArchiveKey {
        world: 7,
        level: 3,
        progress: 153,
        player_y_bucket: 11,
        player_engine_state: 8,
        state_fingerprint: 9,
        room_x_bucket: 0,
        room: [0; 3],
    };

    #[test]
    fn a_pooled_barren_band_is_retired_and_the_reset_frees_it() {
        // Two cells in one band at (1, 0) plus one band below. A single
        // barren draw of entry 0 puts the whole band over a threshold of
        // one, so cell draws must fall through to the lower band even
        // though entry 1 was never drawn.
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 144), (1, 0, 145), (1, 0, 124)];
        let mut archive = selector_archive(&keys);
        archive.selector_policy = super::SmbSelectorPolicy::Retire(super::SmbRetireThresholds {
            entry: 64,
            cell: 64,
            band: 1,
            room: 64,
        });
        let barren_draw = SmbSelectorDraw {
            path: SmbSelectorPath::RoomCellUniform,
            classes_skipped: 0,
            counter_reset: false,
            concentration: None,
        };
        archive.record_selection(0, &barren_draw);
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_5e30);
        let mut fell_through = 0;
        for _ in 0..64 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("selection");
            if draw.path == SmbSelectorPath::RoomCellUniform {
                assert_eq!(id, 2, "cell draws must fall through to the 124 band");
                assert_eq!(draw.classes_skipped, 1);
                assert!(!draw.counter_reset);
                fell_through += 1;
            }
        }
        assert!(fell_through > 0);
        // A retained descendant of the lower band's entry resets nothing in
        // the retired band; a retained descendant of entry 1 clears the
        // pooled counter and the band returns to selection.
        archive.record_selection(1, &barren_draw);
        archive.record_selection_outcome(1, true);
        let mut upper_band_seen = false;
        for _ in 0..64 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("selection after reset");
            if draw.path == SmbSelectorPath::RoomCellUniform && (id == 0 || id == 1) {
                upper_band_seen = true;
            }
        }
        assert!(
            upper_band_seen,
            "a keeper must return its band to selection"
        );
        let accounting = archive.selector_report();
        let retirement = accounting.retirement.expect("retirement accounting");
        assert_eq!(retirement.bands_over_threshold, 0);
    }

    #[test]
    fn a_retired_room_falls_to_the_reset_when_nothing_else_lives() {
        // One room only: a single barren draw retires it at a room
        // threshold of one, and the deterministic all-exhausted reset must
        // clear the pooled counters and free it rather than seal the search.
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 144), (1, 0, 124)];
        let mut archive = selector_archive(&keys);
        for entry in &mut archive.entries {
            entry.report.key.room = [3, 5, 0];
        }
        archive.selector_policy = super::SmbSelectorPolicy::Retire(super::SmbRetireThresholds {
            entry: 64,
            cell: 64,
            band: 64,
            room: 1,
        });
        let barren_draw = SmbSelectorDraw {
            path: SmbSelectorPath::RoomCellUniform,
            classes_skipped: 0,
            counter_reset: false,
            concentration: None,
        };
        archive.record_selection(0, &barren_draw);
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_5e31);
        let mut reset_seen = false;
        for _ in 0..64 {
            let (_, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("selection under a retired room");
            if draw.path == SmbSelectorPath::RoomCellUniform {
                if draw.counter_reset {
                    reset_seen = true;
                    break;
                }
                panic!("a cell draw before the reset must not reach a retired room");
            }
        }
        assert!(reset_seen, "the all-exhausted reset must free the room");
    }

    #[test]
    fn the_selector_starves_exhausted_parents_and_falls_through() {
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 144), (1, 0, 124), (1, 0, 123), (0, 0, 100)];
        let mut archive = selector_archive(&keys);
        let exhausting_draw = SmbSelectorDraw {
            path: SmbSelectorPath::RoomCellUniform,
            classes_skipped: 0,
            counter_reset: false,
            concentration: None,
        };
        for _ in 0..SELECTION_EXHAUSTION_THRESHOLD {
            archive.record_selection(0, &exhausting_draw);
        }
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_5e1f);
        let mut fell_through = 0;
        for _ in 0..64 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("selection");
            if draw.path == SmbSelectorPath::RoomCellUniform {
                fell_through += 1;
                assert!(
                    id == 1 || id == 2,
                    "cell draws must fall through to the 124 band"
                );
                assert_eq!(draw.classes_skipped, 1);
                assert!(!draw.counter_reset);
            }
        }
        assert!(fell_through > 0);
        assert_eq!(
            archive.selector_report().cell_selections,
            SELECTION_EXHAUSTION_THRESHOLD
        );
    }

    #[test]
    fn the_selector_resets_deterministically_when_all_are_exhausted() {
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 144), (0, 0, 100)];
        let mut archive = selector_archive(&keys);
        let exhausting_draw = SmbSelectorDraw {
            path: SmbSelectorPath::RoomCellUniform,
            classes_skipped: 0,
            counter_reset: false,
            concentration: None,
        };
        for id in 0..keys.len() {
            for _ in 0..SELECTION_EXHAUSTION_THRESHOLD {
                archive.record_selection(id, &exhausting_draw);
            }
        }
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_5e20);
        let mut reset_seen = false;
        for _ in 0..256 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("selection");
            if draw.path == SmbSelectorPath::RoomCellUniform {
                assert!(
                    draw.counter_reset,
                    "the first cell draw after full exhaustion must reset"
                );
                assert_eq!(draw.classes_skipped, 2);
                assert_eq!(id, 0);
                archive.record_selection(id, &draw);
                reset_seen = true;
                break;
            }
        }
        assert!(reset_seen);
        assert_eq!(archive.selector_report().counter_resets, 1);
    }

    #[test]
    fn the_cell_draw_samples_only_the_recency_window() {
        // 140 entries in one cell: the window is the 128 greatest ids.
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 124); 140];
        let mut archive = selector_archive(&keys);
        for entry in &mut archive.entries {
            entry.report.key.player_y_bucket = 0;
        }
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_5e21);
        let mut cell_draws = 0;
        for _ in 0..256 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("concentrated selection");
            match draw.path {
                SmbSelectorPath::RoomCellUniform => {
                    cell_draws += 1;
                    assert!(
                        id >= 12,
                        "cell draws must come from the 128 most recent members, got {id}"
                    );
                    let concentration = draw.concentration.expect("concentration record");
                    assert_eq!(concentration.window_size, 128);
                }
                SmbSelectorPath::Uniform => {
                    assert!(draw.concentration.is_none());
                }
            }
        }
        assert!(cell_draws > 0);
    }

    #[test]
    fn concentrated_window_slides_off_exhausted_members() {
        // 129 members at one progress: the window starts as ids 1..=128; when
        // all of them exhaust, the sampled set must refill from the
        // next-most-recent unexhausted member below, not skip the cell.
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 124); 129];
        let mut archive = selector_archive(&keys);
        for entry in &mut archive.entries {
            entry.report.key.player_y_bucket = 0;
        }
        let exhausting_draw = SmbSelectorDraw {
            path: SmbSelectorPath::RoomCellUniform,
            classes_skipped: 0,
            counter_reset: false,
            concentration: None,
        };
        for id in 1..=128 {
            for _ in 0..SELECTION_EXHAUSTION_THRESHOLD {
                archive.record_selection(id, &exhausting_draw);
            }
        }
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_5e22);
        let mut slid = false;
        for _ in 0..64 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("concentrated selection");
            if draw.path == SmbSelectorPath::RoomCellUniform {
                assert_eq!(id, 0, "the only unexhausted member must be sampled");
                assert_eq!(draw.classes_skipped, 0);
                assert!(!draw.counter_reset);
                let concentration = draw.concentration.expect("concentration record");
                assert_eq!(concentration.window_size, 1);
                slid = true;
            }
        }
        assert!(slid);
    }
    fn probe_key(world: u8, level: u8, progress: u16, vertical: u8) -> SmbArchiveKey {
        SmbArchiveKey {
            world,
            level,
            progress,
            player_y_bucket: vertical,
            player_engine_state: 0,
            state_fingerprint: 0,
            room_x_bucket: 0,
            room: [0; 3],
        }
    }

    /// Insert one action onto a parent and report the new entry's identifier.
    fn chain_insert(
        archive: &mut Archive,
        parent: Option<usize>,
        prefix: &SmbInput,
        buttons: u8,
        hold: u8,
        key: SmbArchiveKey,
        snapshot: &SmbSnapshot,
    ) -> (Option<usize>, SmbInput) {
        let mut input = prefix.clone();
        input.actions.push(ButtonChord::new(buttons, hold));
        let id = archive
            .insert(
                parent,
                0,
                ArchiveCandidate {
                    input: input.clone(),
                    key,
                    milestones: crate::smb::target::SmbMilestones::default(),
                },
                snapshot.clone(),
            )
            .expect("chained insert");
        (id, input)
    }

    #[test]
    fn frames_in_level_counts_from_the_recorded_pair_transition() {
        let snapshot = selector_snapshot();
        let mut archive = Archive::new();
        let genesis = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    input: SmbInput::default(),
                    key: probe_key(0, 0, 0, 0),
                    milestones: crate::smb::target::SmbMilestones::default(),
                },
                snapshot.clone(),
            )
            .expect("genesis insert")
            .expect("genesis retained");
        assert_eq!(archive.entry_frames_in_level(genesis), 0);
        // Two actions inside the genesis pair accumulate their held frames.
        let (first, input) = chain_insert(
            &mut archive,
            Some(genesis),
            &SmbInput::default(),
            0x01,
            30,
            probe_key(0, 0, 4, 0),
            &snapshot,
        );
        let first = first.expect("first retained");
        assert_eq!(archive.entry_frames_in_level(first), 30);
        let (second, input) = chain_insert(
            &mut archive,
            Some(first),
            &input,
            0x01,
            20,
            probe_key(0, 0, 8, 0),
            &snapshot,
        );
        let second = second.expect("second retained");
        assert_eq!(archive.entry_frames_in_level(second), 50);
        // Crossing into the next pair restarts the count at the crossing
        // action, and the next action inside the new pair adds to that.
        let (crossed, input) = chain_insert(
            &mut archive,
            Some(second),
            &input,
            0x01,
            40,
            probe_key(0, 1, 2, 0),
            &snapshot,
        );
        let crossed = crossed.expect("crossing retained");
        assert_eq!(archive.entry_frames_in_level(crossed), 40);
        let (after, _) = chain_insert(
            &mut archive,
            Some(crossed),
            &input,
            0x01,
            10,
            probe_key(0, 1, 6, 0),
            &snapshot,
        );
        assert_eq!(archive.entry_frames_in_level(after.expect("retained")), 50);
    }

    #[test]
    fn the_frames_rule_displaces_a_slower_route_into_a_full_cell() {
        let snapshot = selector_snapshot();
        let cell = probe_key(0, 0, 16, 0);
        // Three routes into one cell. The first two are short in actions and
        // long in frames; the third is longer in actions and much shorter in
        // frames, which is exactly the collision the level clock cares about.
        let mut archive = Archive::new();
        let genesis = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    input: SmbInput::default(),
                    key: probe_key(0, 0, 0, 0),
                    milestones: crate::smb::target::SmbMilestones::default(),
                },
                snapshot.clone(),
            )
            .expect("genesis insert")
            .expect("genesis retained");
        for buttons in [0x01_u8, 0x02] {
            chain_insert(
                &mut archive,
                Some(genesis),
                &SmbInput::default(),
                buttons,
                120,
                cell,
                &snapshot,
            );
        }
        let (fast, input) = chain_insert(
            &mut archive,
            Some(genesis),
            &SmbInput::default(),
            0x04,
            5,
            probe_key(0, 0, 8, 0),
            &snapshot,
        );
        let admitted = chain_insert(&mut archive, fast, &input, 0x04, 6, cell, &snapshot)
            .0
            .expect("the eleven-frame route displaces a slower one");
        assert_eq!(archive.entry_frames_in_level(admitted), 11);
        assert_eq!(archive.replacement_frames_displaced(), 1);
        let (slower, _) = chain_insert(&mut archive, fast, &input, 0x04, 200, cell, &snapshot);
        assert!(
            slower.is_none(),
            "a slower route never displaces a faster one"
        );
    }
}
