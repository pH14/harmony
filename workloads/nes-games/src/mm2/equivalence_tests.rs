// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

use machine::{nes::ButtonChord, quicknes::QuickNesMachine};
use sha2::{Digest, Sha256};

use super::{
    action::{Mm2Action, WeaponSwitch, switch_choice},
    archive::{Mm2ArchiveKey, archive_key},
    game::{MegaMan2, Mm2State},
    target::{
        Mm2MechanicalState, Mm2Snapshot, Mm2Target, decode_state, switch_choices, weapon_menu_row,
    },
};
use crate::{
    search::{
        archive::{ArchiveKey, Input, KeyShape},
        assertion::AssertionKey,
        rand::RomuDuoJrRand,
    },
    target::Target,
    workload::{Game, Lowered, NesSnapshot, NesTarget, Shadow},
};

struct Inputs {
    rom: Vec<u8>,
    core: PathBuf,
    core_sha256: String,
    execution_sha256: String,
}

fn inputs() -> Inputs {
    let rom = std::fs::read(std::env::var_os("HARMONY_MM2_ROM").expect("HARMONY_MM2_ROM"))
        .expect("Mega Man 2 ROM");
    let core = PathBuf::from(std::env::var_os("HARMONY_QUICKNES_CORE").expect("core"));
    let core_sha256 = format!("{:x}", Sha256::digest(std::fs::read(&core).expect("core")));
    let execution_sha256 =
        machine::quicknes::prepared_execution_sha256(&core, &core_sha256).expect("execution");
    Inputs {
        rom,
        core,
        core_sha256,
        execution_sha256,
    }
}

fn machine(inputs: &Inputs) -> QuickNesMachine {
    QuickNesMachine::from_rom_bytes_with_identity(
        &inputs.rom,
        &inputs.core,
        &inputs.core_sha256,
        &inputs.execution_sha256,
    )
    .expect("machine")
}

type Strategic = NesTarget<MegaMan2, QuickNesMachine>;

struct Pair {
    hand: Mm2Target<QuickNesMachine>,
    strategy: Strategic,
    shadow: Arc<Shadow<MegaMan2, Mm2ArchiveKey>>,
}

#[derive(Clone)]
struct Saved {
    hand: Mm2Snapshot,
    strategy: NesSnapshot<Mm2State>,
}

#[derive(Clone, Copy, Debug)]
struct Keys {
    hand: Mm2ArchiveKey,
    compiled: AssertionKey,
    state: Mm2MechanicalState,
}

enum Frame {
    Keyed(Keys),
    Stopped { goal: bool },
}

impl Pair {
    fn new(inputs: &Inputs) -> Self {
        let lowered = Arc::new(Lowered::new(MegaMan2, MegaMan2::STRATEGY).expect("strategy"));
        let mut hand =
            Mm2Target::whole_game_from_machine(machine(inputs), &[]).expect("hand target");
        let shadow = Shadow::new(MegaMan2).expect("shadow");
        hand.set_shadow(shadow.hook());
        let strategy = NesTarget::from_power_on(lowered, machine(inputs)).expect("strategy target");
        assert_eq!(
            hand.genesis_prefix(),
            MegaMan2.setup().as_slice(),
            "the setup tape differs from the hand genesis walk"
        );
        assert_eq!(
            decode_state(strategy.wram()).expect("decode"),
            hand.mechanical_state(),
            "genesis states differ"
        );
        Self {
            hand,
            strategy,
            shadow,
        }
    }

    fn save(&mut self) -> Saved {
        Saved {
            hand: self.hand.snapshot().expect("hand snapshot"),
            strategy: self.strategy.snapshot().expect("strategy snapshot"),
        }
    }

    fn restore(&mut self, saved: &Saved) {
        self.hand.restore(&saved.hand).expect("hand restore");
        self.strategy
            .restore(&saved.strategy)
            .expect("strategy restore");
    }

    fn depth(&self) -> u64 {
        let state = self.hand.mechanical_state();
        let progress = self
            .strategy
            .observation()
            .key
            .map_or(0, |key| key.progress);
        u64::from(progress) << 16 | u64::from(state.stage) << 8 | u64::from(state.screen)
    }

    fn frame(&mut self, buttons: u8) -> Frame {
        let chord = ButtonChord::new(buttons, 1);
        self.hand.apply(&Mm2Action::Chord(chord));
        self.strategy.apply(&chord);
        assert_eq!(
            self.hand.exit_kind(),
            self.strategy.exit_kind(),
            "exits differ"
        );
        let observation = self.strategy.observation();
        let hand_state = self.hand.mechanical_state();
        assert_eq!(
            self.hand.is_terminal(),
            observation.stops(),
            "terminal differs at {hand_state:?} against {:?}",
            observation.state
        );
        if observation.stops() {
            assert_eq!(
                self.hand.objective_reached(),
                observation.goal,
                "goal differs"
            );
            return Frame::Stopped {
                goal: observation.goal,
            };
        }
        assert_eq!(
            self.hand.observe().frame_count,
            observation.frame_count,
            "frame counts differ at {hand_state:?}"
        );
        let state = &observation.state;
        assert_eq!(
            (
                hand_state.stage,
                hand_state.enemy_damage,
                hand_state.boss_intro_frames,
                hand_state.weapon_energies,
            ),
            (
                state.stage,
                state.enemy_damage,
                state.boss_intro_frames,
                state.weapon_energies,
            ),
            "decoded states differ"
        );
        self.hand.shadow(Some(archive_key(hand_state)));
        Frame::Keyed(Keys {
            hand: archive_key(hand_state),
            compiled: observation.key.expect("a keyed strategy state"),
            state: hand_state,
        })
    }

    fn act(&mut self, action: Mm2Action, keys: &mut Vec<Keys>) -> Option<bool> {
        let seen = |frame: Frame, keys: &mut Vec<Keys>| match frame {
            Frame::Keyed(found) => {
                keys.push(found);
                None
            }
            Frame::Stopped { goal } => Some(goal),
        };
        match action {
            Mm2Action::Chord(chord) => {
                for _ in 0..chord.bounded_hold_frames() {
                    let before = self.hand.observe().frame_count;
                    if let Some(goal) = seen(self.frame(chord.buttons), keys) {
                        return Some(goal);
                    }
                    if self.hand.observe().frame_count > before + 1 {
                        break;
                    }
                }
            }
            Mm2Action::Switch(word) => {
                let wram = *self.strategy.wram();
                let mut ran = false;
                if let Some(weapon) = switch_choice(&switch_choices(&wram).expect("choices"), word)
                {
                    let mut switch = WeaponSwitch::to_row(weapon_menu_row(weapon));
                    let mut state = decode_state(&wram).expect("decode");
                    while let Some(chord) = switch.next(state) {
                        ran = true;
                        if let Some(goal) = seen(self.frame(chord.buttons), keys) {
                            return Some(goal);
                        }
                        state = decode_state(self.strategy.wram()).expect("decode");
                    }
                }
                if !ran {
                    for _ in 0..super::action::SWITCH_WAIT_FRAMES {
                        if let Some(goal) = seen(self.frame(0), keys) {
                            return Some(goal);
                        }
                    }
                }
            }
        }
        None
    }
}

struct Replayer {
    reference: Mm2Target<QuickNesMachine>,
    genesis: Mm2Snapshot,
    shadow: Arc<Shadow<MegaMan2, Mm2ArchiveKey>>,
    arrivals: usize,
}

struct Replayed {
    stop: Option<(usize, bool)>,
    resyncs: usize,
    differing_keys: usize,
}

impl Replayer {
    fn new(inputs: &Inputs) -> Self {
        let mut reference =
            Mm2Target::whole_game_from_machine(machine(inputs), &[]).expect("reference target");
        let shadow = Shadow::new(MegaMan2).expect("shadow");
        reference.set_shadow(shadow.hook());
        let genesis = reference.snapshot().expect("reference snapshot");
        Self {
            reference,
            genesis,
            shadow,
            arrivals: 0,
        }
    }

    fn replay(
        &mut self,
        pair: &mut Pair,
        actions: &[Mm2Action],
        keys: &mut Vec<Keys>,
        mut after: impl FnMut(usize, &mut Pair),
    ) -> Replayed {
        self.reference
            .restore(&self.genesis)
            .expect("reference restore");
        let mut replayed = Replayed {
            stop: None,
            resyncs: 0,
            differing_keys: 0,
        };
        for (index, action) in actions.iter().enumerate() {
            self.reference.apply(action);
            if self.reference.observe().arrived {
                self.arrivals += 1;
            }
            self.reference
                .shadow(Some(archive_key(self.reference.mechanical_state())));
            if let Some(goal) = pair.act(*action, keys) {
                replayed.stop = Some((index, goal));
                break;
            }
            let frames = self.reference.observe().frame_count;
            if pair.hand.observe().frame_count != frames {
                let truncated = (
                    archive_key(pair.hand.mechanical_state()),
                    pair.strategy.observation().key,
                );
                let saved = self.reference.snapshot().expect("reference snapshot");
                pair.hand.restore(&saved).expect("hand resync");
                let strategy = pair
                    .strategy
                    .snapshot_of_state(&saved.emulator_state().to_vec(), frames)
                    .expect("strategy resync");
                pair.strategy.restore(&strategy).expect("strategy restore");
                replayed.resyncs += 1;
                if truncated
                    != (
                        archive_key(pair.hand.mechanical_state()),
                        pair.strategy.observation().key,
                    )
                {
                    replayed.differing_keys += 1;
                }
            }
            after(index, pair);
        }
        replayed
    }
}

fn order_by<T: Copy + std::fmt::Debug>(
    items: &mut [T],
    hand: impl Fn(&T, &T) -> Ordering,
    compiled: impl Fn(&T, &T) -> Ordering,
    what: &str,
) {
    items.sort_by(&hand);
    for pair in items.windows(2) {
        assert_eq!(
            hand(&pair[0], &pair[1]),
            compiled(&pair[0], &pair[1]),
            "{what} order differs for {:?} and {:?}",
            pair[0],
            pair[1]
        );
    }
}

fn agree(keys: &[Keys], shape: KeyShape) -> usize {
    assert_eq!(KeyShape::of::<Mm2ArchiveKey>(), shape, "key shapes differ");
    let mut pairs: Vec<(Mm2ArchiveKey, AssertionKey)> = keys
        .iter()
        .map(|keys| (keys.hand, keys.compiled))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    order_by(
        &mut pairs,
        |a, b| a.0.progress().cmp(&b.0.progress()),
        |a, b| a.1.progress().cmp(&b.1.progress()),
        "progress",
    );
    order_by(
        &mut pairs,
        |a, b| (a.0.progress(), a.0.place()).cmp(&(b.0.progress(), b.0.place())),
        |a, b| (a.1.progress(), a.1.place()).cmp(&(b.1.progress(), b.1.place())),
        "cell",
    );
    order_by(
        &mut pairs,
        |a, b| {
            (a.0.progress(), a.0.place(), a.0.identity()).cmp(&(
                b.0.progress(),
                b.0.place(),
                b.0.identity(),
            ))
        },
        |a, b| {
            (a.1.progress(), a.1.place(), a.1.identity()).cmp(&(
                b.1.progress(),
                b.1.place(),
                b.1.identity(),
            ))
        },
        "slot",
    );
    for preference in 0..2 {
        order_by(
            &mut pairs,
            |a, b| a.0.preference_cmp(preference, b.0),
            |a, b| a.1.preference_cmp(preference, b.1),
            &format!("preference {preference}"),
        );
    }
    pairs.len()
}

type Balance = (super::target::Mm2Tier, [u8; 11], u8);

fn full_balance(state: &Mm2MechanicalState) -> Balance {
    let mut energies = state.weapon_energies;
    energies.sort_unstable();
    (state.tier(), energies, state.health)
}

#[derive(Debug, Default)]
struct Merged {
    pairs: u64,
    samples: u64,
}

fn merged<K: Ord>(groups: BTreeMap<K, BTreeMap<Balance, u64>>) -> Merged {
    let mut merged = Merged::default();
    for olds in groups.values() {
        if olds.len() < 2 {
            continue;
        }
        let total: u64 = olds.values().sum();
        let same: u64 = olds.values().map(|count| count * count).sum();
        merged.pairs += (total * total - same) / 2;
        merged.samples += total;
    }
    merged
}

fn inverted<K: Ord>(groups: &BTreeMap<K, BTreeMap<Balance, u64>>) -> Merged {
    let mut inverted = Merged::default();
    for olds in groups.values() {
        let olds: Vec<_> = olds.iter().collect();
        let mut touched = BTreeSet::new();
        for (left, (old_left, count_left)) in olds.iter().enumerate() {
            for (right, (old_right, count_right)) in olds.iter().enumerate().skip(left + 1) {
                if old_left.2 != old_right.2
                    && old_left.cmp(old_right) != old_left.2.cmp(&old_right.2)
                {
                    inverted.pairs += *count_left * *count_right;
                    touched.insert(left);
                    touched.insert(right);
                }
            }
        }
        inverted.samples += touched.iter().map(|index| *olds[*index].1).sum::<u64>();
    }
    inverted
}

fn census(label: &str, keys: &[Keys]) {
    let mut global: BTreeMap<_, BTreeMap<Balance, u64>> = BTreeMap::new();
    let mut slots: BTreeMap<_, BTreeMap<Balance, u64>> = BTreeMap::new();
    let mut over = BTreeMap::<&str, u64>::new();
    let mut lowest: BTreeMap<_, BTreeMap<Balance, u64>> = BTreeMap::new();
    let mut slot_lowest: BTreeMap<_, BTreeMap<Balance, u64>> = BTreeMap::new();
    for found in keys {
        let state = &found.state;
        let old = full_balance(state);
        let new = (
            found.hand.tier,
            found.hand.lowest_energies,
            found.hand.health,
        );
        *global.entry(new).or_default().entry(old).or_default() += 1;
        *slots
            .entry((
                found.hand.progress(),
                found.hand.place(),
                found.hand.identity(),
                new,
            ))
            .or_default()
            .entry(old)
            .or_default() += 1;
        *lowest
            .entry((found.hand.tier, found.hand.lowest_energies))
            .or_default()
            .entry(old)
            .or_default() += 1;
        *slot_lowest
            .entry((
                found.hand.progress(),
                found.hand.place(),
                found.hand.identity(),
                found.hand.tier,
                found.hand.lowest_energies,
            ))
            .or_default()
            .entry(old)
            .or_default() += 1;
        for (name, exceeds) in [
            ("platforms > 7", state.platforms > 7),
            ("stage > 13", state.stage > 13),
            ("boss intro step > 255", state.boss_intro_frames / 64 > 255),
        ] {
            if exceeds {
                *over.entry(name).or_default() += 1;
            }
        }
    }
    let distinct_old: BTreeSet<Balance> = keys.iter().map(|k| full_balance(&k.state)).collect();
    eprintln!(
        "mm2 {label} census: {} keyed states, {} distinct full balances, {} distinct three-lowest balances; ties the three-lowest form adds: {:?} across all states, {:?} inside one slot; pairs the three-lowest form inverts: {:?} across all states, {:?} inside one slot; clamp excess {over:?}",
        keys.len(),
        distinct_old.len(),
        global.len(),
        merged(global),
        merged(slots),
        inverted(&lowest),
        inverted(&slot_lowest),
    );
}

const ROOT_SPACING: usize = 50;
const POOL: usize = 512;

fn tape_paths() -> Vec<String> {
    std::env::var("HARMONY_MM2_TAPES")
        .unwrap_or_default()
        .split(':')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect()
}

fn read_tape(path: &str) -> Input<Mm2Action> {
    let bytes = std::fs::read(path).expect("tape");
    serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        let chords: Input<ButtonChord> = serde_json::from_slice(&bytes).expect("tape json");
        Input {
            actions: chords.actions.into_iter().map(Mm2Action::Chord).collect(),
        }
    })
}

fn samples() -> usize {
    std::env::var("HARMONY_MM2_SAMPLES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(20_000)
}

fn seed() -> u64 {
    std::env::var("HARMONY_MM2_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0x6d6d_325f_6571)
}

#[test]
#[ignore = "needs HARMONY_MM2_ROM and HARMONY_QUICKNES_CORE, and seeds from HARMONY_MM2_TAPES"]
fn compiled_keys_match_hand_keys_on_played_states() {
    let inputs = inputs();
    let samples = samples();
    let lowered = Lowered::new(MegaMan2, MegaMan2::STRATEGY).expect("strategy");
    let mut pair = Pair::new(&inputs);
    let genesis = pair.save();
    let mut pool: Vec<(u64, Saved)> = vec![(0, genesis.clone())];
    let mut replayer = Replayer::new(&inputs);
    for path in tape_paths() {
        pair.restore(&genesis);
        let mut discarded = Vec::new();
        replayer.replay(
            &mut pair,
            &read_tape(&path).actions,
            &mut discarded,
            |index, pair| {
                if index % ROOT_SPACING == ROOT_SPACING - 1 {
                    pool.push((pair.depth(), pair.save()));
                }
            },
        );
    }
    let roots = pool.len();
    let seeded = pair.shadow.report().expect("shadow report");
    let mut rand = RomuDuoJrRand::with_seed(seed());
    let mut keys = Vec::new();
    let mut stops = 0usize;
    let mut stages = BTreeMap::<u8, usize>::new();
    while keys.len() < samples {
        let deepest = pool.iter().map(|(depth, _)| *depth).max().unwrap_or(0);
        let leaders: Vec<usize> = (0..pool.len()).filter(|&i| pool[i].0 == deepest).collect();
        let pick = if rand.next_u64().is_multiple_of(4) {
            leaders[usize::try_from(rand.next_u64() % leaders.len() as u64).unwrap()]
        } else {
            usize::try_from(rand.next_u64() % pool.len() as u64).unwrap()
        };
        pair.restore(&pool[pick].1);
        let mut previous = None;
        let mut ended = false;
        let start = keys.len();
        for _ in 0..8 {
            let action = Mm2Action::draw(&mut rand, previous.as_ref()).expect("draw");
            previous = Some(action);
            if pair.act(action, &mut keys).is_some() {
                stops += 1;
                ended = true;
                break;
            }
        }
        for found in &keys[start..] {
            *stages.entry(found.state.stage).or_default() += 1;
        }
        if !ended {
            let saved = pair.save();
            if pool.len() >= roots + POOL {
                let victim = roots + usize::try_from(rand.next_u64() % POOL as u64).unwrap();
                pool.swap_remove(victim);
            }
            pool.push((pair.depth(), saved));
        }
    }
    let distinct = agree(&keys, lowered.compiled.shape());
    let tiers: BTreeSet<_> = keys.iter().map(|k| k.hand.tier).collect();
    eprintln!(
        "mm2 equivalence: {} keyed states ({distinct} distinct key pairs), {stops} stopping rollouts; stages {stages:?}; {} tiers",
        keys.len(),
        tiers.len()
    );
    census("random play", &keys);
    let played = pair.shadow.report().expect("shadow report");
    eprintln!(
        "mm2 random play shadow: while seeding from tapes {} states, disagreements {:?}, unknown restores {}; in total {} states, {} keyed, {} distinct pairs, disagreements {:?}, unknown restores {}",
        seeded.states,
        seeded.disagreements,
        seeded.unknown_restores,
        played.states,
        played.keyed_states,
        played.distinct_pairs,
        played.disagreements,
        played.unknown_restores
    );
    assert_eq!(played.disagreements.total(), 0, "{:?}", played.examples);
    assert_eq!(played.unknown_restores, seeded.unknown_restores);
}

#[test]
#[ignore = "needs HARMONY_MM2_ROM, HARMONY_QUICKNES_CORE and HARMONY_MM2_TAPES"]
fn compiled_keys_match_hand_keys_on_replayed_tapes() {
    let inputs = inputs();
    let lowered = Lowered::new(MegaMan2, MegaMan2::STRATEGY).expect("strategy");
    let mut pair = Pair::new(&inputs);
    let genesis = pair.save();
    let mut replayer = Replayer::new(&inputs);
    let mut keys = Vec::new();
    for path in tape_paths() {
        let tape = read_tape(&path);
        pair.restore(&genesis);
        let start = keys.len();
        let replayed = replayer.replay(&mut pair, &tape.actions, &mut keys, |_, _| {});
        let last = keys.last().map(|k: &Keys| k.hand.tier);
        eprintln!(
            "mm2 tape {path}: {} actions, {} keyed frames, {} resyncs after drives ({} with a different arrival key), stop at {:?}, last tier {last:?}",
            tape.actions.len(),
            keys.len() - start,
            replayed.resyncs,
            replayed.differing_keys,
            replayed.stop,
        );
    }
    let distinct = agree(&keys, lowered.compiled.shape());
    eprintln!(
        "mm2 tape equivalence: {} keyed states ({distinct} distinct key pairs)",
        keys.len()
    );
    census("tape", &keys);
    let report = replayer.shadow.report().expect("shadow report");
    eprintln!(
        "mm2 tape shadow on the faithful replay ({} arrivals): {} states, {} keyed, {} distinct pairs, disagreements {:?}, unknown restores {}",
        replayer.arrivals,
        report.states,
        report.keyed_states,
        report.distinct_pairs,
        report.disagreements,
        report.unknown_restores
    );
    assert_eq!(report.disagreements.total(), 0, "{:?}", report.examples);
    assert_eq!(report.unknown_restores, 0);
}
