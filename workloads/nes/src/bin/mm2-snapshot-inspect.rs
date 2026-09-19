// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use nes_workload::mm2::{
    campaign::{Mm2SnapshotCheckpoint, SNAPSHOT_CHECKPOINT_FORMAT},
    target::Mm2Snapshot,
};
use serde_json::{Value, json};

const USAGE: &str = "usage: mm2-snapshot-inspect <snapshots.bin> [--all]";

struct SnapshotStatus {
    frame_count: u64,
    dead: bool,
    dying_run: u32,
    failed: bool,
    coherent_world: bool,
}

fn snapshot_status(snapshot: &Mm2Snapshot) -> Result<SnapshotStatus, Box<dyn Error>> {
    let value = serde_json::to_value(snapshot)?;
    let observation = value
        .get("observation")
        .and_then(Value::as_object)
        .ok_or("snapshot observation is absent")?;
    Ok(SnapshotStatus {
        frame_count: observation
            .get("frame_count")
            .and_then(Value::as_u64)
            .ok_or("snapshot frame_count is absent")?,
        dead: observation
            .get("dead")
            .and_then(Value::as_bool)
            .ok_or("snapshot dead state is absent")?,
        dying_run: observation
            .get("dying_run")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or("snapshot dying_run is absent")?,
        failed: value
            .get("failed")
            .and_then(Value::as_bool)
            .ok_or("snapshot failure state is absent")?,
        coherent_world: value
            .get("coherent_world")
            .and_then(Value::as_bool)
            .ok_or("snapshot coherent-world state is absent")?,
    })
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let path = PathBuf::from(args.next().ok_or(USAGE)?);
    let mut include_terminal = false;
    for flag in args {
        match flag.as_str() {
            "--all" => include_terminal = true,
            _ => return Err(USAGE.into()),
        }
    }

    let bytes = fs::read(&path)?;
    let checkpoint = Mm2SnapshotCheckpoint::from_bytes(&bytes, SNAPSHOT_CHECKPOINT_FORMAT)?;
    let total = checkpoint.entries.len();
    let mut emitted = 0_usize;
    let mut filtered = 0_usize;
    for entry in checkpoint.entries {
        let status = snapshot_status(&entry.snapshot)?;
        let passes_status_filter = !status.failed && !status.dead && status.dying_run == 0;
        if !include_terminal && !passes_status_filter {
            filtered = filtered.saturating_add(1);
            continue;
        }
        let state = entry.snapshot.state();
        println!(
            "{}",
            serde_json::to_string(&json!({
                "id": entry.id,
                "frame_count": status.frame_count,
                "dead": status.dead,
                "dying_run": status.dying_run,
                "failed": status.failed,
                "coherent_world": status.coherent_world,
                "passes_status_filter": passes_status_filter,
                "state": state,
            }))?
        );
        emitted = emitted.saturating_add(1);
    }
    eprintln!(
        "# checkpoint={} format={} entries={} emitted={} filtered={}",
        path.display(),
        SNAPSHOT_CHECKPOINT_FORMAT,
        total,
        emitted,
        filtered
    );
    Ok(())
}
