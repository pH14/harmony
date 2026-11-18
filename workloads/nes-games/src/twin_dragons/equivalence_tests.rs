// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

use machine::{
    Machine, SnapId, StopConditions,
    nes::{self, ButtonChord, WRAM_SIZE},
    quicknes::QuickNesMachine,
};
use sha2::{Digest, Sha256};

use super::{
    archive::{CHORDS, TwinDragonsArchiveKey, archive_key},
    game::{TOUCHED_GOAL, TwinDragons},
    target::{TwinDragonsTarget, decode_state, setup_tape},
};
use crate::{
    search::{
        archive::{ArchiveKey, Input, KeyShape},
        assertion::{Accumulator, AssertionKey},
        campaign::{Evaluation, TargetExecution},
        rand::RomuDuoJrRand,
    },
    target::Target,
    workload::{Game, Lowered, NesWorkload, Shadow},
};

struct Inputs {
    rom: Vec<u8>,
    core: PathBuf,
    core_sha256: String,
    execution_sha256: String,
}

fn inputs() -> Inputs {
    let rom = std::fs::read(
        std::env::var_os("HARMONY_TWIN_DRAGONS_ROM").expect("HARMONY_TWIN_DRAGONS_ROM"),
    )
    .expect("Twin Dragons ROM");
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

fn run(
    machine: &mut QuickNesMachine,
    from: SnapId,
    chords: &[ButtonChord],
) -> Vec<[u8; WRAM_SIZE]> {
    machine
        .branch(from, &nes::reproducer(chords))
        .expect("branch");
    machine.run(StopConditions::default(), None).expect("run");
    machine.frames().to_vec()
}

struct Frame {
    hand: Option<TwinDragonsArchiveKey>,
    hand_terminal: bool,
    compiled: Option<AssertionKey>,
    stops: bool,
    goal: bool,
}

fn frame(lowered: &Lowered<TwinDragons>, wram: &[u8; WRAM_SIZE]) -> Frame {
    let hand = decode_state(wram).expect("hand decode");
    let state = lowered.game.decode(None, wram);
    lowered.checked_record(&state).expect("checked record");
    let observation = lowered.observe(0, state, &mut Accumulator::default());
    Frame {
        hand: archive_key(hand),
        hand_terminal: hand.terminal(),
        compiled: observation.key,
        stops: observation.stops(),
        goal: observation.goal,
    }
}

fn agree(hand: &[TwinDragonsArchiveKey], compiled: &[AssertionKey], shape: KeyShape) -> usize {
    assert_eq!(
        KeyShape::of::<TwinDragonsArchiveKey>(),
        shape,
        "key shapes differ"
    );
    let pairs: Vec<(TwinDragonsArchiveKey, AssertionKey)> = hand
        .iter()
        .copied()
        .zip(compiled.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    for (index, &(h1, c1)) in pairs.iter().enumerate() {
        for &(h2, c2) in &pairs[index..] {
            let hand_cell = (h1.progress(), h1.place()) == (h2.progress(), h2.place());
            let compiled_cell = (c1.progress(), c1.place()) == (c2.progress(), c2.place());
            assert_eq!(hand_cell, compiled_cell, "cell differs for {h1:?} {h2:?}");
            assert_eq!(
                hand_cell && h1.identity() == h2.identity(),
                compiled_cell && c1.identity() == c2.identity(),
                "slot differs for {h1:?} {h2:?}"
            );
            assert_eq!(
                h1.progress().cmp(&h2.progress()),
                c1.progress().cmp(&c2.progress()),
                "progress order differs for {h1:?} {h2:?}"
            );
            assert_eq!(
                (h1.progress(), h1.place()).cmp(&(h2.progress(), h2.place())),
                (c1.progress(), c1.place()).cmp(&(c2.progress(), c2.place())),
                "cell order differs for {h1:?} {h2:?}"
            );
            assert_eq!(
                (h1.place(), h1.identity()).cmp(&(h2.place(), h2.identity())),
                (c1.place(), c1.identity()).cmp(&(c2.place(), c2.identity())),
                "position order differs for {h1:?} {h2:?}"
            );
            assert_eq!(
                h1.preference_cmp(0, h2),
                c1.preference_cmp(0, c2),
                "preference differs for {h1:?} {h2:?}"
            );
        }
    }
    pairs.len()
}

#[test]
fn the_strategy_game_runs_the_hand_setup_and_vocabulary() {
    assert_eq!(TwinDragons.setup(), setup_tape());
    assert_eq!(TwinDragons::CHORDS, CHORDS);
}

fn random_wram(rand: &mut RomuDuoJrRand) -> [u8; WRAM_SIZE] {
    let mut pick = |values: &[u8]| values[usize::try_from(rand.next_u64()).unwrap() % values.len()];
    let mut wram = [0; WRAM_SIZE];
    let level = pick(&[0, 1, 2, 2, 3]);
    wram[0x42] = level;
    wram[0x35] = pick(&[level + 1, level + 1, 0xff, 0xff, 0, 2]);
    wram[0x9e] = pick(&[0, 1, 1, 1, 2, 2, 3]);
    wram[0x04e3] = pick(&[0, 0, 0, 1, 7]);
    wram[0x75] = pick(&[0, 1, 2, 3, 3, 9]);
    wram[0xce] = pick(&[0, 31, 32, 63, 64, 100, 127, 128, 165, 200, 255]);
    wram[0x52] = pick(&[0, 1, 2, 5, 8, 9, 126]);
    wram[0x57] = pick(&[0, 1, 15, 16, 31, 32, 128, 255]);
    wram[0x62] = pick(&[0, 7, 8, 15, 16, 31, 32, 120, 136, 255]);
    wram[0x63] = pick(&[0, 15, 16, 31, 32, 160, 175, 176, 255]);
    wram[0x6b] = pick(&[0, 1]);
    wram[0x74] = pick(&[0, 1]);
    wram[0x76] = pick(&[0, 1]);
    wram
}

#[test]
fn the_shadow_finds_no_disagreement_on_synthetic_memory() {
    let shadow =
        Shadow::<TwinDragons, TwinDragonsArchiveKey>::new(TwinDragons).expect("pure strategy");
    let hook = shadow.hook();
    let lowered = Lowered::new(TwinDragons, TwinDragons::STRATEGY).expect("strategy");
    let mut rand = RomuDuoJrRand::with_seed(0x7464);
    let (mut hand, mut compiled) = (Vec::new(), Vec::new());
    for _ in 0..20_000 {
        let wram = random_wram(&mut rand);
        let frame = frame(&lowered, &wram);
        assert_eq!(
            frame.hand.is_none(),
            frame.compiled.is_none(),
            "absent differs"
        );
        assert_eq!(frame.hand_terminal, frame.stops, "terminal differs");
        if let (Some(h), Some(c)) = (frame.hand, frame.compiled) {
            hand.push(h);
            compiled.push(c);
        }
        hook.observe(frame.hand, &wram);
    }
    agree(&hand, &compiled, lowered.compiled.shape());
    let report = shadow.report().expect("report");
    assert_eq!(report.disagreements.total(), 0, "{:?}", report.examples);
    assert!(report.tiers.len() >= 3 && report.cells.len() > 100);
}

#[test]
#[ignore = "needs HARMONY_TWIN_DRAGONS_ROM and HARMONY_QUICKNES_CORE"]
fn compiled_keys_match_hand_keys_on_played_states() {
    let inputs = inputs();
    let samples: usize = std::env::var("HARMONY_TWIN_DRAGONS_SAMPLES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(20_000);
    let lowered = Lowered::new(TwinDragons, TwinDragons::STRATEGY).expect("strategy");
    let mut machine = machine(&inputs);
    let power_on = machine.snapshot().expect("power on");
    run(&mut machine, power_on, &TwinDragons.setup());
    let genesis = machine.snapshot().expect("genesis");
    let mut pool: Vec<((u32, u64), SnapId)> = vec![((0, 0), genesis)];
    let mut rand = RomuDuoJrRand::with_seed(0x7477_696e_6472);
    let (mut hand, mut compiled) = (Vec::new(), Vec::new());
    let (mut frames, mut absent, mut stops) = (0usize, 0usize, 0usize);
    let mut stages = BTreeMap::<(u8, u8), usize>::new();
    let mut tiers = BTreeSet::new();
    while hand.len() < samples {
        let deepest = pool.iter().map(|(cell, _)| *cell).max().unwrap_or((0, 0));
        let leaders: Vec<usize> = (0..pool.len()).filter(|&i| pool[i].0 == deepest).collect();
        let pick = if rand.next_u64().is_multiple_of(2) {
            leaders[usize::try_from(rand.next_u64() % leaders.len() as u64).unwrap()]
        } else {
            usize::try_from(rand.next_u64() % pool.len() as u64).unwrap()
        };
        let mut previous = None;
        let chords: Vec<ButtonChord> = (0..8)
            .map(|_| {
                let chord = TwinDragons::CHORDS
                    .draw(&mut rand, previous.as_ref())
                    .unwrap();
                previous = Some(chord);
                chord
            })
            .collect();
        let poked = rand.next_u64().is_multiple_of(16).then(|| {
            machine.replay(pool[pick].1).expect("replay");
            machine.poke_wram(TOUCHED_GOAL, 1).expect("poke");
            machine.snapshot().expect("poked snapshot")
        });
        let start = poked.unwrap_or(pool[pick].1);
        let mut ended = false;
        let mut cell = (0, 0);
        for wram in run(&mut machine, start, &chords) {
            frames += 1;
            let frame = frame(&lowered, &wram);
            assert_eq!(
                frame.hand.is_none(),
                frame.compiled.is_none(),
                "absent differs"
            );
            assert_eq!(frame.hand_terminal, frame.stops, "terminal differs");
            if frame.stops {
                stops += 1;
                ended = true;
                break;
            }
            match (frame.hand, frame.compiled) {
                (Some(h), Some(c)) => {
                    *stages.entry((h.level, h.stage)).or_default() += 1;
                    tiers.insert(h.progress());
                    cell = (c.progress, c.place.get());
                    hand.push(h);
                    compiled.push(c);
                }
                _ => absent += 1,
            }
        }
        if !ended {
            let snapshot = machine.snapshot().expect("snapshot");
            if pool.len() >= 512 {
                let victim = 1 + usize::try_from(rand.next_u64() % 511).unwrap();
                let (_, dropped) = pool.swap_remove(victim);
                machine.drop_snapshot(dropped).expect("drop");
            }
            pool.push((cell, snapshot));
        }
        if let Some(poked) = poked {
            machine.drop_snapshot(poked).expect("drop");
        }
    }
    let distinct = agree(&hand, &compiled, lowered.compiled.shape());
    let furthest = hand.iter().map(|h| (h.level, h.x)).max();
    eprintln!(
        "twin dragons equivalence: {} keyed states ({distinct} distinct key pairs), {absent} absent and {stops} stopping frames of {frames}; {} level stages {stages:?}; {} tiers {tiers:?}; furthest 16-pixel column {furthest:?}",
        hand.len(),
        stages.len(),
        tiers.len()
    );
}

#[test]
#[ignore = "needs HARMONY_TWIN_DRAGONS_ROM and HARMONY_QUICKNES_CORE"]
fn interior_observations_of_one_replayed_run() {
    let inputs = inputs();
    let workload = NesWorkload::new(
        TwinDragons,
        &inputs.rom,
        &inputs.core,
        &inputs.core_sha256,
        &inputs.execution_sha256,
    )
    .expect("workload");
    let lowered = Arc::new(Lowered::new(TwinDragons, TwinDragons::STRATEGY).expect("strategy"));
    let mut strategic = workload.new_target().expect("strategy target");
    let mut hand = TwinDragonsTarget::from_power_on(machine(&inputs)).expect("hand target");
    let mut every = machine(&inputs);
    let power_on = every.snapshot().expect("power on");
    run(&mut every, power_on, &TwinDragons.setup());
    let mut at = every.snapshot().expect("genesis");
    let mut rand = RomuDuoJrRand::with_seed(945);
    let mut previous = None;
    let (mut hand_frames, mut strategy_frames) = (BTreeSet::new(), BTreeSet::new());
    let (mut hand_key_frames, mut compiled_key_frames) = (BTreeSet::new(), BTreeSet::new());
    let mut prior = (
        archive_key(hand.mechanical_state()),
        strategic.observation().key,
    );
    let mut frame_count = 0u64;
    for _ in 0..2_000 {
        let chord = TwinDragons::CHORDS
            .draw(&mut rand, previous.as_ref())
            .unwrap();
        previous = Some(chord);
        hand.apply(&chord);
        strategic.apply(&chord);
        hand_frames.extend(
            hand.last_action_observations()
                .iter()
                .map(|o| o.frame_count),
        );
        strategy_frames.extend(
            strategic
                .last_action_observations()
                .iter()
                .map(|o| o.observation.frame_count),
        );
        for wram in run(&mut every, at, &[chord]) {
            frame_count += 1;
            let frame = frame(&lowered, &wram);
            if frame.hand != prior.0 || frame.hand_terminal {
                hand_key_frames.insert(frame_count);
            }
            if frame.compiled != prior.1 || frame.stops {
                compiled_key_frames.insert(frame_count);
            }
            prior = (frame.hand, frame.compiled);
            if frame.stops {
                break;
            }
        }
        let next = every.snapshot().expect("snapshot");
        every.drop_snapshot(at).expect("drop");
        at = next;
        assert_eq!(
            hand.observe().frame_count,
            strategic.observation().frame_count
        );
        if hand.is_terminal() {
            break;
        }
    }
    assert_eq!(
        hand_key_frames, compiled_key_frames,
        "key-change frames differ"
    );
    assert!(compiled_key_frames.is_subset(&strategy_frames));
    eprintln!(
        "twin dragons interior observations: hand {} frames, strategy {} frames, both {}, hand only {}, strategy only {}; key-change frames {} under both keys",
        hand_frames.len(),
        strategy_frames.len(),
        hand_frames.intersection(&strategy_frames).count(),
        hand_frames.difference(&strategy_frames).count(),
        strategy_frames.difference(&hand_frames).count(),
        compiled_key_frames.len()
    );
    let _ = workload.key_shape();
}

#[test]
#[ignore = "needs HARMONY_TWIN_DRAGONS_ROM, HARMONY_QUICKNES_CORE and HARMONY_TWIN_DRAGONS_TAPES"]
fn compiled_keys_match_hand_keys_on_replayed_tapes() {
    let inputs = inputs();
    let lowered = Lowered::new(TwinDragons, TwinDragons::STRATEGY).expect("strategy");
    let mut machine = machine(&inputs);
    let power_on = machine.snapshot().expect("power on");
    run(&mut machine, power_on, &TwinDragons.setup());
    let genesis = machine.snapshot().expect("genesis");
    let tapes = std::env::var("HARMONY_TWIN_DRAGONS_TAPES").expect("HARMONY_TWIN_DRAGONS_TAPES");
    let (mut hand, mut compiled) = (Vec::new(), Vec::new());
    let mut progress = BTreeSet::new();
    for path in tapes.split(':').filter(|path| !path.is_empty()) {
        let tape: Input<ButtonChord> =
            serde_json::from_slice(&std::fs::read(path).expect("tape")).expect("tape json");
        let (mut frames, mut absent, mut stop) = (0usize, 0usize, None);
        for wram in run(&mut machine, genesis, &tape.actions) {
            frames += 1;
            let frame = frame(&lowered, &wram);
            assert_eq!(
                frame.hand.is_none(),
                frame.compiled.is_none(),
                "absent differs in {path} at frame {frames}"
            );
            assert_eq!(
                frame.hand_terminal, frame.stops,
                "terminal differs in {path} at frame {frames}"
            );
            if frame.stops {
                stop = Some((frames, if frame.goal { "goal" } else { "reject" }));
                break;
            }
            match (frame.hand, frame.compiled) {
                (Some(h), Some(c)) => {
                    progress.insert(format!("{:?}", h.progress()));
                    hand.push(h);
                    compiled.push(c);
                }
                _ => absent += 1,
            }
        }
        let last = hand.last().map(|h| format!("{:?}", h.progress()));
        eprintln!(
            "twin dragons tape {path}: {} actions, {frames} frames, {absent} absent, stop at {stop:?}, last progress {last:?}",
            tape.actions.len()
        );
    }
    let distinct = agree(&hand, &compiled, lowered.compiled.shape());
    eprintln!(
        "twin dragons tape equivalence: {} keyed states ({distinct} distinct key pairs); {} progress values {progress:?}",
        hand.len(),
        progress.len()
    );
}
