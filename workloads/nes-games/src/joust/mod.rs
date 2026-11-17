// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

use crate::{
    chord::{ChordVocabulary, Tap},
    search::assertion::{Assertion, Number},
    workload::{Game, Moves, Never, Step},
};
use machine::nes::ButtonChord;

pub const TARGET_WAVE: u8 = 50;
pub const START_LIVES: u8 = 4;

const GAME_MODE: usize = 0x00;
const GAME_PHASE: usize = 0x02;
const PLAYER_Y: usize = 0x1e;
const PLAYER_X: usize = 0x1f;
const WAVE_ONES: usize = 0x3a;
const WAVE_TENS: usize = 0x3b;
const VELOCITY_X: usize = 0x4e;
const VELOCITY_Y: usize = 0x52;
const WAVE_ENEMIES: usize = 0x73;
const WAVE_SPAWNED: usize = 0x74;
const WAVE_DEFEATED: usize = 0x75;
const LIVES: usize = 0xe9;
const SCORE_LOW: usize = 0xeb;
const SCORE_MID: usize = 0xec;
const SCORE_HIGH: usize = 0xed;
const OAM: usize = 0x200;
const OAM_SPRITES: usize = 64;
const PLAYING_MODE: u8 = 0x90;
const PLAYING_PHASE: u8 = 0x02;
const GAME_OVER_MODE: u8 = 0x80;
const GAME_OVER_PHASE: u8 = 0x03;
const PLAY_AREA_TOP: u8 = 0x10;
const PLAY_AREA_BOTTOM: u8 = 0xc0;
const RIDER_PALETTE: u8 = 2;
const PLAYER_PALETTE: u8 = 0;
const EGG_PALETTE: u8 = 3;
const EGG_TILE: u8 = 204;
const SPRITES_PER_RIDER: u8 = 4;
const FLIP_HORIZONTAL: u8 = 0x40;
const START: u8 = 0x08;
const FLAP: u8 = 0x01;
const HELD: [u8; 6] = [0x00, 0x01, 0x40, 0x41, 0x80, 0x81];

const WAVE_MARKS: usize = 32;

const WAVES: [&str; WAVE_MARKS] = [
    "wave_1", "wave_2", "wave_3", "wave_4", "wave_5", "wave_6", "wave_7", "wave_8", "wave_9",
    "wave_10", "wave_11", "wave_12", "wave_13", "wave_14", "wave_15", "wave_16", "wave_17",
    "wave_18", "wave_19", "wave_20", "wave_21", "wave_22", "wave_23", "wave_24", "wave_25",
    "wave_26", "wave_27", "wave_28", "wave_29", "wave_30", "wave_31", "wave_32",
];

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct JoustState {
    pub wave: u8,
    pub lives: u8,
    pub score: u32,
    pub x: u8,
    pub y: u8,
    pub velocity_x: i8,
    pub velocity_y: i8,
    pub enemies: u8,
    pub spawned: u8,
    pub defeated: u8,
    pub riders: u8,
    pub eggs: u8,
    pub facing_left: bool,
    pub in_game: bool,
    pub game_over: bool,
}

impl JoustState {
    #[must_use]
    pub fn waves_cleared(self) -> u8 {
        self.wave.saturating_sub(1)
    }

    #[must_use]
    pub fn playing(self) -> bool {
        self.in_game && !self.game_over && self.wave > 0
    }

    #[must_use]
    pub fn victory(self) -> bool {
        self.in_game && !self.game_over && self.wave >= TARGET_WAVE
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Joust;

fn byte(ram: &[u8], address: usize) -> u8 {
    ram.get(address).copied().unwrap_or(0)
}

fn bcd(value: u8) -> u32 {
    u32::from(value >> 4).min(9) * 10 + u32::from(value & 0x0f).min(9)
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

impl Game for Joust {
    type State = JoustState;
    type Move = Never;

    const NAME: &'static str = "joust";
    const STRATEGY: &'static str = include_str!("joust.toml");
    const CHORDS: ChordVocabulary = ChordVocabulary {
        held: &HELD,
        tap: Some(Tap {
            buttons: FLAP,
            odds: 3,
            hold_frames: (1, 2),
        }),
        short_hold: (1, 10),
        long_hold: (12, 40),
    };

    fn setup(&self) -> Vec<ButtonChord> {
        let mut tape = Vec::new();
        let wait = |tape: &mut Vec<ButtonChord>, frames: usize| {
            tape.extend((0..frames).map(|_| ButtonChord::new(0, 1)));
        };
        wait(&mut tape, 200);
        tape.push(ButtonChord::new(START, 3));
        wait(&mut tape, 60);
        tape.push(ButtonChord::new(START, 3));
        wait(&mut tape, 200);
        tape
    }

    fn genesis(&self, state: &JoustState) -> Result<(), String> {
        let valid = state.in_game
            && !state.game_over
            && state.wave == 1
            && state.lives == START_LIVES
            && state.score == 0
            && state.defeated == 0;
        if valid {
            Ok(())
        } else {
            Err(format!(
                "Joust setup did not reach the first wave: {state:?}"
            ))
        }
    }

    fn decode(&self, _previous: Option<&JoustState>, ram: &[u8]) -> JoustState {
        let mut riders = 0u8;
        let mut eggs = 0u8;
        let mut facing_left = false;
        for sprite in 0..OAM_SPRITES {
            let base = OAM + sprite * 4;
            let (y, tile, attributes) = (byte(ram, base), byte(ram, base + 1), byte(ram, base + 2));
            if !(PLAY_AREA_TOP..PLAY_AREA_BOTTOM).contains(&y) {
                continue;
            }
            match attributes & 3 {
                RIDER_PALETTE => riders = riders.saturating_add(1),
                EGG_PALETTE if tile == EGG_TILE => eggs = eggs.saturating_add(1),
                PLAYER_PALETTE => facing_left = attributes & FLIP_HORIZONTAL != 0,
                _ => {}
            }
        }
        let tens = byte(ram, WAVE_TENS);
        let ones = byte(ram, WAVE_ONES);
        let mode = byte(ram, GAME_MODE);
        let phase = byte(ram, GAME_PHASE);
        JoustState {
            wave: if tens > 9 || ones > 9 {
                0
            } else {
                tens * 10 + ones
            },
            lives: byte(ram, LIVES),
            score: bcd(byte(ram, SCORE_HIGH)) * 10_000
                + bcd(byte(ram, SCORE_MID)) * 100
                + bcd(byte(ram, SCORE_LOW)),
            x: byte(ram, PLAYER_X),
            y: byte(ram, PLAYER_Y),
            velocity_x: i8::from_ne_bytes([byte(ram, VELOCITY_X)]),
            velocity_y: i8::from_ne_bytes([byte(ram, VELOCITY_Y)]),
            enemies: byte(ram, WAVE_ENEMIES),
            spawned: byte(ram, WAVE_SPAWNED),
            defeated: byte(ram, WAVE_DEFEATED),
            riders: riders.div_ceil(SPRITES_PER_RIDER),
            eggs,
            facing_left,
            in_game: mode == PLAYING_MODE && phase == PLAYING_PHASE,
            game_over: mode == GAME_OVER_MODE && phase == GAME_OVER_PHASE,
        }
    }

    fn declare(&self, state: &JoustState, out: &mut impl FnMut(Assertion)) {
        let state = *state;
        out(value("wave", i64::from(state.wave), 0, 99));
        out(value(
            "waves_cleared",
            i64::from(state.waves_cleared()),
            0,
            98,
        ));
        out(byte_value("lives", state.lives));
        out(value("score", i64::from(state.score), 0, 999_999));
        out(byte_value("x", state.x));
        out(byte_value("y", state.y));
        out(value("velocity_x", i64::from(state.velocity_x), -128, 127));
        out(value("velocity_y", i64::from(state.velocity_y), -128, 127));
        out(byte_value("enemies", state.enemies));
        out(byte_value("spawned", state.spawned));
        out(byte_value("defeated", state.defeated));
        out(value("riders", i64::from(state.riders), 0, 16));
        out(value("eggs", i64::from(state.eggs), 0, 64));
        out(flag("facing_left", state.facing_left));
        out(flag("in_game", state.in_game));
        out(flag("playing", state.playing()));
        out(Assertion::Sometimes {
            name: "target_wave",
            condition: state.victory(),
        });
        out(Assertion::Sometimes {
            name: "game_over",
            condition: state.game_over,
        });
        for (mark, name) in WAVES.iter().enumerate() {
            out(Assertion::Sometimes {
                name,
                condition: usize::from(state.wave) > mark,
            });
        }
    }

    fn moves(&self, _state: &JoustState) -> Moves<Never> {
        Moves::none()
    }

    fn play(&self, mv: Never, _state: &JoustState) -> Option<Step<Never>> {
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
        workload::{Lowered, NesObservation},
    };
    use machine::nes::WRAM_SIZE;

    const EVENTS: usize = 2;

    fn playing_wram() -> Vec<u8> {
        let mut wram = vec![0; WRAM_SIZE];
        wram[GAME_MODE] = PLAYING_MODE;
        wram[GAME_PHASE] = PLAYING_PHASE;
        wram[WAVE_ONES] = 1;
        wram[LIVES] = START_LIVES;
        wram[PLAYER_X] = 0x50;
        wram[PLAYER_Y] = 0xac;
        wram
    }

    fn sprite(wram: &mut [u8], index: usize, y: u8, tile: u8, attributes: u8, x: u8) {
        wram[OAM + index * 4..OAM + index * 4 + 4].copy_from_slice(&[y, tile, attributes, x]);
    }

    fn decode(wram: &[u8]) -> JoustState {
        Joust.decode(None, wram)
    }

    fn lowered() -> Lowered<Joust> {
        Lowered::new(Joust, Joust::STRATEGY).unwrap()
    }

    fn observe(state: JoustState) -> NesObservation<JoustState> {
        let lowered = lowered();
        lowered.checked_record(&state).unwrap();
        lowered.observe(0, state, &mut Accumulator::default())
    }

    fn key(state: JoustState) -> Option<AssertionKey> {
        observe(state).key
    }

    fn state(x: u8, y: u8) -> JoustState {
        JoustState {
            wave: 3,
            lives: 4,
            x,
            y,
            in_game: true,
            ..JoustState::default()
        }
    }

    #[test]
    fn the_wave_is_two_decimal_digits_and_the_score_is_three_bcd_bytes() {
        let mut wram = playing_wram();
        wram[WAVE_TENS] = 2;
        wram[WAVE_ONES] = 3;
        wram[SCORE_HIGH] = 0x01;
        wram[SCORE_MID] = 0x16;
        wram[SCORE_LOW] = 0x50;
        let state = decode(&wram);
        assert_eq!(state.wave, 23);
        assert_eq!(state.waves_cleared(), 22);
        assert_eq!(state.score, 11_650);
        wram[WAVE_ONES] = 0x0f;
        assert_eq!(decode(&wram).wave, 0);
    }

    #[test]
    fn genesis_requires_the_first_wave_with_a_full_reserve_and_no_score() {
        assert!(Joust.genesis(&decode(&playing_wram())).is_ok());
        for (address, value) in [(SCORE_MID, 5), (GAME_PHASE, 0), (WAVE_ONES, 2)] {
            let mut wram = playing_wram();
            wram[address] = value;
            assert!(Joust.genesis(&decode(&wram)).is_err());
        }
    }

    #[test]
    fn riders_eggs_and_facing_come_from_the_sprite_memory() {
        let mut wram = playing_wram();
        for (index, tile) in (0..4).zip(76..) {
            sprite(
                &mut wram,
                index,
                0x48 + (index as u8 / 2) * 8,
                tile,
                0x22,
                0x70,
            );
        }
        for (index, tile) in (4..8).zip(80..) {
            sprite(
                &mut wram,
                index,
                0x60 + (index as u8 / 2) * 8,
                tile,
                0x62,
                0x90,
            );
        }
        sprite(&mut wram, 8, 0x82, EGG_TILE, 0x23, 0x40);
        sprite(&mut wram, 9, 0xac, 14, 0x60, 0x50);
        sprite(&mut wram, 10, 0xc4, 0xdb, 0x03, 0x68);
        let state = decode(&wram);
        assert_eq!(state.riders, 2);
        assert_eq!(state.eggs, 1);
        assert!(state.facing_left);
    }

    #[test]
    fn setup_tape_presses_only_start_twice() {
        let tape = Joust.setup();
        assert_eq!(
            tape.iter().filter(|chord| chord.buttons == START).count(),
            2
        );
        assert!(tape.iter().all(|chord| chord.buttons & !START == 0));
    }

    #[test]
    fn the_place_is_the_wave_and_a_32_pixel_grid_cell_of_the_rider() {
        let here = key(state(0x50, 0xac)).unwrap();
        let near = key(state(0x5f, 0xbf)).unwrap();
        let right = key(state(0x60, 0xac)).unwrap();
        let above = key(state(0x50, 0x9f)).unwrap();
        assert_eq!(here.place(), near.place());
        assert_ne!(here.place(), right.place());
        assert_ne!(here.place(), above.place());
        let mut later = state(0x50, 0xac);
        later.wave = 4;
        assert_ne!(here.place(), key(later).unwrap().place());
    }

    #[test]
    fn the_tier_is_waves_cleared_then_enemies_defeated_in_the_wave() {
        let plain = key(state(0x50, 0xac)).unwrap();
        let mut hit = state(0x50, 0xac);
        hit.defeated = 2;
        assert!(key(hit).unwrap().progress() > plain.progress());
        let mut next = state(0x50, 0xac);
        next.wave = 4;
        assert!(key(next).unwrap().progress() > key(hit).unwrap().progress());
    }

    #[test]
    fn lives_then_score_are_preferred() {
        let full = key(state(0x50, 0xac)).unwrap();
        let mut hurt = state(0x50, 0xac);
        hurt.lives = 3;
        hurt.score = 9_000;
        let hurt = key(hurt).unwrap();
        assert_eq!(full.place(), hurt.place());
        assert_eq!(full.preference_cmp(0, hurt), Ordering::Greater);
        let mut rich = state(0x50, 0xac);
        rich.score = 500;
        assert_eq!(
            key(rich).unwrap().preference_cmp(0, full),
            Ordering::Greater
        );
    }

    #[test]
    fn game_over_is_rejected_and_absent_menus_are_absent_and_the_target_wave_is_the_goal() {
        let mut over = state(0x50, 0xac);
        over.game_over = true;
        let observed = observe(over);
        assert!(observed.key.is_none() && observed.rejected == Some(0) && !observed.goal);
        let mut menu = state(0x50, 0xac);
        menu.in_game = false;
        let observed = observe(menu);
        assert!(observed.key.is_none() && observed.rejected.is_none() && !observed.goal);
        let mut won = state(0x50, 0xac);
        won.wave = TARGET_WAVE;
        let observed = observe(won);
        assert!(observed.key.is_none() && observed.goal && observed.stops());
        assert!(!observe(state(0x50, 0xac)).stops());
    }

    #[test]
    fn named_progress_marks_every_wave_reached_and_the_ending() {
        let lowered = lowered();
        assert_eq!(lowered.sometimes().len(), WAVE_MARKS + EVENTS);
        let mut later = state(0x50, 0xac);
        later.wave = 5;
        let satisfied = observe(later).satisfied;
        let marks: Vec<&str> = lowered
            .sometimes()
            .iter()
            .enumerate()
            .filter(|(index, _)| satisfied.holds(*index))
            .map(|(_, name)| name.as_str())
            .collect();
        assert_eq!(marks, ["wave_1", "wave_2", "wave_3", "wave_4", "wave_5"]);
    }

    #[test]
    fn vocabulary_draws_steering_masks_and_short_flap_taps() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut previous = None;
        let mut taps = 0;
        for _ in 0..1_000 {
            let chord = Joust::CHORDS.draw(&mut rand, previous.as_ref()).unwrap();
            previous = Some(chord);
            assert!(HELD.contains(&chord.buttons));
            if chord.buttons & FLAP != 0 && chord.hold_frames <= 2 {
                taps += 1;
            }
        }
        assert!(taps > 100, "flap taps: {taps}");
    }
}
