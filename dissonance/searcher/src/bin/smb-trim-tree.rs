// SPDX-License-Identifier: AGPL-3.0-or-later

//! Trim a recorded whole-tree checkpoint to one (world, level) pair's
//! subtree: the archive report keeps only that pair's entries (optionally
//! bounded by creation execution), and the snapshot checkpoint keeps only
//! their snapshots. A campaign resuming the trimmed pair starts from the
//! bootstrap state plus exactly that subtree, imported through the normal
//! whole-tree path, which re-roots entries whose parents were trimmed away.
//!
//! The snapshot checkpoint may be piped on stdin (`-`) so a remote copy can
//! stream through without landing whole on disk; the report prints the
//! SHA-256 of the bytes it consumed for verification against the source.

use std::{
    env,
    error::Error,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use searcher::smb::{
    archive::SmbArchiveReport,
    campaign::{SmbSnapshotCheckpoint, SmbSnapshotCheckpointEntry},
};
use sha2::{Digest, Sha256};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let archive_path = PathBuf::from(args.next().ok_or(
        "usage: smb-trim-tree <archive.json> <snapshots.bin|-> <world> <level> \
         <max-created-execution|none> <out-archive.json> <out-snapshots.bin>",
    )?);
    let snapshots_arg = args.next().ok_or("missing snapshots path")?;
    let world: u8 = parse_next(&mut args, "world")?;
    let level: u8 = parse_next(&mut args, "level")?;
    let bound_arg = args
        .next()
        .ok_or("missing creation bound")?
        .to_string_lossy()
        .into_owned();
    let bound: Option<u64> = if bound_arg == "none" {
        None
    } else {
        Some(bound_arg.parse()?)
    };
    let out_archive = PathBuf::from(args.next().ok_or("missing output archive path")?);
    let out_snapshots = PathBuf::from(args.next().ok_or("missing output snapshots path")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }

    let archive_bytes = fs::read(&archive_path)?;
    let archive_sha256 = format!("{:x}", Sha256::digest(&archive_bytes));
    let mut archive: SmbArchiveReport = serde_json::from_slice(&archive_bytes)?;
    drop(archive_bytes);
    let keep = |entry: &searcher::smb::archive::SmbArchiveEntryReport| -> bool {
        (entry.key.world, entry.key.level) == (world, level)
            && bound.is_none_or(|bound| entry.created_execution <= bound)
    };
    let kept_ids: std::collections::BTreeSet<u64> = archive
        .entries
        .iter()
        .filter(|entry| keep(entry))
        .map(|entry| entry.id)
        .collect();
    archive.entries.retain(|entry| kept_ids.contains(&entry.id));
    // A kept entry whose parent was trimmed away must carry its full input
    // on the wire, which the suffix encoder already does when the parent is
    // not in the serialized list; parent ids are cleared so the import
    // re-roots them explicitly rather than chasing missing ids.
    for entry in &mut archive.entries {
        if let Some(parent) = entry.parent_id
            && !kept_ids.contains(&parent)
        {
            entry.parent_id = None;
        }
    }

    let mut snapshot_bytes = Vec::new();
    if snapshots_arg == "-" {
        std::io::stdin().lock().read_to_end(&mut snapshot_bytes)?;
    } else {
        snapshot_bytes = fs::read(PathBuf::from(&snapshots_arg))?;
    }
    let snapshots_sha256 = format!("{:x}", Sha256::digest(&snapshot_bytes));
    let checkpoint = SmbSnapshotCheckpoint::from_bytes(&snapshot_bytes)?;
    drop(snapshot_bytes);
    let kept_snapshots: Vec<SmbSnapshotCheckpointEntry> = checkpoint
        .entries
        .into_iter()
        .filter(|entry| kept_ids.contains(&entry.id))
        .collect();
    let trimmed = SmbSnapshotCheckpoint {
        format: checkpoint.format,
        entries: kept_snapshots,
    };

    create_parent(&out_archive)?;
    create_parent(&out_snapshots)?;
    let archive_out_bytes = serde_json::to_vec(&archive)?;
    fs::write(&out_archive, &archive_out_bytes)?;
    let snapshots_out_bytes = trimmed.to_bytes()?;
    fs::write(&out_snapshots, &snapshots_out_bytes)?;
    println!(
        "kept {} entries, {} snapshots\nsource archive sha256 {archive_sha256}\nsource snapshots sha256 {snapshots_sha256}\ntrimmed archive sha256 {:x}\ntrimmed snapshots sha256 {:x}",
        kept_ids.len(),
        trimmed.entries.len(),
        Sha256::digest(&archive_out_bytes),
        Sha256::digest(&snapshots_out_bytes),
    );
    Ok(())
}

fn parse_next<T>(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
    name: &str,
) -> Result<T, Box<dyn Error>>
where
    T: std::str::FromStr,
    T::Err: Error + 'static,
{
    Ok(args
        .next()
        .ok_or_else(|| format!("missing {name}"))?
        .to_string_lossy()
        .parse()?)
}

fn create_parent(path: &Path) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}
