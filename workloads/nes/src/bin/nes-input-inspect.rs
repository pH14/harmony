// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded raw replay of development-discovered inputs for observation audits.
use machine::{Machine, StopConditions, nes, quicknes::QuickNesMachine};
use nes_workload::{
    metroid::target::{MetroidInput, MetroidTarget, decode_state as metroid_state},
    mm2::target::{Mm2Input, Mm2Stage, Mm2Target, decode_state as mm2_state},
    target::{ExitKind, Target},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{error::Error, fs, path::PathBuf, time::Instant};
#[derive(Deserialize)]
struct Request {
    game: String,
    core: PathBuf,
    rom: PathBuf,
    input: PathBuf,
    input_sha256: String,
    prefix: Option<PathBuf>,
    prefix_sha256: Option<String>,
    stage: Option<String>,
    terminal: Option<String>,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn read_input(path: &PathBuf, expected: &str) -> Result<MetroidInput, Box<dyn Error>> {
    if fs::metadata(path)?.len() > 2_097_152 {
        return Err("input exceeds two MiB".into());
    }
    let bytes = fs::read(path)?;
    if hash(&bytes) != expected {
        return Err("input checksum mismatch".into());
    }
    let input: MetroidInput = serde_json::from_slice(&bytes)?;
    if input.actions.len() > 20_000 {
        return Err("input exceeds 20k actions".into());
    }
    Ok(input)
}
// Host timing is diagnostic only and never enters replay or search decisions.
#[allow(clippy::disallowed_methods)]
fn telemetry_now() -> Instant {
    Instant::now()
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: nes-input-inspect REQUEST OUTPUT".into());
    }
    let started = telemetry_now();
    let request_bytes = fs::read(&args[0])?;
    let request: Request = serde_json::from_slice(&request_bytes)?;
    let input = read_input(&request.input, &request.input_sha256)?;
    let out = PathBuf::from(&args[1]);
    fs::create_dir(&out)?;
    let rom = fs::read(&request.rom)?;
    let core_hash = hash(&fs::read(&request.core)?);
    let mut tape;
    let expected;
    let adapter_frames;
    let adapter_dead;
    if request.game == "metroid" {
        let mut target = MetroidTarget::from_rom_bytes_headless(&rom, &request.core, &core_hash)?
            .with_terminal_policy(nes_workload::metroid::target::MetroidTerminalPolicy::parse(
                request.terminal.as_deref().unwrap_or("death_or_ending_v2"),
            )?);
        tape = target.genesis_prefix().to_vec();
        for action in &input.actions {
            if target.is_dead() || target.is_victory() {
                return Err("terminal input".into());
            }
            target.apply(action);
            if target.exit_kind() != ExitKind::Ok {
                return Err("adapter execution failed".into());
            }
        }
        expected = serde_json::to_value(target.mechanical_state())?;
        adapter_frames = target.frames_clocked();
        adapter_dead = target.is_dead();
        tape.extend(input.actions.iter().copied());
    } else if request.game == "mm2" {
        let prefix = read_input(
            request.prefix.as_ref().ok_or("missing prefix")?,
            request
                .prefix_sha256
                .as_deref()
                .ok_or("missing prefix hash")?,
        )?;
        let stage = Mm2Stage::parse(request.stage.as_deref().ok_or("missing stage")?)?;
        let mut target = Mm2Target::from_rom_bytes_after(
            &rom,
            &request.core,
            &core_hash,
            &prefix.actions,
            stage,
        )?;
        tape = target.genesis_prefix().to_vec();
        tape.extend(
            target
                .physical_input(&Mm2Input {
                    actions: input.actions,
                })?
                .actions,
        );
        expected = serde_json::to_value(target.mechanical_state())?;
        adapter_frames = target.frames_clocked();
        adapter_dead = target.is_dead();
    } else {
        return Err("unsupported game".into());
    }
    let total: u64 = tape
        .iter()
        .map(|a| u64::from(a.bounded_hold_frames()))
        .sum();
    if total > 2_000_000 || tape.len() > 20_000 {
        return Err("physical tape exceeds bounds".into());
    }
    let image = if request.game == "metroid" {
        nes::with_cartridge_ram(&rom)?
    } else {
        rom.clone()
    };
    let mut machine = QuickNesMachine::from_rom_bytes(&image, &request.core, &core_hash)?;
    let mut frames = 0_u64;
    let mut prior = None;
    let mut anomalies = Vec::new();
    for (index, action) in tape.iter().enumerate() {
        machine.set_video_capture(index + 1 == tape.len());
        let snap = machine.snapshot()?;
        machine.branch(snap, &nes::reproducer(&[*action]))?;
        machine.run(StopConditions::default(), None)?;
        machine.drop_snapshot(snap)?;
        for wram in machine.frames() {
            frames += 1;
            if request.game == "metroid" {
                let bcd = |b: u8| u16::from(b >> 4) * 10 + u16::from(b & 15);
                let health = bcd(wram[0x107]) * 100 + bcd(wram[0x106]);
                if let Some(previous) = prior
                    && health.abs_diff(previous) > 1000
                    && anomalies.len() < 64
                {
                    anomalies.push(json!({"frame":frames,"physical_action":index,"before":previous,
                            "after":health,"raw_low":wram[0x106],"raw_high":wram[0x107],"mode":wram[0x1e]}));
                }
                prior = Some(health);
            }
        }
    }
    let video = machine.take_video_frames();
    let last = video.last().ok_or("no endpoint frame")?;
    let mut ppm = format!("P6\n{} {}\n255\n", last.width, last.height).into_bytes();
    ppm.extend(&last.rgb24);
    fs::write(out.join("endpoint.ppm"), ppm)?;
    let wram = machine.read_wram()?;
    let cartridge = if request.game == "metroid" {
        machine.read_save_ram()?
    } else {
        Vec::new()
    };
    fs::write(out.join("wram.bin"), wram)?;
    fs::write(out.join("cartridge.bin"), &cartridge)?;
    let raw: Value = if request.game == "metroid" {
        serde_json::to_value(metroid_state(&wram, &cartridge)?)?
    } else {
        serde_json::to_value(mm2_state(&wram)?)?
    };
    let differences: Vec<_> = expected
        .as_object()
        .ok_or("expected state is not an object")?
        .iter()
        .filter(|(key, value)| raw.get(*key) != Some(*value))
        .map(|(key, value)| json!({"field":key,"adapter":value,"raw":raw.get(key)}))
        .collect();
    let mut mask_followup = Vec::new();
    if request.game == "metroid" && raw["health"].as_u64().is_some_and(|v| v >= 8000) {
        machine.set_video_capture(false);
        let endpoint = machine.snapshot()?;
        for buttons in 0..=255 {
            machine.branch(
                endpoint,
                &nes::reproducer(&[nes::ButtonChord::new(buttons, 1)]),
            )?;
            machine.run(StopConditions::default(), None)?;
            let w = machine.read_wram()?;
            let c = machine.read_save_ram()?;
            mask_followup.push(json!({"buttons":buttons,"state":metroid_state(&w,&c)?}));
        }
        machine.replay(endpoint)?;
        machine.drop_snapshot(endpoint)?;
    }
    let mut idle_followup = Vec::new();
    if request.game == "metroid" {
        machine.set_video_capture(false);
        for offset in 1..=120 {
            let snap = machine.snapshot()?;
            machine.branch(snap, &nes::reproducer(&[nes::ButtonChord::new(0, 1)]))?;
            machine.run(StopConditions::default(), None)?;
            machine.drop_snapshot(snap)?;
            let w = machine.read_wram()?;
            let c = machine.read_save_ram()?;
            idle_followup.push(json!({"offset":offset,"state":metroid_state(&w,&c)?,"raw_health":[w[0x106],w[0x107]]}));
        }
    }
    let result = json!({"format":"nes-input-inspect-v1","scope":"diagnostic raw replay; no fresh search",
        "request_sha256":hash(&request_bytes),"input_sha256":request.input_sha256,"rom_sha256":hash(&rom),
        "core_sha256":core_hash,"adapter_endpoint":expected,"adapter_dead":adapter_dead,"mask_followup":mask_followup,"diagnostic_mask_frames":mask_followup.len(),"raw_endpoint":raw,"differences":differences,
        "adapter_physical_frames":adapter_frames,"raw_physical_frames":frames,"planned_physical_frames":total,
        "large_health_transitions":anomalies,"diagnostic_idle_frames":idle_followup.len(),"idle_followup":idle_followup,"elapsed_seconds":started.elapsed().as_secs_f64()});
    fs::write(out.join("result.json"), serde_json::to_vec_pretty(&result)?)?;
    println!("{result}");
    if frames != total {
        return Err("raw frame accounting differs".into());
    }
    Ok(())
}
