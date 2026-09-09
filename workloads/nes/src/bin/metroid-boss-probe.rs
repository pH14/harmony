// SPDX-License-Identifier: AGPL-3.0-or-later
//! Retrospective raw boss-memory trace; never supplies a search start or reward.

use nes_workload::{
    metroid::target::{ButtonChord, MetroidInput, MetroidTarget, MetroidTerminalPolicy},
    target::{ExitKind, Target},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{error::Error, fs, io::Write, path::Path};

fn retain_trace(raw: &nes_workload::metroid::boss_probe::BossMemory, boss_area: bool) -> bool {
    // A loader flag can clear before damage, and hit states overwrite the tag.
    // Retaining the entire area avoids those gaps without inferring identity.
    (boss_area && matches!(raw.area, 0x12 | 0x14))
        || raw.loader_present != 0
        || raw.enemies.iter().any(|enemy| enemy.special & 0x40 != 0)
}

fn replay(
    core: &Path,
    rom: &[u8],
    core_hash: &str,
    input: &MetroidInput,
    single_frames: bool,
    boss_area: bool,
    mut output: Option<&mut fs::File>,
) -> Result<Value, Box<dyn Error>> {
    let mut target = MetroidTarget::from_rom_bytes_headless(rom, core, core_hash)?
        .with_terminal_policy(MetroidTerminalPolicy::BcdUnderflow);
    let setup = target.frames_clocked();
    let mut route_frames = 0u64;
    let mut trace = Sha256::new();
    let (mut loader_frames, mut agreement_frames, mut stale_frames) = (0u64, 0u64, 0u64);
    let mut first_loader = None;
    let mut written = 0usize;
    for action in &input.actions {
        let steps = if single_frames {
            action.bounded_hold_frames()
        } else {
            1
        };
        let chord = if single_frames {
            ButtonChord::new(action.buttons, 1)
        } else {
            *action
        };
        for _ in 0..steps {
            if target.is_dead() || target.is_victory() {
                return Err("diagnostic tape continues after a corrected terminal boundary".into());
            }
            target.apply(&chord);
            if target.exit_kind() != ExitKind::Ok {
                return Err("diagnostic emulator failure".into());
            }
            route_frames += u64::from(chord.bounded_hold_frames());
            if single_frames {
                let raw = target.diagnostic_boss_memory()?;
                let bytes = serde_json::to_vec(&raw)?;
                trace.update((bytes.len() as u64).to_le_bytes());
                trace.update(&bytes);
                let tagged = raw.enemies.iter().any(|e| e.special & 0x40 != 0);
                let active_tagged = raw
                    .enemies
                    .iter()
                    .any(|e| e.special & 0x40 != 0 && e.status != 0);
                let guarded_loader =
                    matches!(raw.area, 0x12 | 0x14) && raw.mode == 3 && raw.loader_present == 1;
                loader_frames += u64::from(guarded_loader);
                agreement_frames += u64::from(guarded_loader && active_tagged);
                stale_frames += u64::from(tagged && (!active_tagged || raw.loader_present == 0));
                if guarded_loader {
                    first_loader.get_or_insert(route_frames);
                }
                if retain_trace(&raw, boss_area) {
                    // Numeric arrays keep a bounded diagnostic trace small. Column names
                    // are recorded once in the summary, and all frames enter the hash.
                    let slots: Vec<_> = raw
                        .enemies
                        .iter()
                        .map(|e| {
                            [
                                e.slot,
                                e.status,
                                e.data_index,
                                e.special,
                                e.hit_points,
                                e.x,
                                e.y,
                                e.name_table,
                            ]
                        })
                        .collect();
                    let row = json!([
                        route_frames,
                        raw.area,
                        raw.mode,
                        raw.door,
                        raw.room_number,
                        raw.loader_present,
                        raw.kraid_status,
                        raw.ridley_status,
                        slots
                    ]);
                    let mut line = serde_json::to_vec(&row)?;
                    line.push(b'\n');
                    written += line.len();
                    if written > 32 * 1024 * 1024 {
                        return Err("diagnostic trace exceeded 32 MiB".into());
                    }
                    if let Some(writer) = output.as_deref_mut() {
                        writer.write_all(&line)?;
                    }
                }
            }
        }
    }
    let snapshot = target.snapshot().ok_or("diagnostic snapshot failed")?;
    // Compare the actual machine bytes separately from cadence-dependent observer
    // metadata. This explicit field is frozen in the diagnostic's source identity.
    let value = serde_json::to_value(snapshot)?;
    let emulator: Vec<u8> = serde_json::from_value(value["emulator_state"].clone())?;
    Ok(
        json!({"endpoint":target.mechanical_state(),"raw_endpoint":target.diagnostic_boss_memory()?,
        "emulator_sha256":format!("{:x}",Sha256::digest(emulator)),"route_frames":route_frames,
        "physical_frames_including_setup":target.frames_clocked(),"setup_frames":setup,
        "single_frame_sampling":single_frames,"all_frame_trace_sha256":format!("{:x}",trace.finalize()),
        "guarded_loader_frames":loader_frames,"loader_and_active_tag_agreement_frames":agreement_frames,
        "tag_without_loader_or_active_slot_frames":stale_frames,"first_guarded_loader_route_frame":first_loader,
        "relevant_trace_bytes":written}),
    )
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !(4..=5).contains(&args.len()) || args.get(4).is_some_and(|arg| arg != "boss-area") {
        return Err("usage: metroid-boss-probe CORE ROM INPUT.json OUT [boss-area]".into());
    }
    let boss_area = args.len() == 5;
    let core = Path::new(&args[0]);
    let rom = fs::read(&args[1])?;
    let bytes = fs::read(&args[2])?;
    let input: MetroidInput = serde_json::from_slice(&bytes)?;
    let frames: u64 = input
        .actions
        .iter()
        .map(|a| u64::from(a.bounded_hold_frames()))
        .sum();
    if input.actions.is_empty() || input.actions.len() > 8192 || frames > 250_000 {
        return Err("diagnostic input exceeds fixed action/frame bounds".into());
    }
    let out = Path::new(&args[3]);
    fs::create_dir(out)?;
    let core_hash = format!("{:x}", Sha256::digest(fs::read(core)?));
    let ordinary = replay(core, &rom, &core_hash, &input, false, boss_area, None)?;
    fs::write(
        out.join("ordinary.json"),
        serde_json::to_vec_pretty(&ordinary)?,
    )?;
    let mut trace = fs::File::create(out.join("relevant-frames.jsonl"))?;
    let first = replay(
        core,
        &rom,
        &core_hash,
        &input,
        true,
        boss_area,
        Some(&mut trace),
    )?;
    fs::write(out.join("first.json"), serde_json::to_vec_pretty(&first)?)?;
    let second = replay(core, &rom, &core_hash, &input, true, boss_area, None)?;
    if first != second {
        return Err("independent one-frame diagnostics differ".into());
    }
    for field in [
        "endpoint",
        "raw_endpoint",
        "emulator_sha256",
        "route_frames",
    ] {
        if ordinary[field] != first[field] {
            return Err(format!("action grouping changed {field}").into());
        }
    }
    let mut result = json!({"format":"metroid-boss-memory-probe-v1","scope":"one existing searched tape; diagnostic only",
        "terminal_policy":"death_or_bcd_underflow_or_ending_v3","verified_replays":3,
        "input_sha256":format!("{:x}",Sha256::digest(bytes)),"core_sha256":core_hash,
        "rom_sha256":format!("{:x}",Sha256::digest(rom)),"ordinary":ordinary,"one_frame":first,
        "columns":["route_frame","area","mode","door","room_number","loader_present","kraid_status","ridley_status","enemy_slots"],
        "slot_columns":["offset","status","data_index","special","hit_points","x_room","y_room","name_table"],
        "limitations":["loader presence is not a damage or defeat event","special bytes may change during combat or remain stale",
                       "an empty route trace does not imply an encounter-free campaign","no values affect fresh search"]});
    if boss_area {
        result["format"] = json!("metroid-boss-memory-probe-v2");
        result["trace_observation_policy"] = json!("boss_area_all_frames_v1");
    }
    fs::write(
        out.join("summary.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    println!(
        "{}",
        json!({"verified_replays":3,"one_frame":result["one_frame"]})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nes_workload::metroid::boss_probe::{BossMemory, EnemyBytes};

    #[test]
    fn boss_area_trace_preserves_the_observed_hit_filter_counterexample() {
        // F02 Ridley frame 76002: HP 140 -> 136, status 6, special 3,
        // loader 0. Both old instantaneous guards miss this real hit frame.
        let mut raw = BossMemory {
            area: 0x14,
            mode: 3,
            door: 0,
            room_number: 0,
            loader_present: 0,
            kraid_status: 0,
            ridley_status: 0,
            enemies: vec![EnemyBytes {
                slot: 0,
                status: 6,
                data_index: 9,
                special: 3,
                hit_points: 136,
                x: 0,
                y: 0,
                name_table: 0,
            }],
        };
        assert!(!retain_trace(&raw, false));
        assert!(retain_trace(&raw, true));
        raw.area = 0x12;
        assert!(retain_trace(&raw, true));
        raw.area = 0x10;
        assert!(!retain_trace(&raw, true));
        raw.enemies[0].special = 0x40;
        assert!(retain_trace(&raw, false));
        assert!(retain_trace(&raw, true));
        raw.enemies[0].special = 0;
        raw.loader_present = 1;
        assert!(retain_trace(&raw, false));
        assert!(retain_trace(&raw, true));
    }
}
