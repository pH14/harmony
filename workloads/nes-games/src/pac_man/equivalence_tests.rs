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
    archive::{PacManArchiveKey, archive_key},
    game::{FINAL_LEVEL, MODE_MAZE_CLEAR, PacMan, PacManState},
    target::{PacManMechanicalState, PacManTarget, decode_state},
};
use crate::{
    search::{
        archive::{ArchiveKey, KeyShape},
        assertion::{Accumulator, AssertionKey},
        campaign::{Evaluation, TargetExecution},
        rand::RomuDuoJrRand,
    },
    target::Target,
    workload::{Game, Lowered, NesWorkload},
};

struct Inputs {
    rom: Vec<u8>,
    core: PathBuf,
    core_sha256: String,
    execution_sha256: String,
}

fn inputs() -> Inputs {
    let rom = std::fs::read(std::env::var_os("HARMONY_PAC_MAN_ROM").expect("HARMONY_PAC_MAN_ROM"))
        .expect("Pac-Man ROM");
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

#[derive(Clone, Copy)]
struct Prior {
    hand: PacManMechanicalState,
    state: PacManState,
}

fn genesis_prior(lowered: &Lowered<PacMan>, wram: &[u8; WRAM_SIZE]) -> Prior {
    Prior {
        hand: decode_state(wram).expect("hand decode"),
        state: lowered.game.decode(None, wram),
    }
}

struct Frame {
    hand: Option<PacManArchiveKey>,
    hand_terminal: bool,
    compiled: Option<AssertionKey>,
    stops: bool,
    prior: Prior,
}

fn frame(lowered: &Lowered<PacMan>, prior: Prior, wram: &[u8; WRAM_SIZE]) -> Frame {
    let mut hand = decode_state(wram).expect("hand decode");
    hand.repeat_clears = prior.hand.repeat_clears.saturating_add(u8::from(
        hand.mode == MODE_MAZE_CLEAR
            && prior.hand.mode != MODE_MAZE_CLEAR
            && hand.level >= FINAL_LEVEL,
    ));
    let state = lowered.game.decode(Some(&prior.state), wram);
    assert_eq!(
        hand.repeat_clears, state.repeat_clears,
        "repeat clears differ"
    );
    lowered.checked_record(&state).expect("checked record");
    let observation = lowered.observe(0, state, &mut Accumulator::default());
    Frame {
        hand: archive_key(hand),
        hand_terminal: hand.terminal(),
        compiled: observation.key,
        stops: observation.stops(),
        prior: Prior { hand, state },
    }
}

fn agree(hand: &[PacManArchiveKey], compiled: &[AssertionKey], shape: KeyShape) -> usize {
    assert_eq!(
        KeyShape::of::<PacManArchiveKey>(),
        shape,
        "key shapes differ"
    );
    let pairs: Vec<(PacManArchiveKey, AssertionKey)> = hand
        .iter()
        .copied()
        .zip(compiled.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    for (index, &(h1, c1)) in pairs.iter().enumerate() {
        for &(h2, c2) in &pairs[index..] {
            let hand_cell = (h1.progress(), h1.place()).cmp(&(h2.progress(), h2.place()));
            let compiled_cell = (c1.progress(), c1.place()).cmp(&(c2.progress(), c2.place()));
            assert_eq!(hand_cell, compiled_cell, "cell differs for {h1:?} {h2:?}");
            assert_eq!(
                hand_cell,
                compiled_cell.then(c1.identity().cmp(&c2.identity())),
                "slot differs for {h1:?} {h2:?}"
            );
            assert_eq!(
                h1.progress().cmp(&h2.progress()),
                c1.progress().cmp(&c2.progress()),
                "progress order differs for {h1:?} {h2:?}"
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
#[ignore = "needs HARMONY_PAC_MAN_ROM and HARMONY_QUICKNES_CORE"]
fn compiled_keys_match_hand_keys_on_played_states() {
    let inputs = inputs();
    let samples: usize = std::env::var("HARMONY_PAC_MAN_SAMPLES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(20_000);
    let lowered = Lowered::new(PacMan, PacMan::STRATEGY).expect("strategy");
    let mut machine = machine(&inputs);
    let power_on = machine.snapshot().expect("power on");
    run(&mut machine, power_on, &PacMan.setup());
    let genesis = machine.snapshot().expect("genesis");
    let genesis_wram: [u8; WRAM_SIZE] = machine
        .read(0, WRAM_SIZE as u32)
        .expect("genesis memory")
        .try_into()
        .expect("genesis memory length");
    let mut pool: Vec<(u32, SnapId, Prior)> =
        vec![(0, genesis, genesis_prior(&lowered, &genesis_wram))];
    let mut rand = RomuDuoJrRand::with_seed(0x7061_636d_616e);
    let (mut hand, mut compiled) = (Vec::new(), Vec::new());
    let (mut frames, mut absent, mut stops) = (0usize, 0usize, 0usize);
    let mut mazes = BTreeMap::<u8, usize>::new();
    let mut tiers = BTreeSet::new();
    while hand.len() < samples {
        let deepest = pool.iter().map(|(tier, _, _)| *tier).max().unwrap_or(0);
        let leaders: Vec<usize> = (0..pool.len()).filter(|&i| pool[i].0 == deepest).collect();
        let pick = if rand.next_u64().is_multiple_of(2) {
            leaders[usize::try_from(rand.next_u64() % leaders.len() as u64).unwrap()]
        } else {
            usize::try_from(rand.next_u64() % pool.len() as u64).unwrap()
        };
        let mut previous = None;
        let chords: Vec<ButtonChord> = (0..8)
            .map(|_| {
                let chord = PacMan::CHORDS.draw(&mut rand, previous.as_ref()).unwrap();
                previous = Some(chord);
                chord
            })
            .collect();
        let (_, start, mut prior) = pool[pick];
        let mut ended = false;
        let mut tier = 0;
        for wram in run(&mut machine, start, &chords) {
            frames += 1;
            let frame = frame(&lowered, prior, &wram);
            prior = frame.prior;
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
                    *mazes.entry(h.tier.mazes).or_default() += 1;
                    tiers.insert(h.tier);
                    tier = c.progress;
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
                let (_, dropped, _) = pool.swap_remove(victim);
                machine.drop_snapshot(dropped).expect("drop");
            }
            pool.push((tier, snapshot, prior));
        }
    }
    let distinct = agree(&hand, &compiled, lowered.compiled.shape());
    eprintln!(
        "pac-man equivalence: {} keyed states ({distinct} distinct key pairs), {absent} absent and {stops} stopping frames of {frames}; {} maze counts {mazes:?}; {} tiers {tiers:?}",
        hand.len(),
        mazes.len(),
        tiers.len()
    );
}

#[test]
#[ignore = "needs HARMONY_PAC_MAN_ROM and HARMONY_QUICKNES_CORE"]
fn interior_observations_of_one_replayed_run() {
    let inputs = inputs();
    let workload = NesWorkload::new(
        PacMan,
        &inputs.rom,
        &inputs.core,
        &inputs.core_sha256,
        &inputs.execution_sha256,
    )
    .expect("workload");
    let lowered = Arc::new(Lowered::new(PacMan, PacMan::STRATEGY).expect("strategy"));
    let mut strategic = workload.new_target().expect("strategy target");
    let mut hand = PacManTarget::from_power_on(machine(&inputs)).expect("hand target");
    let mut every = machine(&inputs);
    let power_on = every.snapshot().expect("power on");
    run(&mut every, power_on, &PacMan.setup());
    let mut at = every.snapshot().expect("genesis");
    let mut prior = genesis_prior(&lowered, strategic.wram());
    let mut rand = RomuDuoJrRand::with_seed(945);
    let mut previous = None;
    let (mut hand_frames, mut strategy_frames) = (BTreeSet::new(), BTreeSet::new());
    let (mut hand_key_frames, mut compiled_key_frames) = (BTreeSet::new(), BTreeSet::new());
    let mut prior_keys = (
        archive_key(hand.mechanical_state()),
        strategic.observation().key,
    );
    let mut frame_count = 0u64;
    for _ in 0..2_000 {
        let chord = PacMan::CHORDS.draw(&mut rand, previous.as_ref()).unwrap();
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
            let frame = frame(&lowered, prior, &wram);
            prior = frame.prior;
            if frame.hand != prior_keys.0 || frame.hand_terminal {
                hand_key_frames.insert(frame_count);
            }
            if frame.compiled != prior_keys.1 || frame.stops {
                compiled_key_frames.insert(frame_count);
            }
            prior_keys = (frame.hand, frame.compiled);
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
        let (hand_state, state) = (hand.mechanical_state(), *strategic.state());
        assert_eq!(
            (hand_state.mazes_cleared(), hand_state.repeat_clears),
            (state.mazes_cleared(), state.repeat_clears),
            "endpoint maze counts differ"
        );
        assert_eq!(
            archive_key(hand_state).is_none(),
            strategic.observation().key.is_none()
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
        "pac-man interior observations: hand {} frames, strategy {} frames, both {}, hand only {}, strategy only {}; key-change frames {} under both keys",
        hand_frames.len(),
        strategy_frames.len(),
        hand_frames.intersection(&strategy_frames).count(),
        hand_frames.difference(&strategy_frames).count(),
        strategy_frames.difference(&hand_frames).count(),
        compiled_key_frames.len()
    );
    let _ = workload.key_shape();
}
