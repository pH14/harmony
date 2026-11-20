// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

use crate::{
    chord::{ChordVocabulary, NES_PRESSABLE_BUTTON_MASKS, SHORT_HOLD_FRAMES},
    search::assertion::{Assertion, Declarations, Number},
    workload::{Game, Moves, Never, Step},
};
use machine::nes::ButtonChord;

pub const LEVEL_COUNT: u8 = 3;
pub const INITIAL_HEALTH: u8 = 3;
pub const GAME_STATE_TITLE: u8 = 0;
pub const GAME_STATE_PLAYING: u8 = 1;
pub const GAME_STATE_LOADING: u8 = 2;
pub const FINAL_NEXT_LEVEL: u8 = 0xff;

const NEXT_LEVEL: usize = 0x35;
const CURRENT_LEVEL: usize = 0x42;
const CAMERA_SCREEN: usize = 0x52;
const CAMERA_FINE: usize = 0x57;
const PLAYER_X: usize = 0x62;
const PLAYER_Y: usize = 0x63;
const PLAYER_ON_GROUND: usize = 0x6b;
const ANIMATION: usize = 0x71;
const ANIMATION_FRAME: usize = 0x72;
const PLAYER_FACING: usize = 0x74;
const PLAYER_HEALTH: usize = 0x75;
const PLAYER_INVINCIBLE: usize = 0x76;
const GAME_STATE: usize = 0x9e;
const TRANSITION_TIMER: usize = 0xce;
pub const TOUCHED_GOAL: usize = 0x04e3;
const START: u8 = 0x08;

const CLEARED: [&str; LEVEL_COUNT as usize] = ["cleared_1", "cleared_2", "cleared_3"];

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct TwinDragonsState {
    pub game_state: u8,
    pub level: u8,
    pub next_level: u8,
    pub loaded: bool,
    pub at_goal: bool,
    pub timer: u8,
    pub in_play: bool,
    pub x: i16,
    pub y: u8,
    pub health: u8,
    pub on_ground: bool,
    pub facing: u8,
    pub invincible: bool,
    pub animation: u8,
    pub animation_frame: u8,
}

impl TwinDragonsState {
    #[must_use]
    pub fn playing(self) -> bool {
        self.game_state == GAME_STATE_PLAYING
    }

    #[must_use]
    pub fn loading(self) -> bool {
        self.game_state == GAME_STATE_LOADING
    }

    #[must_use]
    pub fn dead(self) -> bool {
        self.playing() && self.loaded && !self.at_goal && !self.in_play
    }

    #[must_use]
    pub fn levels_cleared(self) -> u8 {
        match self.game_state {
            GAME_STATE_PLAYING if self.at_goal => self.level.saturating_add(1),
            GAME_STATE_PLAYING | GAME_STATE_LOADING => self.level,
            _ => 0,
        }
    }

    #[must_use]
    pub fn victory(self) -> bool {
        self.levels_cleared() >= LEVEL_COUNT
    }

    #[must_use]
    pub fn terminal(self) -> bool {
        self.dead() || self.victory()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TwinDragons;

fn byte(ram: &[u8], address: usize) -> u8 {
    ram.get(address).copied().unwrap_or(0)
}

fn expected_next_level(level: u8) -> u8 {
    if level.saturating_add(1) >= LEVEL_COUNT {
        FINAL_NEXT_LEVEL
    } else {
        level.saturating_add(1)
    }
}

fn byte_value(name: &'static str, value: u8) -> Assertion {
    Assertion::Value {
        name,
        value: Number::byte(value),
    }
}

fn flag(name: &'static str, value: bool) -> Assertion {
    Assertion::Value {
        name,
        value: Number::flag(value),
    }
}

impl Game for TwinDragons {
    type State = TwinDragonsState;
    type Move = Never;

    const NAME: &'static str = "twin-dragons";
    const STRATEGY: &'static str = include_str!("twin_dragons.toml");
    const CHORDS: ChordVocabulary = ChordVocabulary {
        held: &NES_PRESSABLE_BUTTON_MASKS,
        tap: None,
        short_hold: SHORT_HOLD_FRAMES,
        long_hold: (6, 40),
    };

    fn setup(&self) -> Vec<ButtonChord> {
        let mut tape = vec![ButtonChord::new(0, 1); 60];
        tape.push(ButtonChord::new(START, 2));
        tape.extend(std::iter::repeat_n(ButtonChord::new(0, 1), 240));
        tape
    }

    fn genesis(&self, state: &TwinDragonsState) -> Result<(), String> {
        let state = *state;
        let valid = state.playing()
            && state.level == 0
            && state.next_level == 1
            && state.in_play
            && state.health == INITIAL_HEALTH;
        if valid {
            Ok(())
        } else {
            Err(format!(
                "Twin Dragons setup did not reach level 1 gameplay: {state:?}"
            ))
        }
    }

    fn decode(
        &self,
        _: Option<&TwinDragonsState>,
        _previous: Option<&TwinDragonsState>,
        ram: &[u8],
    ) -> TwinDragonsState {
        let game_state = byte(ram, GAME_STATE);
        let level = byte(ram, CURRENT_LEVEL);
        let next_level = byte(ram, NEXT_LEVEL);
        let loaded = next_level == expected_next_level(level);
        let at_goal = loaded && byte(ram, TOUCHED_GOAL) != 0;
        let health = byte(ram, PLAYER_HEALTH);
        let in_play = game_state == GAME_STATE_PLAYING && loaded && !at_goal && health != 0;
        let state = TwinDragonsState {
            game_state,
            level,
            next_level,
            loaded,
            at_goal,
            timer: byte(ram, TRANSITION_TIMER),
            animation: byte(ram, ANIMATION),
            animation_frame: byte(ram, ANIMATION_FRAME),
            ..TwinDragonsState::default()
        };
        if !in_play {
            return state;
        }
        let camera = i16::from(byte(ram, CAMERA_SCREEN))
            .wrapping_mul(256)
            .wrapping_add(i16::from(byte(ram, CAMERA_FINE)));
        TwinDragonsState {
            in_play,
            x: camera.wrapping_add(i16::from(byte(ram, PLAYER_X))),
            y: byte(ram, PLAYER_Y),
            health,
            on_ground: byte(ram, PLAYER_ON_GROUND) != 0,
            facing: byte(ram, PLAYER_FACING),
            invincible: byte(ram, PLAYER_INVINCIBLE) != 0,
            ..state
        }
    }

    fn declare(&self, state: &TwinDragonsState, out: &mut impl Declarations) {
        let state = *state;
        out.declare(byte_value("game_state", state.game_state));
        out.declare(byte_value("level", state.level));
        out.declare(byte_value("next_level", state.next_level));
        out.declare(byte_value("cleared", state.levels_cleared()));
        out.declare(byte_value("timer", state.timer));
        out.declare(flag("playing", state.playing()));
        out.declare(flag("loading", state.loading()));
        out.declare(flag("loaded", state.loaded));
        out.declare(flag("at_goal", state.at_goal));
        out.declare(flag("in_play", state.in_play));
        out.declare(Assertion::Value {
            name: "x",
            value: Number::new(i64::from(state.x), i64::from(i16::MIN), i64::from(i16::MAX)),
        });
        out.declare(byte_value("y", state.y));
        out.declare(byte_value("health", state.health));
        out.declare(flag("on_ground", state.on_ground));
        out.declare(byte_value("facing", state.facing));
        out.declare(flag("invincible", state.invincible));
        out.declare(flag("dead", state.dead()));
        out.declare(Assertion::Sometimes {
            name: "victory",
            condition: state.victory(),
        });
        out.declare(Assertion::Sometimes {
            name: "death",
            condition: state.dead(),
        });
        for (mark, name) in CLEARED.iter().enumerate() {
            out.declare(Assertion::Sometimes {
                name,
                condition: usize::from(state.levels_cleared()) > mark,
            });
        }
    }

    fn moves(&self, _state: &TwinDragonsState) -> Moves<Never> {
        Moves::none()
    }

    fn play(&self, mv: Never, _state: &TwinDragonsState) -> Option<Step<Never>> {
        match mv {}
    }

    fn move_frames(&self, mv: Never) -> u64 {
        match mv {}
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::*;
    use crate::{
        search::{
            archive::ArchiveKey,
            assertion::{Accumulator, AssertionKey},
            rand::RomuDuoJrRand,
        },
        workload::{Lowered, NesObservation, Satisfied},
    };
    use machine::nes::WRAM_SIZE;

    fn playing_wram() -> Vec<u8> {
        let mut wram = vec![0; WRAM_SIZE];
        wram[GAME_STATE] = GAME_STATE_PLAYING;
        wram[NEXT_LEVEL] = 1;
        wram[PLAYER_HEALTH] = INITIAL_HEALTH;
        wram[PLAYER_X] = 120;
        wram[PLAYER_Y] = 175;
        wram
    }

    fn decode(wram: &[u8]) -> TwinDragonsState {
        TwinDragons.decode(None, None, wram)
    }

    fn lowered() -> Lowered<TwinDragons> {
        Lowered::new(TwinDragons, TwinDragons::STRATEGY).unwrap()
    }

    fn observe(state: TwinDragonsState) -> NesObservation<TwinDragonsState> {
        let lowered = lowered();
        lowered.checked_record(&state).unwrap();
        lowered.observe(0, state, &mut Accumulator::default())
    }

    fn key(state: TwinDragonsState) -> Option<AssertionKey> {
        observe(state).key
    }

    fn marks(satisfied: Satisfied) -> Vec<String> {
        lowered()
            .sometimes()
            .iter()
            .enumerate()
            .filter(|(index, _)| satisfied.holds(*index))
            .map(|(_, name)| name.clone())
            .collect()
    }

    fn state(level: u8, x: i16, health: u8) -> TwinDragonsState {
        TwinDragonsState {
            game_state: GAME_STATE_PLAYING,
            level,
            next_level: expected_next_level(level),
            loaded: true,
            in_play: true,
            x,
            y: 175,
            health,
            ..TwinDragonsState::default()
        }
    }

    fn transition(level: u8, game_state: u8, timer: u8) -> TwinDragonsState {
        TwinDragonsState {
            game_state,
            level,
            next_level: expected_next_level(level),
            loaded: true,
            timer,
            ..TwinDragonsState::default()
        }
    }

    #[test]
    fn world_x_adds_the_camera_screen_and_fine_scroll() {
        let mut wram = playing_wram();
        wram[CAMERA_SCREEN] = 2;
        wram[CAMERA_FINE] = 40;
        wram[PLAYER_X] = 136;
        let state = decode(&wram);
        assert!(state.in_play);
        assert_eq!(state.x, 2 * 256 + 40 + 136);
        assert_eq!(state.y, 175);
        assert_eq!(decode(&[]), TwinDragonsState::default());
    }

    #[test]
    fn world_x_wraps_as_a_signed_word() {
        let mut wram = playing_wram();
        wram[CAMERA_SCREEN] = 0xff;
        wram[CAMERA_FINE] = 0xf0;
        wram[PLAYER_X] = 0;
        let state = decode(&wram);
        assert_eq!(state.x, -16);
        let near = key(state).unwrap();
        let mut origin = state;
        origin.x = 0;
        assert!(near.place() < key(origin).unwrap().place());
        assert_ne!(near.place(), key(origin).unwrap().place());
        origin.x = -32;
        assert_eq!(near.place(), key(origin).unwrap().place());
    }

    #[test]
    fn position_is_read_only_during_gameplay() {
        let mut wram = playing_wram();
        wram[GAME_STATE] = GAME_STATE_LOADING;
        wram[ANIMATION] = 2;
        let state = decode(&wram);
        assert!(!state.in_play);
        assert_eq!((state.x, state.y, state.health), (0, 0, 0));
        assert_eq!(state.animation, 2);
    }

    #[test]
    fn genesis_requires_level_one_at_full_health() {
        assert!(TwinDragons.genesis(&decode(&playing_wram())).is_ok());
        for (address, value) in [
            (GAME_STATE, GAME_STATE_TITLE),
            (PLAYER_HEALTH, 2),
            (CURRENT_LEVEL, 1),
            (TOUCHED_GOAL, 1),
        ] {
            let mut wram = playing_wram();
            wram[address] = value;
            assert!(TwinDragons.genesis(&decode(&wram)).is_err());
        }
    }

    #[test]
    fn zero_health_is_a_rejected_death() {
        let mut wram = playing_wram();
        wram[PLAYER_HEALTH] = 0;
        let state = decode(&wram);
        assert!(state.dead() && state.terminal() && !state.victory());
        let observed = observe(state);
        assert!(observed.key.is_none() && observed.rejected == Some(0) && !observed.goal);
        assert!(observed.stops());
        assert_eq!(marks(observed.satisfied), ["death"]);
    }

    #[test]
    fn touching_the_final_goal_is_the_goal() {
        let mut wram = playing_wram();
        wram[CURRENT_LEVEL] = LEVEL_COUNT - 1;
        wram[NEXT_LEVEL] = FINAL_NEXT_LEVEL;
        wram[TOUCHED_GOAL] = 1;
        let state = decode(&wram);
        assert!(!state.in_play && !state.dead());
        assert_eq!(state.levels_cleared(), LEVEL_COUNT);
        let observed = observe(state);
        assert!(observed.key.is_none() && observed.rejected.is_none() && observed.goal);
        assert!(observed.stops());
        assert_eq!(
            marks(observed.satisfied),
            ["victory", "cleared_1", "cleared_2", "cleared_3"]
        );
    }

    #[test]
    fn an_earlier_goal_clears_one_level_without_ending_the_game() {
        let mut wram = playing_wram();
        wram[TOUCHED_GOAL] = 1;
        let cleared = decode(&wram);
        assert_eq!(cleared.levels_cleared(), 1);
        let observed = observe(cleared);
        assert!(observed.key.is_some() && !observed.stops());
        assert_eq!(marks(observed.satisfied), ["cleared_1"]);
        wram[TOUCHED_GOAL] = 0;
        wram[GAME_STATE] = GAME_STATE_LOADING;
        wram[CURRENT_LEVEL] = 1;
        let loading = decode(&wram);
        assert_eq!(loading.levels_cleared(), 1);
        assert!(!loading.in_play);
        assert!(!observe(loading).stops());
    }

    #[test]
    fn a_stale_goal_during_the_next_level_load_is_not_a_clear() {
        let mut wram = playing_wram();
        wram[CURRENT_LEVEL] = 1;
        wram[TOUCHED_GOAL] = 1;
        let state = decode(&wram);
        assert!(!state.loaded && !state.at_goal && !state.in_play);
        assert!(!state.terminal());
        assert_eq!(state.levels_cleared(), 1);
        let observed = observe(state);
        assert!(observed.key.is_some() && !observed.stops());
        wram[NEXT_LEVEL] = FINAL_NEXT_LEVEL;
        wram[CURRENT_LEVEL] = LEVEL_COUNT - 1;
        assert!(observe(decode(&wram)).goal);
    }

    #[test]
    fn the_title_screen_has_no_key() {
        let mut wram = playing_wram();
        wram[GAME_STATE] = GAME_STATE_TITLE;
        let observed = observe(decode(&wram));
        assert!(observed.key.is_none() && !observed.stops());
    }

    #[test]
    fn places_are_32_pixel_cells_and_identities_16_pixel_cells() {
        let here = key(state(1, 500, 3)).unwrap();
        let near = key(state(1, 511, 3)).unwrap();
        let half = key(state(1, 480, 3)).unwrap();
        let next = key(state(1, 512, 3)).unwrap();
        assert_eq!(here.place(), near.place());
        assert_eq!(here.identity(), near.identity());
        assert_eq!(here.place(), half.place());
        assert_ne!(here.identity(), half.identity());
        assert!(next.place() > here.place());
        assert_ne!(
            key(state(0, 500, 3)).unwrap().place(),
            key(state(1, 500, 3)).unwrap().place()
        );
        let mut low = state(1, 500, 3);
        low.y = 200;
        assert!(key(low).unwrap().place() > here.place());
    }

    #[test]
    fn level_transitions_are_staged_places_in_the_next_tier() {
        let mut goal = transition(0, GAME_STATE_PLAYING, 0);
        goal.at_goal = true;
        let mut loading = transition(1, GAME_STATE_LOADING, 0);
        loading.loaded = false;
        let mut settling = loading;
        settling.game_state = GAME_STATE_PLAYING;
        let mut stages = Vec::new();
        for timer in [0, 40, 127] {
            goal.timer = timer;
            stages.push(key(goal).unwrap());
        }
        for timer in [0, 100, 165] {
            loading.timer = timer;
            stages.push(key(loading).unwrap());
        }
        stages.push(key(settling).unwrap());
        assert!(
            stages
                .iter()
                .all(|key| key.progress() == stages[0].progress())
        );
        assert!(
            stages
                .windows(2)
                .all(|pair| pair[0].place() < pair[1].place())
        );
        let far = key(state(0, 2200, 3)).unwrap();
        let next = key(state(1, 40, 3)).unwrap();
        assert!(far.progress() < stages[0].progress());
        assert_eq!(next.progress(), stages[0].progress());
        assert!(stages.iter().all(|key| key.place() < next.place()));
    }

    #[test]
    fn later_levels_order_above_further_positions() {
        let far = key(state(0, 2200, 3)).unwrap();
        let next = key(state(1, 40, 1)).unwrap();
        assert!(next.progress() > far.progress());
    }

    #[test]
    fn health_is_the_same_place_preference() {
        let hurt = key(state(0, 300, 1)).unwrap();
        let healthy = key(state(0, 300, 3)).unwrap();
        assert_eq!(hurt.place(), healthy.place());
        assert_eq!(hurt.identity(), healthy.identity());
        assert_eq!(healthy.preference_cmp(0, hurt), Ordering::Greater);
        let mut goal = transition(0, GAME_STATE_PLAYING, 0);
        goal.at_goal = true;
        assert_eq!(
            key(goal).unwrap().preference_cmp(0, key(goal).unwrap()),
            Ordering::Equal
        );
        assert_eq!(lowered().compiled.shape().preferences, 1);
        assert_eq!(lowered().compiled.shape().capacity, 1);
    }

    #[test]
    fn named_progress_marks_victory_death_and_every_cleared_level() {
        let mut names = vec!["victory".to_owned(), "death".to_owned()];
        names.extend(CLEARED.iter().map(|name| (*name).to_owned()));
        assert_eq!(lowered().sometimes(), names);
        let mut loading = transition(2, GAME_STATE_LOADING, 0);
        loading.loaded = false;
        assert_eq!(
            marks(observe(loading).satisfied),
            ["cleared_1", "cleared_2"]
        );
    }

    #[test]
    fn vocabulary_excludes_start_and_select() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut previous = None;
        for _ in 0..1_000 {
            let chord = TwinDragons::CHORDS
                .draw(&mut rand, previous.as_ref())
                .unwrap();
            previous = Some(chord);
            assert_eq!(chord.buttons & 0x0c, 0);
            assert!(NES_PRESSABLE_BUTTON_MASKS.contains(&chord.buttons));
        }
    }

    #[test]
    fn setup_tape_presses_start_without_other_buttons() {
        let tape = TwinDragons.setup();
        assert_eq!(tape.iter().filter(|chord| chord.buttons != 0).count(), 1);
        assert!(tape.iter().all(|chord| chord.buttons & !START == 0));
    }
}
