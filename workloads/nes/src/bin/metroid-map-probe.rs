// SPDX-License-Identifier: AGPL-3.0-or-later

//! Replay a searched Metroid input and print the map cell and resources at
//! each action endpoint.
//!
//! A campaign report names the areas a run entered and the count of map cells
//! it observed. Neither says which cells a route crossed, so neither can say
//! which neighbour of a reached cell was never opened.

use std::{env, error::Error, fs, path::PathBuf};

use nes_workload::{
    metroid::target::{MetroidInput, MetroidTarget},
    target::{ExitKind, Target},
};
use sha2::{Digest, Sha256};

const USAGE: &str = "usage: metroid-map-probe <input.json>";

fn main() -> Result<(), Box<dyn Error>> {
    let input_path = PathBuf::from(env::args().nth(1).ok_or(USAGE)?);
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
    let mut target = MetroidTarget::from_rom_bytes_headless(&rom, &core_path, &core_sha256)?;

    println!(
        "action frames area map_x map_y x y mode pose health equipment missiles capacity tanks bosses"
    );
    for (index, action) in input.actions.iter().enumerate() {
        if target.is_dead() || target.is_victory() || target.exit_kind() != ExitKind::Ok {
            break;
        }
        target.apply(action);
        let s = target.mechanical_state();
        println!(
            "{index} {} {} {} {} {} {} {} {} {} {} {} {} {} {}",
            target.frames_clocked(),
            s.area,
            s.map_x,
            s.map_y,
            s.x,
            s.y,
            s.mode,
            s.pose,
            s.health,
            s.equipment,
            s.missiles,
            s.missile_capacity,
            s.energy_tanks,
            s.bosses
        );
    }
    Ok(())
}
