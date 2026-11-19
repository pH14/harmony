// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::OnceLock;

use machine::nes::{ButtonChord, WRAM_SIZE};

use crate::{
    chord::ChordVocabulary,
    metroid::{
        archive::CHORDS,
        target::{
            MAX_ENERGY_TANKS, MetroidMechanicalState, MetroidTerminalPolicy,
            STARTING_HEALTH_TENTHS, decode_state, follow_frame, health_cap, power_on_walk,
        },
    },
    search::assertion::{Assertion, Declarations, Number},
    workload::{Game, Moves, Never, Step},
};

pub const CARTRIDGE_RAM_SIZE: usize = 0x2000;
pub const MEMORY_SIZE: usize = WRAM_SIZE + CARTRIDGE_RAM_SIZE;

const MAX_HEALTH: i64 = 16_665;
const MORPH_BALL: u8 = 0x10;
const BOMBS: u8 = 0x01;
const KRAID: u8 = 0x12;
const KRAID_DOOR: (u8, u8) = (9, 29);

#[derive(Clone, Copy, Debug, Default)]
pub struct Metroid;

fn value(name: &'static str, value: i64, min: i64, max: i64) -> Assertion {
    Assertion::Value {
        name,
        value: Number::new(value, min, max),
    }
}

fn byte(name: &'static str, value: u8) -> Assertion {
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

fn sometimes(name: &'static str, condition: bool) -> Assertion {
    Assertion::Sometimes { name, condition }
}

#[must_use]
pub fn balance(state: MetroidMechanicalState) -> i64 {
    if state.missile_capacity == 0 {
        return 0;
    }
    let missiles = Ratio::of(u64::from(state.missiles), u64::from(state.missile_capacity));
    let energy = Ratio::of(
        u64::from(state.health),
        u64::from(health_cap(state.energy_tanks)),
    );
    let ratio = missiles.min(energy);
    i64::try_from(balance_table().partition_point(|entry| *entry < ratio)).unwrap_or(i64::MAX)
}

#[must_use]
pub fn balance_max() -> i64 {
    i64::try_from(balance_table().len().saturating_sub(1)).unwrap_or(i64::MAX)
}

#[derive(Clone, Copy, Debug)]
struct Ratio {
    part: u64,
    whole: u64,
}

impl Ratio {
    fn of(part: u64, whole: u64) -> Self {
        Self {
            part: part.min(whole),
            whole,
        }
    }
}

impl PartialEq for Ratio {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for Ratio {}

impl PartialOrd for Ratio {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Ratio {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.part * other.whole).cmp(&(other.part * self.whole))
    }
}

fn balance_table() -> &'static [Ratio] {
    static TABLE: OnceLock<Vec<Ratio>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let missiles = (1..=u8::MAX).flat_map(|capacity| {
            (0..=capacity).map(move |count| Ratio::of(u64::from(count), u64::from(capacity)))
        });
        let energy = (0..=MAX_ENERGY_TANKS).flat_map(|tanks| {
            let cap = health_cap(tanks);
            (0..=cap).map(move |health| Ratio::of(u64::from(health), u64::from(cap)))
        });
        let mut table: Vec<Ratio> = missiles.chain(energy).collect();
        table.sort_unstable();
        table.dedup();
        table
    })
}

fn decode_memory(ram: &[u8]) -> MetroidMechanicalState {
    let mut memory = [0; MEMORY_SIZE];
    let length = ram.len().min(MEMORY_SIZE);
    memory[..length].copy_from_slice(&ram[..length]);
    let (wram, cartridge) = memory.split_at(WRAM_SIZE);
    decode_state(wram, cartridge).unwrap_or_default()
}

impl Game for Metroid {
    type State = MetroidMechanicalState;
    type Move = Never;

    const NAME: &'static str = "metroid";
    const STRATEGY: &'static str = include_str!("metroid.toml");
    const CHORDS: ChordVocabulary = CHORDS;

    fn setup(&self) -> Vec<ButtonChord> {
        power_on_walk()
    }

    fn genesis(&self, state: &MetroidMechanicalState) -> Result<(), String> {
        if state.in_play() && state.health == STARTING_HEALTH_TENTHS {
            Ok(())
        } else {
            Err(format!("Metroid setup did not reach play: {state:?}"))
        }
    }

    fn decode(
        &self,
        previous: Option<&MetroidMechanicalState>,
        ram: &[u8],
    ) -> MetroidMechanicalState {
        let state = decode_memory(ram);
        previous.map_or(state, |prior| follow_frame(state, *prior))
    }

    fn declare(&self, state: &MetroidMechanicalState, out: &mut impl Declarations) {
        let state = *state;
        out.declare(byte("area", state.area));
        out.declare(byte("map_x", state.map_x));
        out.declare(byte("map_y", state.map_y));
        out.declare(byte("x", state.x));
        out.declare(byte("y", state.y));
        out.declare(byte("posture", state.posture()));
        out.declare(byte("door", state.door));
        out.declare(byte("missiles", state.missiles));
        out.declare(byte("missile_capacity", state.missile_capacity));
        out.declare(byte("energy_tanks", state.energy_tanks));
        out.declare(byte("equipment", state.equipment));
        out.declare(value("health", i64::from(state.health), 0, MAX_HEALTH));
        out.declare(value("balance", balance(state), 0, balance_max()));
        out.declare(value(
            "boss_health",
            i64::from(state.boss_health),
            0,
            i64::from(u16::MAX),
        ));
        out.declare(flag("in_play", state.in_play()));
        out.declare(flag(
            "dead",
            MetroidTerminalPolicy::default().is_dead(state),
        ));
        out.declare(sometimes("morph_ball", state.equipment & MORPH_BALL != 0));
        out.declare(sometimes("missiles_found", state.missile_capacity > 0));
        out.declare(sometimes("bombs", state.equipment & BOMBS != 0));
        out.declare(sometimes(
            "kraid_door",
            state.area == KRAID && (state.map_x, state.map_y) == KRAID_DOOR,
        ));
        out.declare(sometimes("kraid_defeated", state.kraid_defeated));
        out.declare(sometimes("ending", state.ending));
    }

    fn moves(&self, _state: &MetroidMechanicalState) -> Moves<Never> {
        Moves::none()
    }

    fn play(&self, mv: Never, _state: &MetroidMechanicalState) -> Option<Step<Never>> {
        match mv {}
    }

    fn move_frames(&self, mv: Never) -> u64 {
        match mv {}
    }
}
