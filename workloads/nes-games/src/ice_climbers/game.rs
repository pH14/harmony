// SPDX-License-Identifier: AGPL-3.0-or-later

use std::ops::Range;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    chord::{ChordVocabulary, LONG_HOLD_FRAMES, NES_PRESSABLE_BUTTON_MASKS, SHORT_HOLD_FRAMES},
    search::assertion::{Assertion, Declarations, Number},
    workload::{Game, Moves, Never, Step},
};
use machine::nes::ButtonChord;

pub const MOUNTAINS: u8 = 32;
pub const FLOORS: u8 = 8;
pub const FLOOR_HEIGHT: u16 = 48;
pub const START_LIVES: u8 = 3;
pub const SCREEN_CROP: u16 = 8;
pub const MAP_PAD: u16 = 194;
pub const BONUS_FLOOR: u8 = 9;
pub const START_X: u8 = 0x38;
pub const START_Y: u8 = 0xc0;
pub const START_CAMERA: u8 = 0x9f;
pub const START_TOP: u16 = 444;
pub const CAMERA_WRAP: u16 = 240;
pub const FLOOR_SCROLL: u16 = 48;
pub const BONUS_BASE: u16 = 384;
pub const WORLD_BOTTOM: u16 = START_TOP + START_Y as u16;
pub const MAX_ALTITUDE: u16 = BONUS_BASE + 255;
pub const JUMP_MARGIN: u16 = 4;

const LIVES: usize = 0x20;
const CAMERA: usize = 0x13;
const LEVEL: usize = 0x59;
const PHASE: usize = 0x5a;
const PLAYER_X: usize = 0x64;
const PLAYER_Y: usize = 0x66;
const AIRBORNE: usize = 0xe0;
const OAM: Range<usize> = 0x200..0x300;
const BLOCK_ROWS: [Range<usize>; 2] = [0x400..0x4c0, 0x500..0x5c0];
const EMPTY_BLOCK: u8 = 0xff;
const HORIZONTAL_FLIP: u8 = 0x40;
const START: u8 = 0x08;
const TITLE_FRAMES: usize = 120;
const WAIT_FRAMES: usize = 780;

const MOUNTAINS_CLEARED: [&str; MOUNTAINS as usize] = [
    "mountain_1_cleared",
    "mountain_2_cleared",
    "mountain_3_cleared",
    "mountain_4_cleared",
    "mountain_5_cleared",
    "mountain_6_cleared",
    "mountain_7_cleared",
    "mountain_8_cleared",
    "mountain_9_cleared",
    "mountain_10_cleared",
    "mountain_11_cleared",
    "mountain_12_cleared",
    "mountain_13_cleared",
    "mountain_14_cleared",
    "mountain_15_cleared",
    "mountain_16_cleared",
    "mountain_17_cleared",
    "mountain_18_cleared",
    "mountain_19_cleared",
    "mountain_20_cleared",
    "mountain_21_cleared",
    "mountain_22_cleared",
    "mountain_23_cleared",
    "mountain_24_cleared",
    "mountain_25_cleared",
    "mountain_26_cleared",
    "mountain_27_cleared",
    "mountain_28_cleared",
    "mountain_29_cleared",
    "mountain_30_cleared",
    "mountain_31_cleared",
    "mountain_32_cleared",
];

const FLOORS_REACHED: [&str; FLOORS as usize] = [
    "floor_1", "floor_2", "floor_3", "floor_4", "floor_5", "floor_6", "floor_7", "floor_8",
];

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct IceClimbersState {
    pub level: u8,
    pub lives: u8,
    pub phase: u8,
    pub x: u8,
    pub y: u8,
    pub camera: u8,
    pub airborne: bool,
    pub holes: u8,
    pub flipped: bool,
    pub altitude: u16,
    pub floor: u8,
}

impl IceClimbersState {
    #[must_use]
    pub fn bonus(self) -> bool {
        self.phase >= BONUS_FLOOR
    }

    #[must_use]
    pub fn scrolled(self) -> u16 {
        let base = (u16::from(START_CAMERA) + CAMERA_WRAP - u16::from(self.camera)) % CAMERA_WRAP;
        if self.phase >= BONUS_FLOOR - 1 && base < FLOOR_SCROLL {
            base + CAMERA_WRAP
        } else {
            base
        }
    }

    #[must_use]
    pub fn world_y(self) -> u16 {
        (START_TOP + u16::from(self.y)).saturating_sub(self.scrolled())
    }

    #[must_use]
    pub fn map_y(self) -> u16 {
        if self.bonus() {
            u16::from(self.y).saturating_sub(SCREEN_CROP - 1)
        } else {
            (self.world_y() + MAP_PAD).saturating_sub(SCREEN_CROP - 1)
        }
    }

    fn climbed(self) -> (u16, u8) {
        let altitude = if self.bonus() {
            BONUS_BASE + 255 - u16::from(self.y)
        } else {
            WORLD_BOTTOM.saturating_sub(self.world_y())
        };
        let footing = if self.airborne && !self.bonus() {
            altitude.saturating_sub(JUMP_MARGIN)
        } else {
            altitude
        };
        let floor = u8::try_from(footing / FLOOR_HEIGHT)
            .unwrap_or(u8::MAX)
            .min(FLOORS);
        (altitude, floor)
    }

    #[must_use]
    pub fn dead(self) -> bool {
        self.lives < START_LIVES
    }

    #[must_use]
    pub fn victory(self) -> bool {
        self.level >= MOUNTAINS
    }

    #[must_use]
    pub fn in_transition(self) -> bool {
        self.phase == 0
    }

    #[must_use]
    pub fn terminal(self) -> bool {
        self.dead() || self.victory()
    }
}

#[derive(Clone, Debug, Default)]
pub struct IceClimbers {
    root: Option<Vec<ButtonChord>>,
}

impl IceClimbers {
    #[must_use]
    pub fn rooted(prefix: Vec<ButtonChord>) -> Self {
        Self { root: Some(prefix) }
    }
}

#[must_use]
pub fn setup_tape() -> Vec<ButtonChord> {
    let mut tape = vec![ButtonChord::new(0, 1); TITLE_FRAMES];
    tape.push(ButtonChord::new(START, 2));
    tape.extend(std::iter::repeat_n(ButtonChord::new(0, 1), WAIT_FRAMES));
    tape
}

fn byte(ram: &[u8], address: usize) -> u8 {
    ram.get(address).copied().unwrap_or(0)
}

fn holes(ram: &[u8]) -> u8 {
    let count = BLOCK_ROWS
        .iter()
        .filter_map(|rows| ram.get(rows.clone()))
        .flatten()
        .filter(|&&block| block == EMPTY_BLOCK)
        .count();
    u8::try_from(count).unwrap_or(u8::MAX)
}

fn flipped(ram: &[u8], x: u8, y: u8) -> bool {
    ram.get(OAM)
        .unwrap_or_default()
        .chunks_exact(4)
        .find(|sprite| sprite[0].abs_diff(y) <= 1 && (x..=x.saturating_add(8)).contains(&sprite[3]))
        .is_some_and(|sprite| sprite[2] & HORIZONTAL_FLIP != 0)
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

impl Game for IceClimbers {
    type State = IceClimbersState;
    type Move = Never;

    const NAME: &'static str = "ice-climbers";
    const STRATEGY: &'static str = include_str!("ice_climbers.toml");
    const CHORDS: ChordVocabulary = ChordVocabulary {
        held: &NES_PRESSABLE_BUTTON_MASKS,
        tap: None,
        short_hold: SHORT_HOLD_FRAMES,
        long_hold: LONG_HOLD_FRAMES,
    };

    fn setup(&self) -> Vec<ButtonChord> {
        setup_tape()
    }

    fn start(&self) -> Vec<ButtonChord> {
        self.root.clone().unwrap_or_default()
    }

    fn genesis_id(&self) -> String {
        match &self.root {
            Some(prefix) => {
                let mut digest = Sha256::new();
                for chord in prefix {
                    digest.update([chord.buttons, chord.hold_frames]);
                }
                format!(
                    "ice-climbers-rooted-v1:prefix-sha256={:x}",
                    digest.finalize()
                )
            }
            None => "ice-climbers-first-mountain-v1".to_owned(),
        }
    }

    fn genesis(&self, state: &IceClimbersState) -> Result<(), String> {
        let state = *state;
        if self.root.is_some() {
            if state.lives == START_LIVES && !state.terminal() && !state.in_transition() {
                return Ok(());
            }
            return Err(format!(
                "Ice Climbers root input did not end in a live climbing state: {state:?}"
            ));
        }
        let valid = state.level == 0
            && state.lives == START_LIVES
            && (state.x, state.y) == (START_X, START_Y)
            && state.camera == START_CAMERA;
        if valid {
            Ok(())
        } else {
            Err(format!(
                "Ice Climbers setup did not reach the first mountain: {state:?}"
            ))
        }
    }

    fn decode(
        &self,
        _: Option<&IceClimbersState>,
        _previous: Option<&IceClimbersState>,
        ram: &[u8],
    ) -> IceClimbersState {
        let (x, y) = (byte(ram, PLAYER_X), byte(ram, PLAYER_Y));
        let mut state = IceClimbersState {
            level: byte(ram, LEVEL),
            lives: byte(ram, LIVES),
            phase: byte(ram, PHASE),
            x,
            y,
            camera: byte(ram, CAMERA),
            airborne: byte(ram, AIRBORNE) != 0,
            holes: holes(ram),
            flipped: flipped(ram, x, y),
            altitude: 0,
            floor: 0,
        };
        (state.altitude, state.floor) = state.climbed();
        state
    }

    fn declare(&self, state: &IceClimbersState, out: &mut impl Declarations) {
        let state = *state;
        out.declare(byte_value("level", state.level));
        out.declare(byte_value("lives", state.lives));
        out.declare(byte_value("phase", state.phase));
        out.declare(byte_value("x", state.x));
        out.declare(byte_value("y", state.y));
        out.declare(byte_value("camera", state.camera));
        out.declare(flag("airborne", state.airborne));
        out.declare(flag("flipped", state.flipped));
        out.declare(byte_value("holes", state.holes));
        out.declare(value(
            "altitude",
            i64::from(state.altitude),
            0,
            i64::from(MAX_ALTITUDE),
        ));
        out.declare(value("floor", i64::from(state.floor), 0, i64::from(FLOORS)));
        out.declare(flag("dead", state.dead()));
        out.declare(Assertion::Sometimes {
            name: "last_mountain",
            condition: state.victory(),
        });
        out.declare(Assertion::Sometimes {
            name: "death",
            condition: state.dead(),
        });
        for (mark, name) in MOUNTAINS_CLEARED.iter().enumerate() {
            out.declare(Assertion::Sometimes {
                name,
                condition: usize::from(state.level) > mark,
            });
        }
        for (mark, name) in FLOORS_REACHED.iter().enumerate() {
            out.declare(Assertion::Sometimes {
                name,
                condition: !state.dead() && usize::from(state.floor) > mark,
            });
        }
    }

    fn moves(&self, _state: &IceClimbersState) -> Moves<Never> {
        Moves::none()
    }

    fn play(&self, mv: Never, _state: &IceClimbersState) -> Option<Step<Never>> {
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

    fn start_wram() -> Vec<u8> {
        let mut wram = vec![0; WRAM_SIZE];
        wram[LIVES] = START_LIVES;
        wram[PHASE] = 1;
        wram[PLAYER_X] = START_X;
        wram[PLAYER_Y] = START_Y;
        wram[CAMERA] = START_CAMERA;
        wram
    }

    fn decode(wram: &[u8]) -> IceClimbersState {
        IceClimbers::default().decode(None, None, wram)
    }

    fn at(x: u8, y: u8) -> IceClimbersState {
        let mut wram = start_wram();
        wram[PLAYER_X] = x;
        wram[PLAYER_Y] = y;
        decode(&wram)
    }

    fn lowered() -> Lowered<IceClimbers> {
        Lowered::new(IceClimbers::default(), IceClimbers::STRATEGY).unwrap()
    }

    fn observe(state: IceClimbersState) -> NesObservation<IceClimbersState> {
        let lowered = lowered();
        lowered.checked_record(&state).unwrap();
        lowered.observe(0, state, &mut Accumulator::default())
    }

    fn key(state: IceClimbersState) -> Option<AssertionKey> {
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

    #[test]
    fn the_start_position_is_altitude_zero_and_each_floor_is_48_pixels() {
        let start = decode(&start_wram());
        assert_eq!((start.altitude, start.floor), (0, 0));
        let mut wram = start_wram();
        wram[PLAYER_Y] = START_Y - 48;
        assert_eq!(decode(&wram).floor, 1);
        wram[PLAYER_Y] = START_Y - 47;
        assert_eq!(decode(&wram).floor, 0);
        wram[PHASE] = 7;
        wram[CAMERA] = 207;
        wram[PLAYER_Y] = 0x60;
        assert_eq!(decode(&wram).altitude, 288);
        wram[PHASE] = 8;
        wram[CAMERA] = START_CAMERA;
        assert_eq!(decode(&wram).altitude, 336);
        wram[PHASE] = BONUS_FLOOR;
        wram[PLAYER_Y] = 0x31;
        let bonus = decode(&wram);
        assert!(bonus.altitude > BONUS_BASE && bonus.floor == FLOORS);
        wram[PLAYER_Y] = 0;
        assert_eq!(decode(&wram).altitude, MAX_ALTITUDE);
    }

    #[test]
    fn altitude_and_floor_stay_in_their_declared_ranges() {
        for phase in [0, 1, BONUS_FLOOR - 1, BONUS_FLOOR, u8::MAX] {
            for camera in 0..=u8::MAX {
                for y in 0..=u8::MAX {
                    for airborne in [false, true] {
                        let state = IceClimbersState {
                            phase,
                            camera,
                            y,
                            airborne,
                            ..IceClimbersState::default()
                        };
                        let (altitude, floor) = state.climbed();
                        assert!(altitude <= MAX_ALTITUDE && floor <= FLOORS);
                    }
                }
            }
        }
    }

    #[test]
    fn the_apex_of_a_jump_does_not_count_as_the_next_floor() {
        let mut wram = start_wram();
        wram[PLAYER_Y] = START_Y - 51;
        assert_eq!(decode(&wram).floor, 1);
        wram[AIRBORNE] = 1;
        let apex = decode(&wram);
        assert!(apex.airborne);
        assert_eq!(apex.floor, 0);
        wram[PLAYER_Y] = START_Y - 48 - JUMP_MARGIN as u8;
        assert_eq!(decode(&wram).floor, 1);
    }

    #[test]
    fn the_hole_count_is_the_empty_cells_of_the_block_map() {
        let mut wram = start_wram();
        assert_eq!(decode(&wram).holes, 0);
        wram[0x452] = EMPTY_BLOCK;
        wram[0x536] = EMPTY_BLOCK;
        wram[0x4c5] = EMPTY_BLOCK;
        assert_eq!(decode(&wram).holes, 2);
        assert_eq!(decode(&[]).holes, 0);
    }

    #[test]
    fn the_flip_is_the_mirrored_sprite_at_the_climber() {
        let mut wram = start_wram();
        wram[OAM.start..OAM.start + 4].copy_from_slice(&[0xc0, 0x5c, 0x00, 0x38]);
        assert!(!decode(&wram).flipped);
        wram[OAM.start..OAM.start + 4].copy_from_slice(&[0xc0, 0x5c, 0x40, 0x40]);
        assert!(decode(&wram).flipped);
        wram[OAM.start..OAM.start + 4].copy_from_slice(&[0x40, 0x5c, 0x40, 0x90]);
        assert!(!decode(&wram).flipped);
    }

    #[test]
    fn genesis_requires_the_first_mountain_with_three_lives() {
        let game = IceClimbers::default();
        assert!(game.genesis(&decode(&start_wram())).is_ok());
        for (address, value) in [(LIVES, 2), (PLAYER_X, 0x40), (CAMERA, 0x10)] {
            let mut wram = start_wram();
            wram[address] = value;
            assert!(game.genesis(&decode(&wram)).is_err());
        }
    }

    #[test]
    fn a_root_input_must_end_alive_and_in_play() {
        let game = IceClimbers::rooted(vec![ButtonChord::new(0x01, 4)]);
        assert!(game.genesis(&decode(&start_wram())).is_ok());
        let mut moved = start_wram();
        moved[PLAYER_X] = 0x80;
        moved[LEVEL] = 3;
        assert!(game.genesis(&decode(&moved)).is_ok());
        for (address, value) in [(LIVES, 2), (PHASE, 0), (LEVEL, MOUNTAINS)] {
            let mut wram = start_wram();
            wram[address] = value;
            assert!(game.genesis(&decode(&wram)).is_err());
        }
        assert_eq!(game.start(), [ButtonChord::new(0x01, 4)]);
        assert!(IceClimbers::default().start().is_empty());
    }

    #[test]
    fn the_genesis_identity_names_the_first_mountain_or_the_root_prefix() {
        assert_eq!(
            IceClimbers::default().genesis_id(),
            "ice-climbers-first-mountain-v1"
        );
        let one = IceClimbers::rooted(vec![ButtonChord::new(0x01, 4)]).genesis_id();
        let two = IceClimbers::rooted(vec![ButtonChord::new(0x01, 5)]).genesis_id();
        assert!(one.starts_with("ice-climbers-rooted-v1:prefix-sha256="));
        assert_ne!(one, two);
    }

    #[test]
    fn a_lost_life_is_rejected_and_the_last_mountain_is_the_goal() {
        let mut dead = at(START_X, START_Y);
        dead.lives = 2;
        let observed = observe(dead);
        assert!(observed.key.is_none() && observed.rejected == Some(0) && !observed.goal);
        assert!(observed.stops());
        assert_eq!(marks(observed.satisfied), ["death"]);
        let mut won = at(START_X, START_Y);
        won.level = MOUNTAINS;
        let observed = observe(won);
        assert!(observed.key.is_none() && observed.rejected.is_none() && observed.goal);
        assert!(observed.stops());
        let mut later = at(START_X, START_Y);
        later.level = MOUNTAINS - 1;
        assert!(!observe(later).stops());
    }

    #[test]
    fn a_transition_has_no_key_and_does_not_stop() {
        let mut wram = start_wram();
        wram[PHASE] = 0;
        let observed = observe(decode(&wram));
        assert!(observed.key.is_none() && !observed.stops());
        assert!(key(at(START_X, START_Y)).is_some());
    }

    #[test]
    fn the_place_is_the_altitude_band_and_an_8_pixel_column() {
        let here = key(at(START_X, START_Y)).unwrap();
        let near = key(at(START_X + 3, START_Y - 2)).unwrap();
        let right = key(at(START_X + 8, START_Y)).unwrap();
        let above = key(at(START_X, START_Y - 20)).unwrap();
        assert_eq!(here.place(), near.place());
        assert_ne!(here.place(), right.place());
        assert_ne!(here.place(), above.place());
        let mut jumping = at(START_X, START_Y);
        jumping.airborne = true;
        assert_ne!(here.place(), key(jumping).unwrap().place());
        let mut phase = at(START_X, START_Y);
        phase.phase = 2;
        assert_ne!(here.place(), key(phase).unwrap().place());
    }

    #[test]
    fn the_tier_is_the_mountain_then_the_floor() {
        let low = key(at(START_X, START_Y)).unwrap();
        let high = key(at(START_X, START_Y - 50)).unwrap();
        assert!(high.progress() > low.progress());
        let mut next = at(START_X, START_Y);
        next.level = 1;
        assert!(key(next).unwrap().progress() > high.progress());
    }

    #[test]
    fn a_broken_block_keeps_its_own_slot_in_the_same_place() {
        let intact = key(at(START_X, START_Y)).unwrap();
        let mut broken = at(START_X, START_Y);
        broken.holes = 1;
        let broken = key(broken).unwrap();
        assert_eq!(intact.place(), broken.place());
        assert_eq!(intact.progress(), broken.progress());
        assert_ne!(intact.identity(), broken.identity());
    }

    #[test]
    fn higher_altitude_is_preferred_within_a_place() {
        let low = key(at(START_X, START_Y)).unwrap();
        let high = key(at(START_X, START_Y - 3)).unwrap();
        assert_eq!(low.place(), high.place());
        assert_eq!(high.preference_cmp(0, low), Ordering::Greater);
        assert_eq!(lowered().compiled.shape().preferences, 1);
        assert_eq!(lowered().compiled.shape().capacity, 1);
    }

    #[test]
    fn named_progress_marks_every_mountain_and_floor() {
        let mut names = vec!["last_mountain".to_owned(), "death".to_owned()];
        names.extend(MOUNTAINS_CLEARED.iter().map(|name| (*name).to_owned()));
        names.extend(FLOORS_REACHED.iter().map(|name| (*name).to_owned()));
        assert_eq!(lowered().sometimes(), names);
        let mut later = at(START_X, START_Y - 100);
        later.level = 2;
        assert_eq!(
            marks(observe(later).satisfied),
            [
                "mountain_1_cleared",
                "mountain_2_cleared",
                "floor_1",
                "floor_2"
            ]
        );
    }

    #[test]
    fn vocabulary_draws_pressable_chords_without_start_or_select() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut previous = None;
        for _ in 0..1_000 {
            let chord = IceClimbers::CHORDS
                .draw(&mut rand, previous.as_ref())
                .unwrap();
            previous = Some(chord);
            assert_eq!(chord.buttons & 0x0c, 0);
            assert!(NES_PRESSABLE_BUTTON_MASKS.contains(&chord.buttons));
        }
    }

    #[test]
    fn setup_tape_presses_only_start() {
        let tape = IceClimbers::default().setup();
        assert_eq!(
            tape.iter().filter(|chord| chord.buttons == START).count(),
            1
        );
        assert!(tape.iter().all(|chord| chord.buttons & !START == 0));
        assert_eq!(tape.len(), TITLE_FRAMES + 1 + WAIT_FRAMES);
    }
}
