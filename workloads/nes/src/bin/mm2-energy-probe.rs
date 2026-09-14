// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use nes_workload::{
    mm2::target::{Mm2Input, Mm2Stage, Mm2Target},
    target::{ExitKind, Target},
};
use sha2::{Digest, Sha256};

const USAGE: &str = "usage: mm2-energy-probe <stage> <chain-prefix.json> <input.json>";

const WEAPONS: [&str; 12] = [
    "heat", "air", "wood", "bubble", "quick", "flash", "metal", "crash", "item1", "item2", "item3",
    "etank",
];

fn report(index: usize, target: &Mm2Target) -> Result<(), Box<dyn Error>> {
    let state = target.mechanical_state();
    print!(
        "{index} {} {} {} {} {} {} {} {} {}",
        target.frames_clocked(),
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
    println!();
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let stage = Mm2Stage::parse(&args.next().ok_or(USAGE)?)?;
    let prefix_path = PathBuf::from(args.next().ok_or(USAGE)?);
    let input_path = PathBuf::from(args.next().ok_or(USAGE)?);
    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_MM2_ROM").ok_or("HARMONY_MM2_ROM must name the external ROM")?,
    ))?;
    let core_path = PathBuf::from(
        env::var_os("HARMONY_QUICKNES_CORE")
            .ok_or("HARMONY_QUICKNES_CORE must name the pinned libretro core")?,
    );
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core_path)?));

    let prefix: Mm2Input = serde_json::from_slice(&fs::read(&prefix_path)?)?;
    let replay: Mm2Input = serde_json::from_slice(&fs::read(&input_path)?)?;
    let mut target =
        Mm2Target::from_rom_bytes_after(&rom, &core_path, &core_sha256, &prefix.actions, stage)?;

    print!("action frames screen room x y posture weapon health sum");
    for name in WEAPONS {
        print!(" {name}");
    }
    println!();
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
        if target.defeated_a_boss() {
            println!("# boss defeated at action {}", index + 1);
            break;
        }
    }
    Ok(())
}
