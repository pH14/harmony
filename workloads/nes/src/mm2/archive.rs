// SPDX-License-Identifier: AGPL-3.0-or-later

//! Mega Man 2 archive keys, state preferences, and report shapes.

use std::{cmp::Ordering, error::Error, num::NonZeroUsize};

use serde::{Deserialize, Serialize};

use crate::{
    mm2::target::{
        BOSS_DAMAGE_BUCKET, ButtonChord, ENEMY_DAMAGE_BUCKET, MENU_CLOSED, Mm2Input,
        Mm2MechanicalState, Mm2Observations, Mm2Snapshot, preference_tuple,
    },
    search::{
        archive::{
            Archive, ArchiveEntryReport, ArchiveKey, ProgressPoint, SelectorAccounting,
            SelectorPolicy, entries_by_suffix,
        },
        rand::RomuDuoJrRand,
    },
};

pub use crate::search::archive::MAX_ARCHIVE_ENTRIES;

/// Largest bounded input horizon accepted by a Mega Man 2 campaign.
pub const MAX_MM2_ACTIONS: usize = 8_192;
/// Recorded archive-key and per-location preference policy.
pub const KEY_POLICY_IDENTIFIER: &str = "mm2_rooms_seen_screen_room_boss_enemy_spatial_16_posture_weapon_menu_energy_platforms_preference_v17";
/// Recorded same-slot replacement policy.
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
/// Recorded controller hold distribution.
pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";

/// The parent selector named by a stream, resolved under this key's depths.
pub fn selector_policy_from_identifier(identifier: &str) -> Result<SelectorPolicy, Box<dyn Error>> {
    crate::search::archive::selector_policy_from_identifier(
        identifier,
        Mm2ArchiveKey::groups().saturating_sub(2),
    )
}

/// The Mega Man 2 archive instantiation.
pub type Mm2Archive = Archive<ButtonChord, Mm2ArchiveKey, Mm2Milestones, Mm2Snapshot>;

/// Opaque pooled identity returned to the generic selector.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Mm2ArchiveGroup {
    bosses: u8,
    stage: u8,
    rooms_seen: u8,
    screen: u8,
    room: u8,
    boss_damage: u8,
    enemy_damage: u8,
    x: u8,
    y: u8,
    posture: u8,
    weapon: u8,
    platforms: u8,
    menu: u8,
}

/// Quality-diversity key for one Mega Man 2 endpoint.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Mm2ArchiveKey {
    /// Robot masters defeated.
    pub bosses: u8,
    /// Stage number.
    pub stage: u8,
    /// Distinct level rooms this lineage has entered within the stage. A
    /// stage's route can double back to a lower screen index while still
    /// moving forward, so the count of rooms entered orders progress
    /// ahead of the screen index.
    pub rooms_seen: u8,
    /// Screen index within the stage.
    pub screen: u8,
    /// Level room around the player; splits locations that share a screen.
    pub room: u8,
    /// Damage dealt to the boss being fought, in fight-length buckets.
    pub boss_damage: u8,
    /// Damage dealt to ordinary enemies on this screen, in buckets.
    pub enemy_damage: u8,
    /// Player horizontal 16-pixel bucket within the screen.
    pub x: u8,
    /// Player vertical 16-pixel bucket within the screen.
    pub y: u8,
    /// Whether the player is grounded, airborne, or on a ladder. A moving
    /// platform and a ladder can share a column, and a landing shares its
    /// pixels with the fall that reached it; each is a different route.
    pub posture: u8,
    /// Equipped weapon; a wall only an item climbs is a different place
    /// with the item equipped. Splits locations only: the menu makes every
    /// weapon cheap to reach everywhere, so a selection cell per weapon
    /// would hand a dozen copies of each position the draws that the few
    /// states doing something new there, such as riding an item, need.
    pub weapon: u8,
    /// Summoned platforms alive on screen. Riding one is only possible
    /// while it lives, so the state with a fresh platform must keep its own
    /// slot beside the state where it has already faded.
    pub platforms: u8,
    /// Weapon menu page and row while it is open; each row is a step on the
    /// only route to another weapon. Rows split locations only: a selection
    /// cell per row would hand the menu most of the draws at a position.
    pub menu: u8,
    /// Current health.
    pub health: u8,
    /// Total weapon and item energy.
    pub energy: u16,
}

impl ArchiveKey for Mm2ArchiveKey {
    type Group = Mm2ArchiveGroup;

    fn groups() -> usize {
        5
    }

    /// Depth 0 is one 16-pixel location, depth 1 one 32-pixel selection cell,
    /// depth 2 a 128-pixel region of a screen, depth 3 the screen, and depth
    /// 4 the stage with its defeated-boss count. Posture and live platforms
    /// split locations and selection cells; the weapon and the menu split
    /// locations only. Resource fields
    /// never multiply slots; they decide which one representative remains at
    /// a location, so a state that spent energy without reaching a new
    /// location gives way to the state that kept it.
    fn group(self, depth: usize) -> Self::Group {
        let location = Mm2ArchiveGroup {
            stage: self.stage,
            rooms_seen: self.rooms_seen,
            screen: self.screen,
            room: self.room,
            boss_damage: self.boss_damage,
            enemy_damage: self.enemy_damage,
            x: self.x,
            y: self.y,
            posture: self.posture,
            weapon: self.weapon,
            platforms: self.platforms,
            menu: self.menu,
            ..Mm2ArchiveGroup::default()
        };
        match depth {
            0 => location,
            1 => Mm2ArchiveGroup {
                x: self.x / 2,
                y: self.y / 2,
                weapon: 0,
                menu: u8::from(self.menu != MENU_CLOSED),
                ..location
            },
            2 => Mm2ArchiveGroup {
                bosses: self.bosses,
                x: self.x / 8,
                y: self.y / 8,
                posture: 0,
                weapon: 0,
                platforms: 0,
                menu: 0,
                ..location
            },
            3 => Mm2ArchiveGroup {
                bosses: self.bosses,
                stage: self.stage,
                rooms_seen: self.rooms_seen,
                screen: self.screen,
                ..Mm2ArchiveGroup::default()
            },
            _ => Mm2ArchiveGroup {
                bosses: self.bosses,
                stage: self.stage,
                ..Mm2ArchiveGroup::default()
            },
        }
    }

    fn slot_capacity() -> usize {
        1
    }

    /// Progress is the stage path and boss damage; where the player stands
    /// inside one screen says nothing about how far the route has come.
    /// Within a stage the rooms entered and the path screen are compared
    /// together: a lineage ahead on one and behind on the other is neither
    /// ahead nor behind, so a detour through an extra room does not push
    /// the lineage further along the path off the front.
    fn progress_cmp(left: Self::Group, right: Self::Group) -> Ordering {
        (left.bosses, left.stage)
            .cmp(&(right.bosses, right.stage))
            .then_with(|| {
                let left = [
                    left.rooms_seen,
                    left.screen,
                    left.boss_damage,
                    left.enemy_damage,
                ];
                let right = [
                    right.rooms_seen,
                    right.screen,
                    right.boss_damage,
                    right.enemy_damage,
                ];
                let ahead = left.iter().zip(&right).any(|(l, r)| l > r);
                let behind = left.iter().zip(&right).any(|(l, r)| l < r);
                match (ahead, behind) {
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    _ => Ordering::Equal,
                }
            })
    }

    fn preference_cmp(self, other: Self) -> Ordering {
        self.preference().cmp(&other.preference())
    }

    /// Bit set of level rooms entered since the lineage's stage began.
    type Lineage = RoomsSeen;

    fn complete(self, parent: Option<(Self, &Self::Lineage)>) -> Self {
        let coarsest = Self::groups() - 1;
        let mut rooms = match parent {
            Some((key, rooms)) if key.group(coarsest) == self.group(coarsest) => *rooms,
            _ => RoomsSeen::default(),
        };
        rooms.insert(self.room);
        Self {
            rooms_seen: rooms.count(),
            ..self
        }
    }

    fn record(lineage: &mut Self::Lineage, key: Self) {
        lineage.insert(key.room);
    }
}

/// Bit set over the 256 possible level room bytes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RoomsSeen([u64; 4]);

impl RoomsSeen {
    fn insert(&mut self, room: u8) {
        self.0[usize::from(room / 64)] |= 1 << (room % 64);
    }

    fn count(self) -> u8 {
        let count: u32 = self.0.iter().map(|word| word.count_ones()).sum();
        u8::try_from(count).unwrap_or(u8::MAX)
    }
}

impl Mm2ArchiveKey {
    fn preference(self) -> (u8, u8, u16) {
        (self.bosses, self.health, self.energy)
    }
}

/// Build the opaque archive key from a decoded state.
#[must_use]
pub fn archive_key(state: Mm2MechanicalState) -> Mm2ArchiveKey {
    let (bosses, health, energy) = preference_tuple(state);
    Mm2ArchiveKey {
        bosses,
        health,
        energy,
        stage: state.stage,
        rooms_seen: 1,
        screen: state.screen,
        room: state.room,
        boss_damage: state.boss_damage() / BOSS_DAMAGE_BUCKET,
        enemy_damage: state.enemy_damage / ENEMY_DAMAGE_BUCKET,
        x: state.x / 16,
        y: state.y / 16,
        posture: state.posture(),
        weapon: state.weapon,
        platforms: state.platforms,
        menu: state.menu,
    }
}

/// Strongest rungs observed by a campaign.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mm2Milestones {
    /// Greatest screen index reached within the selected stage.
    pub max_screen: u8,
    /// Whether any input reached a boss with health on screen.
    pub reached_boss: bool,
    /// Whether any input defeated a robot master.
    pub defeated_boss: bool,
}

/// First deterministic execution reaching each rung.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mm2MilestoneTimes {
    /// First execution that left the first screen.
    pub first_new_screen: Option<u64>,
    /// First execution that reached a boss.
    pub first_boss: Option<u64>,
    /// First execution that defeated a boss.
    pub first_clear: Option<u64>,
}

/// First clean-reset input reaching each rung.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mm2MilestoneInputs {
    /// First input that left the first screen.
    pub first_new_screen: Option<Mm2Input>,
    /// First input that reached a boss.
    pub first_boss: Option<Mm2Input>,
    /// First input that defeated a boss.
    pub first_clear: Option<Mm2Input>,
}

/// Strongest lexicographic mechanical position seen at any emulated frame.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Mm2ProgressWatermark {
    /// Robot masters defeated.
    pub bosses: u8,
    /// Stage number.
    pub stage: u8,
    /// Screen index within the stage.
    pub screen: u8,
    /// Level room around the player.
    pub room: u8,
    /// Damage dealt to the boss being fought.
    pub boss_damage: u8,
    /// Damage dealt to ordinary enemies on this screen.
    pub enemy_damage: u8,
    /// Player X within the screen, in pixels.
    pub x: u8,
    /// Player Y within the screen, in pixels.
    pub y: u8,
}

/// Progress curve point.
pub type Mm2ArchiveProgressPoint = ProgressPoint<Mm2Milestones, Mm2ProgressWatermark>;
/// Archive entry report.
pub type Mm2ArchiveEntryReport = ArchiveEntryReport<ButtonChord, Mm2ArchiveKey, Mm2Milestones>;

/// Complete deterministic report for one Mega Man 2 campaign.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mm2ArchiveReport {
    /// Campaign seed.
    pub seed: u64,
    /// Admitted executions.
    pub executions: u64,
    /// Strongest milestones.
    pub milestones: Mm2Milestones,
    /// Strongest per-frame mechanical progress.
    pub progress_watermark: Mm2ProgressWatermark,
    /// First execution reaching each rung.
    pub first_reached: Mm2MilestoneTimes,
    /// First input reaching each rung.
    pub first_inputs: Mm2MilestoneInputs,
    /// Best input under the adapter's progress/preference order.
    pub champion_input: Mm2Input,
    /// Retained per-location representatives.
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<Mm2ArchiveEntryReport>,
    /// Fixed-interval deterministic progress curve.
    pub progress_curve: Vec<Mm2ArchiveProgressPoint>,
    /// Candidates admitted.
    pub retained: u64,
    /// Candidates rejected or superseded.
    pub rejected: u64,
    /// Terminal deaths observed.
    pub deaths: u64,
    /// Generic selector accounting.
    #[serde(default)]
    pub selector: SelectorAccounting,
}

/// Decode milestones from one state relative to the sealed genesis stage.
#[must_use]
pub fn milestones(state: Mm2MechanicalState, genesis_weapons: u8) -> Mm2Milestones {
    Mm2Milestones {
        max_screen: state.screen,
        reached_boss: state.boss_health != 0,
        defeated_boss: state.weapons_obtained & !genesis_weapons != 0,
    }
}

/// Merge strongest milestone fields.
pub fn merge_milestones(into: &mut Mm2Milestones, from: Mm2Milestones) {
    into.max_screen = into.max_screen.max(from.max_screen);
    into.reached_boss |= from.reached_boss;
    into.defeated_boss |= from.defeated_boss;
}

/// Stable champion order owned by the adapter.
#[must_use]
pub fn milestone_key(value: Mm2Milestones) -> (bool, bool, u8) {
    (value.defeated_boss, value.reached_boss, value.max_screen)
}

/// The watermark of one decoded state.
#[must_use]
pub fn progress_watermark(state: Mm2MechanicalState) -> Mm2ProgressWatermark {
    Mm2ProgressWatermark {
        bosses: state.bosses_beaten(),
        stage: state.stage,
        screen: state.screen,
        room: state.room,
        boss_damage: state.boss_damage(),
        enemy_damage: state.enemy_damage,
        x: state.x,
        y: state.y,
    }
}

/// Fold every action-interior observation into the progress watermark.
pub fn merge_progress_watermark(
    watermark: &mut Mm2ProgressWatermark,
    observations: &[Mm2Observations],
) {
    for observation in observations {
        *watermark = (*watermark).max(progress_watermark(observation.decoded));
    }
}

/// Held-frame clock used by same-slot route replacement.
pub fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

/// Longest hold [`sample_chord`] can draw; the suffix time bound is a
/// multiple of it.
pub const LONGEST_HOLD_FRAMES: u8 = 120;

const DIRECTIONS: [u8; 9] = [0, 0x10, 0x20, 0x40, 0x80, 0x50, 0x90, 0x60, 0xa0];
const AB: [u8; 4] = [0, 0x01, 0x02, 0x03];
const START: u8 = 0x08;
/// One chord in this many is a Start tap; Start opens and closes the weapon
/// menu, and a menu row selected with it is the only way to equip a weapon.
const START_ODDS: usize = 12;

/// Draw one game-neutral controller chord: any direction set with any A/B
/// set, or a short Start tap. Select is excluded because it only pauses.
pub fn sample_chord(rand: &mut RomuDuoJrRand) -> Result<ButtonChord, Box<dyn Error>> {
    if rand.below(NonZeroUsize::new(START_ODDS).ok_or("invalid start odds")?) == 0 {
        let hold = u8::try_from(2 + rand.below(NonZeroUsize::new(6).ok_or("invalid tap")?))?;
        return Ok(ButtonChord::new(START, hold));
    }
    let direction = DIRECTIONS
        [rand.below(NonZeroUsize::new(DIRECTIONS.len()).ok_or("empty direction vocabulary")?)];
    let buttons =
        direction | AB[rand.below(NonZeroUsize::new(AB.len()).ok_or("empty A/B vocabulary")?)];
    let hold_frames = if rand.below(NonZeroUsize::new(2).ok_or("invalid duration odds")?) == 0 {
        u8::try_from(2 + rand.below(NonZeroUsize::new(11).ok_or("invalid short duration")?))?
    } else {
        u8::try_from(48 + rand.below(NonZeroUsize::new(73).ok_or("invalid long duration")?))?
    };
    Ok(ButtonChord::new(buttons, hold_frames))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(x: u8, health: u8, weapons: u8) -> Mm2MechanicalState {
        Mm2MechanicalState {
            stage: 6,
            screen: 2,
            x,
            y: 0xb4,
            health,
            lives: 2,
            weapons_obtained: weapons,
            ..Mm2MechanicalState::default()
        }
    }

    #[test]
    fn one_location_uses_resources_only_for_preference() {
        let weak = archive_key(state(100, 4, 0));
        let strong = archive_key(state(100, 20, 0x40));
        assert_eq!(weak.group(0), strong.group(0));
        assert_eq!(weak.group(1), strong.group(1));
        assert_ne!(weak.group(2), strong.group(2));
        assert_eq!(strong.preference_cmp(weak), Ordering::Greater);
        assert_eq!(Mm2ArchiveKey::slot_capacity(), 1);
    }

    #[test]
    fn screens_are_separate_groups_below_the_stage() {
        let first = archive_key(state(100, 28, 0));
        let mut next_state = state(100, 28, 0);
        next_state.screen = 3;
        let next = archive_key(next_state);
        assert_ne!(first.group(3), next.group(3));
        assert_eq!(first.group(4), next.group(4));
    }

    #[test]
    fn progress_compares_rooms_and_screen_together() {
        let group = |rooms_seen, screen| Mm2ArchiveGroup {
            bosses: 8,
            stage: 11,
            rooms_seen,
            screen,
            ..Mm2ArchiveGroup::default()
        };
        assert_eq!(
            Mm2ArchiveKey::progress_cmp(group(14, 41), group(13, 44)),
            Ordering::Equal
        );
        assert_eq!(
            Mm2ArchiveKey::progress_cmp(group(14, 44), group(13, 41)),
            Ordering::Greater
        );
        assert_eq!(
            Mm2ArchiveKey::progress_cmp(group(13, 41), group(13, 44)),
            Ordering::Less
        );
        let later_stage = Mm2ArchiveGroup {
            stage: 12,
            ..group(1, 1)
        };
        assert_eq!(
            Mm2ArchiveKey::progress_cmp(later_stage, group(14, 44)),
            Ordering::Greater
        );
    }

    #[test]
    fn milestones_are_relative_to_genesis_weapons() {
        let value = milestones(state(0, 28, 0x40), 0x40);
        assert!(!value.defeated_boss);
        let value = milestones(state(0, 28, 0x44), 0x40);
        assert!(value.defeated_boss);
    }

    #[test]
    fn a_lineage_counts_distinct_rooms_within_one_stage() {
        let mut lineage = RoomsSeen::default();
        let first = archive_key(state(0x40, 28, 0)).complete(None);
        assert_eq!(first.rooms_seen, 1);
        Mm2ArchiveKey::record(&mut lineage, first);
        let mut next_state = state(0x40, 28, 0);
        next_state.room = 3;
        next_state.screen = 1;
        let second = archive_key(next_state).complete(Some((first, &lineage)));
        assert_eq!(second.rooms_seen, 2);
        Mm2ArchiveKey::record(&mut lineage, second);
        let back = archive_key(state(0x40, 28, 0)).complete(Some((second, &lineage)));
        assert_eq!(back.rooms_seen, 2);
        let mut other_stage = next_state;
        other_stage.stage = 7;
        let reset = archive_key(other_stage).complete(Some((second, &lineage)));
        assert_eq!(reset.rooms_seen, 1);
    }

    #[test]
    fn vocabulary_draws_bare_start_taps_and_never_select_or_conflicting_directions() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut starts = 0;
        for _ in 0..1_000 {
            let chord = sample_chord(&mut rand).expect("draw chord");
            assert_eq!(chord.buttons & 0x04, 0);
            if chord.buttons & START != 0 {
                assert_eq!(chord.buttons, START);
                assert!(chord.hold_frames <= 8);
                starts += 1;
            }
            assert_ne!(chord.buttons & 0x30, 0x30);
            assert_ne!(chord.buttons & 0xc0, 0xc0);
        }
        assert!((40..=140).contains(&starts), "start taps: {starts}");
    }
}
