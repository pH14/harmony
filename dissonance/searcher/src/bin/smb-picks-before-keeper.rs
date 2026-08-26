// SPDX-License-Identifier: AGPL-3.0-or-later

//! Measure, from a recorded campaign stream, how many selections each
//! eventually-productive parent received before its first retained child,
//! pooled per entry, per cell, per progress band, and per room.
//!
//! Reads recorded artifacts only; no emulation, no mutation of any input
//! file. The distributions parameterize retirement thresholds: the report
//! names, for each pooling level, the smallest draw count at which fewer
//! than one in a hundred eventually-productive classes would have been
//! retired before their first keeper.

use std::{
    collections::BTreeMap,
    env,
    error::Error,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use searcher::smb::{
    archive::{SmbArchiveKey, SmbArchiveReport, SmbRoomIdentity},
    campaign::{SmbCampaignAdmissionDecision, SmbCampaignStreamHeader, SmbCampaignStreamRecord},
};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Progress-band width in buckets, matching the selector's band classes.
const BAND_WIDTH: u16 = 8;

/// One pooling level's running state: picks seen so far and, once the first
/// keeper lands, the number of picks that came before it.
#[derive(Default)]
struct PoolCounter {
    picks: u64,
    before_first_keeper: Option<u64>,
}

impl PoolCounter {
    fn record(&mut self, productive: bool) {
        if productive && self.before_first_keeper.is_none() {
            self.before_first_keeper = Some(self.picks);
        }
        self.picks = self.picks.saturating_add(1);
    }
}

/// Summary of one pooling level's picks-before-first-keeper distribution.
#[derive(Serialize)]
struct PoolSummary {
    /// Classes that received at least one pick.
    classes_picked: u64,
    /// Classes whose picks eventually produced a retained child.
    classes_productive: u64,
    /// Distribution of picks-before-first-keeper over productive classes.
    percentiles: BTreeMap<String, u64>,
    /// Smallest energy threshold at which fewer than one in a hundred
    /// productive classes would have been cut off before their first keeper.
    threshold_1_in_100: u64,
    /// Histogram of picks-before-first-keeper, bucketed by powers of two.
    log2_histogram: BTreeMap<u8, u64>,
}

#[derive(Serialize)]
struct Report {
    stream: PathBuf,
    stream_sha256: String,
    archive: PathBuf,
    archive_sha256: String,
    records: u64,
    picks: u64,
    picks_with_unknown_key: u64,
    truncated_tail_lines: u64,
    per_entry: PoolSummary,
    per_cell: PoolSummary,
    per_band: PoolSummary,
    per_room: PoolSummary,
}

type CellClass = (u8, u8, SmbRoomIdentity, u16, u8, u8, u8);
type BandClass = (u8, u8, SmbRoomIdentity, u16);
type RoomClass = (u8, u8, SmbRoomIdentity);

fn cell_of(key: &SmbArchiveKey) -> CellClass {
    (
        key.world,
        key.level,
        key.room,
        key.progress,
        key.player_y_bucket,
        key.player_engine_state,
        key.room_x_bucket,
    )
}

fn band_of(key: &SmbArchiveKey) -> BandClass {
    (key.world, key.level, key.room, key.progress / BAND_WIDTH)
}

fn room_of(key: &SmbArchiveKey) -> RoomClass {
    (key.world, key.level, key.room)
}

fn summarize(pools: impl Iterator<Item = PoolCounter>) -> PoolSummary {
    let mut classes_picked = 0_u64;
    let mut values: Vec<u64> = Vec::new();
    for pool in pools {
        classes_picked = classes_picked.saturating_add(1);
        if let Some(before) = pool.before_first_keeper {
            values.push(before);
        }
    }
    values.sort_unstable();
    let percentile = |p: u64| -> u64 {
        if values.is_empty() {
            return 0;
        }
        let rank = (u64::try_from(values.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(p))
        .div_ceil(100);
        let index = usize::try_from(rank.saturating_sub(1)).unwrap_or(0);
        values[index.min(values.len() - 1)]
    };
    let mut percentiles = BTreeMap::new();
    for p in [50, 90, 95, 99] {
        percentiles.insert(format!("p{p}"), percentile(p));
    }
    percentiles.insert("max".to_owned(), values.last().copied().unwrap_or(0));
    let mut log2_histogram = BTreeMap::<u8, u64>::new();
    for value in &values {
        let bucket = u8::try_from(64 - value.saturating_add(1).leading_zeros()).unwrap_or(64);
        *log2_histogram.entry(bucket).or_insert(0) += 1;
    }
    PoolSummary {
        classes_picked,
        classes_productive: u64::try_from(values.len()).unwrap_or(u64::MAX),
        threshold_1_in_100: percentile(99).saturating_add(1),
        percentiles,
        log2_histogram,
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let stream_path = PathBuf::from(
        args.next()
            .ok_or("usage: smb-picks-before-keeper <stream.jsonl> <archive.json> <output.json>")?,
    );
    let archive_path = PathBuf::from(args.next().ok_or("missing archive path")?);
    let output = PathBuf::from(args.next().ok_or("missing output path")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }

    let archive_bytes = fs::read(&archive_path)?;
    let archive_sha256 = format!("{:x}", Sha256::digest(&archive_bytes));
    let archive: SmbArchiveReport = serde_json::from_slice(&archive_bytes)?;
    drop(archive_bytes);
    let keys: BTreeMap<u64, SmbArchiveKey> = archive
        .entries
        .iter()
        .map(|entry| (entry.id, entry.key))
        .collect();
    drop(archive);

    let mut stream_hasher = Sha256::new();
    let mut per_entry = BTreeMap::<u64, PoolCounter>::new();
    let mut per_cell = BTreeMap::<CellClass, PoolCounter>::new();
    let mut per_band = BTreeMap::<BandClass, PoolCounter>::new();
    let mut per_room = BTreeMap::<RoomClass, PoolCounter>::new();
    let mut records = 0_u64;
    let mut picks = 0_u64;
    let mut picks_with_unknown_key = 0_u64;
    let mut truncated_tail_lines = 0_u64;

    let reader = BufReader::new(fs::File::open(&stream_path)?);
    let mut lines = reader.split(b'\n');
    let header_line = lines.next().ok_or("stream is empty")??;
    stream_hasher.update(&header_line);
    stream_hasher.update(b"\n");
    let header: SmbCampaignStreamHeader = serde_json::from_slice(&header_line)?;
    if header.parent_scheduler != "room_cell_uniform_128" {
        return Err(format!("unexpected parent scheduler {}", header.parent_scheduler).into());
    }
    for line in lines {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let record: SmbCampaignStreamRecord = match serde_json::from_slice(&line) {
            Ok(record) => record,
            // A live stream's final line can be a partial write; anything
            // unparseable past the header is counted and ends the walk.
            Err(_) => {
                truncated_tail_lines = truncated_tail_lines.saturating_add(1);
                break;
            }
        };
        stream_hasher.update(&line);
        stream_hasher.update(b"\n");
        records = records.saturating_add(1);
        let (parent_id, productive) = match &record {
            SmbCampaignStreamRecord::Job(job) => (
                job.parent_id,
                job.decisions.iter().any(|decision| {
                    matches!(decision, SmbCampaignAdmissionDecision::Retained { .. })
                }),
            ),
            SmbCampaignStreamRecord::Skip(skip) => (skip.parent_id, false),
        };
        picks = picks.saturating_add(1);
        per_entry.entry(parent_id).or_default().record(productive);
        let Some(key) = keys.get(&parent_id) else {
            picks_with_unknown_key = picks_with_unknown_key.saturating_add(1);
            continue;
        };
        per_cell.entry(cell_of(key)).or_default().record(productive);
        per_band.entry(band_of(key)).or_default().record(productive);
        per_room.entry(room_of(key)).or_default().record(productive);
    }

    let report = Report {
        stream: stream_path,
        stream_sha256: format!("{:x}", stream_hasher.finalize()),
        archive: archive_path,
        archive_sha256,
        records,
        picks,
        picks_with_unknown_key,
        truncated_tail_lines,
        per_entry: summarize(per_entry.into_values()),
        per_cell: summarize(per_cell.into_values()),
        per_band: summarize(per_band.into_values()),
        per_room: summarize(per_room.into_values()),
    };
    create_parent(&output)?;
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    for (name, pool) in [
        ("entry", &report.per_entry),
        ("cell", &report.per_cell),
        ("band", &report.per_band),
        ("room", &report.per_room),
    ] {
        println!(
            "{name}: picked {} productive {} p50 {} p99 {} max {} threshold {}",
            pool.classes_picked,
            pool.classes_productive,
            pool.percentiles["p50"],
            pool.percentiles["p99"],
            pool.percentiles["max"],
            pool.threshold_1_in_100,
        );
    }
    println!(
        "records {} picks {} unknown-key {} truncated {}",
        report.records, report.picks, report.picks_with_unknown_key, report.truncated_tail_lines
    );
    Ok(())
}

fn create_parent(path: &Path) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}
