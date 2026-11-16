// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use nes_games::smb::{
    campaign::SmbTerminalPredicate,
    shrink::splice,
    target::{SmbInput, smb_mechanical_state_from_wram},
};
use nes_workload::{search::campaign::TargetExecution, smb::campaign::SmbGame, target::Target};
use serde_json::json;

const USAGE: &str = "usage: smb-splice <prefix.json> <full.json> <control_in:W,L> <out.json>";

fn main() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<String> = env::args().skip(1).collect();
    let [prefix, full, terminal, out] = arguments.as_slice() else {
        return Err(USAGE.into());
    };
    let terminal = SmbTerminalPredicate::from_identifier(terminal)?;
    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_SMB_ROM").ok_or("HARMONY_SMB_ROM must name the ROM")?,
    ))?;
    let game = SmbGame::from_environment(&rom)?;
    let prefix: SmbInput = serde_json::from_slice(&fs::read(prefix)?)?;
    let full: SmbInput = serde_json::from_slice(&fs::read(full)?)?;
    let mut target = game.new_target()?;
    let spliced = splice(&mut target, &prefix, &full, terminal)?;
    target.reset();
    let mut applied = 0_usize;
    for action in &spliced.actions {
        if target.is_dead() || target.is_victory() {
            break;
        }
        target.apply(action);
        applied += 1;
    }
    let state = smb_mechanical_state_from_wram(&target.wram());
    fs::write(out, serde_json::to_vec_pretty(&spliced)?)?;
    println!(
        "{}",
        json!({
            "victory": target.is_victory(),
            "dead": target.is_dead(),
            "frames_from_power_on": target.boot_frames() + target.frame_count(),
            "actions": spliced.actions.len(),
            "actions_applied": applied,
            "end_world": state.world,
            "end_level": state.level,
        })
    );
    Ok(())
}
