// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded inspection of existing snapshots; never runs actions or search.
use nes_workload::{
    metroid::{
        archive::archive_key,
        boss_probe::BossContext,
        campaign::SNAPSHOT_CHECKPOINT_FORMAT,
        target::{MetroidSnapshot, MetroidTarget, MetroidTerminalPolicy},
    },
    search::{archive::ArchiveKey, campaign::SnapshotCheckpoint},
    target::Target,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, error::Error, fs, path::Path};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn bounded_read(path: &Path) -> Result<Vec<u8>> {
    if fs::metadata(path)?.len() > 64 * 1024 * 1024 {
        return Err("input exceeds 64 MiB".into());
    }
    Ok(fs::read(path)?)
}
fn decode(bytes: &[u8]) -> Result<SnapshotCheckpoint<MetroidSnapshot>> {
    let result =
        SnapshotCheckpoint::<MetroidSnapshot>::from_bytes(bytes, SNAPSHOT_CHECKPOINT_FORMAT)?;
    if result.entries.is_empty() || result.entries.len() > 2048 {
        return Err("expected 1..2048 cached snapshots".into());
    }
    let ids: BTreeSet<_> = result.entries.iter().map(|e| e.id).collect();
    if ids.len() != result.entries.len() {
        return Err("duplicate snapshot ID".into());
    }
    Ok(result)
}
fn metadata(id: u64, snapshot: &MetroidSnapshot) -> Result<Value> {
    let value = serde_json::to_value(snapshot)?;
    let obs = &value["observation"];
    Ok(
        json!({"id":id,"snapshot_sha256":sha(&postcard::to_allocvec(snapshot)?),
        "state":snapshot.state(),"retention_group":archive_key(snapshot.state()).group(0),
        "frame_count":obs["frame_count"],"dead":obs["dead"],"failed":value["failed"],
        "endpoint_boss_slots":obs["endpoint_boss_slots"]}),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    core: String,
    core_sha256: String,
    rom: String,
    rom_sha256: String,
    checkpoint: String,
    checkpoint_sha256: String,
    origin: String,
    origin_sha256: String,
    root_snapshot_sha256: String,
    expected_root_context: Value,
}
fn pinned(path: &str, expected: &str) -> Result<Vec<u8>> {
    let bytes = bounded_read(Path::new(path))?;
    if sha(&bytes) != expected {
        return Err(format!("identity mismatch: {path}").into());
    }
    Ok(bytes)
}
fn read_paused(target: &mut MetroidTarget, snapshot: &MetroidSnapshot) -> Result<BossContext> {
    let frames = target.frames_clocked();
    target.restore(snapshot)?;
    let context = target.diagnostic_boss_context()?;
    if target.frames_clocked() != frames || target.snapshot().as_ref() != Some(snapshot) {
        return Err("restore or diagnostic changed snapshot or advanced a frame".into());
    }
    Ok(context)
}
fn inspect(q: &Request) -> Result<Value> {
    let bytes = pinned(&q.checkpoint, &q.checkpoint_sha256)?;
    let checkpoint = decode(&bytes)?;
    let origin = decode(&pinned(&q.origin, &q.origin_sha256)?)?;
    if origin.entries.len() != 1 || origin.entries[0].id != 0 {
        return Err("origin must contain only ID zero".into());
    }
    let root = &origin.entries[0].snapshot;
    if sha(&postcard::to_allocvec(root)?) != q.root_snapshot_sha256 {
        return Err("origin snapshot mismatch".into());
    }
    let rom = pinned(&q.rom, &q.rom_sha256)?;
    pinned(&q.core, &q.core_sha256)?;
    // The only emulation in this command is ordinary constructor setup (929 frames).
    // No apply/run/execute call follows: each endpoint is read at its paused boundary.
    let mut target =
        MetroidTarget::from_rom_bytes_headless(&rom, Path::new(&q.core), &q.core_sha256)?
            .with_terminal_policy(MetroidTerminalPolicy::BcdUnderflow);
    if target.frames_clocked() != 929 {
        return Err("ordinary setup frame count changed".into());
    }
    let control = read_paused(&mut target, root)?;
    if serde_json::to_value(&control)? != q.expected_root_context {
        return Err("positive-control context changed".into());
    }
    let mut entries = Vec::with_capacity(checkpoint.entries.len());
    for entry in &checkpoint.entries {
        let mut row = metadata(entry.id, &entry.snapshot)?;
        row["context"] = serde_json::to_value(read_paused(&mut target, &entry.snapshot)?)?;
        entries.push(row);
    }
    Ok(json!({"format":"metroid-paused-checkpoint-context-v1",
        "checkpoint_sha256":sha(&bytes),"origin_sha256":q.origin_sha256,
        "positive_control":control,"entries":entries,"verified_restores":entries.len()+1,
        "direct_physical_frames":target.frames_clocked(),"setup_frames":929,"continuation_frames":0,
        "scope":"snapshot-local state observations; no lifetime damage or active membership inferred"}))
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [mode, input, output] = args.as_slice() else {
        return Err(
            "usage: metroid-checkpoint-inspect inventory CHECKPOINT OUT | inspect REQUEST OUT"
                .into(),
        );
    };
    if Path::new(output).exists() {
        return Err("output already exists".into());
    }
    let bytes = bounded_read(Path::new(input))?;
    let result = match mode.as_str() {
        "inventory" => {
            let checkpoint = decode(&bytes)?;
            let rows = checkpoint
                .entries
                .iter()
                .map(|e| metadata(e.id, &e.snapshot))
                .collect::<Result<Vec<_>>>()?;
            json!({"format":"metroid-checkpoint-inventory-v1","checkpoint_sha256":sha(&bytes),
                "checkpoint_format":checkpoint.format,"entries":rows,"emulated_frames":0,
                "scope":"all cached snapshots, including inactive history; active membership not encoded"})
        }
        "inspect" => {
            let mut report = inspect(&serde_json::from_slice(&bytes)?)?;
            report["request_sha256"] = Value::String(sha(&bytes));
            report
        }
        _ => return Err("unknown inspection mode".into()),
    };
    fs::write(output, serde_json::to_vec_pretty(&result)?)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checkpoint_format_and_duplicate_ids_are_rejected() {
        // A serialized empty snapshot is enough for decoder structure tests; never restored.
        let snapshot: MetroidSnapshot = serde_json::from_value(json!({"emulator_state":[],
            "observation":{"frame_count":0,"decoded":nes_workload::metroid::target::MetroidMechanicalState::default(),
                "boss_defeats":{"kraid":false,"ridley":false},"mother_brain_status":0,
                "tourian_events":{"mother_brain_defeated":false,"escape_started":false},
                "endpoint_boss_slots":null,"changed_indices":[],"dead":false,"log_line":""},
            "failed":false})).unwrap();
        let entry = nes_workload::search::campaign::SnapshotCheckpointEntry { id: 0, snapshot };
        let mut checkpoint = SnapshotCheckpoint {
            format: SNAPSHOT_CHECKPOINT_FORMAT.into(),
            entries: vec![entry.clone()],
        };
        assert!(decode(&checkpoint.to_bytes().unwrap()).is_ok());
        checkpoint.entries.push(entry);
        assert!(decode(&checkpoint.to_bytes().unwrap()).is_err());
        checkpoint.entries.pop();
        checkpoint.format = "unrecognized".into();
        assert!(decode(&checkpoint.to_bytes().unwrap()).is_err());
    }
}
