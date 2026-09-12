// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, num::NonZeroUsize};

use crate::search::archive::{
    Archive, ArchiveEntryReport, ArchiveKey, SelectorAccounting, SelectorPolicy, entries_by_suffix,
};

pub use crate::search::archive::MAX_ARCHIVE_ENTRIES;
pub use crate::smb::target::ROOM_IDENTITY_BYTES;

pub fn selector_policy_from_identifier(identifier: &str) -> Result<SelectorPolicy, Box<dyn Error>> {
    crate::search::archive::selector_policy_from_identifier(
        identifier,
        SmbArchiveKey::groups().saturating_sub(2),
    )
}
use crate::search::rand::RomuDuoJrRand;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    nes_backend::{NesBackend, SnapshotState},
    smb::target::{
        ButtonChord, SmbInput, SmbMilestoneInputs, SmbMilestoneTimes, SmbMilestones,
        SmbObservations, SmbProgressWatermark, SmbSnapshot, SmbTarget, smb_camera_pixels,
        smb_mechanical_state_from_wram, smb_milestones_from_wram,
    },
    target::Target,
};

pub type SmbArchive<P = Vec<u8>> =
    Archive<ButtonChord, SmbArchiveKey, SmbMilestones, SmbSnapshot<P>>;

pub(crate) fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

const FRONTIER_PROGRESS_BAND: u16 = 4;
const STATE_FINGERPRINT_MASK: u8 = 0x3f;

pub const MAX_SMB_COMPLETION_ACTIONS: usize = 8192;

pub const KEY_POLICY_IDENTIFIER: &str =
    "frozen_area_span_screen_x_16_clock_100_band_4_player_x_cells_no_engine_state";

pub type SmbRoomIdentity = [u8; 3];

const ROOM_ARRIVAL_SNAP: u16 = 17;

pub const REPLACEMENT_IDENTIFIER: &str = "fewest_frames_in_level";

const VIABILITY_PROBE_MASKS: [u8; 3] = [0x00, 0x01, 0x81];
const VIABILITY_PROBE_FRAMES: u16 = 45;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SmbArchiveKey {
    pub world: u8,
    pub level: u8,
    pub progress: u16,
    pub player_y_bucket: u8,
    pub state_fingerprint: u8,
    #[serde(default, skip_serializing_if = "room_x_bucket_is_absent")]
    pub room_x_bucket: u8,
    #[serde(default)]
    pub time_bucket: u8,
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

    fn group(self, depth: usize) -> Self::Group {
        let mut group = self;
        if depth >= 1 {
            group.state_fingerprint = 0;
            group.progress = self.progress.saturating_add(u16::from(self.room_x_bucket));
            group.room_x_bucket = 0;
        }
        if depth >= 2 {
            group.player_y_bucket = 0;
            group.time_bucket = 0;
            group.progress /= FRONTIER_PROGRESS_BAND;
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

pub type SmbArchiveProgressPoint =
    crate::search::archive::ProgressPoint<SmbMilestones, SmbProgressWatermark>;

pub type SmbArchiveEntryReport = ArchiveEntryReport<ButtonChord, SmbArchiveKey, SmbMilestones>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbArchiveReport {
    pub seed: u64,
    pub executions: u64,
    pub milestones: SmbMilestones,
    #[serde(default)]
    pub progress_watermark: SmbProgressWatermark,
    pub first_reached: SmbMilestoneTimes,
    pub first_inputs: SmbMilestoneInputs,
    pub champion_input: SmbInput,
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<SmbArchiveEntryReport>,
    pub progress_curve: Vec<SmbArchiveProgressPoint>,
    pub retained: u64,
    pub rejected: u64,
    pub deaths: u64,
    #[serde(default)]
    pub selector: SelectorAccounting,
}

pub(crate) fn admission_is_viable<M, P>(
    target: &mut SmbTarget<M, P>,
    snapshot: &SmbSnapshot<P>,
) -> Result<bool, Box<dyn Error>>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
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
        state_fingerprint: digest[0] & STATE_FINGERPRINT_MASK,
        room_x_bucket: screen_x_bucket(wram),
        time_bucket: wram[GAME_TIMER_HUNDREDS_OFFSET],
        room: [0; 3],
    }
}

const GAME_TIMER_HUNDREDS_OFFSET: usize = 0x07f8;

pub(crate) fn stamp_arrival_room(
    key: SmbArchiveKey,
    wram: &[u8],
) -> Result<SmbArchiveKey, Box<dyn Error>> {
    let mut area = [0_u8; 2];
    for (slot, offset) in area.iter_mut().zip(ROOM_IDENTITY_BYTES) {
        *slot = *wram
            .get(offset)
            .ok_or("room identity byte outside work RAM")?;
    }
    stamp_arrival_room_identity(key, area)
}

pub(crate) fn stamp_arrival_room_identity(
    mut key: SmbArchiveKey,
    area: [u8; 2],
) -> Result<SmbArchiveKey, Box<dyn Error>> {
    key.room = [area[0], area[1], u8::try_from(key.progress / 16)?];
    Ok(key)
}

const PLAYER_ROOM_X_PAGE_OFFSET: usize = 0x006d;
const PLAYER_ROOM_X_LOW_OFFSET: usize = 0x0086;
fn screen_x_bucket(wram: &[u8; 2_048]) -> u8 {
    let player_x = u32::from(wram[PLAYER_ROOM_X_PAGE_OFFSET]) * 256
        + u32::from(wram[PLAYER_ROOM_X_LOW_OFFSET]);
    let screen_x = player_x.saturating_sub(smb_camera_pixels(wram));
    u8::try_from(screen_x.min(255) / 16).unwrap_or(15)
}

pub const DOWN_TEN_BUTTON_MASKS: [u8; 10] =
    [0x00, 0x01, 0x02, 0x40, 0x80, 0x81, 0x82, 0x83, 0x10, 0x20];

pub const NES_DOWN_TEN_BUTTON_MASKS: [u8; 10] =
    [0x00, 0x80, 0x40, 0x02, 0x01, 0x81, 0x41, 0xc1, 0x10, 0x20];

pub const NES_RUN_THIRTEEN_BUTTON_MASKS: [u8; 13] = [
    0x00, 0x80, 0x40, 0x02, 0x01, 0x81, 0x41, 0x82, 0x42, 0x83, 0x43, 0x10, 0x20,
];

pub const NES_PRESSABLE_BUTTON_MASKS: [u8; 36] = [
    0x00, 0x01, 0x02, 0x03, 0x80, 0x81, 0x82, 0x83, 0x40, 0x41, 0x42, 0x43, 0x10, 0x11, 0x12, 0x13,
    0x20, 0x21, 0x22, 0x23, 0x90, 0x91, 0x92, 0x93, 0xa0, 0xa1, 0xa2, 0xa3, 0x50, 0x51, 0x52, 0x53,
    0x60, 0x61, 0x62, 0x63,
];

pub const SHORT_HOLD_FRAMES: (u8, u8) = (2, 12);
pub const LONG_HOLD_FRAMES: (u8, u8) = (96, 120);

pub(crate) fn sample_chord_from_masks(
    rand: &mut RomuDuoJrRand,
    masks: &[u8],
) -> Result<ButtonChord, Box<dyn Error>> {
    let buttons =
        masks[rand.below(NonZeroUsize::new(masks.len()).ok_or("empty SMB button vocabulary")?)];
    let (low, high) = if rand.below(NonZeroUsize::new(2).ok_or("invalid stratum odds")?) == 0 {
        SHORT_HOLD_FRAMES
    } else {
        LONG_HOLD_FRAMES
    };
    let span = NonZeroUsize::new(usize::from(high - low) + 1).ok_or("invalid hold span")?;
    let hold_frames = u8::try_from(usize::from(low) + rand.below(span))?;
    Ok(ButtonChord::new(buttons, hold_frames))
}

pub(crate) fn merge_action_milestones<M, P>(
    milestones: &mut SmbMilestones,
    target: &SmbTarget<M, P>,
) -> Result<(), Box<dyn Error>>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
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
    use crate::search::archive::{Archive, ArchiveCandidate, ArchiveKey};
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
            state_fingerprint: 9,
            room_x_bucket: 0,
            time_bucket: 0,
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
        let insert = |archive: &mut Archive<u8, SmbArchiveKey, (), ()>,
                      parent: Option<usize>,
                      actions: usize,
                      key| {
            let suffix_len = parent.map_or(actions, |_| 1);
            archive
                .insert(
                    parent,
                    0,
                    ArchiveCandidate {
                        suffix: vec![1_u8; suffix_len],
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
        assert_eq!(archive.entry_key(back).expect("back key").room, [3, 5, 16]);
        let tip = insert(&mut archive, Some(back), 5, key(304, [3, 5])).expect("tip");
        let looped = insert(&mut archive, Some(tip), 6, key(258, [3, 5])).expect("loop");
        assert_eq!(
            archive.entry_key(looped).expect("loop key").room,
            [3, 5, 16]
        );
        let restart = insert(&mut archive, Some(looped), 7, key(20, [3, 5])).expect("restart");
        assert_eq!(
            archive.entry_key(restart).expect("restart key").room,
            [3, 5, 0]
        );
        let rooms: &Vec<SmbRoomIdentity> = archive.lineage(restart).expect("lineage");
        assert_eq!(rooms, &vec![[0, 2, 1], [3, 5, 0], [3, 5, 16]]);
    }

    #[test]
    fn a_rejected_boundary_carries_its_room_to_the_next_boundary() {
        let mut archive: Archive<u8, SmbArchiveKey, (), ()> = Archive::new(|_| 1);
        let insert = |archive: &mut Archive<u8, SmbArchiveKey, (), ()>,
                      parent: Option<usize>,
                      previous: Option<SmbArchiveKey>,
                      actions: usize,
                      key| {
            archive
                .insert_after(
                    parent,
                    previous,
                    0,
                    ArchiveCandidate {
                        suffix: vec![1_u8; actions],
                        key,
                        milestones: (),
                    },
                    (),
                )
                .expect("insert")
        };
        let (land, _) = insert(&mut archive, None, None, 1, key(10, [3, 5]));
        let land = land.expect("land");
        let (first, _) = insert(&mut archive, Some(land), None, 1, key(3, [0, 2]));
        let first = first.expect("first");
        let (second, _) = insert(&mut archive, Some(land), None, 2, key(3, [0, 2]));
        second.expect("second");
        assert_eq!(archive.entry_key(first).expect("first key").room, [0, 2, 0]);
        let (rejected, at_page_0) = insert(&mut archive, Some(land), None, 3, key(3, [0, 2]));
        assert!(rejected.is_none());
        assert_eq!(at_page_0.room, [0, 2, 0]);
        let (deep, key_deep) = insert(
            &mut archive,
            Some(land),
            Some(at_page_0),
            4,
            key(50, [0, 2]),
        );
        let deep = deep.expect("deep");
        assert_eq!(key_deep.room, [0, 2, 0]);
        assert_eq!(archive.entry_key(deep).expect("deep key").room, [0, 2, 0]);
        let (fresh, _) = insert(&mut archive, Some(land), None, 5, key(50, [0, 2]));
        assert_eq!(
            archive
                .entry_key(fresh.expect("fresh"))
                .expect("fresh key")
                .room,
            [0, 2, 3]
        );
    }

    #[test]
    fn groups_pool_from_slot_to_pair() {
        let key = key(153, [3, 5]);
        assert_eq!(key.group(0), key);
        assert_eq!(key.group(1).state_fingerprint, 0);
        assert_eq!(key.group(2).progress, 153 / 4);
        let on_screen = SmbArchiveKey {
            room_x_bucket: 6,
            ..key
        };
        assert_eq!(on_screen.group(1).progress, 159);
        assert_eq!(on_screen.group(1).room_x_bucket, 0);
        assert_eq!(on_screen.group(2).progress, 159 / 4);
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
