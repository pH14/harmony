// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use nes_workload::{
    metroid::target::{GenesisDepth, MetroidInput, MetroidTarget, MetroidTerminalPolicy},
    target::{ExitKind, Target},
};
use sha2::{Digest, Sha256};

const USAGE: &str = "usage: metroid-map-probe <input.json> [--root ROOT_INPUT.json] \
     [--wram START:LEN] [--terminal-policy IDENTIFIER]";

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let input_path = PathBuf::from(args.next().ok_or(USAGE)?);
    let mut terminal_policy = MetroidTerminalPolicy::Legacy;
    let mut root_input = None;
    let mut wram_window = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--root" => {
                root_input = Some(PathBuf::from(args.next().ok_or(USAGE)?));
            }
            "--wram" => {
                let value = args.next().ok_or(USAGE)?;
                let (start, len) = value.split_once(':').ok_or(USAGE)?;
                wram_window = Some((
                    usize::from_str_radix(start.trim_start_matches("0x"), 16)?,
                    len.parse::<usize>()?,
                ));
            }
            "--terminal-policy" => {
                terminal_policy = MetroidTerminalPolicy::parse(&args.next().ok_or(USAGE)?)?;
            }
            _ => return Err(USAGE.into()),
        }
    }
    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_METROID_ROM")
            .ok_or("HARMONY_METROID_ROM must name the external Metroid ROM")?,
    ))?;
    let core_path = PathBuf::from(
        env::var_os("HARMONY_QUICKNES_CORE")
            .ok_or("HARMONY_QUICKNES_CORE must name the pinned libretro core")?,
    );
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core_path)?));
    let input: MetroidInput = serde_json::from_slice(&fs::read(&input_path)?)?;
    let mut target = match &root_input {
        None => MetroidTarget::from_rom_bytes_headless(&rom, &core_path, &core_sha256)?,
        Some(path) => {
            let root: MetroidInput = serde_json::from_slice(&fs::read(path)?)?;
            let mut prefix =
                MetroidTarget::from_rom_bytes_headless(&rom, &core_path, &core_sha256)?
                    .genesis_prefix()
                    .to_vec();
            prefix.extend(root.actions.iter().copied());
            MetroidTarget::from_rom_bytes_rooted(
                &rom,
                &core_path,
                &core_sha256,
                &prefix,
                GenesisDepth::Rooted,
            )?
        }
    }
    .with_terminal_policy(terminal_policy);

    println!("# terminal_policy {}", terminal_policy.identifier());
    if let Some(path) = &root_input {
        println!("# root {}", path.display());
    }
    println!(
        "action frames area map_x map_y x y mode pose health equipment missiles capacity tanks bosses boss_health"
    );
    for (index, action) in input.actions.iter().enumerate() {
        if target.is_dead() || target.is_victory() || target.exit_kind() != ExitKind::Ok {
            break;
        }
        target.apply(action);
        let state = target.mechanical_state();
        println!(
            "{index} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {}",
            target.frames_clocked(),
            state.area,
            state.map_x,
            state.map_y,
            state.x,
            state.y,
            state.mode,
            state.pose,
            state.health,
            state.equipment,
            state.missiles,
            state.missile_capacity,
            state.energy_tanks,
            state.bosses,
            state.boss_health
        );
        if let Some((start, len)) = wram_window {
            let wram = target.current_wram();
            let end = start.saturating_add(len).min(wram.len());
            let bytes: Vec<String> = wram
                .get(start..end)
                .unwrap_or_default()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            println!("# wram {index} {start:#x} {}", bytes.join(""));
        }
    }
    Ok(())
}
