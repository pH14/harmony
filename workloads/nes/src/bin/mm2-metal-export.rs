// SPDX-License-Identifier: AGPL-3.0-or-later
//! Qualify the existing chain bridge from a searched Metal victory without search.

use nes_workload::{
    mm2::{
        campaign::Mm2Game,
        target::{Mm2Input, Mm2Stage},
    },
    search::campaign::TargetExecution,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{error::Error, fs, path::Path};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 4 {
        return Err("usage: mm2-metal-export CORE ROM VICTORY.json OUT".into());
    }
    let core = Path::new(&args[0]);
    let rom = fs::read(&args[1])?;
    let bytes = fs::read(&args[2])?;
    let input: Mm2Input = serde_json::from_slice(&bytes)?;
    if input.actions.is_empty() || input.actions.len() > 4096 {
        return Err("invalid victory action count".into());
    }
    let out = Path::new(&args[3]);
    fs::create_dir(out)?;
    let core_hash = format!("{:x}", Sha256::digest(fs::read(core)?));
    let game = Mm2Game::new_at_stage(&rom, core, &core_hash, Mm2Stage::from_number(6)?);
    let mut target = game.new_target()?;
    let mut next = target.genesis_prefix().to_vec();
    let before = target.frames_clocked();
    let physical = target.physical_input(&input)?;
    let export_frames = target.frames_clocked() - before;
    if !target.defeated_a_boss() {
        return Err("input does not retain the searched boss award".into());
    }
    let endpoint = target.mechanical_state();
    next.extend(physical.actions);
    let walk = game.walk_to_stage_select(&next)?;
    let transition_frames: u64 = next
        .iter()
        .chain(&walk)
        .map(|a| u64::from(a.bounded_hold_frames()))
        .sum();
    next.extend(walk);
    let prefix_bytes = serde_json::to_vec(&Mm2Input { actions: next })?;
    fs::write(out.join("next-prefix.json"), &prefix_bytes)?;
    let report = json!({"format":"mm2-searched-metal-export-v1",
        "scope":"existing searched victory export; no search executed, not a new chain result",
        "input_sha256":format!("{:x}",Sha256::digest(bytes)),
        "prefix_sha256":format!("{:x}",Sha256::digest(&prefix_bytes)),
        "core_sha256":core_hash,"rom_sha256":format!("{:x}",Sha256::digest(rom)),
        "searched_victory_endpoint":endpoint,"reported_dead_at_award":target.is_dead(),
        "new_target_setup_frames":game.setup_frame_count(),
        "physical_export_replay_frames":export_frames,"award_transition_physical_frames":transition_frames,
        "next_stage_bridge_verification":"required separately before use"});
    fs::write(
        out.join("summary.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{report}");
    Ok(())
}
