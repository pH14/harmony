// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use nes_workload::{
    mm2::target::{Mm2Input, Mm2Target, target_from_args},
    target::{ExitKind, Target},
};
use sha2::{Digest, Sha256};

const USAGE: &str = "usage: mm2-energy-probe (<stage> <chain-prefix.json> | whole-game [--root ROOT.json | --tape TAPE.json]) <input.json>";

const WEAPONS: [&str; 12] = [
    "heat", "air", "wood", "bubble", "quick", "flash", "metal", "crash", "item1", "item2", "item3",
    "etank",
];

fn report(index: usize, target: &Mm2Target) -> Result<(), Box<dyn Error>> {
    let state = target.mechanical_state();
    print!(
        "{index} {} {} {} {} {} {} {} {} {} {} {} {} {}",
        target.frames_clocked(),
        state.stage,
        state.tier().robot_masters,
        state.refights,
        u8::from(
            target
                .last_action_observations()
                .iter()
                .any(|obs| obs.arrived)
        ),
        state.screen,
        state.room,
        state.x,
        state.y,
        state.posture(),
        state.weapon,
        state.health,
        state.weapon_energy
    );
    for value in target.diagnostic_weapon_energies()? {
        print!(" {value}");
    }
    println!(" {}", state.boss_intro_frames);
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_MM2_ROM").ok_or("HARMONY_MM2_ROM must name the external ROM")?,
    ))?;
    let core_path = PathBuf::from(
        env::var_os("HARMONY_QUICKNES_CORE")
            .ok_or("HARMONY_QUICKNES_CORE must name the pinned libretro core")?,
    );
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core_path)?));

    let (mut target, rest) = target_from_args(&args, &rom, &core_path, &core_sha256)?;
    let [input_path] = rest else {
        return Err(USAGE.into());
    };
    let replay: Mm2Input = serde_json::from_slice(&fs::read(input_path)?)?;

    print!("action frames stage bosses refights arrived screen room x y posture weapon health sum");
    for name in WEAPONS {
        print!(" {name}");
    }
    println!(" intro");
    report(0, &target)?;
    for (index, action) in replay.actions.iter().enumerate() {
        if target.exit_kind() != ExitKind::Ok {
            break;
        }
        target.apply(action);
        report(index + 1, &target)?;
        if target.is_dead() {
            println!("# dead at action {}", index + 1);
            break;
        }
        if target.objective_reached() {
            println!("# objective reached at action {}", index + 1);
            break;
        }
    }
    Ok(())
}
