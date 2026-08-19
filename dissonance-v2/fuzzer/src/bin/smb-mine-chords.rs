// SPDX-License-Identifier: AGPL-3.0-or-later

//! Mine reproducible recent and all-history chord tables from any SMB archive.

use std::{
    collections::BTreeMap,
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use fuzzer::{
    chord_table::{ChordTableCheckpoint, ChordTableParameters, ChordTables},
    phase4b::ButtonChord,
    phase4c::SmbArchiveReport,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Serialize)]
struct SmbSourceFilter {
    world: u8,
    level: u8,
    minimum_progress: u16,
}

#[derive(Debug, Serialize)]
struct MiningReport {
    source: PathBuf,
    source_sha256: String,
    filter: SmbSourceFilter,
    parameters: ChordTableParameters,
    entries_examined: u64,
    entries_used: u64,
    checkpoint: ChordTableCheckpoint,
    first_success_execution: Option<u64>,
    recent_window_start_execution: Option<u64>,
    last_success_execution: Option<u64>,
    recent: Vec<ButtonChord>,
    all_history: Vec<ButtonChord>,
    mixed_chords: usize,
    mask_histogram: BTreeMap<u8, u64>,
    hold_histogram_by_12s: BTreeMap<u8, u64>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let source = PathBuf::from(args.next().ok_or(
        "usage: smb-mine-chords <archive.json> <world> <level> <minimum-progress> <prefix-steps> <recent-successes> <recent-weight> <all-history-weight> <update-every-records> <hash-every-records> <output.json>",
    )?);
    let filter = SmbSourceFilter {
        world: parse_next(&mut args, "world")?,
        level: parse_next(&mut args, "level")?,
        minimum_progress: parse_next(&mut args, "minimum progress")?,
    };
    let parameters = ChordTableParameters {
        prefix_steps: parse_next(&mut args, "prefix steps")?,
        recent_successes: parse_next(&mut args, "recent successes")?,
        recent_weight: parse_next(&mut args, "recent weight")?,
        all_history_weight: parse_next(&mut args, "all-history weight")?,
        update_every_records: parse_next(&mut args, "update interval")?,
        hash_every_records: parse_next(&mut args, "hash interval")?,
    };
    let output = PathBuf::from(args.next().ok_or("missing output path")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }

    let source_bytes = fs::read(&source)?;
    let archive: SmbArchiveReport = serde_json::from_slice(&source_bytes)?;
    let mut tables = ChordTables::new(parameters)?;
    let mut entries_used = 0_u64;
    let mut success_executions = Vec::new();
    for entry in &archive.entries {
        if (entry.key.world, entry.key.level) == (filter.world, filter.level)
            && entry.key.progress >= filter.minimum_progress
            && entry.input.actions.len() > parameters.prefix_steps
        {
            tables.fold_retained(&entry.input.actions)?;
            entries_used = entries_used.saturating_add(1);
            success_executions.push(entry.created_execution);
        }
    }
    tables.flush()?;
    let checkpoint = tables.checkpoint()?;
    let mixed_len = tables.mixed_len()?;
    let mut mask_histogram = BTreeMap::<u8, u64>::new();
    let mut hold_histogram_by_12s = BTreeMap::<u8, u64>::new();
    for chord in tables.all_history() {
        *mask_histogram.entry(chord.buttons).or_insert(0) += 1;
        *hold_histogram_by_12s
            .entry(chord.hold_frames / 12)
            .or_insert(0) += 1;
    }
    let report = MiningReport {
        source,
        source_sha256: format!("{:x}", Sha256::digest(source_bytes)),
        filter,
        parameters,
        entries_examined: u64::try_from(archive.entries.len()).unwrap_or(u64::MAX),
        entries_used,
        checkpoint,
        first_success_execution: success_executions.first().copied(),
        recent_window_start_execution: success_executions
            .len()
            .checked_sub(parameters.recent_successes)
            .and_then(|index| success_executions.get(index))
            .copied()
            .or_else(|| success_executions.first().copied()),
        last_success_execution: success_executions.last().copied(),
        recent: tables.recent().to_vec(),
        all_history: tables.all_history().to_vec(),
        mixed_chords: mixed_len,
        mask_histogram,
        hold_histogram_by_12s,
    };
    create_parent(&output)?;
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "entries {} chords {} recent {} mixed {} table_sha256 {}",
        report.entries_used,
        report.all_history.len(),
        report.recent.len(),
        report.mixed_chords,
        report.checkpoint.table_sha256
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
