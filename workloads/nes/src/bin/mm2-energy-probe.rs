// SPDX-License-Identifier: AGPL-3.0-or-later

//! Replay a searched Mega Man 2 input and print the per-weapon energy meters
//! at each action endpoint, beside the position the archive keys.
//!
//! The decoded state keeps only the summed meter, so a campaign report cannot
//! say whether a lineage still holds the weapon its route needs.

use std::{env, error::Error, fs, path::PathBuf};

use nes_workload::mm2::{
    campaign::Mm2Game,
    target::{Mm2Input, Mm2Stage},
};
use nes_workload::search::campaign::TargetExecution;
use nes_workload::target::Target;
use sha2::{Digest, Sha256};

const WEAPONS: [&str; 12] = [
    "heat", "air", "wood", "bubble", "quick", "flash", "metal", "crash", "item1", "item2", "item3",
    "etank",
];

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let stage = Mm2Stage::parse(
        &args
            .next()
            .ok_or("usage: mm2-energy-probe STAGE PREFIX INPUT")?,
    )?;
    let prefix_path = args.next().ok_or("missing prefix input")?;
    let input_path = args.next().ok_or("missing replay input")?;
    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_MM2_ROM").ok_or("HARMONY_MM2_ROM")?,
    ))?;
    let core = PathBuf::from(env::var_os("HARMONY_QUICKNES_CORE").ok_or("HARMONY_QUICKNES_CORE")?);
    let core_sha = format!("{:x}", Sha256::digest(fs::read(&core)?));

    let prefix: Mm2Input = serde_json::from_slice(&fs::read(&prefix_path)?)?;
    let replay: Mm2Input = serde_json::from_slice(&fs::read(&input_path)?)?;
    let game = Mm2Game::new_at_stage_after(&rom, &core, &core_sha, prefix.actions, stage);
    let mut target = game.new_target()?;

    print!("action frames screen room x y posture weapon health sum");
    for name in WEAPONS {
        print!(" {name}");
    }
    println!();
    let report = |index: usize,
                  target: &nes_workload::mm2::target::Mm2Target|
     -> Result<(), Box<dyn Error>> {
        let s = target.mechanical_state();
        print!(
            "{index} {} {} {} {} {} {} {} {} {}",
            target.frames_clocked(),
            s.screen,
            s.room,
            s.x,
            s.y,
            s.posture(),
            s.weapon,
            s.health,
            s.weapon_energy
        );
        for value in target.diagnostic_weapon_energies()? {
            print!(" {value}");
        }
        println!();
        Ok(())
    };
    report(0, &target)?;
    for (index, action) in replay.actions.iter().enumerate() {
        target.apply(action);
        report(index + 1, &target)?;
        if target.is_dead() {
            println!("# dead at action {}", index + 1);
            break;
        }
        // The target refuses actions once a boss is down, so every later row
        // would repeat this one.
        if target.defeated_a_boss() {
            println!("# boss defeated at action {}", index + 1);
            break;
        }
    }
    Ok(())
}
