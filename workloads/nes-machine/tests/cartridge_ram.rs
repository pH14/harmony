// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(all(unix, not(miri)))]

use machine::{Machine, nes::with_cartridge_ram, quicknes::QuickNesMachine};
use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf};

#[test]
#[ignore = "requires HARMONY_QUICKNES_CORE and source-built HARMONY_NOVA_ROM"]
fn declared_cartridge_ram_survives_real_core_restore() {
    let core = PathBuf::from(env::var_os("HARMONY_QUICKNES_CORE").expect("core path"));
    let rom_path = env::var_os("HARMONY_NOVA_ROM").expect("source-built Nova path");
    let rom = with_cartridge_ram(&fs::read(rom_path).expect("read ROM")).expect("iNES image");
    let hash = format!("{:x}", Sha256::digest(fs::read(&core).expect("read core")));
    let mut first = QuickNesMachine::from_rom_bytes(&rom, &core, &hash).expect("first core");
    let initial = first.read_save_ram().expect("cartridge RAM");
    assert!(!initial.is_empty(), "the image must expose cartridge RAM");
    assert!(initial.iter().all(|byte| *byte == 0xff));
    let marker: Vec<_> = (0..initial.len())
        .map(|i| (i as u8).wrapping_mul(73))
        .collect();
    first.write_save_ram(0, &marker).expect("write marker");
    let snapshot = first.snapshot().expect("capture marker");
    let bytes = first.take_snapshot(snapshot).expect("portable snapshot");
    first.write_save_ram(0, &initial).expect("clobber marker");
    assert_ne!(first.read_save_ram().unwrap(), marker);
    first.restore_bytes(&bytes).expect("restore in same core");
    assert_eq!(first.read_save_ram().unwrap(), marker);

    let mut second = QuickNesMachine::from_rom_bytes(&rom, &core, &hash).expect("second core");
    assert_eq!(second.read_save_ram().unwrap(), initial);
    second
        .restore_bytes(&bytes)
        .expect("restore in independent core");
    assert_eq!(second.read_save_ram().unwrap(), marker);
}
