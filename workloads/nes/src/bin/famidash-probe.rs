// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeMap, env, error::Error, fs, path::PathBuf};

use nes_workload::famidash::target::{
    A_BUTTON, ButtonChord, FamidashInput, FamidashMechanicalState, FamidashSnapshot, FamidashTarget,
};
use nes_workload::target::Target;
use sha2::{Digest, Sha256};

const USAGE: &str =
    "usage: famidash-probe ROM LEVEL [INPUT.json] [trace | frontier FRAMES [OUT.json]]";
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
                if !next.contains_key(&key) {
                    let snapshot = target.snapshot().ok_or("could not snapshot a branch")?;
                    next.insert(key, Branch { snapshot, frames });
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
