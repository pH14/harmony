// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use machine::nes::ButtonChord;
use machine::{Machine, StopConditions, nes, quicknes::QuickNesMachine};
use sha2::{Digest, Sha256};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let schedule = args
        .next()
        .ok_or("usage: castlevania-probe <buttons:hold|pADDR=VALUE,...> <every> <addr,addr,...>")?;
    let every: u32 = args.next().ok_or("missing print interval")?.parse()?;
    let addrs = args
        .next()
        .ok_or("missing address list")?
        .split(',')
        .map(|a| usize::from_str_radix(a.trim_start_matches("0x"), 16))
        .collect::<Result<Vec<_>, _>>()?;
    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_CASTLEVANIA_ROM").ok_or("HARMONY_CASTLEVANIA_ROM")?,
    ))?;
    let core_path =
        PathBuf::from(env::var_os("HARMONY_QUICKNES_CORE").ok_or("HARMONY_QUICKNES_CORE")?);
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core_path)?));
    let mut machine = QuickNesMachine::from_rom_bytes(&rom, &core_path, &core_sha256)?;
    let shots = env::var_os("PROBE_SHOTS").map(PathBuf::from);
    let shot_every: u32 = env::var("PROBE_SHOT_EVERY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let diff = env::var_os("PROBE_DIFF").is_some();
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
    let mut prior = machine.read_wram()?;
    for step in schedule.split(',') {
        if let Some(poke) = step.strip_prefix('p') {
            let (address, value) = poke.split_once('=').ok_or("bad poke")?;
            machine.poke_wram(
                usize::from_str_radix(address, 16)?,
                u8::from_str_radix(value, 16)?,
            )?;
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
                if diff && w != &prior {
                    let diffs: Vec<String> = w
                        .iter()
                        .zip(prior.iter())
                        .enumerate()
                        .filter(|(i, (a, b))| a != b && *i < 0x100)
                        .map(|(i, (a, _))| format!("{i:03x}={a:02x}"))
                        .collect();
                    if !diffs.is_empty() {
                        println!("{frame:5} diff {}", diffs.join(" "));
                    }
                }
                if (every > 0 && frame.is_multiple_of(every))
                    || (every == 0 && addrs.iter().any(|a| w[*a] != prior[*a]))
                {
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
    if let Some(dir) = &shots {
        fs::write(dir.join("final-wram.bin"), prior)?;
    }
    Ok(())
}
