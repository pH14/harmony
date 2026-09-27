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
        .ok_or("usage: mm2-tape-probe <input.json> [root.json]")?;
    let root_path = args.next();
    if args.next().is_some() {
        return Err("unexpected arguments".into());
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
    }
    println!(
        "{}",
        serde_json::json!({
            "actions": input.actions.len(),
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
