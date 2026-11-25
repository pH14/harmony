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
    let largest =
        usize::from(state.missiles.min(state.missiles_needed())).min(MAX_OFFER_OPTIONS - 1);
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
        out.declare(byte("zebetites_destroyed", state.zebetites_destroyed));
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
    use std::collections::BTreeSet;

    use super::*;
    use crate::{
        metroid::{
            keying::POSITION_STRATEGY,
            plan::{self, PLAN},
            target::{
                BCD_BORROW_HEALTH, JOYPAD_B, JOYPAD_SELECT, MISSILE_DOOR, MISSILES_ARMED,
                TEN_MISSILE_DOOR,
            },
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

    fn stage_goal(name: &str) -> &'static str {
        let stage = Metroid::STRATEGY
            .split("[[stage]]")
            .find(|stage| stage.contains(&format!("name = \"{name}\"")))
            .unwrap();
        let line = stage
            .lines()
            .find_map(|line| line.strip_prefix("goal = "))
            .unwrap();
        line.trim_matches('"')
    }

    fn goal_holds(goal: &str, state: &MetroidMechanicalState) -> bool {
        let lowered = Lowered::new(Metroid, &format!("goal = \"{goal}\"\n")).unwrap();
        let (mut record, _) = lowered.record(state);
        record.set_action_end(true);
        lowered
            .compiled
            .observe(&record, &mut Accumulator::default())
            .goal
    }

    fn deaths(strategy: &str) -> Vec<MetroidMechanicalState> {
        let mut cells = vec![0_u32];
        cells.extend(
            strategy
                .split(|c: char| !c.is_ascii_digit())
                .filter_map(|token| token.parse::<u32>().ok())
                .filter(|cell| (160_000..210_000).contains(cell)),
        );
        cells.sort_unstable();
        cells.dedup();
        let mut deaths = Vec::new();
        for cell in cells {
            for health in [0, BCD_BORROW_HEALTH, 9_700, 9_999] {
                for energy_tanks in 0..=6 {
                    for won in [false, true] {
                        let full = if won { u8::MAX } else { 0 };
                        deaths.push(MetroidMechanicalState {
                            area: u8::try_from(cell / 10_000).unwrap(),
                            map_x: u8::try_from(cell / 100 % 100).unwrap(),
                            map_y: u8::try_from(cell % 100).unwrap(),
                            mode: 3,
                            health,
                            equipment: full,
                            missiles: full,
                            missile_capacity: full,
                            energy_tanks,
                            zebetites_destroyed: if won { 5 } else { 0 },
                            bosses: if won { 2 } else { 0 },
                            statues: if won { 2 } else { 0 },
                            mother_brain_defeated: won,
                            ending: won,
                            kraid_defeated: won,
                            ..MetroidMechanicalState::default()
                        });
                    }
                }
            }
        }
        deaths
    }

    fn goals(
        lowered: &Lowered<Metroid>,
        state: &MetroidMechanicalState,
        action_end: bool,
    ) -> (bool, Vec<bool>) {
        let (mut record, _) = lowered.record(state);
        record.set_action_end(action_end);
        let header = lowered
            .compiled
            .observe(&record, &mut Accumulator::default())
            .goal;
        let stages = lowered
            .compiled
            .stage_goals(&record, &Accumulator::default());
        (header, stages)
    }

    #[test]
    fn no_metroid_goal_holds_on_a_dead_state() {
        let mut holding = BTreeSet::new();
        for (file, strategy) in [
            ("metroid.toml", Metroid::STRATEGY),
            ("position.toml", POSITION_STRATEGY),
        ] {
            let lowered = Lowered::new(Metroid, strategy).unwrap();
            let names = lowered.compiled.stage_names();
            for death in deaths(strategy) {
                assert!(MetroidTerminalPolicy::default().is_dead(death), "{death:?}");
                for action_end in [false, true] {
                    let (header, stages) = goals(&lowered, &death, action_end);
                    if header {
                        holding.insert(format!("{file} header goal"));
                    }
                    for (name, holds) in names.iter().zip(stages) {
                        if holds {
                            holding.insert(format!("{file} stage {name}"));
                        }
                    }
                }
            }
        }
        assert!(
            holding.is_empty(),
            "goals that hold on a dead state: {holding:?}"
        );
    }

    #[test]
    fn a_kraid_win_short_of_the_energy_target_ends_the_energy_stage_on_any_frame() {
        let lowered = strategy();
        let names = lowered.compiled.stage_names();
        let energy = names
            .iter()
            .position(|stage| *stage == "energy_before_kraid")
            .unwrap();
        let won = MetroidMechanicalState {
            area: KRAID,
            map_x: 9,
            map_y: 29,
            mode: 3,
            health: 795,
            energy_tanks: 1,
            equipment: MORPH_BALL | BOMBS,
            missiles: 5,
            missile_capacity: 5,
            bosses: 1,
            kraid_defeated: true,
            ..MetroidMechanicalState::default()
        };
        let alive = MetroidMechanicalState {
            bosses: 0,
            kraid_defeated: false,
            ..won
        };
        for action_end in [false, true] {
            assert!(goals(&lowered, &won, action_end).1[energy]);
            assert!(!goals(&lowered, &alive, action_end).1[energy]);
        }
    }

    #[test]
    fn the_ridley_fight_follows_his_door_with_no_energy_stage_between() {
        let lowered = strategy();
        let names = lowered.compiled.stage_names();
        let at = |name: &str| names.iter().position(|stage| *stage == name).unwrap();
        assert_eq!(at("ridley_door") + 1, at("ridley"));
    }

    #[test]
    fn the_plan_takes_the_ice_beam_in_norfair_before_refilling_for_tourian() {
        let lowered = strategy();
        let names = lowered.compiled.stage_names();
        let at = |name: &str| names.iter().position(|stage| *stage == name).unwrap();
        assert_eq!(at("norfair_return") + 1, at("ice_beam"));
        assert_eq!(at("ice_beam") + 1, at("missiles_before_tourian"));
        let route = table("to_ice_beam");
        assert_eq!(route[0], 162_211);
        assert_eq!(route.last(), Some(&172_612));
        assert!(walks(&route[..2]));
        assert!(walks(&route[2..]));
        assert_eq!(route[1] % 10_000, route[2] % 10_000);
        let ending = table("to_ending");
        let back: Vec<u32> = route[route.len() - 6..].iter().rev().copied().collect();
        let start = ending.iter().position(|cell| *cell == back[0]).unwrap();
        assert_eq!(ending[start..start + 7], [&back[..], &[173_014]].concat());
        for (index, cell) in route.iter().enumerate() {
            assert!(!route[..index].contains(cell), "{cell}");
        }
        let (credit, bytes) = route_credit(&stage_alone("ice_beam"), &[162_211, 172_914, 160_101]);
        assert_eq!((credit, bytes), (vec![0, 11, 0], 0));
        let goal = stage_goal("ice_beam");
        let held = MetroidMechanicalState {
            equipment: 0x80 | MORPH_BALL | BOMBS,
            ..at_cell(172_612, 760)
        };
        let without = MetroidMechanicalState {
            equipment: MORPH_BALL | BOMBS,
            ..held
        };
        assert!(goal_holds(goal, &held), "{goal}");
        assert!(!goal_holds(goal, &without), "{goal}");
    }

    #[test]
    fn the_way_to_ridleys_door_climbs_east_along_row_30_and_west_along_row_29() {
        let route = table("to_ridley");
        let at = |cell: u32| route.iter().position(|kept| *kept == cell).unwrap();
        for (index, cell) in route.iter().enumerate() {
            assert!(!route[..index].contains(cell), "{cell}");
        }
        assert!(walks(&route[at(202_525)..=at(203_028)]));
        assert!(walks(&route[at(202_030)..=at(203_030)]));
        assert!(walks(&route[at(203_030)..]));
        assert_eq!(at(203_028) + 1, at(202_030));
        assert_eq!(route.last(), Some(&201_829));
        let mut path: Vec<u32> = (24..=30).map(|x| 200_030 + x * 100).collect();
        path.extend((19..=29).rev().map(|x| 200_029 + x * 100));
        let (credit, _) = route_credit(&stage_alone("ridley_door"), &path);
        assert!(
            credit.windows(2).all(|pair| pair[0] < pair[1]),
            "{credit:?}"
        );
        let (credit, _) = route_credit(&stage_alone("ridley_door"), &[202_430, 202_030]);
        assert!(credit[1] < credit[0], "{credit:?}");
    }

    #[test]
    fn missiles_then_energy_refill_before_the_tourian_elevator() {
        let lowered = strategy();
        let names = lowered.compiled.stage_names();
        let at = |name: &str| names.iter().position(|stage| *stage == name).unwrap();
        assert_eq!(
            at("missiles_before_tourian") + 1,
            at("energy_before_tourian")
        );
        assert_eq!(at("energy_before_tourian") + 1, at("tourian_elevator"));
        let full = MetroidMechanicalState {
            area: 0x10,
            map_x: 20,
            map_y: 11,
            x: 68,
            y: 155,
            pose: 1,
            mode: 3,
            health: 3999,
            equipment: 151,
            missiles: 180,
            missile_capacity: 180,
            energy_tanks: 3,
            bosses: 2,
            statues: 1,
            kraid_defeated: true,
            ..MetroidMechanicalState::default()
        };
        let least = MetroidMechanicalState {
            health: 3900,
            ..full
        };
        let short = MetroidMechanicalState {
            health: 3899,
            ..full
        };
        let goal = stage_goal("energy_before_tourian");
        assert!(goal_holds(goal, &full), "{goal}");
        assert!(goal_holds(goal, &least), "{goal}");
        assert!(!goal_holds(goal, &short), "{goal}");
        assert!(goal_holds(stage_goal("missiles_before_tourian"), &full));
    }

    fn stages_alone(names: &[&str]) -> Lowered<Metroid> {
        let stages: String = names
            .iter()
            .map(|name| {
                let stage = Metroid::STRATEGY
                    .split("[[stage]]")
                    .find(|stage| stage.contains(&format!("name = \"{name}\"")))
                    .unwrap();
                format!("[[stage]]{stage}")
            })
            .collect();
        let tables = &Metroid::STRATEGY[Metroid::STRATEGY.find("[define]").unwrap()..];
        Lowered::new(
            Metroid,
            &format!(
                "goal = \"false\"\n{stages}[[stage]]\nname = \"past\"\ngoal = \"false\"\n{tables}"
            ),
        )
        .unwrap()
    }

    fn stage_alone(name: &str) -> Lowered<Metroid> {
        stages_alone(&[name])
    }

    fn at_cell(cell: u32, health: u16) -> MetroidMechanicalState {
        MetroidMechanicalState {
            area: u8::try_from(cell / 10_000).unwrap(),
            map_x: u8::try_from(cell / 100 % 100).unwrap(),
            map_y: u8::try_from(cell % 100).unwrap(),
            mode: 3,
            health,
            ..MetroidMechanicalState::default()
        }
    }

    fn keys(
        lowered: &Lowered<Metroid>,
        states: &[MetroidMechanicalState],
    ) -> (Vec<(String, i64)>, usize) {
        let mut accumulator = Accumulator::default();
        let keys = states
            .iter()
            .map(|state| {
                let (mut record, _) = lowered.record(state);
                record.set_action_end(true);
                let key = lowered
                    .compiled
                    .observe(&record, &mut accumulator)
                    .key
                    .unwrap();
                let view = lowered.compiled.view(key);
                (view.stage.unwrap_or_default(), view.progress[0])
            })
            .collect();
        (keys, accumulator.heap_bytes())
    }

    fn route_credit(lowered: &Lowered<Metroid>, cells: &[u32]) -> (Vec<i64>, usize) {
        let states: Vec<_> = cells.iter().map(|cell| at_cell(*cell, 300)).collect();
        let (keys, bytes) = keys(lowered, &states);
        (keys.into_iter().map(|(_, credit)| credit).collect(), bytes)
    }

    fn table(name: &str) -> Vec<u32> {
        Metroid::STRATEGY
            .split(&format!("\n{name} = ["))
            .nth(1)
            .unwrap()
            .split(']')
            .next()
            .unwrap()
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|token| token.parse().ok())
            .collect()
    }

    fn walks(cells: &[u32]) -> bool {
        cells.windows(2).all(|pair| {
            let (from, to) = (pair[0], pair[1]);
            from / 10_000 == to / 10_000
                && (from / 100 % 100).abs_diff(to / 100 % 100) + (from % 100).abs_diff(to % 100)
                    == 1
        })
    }

    #[test]
    fn after_ridley_the_plan_walks_to_the_refill_rooms_and_out_along_row_30() {
        let lowered = strategy();
        let names = lowered.compiled.stage_names();
        let at = |name: &str| names.iter().position(|stage| *stage == name).unwrap();
        assert_eq!(at("ridley") + 1, at("ridley_refill_rooms"));
        assert_eq!(at("ridley_refill_rooms") + 1, at("energy_after_ridley"));
        assert_eq!(at("energy_after_ridley") + 1, at("ridley_exit"));
        let refill = table("to_ridley_refill");
        let ending = table("to_ending");
        assert_eq!(refill[0], 201_729);
        assert!(walks(&refill));
        assert_eq!(refill[12..15], [202_929, 203_029, 203_030]);
        assert_eq!(*refill.last().unwrap(), 202_730);
        assert_eq!(ending[..11].first(), Some(&202_030));
        assert_eq!(ending[10], 203_030);
        assert!(walks(&ending[..11]));
        assert!(walks(&[ending[10], 203_029]));
        assert!(ending[11..].contains(&203_029));
        assert!(ending[..11].contains(refill.last().unwrap()));
        for route in [&refill, &ending] {
            for (index, cell) in route.iter().enumerate() {
                assert!(!route[..index].contains(cell), "{cell}");
            }
        }
        let before_goal = i64::try_from(refill.len() - 2).unwrap();
        let (credit, bytes) = route_credit(
            &stage_alone("ridley_refill_rooms"),
            &[201_729, 202_529, 160_101, 202_830],
        );
        assert_eq!((credit, bytes), (vec![0, 8, 0, before_goal], 0));
        let refill_then_energy = stages_alone(&["ridley_refill_rooms", "energy_after_ridley"]);
        let named = |name: &str, objective: i64| (name.to_owned(), objective);
        assert_eq!(
            keys(
                &refill_then_energy,
                &[
                    at_cell(202_830, 300),
                    at_cell(202_730, 300),
                    at_cell(202_830, 300),
                    at_cell(202_830, 960),
                ]
            )
            .0,
            [
                named("ridley_refill_rooms", before_goal),
                named("energy_after_ridley", 30),
                named("energy_after_ridley", 30),
                named("past", 0),
            ]
        );
        assert_eq!(
            keys(&refill_then_energy, &[at_cell(202_730, 950)]).0,
            [named("energy_after_ridley", 95)]
        );
        assert_eq!(
            keys(&refill_then_energy, &[at_cell(202_830, 990)]).0,
            [named("ridley_refill_rooms", before_goal)]
        );
        let mut accumulator = Accumulator::default();
        for (cell, action_end) in [(202_730, false), (202_830, true)] {
            let (mut record, _) = refill_then_energy.record(&at_cell(cell, 300));
            record.set_action_end(action_end);
            let observed = refill_then_energy
                .compiled
                .observe(&record, &mut accumulator);
            if action_end {
                let view = refill_then_energy.compiled.view(observed.key.unwrap());
                assert_eq!(view.stage.as_deref(), Some("ridley_refill_rooms"));
            }
        }
        let index = |cell: u32| {
            i64::try_from(ending.iter().position(|kept| *kept == cell).unwrap()).unwrap()
        };
        let (credit, _) = route_credit(
            &stage_alone("ridley_exit"),
            &[202_730, 203_030, 160_101, 203_029],
        );
        assert_eq!(
            credit,
            [
                index(202_730),
                index(203_030),
                index(203_030),
                index(203_029)
            ]
        );
        assert!(index(203_029) > index(203_030));
    }

    #[test]
    fn the_tourian_elevator_stage_scores_only_the_route_cell_it_is_in() {
        let route = table("to_ending");
        let index = route.iter().position(|cell| *cell == 190_304).unwrap() - 2;
        let on = route[index];
        let off = 160_101;
        assert!(!route.contains(&off));
        let below = route[index - 1];
        let index = i64::try_from(index).unwrap();
        assert_eq!(
            route_credit(&stage_alone("tourian_elevator"), &[on, off, below]),
            (vec![index, 0, index - 1], 0)
        );
        let carrying: Vec<&str> = Metroid::STRATEGY
            .split("[[stage]]")
            .filter(|stage| stage.contains("index(cell, to_ending) else none"))
            .map(|stage| {
                stage
                    .lines()
                    .find_map(|line| line.strip_prefix("name = "))
                    .unwrap()
                    .trim_matches('"')
            })
            .collect();
        assert_eq!(
            carrying,
            [
                "ridley_exit",
                "norfair_return",
                "zebetite_door",
                "zebetites",
                "escape"
            ]
        );
        for name in carrying {
            let lowered = stage_alone(name);
            let (credit, _) = route_credit(&lowered, &[on, off, below]);
            let (lower, _) = route_credit(&lowered, &[below]);
            assert!(credit[0] >= index, "{name}");
            assert!(lower[0] < credit[0], "{name}");
            assert_eq!(credit, [credit[0], credit[0], lower[0]], "{name}");
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
        assert_eq!(volley_offer(boss), 0b1110);
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
    fn the_volley_offers_no_more_missiles_than_the_room_target_takes() {
        let stocked = MetroidMechanicalState {
            area: 0x13,
            health: 300,
            mode: 3,
            missiles: 150,
            missile_capacity: 155,
            ..MetroidMechanicalState::default()
        };
        let largest = |state| 63 - u64::leading_zeros(volley_offer(state));
        let red = MetroidMechanicalState {
            missile_flags: MISSILE_DOOR,
            ..stocked
        };
        assert_eq!(largest(red), 5);
        let orange = MetroidMechanicalState {
            missile_flags: MISSILE_DOOR | TEN_MISSILE_DOOR,
            ..stocked
        };
        assert_eq!(largest(orange), 10);
        let columns = MetroidMechanicalState {
            zebetite_hits_left: 13,
            ..stocked
        };
        assert_eq!(largest(columns), 8);
        let damaged = MetroidMechanicalState {
            zebetite_hits_left: 3,
            ..stocked
        };
        assert_eq!(largest(damaged), 3);
        let mother_brain = MetroidMechanicalState {
            boss_health: 128,
            ..stocked
        };
        assert_eq!(largest(mother_brain), 1);
        assert_eq!(
            largest(MetroidMechanicalState {
                zebetite_hits_left: 5,
                ..mother_brain
            }),
            5
        );
        let ridley = MetroidMechanicalState {
            area: 0x14,
            boss_health: 28,
            ..stocked
        };
        assert_eq!(largest(ridley), 7);
        assert_eq!(
            largest(MetroidMechanicalState {
                missiles: 3,
                ..ridley
            }),
            3
        );
        assert_eq!(volley_offer(stocked), 0);
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
