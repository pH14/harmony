// SPDX-License-Identifier: AGPL-3.0-or-later

//! Print selected work-RAM bytes after every chord of a recorded input.
//!
//! The replay drives the same [`SmbTarget`] the searcher drives, so the trace
//! reports the bytes the search actually saw rather than a re-derivation.

use std::{env, error::Error, fs, path::PathBuf};

use searcher::{
    smb::target::{SmbInput, SmbTarget},
    target::Target,
};
use sha2::{Digest, Sha256};

const WATCHED: [(&str, usize); 16] = [
    ("0770", 0x0770),
    ("0772", 0x0772),
    ("000e", 0x000e),
    ("00b5", 0x00b5),
    ("00ce", 0x00ce),
    ("075f", 0x075f),
    ("075c", 0x075c),
    ("0760", 0x0760),
    ("0746", 0x0746),
    ("074e", 0x074e),
    ("074f", 0x074f),
    ("071a", 0x071a),
    ("071c", 0x071c),
    ("006d", 0x006d),
    ("0086", 0x0086),
    ("07f8", 0x07f8),
];

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let input_path = PathBuf::from(args.next().ok_or("usage: smb-trace <input.json>")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }

    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_SMB_ROM").ok_or("HARMONY_SMB_ROM must name the ROM")?,
    ))?;
    let core_path = PathBuf::from(
        env::var_os("HARMONY_QUICKNES_CORE").ok_or("HARMONY_QUICKNES_CORE must name the core")?,
    );
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core_path)?));
    let input: SmbInput = serde_json::from_slice(&fs::read(&input_path)?)?;

    let mut target = SmbTarget::from_smb_rom_bytes_headless(&rom, &core_path, &core_sha256)?;
    target.reset();

    let header: Vec<&str> = WATCHED.iter().map(|(name, _)| *name).collect();
    println!(
        "action\tframe\tbuttons\thold\t{}\tdead\tvictory",
        header.join("\t")
    );
    let mut frame = 0_u32;
    for (index, action) in input.actions.iter().enumerate() {
        if target.is_dead() || target.is_victory() {
            break;
        }
        target.apply(action);
        frame = frame.saturating_add(u32::from(action.bounded_hold_frames()));
        let wram = target.wram();
        let bytes: Vec<String> = WATCHED
            .iter()
            .map(|(_, addr)| format!("{:3}", wram[*addr]))
            .collect();
        println!(
            "{}\t{frame}\t{}\t{}\t{}\t{}\t{}",
            index + 1,
            action.buttons,
            action.bounded_hold_frames(),
            bytes.join("\t"),
            target.is_dead(),
            target.is_victory(),
        );
    }
    Ok(())
}
