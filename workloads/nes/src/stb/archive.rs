// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{cmp::Ordering, error::Error, num::NonZeroUsize};

use serde::{Deserialize, Serialize};

use crate::{
    search::{
        archive::{
            Archive, ArchiveEntryReport, ArchiveKey, ProgressPoint, SelectorAccounting,
            SelectorPolicy, entries_by_suffix,
        },
        rand::RomuDuoJrRand,
    },
    stb::target::{ButtonChord, StbInput, StbMechanicalState, StbObservations, StbSnapshot},
};

pub use crate::search::archive::MAX_ARCHIVE_ENTRIES;

pub const MAX_STB_ACTIONS: usize = 8_192;
pub const KEY_POLICY_IDENTIFIER: &str = "stb_local_ai_spatial_16_preference_v3";
pub const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
pub const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";
pub use crate::stb::target::INITIAL_STOCKS;

pub fn selector_policy_from_identifier(identifier: &str) -> Result<SelectorPolicy, Box<dyn Error>> {
    crate::search::archive::selector_policy_from_identifier(
        identifier,
        StbArchiveKey::groups().saturating_sub(2),
    )
}

pub type StbArchive = Archive<ButtonChord, StbArchiveKey, StbMilestones, StbSnapshot>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct StbArchiveGroup {
    stage: u8,
    player_a_x: i16,
    player_a_y: i16,
    player_b_x: i16,
    player_b_y: i16,
    opponent_kos: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct StbArchiveKey {
    pub opponent_kos: u8,
    pub opponent_damage: u8,
    pub player_a_stocks: u8,
    pub stage: u8,
    pub player_a_x: i16,
    pub player_a_y: i16,
    pub player_b_x: i16,
    pub player_b_y: i16,
    pub player_b_stocks: u8,
    pub player_a_damage: u8,
}

impl Ord for StbArchiveKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.progress_key()
            .cmp(&other.progress_key())
            .then_with(|| self.identity_key().cmp(&other.identity_key()))
    }
}

impl PartialOrd for StbArchiveKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl ArchiveKey for StbArchiveKey {
    type Group = StbArchiveGroup;

    fn groups() -> usize {
        5
    }

    fn group(self, depth: usize) -> Self::Group {
        let location = StbArchiveGroup {
            stage: self.stage,
            player_a_x: self.player_a_x,
            player_a_y: self.player_a_y,
            player_b_x: self.player_b_x,
            player_b_y: self.player_b_y,
            opponent_kos: self.opponent_kos,
        };
        match depth {
            0 => location,
            1 => StbArchiveGroup {
                player_a_x: self.player_a_x.div_euclid(2),
                player_a_y: self.player_a_y.div_euclid(2),
                player_b_x: self.player_b_x.div_euclid(2),
                player_b_y: self.player_b_y.div_euclid(2),
                ..location
            },
            2 => StbArchiveGroup {
                player_a_x: self.player_a_x.div_euclid(8),
                player_a_y: self.player_a_y.div_euclid(8),
                player_b_x: self.player_b_x.div_euclid(8),
                player_b_y: self.player_b_y.div_euclid(8),
                ..location
            },
            3 => StbArchiveGroup {
                stage: self.stage,
                opponent_kos: self.opponent_kos,
                ..StbArchiveGroup::default()
            },
            _ => StbArchiveGroup {
                opponent_kos: self.opponent_kos,
                ..StbArchiveGroup::default()
            },
        }
    }

    fn slot_capacity() -> usize {
        1
    }

    fn preference_cmp(self, other: Self) -> Ordering {
        self.preference().cmp(&other.preference())
    }

    type Lineage = ();

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct StbChampionKey {
    progress: (u8, u8, u8),
    preference: (u8, u8, u8, u8, u8),
}

impl StbArchiveKey {
    pub(super) fn champion_key(self) -> StbChampionKey {
        StbChampionKey {
            progress: self.progress_key(),
            preference: self.preference(),
        }
    }

    fn progress_key(self) -> (u8, u8, u8) {
        (
            self.opponent_kos,
            self.opponent_damage,
            self.player_a_stocks,
        )
    }

    fn identity_key(self) -> (u8, i16, i16, i16, i16, u8, u8) {
        (
            self.stage,
            self.player_a_x,
            self.player_a_y,
            self.player_b_x,
            self.player_b_y,
            self.player_b_stocks,
            self.player_a_damage,
        )
    }

    fn preference(self) -> (u8, u8, u8, u8, u8) {
        (
            self.opponent_kos,
            u8::MAX - self.player_b_stocks,
            self.opponent_damage,
            self.player_a_stocks,
            u8::MAX - self.player_a_damage,
        )
    }
}

#[must_use]
pub fn player_b_kos(state: StbMechanicalState) -> u8 {
    state.gameplay.map_or(0, |gameplay| {
        INITIAL_STOCKS.saturating_sub(gameplay.player_b_stocks)
    })
}

#[must_use]
pub fn player_a_kos(state: StbMechanicalState) -> u8 {
    state.gameplay.map_or(0, |gameplay| {
        INITIAL_STOCKS.saturating_sub(gameplay.player_a_stocks)
    })
}

#[must_use]
pub fn archive_key(state: StbMechanicalState) -> Option<StbArchiveKey> {
    let gameplay = state.gameplay?;
    Some(StbArchiveKey {
        opponent_kos: player_b_kos(state),
        opponent_damage: gameplay.player_b_damage,
        player_a_stocks: gameplay.player_a_stocks,
        stage: state.stage,
        player_a_x: gameplay.player_a_x.div_euclid(16),
        player_a_y: gameplay.player_a_y.div_euclid(16),
        player_b_x: gameplay.player_b_x.div_euclid(16),
        player_b_y: gameplay.player_b_y.div_euclid(16),
        player_b_stocks: gameplay.player_b_stocks,
        player_a_damage: gameplay.player_a_damage,
    })
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct StbMilestones {
    pub opponent_damage: u8,
    pub opponent_kos: u8,
    pub player_a_kos: u8,
    pub victory: bool,
    pub defeat: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct StbMilestoneTimes {
    pub first_opponent_ko: Option<u64>,
    pub first_victory: Option<u64>,
    pub first_defeat: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct StbMilestoneInputs {
    pub first_opponent_ko: Option<StbInput>,
    pub first_victory: Option<StbInput>,
    pub first_defeat: Option<StbInput>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct StbProgressWatermark {
    pub opponent_kos: u8,
    pub opponent_damage: u8,
}

pub type StbArchiveProgressPoint = ProgressPoint<StbMilestones, StbProgressWatermark>;
pub type StbArchiveEntryReport = ArchiveEntryReport<ButtonChord, StbArchiveKey, StbMilestones>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StbArchiveReport {
    pub seed: u64,
    pub executions: u64,
    pub milestones: StbMilestones,
    pub progress_watermark: StbProgressWatermark,
    pub first_reached: StbMilestoneTimes,
    pub first_inputs: StbMilestoneInputs,
    pub champion_input: StbInput,
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<StbArchiveEntryReport>,
    pub progress_curve: Vec<StbArchiveProgressPoint>,
    pub retained: u64,
    pub rejected: u64,
    pub deaths: u64,
    #[serde(default)]
    pub selector: SelectorAccounting,
}

#[must_use]
pub fn milestones(state: StbMechanicalState) -> StbMilestones {
    let Some(gameplay) = state.gameplay else {
        return StbMilestones {
            victory: state.player_a_won(),
            defeat: state.match_over() && state.game_winner == 1,
            ..StbMilestones::default()
        };
    };
    StbMilestones {
        opponent_damage: gameplay.player_b_damage,
        opponent_kos: player_b_kos(state),
        player_a_kos: player_a_kos(state),
        victory: state.player_a_won(),
        defeat: state.match_over() && state.game_winner == 1,
    }
}

#[must_use]
pub fn milestones_from_observation(observation: &StbObservations) -> StbMilestones {
    let mut value = milestones(observation.decoded);
    value.opponent_kos = observation.player_b_ko_count;
    value.player_a_kos = observation.player_a_ko_count;
    value
}

pub fn merge_milestones(into: &mut StbMilestones, from: StbMilestones) {
    into.opponent_damage = into.opponent_damage.max(from.opponent_damage);
    into.opponent_kos = into.opponent_kos.max(from.opponent_kos);
    into.player_a_kos = into.player_a_kos.max(from.player_a_kos);
    into.victory |= from.victory;
    into.defeat |= from.defeat;
}

#[must_use]
pub fn milestone_key(value: StbMilestones) -> (u8, u8, bool, u8) {
    (
        value.opponent_kos,
        value.opponent_damage,
        value.victory,
        u8::MAX - value.player_a_kos,
    )
}

pub fn merge_progress_watermark(
    watermark: &mut StbProgressWatermark,
    observations: &[StbObservations],
) {
    for observation in observations {
        let state = observation.decoded;
        let opponent_damage = state
            .gameplay
            .map_or(watermark.opponent_damage, |gameplay| {
                gameplay.player_b_damage
            });
        watermark.opponent_kos = watermark.opponent_kos.max(observation.player_b_ko_count);
        watermark.opponent_damage = watermark.opponent_damage.max(opponent_damage);
    }
}

pub fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

pub const LONGEST_HOLD_FRAMES: u8 = 120;

const DIRECTIONS: [u8; 9] = [0, 0x10, 0x20, 0x40, 0x80, 0x90, 0x50, 0xa0, 0x60];
const AB: [u8; 4] = [0, 0x02, 0x01, 0x03];

pub fn sample_chord(rand: &mut RomuDuoJrRand) -> Result<ButtonChord, Box<dyn Error>> {
    let direction = DIRECTIONS
        [rand.below(NonZeroUsize::new(DIRECTIONS.len()).ok_or("empty STB direction vocabulary")?)];
    let buttons =
        direction | AB[rand.below(NonZeroUsize::new(AB.len()).ok_or("empty STB A/B vocabulary")?)];
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

    fn state(x: i16, b_damage: u8, b_stocks: u8) -> StbMechanicalState {
        StbMechanicalState {
            gameplay: Some(crate::stb::target::StbGameplayState {
                player_a_x: x,
                player_b_x: 100,
                player_a_y: 80,
                player_b_y: 80,
                player_b_damage: b_damage,
                player_b_stocks: b_stocks,
                player_a_stocks: 4,
                ..crate::stb::target::StbGameplayState::default()
            }),
            ..StbMechanicalState::default()
        }
    }

    #[test]
    fn coarse_groups_have_uniform_width_on_both_sides_of_zero() {
        for x in -512_i16..=512 {
            let key = archive_key(state(x, 0, 4)).unwrap();
            assert_eq!(key.group(1).player_a_x, x.div_euclid(32));
            assert_eq!(key.group(2).player_a_x, x.div_euclid(128));
        }
    }

    #[test]
    fn location_is_separate_from_same_location_preference() {
        let weak = archive_key(state(100, 0, 4)).expect("live archive key");
        let strong = archive_key(state(100, 80, 4)).expect("live archive key");
        assert_eq!(weak.group(0), strong.group(0));
        assert_eq!(strong.preference_cmp(weak), Ordering::Greater);
        assert_eq!(StbArchiveKey::slot_capacity(), 1);
    }

    #[test]
    fn objective_progress_precedes_positional_tie_breaking() {
        let behind = archive_key(state(300, 0, 4)).expect("live archive key");
        let ahead = archive_key(state(-300, 1, 3)).expect("live archive key");
        assert!(ahead > behind);
    }

    #[test]
    fn terminal_state_has_no_gameplay_payload() {
        let mut state = state(100, 80, 0);
        state.game_state = 2;
        state.game_winner = 0;
        state.gameplay = None;
        assert_eq!(archive_key(state), None);
        assert_eq!(player_b_kos(state), 0);
        assert_eq!(milestones(state).opponent_kos, 0);
    }

    #[test]
    fn observation_counts_terminal_underflow_as_final_loss() {
        let mut prior = state(100, 80, 0);
        prior.game_state = 0;
        let mut terminal = prior;
        terminal.game_state = 2;
        terminal.game_winner = 0;
        terminal.gameplay = None;
        let observation = StbObservations {
            frame_count: 9,
            decoded: terminal,
            changed_indices: vec![],
            player_a_ko: false,
            player_b_ko: true,
            player_a_ko_count: 0,
            player_b_ko_count: 5,
            terminal: true,
            log_line: String::new(),
        };
        let value = milestones_from_observation(&observation);
        assert_eq!(value.opponent_kos, 5);
        assert!(value.victory);
    }

    #[test]
    fn vocabulary_covers_a_b_and_directions_without_menu_buttons() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut saw_a = false;
        let mut saw_b = false;
        for _ in 0..1_000 {
            let chord = sample_chord(&mut rand).expect("draw chord");
            assert_eq!(chord.buttons & 0x0c, 0);
            saw_a |= chord.buttons & 0x01 != 0;
            saw_b |= chord.buttons & 0x02 != 0;
        }
        assert!(saw_a && saw_b);
    }
}
