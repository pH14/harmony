// SPDX-License-Identifier: AGPL-3.0-or-later

use consonance_client::session::SparseSnapshot;
use machine::{
    Machine, StopConditions, StopReason,
    nes::{ButtonChord, reproducer},
    quicknes::QuickNesMachine,
};
use nes_wasm::Package;
use sha2::{Digest, Sha256};
use std::{error::Error, path::Path};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 6 {
        return Err(
            "usage: qualify run|restore|compare PACKAGE ROM|SNAPSHOT OUTPUT CORE|none".into(),
        );
    }
    let package = Package::from_directory(Path::new(&args[2]))?;
    let mut machine = match args[1].as_str() {
        "run" | "compare" => package.machine(&std::fs::read(&args[3])?, 7)?,
        "restore" => package.restored_machine(&postcard::from_bytes::<SparseSnapshot>(
            &std::fs::read(&args[3])?,
        )?)?,
        _ => return Err("unknown qualification mode".into()),
    };
    let mut native = if args[1] == "compare" {
        Some(QuickNesMachine::from_rom_bytes(
            &std::fs::read(&args[3])?,
            Path::new(&args[5]),
            &format!("{:x}", Sha256::digest(std::fs::read(&args[5])?)),
        )?)
    } else {
        None
    };
    let initial_hash = machine.execution_hash()?;
    let mut trace = Vec::new();
    for (buttons, hold_frames) in [(0, 60), (8, 6), (0, 54)] {
        let snapshot = machine.snapshot()?;
        let input = reproducer(&[ButtonChord {
            buttons,
            hold_frames,
        }]);
        machine.branch(snapshot, &input)?;
        let stop = machine.run(StopConditions::default(), None)?;
        if !matches!(stop, StopReason::SnapshotPoint { .. }) {
            return Err(format!("unexpected action stop: {stop:?}").into());
        }
        if let Some(native) = &mut native {
            let snapshot = native.snapshot()?;
            native.branch(snapshot, &input)?;
            native.run(StopConditions::default(), None)?;
            if machine.frames() != native.frames()
                || machine.read(0, 2048)? != native.read_wram()?.to_vec()
                || machine.read(0x6000, 8192)? != native.read_save_ram()?
            {
                return Err("WASM/native game observation mismatch".into());
            }
        }
        let endpoint = machine.snapshot()?;
        let portable = machine.export(endpoint, None)?;
        let mut restored = package.restored_machine(&portable)?;
        if restored.execution_hash()? != machine.execution_hash()? {
            return Err("fresh restore changed complete state".into());
        }
        let expected = machine.execution_hash()?;
        let restored_snapshot = restored.snapshot()?;
        let same = reproducer(&[ButtonChord::new(0x81, 1)]);
        restored.branch(restored_snapshot, &same)?;
        restored.run(StopConditions::default(), None)?;
        let mut sibling = package.restored_machine(&portable)?;
        let sibling_snapshot = sibling.snapshot()?;
        sibling.branch(sibling_snapshot, &same)?;
        sibling.run(StopConditions::default(), None)?;
        if restored.execution_hash()? != sibling.execution_hash()? {
            return Err("restored branches diverged with identical inputs".into());
        }
        sibling.branch(sibling_snapshot, &reproducer(&[ButtonChord::new(0x40, 1)]))?;
        sibling.run(StopConditions::default(), None)?;
        if restored.execution_hash()? == sibling.execution_hash()? {
            return Err("different sibling inputs did not change complete state".into());
        }
        sibling.replay(sibling_snapshot)?;
        if sibling.execution_hash()? != expected || machine.execution_hash()? != expected {
            return Err("sibling execution contaminated its checkpoint or parent".into());
        }
        trace.push(serde_json::json!({"buttons": buttons, "frames": hold_frames, "state": machine.execution_hash()?, "work_ram_sha256": format!("{:x}", Sha256::digest(machine.read(0, 2048)?)), "save_ram_sha256": format!("{:x}", Sha256::digest(machine.read(0x6000, 8192)?))}));
        std::fs::write(
            format!("{}-{}.snapshot", args[4], trace.len()),
            postcard::to_stdvec(&portable)?,
        )?;
        machine.drop_snapshot(snapshot)?;
    }
    std::fs::write(
        &args[4],
        serde_json::to_vec_pretty(
            &serde_json::json!({"initial_state": initial_hash, "trace": trace, "host": [std::env::consts::OS, std::env::consts::ARCH], "native_compared": native.is_some()}),
        )?,
    )?;
    Ok(())
}
