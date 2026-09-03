// SPDX-License-Identifier: AGPL-3.0-or-later

//! Print selected work-RAM bytes after each chord of a schedule run from power-on.

use std::{env, error::Error, fs, path::PathBuf};

use machine::nes::ButtonChord;
use machine::{Machine, StopConditions, nes, quicknes::QuickNesMachine};
use sha2::{Digest, Sha256};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let schedule = args.next().ok_or("usage: smb-probe <buttons:hold,...>")?;
    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_SMB_ROM").ok_or("HARMONY_SMB_ROM")?,
    ))?;
    let core_path =
        PathBuf::from(env::var_os("HARMONY_QUICKNES_CORE").ok_or("HARMONY_QUICKNES_CORE")?);
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core_path)?));
    let mut machine = QuickNesMachine::from_rom_bytes(&rom, &core_path, &core_sha256)?;
    let mut frame = 0u32;
    println!("frame  0770 0772 000e 00b5 00ce 075f 075c 0746 071a 006d 0086 001d dead");
    for step in schedule.split(',') {
        let (b, h) = step.split_once(':').ok_or("bad step")?;
        let chord = ButtonChord {
            buttons: u8::from_str_radix(b.trim_start_matches("0x"), 16)?,
            hold_frames: h.parse()?,
        };
        let snap = machine.snapshot()?;
        machine.branch(snap, &nes::reproducer(&[chord]))?;
        machine.run(StopConditions::default(), None)?;
        machine.drop_snapshot(snap)?;
        frame += u32::from(chord.bounded_hold_frames());
        let w = machine.read_wram()?;
        let dead = w[0x0e] == 0x0b || w[0xb5] >= 2;
        println!(
            "{frame:5}  {:04x} {:04x} {:04x} {:04x} {:04x} {:04x} {:04x} {:04x} {:04x} {:04x} {:04x} {:04x} {dead}",
            w[0x770],
            w[0x772],
            w[0x0e],
            w[0xb5],
            w[0xce],
            w[0x75f],
            w[0x75c],
            w[0x746],
            w[0x71a],
            w[0x6d],
            w[0x86],
            w[0x1d]
        );
    }
    Ok(())
}
