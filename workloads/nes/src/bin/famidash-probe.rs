// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeMap, env, error::Error, fs, path::PathBuf};

use nes_workload::famidash::target::{
    A_BUTTON, ButtonChord, FamidashInput, FamidashMechanicalState, FamidashSnapshot, FamidashTarget,
};
use nes_workload::target::Target;
use sha2::{Digest, Sha256};

const USAGE: &str = "usage: famidash-probe ROM LEVEL [INPUT.json] [trace | frontier FRAMES [OUT.json] | timing STRIDE HORIZON SAMPLES]";
const FRONTIER_LIMIT: usize = 4_096;

type FrontierKey = (i32, i16, u8, bool, bool, bool, bool);

struct Branch {
    snapshot: FamidashSnapshot,
    frames: Vec<u8>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    let rom = PathBuf::from(args.first().ok_or(USAGE)?);
    let level: u8 = args.get(1).ok_or(USAGE)?.parse()?;
    let input: FamidashInput = match args.get(2).filter(|value| value.as_str() != "-") {
        Some(path) => serde_json::from_slice(&fs::read(path)?)?,
        None => FamidashInput::default(),
    };
    let core = PathBuf::from(
        env::var_os("HARMONY_QUICKNES_CORE").ok_or("HARMONY_QUICKNES_CORE must name QuickNES")?,
    );
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core)?));
    let mut target =
        FamidashTarget::from_rom_bytes_headless(&fs::read(&rom)?, &core, &core_sha256, level)?;
    if args.get(3).map(String::as_str) == Some("timing") {
        let stride: u64 = args.get(4).ok_or(USAGE)?.parse()?;
        let horizon: u64 = args.get(5).ok_or(USAGE)?.parse()?;
        let samples: u32 = args.get(6).ok_or(USAGE)?.parse()?;
        return timing(&mut target, &input, stride, horizon, samples);
    }
    let trace = args.get(3).map(String::as_str) == Some("trace");
    let mut frame = 0_u64;
    for action in &input.actions {
        if trace {
            for _ in 0..action.bounded_hold_frames() {
                target.apply(&ButtonChord::new(action.buttons, 1));
                frame += 1;
                println!(
                    "frame {frame} a={} {}",
                    action.buttons & A_BUTTON,
                    describe(target.mechanical_state())
                );
                if target.is_terminal() {
                    break;
                }
            }
        } else {
            target.apply(action);
            frame += u64::from(action.bounded_hold_frames());
        }
        if target.is_terminal() {
            break;
        }
    }
    println!(
        "endpoint frame {frame} {}",
        describe(target.mechanical_state())
    );
    if args.get(3).map(String::as_str) == Some("frontier") {
        let frames: u32 = args.get(4).ok_or(USAGE)?.parse()?;
        frontier(&mut target, &input, frames, args.get(5).map(PathBuf::from))?;
    }
    Ok(())
}

fn describe(state: FamidashMechanicalState) -> String {
    match state.gameplay {
        Some(gameplay) => format!(
            "x={} y={} vy={} mode={} gravity={} mini={} dual={} percent={}",
            gameplay.x,
            gameplay.y,
            gameplay.velocity_y,
            gameplay.gamemode,
            gameplay.gravity,
            gameplay.mini,
            gameplay.dual,
            state.percent()
        ),
        None => format!(
            "state={} dead={} complete={}",
            state.game_state,
            state.dead,
            state.complete()
        ),
    }
}

fn frontier_key(state: FamidashMechanicalState, held: bool) -> Option<FrontierKey> {
    state.gameplay.map(|gameplay| {
        (
            gameplay.y,
            gameplay.velocity_y,
            gameplay.gamemode,
            gameplay.gravity,
            gameplay.mini,
            gameplay.dual,
            held,
        )
    })
}

fn frontier(
    target: &mut FamidashTarget<machine::quicknes::QuickNesMachine>,
    prefix: &FamidashInput,
    frames: u32,
    out: Option<PathBuf>,
) -> Result<(), Box<dyn Error>> {
    let root = target.snapshot().ok_or("could not snapshot the endpoint")?;
    let mut branches = vec![Branch {
        snapshot: root,
        frames: Vec::new(),
    }];
    for step in 1..=frames {
        let mut next: BTreeMap<FrontierKey, Branch> = BTreeMap::new();
        let mut best_x = i32::MIN;
        for branch in &branches {
            for buttons in [0, A_BUTTON] {
                target.restore(&branch.snapshot)?;
                target.apply(&ButtonChord::new(buttons, 1));
                let state = target.mechanical_state();
                let mut frames = branch.frames.clone();
                frames.push(buttons);
                if state.complete() {
                    println!("complete after {step} frames");
                    if let Some(path) = &out {
                        let mut actions = prefix.actions.clone();
                        actions.extend(frames.iter().map(|&b| ButtonChord::new(b, 1)));
                        fs::write(path, serde_json::to_vec(&FamidashInput { actions })?)?;
                    }
                    return Ok(());
                }
                let Some(key) = frontier_key(state, buttons != 0) else {
                    continue;
                };
                if let Some(gameplay) = state.gameplay {
                    best_x = best_x.max(gameplay.x);
                }
                if let std::collections::btree_map::Entry::Vacant(slot) = next.entry(key) {
                    let snapshot = target.snapshot().ok_or("could not snapshot a branch")?;
                    slot.insert(Branch { snapshot, frames });
                }
            }
        }
        let stride = next.len().div_ceil(FRONTIER_LIMIT).max(1);
        branches = next.into_values().step_by(stride).collect();
        let presses: usize = branches
            .iter()
            .filter(|branch| branch.frames.last() == Some(&A_BUTTON))
            .count();
        if step % 10 == 0 || branches.is_empty() {
            println!(
                "frame +{step} alive {} (last frame pressed {presses}) x {best_x}",
                branches.len()
            );
        }
        if branches.is_empty() {
            println!("every continuation dies within {step} frames");
            return Ok(());
        }
    }
    if let (Some(path), Some(branch)) = (out, branches.first()) {
        let mut actions = prefix.actions.clone();
        actions.extend(branch.frames.iter().map(|&b| ButtonChord::new(b, 1)));
        fs::write(path, serde_json::to_vec(&FamidashInput { actions })?)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Vocabulary {
    Search,
    Taps,
    Frames,
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

fn draw_chord(rng: &mut Rng, vocabulary: Vocabulary) -> ButtonChord {
    let buttons = if rng.below(2) == 0 { 0 } else { A_BUTTON };
    let hold = match vocabulary {
        Vocabulary::Search if rng.below(2) == 0 => 1 + rng.below(4),
        Vocabulary::Search => 6 + rng.below(35),
        Vocabulary::Taps => 1 + rng.below(4),
        Vocabulary::Frames => 1,
    };
    ButtonChord::new(buttons, u8::try_from(hold).unwrap_or(1))
}

fn alive_after(
    target: &mut FamidashTarget<machine::quicknes::QuickNesMachine>,
    actions: &[ButtonChord],
    horizon: u64,
) -> bool {
    let mut frames = 0_u64;
    for action in actions {
        if frames >= horizon {
            break;
        }
        let hold = u64::from(action.bounded_hold_frames()).min(horizon - frames);
        target.apply(&ButtonChord::new(
            action.buttons,
            u8::try_from(hold).unwrap_or(1),
        ));
        frames += hold;
        if target.is_terminal() {
            return !target.mechanical_state().dead;
        }
    }
    frames >= horizon
}

fn survival(
    target: &mut FamidashTarget<machine::quicknes::QuickNesMachine>,
    root: &FamidashSnapshot,
    vocabulary: Vocabulary,
    horizon: u64,
    samples: u32,
    rng: &mut Rng,
) -> Result<u32, Box<dyn Error>> {
    let mut alive = 0;
    for _ in 0..samples {
        let mut actions = Vec::new();
        let mut frames = 0_u64;
        while frames < horizon {
            let chord = draw_chord(rng, vocabulary);
            frames += u64::from(chord.bounded_hold_frames());
            actions.push(chord);
        }
        target.restore(root)?;
        if alive_after(target, &actions, horizon) {
            alive += 1;
        }
    }
    Ok(alive)
}

fn shifted_survival(
    target: &mut FamidashTarget<machine::quicknes::QuickNesMachine>,
    root: &FamidashSnapshot,
    tail: &[ButtonChord],
    horizon: u64,
    shift: i64,
) -> Result<(u32, u32), Box<dyn Error>> {
    let (mut alive, mut tried) = (0, 0);
    let mut start = 0_u64;
    for index in 0..tail.len().saturating_sub(1) {
        if start >= horizon {
            break;
        }
        start += u64::from(tail[index].bounded_hold_frames());
        if tail[index].buttons == tail[index + 1].buttons {
            continue;
        }
        let first = i64::from(tail[index].bounded_hold_frames()) + shift;
        let second = i64::from(tail[index + 1].bounded_hold_frames()) - shift;
        if first < 1 || second < 1 {
            continue;
        }
        let mut actions = tail.to_vec();
        actions[index] = ButtonChord::new(actions[index].buttons, u8::try_from(first)?);
        actions[index + 1] = ButtonChord::new(actions[index + 1].buttons, u8::try_from(second)?);
        target.restore(root)?;
        tried += 1;
        if alive_after(target, &actions, horizon) {
            alive += 1;
        }
    }
    Ok((alive, tried))
}

fn timing(
    target: &mut FamidashTarget<machine::quicknes::QuickNesMachine>,
    input: &FamidashInput,
    stride: u64,
    horizon: u64,
    samples: u32,
) -> Result<(), Box<dyn Error>> {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut points = Vec::new();
    let mut frame = 0_u64;
    let mut next_point = 0_u64;
    for (index, action) in input.actions.iter().enumerate() {
        if frame >= next_point && target.mechanical_state().playing() {
            let snapshot = target.snapshot().ok_or("could not snapshot a point")?;
            points.push((frame, index, snapshot));
            next_point = frame + stride;
        }
        target.apply(action);
        frame += u64::from(action.bounded_hold_frames());
        if target.is_terminal() {
            break;
        }
    }
    println!("frame x mode level search taps frames shift1 shift2 of {samples}");
    for (frame, index, snapshot) in points {
        target.restore(&snapshot)?;
        let state = target.mechanical_state();
        let Some(gameplay) = state.gameplay else {
            continue;
        };
        let tail = &input.actions[index..];
        target.restore(&snapshot)?;
        if !alive_after(target, tail, horizon) {
            continue;
        }
        let mut counts = Vec::new();
        for vocabulary in [Vocabulary::Search, Vocabulary::Taps, Vocabulary::Frames] {
            counts.push(survival(target, &snapshot, vocabulary, horizon, samples, &mut rng)?);
        }
        let mut shifts = Vec::new();
        for size in [1, 2] {
            let (mut alive, mut tried) = (0, 0);
            for shift in [-size, size] {
                let (a, t) = shifted_survival(target, &snapshot, tail, horizon, shift)?;
                alive += a;
                tried += t;
            }
            shifts.push(format!("{alive}/{tried}"));
        }
        println!(
            "{frame} {} {} {} {} {} {} {} {}",
            gameplay.x,
            gameplay.gamemode,
            state.level,
            counts[0],
            counts[1],
            counts[2],
            shifts[0],
            shifts[1]
        );
    }
    Ok(())
}
