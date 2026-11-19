// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeMap, path::PathBuf};

use machine::{
    Machine, SnapId, StopConditions,
    nes::{self, ButtonChord, WRAM_SIZE},
    quicknes::QuickNesMachine,
};
use sha2::{Digest, Sha256};

use super::{
    archive::{SmbArchiveKey, archive_key, stamp_arrival_room},
    game::{SmbState, SuperMarioBros, WARPS_STRATEGY},
    plan::SmbRoute,
    target::{SmbTarget, smb_camera_pixels},
};
use crate::{
    search::{
        archive::{ArchiveKey, Input, KeyShape},
        assertion::{Accumulator, AssertionKey},
        rand::RomuDuoJrRand,
    },
    workload::{Game, Lowered},
};

struct Inputs {
    rom: Vec<u8>,
    core: PathBuf,
    core_sha256: String,
    execution_sha256: String,
}

fn inputs() -> Inputs {
    let rom = std::fs::read(std::env::var_os("HARMONY_SMB_ROM").expect("HARMONY_SMB_ROM"))
        .expect("Super Mario Bros. ROM");
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

fn read_wram(machine: &QuickNesMachine) -> [u8; WRAM_SIZE] {
    machine
        .read(0, WRAM_SIZE as u32)
        .expect("memory")
        .try_into()
        .expect("memory length")
}

fn genesis(inputs: &Inputs) -> (QuickNesMachine, SnapId, [u8; WRAM_SIZE]) {
    let hand =
        SmbTarget::from_smb_rom_bytes_headless(&inputs.rom, &inputs.core, &inputs.core_sha256)
            .expect("hand target");
    let setup = SuperMarioBros.setup();
    let setup_frames: u64 = setup.iter().map(|chord| u64::from(chord.hold_frames)).sum();
    assert_eq!(
        setup_frames,
        hand.boot_frames(),
        "the setup tape and the hand boot differ"
    );
    let mut machine = machine(inputs);
    let power_on = machine.snapshot().expect("power on");
    run(&mut machine, power_on, &setup);
    let wram = read_wram(&machine);
    assert_eq!(
        wram,
        hand.wram(),
        "the genesis memory differs from the hand boot"
    );
    let genesis = machine.snapshot().expect("genesis");
    (machine, genesis, wram)
}

#[derive(Clone)]
struct Chain {
    kept: SmbArchiveKey,
    candidate: SmbArchiveKey,
    bucket: u32,
    parent: SmbState,
    state: SmbState,
    accumulator: Accumulator,
}

impl Chain {
    fn genesis(lowered: &Lowered<SuperMarioBros>, route: SmbRoute, wram: &[u8; WRAM_SIZE]) -> Self {
        let key = hand_key(route, wram).complete(None);
        let state = lowered.game.decode(None, None, wram);
        let mut accumulator = Accumulator::default();
        let _ = lowered.observe(0, state, &mut accumulator);
        Self {
            kept: key,
            candidate: key,
            bucket: smb_camera_pixels(wram) / 16,
            parent: state,
            state,
            accumulator,
        }
    }
}

fn hand_key(route: SmbRoute, wram: &[u8; WRAM_SIZE]) -> SmbArchiveKey {
    stamp_arrival_room(archive_key(wram).on_route(route), wram).expect("arrival room")
}

fn routes() -> [(SmbRoute, Lowered<SuperMarioBros>); 2] {
    [
        (SmbRoute::AllLevels, SuperMarioBros::STRATEGY),
        (SmbRoute::Warps, WARPS_STRATEGY),
    ]
    .map(|(route, strategy)| {
        (
            route,
            Lowered::new(SuperMarioBros, strategy).expect("strategy"),
        )
    })
}

struct Frame {
    hand: SmbArchiveKey,
    candidate: Option<SmbArchiveKey>,
    compiled: Option<AssertionKey>,
    hand_stops: bool,
    stops: bool,
}

fn step(
    lowered: &Lowered<SuperMarioBros>,
    route: SmbRoute,
    chain: &mut Chain,
    wram: &[u8; WRAM_SIZE],
    endpoint: bool,
) -> Frame {
    let raw = hand_key(route, wram);
    let hand = raw.complete(Some((chain.kept, &())));
    let state = lowered
        .game
        .decode(Some(&chain.parent), Some(&chain.state), wram);
    chain.state = state;
    if endpoint {
        chain.kept = hand;
        chain.parent = state;
    }
    let bucket = smb_camera_pixels(wram) / 16;
    let hand_stops = state.dead
        || state.victory
        || (endpoint && (state.doomed || (!state.dead && route.is_doomed(wram))));
    let candidate =
        (bucket != chain.bucket || state.dead || state.victory || endpoint).then(|| {
            let key = raw.complete(Some((chain.candidate, &())));
            chain.candidate = key;
            key
        });
    chain.bucket = bucket;
    lowered.checked_record(&state).expect("checked record");
    let observation = lowered
        .observe_ending(0, state, &mut chain.accumulator, |_| endpoint)
        .observation;
    Frame {
        hand,
        candidate,
        compiled: observation.key,
        hand_stops,
        stops: observation.stops(),
    }
}

fn order<A: Ord, B: Ord>(
    pairs: &mut [(SmbArchiveKey, AssertionKey)],
    hand: impl Fn(&SmbArchiveKey) -> A,
    compiled: impl Fn(&AssertionKey) -> B,
    what: &str,
) {
    pairs.sort_by_key(|pair| hand(&pair.0));
    for pair in pairs.windows(2) {
        assert_eq!(
            hand(&pair[0].0).cmp(&hand(&pair[1].0)),
            compiled(&pair[0].1).cmp(&compiled(&pair[1].1)),
            "{what} order differs for {:?} and {:?}",
            pair[0].0,
            pair[1].0
        );
    }
}

fn agree(pairs: &mut Vec<(SmbArchiveKey, AssertionKey)>, shape: KeyShape) -> usize {
    assert_eq!(KeyShape::of::<SmbArchiveKey>(), shape, "key shapes differ");
    pairs.sort();
    pairs.dedup();
    order(pairs, |h| h.progress(), |c| c.progress(), "progress");
    order(
        pairs,
        |h| (h.progress(), h.place()),
        |c| (c.progress(), c.place()),
        "cell",
    );
    order(
        pairs,
        |h| (h.progress(), h.place(), h.identity()),
        |c| (c.progress(), c.place(), c.identity()),
        "slot",
    );
    pairs.sort_by_key(|pair| pair.0.clock);
    for pair in pairs.windows(2) {
        assert_eq!(
            pair[0].0.preference_cmp(0, pair[1].0),
            pair[0].1.preference_cmp(0, pair[1].1),
            "preference differs for {:?} and {:?}",
            pair[0].0,
            pair[1].0
        );
    }
    pairs.len()
}

#[derive(Default)]
struct Tally {
    pairs: Vec<(SmbArchiveKey, AssertionKey)>,
    frames: usize,
    candidates: usize,
    candidate_differs: usize,
    stops: usize,
    tiers: BTreeMap<(u8, u16), usize>,
}

impl Tally {
    fn add(&mut self, frame: &Frame, place: &str) {
        self.frames += 1;
        assert_eq!(frame.hand_stops, frame.stops, "stop differs {place}");
        let compiled = frame
            .compiled
            .expect("every Super Mario Bros. state has a key");
        if frame.stops {
            self.stops += 1;
            return;
        }
        if let Some(candidate) = frame.candidate {
            self.candidates += 1;
            self.candidate_differs += usize::from(candidate != frame.hand);
        }
        *self.tiers.entry(frame.hand.progress()).or_default() += 1;
        self.pairs.push((frame.hand, compiled));
    }
}

fn tape_starts(
    machine: &mut QuickNesMachine,
    genesis: SnapId,
    genesis_wram: &[u8; WRAM_SIZE],
    route: SmbRoute,
    lowered: &Lowered<SuperMarioBros>,
    every: usize,
) -> Vec<(u32, SnapId, Chain)> {
    let mut starts = Vec::new();
    let Ok(tapes) = std::env::var("HARMONY_SMB_TAPES") else {
        return starts;
    };
    for path in tapes.split(':').filter(|path| !path.is_empty()) {
        let tape: Input<ButtonChord> =
            serde_json::from_slice(&std::fs::read(path).expect("tape")).expect("tape json");
        let mut chain = Chain::genesis(lowered, route, genesis_wram);
        let mut at = genesis;
        'tape: for (index, chord) in tape.actions.iter().enumerate() {
            let frames = run(machine, at, std::slice::from_ref(chord));
            for (offset, wram) in frames.iter().enumerate() {
                if step(lowered, route, &mut chain, wram, offset + 1 == frames.len()).stops {
                    break 'tape;
                }
            }
            let next = machine.snapshot().expect("snapshot");
            if at != genesis && !starts.iter().any(|(_, kept, _)| *kept == at) {
                machine.drop_snapshot(at).expect("drop");
            }
            at = next;
            if (index + 1) % every == 0 {
                starts.push((0, at, chain.clone()));
            }
        }
        if at != genesis && !starts.iter().any(|(_, kept, _)| *kept == at) {
            machine.drop_snapshot(at).expect("drop");
        }
    }
    starts
}

fn random_play(
    machine: &mut QuickNesMachine,
    genesis: SnapId,
    genesis_wram: &[u8; WRAM_SIZE],
    route: SmbRoute,
    lowered: &Lowered<SuperMarioBros>,
    samples: usize,
) -> Tally {
    let starts = tape_starts(machine, genesis, genesis_wram, route, lowered, 40);
    let fixed = 1 + starts.len();
    let mut pool: Vec<(u32, SnapId, Chain)> =
        vec![(0, genesis, Chain::genesis(lowered, route, genesis_wram))];
    pool.extend(starts);
    let mut rand = RomuDuoJrRand::with_seed(0x736d_6273);
    let mut tally = Tally::default();
    while tally.pairs.len() < samples {
        let deepest = pool[fixed - 1..]
            .iter()
            .map(|(tier, _, _)| *tier)
            .max()
            .unwrap_or(0);
        let leaders: Vec<usize> = (fixed - 1..pool.len())
            .filter(|&i| pool[i].0 == deepest)
            .collect();
        let pick = if fixed == 1 && rand.next_u64().is_multiple_of(2) {
            leaders[usize::try_from(rand.next_u64() % leaders.len() as u64).unwrap()]
        } else {
            usize::try_from(rand.next_u64() % pool.len() as u64).unwrap()
        };
        let (_, mut at, mut chain) = pool[pick].clone();
        let mut previous = None;
        let mut ended = false;
        let mut owned = false;
        for _ in 0..8 {
            let chord = SuperMarioBros::CHORDS
                .draw(&mut rand, previous.as_ref())
                .unwrap();
            previous = Some(chord);
            let frames = run(machine, at, &[chord]);
            for (offset, wram) in frames.iter().enumerate() {
                let frame = step(lowered, route, &mut chain, wram, offset + 1 == frames.len());
                tally.add(&frame, "in random play");
                if frame.stops {
                    ended = true;
                    break;
                }
            }
            if ended {
                break;
            }
            let next = machine.snapshot().expect("snapshot");
            if owned {
                machine.drop_snapshot(at).expect("drop");
            }
            at = next;
            owned = true;
        }
        if ended {
            if owned {
                machine.drop_snapshot(at).expect("drop");
            }
            continue;
        }
        if pool.len() >= fixed + 512 {
            let victim = fixed + usize::try_from(rand.next_u64() % 512).unwrap();
            let (_, dropped, _) = pool.swap_remove(victim);
            machine.drop_snapshot(dropped).expect("drop");
        }
        let tier = tally.pairs.last().map_or(0, |pair| {
            let (stage, _) = pair.0.progress();
            u32::from(stage) << 16 | u32::from(pair.0.place().1)
        });
        pool.push((tier, at, chain));
    }
    for (_, snapshot, _) in pool.into_iter().skip(1) {
        machine.drop_snapshot(snapshot).expect("drop");
    }
    tally
}

#[test]
#[ignore = "needs HARMONY_SMB_ROM and HARMONY_QUICKNES_CORE"]
fn compiled_keys_match_hand_keys_on_played_states() {
    let inputs = inputs();
    let samples: usize = std::env::var("HARMONY_SMB_SAMPLES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(20_000);
    let (mut machine, genesis, genesis_wram) = genesis(&inputs);
    for (route, lowered) in routes() {
        let mut tally = random_play(
            &mut machine,
            genesis,
            &genesis_wram,
            route,
            &lowered,
            samples,
        );
        let keyed = tally.pairs.len();
        let distinct = agree(&mut tally.pairs, lowered.compiled.shape());
        eprintln!(
            "smb equivalence on {}: {keyed} keyed states ({distinct} distinct key pairs), {} stopping frames of {}; {} candidate frames, {} where the candidate chain's key differs from the frame chain's; {} tiers {:?}",
            route.identifier(),
            tally.stops,
            tally.frames,
            tally.candidates,
            tally.candidate_differs,
            tally.tiers.len(),
            tally.tiers
        );
    }
}

#[test]
#[ignore = "needs HARMONY_SMB_ROM, HARMONY_QUICKNES_CORE and HARMONY_SMB_TAPES"]
fn compiled_keys_match_hand_keys_on_replayed_tapes() {
    let inputs = inputs();
    let (mut machine, genesis, genesis_wram) = genesis(&inputs);
    let tapes = std::env::var("HARMONY_SMB_TAPES").expect("HARMONY_SMB_TAPES");
    for (route, lowered) in routes() {
        let mut tally = Tally::default();
        for path in tapes.split(':').filter(|path| !path.is_empty()) {
            let tape: Input<ButtonChord> =
                serde_json::from_slice(&std::fs::read(path).expect("tape")).expect("tape json");
            assert!(
                tape.actions.iter().all(|chord| chord.hold_frames > 0),
                "{path} holds a wait-for-control action, which a frame replay cannot run"
            );
            let mut chain = Chain::genesis(&lowered, route, &genesis_wram);
            let mut at = genesis;
            let (frames_before, mut stop) = (tally.frames, None);
            for (index, chord) in tape.actions.iter().enumerate() {
                let frames = run(&mut machine, at, std::slice::from_ref(chord));
                for (offset, wram) in frames.iter().enumerate() {
                    let frame = step(
                        &lowered,
                        route,
                        &mut chain,
                        wram,
                        offset + 1 == frames.len(),
                    );
                    tally.add(&frame, &format!("in {path} at action {index}"));
                    if frame.stops {
                        stop = Some((index, chain.state.victory));
                        break;
                    }
                }
                if stop.is_some() {
                    break;
                }
                let next = machine.snapshot().expect("snapshot");
                if at != genesis {
                    machine.drop_snapshot(at).expect("drop");
                }
                at = next;
            }
            if at != genesis {
                machine.drop_snapshot(at).expect("drop");
            }
            eprintln!(
                "smb tape {path} on {}: {} actions, {} frames, stop at {stop:?} (action, victory), last tier {:?}",
                route.identifier(),
                tape.actions.len(),
                tally.frames - frames_before,
                tally.pairs.last().map(|pair| pair.0.progress())
            );
        }
        let keyed = tally.pairs.len();
        let distinct = agree(&mut tally.pairs, lowered.compiled.shape());
        eprintln!(
            "smb tape equivalence on {}: {keyed} keyed states ({distinct} distinct key pairs); {} candidate frames, {} where the candidate chain's key differs from the frame chain's; {} tiers {:?}",
            route.identifier(),
            tally.candidates,
            tally.candidate_differs,
            tally.tiers.len(),
            tally.tiers
        );
    }
}
