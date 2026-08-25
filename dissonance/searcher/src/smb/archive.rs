// SPDX-License-Identifier: AGPL-3.0-or-later

//! SMB archive key, room completion, and campaign report shapes.
//!
//! The archive mechanism itself is generic ([`crate::search::archive`]);
//! this module supplies the Super Mario Bros key, its group functions, room
//! completion against the lineage, and the memory decoders that build keys
//! from work RAM.

use std::{error::Error, num::NonZeroUsize};

use crate::search::archive::{
    Archive, ArchiveEntryReport, ArchiveKey, SelectorAccounting, SelectorPolicy, entries_by_suffix,
};

pub use crate::search::archive::MAX_ARCHIVE_ENTRIES;

/// The parent selector a recorded identifier names, resolved under the SMB
/// key's pooled depths.
///
/// # Errors
///
/// Returns an error for an unrecognized identifier.
pub fn selector_policy_from_identifier(identifier: &str) -> Result<SelectorPolicy, Box<dyn Error>> {
    crate::search::archive::selector_policy_from_identifier(identifier, SmbArchiveKey::groups() - 2)
}
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

/// The SMB archive instantiation of the generic snapshot archive.
pub type SmbArchive = Archive<ButtonChord, SmbArchiveKey, SmbMilestones, SmbSnapshot>;

/// The action-duration clock the replacement rule counts: held frames.
pub(crate) fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

/// Progress-band width in 16-pixel buckets for the band group depth.
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

/// Which of a full slot's entries a better candidate displaces.
///
/// The archive key locates a state; it says nothing about what reaching that
/// state cost. Two routes to the same slot therefore collide, and the level
/// clock is denominated in frames, so the candidate displaces the slot's
/// costliest entry when it spent strictly fewer frames inside the current
/// level. Frames-in-level is derived from the recorded action durations and
/// the recorded level transitions alone.
pub const REPLACEMENT_IDENTIFIER: &str = "fewest_frames_in_level";

/// Fixed masks the admission probe tries, in order, stopping at the first survivor.
const VIABILITY_PROBE_MASKS: [u8; 3] = [0x00, 0x01, 0x81];
/// Admission-probe horizon in frames.
const VIABILITY_PROBE_FRAMES: u16 = 45;

/// One bounded quality-diversity key for an action-boundary snapshot.
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
    /// The room the entry stands in, see [`SmbRoomIdentity`]. A freshly
    /// decoded key carries the arrival identity (area bytes plus the current
    /// page); completion against the parent's key and lineage resolves it.
    #[serde(default, skip_serializing_if = "room_is_absent")]
    pub room: SmbRoomIdentity,
}

fn room_is_absent(room: &SmbRoomIdentity) -> bool {
    *room == SmbRoomIdentity::default()
}

fn room_x_bucket_is_absent(bucket: &u8) -> bool {
    *bucket == 0
}

impl ArchiveKey for SmbArchiveKey {
    type Group = SmbArchiveKey;

    fn groups() -> usize {
        5
    }

    /// Depths, finest to coarsest: 0 the retention slot (the full key), 1
    /// the selection cell (fingerprint pooled), 2 the progress band within a
    /// room, 3 the room, 4 the `(world, level)` pair.
    fn group(self, depth: usize) -> Self::Group {
        let mut group = self;
        if depth >= 1 {
            group.state_fingerprint = 0;
        }
        if depth >= 2 {
            group.player_y_bucket = 0;
            group.player_engine_state = 0;
            group.room_x_bucket = 0;
            group.progress = self.progress / FRONTIER_PROGRESS_BAND;
        }
        if depth >= 3 {
            group.progress = 0;
        }
        if depth >= 4 {
            group.room = [0; 3];
        }
        group
    }

    type Lineage = Vec<SmbRoomIdentity>;

    /// Resolve the room: an area change opens a room; a backward warp inside
    /// one area lands in the lineage's room of that area with the greatest
    /// arrival page not past the landing page, or opens a room at the
    /// landing page when there is none; ordinary forward play keeps the
    /// parent's room.
    fn complete(self, parent: Option<(Self, &Self::Lineage)>) -> Self {
        let arrived_here = self.room;
        let room = match parent {
            Some((parent_key, rooms))
                if (parent_key.world, parent_key.level) == (self.world, self.level) =>
            {
                let parent_room = parent_key.room;
                let same_area = parent_room[..2] == arrived_here[..2];
                let warped = parent_key.progress >= self.progress.saturating_add(ROOM_ARRIVAL_SNAP);
                if !same_area {
                    arrived_here
                } else if warped {
                    rooms
                        .iter()
                        .copied()
                        .filter(|room| room[..2] == arrived_here[..2] && room[2] <= arrived_here[2])
                        .max_by_key(|room| room[2])
                        .unwrap_or(arrived_here)
                } else {
                    parent_room
                }
            }
            _ => arrived_here,
        };
        Self { room, ..self }
    }

    fn record(lineage: &mut Self::Lineage, key: Self) {
        if let Err(slot) = lineage.binary_search(&key.room) {
            lineage.insert(slot, key.room);
        }
    }
}

/// The SMB progress-curve point instantiation.
pub type SmbArchiveProgressPoint = crate::search::archive::ProgressPoint<SmbMilestones>;

/// The SMB entry report instantiation.
pub type SmbArchiveEntryReport = ArchiveEntryReport<ButtonChord, SmbArchiveKey, SmbMilestones>;

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
    pub selector: SelectorAccounting,
}

/// Probe whether a candidate stays alive for the admission horizon under any
/// of the fixed masks; the target is left restored to the candidate.
///
/// # Errors
///
/// Returns an error when a restore fails.
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

/// Decode the archive key from work RAM. Workers leave the room zero so
/// recorded result digests stay independent of room assignment; the
/// coordinator stamps the arrival identity with [`stamp_arrival_room`]
/// before insertion and [`ArchiveKey::complete`] resolves it against the
/// lineage.
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

/// Stamp the candidate's arrival identity into the key's room field: the
/// area bytes at [`ROOM_IDENTITY_BYTES`] and the arrival page.
///
/// # Errors
///
/// Returns an error when the work RAM is too short to hold the identity
/// bytes.
pub(crate) fn stamp_arrival_room(
    mut key: SmbArchiveKey,
    wram: &[u8],
) -> Result<SmbArchiveKey, Box<dyn Error>> {
    let mut area = [0_u8; 2];
    for (slot, offset) in area.iter_mut().zip(ROOM_IDENTITY_BYTES) {
        *slot = *wram
            .get(offset)
            .ok_or("room identity byte outside work RAM")?;
    }
    key.room = [area[0], area[1], u8::try_from(key.progress / 16)?];
    Ok(key)
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
/// position in the region shares one slot. The bucket covers the whole
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
    use super::{SmbArchiveKey, SmbRoomIdentity};
    use crate::search::archive::{Archive, ArchiveCandidate, ArchiveKey, Input};
    use crate::smb::target::{SmbObservations, SmbProgressWatermark};

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
        super::merge_progress_watermark(&mut watermark, &[first, endpoint]);
        assert_eq!(watermark.progress, 41);
    }

    fn key(progress: u16, area: [u8; 2]) -> SmbArchiveKey {
        SmbArchiveKey {
            world: 7,
            level: 3,
            progress,
            player_y_bucket: 11,
            player_engine_state: 8,
            state_fingerprint: 9,
            room_x_bucket: 0,
            room: [
                area[0],
                area[1],
                u8::try_from(progress / 16).expect("arrival page"),
            ],
        }
    }

    #[test]
    fn frozen_area_span_lands_same_area_warps_in_the_room_that_covers_the_page() {
        let mut archive: Archive<u8, SmbArchiveKey, (), ()> = Archive::new(|_| 1);
        let insert =
            |archive: &mut Archive<u8, SmbArchiveKey, (), ()>, parent, actions: usize, key| {
                archive
                    .insert(
                        parent,
                        0,
                        ArchiveCandidate {
                            input: Input {
                                actions: vec![1_u8; actions],
                            },
                            key,
                            milestones: (),
                        },
                        (),
                    )
                    .expect("insert")
            };
        let root = insert(&mut archive, None, 1, key(10, [3, 5])).expect("root");
        let deep = insert(&mut archive, Some(root), 2, key(230, [3, 5])).expect("deep");
        let water = insert(&mut archive, Some(deep), 3, key(20, [0, 2])).expect("water");
        let back = insert(&mut archive, Some(water), 4, key(260, [3, 5])).expect("back");
        assert_eq!(archive.entries[back].report.key.room, [3, 5, 16]);
        let tip = insert(&mut archive, Some(back), 5, key(304, [3, 5])).expect("tip");
        // The page-19 loop returns to page 16: the after-water room covers it.
        let looped = insert(&mut archive, Some(tip), 6, key(258, [3, 5])).expect("loop");
        assert_eq!(archive.entries[looped].report.key.room, [3, 5, 16]);
        // A pipe back to page 1 lands in the start room, which covers page 1.
        let restart = insert(&mut archive, Some(looped), 7, key(20, [3, 5])).expect("restart");
        assert_eq!(archive.entries[restart].report.key.room, [3, 5, 0]);
        let rooms: &Vec<SmbRoomIdentity> = archive.lineage(restart).expect("lineage");
        assert_eq!(rooms, &vec![[0, 2, 1], [3, 5, 0], [3, 5, 16]]);
    }

    #[test]
    fn groups_pool_from_slot_to_pair() {
        let key = key(153, [3, 5]);
        assert_eq!(key.group(0), key);
        assert_eq!(key.group(1).state_fingerprint, 0);
        assert_eq!(key.group(2).progress, 153 / 8);
        assert_eq!(key.group(2).player_y_bucket, 0);
        assert_eq!(key.group(3).progress, 0);
        assert_eq!(key.group(3).room, key.room);
        assert_eq!(key.group(4).room, [0; 3]);
        assert_eq!(
            (key.group(4).world, key.group(4).level),
            (key.world, key.level)
        );
        assert_eq!(SmbArchiveKey::groups(), 5);
    }
}
