// SPDX-License-Identifier: AGPL-3.0-or-later

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::{
    castlevania::target::{
        ButtonChord, CastlevaniaInput, CastlevaniaMechanicalState, CastlevaniaObservations,
        CastlevaniaSnapshot, FINAL_STAGE,
    },
    chord::{ChordVocabulary, SHORT_HOLD_FRAMES},
    search::archive::{
        Archive, ArchiveEntryReport, ArchiveKey, ProgressPoint, SelectorAccounting,
        entries_by_suffix,
    },
};

pub use crate::search::archive::MAX_ARCHIVE_ENTRIES;
pub const KEY_POLICY_IDENTIFIER: &str = "castlevania_stage_boss_whip_subweapon_hearts_tier_section_spatial_32_boss_damage_health_preference_v1";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";
const PLAY_MODE_MARK: u8 = 0;
const BOSS_DAMAGE_STEP: u8 = 8;

pub type CastlevaniaArchive =
    Archive<ButtonChord, CastlevaniaArchiveKey, CastlevaniaMilestones, CastlevaniaSnapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CastlevaniaTier {
    pub stage: u8,
    pub boss_down: bool,
    pub whip: u8,
    pub subweapon: bool,
    pub hearts: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CastlevaniaArchiveKey {
    pub tier: CastlevaniaTier,
    pub section: u8,
    pub mode: u8,
    pub boss_damage: u8,
    pub x: u16,
    pub y: u8,
    pub health: u8,
    pub hearts: u8,
    pub lives: u8,
}

impl Ord for CastlevaniaArchiveKey {
    fn cmp(&self, other: &Self) -> Ordering {
        (
            self.tier,
            self.section,
            self.boss_damage,
            self.x,
            self.health,
            self.mode,
            self.y,
            self.hearts,
            self.lives,
        )
            .cmp(&(
                other.tier,
                other.section,
                other.boss_damage,
                other.x,
                other.health,
                other.mode,
                other.y,
                other.hearts,
                other.lives,
            ))
    }
}

impl PartialOrd for CastlevaniaArchiveKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl ArchiveKey for CastlevaniaArchiveKey {
    type Place = (u8, u8, u8, u16, u8);
    type Progress = CastlevaniaTier;
    type Identity = (u8, u8, u8, u16, u8);

    fn place(self) -> Self::Place {
        (
            self.section,
            self.mode,
            self.boss_damage,
            self.x.div_euclid(2),
            self.y.div_euclid(2),
        )
    }

    fn progress(self) -> Self::Progress {
        self.tier
    }

    fn identity(self) -> Self::Identity {
        (self.section, self.mode, self.boss_damage, self.x, self.y)
    }

    fn capacity() -> usize {
        1
    }

    fn preferences() -> usize {
        1
    }

    fn preference_cmp(self, _preference: usize, other: Self) -> Ordering {
        (self.health, self.hearts, self.lives).cmp(&(other.health, other.hearts, other.lives))
    }

    type Lineage = ();

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CastlevaniaChampionKey {
    tier: CastlevaniaTier,
    section: u8,
    boss_damage: u8,
    x: u16,
    health: u8,
}

impl CastlevaniaArchiveKey {
    pub fn champion_key(self) -> CastlevaniaChampionKey {
        CastlevaniaChampionKey {
            tier: self.tier,
            section: self.section,
            boss_damage: self.boss_damage,
            x: self.x,
            health: self.health,
        }
    }
}

#[must_use]
pub fn tier(state: CastlevaniaMechanicalState) -> CastlevaniaTier {
    CastlevaniaTier {
        stage: state.stage,
        boss_down: state.boss_down(),
        whip: state.whip,
        subweapon: state.subweapon != 0,
        hearts: state.hearts_bucket(),
    }
}

#[must_use]
pub fn archive_key(state: CastlevaniaMechanicalState) -> Option<CastlevaniaArchiveKey> {
    if state.terminal() {
        return None;
    }
    let playing = state.playing();
    Some(CastlevaniaArchiveKey {
        tier: tier(state),
        section: state.section,
        mode: if playing {
            PLAY_MODE_MARK
        } else {
            state.mode.saturating_add(1)
        },
        boss_damage: state.boss_damage() / BOSS_DAMAGE_STEP,
        x: if playing { state.x / 16 } else { 0 },
        y: if playing { state.y / 16 } else { state.submode },
        health: state.health,
        hearts: state.hearts,
        lives: state.lives,
    })
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CastlevaniaMilestones {
    pub stage: u8,
    pub bosses_down: u8,
    pub victory: bool,
    pub death: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CastlevaniaMilestoneTimes {
    pub first_stage: [Option<u64>; FINAL_STAGE as usize + 1],
    pub first_victory: Option<u64>,
    pub first_death: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CastlevaniaMilestoneInputs {
    pub first_stage: Vec<Option<CastlevaniaInput>>,
    pub first_victory: Option<CastlevaniaInput>,
    pub first_death: Option<CastlevaniaInput>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CastlevaniaProgressWatermark {
    pub stage: u8,
    pub section: u8,
    pub boss_damage: u8,
    pub x: u16,
}

pub type CastlevaniaArchiveProgressPoint =
    ProgressPoint<CastlevaniaMilestones, CastlevaniaProgressWatermark>;
pub type CastlevaniaArchiveEntryReport =
    ArchiveEntryReport<ButtonChord, CastlevaniaArchiveKey, CastlevaniaMilestones>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CastlevaniaArchiveReport {
    pub seed: u64,
    pub executions: u64,
    pub milestones: CastlevaniaMilestones,
    pub progress_watermark: CastlevaniaProgressWatermark,
    pub first_reached: CastlevaniaMilestoneTimes,
    pub first_inputs: CastlevaniaMilestoneInputs,
    pub champion_input: CastlevaniaInput,
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<CastlevaniaArchiveEntryReport>,
    pub progress_curve: Vec<CastlevaniaArchiveProgressPoint>,
    pub retained: u64,
    pub rejected: u64,
    pub deaths: u64,
    #[serde(default)]
    pub selector: SelectorAccounting,
}

#[must_use]
pub fn bosses_down(state: CastlevaniaMechanicalState) -> u8 {
    let passed = u8::try_from(
        crate::castlevania::target::BOSSES
            .iter()
            .filter(|(stage, _)| *stage < state.stage)
            .count(),
    )
    .unwrap_or(u8::MAX);
    passed.saturating_add(u8::from(state.boss_down() || state.victory()))
}

#[must_use]
pub fn milestones(state: CastlevaniaMechanicalState) -> CastlevaniaMilestones {
    let dead = state.dead();
    CastlevaniaMilestones {
        stage: if dead { 0 } else { state.stage },
        bosses_down: if dead { 0 } else { bosses_down(state) },
        victory: state.victory(),
        death: dead,
    }
}

#[must_use]
pub fn milestones_from_observation(observation: &CastlevaniaObservations) -> CastlevaniaMilestones {
    milestones(observation.decoded)
}

pub fn merge_milestones(into: &mut CastlevaniaMilestones, from: CastlevaniaMilestones) {
    into.stage = into.stage.max(from.stage);
    into.bosses_down = into.bosses_down.max(from.bosses_down);
    into.victory |= from.victory;
    into.death |= from.death;
}

#[must_use]
pub fn milestone_key(value: CastlevaniaMilestones) -> (bool, u8, u8, bool) {
    (value.victory, value.bosses_down, value.stage, !value.death)
}

#[must_use]
pub fn progress_point(state: CastlevaniaMechanicalState) -> Option<CastlevaniaProgressWatermark> {
    state.playing().then(|| CastlevaniaProgressWatermark {
        stage: state.stage,
        section: state.section,
        boss_damage: state.boss_damage(),
        x: state.x,
    })
}

pub fn merge_progress_watermark(
    watermark: &mut CastlevaniaProgressWatermark,
    observations: &[CastlevaniaObservations],
) {
    for observation in observations {
        if let Some(point) = progress_point(observation.decoded) {
            *watermark = (*watermark).max(point);
        }
    }
}

pub fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

pub const HELD_CHORDS: [u8; 20] = [
    0x00, 0x80, 0x40, 0x10, 0x20, 0x90, 0xa0, 0x50, 0x60, 0x01, 0x81, 0x41, 0x02, 0x82, 0x42, 0x22,
    0x12, 0x92, 0x52, 0x83,
];

pub const CHORDS: ChordVocabulary = ChordVocabulary {
    held: &HELD_CHORDS,
    tap: None,
    short_hold: SHORT_HOLD_FRAMES,
    long_hold: (16, 60),
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::rand::RomuDuoJrRand;

    fn state(stage: u8, x: u16, health: u8) -> CastlevaniaMechanicalState {
        CastlevaniaMechanicalState {
            mode: 5,
            submode: 6,
            stage,
            lives: 4,
            health,
            x,
            y: 0xa0,
            hearts: 5,
            boss_health: 0x40,
            ..CastlevaniaMechanicalState::default()
        }
    }

    #[test]
    fn places_are_32_pixel_cells_within_a_section() {
        for x in (0_u16..=0x0600).step_by(7) {
            let key = archive_key(state(1, x, 0x40)).unwrap();
            assert_eq!(key.place(), (0, 0, 0, x / 32, 0xa0 / 32));
            assert_eq!(key.identity().3, x / 16);
        }
        assert_ne!(
            archive_key(state(1, 500, 0x40)).unwrap().progress(),
            archive_key(state(2, 500, 0x40)).unwrap().progress()
        );
    }

    #[test]
    fn later_stages_order_above_further_positions_and_better_items() {
        let mut far = state(1, 0x0700, 0x40);
        far.whip = 2;
        far.subweapon = 8;
        far.hearts = 0x40;
        let next = state(2, 0x10, 0x08);
        assert!(archive_key(next).unwrap().progress() > archive_key(far).unwrap().progress());
        assert!(archive_key(next).unwrap() > archive_key(far).unwrap());
        let mut whip = state(1, 0x100, 0x40);
        whip.whip = 1;
        assert!(
            archive_key(whip).unwrap().progress()
                > archive_key(state(1, 0x100, 0x40)).unwrap().progress()
        );
    }

    #[test]
    fn health_is_the_same_place_preference() {
        let hurt = archive_key(state(2, 300, 0x10)).unwrap();
        let healthy = archive_key(state(2, 300, 0x40)).unwrap();
        assert_eq!(hurt.place(), healthy.place());
        assert_eq!(healthy.preference_cmp(0, hurt), Ordering::Greater);
    }

    #[test]
    fn boss_hits_are_places_and_a_downed_boss_is_a_tier() {
        let mut fight = state(3, 0x300, 0x40);
        fight.boss_health = 0x20;
        let hit = archive_key(fight).unwrap();
        assert_eq!(hit.boss_damage, 4);
        assert_ne!(
            hit.place(),
            archive_key(state(3, 0x300, 0x40)).unwrap().place()
        );
        fight.boss_health = 0;
        let down = archive_key(fight).unwrap();
        assert!(down.tier.boss_down);
        assert!(down.progress() > hit.progress());
        assert_eq!(bosses_down(fight), 1);
        assert_eq!(bosses_down(state(7, 0, 0x40)), 2);
    }

    #[test]
    fn transitions_are_staged_places_and_deaths_have_no_key() {
        let mut door = state(1, 0x2f0, 0x40);
        door.mode = 0x0a;
        door.submode = 1;
        let key = archive_key(door).unwrap();
        assert_eq!(key.x, 0);
        assert_eq!(key.y, 1);
        assert_ne!(key.place(), archive_key(state(1, 0, 0x40)).unwrap().place());
        let mut dead = state(1, 300, 0);
        assert_eq!(archive_key(dead), None);
        assert!(milestones(dead).death);
        dead.mode = 6;
        dead.health = 0x20;
        assert_eq!(archive_key(dead), None);
        assert_eq!(milestones(dead).stage, 0);
    }

    #[test]
    fn watermark_prefers_stages_over_distance() {
        let mut watermark = CastlevaniaProgressWatermark::default();
        let observation = |state| CastlevaniaObservations {
            frame_count: 1,
            decoded: state,
            terminal: false,
        };
        merge_progress_watermark(&mut watermark, &[observation(state(1, 2000, 0x40))]);
        merge_progress_watermark(&mut watermark, &[observation(state(2, 100, 0x40))]);
        merge_progress_watermark(&mut watermark, &[observation(state(1, 2200, 0x40))]);
        assert_eq!((watermark.stage, watermark.x), (2, 100));
    }

    #[test]
    fn vocabulary_excludes_start_and_select() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut previous = None;
        for _ in 0..1_000 {
            let chord = CHORDS.draw(&mut rand, previous.as_ref()).unwrap();
            previous = Some(chord);
            assert_eq!(chord.buttons & 0x0c, 0);
            assert!(HELD_CHORDS.contains(&chord.buttons));
        }
    }
}
