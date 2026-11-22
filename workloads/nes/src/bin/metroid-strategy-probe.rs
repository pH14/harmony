// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf, sync::Arc};

use machine::{
    nes::{ButtonChord, SAVE_RAM_SIZE},
    quicknes::QuickNesMachine,
};
use nes_games::{
    metroid::game::Metroid,
    workload::{Game, Lowered, NativeMemory, NesTarget},
};
use nes_workload::{
    search::archive::Input,
    target::{ExitKind, Target},
};
use sha2::{Digest, Sha256};

const USAGE: &str = "usage: metroid-strategy-probe <root.json> [--strategy STRATEGY.toml] [--every] [--wram START:LEN] [--cartridge START:LEN] [--frames FIRST:LAST] [--terms NAME,...]";

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let root_path = PathBuf::from(args.next().ok_or(USAGE)?);
    let mut strategy = Metroid::STRATEGY.to_owned();
    let mut every = false;
    let mut window = None;
    let mut cartridge = None;
    let mut frames = None;
    let mut terms = vec![
        "missiles".to_owned(),
        "health".to_owned(),
        "x".to_owned(),
        "y".to_owned(),
        "boss_health".to_owned(),
    ];
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--strategy" => strategy = fs::read_to_string(args.next().ok_or(USAGE)?)?,
            "--every" => every = true,
            "--wram" => {
                let value = args.next().ok_or(USAGE)?;
                let (start, len) = value.split_once(':').ok_or(USAGE)?;
                window = Some((
                    usize::from_str_radix(start.trim_start_matches("0x"), 16)?,
                    len.parse::<usize>()?,
                ));
            }
            "--cartridge" => {
                let value = args.next().ok_or(USAGE)?;
                let (start, len) = value.split_once(':').ok_or(USAGE)?;
                let start = usize::from_str_radix(start.trim_start_matches("0x"), 16)?;
                let len = len.parse::<u32>()?;
                let end = u64::try_from(start)?.checked_add(u64::from(len));
                if end.is_none_or(|end| end > SAVE_RAM_SIZE) {
                    return Err(format!(
                        "--cartridge {value} runs past the {SAVE_RAM_SIZE:#x} bytes of cartridge RAM"
                    )
                    .into());
                }
                cartridge = Some((start, len));
            }
            "--frames" => {
                let value = args.next().ok_or(USAGE)?;
                let (first, last) = value.split_once(':').ok_or(USAGE)?;
                frames = Some((first.parse::<usize>()?, last.parse::<usize>()?));
            }
            "--terms" => {
                terms = args
                    .next()
                    .ok_or(USAGE)?
                    .split(',')
                    .map(str::to_owned)
                    .collect();
            }
            _ => return Err(USAGE.into()),
        }
    }
    let rom = machine::nes::with_cartridge_ram(&fs::read(PathBuf::from(
        env::var_os("HARMONY_METROID_ROM").ok_or("HARMONY_METROID_ROM must name the ROM")?,
    ))?)?;
    let core = PathBuf::from(
        env::var_os("HARMONY_QUICKNES_CORE").ok_or("HARMONY_QUICKNES_CORE must name the core")?,
    );
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core)?));
    let execution = machine::quicknes::prepared_execution_sha256(&core, &core_sha256)?;
    let machine =
        QuickNesMachine::from_rom_bytes_with_identity(&rom, &core, &core_sha256, &execution)?;
    let lowered = Arc::new(Lowered::new(Metroid, &strategy)?);
    let root: Input<ButtonChord> = serde_json::from_slice(&fs::read(&root_path)?)?;
    if root.actions.is_empty() {
        return Err(format!("{} holds no actions", root_path.display()).into());
    }
    let mut target =
        NesTarget::from_power_on(Arc::clone(&lowered), machine, &[], Some(NativeMemory::of()))?;
    println!(
        "# root {} actions {}",
        root_path.display(),
        root.actions.len()
    );
    println!(
        "action frames stage objective area map_x map_y health missiles capacity tanks bosses mb ending zeb_left zeb_dead"
    );
    let catalog = lowered.records_catalog()?;
    let slots = terms
        .iter()
        .map(|term| {
            catalog
                .slot(term)
                .ok_or_else(|| format!("the strategy declares no term {term}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if frames.is_some() {
        println!("# frame action offset {}", terms.join(" "));
    }
    let mut last_stage = None;
    for (index, action) in root.actions.iter().enumerate() {
        let recorded = frames.is_some_and(|(first, last)| (first..=last).contains(&index));
        target.record_frames(recorded);
        target.apply(action);
        if recorded {
            let records = target
                .last_action_observations()
                .iter()
                .flat_map(|observed| observed.records.iter());
            for (offset, record) in records.enumerate() {
                let values: Vec<String> = slots
                    .iter()
                    .map(|&slot| {
                        record
                            .values()
                            .get(slot)
                            .map_or_else(|| "-".to_owned(), i64::to_string)
                    })
                    .collect();
                println!("# frame {index} {offset} {}", values.join(" "));
            }
        }
        if target.exit_kind() != ExitKind::Ok {
            return Err(
                format!("action {index} ended the target: {:?}", target.exit_kind()).into(),
            );
        }
        let observation = target.observation();
        let view = observation.key.map(|key| lowered.compiled.view(key));
        let stage = view.as_ref().and_then(|view| view.stage.clone());
        let last = index + 1 == root.actions.len();
        if every || last || stage != last_stage {
            let state = observation.state;
            println!(
                "{index} {} {} {:?} {} {} {} {} {} {} {} {} {} {} {} {}",
                observation.frame_count,
                stage.as_deref().unwrap_or("none"),
                view.as_ref()
                    .map(|view| view.progress.clone())
                    .unwrap_or_default(),
                state.area,
                state.map_x,
                state.map_y,
                state.health,
                state.missiles,
                state.missile_capacity,
                state.energy_tanks,
                state.bosses,
                u8::from(state.mother_brain_defeated),
                u8::from(state.ending),
                state.zebetite_hits_left,
                state.zebetites_destroyed,
            );
            if let Some((start, len)) = window {
                let frame = target.frame();
                let end = start.saturating_add(len).min(frame.len());
                let bytes: String = frame
                    .get(start..end)
                    .unwrap_or_default()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                println!("# wram {index} {start:#x} {bytes}");
            }
            if let Some((start, len)) = cartridge {
                let bytes: String = target
                    .read_save_ram(start, len)?
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                println!("# cartridge {index} {start:#x} {bytes}");
            }
        }
        last_stage = stage;
    }
    println!(
        "# end frame_sha256 {:x} rooted_genesis {:?}",
        Sha256::digest(target.frame()),
        lowered.game.rooted_genesis(&target.observation().state)
    );
    Ok(())
}
