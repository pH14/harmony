// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use nes_workload::{
    smb::target::{SmbInput, SmbTarget, smb_is_playing},
    target::Target,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

const WORLD_NUMBER_OFFSET: usize = 0x075f;
const LEVEL_NUMBER_OFFSET: usize = 0x075c;

#[derive(Serialize)]
struct Level {
    level: String,
    start_frame: u64,
    frames: u64,
    exit: &'static str,
}

#[derive(Serialize)]
struct Route {
    boot_frames: u64,
    tape_frames: u64,
    frames_from_power_on: u64,
    actions_applied: usize,
    victory: bool,
    dead: bool,
    levels: Vec<Level>,
}

fn label(world: u8, level: u8) -> String {
    format!("{}-{}", world + 1, level + 1)
}

fn exit_kind(from: (u8, u8), to: (u8, u8)) -> &'static str {
    let next = if from.1 == 3 {
        (from.0 + 1, 0)
    } else {
        (from.0, from.1 + 1)
    };
    if to == next {
        "next"
    } else if to.0 > from.0 && to.1 == 0 {
        "warp"
    } else {
        "other"
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let input_path = PathBuf::from(args.next().ok_or("usage: smb-route <input.json>")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_SMB_ROM").ok_or("HARMONY_SMB_ROM must name the ROM")?,
    ))?;
    let core_path = PathBuf::from(
        env::var_os("HARMONY_QUICKNES_CORE").ok_or("HARMONY_QUICKNES_CORE must name the core")?,
    );
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core_path)?));
    let input: SmbInput = serde_json::from_slice(&fs::read(&input_path)?)?;

    let mut target = SmbTarget::from_smb_rom_bytes_headless(&rom, &core_path, &core_sha256)?;
    target.reset();
    let boot_frames = target.boot_frames();

    let mut levels: Vec<(u8, u8, u64)> = Vec::new();
    let mut frames = 0_u64;
    let mut applied = 0_usize;
    let wram = target.wram();
    levels.push((wram[WORLD_NUMBER_OFFSET], wram[LEVEL_NUMBER_OFFSET], 0));
    for action in &input.actions {
        if target.is_dead() || target.is_victory() {
            break;
        }
        target.apply(action);
        applied += 1;
        frames += target.last_action_frames();
        let wram = target.wram();
        let here = (wram[WORLD_NUMBER_OFFSET], wram[LEVEL_NUMBER_OFFSET]);
        if smb_is_playing(&wram)
            && levels
                .last()
                .is_some_and(|(world, level, _)| (*world, *level) != here)
        {
            levels.push((here.0, here.1, frames));
        }
    }
    let route = Route {
        boot_frames,
        tape_frames: frames,
        frames_from_power_on: boot_frames + frames,
        actions_applied: applied,
        victory: target.is_victory(),
        dead: target.is_dead(),
        levels: levels
            .iter()
            .enumerate()
            .map(|(index, (world, level, start))| {
                let next = levels.get(index + 1);
                Level {
                    level: label(*world, *level),
                    start_frame: boot_frames + start,
                    frames: next.map_or(frames, |(_, _, end)| *end) - start,
                    exit: next.map_or("end", |(to_world, to_level, _)| {
                        exit_kind((*world, *level), (*to_world, *to_level))
                    }),
                }
            })
            .collect(),
    };
    println!("{}", serde_json::to_string_pretty(&route)?);
    Ok(())
}
