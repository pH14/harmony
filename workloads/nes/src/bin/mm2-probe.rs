// SPDX-License-Identifier: AGPL-3.0-or-later

//! Print selected work-RAM bytes per frame while a chord schedule runs from
//! power-on. Used to establish a game's boot walk and memory decoders.

use std::{env, error::Error, fs, path::PathBuf};

use machine::nes::ButtonChord;
use machine::{Machine, StopConditions, nes, quicknes::QuickNesMachine};
use sha2::{Digest, Sha256};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let schedule = args
        .next()
        .ok_or("usage: mm2-probe <buttons:hold,...> <every> <addr,addr,...>")?;
    let every: u32 = args.next().ok_or("missing print interval")?.parse()?;
    let addrs = args
        .next()
        .ok_or("missing address list")?
        .split(',')
        .map(|a| usize::from_str_radix(a.trim_start_matches("0x"), 16))
        .collect::<Result<Vec<_>, _>>()?;
    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_MM2_ROM").ok_or("HARMONY_MM2_ROM")?,
    ))?;
    let core_path =
        PathBuf::from(env::var_os("HARMONY_QUICKNES_CORE").ok_or("HARMONY_QUICKNES_CORE")?);
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core_path)?));
    let mut machine = QuickNesMachine::from_rom_bytes(&rom, &core_path, &core_sha256)?;
    let shots = env::var_os("MM2_PROBE_SHOTS").map(PathBuf::from);
    let shot_every: u32 = env::var("MM2_PROBE_SHOT_EVERY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    if let Some(dir) = &shots {
        fs::create_dir_all(dir)?;
        machine.set_video_capture(true);
    }
    let mut frame = 0u32;
    print!("frame  chord ");
    for a in &addrs {
        print!("{a:04x} ");
    }
    println!();
    let mut prior = [0u8; 2048];
    for step in schedule.split(',') {
        if step == "play" {
            for _ in 0..1_200 {
                let w = machine.read_wram()?;
                if w[0x2c] == 0x03 && w[0x6c0] == 28 {
                    break;
                }
                let chord = ButtonChord::new(0, 1);
                let snap = machine.snapshot()?;
                machine.branch(snap, &nes::reproducer(&[chord]))?;
                machine.run(StopConditions::default(), None)?;
                machine.drop_snapshot(snap)?;
                frame += 1;
                prior = machine.read_wram()?;
            }
            println!("{frame:5}  play");
            continue;
        }
        let (b, h) = step.split_once(':').ok_or("bad step")?;
        let buttons = u8::from_str_radix(b.trim_start_matches("0x"), 16)?;
        let mut remaining: u32 = h.parse()?;
        while remaining > 0 {
            let hold = remaining.min(120);
            let chord = ButtonChord::new(buttons, u8::try_from(hold)?);
            let snap = machine.snapshot()?;
            machine.branch(snap, &nes::reproducer(&[chord]))?;
            machine.run(StopConditions::default(), None)?;
            machine.drop_snapshot(snap)?;
            let video = machine.take_video_frames();
            for (index, w) in machine.frames().iter().enumerate() {
                frame += 1;
                if let Some(dir) = &shots
                    && frame.is_multiple_of(shot_every)
                    && let Some(v) = video.get(index)
                {
                    fs::write(
                        dir.join(format!("{frame:06}-{}x{}.rgb", v.width, v.height)),
                        &v.rgb24,
                    )?;
                }
                let changed = w != &prior;
                if changed && env::var_os("MM2_PROBE_DIFF").is_some() {
                    let diffs: Vec<String> = w
                        .iter()
                        .zip(prior.iter())
                        .enumerate()
                        .filter(|(_, (a, b))| a != b)
                        .map(|(i, (a, _))| format!("{i:03x}={a:02x}"))
                        .collect();
                    println!("{frame:5} diff {}", diffs.join(" "));
                }
                if every == 0 || frame.is_multiple_of(every) || (every == 1 && changed) {
                    print!("{frame:5}  {buttons:02x}    ");
                    for a in &addrs {
                        print!("{:02x}   ", w[*a]);
                    }
                    println!();
                }
                prior = *w;
            }
            remaining -= hold;
        }
    }
    Ok(())
}
