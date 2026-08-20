// SPDX-License-Identifier: AGPL-3.0-or-later

//! Measure parent-specific discovery yield from one recorded SMB campaign.

use std::{
    env,
    error::Error,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use fuzzer::{
    search::yield_measurement::{
        YieldMeasurementParameters, YieldObservation, measure_parent_yield,
    },
    smb::archive::SmbSelectorPath,
    smb::campaign::{SmbCampaignAdmissionDecision, SmbCampaignStreamRecord},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Deserialize)]
struct ArchiveKeyProjection {
    world: u8,
    level: u8,
    progress: u16,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct ArchiveEntryProjection {
    id: u64,
    key: ArchiveKeyProjection,
}

#[derive(Debug, Deserialize)]
struct ArchiveProjection {
    entries: Vec<ArchiveEntryProjection>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SmbClass {
    world: u8,
    level: u8,
    progress_band: u16,
}

#[derive(Debug, Serialize)]
struct SmbParentYieldReport {
    stream: PathBuf,
    stream_sha256: String,
    own_archive: PathBuf,
    own_archive_sha256: String,
    parent_id_resolution: &'static str,
    class_projection: &'static str,
    non_tie_class_jobs_excluded: u64,
    skipped_records_excluded: u64,
    measurement: fuzzer::search::yield_measurement::YieldMeasurementReport,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let stream = PathBuf::from(args.next().ok_or(
        "usage: smb-parent-yield <stream.jsonl> <own-archive.json> <class-window> <parent-window> <minimum-parent-history> <class-prior-strength> <output.json>",
    )?);
    let own_archive = PathBuf::from(args.next().ok_or("missing own archive path")?);
    let parameters = YieldMeasurementParameters {
        class_window: parse_next(&mut args, "class window")?,
        parent_window: parse_next(&mut args, "parent window")?,
        minimum_parent_history: parse_next(&mut args, "minimum parent history")?,
        class_prior_strength: parse_next(&mut args, "class prior strength")?,
    };
    parameters.validate()?;
    let output = PathBuf::from(args.next().ok_or("missing output path")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }

    let archive_bytes = fs::read(&own_archive)?;
    let archive: ArchiveProjection = serde_json::from_slice(&archive_bytes)?;
    let stream_bytes = fs::read(&stream)?;
    let mut lines = BufReader::new(stream_bytes.as_slice()).lines();
    let _header = lines.next().ok_or("campaign stream is empty")??;
    let mut observations = Vec::new();
    let mut non_tie_class_jobs = 0_u64;
    let mut skipped_records = 0_u64;
    for line in lines {
        match serde_json::from_str::<SmbCampaignStreamRecord>(&line?)? {
            SmbCampaignStreamRecord::Job(job) => {
                if !job
                    .selector
                    .as_ref()
                    .is_some_and(|draw| draw.path == SmbSelectorPath::TieClass)
                {
                    non_tie_class_jobs = non_tie_class_jobs.saturating_add(1);
                    continue;
                }
                let parent_index = usize::try_from(job.parent_id)?;
                let entry = archive
                    .entries
                    .get(parent_index)
                    .ok_or("stream parent id does not resolve against the run's own archive")?;
                if entry.id != job.parent_id {
                    return Err("own archive ids are not insertion-order indexes".into());
                }
                observations.push(YieldObservation {
                    parent: job.parent_id,
                    class: SmbClass {
                        world: entry.key.world,
                        level: entry.key.level,
                        progress_band: entry.key.progress / 8,
                    },
                    productive: job.decisions.iter().any(|decision| {
                        matches!(decision, SmbCampaignAdmissionDecision::Retained { .. })
                    }),
                    cost: job.frames,
                });
            }
            SmbCampaignStreamRecord::Skip(_) => {
                skipped_records = skipped_records.saturating_add(1);
            }
        }
    }
    let measurement = measure_parent_yield(observations, parameters)?;
    let report = SmbParentYieldReport {
        stream,
        stream_sha256: format!("{:x}", Sha256::digest(&stream_bytes)),
        own_archive,
        own_archive_sha256: format!("{:x}", Sha256::digest(&archive_bytes)),
        parent_id_resolution: "stream ids index this run's own archive (GitHub issue #189)",
        class_projection: "(world, level, floor(progress / 8)); stable proxy for the selector's dynamic eight-bucket tie class",
        non_tie_class_jobs_excluded: non_tie_class_jobs,
        skipped_records_excluded: skipped_records,
        measurement,
    };
    create_parent(&output)?;
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
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
