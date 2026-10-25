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
pub const KEY_POLICY_IDENTIFIER: &str = "castlevania_stage_boss_tier_section_spatial_32_boss_damage_health_whip_subweapon_hearts_preference_dracula_health_place_v6";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";
const PLAY_MODE_MARK: u8 = 0;
const BOSS_DAMAGE_STEP: u8 = 8;
const DRACULA_HEALTH_STEP: u8 = 16;

pub type CastlevaniaArchive =
    Archive<ButtonChord, CastlevaniaArchiveKey, CastlevaniaMilestones, CastlevaniaSnapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CastlevaniaTier {
    pub stage: u8,
    pub boss_down: bool,
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
    pub whip: u8,
    pub subweapon: bool,
    pub hearts: u8,
    pub lives: u8,
    pub fight_health: u8,
}

impl Ord for CastlevaniaArchiveKey {
    fn cmp(&self, other: &Self) -> Ordering {
        (
            self.tier,
            self.section,
            self.boss_damage,
            self.x,
            self.health,
            self.whip,
            self.subweapon,
            self.hearts,
            self.mode,
            self.y,
            self.lives,
            self.fight_health,
        )
            .cmp(&(
                other.tier,
                other.section,
                other.boss_damage,
                other.x,
                other.health,
                other.whip,
                other.subweapon,
                other.hearts,
                other.mode,
                other.y,
                other.lives,
                other.fight_health,
            ))
    }
}

impl PartialOrd for CastlevaniaArchiveKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl ArchiveKey for CastlevaniaArchiveKey {
    type Place = (u8, u8, u8, u16, u8, u8);
    type Progress = CastlevaniaTier;
    type Identity = (u8, u8, u8, u16, u8);

    fn place(self) -> Self::Place {
        (
            self.section,
            self.mode,
            self.boss_damage,
            self.x.div_euclid(2),
            self.y.div_euclid(2),
            self.fight_health,
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
        (
            self.health,
            self.whip,
            self.subweapon,
            self.hearts,
            self.lives,
        )
            .cmp(&(
                other.health,
                other.whip,
                other.subweapon,
                other.hearts,
                other.lives,
            ))
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
        whip: state.whip,
        subweapon: state.subweapon != 0,
        hearts: state.hearts,
        lives: state.lives,
        fight_health: if dracula_fight(state) {
            state.health / DRACULA_HEALTH_STEP
        } else {
            0
        },
    })
}

#[must_use]
pub fn dracula_fight(state: CastlevaniaMechanicalState) -> bool {
    state.stage == FINAL_STAGE && state.boss_damage() > 0
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
            assert_eq!(key.place(), (0, 0, 0, x / 32, 0xa0 / 32, 0));
            assert_eq!(key.identity().3, x / 16);
        }
        assert_ne!(
            archive_key(state(1, 500, 0x40)).unwrap().progress(),
            archive_key(state(2, 500, 0x40)).unwrap().progress()
        );
    }

    #[test]
    fn later_stages_order_above_further_positions() {
        let mut far = state(1, 0x0700, 0x40);
        far.whip = 2;
        far.subweapon = 8;
        far.hearts = 0x40;
        let next = state(2, 0x10, 0x08);
        assert!(archive_key(next).unwrap().progress() > archive_key(far).unwrap().progress());
        assert!(archive_key(next).unwrap() > archive_key(far).unwrap());
    }

    #[test]
    fn items_and_hearts_share_a_tier_and_rank_by_preference() {
        let base = state(5, 0x100, 0x40);
        let base_key = archive_key(base).unwrap();
        let mut whip = base;
        whip.whip = 2;
        let mut subweapon = base;
        subweapon.subweapon = 8;
        let mut hearts = base;
        hearts.hearts = 0x40;
        for variant in [whip, subweapon, hearts] {
            let key = archive_key(variant).unwrap();
            assert_eq!(key.progress(), base_key.progress());
            assert_eq!(key.place(), base_key.place());
            assert_eq!(key.identity(), base_key.identity());
            assert_eq!(key.preference_cmp(0, base_key), Ordering::Greater);
        }
        let mut hurt_armed = whip;
        hurt_armed.health = 0x10;
        assert_eq!(
            base_key.preference_cmp(0, archive_key(hurt_armed).unwrap()),
            Ordering::Greater
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
    fn boss_hits_are_places_and_a_cleared_boss_is_a_tier() {
        let mut fight = state(3, 0x300, 0x40);
        fight.boss_health = 0x20;
        let hit = archive_key(fight).unwrap();
        assert_eq!(hit.boss_damage, 4);
        assert_ne!(
            hit.place(),
            archive_key(state(3, 0x300, 0x40)).unwrap().place()
        );
        fight.boss_health = 0;
        let empty_meter = archive_key(fight).unwrap();
        assert!(!empty_meter.tier.boss_down);
        assert_eq!(empty_meter.boss_damage, 8);
        assert_eq!(bosses_down(fight), 0);
        fight.mode = 0x0c;
        fight.submode = 0;
        let down = archive_key(fight).unwrap();
        assert!(down.tier.boss_down);
        assert!(down.progress() > hit.progress());
        assert_eq!(bosses_down(fight), 1);
        assert_eq!(bosses_down(state(7, 0, 0x40)), 2);
    }

    #[test]
    fn dracula_phases_are_separate_places() {
        let mut fight = state(18, 0x300, 0x40);
        let mut place = |form: u8, meter: u8| {
            fight.second_form = form != 0;
            fight.boss_health = meter;
            archive_key(fight).unwrap().place()
        };
        let places = [
            place(0, 0x40),
            place(0, 0),
            place(1, 0),
            place(1, 0x40),
            place(1, 1),
        ];
        for (index, left) in places.iter().enumerate() {
            for right in &places[index + 1..] {
                assert_ne!(left, right);
            }
        }
    }

    #[test]
    fn each_dracula_hit_on_simon_is_a_new_place() {
        let mut full = state(18, 0x300, 0x40);
        full.boss_health = 0x30;
        let mut hurt = full;
        hurt.health = 0x30;
        assert_ne!(
            archive_key(full).unwrap().place(),
            archive_key(hurt).unwrap().place()
        );
        assert_eq!(
            archive_key(full).unwrap().progress(),
            archive_key(hurt).unwrap().progress()
        );
        full.second_form = true;
        hurt.second_form = true;
        assert_ne!(
            archive_key(full).unwrap().place(),
            archive_key(hurt).unwrap().place()
        );
        for (stage, meter) in [(18, 0x40), (17, 0x30), (3, 0x30)] {
            let mut full = state(stage, 0x300, 0x40);
            full.boss_health = meter;
            let mut hurt = full;
            hurt.health = 0x30;
            assert_eq!(
                archive_key(full).unwrap().place(),
                archive_key(hurt).unwrap().place()
            );
        }
    }

    #[test]
    fn the_stair_exit_into_the_final_stage_is_not_a_victory() {
        let mut entry = state(18, 0, 8);
        entry.mode = 0x08;
        entry.submode = 0;
        entry.boss_health = 0x40;
        let found = milestones(entry);
        assert!(!found.victory);
        assert!(!found.death);
        assert_eq!(found.stage, 18);
        assert_eq!(found.bosses_down, 5);
        entry.mode = 0x0f;
        let ending = milestones(entry);
        assert!(ending.victory);
        assert_eq!(ending.bosses_down, 6);
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
