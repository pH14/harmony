// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

use crate::{
    chord::{ChordVocabulary, LONG_HOLD_FRAMES, NES_PRESSABLE_BUTTON_MASKS, SHORT_HOLD_FRAMES},
    search::assertion::{Assertion, Declarations, Number},
    workload::{Game, Moves, Never, Step},
};
use machine::nes::ButtonChord;

pub const STAGES_PER_ROUND: u8 = 3;
pub const GOAL_ROUNDS: u8 = 10;
pub const TRACKED_STAGES: usize = GOAL_ROUNDS as usize * STAGES_PER_ROUND as usize;
pub const STAGE_BARRELS: u8 = 1;
pub const STAGE_ELEVATORS: u8 = 3;
pub const STAGE_RIVETS: u8 = 4;
pub const RIVETS: u8 = 8;
pub const STATUS_DEAD: u8 = 0xff;
pub const START_X: u8 = 0x35;
pub const START_Y: u8 = 0xcc;
pub const START_LIVES: u8 = 2;
pub const START_BONUS: u8 = 0x49;

const BONUS: usize = 0x2e;
const STAGE: usize = 0x53;
const ROUND: usize = 0x54;
const LIVES: usize = 0x55;
const DEMO: usize = 0x58;
const MARIO_X: usize = 0x46;
const MARIO_Y: usize = 0x47;
const STATUS: usize = 0x96;
const FACING: usize = 0x202;
const SCORE: usize = 0x20;
const RIVET_FLAGS: usize = 0xc1;
const START: u8 = 0x08;

const CLEARED: [&str; TRACKED_STAGES] = [
    "cleared_1",
    "cleared_2",
    "cleared_3",
    "cleared_4",
    "cleared_5",
    "cleared_6",
    "cleared_7",
    "cleared_8",
    "cleared_9",
    "cleared_10",
    "cleared_11",
    "cleared_12",
    "cleared_13",
    "cleared_14",
    "cleared_15",
    "cleared_16",
    "cleared_17",
    "cleared_18",
    "cleared_19",
    "cleared_20",
    "cleared_21",
    "cleared_22",
    "cleared_23",
    "cleared_24",
    "cleared_25",
    "cleared_26",
    "cleared_27",
    "cleared_28",
    "cleared_29",
    "cleared_30",
];

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DonkeyKongState {
    pub stage: u8,
    pub round: u8,
    pub lives: u8,
    pub demo: u8,
    pub bonus: u8,
    pub status: u8,
    pub facing: u8,
    pub x: u8,
    pub y: u8,
    pub score: u32,
    pub rivets: u8,
}

impl DonkeyKongState {
    #[must_use]
    pub fn stage_index(self) -> u8 {
        match self.stage {
            STAGE_RIVETS => 2,
            STAGE_ELEVATORS => 1,
            _ => 0,
        }
    }

    #[must_use]
    pub fn room(self) -> u8 {
        self.stage_index() + STAGES_PER_ROUND * self.round.min(1)
    }

    #[must_use]
    pub fn stages_cleared(self) -> u8 {
        self.round
            .saturating_mul(STAGES_PER_ROUND)
            .saturating_add(self.stage_index())
    }

    #[must_use]
    pub fn height(self) -> u8 {
        if self.y == 0 {
            0
        } else {
            START_Y.saturating_sub(self.y)
        }
    }

    #[must_use]
    pub fn in_play(self) -> bool {
        self.demo == 0 && self.y != 0 && self.status != 0 && self.status != STATUS_DEAD
    }

    #[must_use]
    pub fn dead(self) -> bool {
        self.status == STATUS_DEAD || self.demo != 0
    }

    #[must_use]
    pub fn victory(self) -> bool {
        self.round >= GOAL_ROUNDS
    }

    #[must_use]
    pub fn terminal(self) -> bool {
        self.dead() || self.victory()
    }

    #[must_use]
    pub fn in_transition(self) -> bool {
        !self.in_play()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DonkeyKong;

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

impl Game for DonkeyKong {
    type State = DonkeyKongState;
    type Move = Never;

    const NAME: &'static str = "donkey-kong";
    const STRATEGY: &'static str = include_str!("donkey_kong.toml");
    const CHORDS: ChordVocabulary = ChordVocabulary {
        held: &NES_PRESSABLE_BUTTON_MASKS,
        tap: None,
        short_hold: SHORT_HOLD_FRAMES,
        long_hold: LONG_HOLD_FRAMES,
    };

    fn setup(&self) -> Vec<ButtonChord> {
        vec![
            ButtonChord::new(0, 100),
            ButtonChord::new(START, 3),
            ButtonChord::new(0, 120),
            ButtonChord::new(0, 120),
            ButtonChord::new(0, 120),
            ButtonChord::new(0, 120),
            ButtonChord::new(0, 60),
        ]
    }

    fn genesis(&self, state: &DonkeyKongState) -> Result<(), String> {
        let state = *state;
        let valid = state.in_play()
            && state.stage == STAGE_BARRELS
            && state.round == 0
            && state.lives == START_LIVES
            && state.bonus == START_BONUS
            && state.score == 0
            && (state.x, state.y) == (START_X, START_Y);
        if valid {
            Ok(())
        } else {
            Err(format!(
                "Donkey Kong setup did not reach the first barrel stage: {state:?}"
            ))
        }
    }

    fn decode(&self, _previous: Option<&DonkeyKongState>, ram: &[u8]) -> DonkeyKongState {
        let stage = byte(ram, STAGE);
        let rivets = if stage == STAGE_RIVETS {
            (0..usize::from(RIVETS))
                .filter(|slot| byte(ram, RIVET_FLAGS + slot) != 0)
                .fold(0, |count, _| count + 1)
        } else {
            0
        };
        DonkeyKongState {
            stage,
            round: byte(ram, ROUND),
            lives: byte(ram, LIVES),
            demo: byte(ram, DEMO),
            bonus: byte(ram, BONUS),
            status: byte(ram, STATUS),
            facing: byte(ram, FACING),
            x: byte(ram, MARIO_X),
            y: byte(ram, MARIO_Y),
            score: bcd(byte(ram, SCORE))
                + bcd(byte(ram, SCORE + 2)) * 100
                + bcd(byte(ram, SCORE + 1)) * 10_000,
            rivets,
        }
    }

    fn declare(&self, state: &DonkeyKongState, out: &mut impl Declarations) {
        let state = *state;
        out.declare(byte_value("stage", state.stage));
        out.declare(byte_value("round", state.round));
        out.declare(byte_value("stages_cleared", state.stages_cleared()));
        out.declare(byte_value("lives", state.lives));
        out.declare(value("bonus", i64::from(bcd(state.bonus)), 0, 99));
        out.declare(value("score", i64::from(state.score), 0, 999_999));
        out.declare(byte_value("status", state.status));
        out.declare(byte_value("x", state.x));
        out.declare(byte_value("y", state.y));
        out.declare(byte_value("height", state.height()));
        out.declare(value(
            "rivets",
            i64::from(state.rivets),
            0,
            i64::from(RIVETS),
        ));
        out.declare(flag("in_play", state.in_play()));
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
                condition: usize::from(state.stages_cleared()) > mark,
            });
        }
    }

    fn moves(&self, _state: &DonkeyKongState) -> Moves<Never> {
        Moves::none()
    }

    fn play(&self, mv: Never, _state: &DonkeyKongState) -> Option<Step<Never>> {
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
        wram[STAGE] = STAGE_BARRELS;
        wram[LIVES] = START_LIVES;
        wram[BONUS] = START_BONUS;
        wram[STATUS] = 1;
        wram[MARIO_X] = START_X;
        wram[MARIO_Y] = START_Y;
        wram
    }

    fn decode(wram: &[u8]) -> DonkeyKongState {
        DonkeyKong.decode(None, wram)
    }

    fn lowered() -> Lowered<DonkeyKong> {
        Lowered::new(DonkeyKong, DonkeyKong::STRATEGY).unwrap()
    }

    fn observe(state: DonkeyKongState) -> NesObservation<DonkeyKongState> {
        let lowered = lowered();
        lowered.checked_record(&state).unwrap();
        lowered.observe(0, state, &mut Accumulator::default())
    }

    fn key(state: DonkeyKongState) -> Option<AssertionKey> {
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

    fn state(x: u8, y: u8) -> DonkeyKongState {
        DonkeyKongState {
            stage: STAGE_BARRELS,
            lives: START_LIVES,
            bonus: START_BONUS,
            status: 1,
            x,
            y,
            ..DonkeyKongState::default()
        }
    }

    #[test]
    fn the_decoder_reads_stage_round_lives_position_and_score() {
        let mut wram = playing_wram();
        wram[SCORE + 2] = 0x13;
        wram[SCORE + 1] = 0x02;
        wram[ROUND] = 1;
        wram[STAGE] = STAGE_ELEVATORS;
        let state = decode(&wram);
        assert_eq!(state.score, 21_300);
        assert_eq!((state.x, state.y), (START_X, START_Y));
        assert_eq!(state.stages_cleared(), 4);
        assert_eq!(state.lives, START_LIVES);
        assert_eq!(decode(&[]), DonkeyKongState::default());
    }

    #[test]
    fn rivets_count_only_on_the_rivet_stage() {
        let mut wram = playing_wram();
        wram[RIVET_FLAGS] = 1;
        wram[RIVET_FLAGS + 5] = 2;
        assert_eq!(decode(&wram).rivets, 0);
        wram[STAGE] = STAGE_RIVETS;
        assert_eq!(decode(&wram).rivets, 2);
    }

    #[test]
    fn genesis_requires_the_first_barrel_stage_at_the_start_position() {
        assert!(DonkeyKong.genesis(&decode(&playing_wram())).is_ok());
        for (address, value) in [(MARIO_X, 0x40), (STATUS, 0), (STAGE, STAGE_ELEVATORS)] {
            let mut wram = playing_wram();
            wram[address] = value;
            assert!(DonkeyKong.genesis(&decode(&wram)).is_err());
        }
    }

    #[test]
    fn a_dead_mario_and_the_attract_demo_are_rejected_and_the_goal_round_is_the_goal() {
        let mut dead = state(START_X, START_Y);
        dead.status = STATUS_DEAD;
        let observed = observe(dead);
        assert!(observed.key.is_none() && observed.rejected == Some(0) && !observed.goal);
        assert!(observed.stops());
        assert_eq!(marks(observed.satisfied), ["death"]);
        let mut demo = state(START_X, START_Y);
        demo.demo = 1;
        assert_eq!(observe(demo).rejected, Some(0));
        let mut later = state(START_X, START_Y);
        later.round = 1;
        assert!(!observe(later).stops());
        let mut won = state(START_X, START_Y);
        won.round = GOAL_ROUNDS;
        let observed = observe(won);
        assert!(observed.key.is_none() && observed.rejected.is_none() && observed.goal);
        assert!(observed.stops());
    }

    #[test]
    fn the_intro_has_no_mario_and_no_key() {
        let mut intro = state(START_X, 0);
        intro.status = 0;
        let observed = observe(intro);
        assert!(observed.key.is_none() && !observed.stops());
        assert!(key(state(START_X, START_Y)).is_some());
    }

    #[test]
    fn the_place_is_the_stage_a_16_pixel_cell_of_mario_and_his_posture() {
        let here = key(state(START_X, START_Y)).unwrap();
        let near = key(state(START_X + 2, START_Y + 2)).unwrap();
        let right = key(state(START_X + 16, START_Y)).unwrap();
        assert_eq!(here.place(), near.place());
        assert_ne!(here.place(), right.place());
        let mut climbing = state(START_X, START_Y);
        climbing.status = 2;
        assert_ne!(here.place(), key(climbing).unwrap().place());
        let mut elsewhere = state(START_X, START_Y);
        elsewhere.stage = STAGE_ELEVATORS;
        assert_ne!(here.place(), key(elsewhere).unwrap().place());
    }

    #[test]
    fn the_tier_orders_stages_then_rivets_then_height_except_on_the_rivet_stage() {
        let low = key(state(START_X, START_Y)).unwrap();
        let high = key(state(START_X, START_Y - 0x40)).unwrap();
        assert!(high.progress() > low.progress());
        let mut next = state(START_X, START_Y);
        next.stage = STAGE_ELEVATORS;
        assert!(key(next).unwrap().progress() > high.progress());
        let mut rivet = state(START_X, START_Y);
        rivet.stage = STAGE_RIVETS;
        let mut one = rivet;
        one.rivets = 1;
        let mut tall = rivet;
        tall.y = START_Y - 0x60;
        assert!(key(one).unwrap().progress() > key(tall).unwrap().progress());
        assert_eq!(
            key(tall).unwrap().progress(),
            key(rivet).unwrap().progress()
        );
    }

    #[test]
    fn more_bonus_then_lives_then_score_is_preferred() {
        let slow = state(START_X, START_Y);
        let mut fast = slow;
        fast.bonus = 0x50;
        let (slow_key, fast_key) = (key(slow).unwrap(), key(fast).unwrap());
        assert_eq!(slow_key.place(), fast_key.place());
        assert_eq!(fast_key.preference_cmp(0, slow_key), Ordering::Greater);
        let mut spare = slow;
        spare.lives = START_LIVES + 1;
        spare.score = 0;
        let mut rich = slow;
        rich.score = 800;
        assert_eq!(
            key(rich).unwrap().preference_cmp(0, slow_key),
            Ordering::Greater
        );
        assert_eq!(
            key(spare).unwrap().preference_cmp(0, key(rich).unwrap()),
            Ordering::Greater
        );
    }

    #[test]
    fn named_progress_marks_every_cleared_stage() {
        let mut names = vec!["victory".to_owned(), "death".to_owned()];
        names.extend(CLEARED.iter().map(|name| (*name).to_owned()));
        assert_eq!(lowered().sometimes(), names);
        let mut later = state(START_X, START_Y);
        later.round = 1;
        later.stage = STAGE_ELEVATORS;
        assert_eq!(
            marks(observe(later).satisfied),
            ["cleared_1", "cleared_2", "cleared_3", "cleared_4"]
        );
    }

    #[test]
    fn vocabulary_draws_pressable_chords_without_start_or_select() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut previous = None;
        for _ in 0..1_000 {
            let chord = DonkeyKong::CHORDS
                .draw(&mut rand, previous.as_ref())
                .unwrap();
            previous = Some(chord);
            assert_eq!(chord.buttons & 0x0c, 0);
            assert!(NES_PRESSABLE_BUTTON_MASKS.contains(&chord.buttons));
        }
    }

    #[test]
    fn setup_tape_presses_only_start() {
        let tape = DonkeyKong.setup();
        assert_eq!(
            tape.iter().filter(|chord| chord.buttons == START).count(),
            1
        );
        assert!(tape.iter().all(|chord| chord.buttons & !START == 0));
    }
}
