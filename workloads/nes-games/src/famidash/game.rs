// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

use crate::{
    chord::ChordVocabulary,
    famidash::levels::{self, Menu, Side, Stage, group_stage},
    search::assertion::{Assertion, Number},
    workload::{Game, Moves, Step},
};
use machine::nes::{ButtonChord, MAX_HOLD_FRAMES};

pub const A_BUTTON: u8 = 0x01;
pub const RIGHT_BUTTON: u8 = 0x80;
pub const GAME_STATE_PLAYING: u8 = 2;
pub const GAME_STATE_COMPLETE: u8 = 3;
pub const GAME_STATE_LEVEL_SELECT: u8 = 6;
pub const LEVEL: usize = 0x047b;
pub const SCREEN_HEIGHT: i32 = 240;
pub const X_MIN: i64 = -(1 << 20);
pub const X_MAX: i64 = (1 << 20) - 1;

const GAMEMODE: usize = 0x7a;
const CUBE_DATA: usize = 0x7b;
const DUAL: usize = 0x96;
const PLAYER_X: usize = 0x043d;
const PLAYER_Y: usize = 0x0441;
const PLAYER_VELOCITY_Y: usize = 0x0449;
const PLAYER_GRAVITY: usize = 0x044d;
const PLAYER_MINI: usize = 0x044f;
const GAME_STATE: usize = 0x0498;
const SCROLL_X: usize = 0x04a2;
const SCROLL_Y: usize = 0x04a6;
const SCROLL_Y_SUBPIXEL: usize = 0x04a8;
const SPEED: usize = 0x045e;
const PAUSED: usize = 0x05b5;
const DEATH: u8 = 0x01;
const DEATH_CHECK_SCREEN_X: u8 = 0x20;
const LEVEL_LENGTH_UNIT: i32 = 100;
const MENU_SETTLE_FRAMES: u32 = 20;
const LEVEL_START_FRAMES: u32 = 40;
const LEVEL_DONE_POLL_FRAMES: u8 = 15;
const LEVEL_DONE_POLLS: u64 = 40;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Gameplay {
    pub x: i32,
    pub y: i32,
    pub scroll_x: i32,
    pub camera_y: i32,
    pub camera_subpixel: u8,
    pub velocity_y: i16,
    pub gamemode: u8,
    pub gravity: bool,
    pub mini: bool,
    pub dual: bool,
    pub speed: u8,
}

impl Gameplay {
    #[must_use]
    pub fn vertical_motion(self) -> u8 {
        match self.velocity_y.signum() {
            -1 => 1,
            1 => 2,
            _ => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FamidashState {
    pub game_state: u8,
    pub level: u8,
    pub paused: bool,
    pub dead: bool,
    pub gameplay: Option<Gameplay>,
    pub side: Side,
    pub goal: u8,
}

impl FamidashState {
    #[must_use]
    pub fn complete(self) -> bool {
        self.game_state == GAME_STATE_COMPLETE && !self.dead
    }

    #[must_use]
    pub fn finished(self) -> bool {
        self.complete() && self.level >= self.goal
    }

    #[must_use]
    pub fn advancing(self) -> bool {
        self.complete() && self.level < self.goal
    }

    #[must_use]
    pub fn terminal(self) -> bool {
        self.dead || self.finished()
    }

    #[must_use]
    pub fn cleared(self) -> u8 {
        self.level.saturating_add(u8::from(self.complete()))
    }

    #[must_use]
    pub fn percent(self) -> u8 {
        if self.complete() {
            100
        } else {
            self.gameplay.map_or(0, |gameplay| {
                levels::percent(self.side, self.level, gameplay.scroll_x)
            })
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Advance {
    Poll,
    PollWait,
    Right,
    Settle,
    Start,
    StartWait,
}

impl Advance {
    #[must_use]
    pub const fn frames() -> u64 {
        LEVEL_DONE_POLLS * (1 + LEVEL_DONE_POLL_FRAMES as u64)
            + 2 * MENU_SETTLE_FRAMES as u64
            + 2
            + LEVEL_START_FRAMES as u64
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Famidash<const GROUP: usize = 0> {
    pub stage: Stage,
}

impl<const GROUP: usize> Default for Famidash<GROUP> {
    fn default() -> Self {
        Self {
            stage: group_stage(GROUP),
        }
    }
}

impl<const GROUP: usize> Famidash<GROUP> {
    #[must_use]
    pub fn new(stage: Stage) -> Self {
        Self { stage }
    }
}

fn byte(ram: &[u8], address: usize) -> u8 {
    ram.get(address).copied().unwrap_or(0)
}

fn word(ram: &[u8], address: usize) -> u16 {
    u16::from_le_bytes([byte(ram, address), byte(ram, address + 1)])
}

fn long(ram: &[u8], address: usize) -> i32 {
    i32::from_le_bytes([
        byte(ram, address),
        byte(ram, address + 1),
        byte(ram, address + 2),
        byte(ram, address + 3),
    ])
}

#[must_use]
pub fn decode(stage: Stage, ram: &[u8]) -> FamidashState {
    let game_state = byte(ram, GAME_STATE);
    let level = byte(ram, LEVEL);
    let paused = byte(ram, PAUSED) != 0;
    let playing = game_state == GAME_STATE_PLAYING;
    let scroll_x = long(ram, SCROLL_X);
    let past_end = stage
        .side
        .level_length(level)
        .is_some_and(|length| scroll_x > i32::from(length) * LEVEL_LENGTH_UNIT);
    let player_dead = byte(ram, PLAYER_X + 1) > DEATH_CHECK_SCREEN_X
        && (byte(ram, CUBE_DATA) & DEATH != 0 || byte(ram, CUBE_DATA + 1) & DEATH != 0);
    let dead =
        playing && (past_end || player_dead) || game_state == GAME_STATE_COMPLETE && player_dead;
    let gameplay = (playing && !dead && !paused).then(|| {
        let [fine_y, screen_y] = word(ram, SCROLL_Y).to_le_bytes();
        let camera_y = i32::from(screen_y) * SCREEN_HEIGHT + i32::from(fine_y);
        Gameplay {
            x: scroll_x.saturating_add(i32::from(byte(ram, PLAYER_X + 1))),
            y: camera_y + i32::from(byte(ram, PLAYER_Y + 1)),
            scroll_x,
            camera_y,
            camera_subpixel: byte(ram, SCROLL_Y_SUBPIXEL),
            velocity_y: i16::from_le_bytes(word(ram, PLAYER_VELOCITY_Y).to_le_bytes()),
            gamemode: byte(ram, GAMEMODE),
            gravity: byte(ram, PLAYER_GRAVITY) != 0,
            mini: byte(ram, PLAYER_MINI) != 0,
            dual: byte(ram, DUAL) != 0,
            speed: byte(ram, SPEED),
        }
    });
    FamidashState {
        game_state,
        level,
        paused,
        dead,
        gameplay,
        side: stage.side,
        goal: stage.goal,
    }
}

fn idle(mut frames: u32) -> Vec<ButtonChord> {
    let mut chords = Vec::new();
    while frames > 0 {
        let hold = frames.min(u32::from(MAX_HOLD_FRAMES));
        chords.push(ButtonChord::new(
            0,
            u8::try_from(hold).unwrap_or(MAX_HOLD_FRAMES),
        ));
        frames -= hold;
    }
    chords
}

#[must_use]
pub fn level_select_tape(menu: Menu) -> Vec<ButtonChord> {
    let mut tape = idle(22);
    tape.push(ButtonChord::new(A_BUTTON, 1));
    tape.extend(idle(936));
    if menu == Menu::CommunityLevels {
        tape.push(ButtonChord::new(RIGHT_BUTTON, 1));
        tape.extend(idle(MENU_SETTLE_FRAMES));
    }
    tape.push(ButtonChord::new(A_BUTTON, 1));
    tape.extend(idle(90));
    tape
}

#[must_use]
pub fn start_level_tape() -> Vec<ButtonChord> {
    let mut tape = vec![ButtonChord::new(A_BUTTON, 1)];
    tape.extend(idle(LEVEL_START_FRAMES));
    tape
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

impl<const GROUP: usize> Game for Famidash<GROUP> {
    type State = FamidashState;
    type Move = Advance;

    const NAME: &'static str = "famidash";
    const STRATEGY: &'static str = include_str!("famidash.toml");
    const CHORDS: ChordVocabulary = ChordVocabulary {
        held: &[0x00, A_BUTTON],
        tap: None,
        short_hold: (1, 4),
        long_hold: (6, 40),
    };

    fn setup(&self) -> Vec<ButtonChord> {
        level_select_tape(self.stage.group().menu)
    }

    fn setup_pokes(&self) -> Vec<(usize, u8)> {
        vec![(LEVEL, self.stage.level)]
    }

    fn start(&self) -> Vec<ButtonChord> {
        start_level_tape()
    }

    fn genesis_id(&self) -> String {
        format!(
            "famidash-{}-level-{}-to-{}-v1",
            self.stage.side.id(),
            self.stage.level,
            self.stage.goal
        )
    }

    fn genesis(&self, state: &FamidashState) -> Result<(), String> {
        Stage::new(self.stage.side, self.stage.level, self.stage.goal)?;
        if state.gameplay.is_some() && state.level == self.stage.level && state.percent() == 0 {
            Ok(())
        } else {
            Err(format!(
                "Famidash setup did not reach level {} gameplay: {state:?}",
                self.stage.level
            ))
        }
    }

    fn decode(&self, _previous: Option<&FamidashState>, ram: &[u8]) -> FamidashState {
        decode(self.stage, ram)
    }

    fn declare(&self, state: &FamidashState, out: &mut impl FnMut(Assertion)) {
        let state = *state;
        let gameplay = state.gameplay.unwrap_or_default();
        out(byte_value("game_state", state.game_state));
        out(byte_value("level", state.level));
        out(flag("gameplay", state.gameplay.is_some()));
        out(flag("dead", state.dead));
        out(flag("paused", state.paused));
        out(flag("complete", state.complete()));
        out(value("percent", i64::from(state.percent()), 0, 100));
        out(byte_value("cleared", state.cleared()));
        out(value("x", i64::from(gameplay.x), X_MIN, X_MAX));
        out(value("y", i64::from(gameplay.y), 0, i64::from(u16::MAX)));
        out(value(
            "scroll_x",
            i64::from(gameplay.scroll_x),
            X_MIN,
            X_MAX,
        ));
        out(byte_value("gamemode", gameplay.gamemode));
        out(flag("gravity", gameplay.gravity));
        out(flag("mini", gameplay.mini));
        out(flag("dual", gameplay.dual));
        out(byte_value("speed", gameplay.speed));
        out(value("motion", i64::from(gameplay.vertical_motion()), 0, 2));
        out(byte_value("camera_subpixel", gameplay.camera_subpixel));
        out(value(
            "velocity_y",
            i64::from(gameplay.velocity_y),
            i64::from(i16::MIN),
            i64::from(i16::MAX),
        ));
        out(Assertion::Sometimes {
            name: "level_complete",
            condition: state.complete(),
        });
        out(Assertion::Sometimes {
            name: "finished",
            condition: state.finished(),
        });
    }

    fn moves(&self, state: &FamidashState) -> Moves<Advance> {
        if state.advancing() {
            Moves::forced(Advance::Poll)
        } else {
            Moves::none()
        }
    }

    fn play(&self, mv: Advance, state: &FamidashState) -> Option<Step<Advance>> {
        let step = |buttons, hold, rest| Step {
            chord: ButtonChord::new(buttons, hold),
            rest,
        };
        let settle = u8::try_from(MENU_SETTLE_FRAMES).unwrap_or(MAX_HOLD_FRAMES);
        Some(match mv {
            Advance::Poll if state.game_state == GAME_STATE_LEVEL_SELECT => {
                step(0, settle, Some(Advance::Right))
            }
            Advance::Poll => step(A_BUTTON, 1, Some(Advance::PollWait)),
            Advance::PollWait => step(0, LEVEL_DONE_POLL_FRAMES, Some(Advance::Poll)),
            Advance::Right => step(RIGHT_BUTTON, 1, Some(Advance::Settle)),
            Advance::Settle => step(0, settle, Some(Advance::Start)),
            Advance::Start => step(A_BUTTON, 1, Some(Advance::StartWait)),
            Advance::StartWait => step(
                0,
                u8::try_from(LEVEL_START_FRAMES).unwrap_or(MAX_HOLD_FRAMES),
                None,
            ),
        })
    }

    fn move_frames(&self, _mv: Advance) -> u64 {
        Advance::frames()
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
        workload::{Lowered, NesObservation},
    };
    use machine::nes::WRAM_SIZE;

    fn main() -> Stage {
        Stage::through_group(Side::Main, 0).unwrap()
    }

    fn game() -> Famidash {
        Famidash::new(main())
    }

    fn playing_wram() -> Vec<u8> {
        let mut wram = vec![0; WRAM_SIZE];
        wram[GAME_STATE] = GAME_STATE_PLAYING;
        wram[PLAYER_X + 1] = 80;
        wram[PLAYER_Y + 1] = 178;
        wram[SCROLL_Y] = 0xef;
        wram[SCROLL_Y + 1] = 2;
        wram
    }

    fn lowered() -> Lowered<Famidash> {
        Lowered::new(game(), Famidash::<0>::STRATEGY).unwrap()
    }

    fn observe(state: FamidashState) -> NesObservation<FamidashState> {
        let lowered = lowered();
        lowered.checked_record(&state).unwrap();
        lowered.observe(0, state, &mut Accumulator::default())
    }

    fn key(state: FamidashState) -> AssertionKey {
        observe(state).key.unwrap()
    }

    fn state(x: i32, y: i32, gamemode: u8) -> FamidashState {
        FamidashState {
            game_state: GAME_STATE_PLAYING,
            gameplay: Some(Gameplay {
                x,
                y,
                scroll_x: x - 80,
                gamemode,
                ..Gameplay::default()
            }),
            goal: 21,
            ..FamidashState::default()
        }
    }

    fn with(mut state: FamidashState, change: impl FnOnce(&mut Gameplay)) -> FamidashState {
        change(state.gameplay.as_mut().unwrap());
        state
    }

    fn cell(key: AssertionKey) -> (u32, u64) {
        (key.progress(), key.place().get())
    }

    #[test]
    fn the_forced_advance_polls_to_the_level_select_then_starts_the_next_level() {
        let game = Famidash::<0>::new(Stage::new(Side::Main, 0, 1).unwrap());
        let complete = FamidashState {
            game_state: GAME_STATE_COMPLETE,
            goal: 1,
            ..FamidashState::default()
        };
        let select = FamidashState {
            game_state: GAME_STATE_LEVEL_SELECT,
            ..complete
        };
        assert_eq!(game.moves(&complete).forced, Some(Advance::Poll));
        assert_eq!(game.moves(&select).forced, None);
        assert_eq!(
            game.moves(&FamidashState {
                goal: 0,
                ..complete
            })
            .forced,
            None
        );
        let mut mv = Some(Advance::Poll);
        let mut chords = Vec::new();
        let mut polls = 0;
        while let Some(current) = mv {
            let state = if polls < 2 { complete } else { select };
            let step = game.play(current, &state).unwrap();
            if step.chord == ButtonChord::new(A_BUTTON, 1) && current == Advance::Poll {
                polls += 1;
            }
            chords.push((step.chord.buttons, step.chord.bounded_hold_frames()));
            mv = step.rest;
        }
        assert_eq!(
            chords,
            [
                (A_BUTTON, 1),
                (0, 15),
                (A_BUTTON, 1),
                (0, 15),
                (0, 20),
                (RIGHT_BUTTON, 1),
                (0, 20),
                (A_BUTTON, 1),
                (0, 40),
            ]
        );
        assert_eq!(game.move_frames(Advance::Poll), 722);
    }

    #[test]
    fn world_position_adds_the_scroll_to_the_screen_position() {
        let mut wram = playing_wram();
        wram[SCROLL_X..SCROLL_X + 4].copy_from_slice(&1_000_i32.to_le_bytes());
        let gameplay = decode(main(), &wram).gameplay.unwrap();
        assert_eq!(gameplay.x, 1_080);
        assert_eq!(gameplay.camera_y, 2 * 240 + 0xef);
        assert_eq!(gameplay.y, 2 * 240 + 0xef + 178);
    }

    #[test]
    fn a_negative_scroll_before_the_level_moves_is_signed() {
        let mut wram = playing_wram();
        wram[SCROLL_X..SCROLL_X + 4].copy_from_slice(&(-240_i32).to_le_bytes());
        let state = decode(main(), &wram);
        assert_eq!(state.gameplay.unwrap().x, -160);
        assert_eq!(state.percent(), 0);
        assert!(observe(state).key.is_some());
    }

    #[test]
    fn either_player_death_bit_is_a_rejected_death() {
        for player in 0..2 {
            let mut wram = playing_wram();
            wram[CUBE_DATA + player] = DEATH;
            let state = decode(main(), &wram);
            assert!(state.dead && state.terminal() && state.gameplay.is_none());
            let observed = observe(state);
            assert!(observed.key.is_none() && observed.rejected == Some(0) && !observed.goal);
        }
    }

    #[test]
    fn a_death_bit_left_of_the_death_check_column_is_not_a_death() {
        let mut wram = playing_wram();
        wram[PLAYER_X + 1] = DEATH_CHECK_SCREEN_X;
        wram[CUBE_DATA] = DEATH;
        let state = decode(main(), &wram);
        assert!(!state.dead && state.gameplay.is_some());
        assert!(!observe(state).stops());
        wram[PLAYER_X + 1] = DEATH_CHECK_SCREEN_X + 1;
        assert!(decode(main(), &wram).dead);
    }

    #[test]
    fn only_the_last_level_complete_state_is_the_goal() {
        let mut wram = playing_wram();
        wram[GAME_STATE] = GAME_STATE_COMPLETE;
        let state = decode(main(), &wram);
        assert!(state.complete() && !state.dead && state.advancing() && !state.terminal());
        assert_eq!((state.percent(), state.cleared()), (100, 1));
        let observed = observe(state);
        assert!(observed.key.is_none() && !observed.stops());
        wram[LEVEL] = 21;
        let state = decode(main(), &wram);
        assert!(state.finished() && !state.advancing() && state.terminal());
        assert_eq!(state.cleared(), 22);
        let observed = observe(state);
        assert!(observed.key.is_none() && observed.goal && observed.rejected.is_none());
        wram[LEVEL] = 30;
        let state = decode(Stage::single(Side::B, 30).unwrap(), &wram);
        assert!(state.finished());
        assert_eq!(state.side, Side::B);
    }

    #[test]
    fn the_genesis_identity_names_the_side_and_the_levels() {
        let identity = |level, goal| {
            Famidash::<0>::new(Stage {
                level,
                goal,
                ..main()
            })
            .genesis_id()
        };
        assert_eq!(identity(0, 21), "famidash-main-level-0-to-21-v1");
        assert_ne!(identity(0, 0), identity(7, 9));
        assert_ne!(identity(0, 0), identity(0, 21));
    }

    #[test]
    fn a_death_bit_on_the_level_complete_frame_is_a_death() {
        let mut wram = playing_wram();
        wram[LEVEL] = 9;
        wram[SCROLL_X..SCROLL_X + 4].copy_from_slice(&13_219_i32.to_le_bytes());
        wram[PLAYER_X + 1] = 0x50;
        wram[GAME_STATE] = GAME_STATE_COMPLETE;
        wram[CUBE_DATA] = DEATH;
        let state = decode(main(), &wram);
        assert!(state.dead && !state.complete() && !state.advancing() && state.terminal());
        assert!(Famidash::<0>::new(main()).moves(&state).forced.is_none());
        wram[LEVEL] = 21;
        let state = decode(main(), &wram);
        assert!(state.dead && !state.finished());
        wram[PLAYER_X + 1] = DEATH_CHECK_SCREEN_X;
        let state = decode(main(), &wram);
        assert!(!state.dead && state.finished());
    }

    #[test]
    fn the_percent_uses_the_side_level_length() {
        let mut wram = playing_wram();
        wram[LEVEL] = 23;
        wram[SCROLL_X..SCROLL_X + 4].copy_from_slice(&(732_i32 * 40).to_le_bytes());
        assert_eq!(
            decode(Stage::single(Side::C, 23).unwrap(), &wram).percent(),
            40
        );
        assert!(decode(Stage::single(Side::Main, 23).unwrap(), &wram).dead);
    }

    #[test]
    fn a_scroll_past_the_level_length_is_a_dead_end() {
        let mut wram = playing_wram();
        wram[LEVEL] = 17;
        let stage = Stage::single(Side::C, 17).unwrap();
        wram[SCROLL_X..SCROLL_X + 4].copy_from_slice(&15_400_i32.to_le_bytes());
        let state = decode(stage, &wram);
        assert!(!state.dead);
        assert_eq!(state.percent(), 99);
        wram[SCROLL_X..SCROLL_X + 4].copy_from_slice(&15_401_i32.to_le_bytes());
        let state = decode(stage, &wram);
        assert!(state.dead && state.terminal() && state.gameplay.is_none());
    }

    #[test]
    fn a_paused_level_is_absent_and_does_not_stop() {
        let mut wram = playing_wram();
        wram[PAUSED] = 1;
        let state = decode(main(), &wram);
        assert!(state.gameplay.is_none() && !state.terminal());
        let observed = observe(state);
        assert!(observed.key.is_none() && !observed.stops());
    }

    #[test]
    fn genesis_requires_gameplay_in_the_selected_level() {
        assert!(game().genesis(&decode(main(), &playing_wram())).is_ok());
        let mut wram = playing_wram();
        wram[LEVEL] = 1;
        assert!(game().genesis(&decode(main(), &wram)).is_err());
        let first = Famidash::<0>::new(Stage::through_group(Side::Main, 1).unwrap());
        assert!(first.genesis(&decode(first.stage, &wram)).is_ok());
        wram[LEVEL] = 0;
        wram[GAME_STATE] = GAME_STATE_LEVEL_SELECT;
        assert!(game().genesis(&decode(main(), &wram)).is_err());
    }

    #[test]
    fn setup_presses_only_a_and_pokes_the_stage_level_before_the_start() {
        let mut tape = game().setup();
        tape.extend(game().start());
        assert_eq!(tape.iter().filter(|chord| chord.buttons != 0).count(), 3);
        assert!(tape.iter().all(|chord| chord.buttons & !A_BUTTON == 0));
        let frames: u32 = tape
            .iter()
            .map(|chord| u32::from(chord.bounded_hold_frames()))
            .sum();
        assert_eq!(frames, 1_091);
        let later = Famidash::<1>::new(Stage::single(Side::Main, 30).unwrap());
        assert_eq!(later.setup_pokes(), [(LEVEL, 30)]);
        assert_eq!(later.setup(), level_select_tape(Menu::CommunityLevels));
        assert_eq!(Famidash::<1>::default().stage, group_stage(1));
    }

    #[test]
    fn the_community_menu_is_one_right_press_from_the_main_menu() {
        let main = level_select_tape(Menu::MainLevels);
        let community = level_select_tape(Menu::CommunityLevels);
        let pressed: Vec<u8> = community
            .iter()
            .filter(|chord| chord.buttons != 0)
            .map(|chord| chord.buttons)
            .collect();
        assert_eq!(pressed, [A_BUTTON, RIGHT_BUTTON, A_BUTTON]);
        let frames = |tape: &[ButtonChord]| -> u32 {
            tape.iter()
                .map(|chord| u32::from(chord.bounded_hold_frames()))
                .sum()
        };
        assert_eq!(frames(&community), frames(&main) + 1 + MENU_SETTLE_FRAMES);
    }

    #[test]
    fn places_are_32_by_16_pixel_cells_per_level_and_mode() {
        let here = key(state(1_000, 897, 0));
        let mut later = state(1_000, 897, 0);
        later.level = 1;
        assert_ne!(cell(key(later)), cell(here));
        assert_ne!(key(later).place(), here.place());
        assert_ne!(cell(key(state(1_000, 897, 1))), cell(here));
        assert_eq!(cell(key(state(1_023, 911, 0))), cell(here));
        assert_ne!(cell(key(state(1_024, 911, 0))), cell(here));
        assert_ne!(cell(key(state(1_023, 912, 0))), cell(here));
        for change in [
            (|gameplay: &mut Gameplay| gameplay.gravity = true) as fn(&mut Gameplay),
            |gameplay| gameplay.mini = true,
            |gameplay| gameplay.dual = true,
        ] {
            assert_ne!(cell(key(with(state(1_000, 897, 0), change))), cell(here));
        }
    }

    #[test]
    fn each_speed_is_its_own_place() {
        let at = |speed| {
            key(with(state(2_784, 816, 1), |gameplay| {
                gameplay.speed = speed
            }))
        };
        let [double, triple] = [2, 3].map(at);
        assert_ne!(double.place(), triple.place());
        assert_eq!(double.progress(), triple.progress());
        assert_eq!(double.identity(), triple.identity());
    }

    #[test]
    fn progress_bands_are_512_pixels_of_distance_within_a_level() {
        let at = |x| key(state(x, 897, 0)).progress();
        assert_eq!(at(0), at(511));
        assert!(at(512) > at(511));
        assert!(at(-160) < at(0));
        let mut next = state(-160, 897, 0);
        next.level = 1;
        assert!(key(next).progress() > at(20_000));
    }

    #[test]
    fn rising_falling_and_resting_states_are_distinct_identities() {
        let rest = key(state(500, 850, 0));
        let up = key(with(state(500, 850, 0), |gameplay| {
            gameplay.velocity_y = -400
        }));
        let down = key(with(state(500, 850, 0), |gameplay| {
            gameplay.velocity_y = 400
        }));
        assert_eq!(cell(rest), cell(up));
        assert_eq!(cell(up), cell(down));
        assert_ne!(rest.identity(), up.identity());
        assert_ne!(up.identity(), down.identity());
        assert_ne!(rest.identity(), down.identity());
    }

    #[test]
    fn camera_subpixel_eighths_are_distinct_identities() {
        let at = |subpixel| {
            key(with(state(8_448, 751, 0), |gameplay| {
                gameplay.camera_subpixel = subpixel;
            }))
        };
        let [low, high, next] = [0x60, 0x7f, 0xb4].map(at);
        assert_eq!(cell(low), cell(next));
        assert_eq!(low.identity(), high.identity());
        assert_ne!(high.identity(), next.identity());
    }

    #[test]
    fn each_8_pixel_slot_of_a_cell_is_its_own_identity() {
        let [first, second, third, last] =
            [992, 1_000, 1_007, 1_023].map(|x| key(state(x, 897, 0)));
        assert_eq!(cell(first), cell(last));
        assert_ne!(first.identity(), second.identity());
        assert_eq!(second.identity(), third.identity());
        assert_ne!(second.identity(), last.identity());
    }

    #[test]
    fn the_earliest_state_in_a_slot_is_preferred_and_a_slot_keeps_one() {
        let near = key(state(1_000, 897, 0));
        let far = key(state(1_007, 897, 0));
        assert_eq!(cell(near), cell(far));
        assert_eq!(near.identity(), far.identity());
        assert_eq!(near.preference_cmp(0, far), Ordering::Greater);
        let shape = lowered().compiled.shape();
        assert_eq!((shape.capacity, shape.preferences), (1, 1));
    }

    #[test]
    fn named_progress_marks_a_level_complete_and_the_finish() {
        let lowered = lowered();
        assert_eq!(lowered.sometimes(), ["level_complete", "finished"]);
        let mut complete = state(14_000, 897, 0);
        complete.game_state = GAME_STATE_COMPLETE;
        complete.gameplay = None;
        assert_eq!(observe(complete).satisfied.0, 0b01);
        complete.level = 21;
        assert_eq!(observe(complete).satisfied.0, 0b11);
    }

    #[test]
    fn vocabulary_presses_only_a_with_taps_and_holds() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut previous = None;
        let (mut taps, mut holds) = (0, 0);
        for _ in 0..1_000 {
            let chord = Famidash::<0>::CHORDS
                .draw(&mut rand, previous.as_ref())
                .unwrap();
            previous = Some(chord);
            assert_eq!(chord.buttons & !A_BUTTON, 0);
            match chord.bounded_hold_frames() {
                1..=4 => taps += 1,
                6..=40 => holds += 1,
                other => panic!("hold {other} is outside both strata"),
            }
        }
        assert!(taps > 300 && holds > 300);
    }
}
