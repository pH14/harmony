// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

use crate::{
    chord::ChordVocabulary,
    search::assertion::{Assertion, Declarations, Number},
    workload::{Game, Moves, Never, Step},
};
use machine::nes::ButtonChord;

pub const MAZES: u8 = 32;
pub const MAZE_LIMIT: u8 = 200;
pub const DOTS: u8 = 192;
pub const START_LIVES: u8 = 3;
pub const MAZES_PER_LEVEL: u8 = 4;
pub const FINAL_LEVEL: u8 = 22;
pub const LEVELS: u8 = 6;

pub const MODE_READY: u8 = 0x02;
pub const MODE_PLAY: u8 = 0x04;
pub const MODE_GHOST_EATEN: u8 = 0x06;
pub const MODE_DEATH: u8 = 0x08;
pub const MODE_GAME_OVER: u8 = 0x0a;
pub const MODE_MAZE_CLEAR: u8 = 0x0c;

pub const UP: u8 = 0x10;
pub const DOWN: u8 = 0x20;
pub const LEFT: u8 = 0x40;
pub const RIGHT: u8 = 0x80;

const PAC_X: usize = 0x1a;
const PAC_Y: usize = 0x1c;
const MODE: usize = 0x3f;
const DEMO: usize = 0x48;
const DIRECTION: usize = 0x51;
const LIVES: usize = 0x67;
const LEVEL: usize = 0x68;
const DOTS_LEFT: usize = 0x6a;
const SCORE: usize = 0x70;
const SCORE_DIGITS: usize = 6;
const POWER: usize = 0x88;
const POWER_SECONDS: usize = 0x89;
const POWER_FRAMES: usize = 0x8a;
const PAUSED: usize = 0x60f;

const LEVEL_UNSET: u8 = 0xff;
const FRAMES_PER_SECOND: u16 = 60;
const MAX_POWER_FRAMES: u16 = 0x7f * FRAMES_PER_SECOND + FRAMES_PER_SECOND;
const MAX_SCORE: i64 = 9_999_990;
const SCREEN_LEFT: i32 = 12;
const SCREEN_TOP: i32 = 4;
const START: u8 = 0x08;
const GENESIS_WAIT_FRAMES: usize = 252;
const HELD: [u8; 5] = [0x00, UP, DOWN, LEFT, RIGHT];

const CLEARED: [&str; MAZES as usize] = [
    "maze_1", "maze_2", "maze_3", "maze_4", "maze_5", "maze_6", "maze_7", "maze_8", "maze_9",
    "maze_10", "maze_11", "maze_12", "maze_13", "maze_14", "maze_15", "maze_16", "maze_17",
    "maze_18", "maze_19", "maze_20", "maze_21", "maze_22", "maze_23", "maze_24", "maze_25",
    "maze_26", "maze_27", "maze_28", "maze_29", "maze_30", "maze_31", "maze_32",
];

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PacManState {
    pub mode: u8,
    pub demo: u8,
    pub lives: u8,
    pub level: u8,
    pub dots: u8,
    pub score: u32,
    pub x: u8,
    pub y: u8,
    pub direction: u8,
    pub repeat_clears: u8,
    pub power: u8,
    pub power_frames: u16,
    pub paused: bool,
}

impl PacManState {
    #[must_use]
    pub fn mazes_cleared(self) -> u8 {
        if self.level == LEVEL_UNSET {
            return 0;
        }
        if self.level >= FINAL_LEVEL {
            return self
                .level
                .saturating_add(self.repeat_clears)
                .saturating_add(u8::from(self.repeat_clears == 0 && self.dots == 0))
                .min(MAZE_LIMIT);
        }
        self.level
            .saturating_add(u8::from(self.dots == 0))
            .min(MAZE_LIMIT)
    }

    #[must_use]
    pub fn dots_eaten(self) -> u8 {
        DOTS.saturating_sub(self.dots)
    }

    #[must_use]
    pub fn in_game(self) -> bool {
        self.demo == 0 && self.level != LEVEL_UNSET
    }

    #[must_use]
    pub fn on_maze(self) -> bool {
        self.in_game() && matches!(self.mode, MODE_PLAY | MODE_GHOST_EATEN | MODE_DEATH)
    }

    #[must_use]
    pub fn playing(self) -> bool {
        self.mode == MODE_PLAY && self.in_game() && !self.paused && self.dots != 0
    }

    #[must_use]
    pub fn game_over(self) -> bool {
        self.mode == MODE_GAME_OVER
    }

    #[must_use]
    pub fn terminal(self) -> bool {
        self.game_over() || self.demo != 0
    }

    #[must_use]
    pub fn empowered(self) -> bool {
        self.power != 0
    }

    #[must_use]
    pub fn screen(self) -> (u16, u16) {
        let x = (i32::from(self.x) - SCREEN_LEFT).clamp(0, 255);
        let y = (i32::from(self.y) - SCREEN_TOP).clamp(0, 223);
        (
            u16::try_from(x).unwrap_or_default(),
            u16::try_from(y).unwrap_or_default(),
        )
    }

    #[must_use]
    pub fn pose(self) -> u16 {
        let column = match self.direction & 3 {
            0 => 2,
            1 => 1,
            2 => 3,
            _ => 0,
        };
        let (x, y) = self.screen();
        column << 1 | ((x + y) >> 2 & 1)
    }

    #[must_use]
    pub fn level_group(self) -> u8 {
        (self.mazes_cleared() / MAZES_PER_LEVEL).min(LEVELS - 1)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PacMan;

fn byte(ram: &[u8], address: usize) -> u8 {
    ram.get(address).copied().unwrap_or(0)
}

fn value(name: &'static str, value: i64, min: i64, max: i64) -> Assertion {
    Assertion::Value {
        name,
        value: Number::new(value, min, max),
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

impl Game for PacMan {
    type State = PacManState;
    type Move = Never;

    const NAME: &'static str = "pac-man";
    const STRATEGY: &'static str = include_str!("pac_man.toml");
    const CHORDS: ChordVocabulary = ChordVocabulary {
        held: &HELD,
        tap: None,
        short_hold: (2, 16),
        long_hold: (24, 64),
    };

    fn setup(&self) -> Vec<ButtonChord> {
        let mut tape = Vec::new();
        let wait = |tape: &mut Vec<ButtonChord>, frames: usize| {
            tape.extend((0..frames).map(|_| ButtonChord::new(0, 1)));
        };
        wait(&mut tape, 300);
        tape.push(ButtonChord::new(START, 2));
        wait(&mut tape, 100);
        tape.push(ButtonChord::new(START, 2));
        wait(&mut tape, GENESIS_WAIT_FRAMES);
        tape
    }

    fn genesis(&self, state: &PacManState) -> Result<(), String> {
        let state = *state;
        let valid = state.mode == MODE_PLAY
            && state.in_game()
            && state.lives == START_LIVES
            && state.level == 0
            && state.dots >= DOTS - 1
            && state.score <= 10
            && state.power == 0
            && !state.paused;
        if valid {
            Ok(())
        } else {
            Err(format!(
                "Pac-Man setup did not reach the first maze: {state:?}"
            ))
        }
    }

    fn decode(&self, previous: Option<&PacManState>, ram: &[u8]) -> PacManState {
        let mut score = 0u32;
        for digit in (0..SCORE_DIGITS).rev() {
            score = score * 10 + u32::from(byte(ram, SCORE + digit).min(9));
        }
        let power = byte(ram, POWER);
        let seconds = byte(ram, POWER_SECONDS);
        let power_frames = if power == 0 || seconds >= 0x80 {
            0
        } else {
            u16::from(seconds) * FRAMES_PER_SECOND
                + FRAMES_PER_SECOND.saturating_sub(u16::from(byte(ram, POWER_FRAMES)))
        };
        let mode = byte(ram, MODE);
        let level = byte(ram, LEVEL);
        let repeat_clears = previous.map_or(0, |previous| {
            previous.repeat_clears.saturating_add(u8::from(
                mode == MODE_MAZE_CLEAR && previous.mode != MODE_MAZE_CLEAR && level >= FINAL_LEVEL,
            ))
        });
        PacManState {
            mode,
            demo: byte(ram, DEMO),
            lives: byte(ram, LIVES),
            level,
            dots: byte(ram, DOTS_LEFT),
            score: score * 10,
            x: byte(ram, PAC_X),
            y: byte(ram, PAC_Y),
            direction: byte(ram, DIRECTION) & 3,
            repeat_clears,
            power,
            power_frames,
            paused: byte(ram, PAUSED) != 0,
        }
    }

    fn declare(&self, state: &PacManState, out: &mut impl Declarations) {
        let state = *state;
        out.declare(byte_value("mode", state.mode));
        out.declare(byte_value("level", state.level));
        out.declare(byte_value("lives", state.lives));
        out.declare(value(
            "mazes_cleared",
            i64::from(state.mazes_cleared()),
            0,
            i64::from(MAZE_LIMIT),
        ));
        out.declare(value(
            "dots_eaten",
            i64::from(state.dots_eaten()),
            0,
            i64::from(DOTS),
        ));
        out.declare(value("score", i64::from(state.score), 0, MAX_SCORE));
        out.declare(byte_value("x", state.x));
        out.declare(byte_value("y", state.y));
        out.declare(flag("powered", state.empowered()));
        out.declare(value(
            "power_frames",
            i64::from(state.power_frames),
            0,
            i64::from(MAX_POWER_FRAMES),
        ));
        out.declare(flag("playing", state.playing()));
        out.declare(flag("demo", state.demo != 0));
        out.declare(Assertion::Sometimes {
            name: "maze_200",
            condition: state.mazes_cleared() >= MAZE_LIMIT,
        });
        out.declare(Assertion::Sometimes {
            name: "game_over",
            condition: state.game_over(),
        });
        for (mark, name) in CLEARED.iter().enumerate() {
            out.declare(Assertion::Sometimes {
                name,
                condition: usize::from(state.mazes_cleared()) > mark,
            });
        }
    }

    fn moves(&self, _state: &PacManState) -> Moves<Never> {
        Moves::none()
    }

    fn play(&self, mv: Never, _state: &PacManState) -> Option<Step<Never>> {
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
        wram[MODE] = MODE_PLAY;
        wram[LIVES] = START_LIVES;
        wram[DOTS_LEFT] = DOTS;
        wram[PAC_X] = 95;
        wram[PAC_Y] = 168;
        wram[DIRECTION] = 1;
        wram
    }

    fn decode(wram: &[u8]) -> PacManState {
        PacMan.decode(None, wram)
    }

    fn lowered() -> Lowered<PacMan> {
        Lowered::new(PacMan, PacMan::STRATEGY).unwrap()
    }

    fn observe(state: PacManState) -> NesObservation<PacManState> {
        let lowered = lowered();
        lowered.checked_record(&state).unwrap();
        lowered.observe(0, state, &mut Accumulator::default())
    }

    fn key(state: PacManState) -> Option<AssertionKey> {
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

    fn state(x: u8, y: u8) -> PacManState {
        PacManState {
            mode: MODE_PLAY,
            lives: START_LIVES,
            dots: DOTS,
            x,
            y,
            ..PacManState::default()
        }
    }

    #[test]
    fn the_decoder_reads_the_score_digits_and_the_power_timer() {
        let mut wram = playing_wram();
        wram[SCORE..SCORE + SCORE_DIGITS].copy_from_slice(&[3, 2, 1, 0, 0, 0]);
        wram[POWER] = 1;
        wram[POWER_SECONDS] = 4;
        wram[POWER_FRAMES] = 50;
        let state = decode(&wram);
        assert_eq!(state.score, 1230);
        assert_eq!(state.power_frames, 4 * 60 + 10);
        assert!(state.empowered());
        wram[POWER] = 0;
        assert_eq!(decode(&wram).power_frames, 0);
        assert_eq!(decode(&[]), PacManState::default());
    }

    #[test]
    fn the_declared_ranges_hold_at_their_extremes() {
        let mut wram = vec![0xff; WRAM_SIZE];
        wram[POWER_SECONDS] = 0x7f;
        wram[POWER_FRAMES] = 0;
        let state = decode(&wram);
        assert_eq!(state.score, 9_999_990);
        assert_eq!(state.power_frames, MAX_POWER_FRAMES);
        lowered().checked_record(&state).unwrap();
        let mut last = state;
        last.level = MAZE_LIMIT;
        last.repeat_clears = 0xff;
        assert_eq!(last.mazes_cleared(), MAZE_LIMIT);
        lowered().checked_record(&last).unwrap();
    }

    #[test]
    fn the_screen_position_is_the_top_left_of_the_sprite() {
        let state = decode(&playing_wram());
        assert_eq!(state.screen(), (83, 164));
        let mut corner = playing_wram();
        corner[PAC_X] = 3;
        corner[PAC_Y] = 1;
        assert_eq!(decode(&corner).screen(), (0, 0));
    }

    #[test]
    fn a_cleared_maze_counts_once_before_and_after_the_level_counter_moves() {
        let mut wram = playing_wram();
        wram[DOTS_LEFT] = 0;
        wram[MODE] = MODE_MAZE_CLEAR;
        assert_eq!(decode(&wram).mazes_cleared(), 1);
        wram[LEVEL] = 1;
        wram[DOTS_LEFT] = DOTS;
        wram[MODE] = MODE_READY;
        assert_eq!(decode(&wram).mazes_cleared(), 1);
        wram[LEVEL] = LEVEL_UNSET;
        wram[DOTS_LEFT] = 0;
        assert_eq!(decode(&wram).mazes_cleared(), 0);
    }

    #[test]
    fn mazes_past_the_final_level_count_by_repeat_clears() {
        let mut state = decode(&playing_wram());
        state.level = FINAL_LEVEL;
        assert_eq!(state.mazes_cleared(), FINAL_LEVEL);
        state.dots = 0;
        assert_eq!(state.mazes_cleared(), FINAL_LEVEL + 1);
        state.repeat_clears = 1;
        assert_eq!(state.mazes_cleared(), FINAL_LEVEL + 1);
        state.dots = DOTS;
        assert_eq!(state.mazes_cleared(), FINAL_LEVEL + 1);
        state.repeat_clears = 3;
        assert_eq!(state.mazes_cleared(), FINAL_LEVEL + 3);
    }

    #[test]
    fn repeat_clears_count_maze_clear_entries_past_the_final_level_through_previous() {
        let mut wram = playing_wram();
        wram[LEVEL] = FINAL_LEVEL;
        let play = decode(&wram);
        wram[DOTS_LEFT] = 0;
        wram[MODE] = MODE_MAZE_CLEAR;
        let entered = PacMan.decode(Some(&play), &wram);
        assert_eq!(entered.repeat_clears, 1);
        let held = PacMan.decode(Some(&entered), &wram);
        assert_eq!(held.repeat_clears, 1);
        assert_eq!(decode(&wram).repeat_clears, 0);
        wram[DOTS_LEFT] = DOTS;
        wram[MODE] = MODE_PLAY;
        let next = PacMan.decode(Some(&held), &wram);
        assert_eq!(next.repeat_clears, 1);
        assert_eq!(next.mazes_cleared(), FINAL_LEVEL + 1);
        wram[DOTS_LEFT] = 0;
        wram[MODE] = MODE_MAZE_CLEAR;
        let again = PacMan.decode(Some(&next), &wram);
        assert_eq!(again.repeat_clears, 2);
        assert_eq!(again.mazes_cleared(), FINAL_LEVEL + 2);
        let mut early = playing_wram();
        early[LEVEL] = FINAL_LEVEL - 1;
        let before = decode(&early);
        early[MODE] = MODE_MAZE_CLEAR;
        assert_eq!(PacMan.decode(Some(&before), &early).repeat_clears, 0);
    }

    #[test]
    fn mazes_group_four_to_a_level() {
        let mut state = decode(&playing_wram());
        assert_eq!(state.level_group(), 0);
        state.level = 3;
        state.dots = 0;
        assert_eq!(state.level_group(), 1);
        state.level = 200;
        assert_eq!(state.level_group(), LEVELS - 1);
    }

    #[test]
    fn genesis_requires_the_first_maze_in_play_with_three_lives() {
        let state = decode(&playing_wram());
        assert!(PacMan.genesis(&state).is_ok());
        for spoil in [
            |s: &mut PacManState| s.mode = MODE_READY,
            |s: &mut PacManState| s.lives = 2,
            |s: &mut PacManState| s.level = 1,
            |s: &mut PacManState| s.demo = 1,
            |s: &mut PacManState| s.dots = 100,
        ] {
            let mut spoiled = state;
            spoil(&mut spoiled);
            assert!(PacMan.genesis(&spoiled).is_err());
        }
    }

    #[test]
    fn only_game_over_and_the_attract_demo_are_rejected_and_nothing_reaches_the_goal() {
        assert!(!observe(state(0x78, 0x8d)).stops());
        let mut dead = state(0x78, 0x8d);
        dead.mode = MODE_DEATH;
        let observed = observe(dead);
        assert!(observed.key.is_none() && !observed.stops());
        let mut over = state(0x78, 0x8d);
        over.mode = MODE_GAME_OVER;
        let observed = observe(over);
        assert!(observed.key.is_none() && observed.rejected == Some(0) && !observed.goal);
        assert_eq!(marks(observed.satisfied), ["game_over"]);
        let mut demo = state(0x78, 0x8d);
        demo.demo = 1;
        assert_eq!(observe(demo).rejected, Some(1));
        let mut last = state(0x78, 0x8d);
        last.level = MAZE_LIMIT - 1;
        assert!(!observe(last).stops());
        last.level = MAZE_LIMIT;
        assert!(observe(last).goal);
    }

    #[test]
    fn death_ready_pause_and_clear_frames_have_no_key() {
        assert!(key(state(0x78, 0x8d)).is_some());
        let mut dead = state(0x78, 0x8d);
        dead.mode = MODE_DEATH;
        assert_eq!(key(dead), None);
        let mut ready = state(0x78, 0x8d);
        ready.mode = MODE_READY;
        assert_eq!(key(ready), None);
        let mut paused = state(0x78, 0x8d);
        paused.paused = true;
        assert_eq!(key(paused), None);
        let mut unset = state(0x78, 0x8d);
        unset.level = LEVEL_UNSET;
        assert_eq!(key(unset), None);
        let mut cleared = state(0x78, 0x8d);
        cleared.dots = 0;
        let observed = observe(cleared);
        assert_eq!(observed.key, None);
        assert_eq!(marks(observed.satisfied), ["maze_1"]);
    }

    #[test]
    fn the_place_is_the_eight_pixel_cell_and_the_power_state() {
        let here = key(state(0x78, 0x8d)).unwrap();
        let near = key(state(0x7f, 0x8f)).unwrap();
        let right = key(state(0x80, 0x8d)).unwrap();
        let below = key(state(0x78, 0x90)).unwrap();
        assert_eq!(here.place(), near.place());
        assert_ne!(here.place(), right.place());
        assert_ne!(here.place(), below.place());
        let mut powered = state(0x78, 0x8d);
        powered.power = 1;
        assert_ne!(here.place(), key(powered).unwrap().place());
    }

    #[test]
    fn the_tier_is_mazes_cleared_then_dots_eaten() {
        let plain = key(state(0x78, 0x8d)).unwrap();
        let mut eaten = state(0x78, 0x8d);
        eaten.dots = DOTS - 1;
        assert!(key(eaten).unwrap().progress() > plain.progress());
        let mut next = state(0x78, 0x8d);
        next.level = 1;
        assert!(key(next).unwrap().progress() > key(eaten).unwrap().progress());
        let mut repeated = state(0x78, 0x8d);
        repeated.level = FINAL_LEVEL;
        repeated.dots = 1;
        let mut again = repeated;
        again.repeat_clears = 1;
        again.dots = DOTS;
        assert!(key(again).unwrap().progress() > key(repeated).unwrap().progress());
    }

    #[test]
    fn lives_then_score_are_preferred() {
        let full = key(state(0x78, 0x8d)).unwrap();
        let mut hurt = state(0x78, 0x8d);
        hurt.lives = START_LIVES - 1;
        hurt.score = 5000;
        let hurt = key(hurt).unwrap();
        assert_eq!(full.place(), hurt.place());
        assert_eq!(full.preference_cmp(0, hurt), Ordering::Greater);
        let mut rich = state(0x78, 0x8d);
        rich.score = 200;
        assert_eq!(
            key(rich).unwrap().preference_cmp(0, full),
            Ordering::Greater
        );
        assert_eq!(lowered().compiled.shape().preferences, 1);
        assert_eq!(lowered().compiled.shape().capacity, 1);
    }

    #[test]
    fn named_progress_marks_every_cleared_maze() {
        let mut names = vec!["maze_200".to_owned(), "game_over".to_owned()];
        names.extend(CLEARED.iter().map(|name| (*name).to_owned()));
        assert_eq!(lowered().sometimes(), names);
        let mut later = state(0x78, 0x8d);
        later.level = 2;
        assert_eq!(marks(observe(later).satisfied), ["maze_1", "maze_2"]);
    }

    #[test]
    fn vocabulary_draws_single_directions_and_neutral() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut previous = None;
        for _ in 0..1_000 {
            let chord = PacMan::CHORDS.draw(&mut rand, previous.as_ref()).unwrap();
            previous = Some(chord);
            assert!(HELD.contains(&chord.buttons));
            assert!(chord.hold_frames >= 2);
        }
    }

    #[test]
    fn setup_tape_only_taps_start() {
        let tape = PacMan.setup();
        assert!(tape.iter().all(|chord| chord.buttons & !START == 0));
        assert_eq!(
            tape.iter().filter(|chord| chord.buttons == START).count(),
            2
        );
    }
}
