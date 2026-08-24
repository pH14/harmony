// SPDX-License-Identifier: AGPL-3.0-or-later

//! Compare, within one (world, level) pair of a recorded campaign, the
//! button masks and hold lengths of retained children's added actions
//! against the first chords of jobs whose child died before any boundary,
//! and against the uniform draw's expectation.
//!
//! Reads recorded artifacts only; no emulation. Dead jobs re-derive their
//! suffix from the recorded mutation seed; only the first chord is counted
//! because execution stops at the death. Recorded for a later
//! input-generation change; nothing here alters any mechanism.

use std::{
    collections::BTreeMap,
    env,
    error::Error,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use fuzzer::smb::{
    archive::SmbArchiveReport,
    campaign::{
        SmbCampaignAdmissionDecision, SmbCampaignChordPolicy, SmbCampaignStreamHeader,
        SmbCampaignStreamRecord, derive_suffix,
    },
    target::ButtonChord,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Default, Serialize)]
struct ChordHistogram {
    chords: u64,
    mask_counts: BTreeMap<u8, u64>,
    /// Hold lengths bucketed by twelve frames, matching the stratified
    /// draw's short (2..=12) and long (96..=120) bands.
    hold_counts_by_12s: BTreeMap<u8, u64>,
    long_hold_share: f64,
}

impl ChordHistogram {
    fn record(&mut self, chord: ButtonChord) {
        self.chords = self.chords.saturating_add(1);
        *self.mask_counts.entry(chord.buttons).or_insert(0) += 1;
        *self
            .hold_counts_by_12s
            .entry(chord.bounded_hold_frames() / 12)
            .or_insert(0) += 1;
    }

    fn finish(&mut self) {
        let long: u64 = self
            .hold_counts_by_12s
            .iter()
            .filter(|(bucket, _)| **bucket >= 8)
            .map(|(_, count)| *count)
            .sum();
        self.long_hold_share = if self.chords > 0 {
            long as f64 / self.chords as f64
        } else {
            0.0
        };
    }
}

#[derive(Serialize)]
struct Report {
    stream: PathBuf,
    archive: PathBuf,
    archive_sha256: String,
    world: u8,
    level: u8,
    jobs_in_pair: u64,
    dead_jobs: u64,
    retained_added: ChordHistogram,
    dead_first_chords: ChordHistogram,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let stream_path = PathBuf::from(args.next().ok_or(
        "usage: smb-input-shapes <stream.jsonl> <archive.json> <world> <level> <output.json>",
    )?);
    let archive_path = PathBuf::from(args.next().ok_or("missing archive path")?);
    let world: u8 = parse_next(&mut args, "world")?;
    let level: u8 = parse_next(&mut args, "level")?;
    let output = PathBuf::from(args.next().ok_or("missing output path")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }

    let archive_bytes = fs::read(&archive_path)?;
    let archive_sha256 = format!("{:x}", Sha256::digest(&archive_bytes));
    let archive: SmbArchiveReport = serde_json::from_slice(&archive_bytes)?;
    drop(archive_bytes);
    let index_of: BTreeMap<u64, usize> = archive
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.id, index))
        .collect();

    // Retained children of parents inside the pair: the actions past the
    // parent's input.
    let mut retained_added = ChordHistogram::default();
    for entry in &archive.entries {
        let Some(parent) = entry.parent_id.and_then(|id| index_of.get(&id)) else {
            continue;
        };
        let parent = &archive.entries[*parent];
        if (parent.key.world, parent.key.level) != (world, level) {
            continue;
        }
        for action in &entry.input.actions[parent.input.actions.len()..] {
            retained_added.record(*action);
        }
    }

    // Jobs in the pair whose child died before any boundary: no admission
    // decision was recorded, so death came during the first action.
    let mut dead_first_chords = ChordHistogram::default();
    let mut jobs_in_pair = 0_u64;
    let mut dead_jobs = 0_u64;
    let reader = BufReader::new(fs::File::open(&stream_path)?);
    let mut lines = reader.split(b'\n');
    let header_line = lines.next().ok_or("stream is empty")??;
    let header: SmbCampaignStreamHeader = serde_json::from_slice(&header_line)?;
    if header.chord_policy != "chord_uniform" {
        return Err(format!("unexpected chord policy {}", header.chord_policy).into());
    }
    for line in lines {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<SmbCampaignStreamRecord>(&line) else {
            break;
        };
        let SmbCampaignStreamRecord::Job(job) = record else {
            continue;
        };
        let Some(parent) = index_of.get(&job.parent_id) else {
            continue;
        };
        let parent = &archive.entries[*parent];
        if (parent.key.world, parent.key.level) != (world, level) {
            continue;
        }
        jobs_in_pair = jobs_in_pair.saturating_add(1);
        let victory = job
            .decisions
            .iter()
            .any(|d| matches!(d, SmbCampaignAdmissionDecision::Victory));
        if job.decisions.is_empty() && !victory {
            dead_jobs = dead_jobs.saturating_add(1);
            let suffix = derive_suffix(job.mutation_seed, SmbCampaignChordPolicy::Uniform, None)?;
            if let Some(first) = suffix.first() {
                dead_first_chords.record(*first);
            }
        }
    }
    retained_added.finish();
    dead_first_chords.finish();

    let report = Report {
        stream: stream_path,
        archive: archive_path,
        archive_sha256,
        world,
        level,
        jobs_in_pair,
        dead_jobs,
        retained_added,
        dead_first_chords,
    };
    create_parent(&output)?;
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "jobs {} dead {} retained-chords {} dead-chords {}",
        report.jobs_in_pair,
        report.dead_jobs,
        report.retained_added.chords,
        report.dead_first_chords.chords
    );
    for (name, histogram) in [
        ("retained", &report.retained_added),
        ("dead", &report.dead_first_chords),
    ] {
        println!(
            "{name}: masks {:?} long-hold {:.3}",
            histogram.mask_counts, histogram.long_hold_share
        );
    }
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
