// SPDX-License-Identifier: AGPL-3.0-or-later

use nes_workload::{
    metroid::{
        progress::{NamedProgress, area_name},
        target::{MetroidInput, MetroidTarget},
    },
    mm2::target::{Mm2Stage, Mm2Target},
    target::{ExitKind, Target},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, error::Error, fs, path::Path};

fn replay(
    game: &str,
    core: &Path,
    rom: &[u8],
    core_hash: &str,
    input: &MetroidInput,
    stage: Option<Mm2Stage>,
) -> Result<Value, Box<dyn Error>> {
    if game == "mm2" {
        let stage = stage.ok_or("MM2 needs the expected stage name")?;
        let mut target =
            Mm2Target::from_rom_bytes_after(rom, core, core_hash, &input.actions, stage)?;
        let prefix_frames: u64 = input.actions.iter().map(|a| u64::from(a.hold_frames)).sum();
        let genesis_frames: u64 = target
            .genesis_prefix()
            .iter()
            .map(|a| u64::from(a.hold_frames))
            .sum();
        return Ok(json!({
            "origin": "retained searched power-on tape, followed by ordinary stage setup",
            "scope": "replay compatibility and stage reach; not a fresh search result",
            "stage": stage.name(), "endpoint": target.mechanical_state(),
            "setup_frames_after_tape": genesis_frames - prefix_frames,
            "snapshot_sha256": format!("{:x}", Sha256::digest(postcard::to_allocvec(&target.snapshot().ok_or("snapshot failed")?)?))
        }));
    }
    if game != "metroid" {
        return Err("game must be metroid or mm2".into());
    }
    let mut target = MetroidTarget::from_rom_bytes_headless(rom, core, core_hash)?;
    let setup_frames = target.frames_clocked();
    let mut progress = NamedProgress::default();
    let mut cells = BTreeSet::new();
    progress.observe(&target.observe(), 0, 0);
    for (index, action) in input.actions.iter().enumerate() {
        if target.is_dead() || target.is_victory() {
            return Err("tape continues after a terminal action".into());
        }
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok {
            return Err("emulator failed".into());
        }
        let end = target.observe().frame_count;
        for observation in target.last_action_observations() {
            progress.observe(observation, index as u64 + 1, end);
            let state = observation.decoded;
            if !observation.dead && state.in_play() {
                cells.insert((state.area, state.map_x, state.map_y));
            }
        }
    }
    let mut area_cells = serde_json::Map::new();
    for area in [0x10, 0x11, 0x12, 0x13, 0x14] {
        area_cells.insert(
            area_name(area).unwrap().into(),
            json!(cells.iter().filter(|(a, _, _)| *a == area).count()),
        );
    }
    Ok(json!({
        "origin": "ordinary new-game genesis followed by retained searched gameplay tape",
        "scope": "one replayed trajectory; first_seen.execution counts tape actions, not search work",
        "named_progress": progress, "observed_map_cells": area_cells,
        "endpoint": target.mechanical_state(), "route_frames": target.observe().frame_count,
        "physical_frames_including_setup": target.frames_clocked(), "setup_frames": setup_frames,
        "snapshot_sha256": format!("{:x}", Sha256::digest(postcard::to_allocvec(&target.snapshot().ok_or("snapshot failed")?)?))
    }))
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !(4..=5).contains(&args.len()) {
        return Err("usage: nes-progress GAME CORE ROM INPUT.json [MM2_STAGE]".into());
    }
    let core = Path::new(&args[1]);
    let rom = fs::read(&args[2])?;
    let bytes = fs::read(&args[3])?;
    let input: MetroidInput = serde_json::from_slice(&bytes)?;
    let core_hash = format!("{:x}", Sha256::digest(fs::read(core)?));
    let stage = args.get(4).map(|s| Mm2Stage::parse(s)).transpose()?;
    let first = replay(&args[0], core, &rom, &core_hash, &input, stage)?;
    let second = replay(&args[0], core, &rom, &core_hash, &input, stage)?;
    if first != second {
        return Err("replay differs between independent machines".into());
    }
    println!(
        "{}",
        json!({
            "format": "nes-progress-replay-v2", "game": args[0], "verified_replays": 2,
            "rom_sha256": format!("{:x}", Sha256::digest(&rom)), "core_sha256": core_hash,
            "input_sha256": format!("{:x}", Sha256::digest(&bytes)), "result": first
        })
    );
    Ok(())
}
