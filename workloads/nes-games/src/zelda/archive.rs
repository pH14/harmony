// SPDX-License-Identifier: AGPL-3.0-or-later

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::{
    chord::{
        ChordVocabulary, LONG_HOLD_FRAMES, NES_PRESSABLE_BUTTON_MASKS, SHORT_HOLD_FRAMES, Tap,
    },
    search::archive::{
        Archive, ArchiveEntryReport, ArchiveKey, ProgressPoint, SelectorAccounting,
        entries_by_suffix,
    },
    zelda::target::{
        ButtonChord, GRID_SHIFT, ITEMS, SWORDS, TRIFORCE_PIECES, ZeldaInput, ZeldaMechanicalState,
        ZeldaObservations, ZeldaSnapshot,
    },
};

pub use crate::search::archive::MAX_ARCHIVE_ENTRIES;
pub const KEY_POLICY_IDENTIFIER: &str = "zelda_triforce_tier_stable_mode_level_room_grid_64_place_containers_health_rupees_preference_v2";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";

pub type ZeldaArchive = Archive<ButtonChord, ZeldaArchiveKey, ZeldaMilestones, ZeldaSnapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ZeldaTier {
    pub triforce: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ZeldaArchiveKey {
    pub tier: ZeldaTier,
    pub mode: u8,
    pub level: u8,
    pub room: u8,
    pub column: u8,
    pub row: u8,
    pub containers: u8,
    pub health: u16,
    pub rupees: u8,
}

impl ArchiveKey for ZeldaArchiveKey {
    type Place = (u8, u8, u8, u8, u8);
    type Progress = ZeldaTier;
    type Identity = ();

    fn place(self) -> Self::Place {
        (self.mode, self.level, self.room, self.column, self.row)
    }

    fn progress(self) -> Self::Progress {
        self.tier
    }

    fn identity(self) -> Self::Identity {}

    fn capacity() -> usize {
        1
    }

    fn preferences() -> usize {
        1
    }

    fn preference_cmp(self, _preference: usize, other: Self) -> Ordering {
        (self.containers, self.health, self.rupees).cmp(&(
            other.containers,
            other.health,
            other.rupees,
        ))
    }

    type Lineage = ();

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

#[must_use]
pub fn tier(state: ZeldaMechanicalState) -> ZeldaTier {
    ZeldaTier {
        triforce: state.triforce_pieces(),
    }
}

#[must_use]
pub fn archive_key(state: ZeldaMechanicalState) -> Option<ZeldaArchiveKey> {
    if state.terminal() || state.in_transition() {
        return None;
    }
    Some(ZeldaArchiveKey {
        tier: tier(state),
        mode: state.mode,
        level: state.level,
        room: state.room,
        column: state.x >> GRID_SHIFT,
        row: state.y >> GRID_SHIFT,
        containers: state.containers(),
        health: state.health(),
        rupees: state.rupees,
    })
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ZeldaMilestones {
    pub triforce: u8,
    pub sword: u8,
    pub items: u16,
    pub containers: u8,
    pub victory: bool,
    pub death: bool,
}

impl ZeldaMilestones {
    #[must_use]
    pub fn item_count(self) -> u32 {
        self.items.count_ones()
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ZeldaMilestoneTimes {
    pub first_triforce: [Option<u64>; TRIFORCE_PIECES as usize],
    pub first_swords: [Option<u64>; SWORDS.len()],
    pub first_items: [Option<u64>; ITEMS.len()],
    pub first_victory: Option<u64>,
    pub first_death: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ZeldaMilestoneInputs {
    pub first_triforce: [Option<ZeldaInput>; TRIFORCE_PIECES as usize],
    pub first_swords: [Option<ZeldaInput>; SWORDS.len()],
    pub first_items: [Option<ZeldaInput>; ITEMS.len()],
    pub first_victory: Option<ZeldaInput>,
    pub first_death: Option<ZeldaInput>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ZeldaProgressWatermark {
    pub triforce: u8,
    pub sword: u8,
    pub items: u8,
    pub containers: u8,
}

pub type ZeldaArchiveProgressPoint = ProgressPoint<ZeldaMilestones, ZeldaProgressWatermark>;
pub type ZeldaArchiveEntryReport =
    ArchiveEntryReport<ButtonChord, ZeldaArchiveKey, ZeldaMilestones>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ZeldaArchiveReport {
    pub seed: u64,
    pub executions: u64,
    pub milestones: ZeldaMilestones,
    pub progress_watermark: ZeldaProgressWatermark,
    pub first_reached: ZeldaMilestoneTimes,
    pub first_inputs: ZeldaMilestoneInputs,
    pub champion_input: ZeldaInput,
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<ZeldaArchiveEntryReport>,
    pub progress_curve: Vec<ZeldaArchiveProgressPoint>,
    pub retained: u64,
    pub rejected: u64,
    pub deaths: u64,
    #[serde(default)]
    pub selector: SelectorAccounting,
}

#[must_use]
pub fn milestones(state: ZeldaMechanicalState) -> ZeldaMilestones {
    ZeldaMilestones {
        triforce: state.triforce,
        sword: state.sword,
        items: state.items,
        containers: state.containers(),
        victory: state.victory(),
        death: state.dead(),
    }
}

#[must_use]
pub fn milestones_from_observation(observation: &ZeldaObservations) -> ZeldaMilestones {
    milestones(observation.decoded)
}

pub fn merge_milestones(into: &mut ZeldaMilestones, from: ZeldaMilestones) {
    into.triforce |= from.triforce;
    into.sword = into.sword.max(from.sword);
    into.items |= from.items;
    into.containers = into.containers.max(from.containers);
    into.victory |= from.victory;
    into.death |= from.death;
}

#[must_use]
pub fn milestone_key(value: ZeldaMilestones) -> (bool, u32, u8, u32, u8, bool) {
    (
        value.victory,
        value.triforce.count_ones(),
        value.sword,
        value.item_count(),
        value.containers,
        !value.death,
    )
}

#[must_use]
pub fn progress_point(state: ZeldaMechanicalState) -> ZeldaProgressWatermark {
    ZeldaProgressWatermark {
        triforce: state.triforce_pieces(),
        sword: state.sword,
        items: u8::try_from(state.items.count_ones()).unwrap_or(u8::MAX),
        containers: state.containers(),
    }
}

pub fn merge_progress_watermark(
    watermark: &mut ZeldaProgressWatermark,
    observations: &[ZeldaObservations],
) {
    for observation in observations {
        *watermark = (*watermark).max(progress_point(observation.decoded));
    }
}

pub fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

const START: u8 = 0x08;

pub const CHORDS: ChordVocabulary = ChordVocabulary {
    held: &NES_PRESSABLE_BUTTON_MASKS,
    tap: Some(Tap {
        buttons: START,
        odds: 12,
        hold_frames: (2, 7),
    }),
    short_hold: SHORT_HOLD_FRAMES,
    long_hold: LONG_HOLD_FRAMES,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::rand::RomuDuoJrRand;
    use crate::zelda::target::{MODE_DEATH, MODE_END_LEVEL, MODE_PLAY, START_ROOM};

    fn state(x: u8, y: u8) -> ZeldaMechanicalState {
        ZeldaMechanicalState {
            mode: MODE_PLAY,
            room: START_ROOM,
            x,
            y,
            hearts: 0x22,
            partial_heart: 0xff,
            ..ZeldaMechanicalState::default()
        }
    }

    #[test]
    fn the_place_is_the_room_and_a_64_pixel_grid_cell_of_link() {
        let here = archive_key(state(0x78, 0x8d)).unwrap();
        let near = archive_key(state(0x7f, 0xbf)).unwrap();
        let right = archive_key(state(0x80, 0x8d)).unwrap();
        let below = archive_key(state(0x78, 0xc0)).unwrap();
        assert_eq!(here.place(), near.place());
        assert_ne!(here.place(), right.place());
        assert_ne!(here.place(), below.place());
        let mut next_room = state(0x78, 0x8d);
        next_room.room = START_ROOM + 1;
        assert_ne!(here.place(), archive_key(next_room).unwrap().place());
        let mut dungeon = state(0x78, 0x8d);
        dungeon.level = 1;
        assert_ne!(here.place(), archive_key(dungeon).unwrap().place());
        let mut cave = state(0x78, 0x8d);
        cave.mode = 0x0b;
        assert_ne!(here.place(), archive_key(cave).unwrap().place());
    }

    #[test]
    fn the_tier_is_the_triforce_count_and_items_stay_out_of_the_key() {
        let plain = state(0x78, 0x8d);
        let mut armed = plain;
        armed.sword = 2;
        armed.items = 0b111;
        armed.bombs = 8;
        armed.keys = 3;
        let (plain_key, armed_key) = (archive_key(plain).unwrap(), archive_key(armed).unwrap());
        assert_eq!(plain_key, armed_key);
        let mut piece = plain;
        piece.triforce = 0b100;
        piece.hearts = 0x20;
        piece.partial_heart = 0x10;
        assert!(archive_key(piece).unwrap().progress() > plain_key.progress());
    }

    #[test]
    fn heart_containers_then_health_then_rupees_are_preferred() {
        let full = archive_key(state(0x78, 0x8d)).unwrap();
        let mut hurt = state(0x78, 0x8d);
        hurt.partial_heart = 0x7f;
        hurt.rupees = 200;
        let hurt = archive_key(hurt).unwrap();
        assert_eq!(full.place(), hurt.place());
        assert_eq!(full.preference_cmp(0, hurt), Ordering::Greater);
        let mut rich = state(0x78, 0x8d);
        rich.rupees = 5;
        assert_eq!(
            archive_key(rich).unwrap().preference_cmp(0, full),
            Ordering::Greater
        );
        let mut container = state(0x78, 0x8d);
        container.hearts = 0x30;
        container.partial_heart = 0x10;
        assert_eq!(
            archive_key(container).unwrap().preference_cmp(0, full),
            Ordering::Greater
        );
    }

    #[test]
    fn death_has_no_key_and_counts_as_a_death_milestone() {
        let mut dead = state(0x78, 0x8d);
        dead.mode = MODE_DEATH;
        assert_eq!(archive_key(dead), None);
        assert!(milestones(dead).death);
    }

    #[test]
    fn a_scroll_or_triforce_pickup_frame_has_no_key_but_keeps_its_milestones() {
        let mut scroll = state(0x78, 0x8d);
        scroll.mode = 0x07;
        assert_eq!(archive_key(scroll), None);
        let mut pickup = state(0x78, 0x8d);
        pickup.mode = MODE_END_LEVEL;
        pickup.level = 1;
        pickup.triforce = 0x01;
        assert_eq!(archive_key(pickup), None);
        assert_eq!(milestones(pickup).triforce, 0x01);
        let mut cave = state(0x78, 0x8d);
        cave.mode = 0x0b;
        assert_ne!(archive_key(cave), archive_key(state(0x78, 0x8d)));
    }

    #[test]
    fn milestones_keep_every_triforce_piece_item_and_the_best_sword() {
        let mut aggregate = ZeldaMilestones::default();
        let mut first = state(0x78, 0x8d);
        first.triforce = 0b1;
        first.sword = 2;
        first.items = 0b10;
        merge_milestones(&mut aggregate, milestones(first));
        let mut second = state(0x78, 0x8d);
        second.triforce = 0b100;
        second.sword = 1;
        second.items = 0b1;
        merge_milestones(&mut aggregate, milestones(second));
        assert_eq!(aggregate.triforce, 0b101);
        assert_eq!(aggregate.sword, 2);
        assert_eq!(aggregate.items, 0b11);
    }

    #[test]
    fn vocabulary_draws_pressable_chords_and_short_start_taps() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut previous = None;
        let mut starts = 0;
        for _ in 0..1_000 {
            let chord = CHORDS.draw(&mut rand, previous.as_ref()).unwrap();
            previous = Some(chord);
            assert_eq!(chord.buttons & 0x04, 0);
            assert!(NES_PRESSABLE_BUTTON_MASKS.contains(&(chord.buttons & !START)));
            if chord.buttons & START != 0 {
                starts += 1;
                assert!(chord.hold_frames <= 7);
            }
        }
        assert!((40..=140).contains(&starts), "start taps: {starts}");
    }
}
