// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

use crate::{
    chord::{
        ChordVocabulary, LONG_HOLD_FRAMES, NES_PRESSABLE_BUTTON_MASKS, SHORT_HOLD_FRAMES, Tap,
    },
    search::assertion::{Assertion, Declarations, Number},
    workload::{Game, Moves, Never, Step},
};
use machine::nes::ButtonChord;

pub const TRIFORCE_PIECES: u8 = 8;
pub const ITEMS: [&str; 10] = [
    "raft",
    "ladder",
    "bow",
    "candle",
    "recorder",
    "power_bracelet",
    "magic_key",
    "magic_rod",
    "boomerang",
    "book",
];
pub const SWORDS: [&str; 3] = ["wooden_sword", "white_sword", "magic_sword"];
pub const TRIFORCE: [&str; TRIFORCE_PIECES as usize] = [
    "triforce_1",
    "triforce_2",
    "triforce_3",
    "triforce_4",
    "triforce_5",
    "triforce_6",
    "triforce_7",
    "triforce_8",
];

pub const MODE_PLAY: u8 = 0x05;
pub const MODE_DEATH: u8 = 0x11;
pub const MODE_END_LEVEL: u8 = 0x12;
pub const MODE_ENDING: u8 = 0x13;
pub const TRANSITION_MODES: [u8; 8] = [0x02, 0x03, 0x04, 0x06, 0x07, 0x0a, 0x10, MODE_END_LEVEL];
pub const OVERWORLD: u8 = 0;
pub const START_ROOM: u8 = 0x77;
pub const START_CONTAINERS: u8 = 3;

const LEVEL: usize = 0x10;
const MODE: usize = 0x12;
const LINK_X: usize = 0x70;
const LINK_Y: usize = 0x84;
const LINK_DIRECTION: usize = 0x98;
const ROOM: usize = 0xeb;
const SWORD: usize = 0x657;
const BOMBS: usize = 0x658;
const BOW: usize = 0x65a;
const CANDLE: usize = 0x65b;
const RECORDER: usize = 0x65c;
const MAGIC_ROD: usize = 0x65f;
const RAFT: usize = 0x660;
const BOOK: usize = 0x661;
const LADDER: usize = 0x663;
const MAGIC_KEY: usize = 0x664;
const POWER_BRACELET: usize = 0x665;
const RUPEES: usize = 0x66d;
const KEYS: usize = 0x66e;
const HEARTS: usize = 0x66f;
const PARTIAL_HEART: usize = 0x670;
const TRIFORCE_BITS: usize = 0x671;
const BOOMERANG: usize = 0x674;
const MAGIC_BOOMERANG: usize = 0x675;
const FACING_LEFT: u8 = 0x02;

const START: u8 = 0x08;
const SELECT: u8 = 0x04;
const BUTTON_A: u8 = 0x01;

const MARKS: usize = SWORDS.len() + ITEMS.len() + TRIFORCE.len() + 1;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ZeldaState {
    pub mode: u8,
    pub level: u8,
    pub room: u8,
    pub x: u8,
    pub y: u8,
    pub hearts: u8,
    pub partial_heart: u8,
    pub rupees: u8,
    pub bombs: u8,
    pub keys: u8,
    pub sword: u8,
    pub items: u16,
    pub triforce: u8,
    pub facing_left: bool,
}

impl ZeldaState {
    #[must_use]
    pub fn containers(self) -> u8 {
        (self.hearts >> 4) + 1
    }

    #[must_use]
    pub fn health(self) -> u16 {
        let full = u16::from(self.hearts & 0x0f);
        let partial = match self.partial_heart {
            0 => 0,
            1..=0x7f => 1,
            _ => 2,
        };
        full * 2 + partial
    }

    #[must_use]
    pub fn triforce_pieces(self) -> u8 {
        u8::try_from(self.triforce.count_ones()).unwrap_or(TRIFORCE_PIECES)
    }

    #[must_use]
    pub fn dead(self) -> bool {
        self.mode == MODE_DEATH
    }

    #[must_use]
    pub fn victory(self) -> bool {
        self.mode == MODE_ENDING
    }

    #[must_use]
    pub fn in_transition(self) -> bool {
        TRANSITION_MODES.contains(&self.mode)
    }

    #[must_use]
    pub fn terminal(self) -> bool {
        self.dead() || self.victory()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Zelda;

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

fn mark(index: usize, state: ZeldaState) -> Assertion {
    let swords = SWORDS.len();
    let items = swords + ITEMS.len();
    let pieces = items + TRIFORCE.len();
    let (name, condition) = match index {
        index if index < swords => (SWORDS[index], usize::from(state.sword) > index),
        index if index < items => (
            ITEMS[index - swords],
            state.items & (1 << (index - swords)) != 0,
        ),
        index if index < pieces => (
            TRIFORCE[index - items],
            state.triforce & (1 << (index - items)) != 0,
        ),
        _ => ("ending", state.victory()),
    };
    Assertion::Sometimes { name, condition }
}

impl Game for Zelda {
    type State = ZeldaState;
    type Move = Never;

    const NAME: &'static str = "zelda";
    const STRATEGY: &'static str = include_str!("zelda.toml");
    const CHORDS: ChordVocabulary = ChordVocabulary {
        held: &NES_PRESSABLE_BUTTON_MASKS,
        tap: Some(Tap {
            buttons: START,
            odds: 12,
            hold_frames: (2, 7),
        }),
        short_hold: SHORT_HOLD_FRAMES,
        long_hold: LONG_HOLD_FRAMES,
    };

    fn setup(&self) -> Vec<ButtonChord> {
        let mut tape = Vec::new();
        let wait = |tape: &mut Vec<ButtonChord>, frames: usize| {
            tape.extend((0..frames).map(|_| ButtonChord::new(0, 1)));
        };
        wait(&mut tape, 300);
        tape.push(ButtonChord::new(START, 2));
        wait(&mut tape, 200);
        tape.push(ButtonChord::new(START, 2));
        wait(&mut tape, 100);
        tape.push(ButtonChord::new(BUTTON_A, 2));
        wait(&mut tape, 30);
        for _ in 0..3 {
            tape.push(ButtonChord::new(SELECT, 2));
            wait(&mut tape, 20);
        }
        tape.push(ButtonChord::new(START, 2));
        wait(&mut tape, 150);
        tape.push(ButtonChord::new(START, 2));
        wait(&mut tape, 200);
        tape
    }

    fn genesis(&self, state: &ZeldaState) -> Result<(), String> {
        let state = *state;
        let valid = state.mode == MODE_PLAY
            && state.level == OVERWORLD
            && state.room == START_ROOM
            && state.containers() == START_CONTAINERS
            && state.health() == u16::from(START_CONTAINERS) * 2
            && state.sword == 0
            && state.items == 0
            && state.triforce == 0
            && state.rupees == 0;
        if valid {
            Ok(())
        } else {
            Err(format!(
                "Zelda setup did not reach the first overworld screen: {state:?}"
            ))
        }
    }

    fn decode(&self, _previous: Option<&ZeldaState>, ram: &[u8]) -> ZeldaState {
        let held = |address| byte(ram, address) != 0;
        let items = [
            held(RAFT),
            held(LADDER),
            held(BOW),
            held(CANDLE),
            held(RECORDER),
            held(POWER_BRACELET),
            held(MAGIC_KEY),
            held(MAGIC_ROD),
            held(BOOMERANG) || held(MAGIC_BOOMERANG),
            held(BOOK),
        ]
        .iter()
        .enumerate()
        .fold(0u16, |mask, (bit, &held)| mask | (u16::from(held) << bit));
        ZeldaState {
            mode: byte(ram, MODE),
            level: byte(ram, LEVEL),
            room: byte(ram, ROOM),
            x: byte(ram, LINK_X),
            y: byte(ram, LINK_Y),
            hearts: byte(ram, HEARTS),
            partial_heart: byte(ram, PARTIAL_HEART),
            rupees: byte(ram, RUPEES),
            bombs: byte(ram, BOMBS),
            keys: byte(ram, KEYS),
            sword: byte(ram, SWORD),
            items,
            triforce: byte(ram, TRIFORCE_BITS),
            facing_left: byte(ram, LINK_DIRECTION) & FACING_LEFT != 0,
        }
    }

    fn declare(&self, state: &ZeldaState, out: &mut impl Declarations) {
        let state = *state;
        out.declare(byte_value("mode", state.mode));
        out.declare(byte_value("level", state.level));
        out.declare(byte_value("room", state.room));
        out.declare(byte_value("x", state.x));
        out.declare(byte_value("y", state.y));
        out.declare(value("containers", i64::from(state.containers()), 1, 16));
        out.declare(value("health", i64::from(state.health()), 0, 32));
        out.declare(byte_value("rupees", state.rupees));
        out.declare(byte_value("bombs", state.bombs));
        out.declare(byte_value("keys", state.keys));
        out.declare(byte_value("sword", state.sword));
        out.declare(value("items", i64::from(state.items), 0, 1023));
        out.declare(byte_value("triforce", state.triforce));
        out.declare(flag("dead", state.dead()));
        out.declare(flag("in_transition", state.in_transition()));
        for index in 0..MARKS {
            out.declare(mark(index, state));
        }
    }

    fn moves(&self, _state: &ZeldaState) -> Moves<Never> {
        Moves::none()
    }

    fn play(&self, mv: Never, _state: &ZeldaState) -> Option<Step<Never>> {
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
        wram[ROOM] = START_ROOM;
        wram[LINK_X] = 0x78;
        wram[LINK_Y] = 0x8d;
        wram[HEARTS] = 0x22;
        wram[PARTIAL_HEART] = 0xff;
        wram
    }

    fn decode(wram: &[u8]) -> ZeldaState {
        Zelda.decode(None, wram)
    }

    fn lowered() -> Lowered<Zelda> {
        Lowered::new(Zelda, Zelda::STRATEGY).unwrap()
    }

    fn observe(state: ZeldaState) -> NesObservation<ZeldaState> {
        let lowered = lowered();
        lowered.checked_record(&state).unwrap();
        lowered.observe(0, state, &mut Accumulator::default())
    }

    fn key(state: ZeldaState) -> Option<AssertionKey> {
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

    fn state(x: u8, y: u8) -> ZeldaState {
        ZeldaState {
            mode: MODE_PLAY,
            room: START_ROOM,
            x,
            y,
            hearts: 0x22,
            partial_heart: 0xff,
            ..ZeldaState::default()
        }
    }

    #[test]
    fn health_counts_half_hearts_from_the_low_nibble_and_the_partial_heart() {
        let mut wram = playing_wram();
        assert_eq!(decode(&wram).health(), 6);
        assert_eq!(decode(&wram).containers(), 3);
        wram[PARTIAL_HEART] = 0x7f;
        assert_eq!(decode(&wram).health(), 5);
        wram[HEARTS] = 0x21;
        wram[PARTIAL_HEART] = 0xfe;
        assert_eq!(decode(&wram).health(), 4);
        wram[HEARTS] = 0x20;
        wram[PARTIAL_HEART] = 0;
        assert_eq!(decode(&wram).health(), 0);
    }

    #[test]
    fn the_decoder_counts_triforce_bits_and_sets_one_bit_per_key_item() {
        let mut wram = playing_wram();
        wram[TRIFORCE_BITS] = 0b1010_0001;
        wram[RAFT] = 1;
        wram[MAGIC_BOOMERANG] = 1;
        wram[CANDLE] = 2;
        wram[LINK_DIRECTION] = FACING_LEFT;
        let state = decode(&wram);
        assert_eq!(state.triforce_pieces(), 3);
        assert_eq!(state.items, 0b01_0000_1001);
        assert!(state.facing_left);
    }

    #[test]
    fn a_short_memory_reads_as_zero() {
        assert_eq!(decode(&playing_wram()[..0x100]).hearts, 0);
        assert_eq!(decode(&[]), ZeldaState::default());
    }

    #[test]
    fn genesis_requires_the_first_screen_with_three_full_hearts_and_no_items() {
        assert!(Zelda.genesis(&decode(&playing_wram())).is_ok());
        for (address, value) in [(PARTIAL_HEART, 0x7f), (SWORD, 1), (MODE, 0x0e)] {
            let mut wram = playing_wram();
            wram[address] = value;
            assert!(Zelda.genesis(&decode(&wram)).is_err());
        }
    }

    #[test]
    fn the_death_mode_is_rejected_and_the_ending_mode_is_the_goal() {
        let mut dead = state(0x78, 0x8d);
        dead.mode = MODE_DEATH;
        assert!(dead.dead() && dead.terminal() && !dead.victory());
        let observed = observe(dead);
        assert!(observed.key.is_none() && observed.rejected == Some(0) && !observed.goal);
        assert!(observed.stops());
        let mut won = state(0x78, 0x8d);
        won.mode = MODE_ENDING;
        assert!(won.victory() && won.terminal() && !won.dead());
        let observed = observe(won);
        assert!(observed.key.is_none() && observed.rejected.is_none() && observed.goal);
        assert!(observed.stops());
        let mut cave = state(0x78, 0x8d);
        cave.mode = 0x0b;
        assert!(!cave.terminal() && !observe(cave).stops());
    }

    #[test]
    fn only_the_ending_mode_is_victory_and_a_triforce_pickup_is_not() {
        let mut pickup = state(0x78, 0x8d);
        pickup.level = 1;
        pickup.triforce = 0x01;
        pickup.mode = MODE_END_LEVEL;
        assert_eq!(pickup.triforce_pieces(), 1);
        assert!(!pickup.victory() && !pickup.terminal() && pickup.in_transition());
        assert!(!observe(pickup).goal);
        for mode in (0..=0xff).filter(|mode| *mode != MODE_ENDING) {
            let mut other = pickup;
            other.mode = mode;
            assert!(!other.victory(), "mode {mode:#04x}");
            assert!(!observe(other).goal, "mode {mode:#04x}");
        }
        let mut ending = pickup;
        ending.level = 9;
        ending.triforce = 0xff;
        ending.mode = MODE_ENDING;
        assert!(ending.victory() && ending.terminal());
        assert_eq!(ending.triforce_pieces(), TRIFORCE_PIECES);
        assert!(observe(ending).goal);
    }

    #[test]
    fn play_and_cave_modes_are_stable_and_scrolls_and_loads_are_transitions() {
        for mode in [MODE_PLAY, 0x0b] {
            let mut stable = state(0x78, 0x8d);
            stable.mode = mode;
            assert!(!stable.in_transition());
            assert!(key(stable).is_some());
        }
        for mode in TRANSITION_MODES {
            let mut moving = state(0x78, 0x8d);
            moving.mode = mode;
            assert!(moving.in_transition());
            let observed = observe(moving);
            assert!(observed.key.is_none() && !observed.stops());
        }
    }

    #[test]
    fn setup_tape_uses_only_start_select_and_a() {
        let tape = Zelda.setup();
        assert_eq!(
            tape.iter().filter(|chord| chord.buttons == START).count(),
            4
        );
        assert!(
            tape.iter()
                .all(|chord| chord.buttons & !(START | SELECT | BUTTON_A) == 0)
        );
    }

    #[test]
    fn the_place_is_the_room_and_a_64_pixel_grid_cell_of_link() {
        let here = key(state(0x78, 0x8d)).unwrap();
        let near = key(state(0x7f, 0xbf)).unwrap();
        let right = key(state(0x80, 0x8d)).unwrap();
        let below = key(state(0x78, 0xc0)).unwrap();
        assert_eq!(here.place(), near.place());
        assert_ne!(here.place(), right.place());
        assert_ne!(here.place(), below.place());
        let mut next_room = state(0x78, 0x8d);
        next_room.room = START_ROOM + 1;
        assert_ne!(here.place(), key(next_room).unwrap().place());
        let mut dungeon = state(0x78, 0x8d);
        dungeon.level = 1;
        assert_ne!(here.place(), key(dungeon).unwrap().place());
        let mut cave = state(0x78, 0x8d);
        cave.mode = 0x0b;
        assert_ne!(here.place(), key(cave).unwrap().place());
    }

    #[test]
    fn the_tier_is_the_triforce_count_and_items_stay_out_of_the_key() {
        let plain = state(0x78, 0x8d);
        let mut armed = plain;
        armed.sword = 2;
        armed.items = 0b111;
        armed.bombs = 8;
        armed.keys = 3;
        armed.facing_left = true;
        let (plain_key, armed_key) = (key(plain).unwrap(), key(armed).unwrap());
        assert_eq!(plain_key, armed_key);
        let mut piece = plain;
        piece.triforce = 0b100;
        piece.hearts = 0x20;
        piece.partial_heart = 0x10;
        assert!(key(piece).unwrap().progress() > plain_key.progress());
    }

    #[test]
    fn heart_containers_then_health_then_rupees_are_preferred() {
        let full = key(state(0x78, 0x8d)).unwrap();
        let mut hurt = state(0x78, 0x8d);
        hurt.partial_heart = 0x7f;
        hurt.rupees = 200;
        let hurt = key(hurt).unwrap();
        assert_eq!(full.place(), hurt.place());
        assert_eq!(full.preference_cmp(0, hurt), Ordering::Greater);
        let mut rich = state(0x78, 0x8d);
        rich.rupees = 5;
        assert_eq!(
            key(rich).unwrap().preference_cmp(0, full),
            Ordering::Greater
        );
        let mut container = state(0x78, 0x8d);
        container.hearts = 0x30;
        container.partial_heart = 0x10;
        assert_eq!(
            key(container).unwrap().preference_cmp(0, full),
            Ordering::Greater
        );
    }

    #[test]
    fn a_scroll_or_triforce_pickup_frame_has_no_key_but_keeps_its_milestones() {
        let mut scroll = state(0x78, 0x8d);
        scroll.mode = 0x07;
        assert_eq!(key(scroll), None);
        let mut pickup = state(0x78, 0x8d);
        pickup.mode = MODE_END_LEVEL;
        pickup.level = 1;
        pickup.triforce = 0x01;
        let observed = observe(pickup);
        assert_eq!(observed.key, None);
        assert_eq!(marks(observed.satisfied), ["triforce_1"]);
        let mut cave = state(0x78, 0x8d);
        cave.mode = 0x0b;
        assert_ne!(key(cave), key(state(0x78, 0x8d)));
    }

    #[test]
    fn milestones_keep_every_triforce_piece_item_and_the_best_sword() {
        let mut first = state(0x78, 0x8d);
        first.triforce = 0b1;
        first.sword = 2;
        first.items = 0b10;
        let mut second = state(0x78, 0x8d);
        second.triforce = 0b100;
        second.sword = 1;
        second.items = 0b1;
        let aggregate = Satisfied(observe(first).satisfied.0 | observe(second).satisfied.0);
        assert_eq!(
            marks(aggregate),
            [
                "wooden_sword",
                "white_sword",
                "raft",
                "ladder",
                "triforce_1",
                "triforce_3"
            ]
        );
        let mut magic = second;
        magic.sword = 3;
        assert!(marks(observe(magic).satisfied).contains(&"magic_sword".to_owned()));
        let mut ending = state(0x78, 0x8d);
        ending.mode = MODE_ENDING;
        assert_eq!(marks(observe(ending).satisfied), ["ending"]);
    }

    #[test]
    fn named_progress_names_every_sword_item_triforce_piece_and_the_ending() {
        let names: Vec<&str> = SWORDS
            .iter()
            .chain(&ITEMS)
            .chain(&TRIFORCE)
            .copied()
            .chain(["ending"])
            .collect();
        assert_eq!(lowered().sometimes(), names);
        assert!(marks(observe(state(0x78, 0x8d)).satisfied).is_empty());
    }

    #[test]
    fn vocabulary_draws_pressable_chords_and_short_start_taps() {
        let mut rand = RomuDuoJrRand::with_seed(7);
        let mut previous = None;
        let mut starts = 0;
        for _ in 0..1_000 {
            let chord = Zelda::CHORDS.draw(&mut rand, previous.as_ref()).unwrap();
            previous = Some(chord);
            assert_eq!(chord.buttons & 0x04, 0);
            assert!(NES_PRESSABLE_BUTTON_MASKS.contains(&(chord.buttons & !START)));
            if chord.buttons & START != 0 {
                starts += 1;
                assert!(chord.hold_frames <= 7);
            }
        }
        assert!((40..=140).contains(&starts), "start taps: {starts}");
    }
}
