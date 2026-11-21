// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::OnceLock;

use machine::nes::{ButtonChord, SAVE_RAM_BASE, WRAM_SIZE};

use crate::{
    chord::ChordVocabulary,
    metroid::{
        action::{FIRE_ODDS, FIRE_PLANNED_FRAMES, FireMissiles, MetroidReading},
        archive::CHORDS,
        target::{
            CARTRIDGE_FRAME_RANGE, CARTRIDGE_RAM_SIZE, MAX_ENERGY_TANKS, MetroidMechanicalState,
            MetroidTerminalPolicy, STARTING_HEALTH_TENTHS, decode_state, follow_frame, health_cap,
            power_on_walk,
        },
    },
    search::{
        assertion::{Assertion, Declarations, Number},
        choice::{CERTAIN, ChoiceSite},
    },
    workload::{Game, MAX_OFFER_OPTIONS, Moves, Offer, Play, Step},
};

const CARTRIDGE_WINDOW: (u64, u32) = (
    SAVE_RAM_BASE + CARTRIDGE_FRAME_RANGE.start as u64,
    (CARTRIDGE_FRAME_RANGE.end - CARTRIDGE_FRAME_RANGE.start) as u32,
);

const CHORD_LABEL: u16 = 0;
pub const MISSILE_SITE: u16 = 0;
pub const VOLLEYS: [(u16, Play<FireMissiles>); MAX_OFFER_OPTIONS] = volleys();

const fn volleys() -> [(u16, Play<FireMissiles>); MAX_OFFER_OPTIONS] {
    let mut options = [(CHORD_LABEL, Play::Chord); MAX_OFFER_OPTIONS];
    let mut count = 1;
    while count < MAX_OFFER_OPTIONS {
        options[count] = (count as u16, Play::Move(FireMissiles::volley(count as u8)));
        count += 1;
    }
    options
}

#[must_use]
pub fn volley_offer(state: MetroidMechanicalState) -> u64 {
    if !state.in_play() || state.is_dead() || !state.missile_target() {
        return 0;
    }
    let largest = usize::from(state.missiles).min(MAX_OFFER_OPTIONS - 1);
    (1..=largest).fold(0, |offered, count| offered | 1 << count)
}

const MAX_HEALTH: i64 = 16_665;
const MORPH_BALL: u8 = 0x10;
const BOMBS: u8 = 0x01;
const KRAID: u8 = 0x12;
const KRAID_DOOR: (u8, u8) = (9, 29);
const GENESIS_WAIT_FRAMES: usize = 261;

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

fn decode_memory(frame: &[u8]) -> MetroidMechanicalState {
    let mut wram = [0; WRAM_SIZE];
    let mut cartridge = [0; CARTRIDGE_RAM_SIZE];
    let (work, window) = frame.split_at(frame.len().min(WRAM_SIZE));
    wram[..work.len()].copy_from_slice(work);
    let window = &window[..window.len().min(CARTRIDGE_FRAME_RANGE.len())];
    cartridge[CARTRIDGE_FRAME_RANGE.start..][..window.len()].copy_from_slice(window);
    decode_state(&wram, &cartridge).unwrap_or_default()
}

impl Game for Metroid {
    type State = MetroidMechanicalState;
    type Move = FireMissiles;

    const NAME: &'static str = "metroid";
    const STRATEGY: &'static str = include_str!("metroid.toml");
    const CHORDS: ChordVocabulary = CHORDS;
    const MEMORY_WINDOWS: &'static [(u64, u32)] = &[CARTRIDGE_WINDOW];

    fn setup(&self) -> Vec<ButtonChord> {
        let mut tape = power_on_walk();
        tape.extend((0..GENESIS_WAIT_FRAMES).map(|_| ButtonChord::new(0, 1)));
        tape
    }

    fn genesis(&self, state: &MetroidMechanicalState) -> Result<(), String> {
        if state.in_play() && state.health == STARTING_HEALTH_TENTHS {
            Ok(())
        } else {
            Err(format!("Metroid setup did not reach play: {state:?}"))
        }
    }

    fn rooted_genesis(&self, state: &MetroidMechanicalState) -> Result<(), String> {
        if state.in_play_or_item_pickup()
            && !MetroidTerminalPolicy::default().is_dead(*state)
            && !state.ending
        {
            Ok(())
        } else {
            Err(format!(
                "Metroid root input did not end in live play or an item pickup: {state:?}"
            ))
        }
    }

    fn decode(
        &self,
        _parent: Option<&MetroidMechanicalState>,
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
        out.declare(byte(
            "items",
            state
                .items()
                .saturating_add(u8::from(state.mother_brain_defeated)),
        ));
        out.declare(byte("tanks", state.collectibles()));
        out.declare(byte("zebetite_hits_left", state.zebetite_hits_left));
        out.declare(value("health", i64::from(state.health), 0, MAX_HEALTH));
        out.declare(value("balance", balance(state), 0, balance_max()));
        out.declare(value(
            "boss_health",
            i64::from(state.boss_health),
            0,
            i64::from(u16::MAX),
        ));
        out.declare(value(
            "boss_health_ceiling",
            i64::from(state.boss_health_ceiling()),
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
        out.declare(sometimes(
            "ridley_defeated",
            state.bosses > u8::from(state.kraid_defeated),
        ));
        out.declare(sometimes(
            "mother_brain_defeated",
            state.mother_brain_defeated,
        ));
        out.declare(sometimes("ending", state.ending));
    }

    fn moves(&self, state: &MetroidMechanicalState) -> Moves<FireMissiles> {
        match volley_offer(*state) {
            0 => Moves::none(),
            offered => Moves::offer(Offer {
                site: MISSILE_SITE,
                default: CHORD_LABEL,
                options: &VOLLEYS,
                offered,
            }),
        }
    }

    fn move_sites(&self) -> Vec<ChoiceSite> {
        let labels = std::iter::once("chord".to_owned())
            .chain((1..MAX_OFFER_OPTIONS).map(|count| format!("fire_{count}")));
        vec![ChoiceSite::new("missiles", labels).with_contrary(CERTAIN / FIRE_ODDS as u32)]
    }

    fn play(
        &self,
        mut mv: FireMissiles,
        state: &MetroidMechanicalState,
    ) -> Option<Step<FireMissiles>> {
        mv.next(MetroidReading::of(*state)).map(|chord| Step {
            chord,
            rest: Some(mv),
        })
    }

    fn move_frames(&self, mv: FireMissiles) -> u64 {
        mv.frame_limit()
    }

    fn answer_frames(site: u16, label: u16) -> u64 {
        if site == MISSILE_SITE && label != CHORD_LABEL && usize::from(label) < MAX_OFFER_OPTIONS {
            FIRE_PLANNED_FRAMES
        } else {
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        metroid::{
            plan::{self, PLAN},
            target::{JOYPAD_B, JOYPAD_SELECT, MISSILE_DOOR, MISSILES_ARMED},
        },
        search::{assertion::Accumulator, plan::StageProgress, rand::RomuDuoJrRand},
        workload::{Lowered, target::windowed},
    };

    struct Path {
        state: MetroidMechanicalState,
        accumulator: Accumulator,
        hand: Option<StageProgress>,
    }

    fn strategy() -> Lowered<Metroid> {
        Lowered::new(Metroid, Metroid::STRATEGY).unwrap()
    }

    fn interior(lowered: &Lowered<Metroid>, path: &mut Path) {
        let (mut record, _) = lowered.record(&path.state);
        record.set_action_end(false);
        let _ = lowered.compiled.observe(&record, &mut path.accumulator);
    }

    fn step(lowered: &Lowered<Metroid>, path: &mut Path) -> StageProgress {
        let parent = path.hand;
        let hand = plan::locate(path.state, 0, parent);
        let (mut record, _) = lowered.record(&path.state);
        record.set_action_end(true);
        let key = lowered
            .compiled
            .observe(&record, &mut path.accumulator)
            .key
            .unwrap();
        let view = lowered.compiled.view(key);
        path.hand = Some(hand);
        if usize::from(hand.0) + 1 == PLAN.len() {
            let stage = view.stage.as_deref().unwrap();
            assert!(
                PLAN[..PLAN.len() - 1].iter().all(|kept| kept.name != stage),
                "{stage} after Kraid's defeat in {:?}",
                path.state
            );
            return hand;
        }
        assert_eq!(
            view.stage.as_deref(),
            Some(plan::stage_name(hand.0)),
            "{:?}",
            path.state
        );
        assert_eq!(
            view.progress,
            [i64::from(hand.1)],
            "{:?} after {:?}",
            path.state,
            parent
        );
        hand
    }

    fn wander(rand: &mut RomuDuoJrRand, state: &mut MetroidMechanicalState) {
        let mut draw = |range: u64| rand.next_u64() % range;
        match draw(4) {
            0 => state.map_x = state.map_x.saturating_add(1).min(31),
            1 => state.map_x = state.map_x.saturating_sub(1).max(1),
            2 => state.map_y = state.map_y.saturating_add(1).min(30),
            _ => state.map_y = state.map_y.saturating_sub(1).max(3),
        }
        if draw(40) == 0 {
            state.area = if state.area == KRAID { 0x10 } else { KRAID };
        }
        if draw(60) == 0 {
            state.equipment |= [MORPH_BALL, BOMBS][usize::try_from(draw(2)).unwrap()];
        }
        if draw(80) == 0 {
            state.missile_capacity = state.missile_capacity.saturating_add(5);
        }
        if draw(400) == 0 && state.area == KRAID {
            state.kraid_defeated = true;
        }
        let health = i64::from(state.health) + i64::try_from(draw(161)).unwrap() - 80;
        state.health = u16::try_from(health.clamp(1, 999)).unwrap();
    }

    #[test]
    fn the_route_strategy_locates_every_state_as_the_plan_across_restores() {
        let lowered = strategy();
        let mut rand = RomuDuoJrRand::with_seed(0x6d65_7472_6f69);
        let mut carried = [0_u32; 10];
        for _ in 0..40 {
            let mut path = Path {
                state: MetroidMechanicalState {
                    area: 0x10,
                    map_x: 3,
                    map_y: 14,
                    health: 300,
                    mode: 3,
                    ..MetroidMechanicalState::default()
                },
                accumulator: Accumulator::default(),
                hand: None,
            };
            let mut saved = Vec::new();
            for _ in 0..3000 {
                let (stage, objective) = step(&lowered, &mut path);
                let on_route = match PLAN[usize::from(stage)].objective {
                    plan::Objective::Route(route) => {
                        route.contains(&(path.state.area, path.state.map_x, path.state.map_y))
                    }
                    _ => true,
                };
                if !on_route && objective > 0 {
                    carried[usize::from(stage)] += 1;
                }
                if rand.next_u64().is_multiple_of(50) {
                    saved.push((path.state, path.accumulator.clone(), path.hand));
                }
                if !saved.is_empty() && rand.next_u64().is_multiple_of(70) {
                    let index = usize::try_from(rand.next_u64()).unwrap() % saved.len();
                    let (state, accumulator, hand) = saved[index].clone();
                    path = Path {
                        state,
                        accumulator,
                        hand,
                    };
                }
                for _ in 0..rand.next_u64() % 4 {
                    wander(&mut rand, &mut path.state);
                    interior(&lowered, &mut path);
                }
                wander(&mut rand, &mut path.state);
            }
        }
        for (stage, count) in carried.iter().enumerate() {
            if matches!(PLAN[stage].objective, plan::Objective::Route(_)) {
                assert!(*count > 0, "{}", PLAN[stage].name);
            }
        }
    }

    #[test]
    fn a_root_must_end_in_live_play_or_an_item_pickup() {
        let playing = MetroidMechanicalState {
            area: 0x10,
            health: 300,
            mode: 3,
            ..MetroidMechanicalState::default()
        };
        assert!(Metroid.rooted_genesis(&playing).is_ok());
        let pickup = MetroidMechanicalState { mode: 9, ..playing };
        assert!(Metroid.rooted_genesis(&pickup).is_ok());
        for state in [
            MetroidMechanicalState { mode: 8, ..playing },
            MetroidMechanicalState { mode: 1, ..playing },
            MetroidMechanicalState {
                health: 0,
                ..pickup
            },
            MetroidMechanicalState {
                health: 0,
                ..playing
            },
            MetroidMechanicalState {
                ending: true,
                ..playing
            },
        ] {
            assert!(Metroid.rooted_genesis(&state).is_err(), "{state:?}");
        }
    }

    #[test]
    fn the_volley_offer_needs_play_missiles_and_a_missile_target() {
        let door = MetroidMechanicalState {
            area: 0x10,
            health: 300,
            mode: 3,
            missiles: 5,
            missile_capacity: 5,
            missile_flags: MISSILE_DOOR,
            ..MetroidMechanicalState::default()
        };
        assert_eq!(volley_offer(door), 0b11_1110);
        assert_eq!(
            volley_offer(MetroidMechanicalState {
                missiles: 2,
                ..door
            }),
            0b110
        );
        let boss = MetroidMechanicalState {
            missile_flags: 0,
            boss_health: 10,
            ..door
        };
        assert_eq!(volley_offer(boss), 0b11_1110);
        for state in [
            MetroidMechanicalState {
                missile_flags: 0,
                ..door
            },
            MetroidMechanicalState {
                missiles: 0,
                ..door
            },
            MetroidMechanicalState { health: 0, ..door },
            MetroidMechanicalState { mode: 9, ..door },
        ] {
            assert_eq!(volley_offer(state), 0, "{state:?}");
            assert!(Metroid.moves(&state).offer.is_none());
        }
        let offer = Metroid.moves(&door).offer.unwrap();
        assert_eq!(offer.labels(), [0, 1, 2, 3, 4, 5]);
        assert_eq!(Metroid.move_sites()[0].labels.len(), VOLLEYS.len());
        assert_eq!(CHORDS.tap, None);
    }

    #[test]
    fn a_volley_arms_fires_its_count_and_disarms() {
        let mut state = MetroidMechanicalState {
            area: 0x10,
            health: 300,
            mode: 3,
            missiles: 5,
            missile_capacity: 5,
            missile_flags: MISSILE_DOOR,
            ..MetroidMechanicalState::default()
        };
        let Play::Move(volley) = VOLLEYS[2].1 else {
            panic!("label 2 is a volley");
        };
        let mut mv = volley;
        let mut previous = 0_u8;
        let mut frames = 0_u64;
        while let Some(step) = Metroid.play(mv, &state) {
            frames += u64::from(step.chord.hold_frames);
            let pressed = step.chord.buttons & !previous;
            if pressed & JOYPAD_SELECT != 0 {
                state.missile_flags ^= MISSILES_ARMED;
            }
            if state.missiles_armed() && pressed & JOYPAD_B != 0 {
                state.missiles -= 1;
            }
            previous = step.chord.buttons;
            mv = step.rest.unwrap();
        }
        assert_eq!(state.missiles, 3);
        assert!(!state.missiles_armed());
        assert!(frames <= Metroid.move_frames(volley));
    }

    #[test]
    fn a_frame_of_work_ram_and_the_cartridge_window_decodes_as_the_whole_cartridge_ram() {
        let mut rand = RomuDuoJrRand::with_seed(0x7769_6e64_6f77);
        for _ in 0..200 {
            let mut wram = [0_u8; WRAM_SIZE];
            let mut cartridge = [0_u8; CARTRIDGE_RAM_SIZE];
            for byte in wram.iter_mut().chain(cartridge.iter_mut()) {
                *byte = rand.next_u64().to_le_bytes()[0];
            }
            let frame = windowed::<Metroid>(&wram, &cartridge, 0);
            assert_eq!(frame.len(), WRAM_SIZE + CARTRIDGE_FRAME_RANGE.len());
            assert_eq!(
                Metroid.decode(None, None, &frame),
                decode_state(&wram, &cartridge).unwrap()
            );
        }
    }
}
