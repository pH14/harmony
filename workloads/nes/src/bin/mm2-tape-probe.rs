// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use nes_workload::{
    mm2::{
        progress::NamedProgress,
        target::{Mm2Input, Mm2Target},
    },
    target::{ExitKind, Target},
};
use sha2::{Digest, Sha256};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let input_path = args
        .next()
        .ok_or("usage: mm2-tape-probe <input.json> [root.json] [--screenshot frame.ppm]")?;
    let mut root_path = None;
    let mut screenshot = None;
    while let Some(argument) = args.next() {
        if argument == "--screenshot" && screenshot.is_none() {
            screenshot = Some(PathBuf::from(args.next().ok_or("missing screenshot path")?));
        } else if !argument.starts_with("--") && root_path.is_none() {
            root_path = Some(argument);
        } else {
            return Err("unexpected arguments".into());
        }
    }
    let rom = fs::read(env::var_os("HARMONY_MM2_ROM").ok_or("HARMONY_MM2_ROM")?)?;
    let core = PathBuf::from(env::var_os("HARMONY_QUICKNES_CORE").ok_or("HARMONY_QUICKNES_CORE")?);
    let digest = format!("{:x}", Sha256::digest(fs::read(&core)?));
    let mut target = Mm2Target::from_rom_bytes_whole_game(&rom, &core, &digest)?;
    if let Some(path) = root_path {
        let root: Mm2Input = serde_json::from_slice(&fs::read(path)?)?;
        target.advance_genesis(&root.actions)?;
    }
    let input: Mm2Input = serde_json::from_slice(&fs::read(input_path)?)?;
    if screenshot.is_some() {
        target.start_capturing();
    }
    let mut last_frame = None;
    let mut progress = NamedProgress::default();
    progress.observe(&target.observe(), 0, target.frames_clocked());
    for (index, action) in input.actions.iter().enumerate() {
        let before = target.execution_work();
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok
            || target.execution_work().saturating_sub(before)
                != u64::from(action.bounded_hold_frames())
        {
            return Err(format!("tape action {} did not replay exactly", index + 1).into());
        }
        let end_frame = target.observe().frame_count;
        for observation in target.last_action_observations() {
            progress.observe(observation, u64::try_from(index + 1)?, end_frame);
        }
        if screenshot.is_some() {
            last_frame = target.drain_frames().pop().or(last_frame);
            target.drain_audio();
        }
    }
    if let Some(path) = &screenshot {
        let frame = last_frame.ok_or("screenshot requires at least one captured input action")?;
        let mut bytes = format!("P6\n{} {}\n255\n", frame.width, frame.height).into_bytes();
        bytes.extend_from_slice(&frame.rgb24);
        fs::write(path, bytes)?;
    }
    println!(
        "{}",
        serde_json::json!({
            "actions": input.actions.len(),
            "screenshot": screenshot,
            "milestone_counter_unit": "one-based tape action index; zero means already present at root; not search executions",
            "named_progress": progress,
            "endpoint": target.mechanical_state(),
            "weapon_energies": target.diagnostic_weapon_energies()?,
            "dead": target.is_dead(),
            "ending": target.ending_reached(),
        })
    );
    Ok(())
}
