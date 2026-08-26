// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recorded campaign-mode conquest runs and their exact replays.

use std::{env, error::Error, fs, io::BufWriter, path::PathBuf, time::Duration};

use searcher::{
    search::archive::{
        MAX_ARCHIVE_ENTRIES, RetentionPolicy, RetireThresholds, SelectorPolicy,
        retention_policy_from_identifier,
    },
    search::draw::{
        DrawMixture, SuffixShape, draw_mixture_from_identifier, suffix_shape_from_identifier,
    },
    smb::archive::{MAX_SMB_COMPLETION_ACTIONS, SmbArchiveReport, selector_policy_from_identifier},
    smb::campaign::{
        SNAPSHOT_CHECKPOINT_FORMAT, SmbButtonVocabulary, SmbCampaignCheckpoint, SmbCampaignConfig,
        SmbCampaignModeReport, SmbCampaignOrigin, SmbSnapshotCheckpoint,
        button_vocabulary_from_identifier, chord_policy_from_identifier,
        replay_smb_campaign_checkpointed, run_smb_campaign_checkpointed,
    },
};
use serde::Serialize;
use sha2::{Digest, Sha256};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let mode = args.next().ok_or("usage: smb-campaign <run|replay> ...")?;
    if mode == "run" {
        return run_mode(&mut args);
    }
    if mode == "replay" {
        return replay_mode(&mut args);
    }
    Err("unknown smb-campaign mode".into())
}

/// Live-only wall measurements; never part of the replayable report.
#[derive(Debug, Serialize)]
struct LiveThroughput {
    wall_seconds: f64,
    executions_completed: u64,
    frames_emulated: u64,
    executions_per_second: f64,
    frames_per_second: f64,
}

#[allow(clippy::disallowed_methods)] // not order-observable: wall time is live throughput evidence only.
fn run_mode(args: &mut impl Iterator<Item = std::ffi::OsString>) -> Result<(), Box<dyn Error>> {
    let origin_arg = args
        .next()
        .ok_or("missing origin (genesis or a source archive path)")?;
    let campaign_seed = parse_u64(
        &args
            .next()
            .ok_or("missing campaign seed")?
            .to_string_lossy(),
    )?;
    let workers = u32::try_from(parse_u64(
        &args.next().ok_or("missing worker count")?.to_string_lossy(),
    )?)?;
    let execution_budget = parse_u64(
        &args
            .next()
            .ok_or("missing execution budget")?
            .to_string_lossy(),
    )?;
    let action_limit = usize::try_from(parse_u64(
        &args.next().ok_or("missing action limit")?.to_string_lossy(),
    )?)?;
    let host = args
        .next()
        .ok_or("missing host name")?
        .to_string_lossy()
        .into_owned();
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    let mut wall_budget = None;
    // Defaults are the current behavior; the older policies stay selectable
    // so historical recordings keep replaying under their own identifiers.
    // The chord draw biases half of each draw toward recently retained
    // button sequences, seeded from every retained entry of a source
    // archive; the fold parameters are the registered head-to-head winners.
    // Every run uses it: replaced draw policies survive only as stream
    // identifiers, never as run options.
    // Retire thresholds are measured search statistics (99th-percentile
    // picks-before-first-keeper per class) and should be re-measured for a
    // new game rather than treated as universal constants.
    let chord = chord_policy_from_identifier("chord_draw_recorded_51:all,0,128,3,1,64,1024")?;
    let mut retention = RetentionPolicy::AdmitAlive;
    let mut selector = SelectorPolicy::EnergyFrontier(RetireThresholds {
        entry: 3,
        groups: vec![6, 12, 2],
    });
    let mut vocabulary = SmbButtonVocabulary::default();
    let mut suffix = SuffixShape::default();
    let mut mixture = DrawMixture::Energy { scale: 6 };
    let mut checkpoint_path = None;
    while let Some(flag) = args.next() {
        if flag == "--wall-seconds" {
            let seconds = parse_u64(
                &args
                    .next()
                    .ok_or("missing --wall-seconds value")?
                    .to_string_lossy(),
            )?;
            wall_budget = Some(Duration::from_secs(seconds));
        } else if flag == "--retention" {
            retention = retention_policy_from_identifier(
                &args
                    .next()
                    .ok_or("missing --retention value")?
                    .to_string_lossy(),
            )?;
        } else if flag == "--selector" {
            selector = selector_policy_from_identifier(
                &args
                    .next()
                    .ok_or("missing --selector value")?
                    .to_string_lossy(),
            )?;
        } else if flag == "--vocabulary" {
            vocabulary = button_vocabulary_from_identifier(
                &args
                    .next()
                    .ok_or("missing --vocabulary value")?
                    .to_string_lossy(),
            )?;
        } else if flag == "--suffix" {
            suffix = suffix_shape_from_identifier(
                &args
                    .next()
                    .ok_or("missing --suffix value")?
                    .to_string_lossy(),
            )?;
        } else if flag == "--mixture" {
            mixture = draw_mixture_from_identifier(
                &args
                    .next()
                    .ok_or("missing --mixture value")?
                    .to_string_lossy(),
            )?;
        } else if flag == "--checkpoint" {
            checkpoint_path = Some(PathBuf::from(
                args.next().ok_or("missing --checkpoint value")?,
            ));
        } else {
            return Err("unexpected run argument".into());
        }
    }
    if action_limit > MAX_SMB_COMPLETION_ACTIONS {
        return Err("action limit exceeds the compiled completion bound".into());
    }
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let origin = load_origin(&origin_arg.to_string_lossy(), checkpoint_path.as_deref())?;
    let config = SmbCampaignConfig {
        vocabulary,
        campaign_seed,
        workers,
        execution_budget,
        action_limit,
        host,
        wall_budget,
        archive_entry_limit: MAX_ARCHIVE_ENTRIES,
        chord,
        retention,
        selector,
        suffix,
        mixture,
        victory_input_path: Some(output.join("victory-input.json")),
        checkpoint_dir: Some(output.clone()),
    };

    let stream_path = output.join("stream.jsonl");
    let stream_file = fs::File::create(&stream_path)?;
    let mut stream = BufWriter::new(stream_file);
    // Sidecar for live observation; separate file so the recorded stream is
    // untouched by it.
    let mut progress = BufWriter::new(fs::File::create(output.join("progress-live.jsonl"))?);
    let started = std::time::Instant::now();
    let (report, checkpoint) =
        run_smb_campaign_checkpointed(&rom, &config, &origin, &mut stream, Some(&mut progress))?;
    let wall_seconds = started.elapsed().as_secs_f64();
    drop(stream);

    write_report_files(
        &output,
        &report,
        "archive-live.json",
        "campaign-report.json",
    )?;
    fs::write(output.join("snapshots-live.bin"), checkpoint.to_bytes()?)?;
    let throughput = LiveThroughput {
        wall_seconds,
        executions_completed: report.executions_completed,
        frames_emulated: report.frames_emulated,
        executions_per_second: rate(report.executions_completed, wall_seconds),
        frames_per_second: rate(report.frames_emulated, wall_seconds),
    };
    fs::write(
        output.join("throughput-live.json"),
        serde_json::to_vec_pretty(&throughput)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary(&report))?);
    Ok(())
}

fn replay_mode(args: &mut impl Iterator<Item = std::ffi::OsString>) -> Result<(), Box<dyn Error>> {
    let run_dir = PathBuf::from(args.next().ok_or("missing recorded run directory")?);
    let origin_arg = args
        .next()
        .ok_or("missing origin (genesis or the recorded source archive path)")?;
    let checkpoint_arg = args.next().map(PathBuf::from);
    if args.next().is_some() {
        return Err("unexpected extra replay argument".into());
    }
    let rom = read_rom()?;
    let stream_bytes = fs::read(run_dir.join("stream.jsonl"))?;
    let origin_report = match origin_arg.to_string_lossy().as_ref() {
        "genesis" => None,
        path => Some(serde_json::from_slice::<SmbArchiveReport>(&fs::read(
            path,
        )?)?),
    };
    let origin_checkpoint = checkpoint_arg.as_deref().map(load_checkpoint).transpose()?;
    let (report, checkpoint) = replay_smb_campaign_checkpointed(
        &rom,
        &stream_bytes,
        origin_report.as_ref(),
        origin_checkpoint.as_ref(),
    )?;
    write_report_files(
        &run_dir,
        &report,
        "archive-replay.json",
        "campaign-report-replay.json",
    )?;
    fs::write(run_dir.join("snapshots-replay.bin"), checkpoint.to_bytes()?)?;

    let archive_live = fs::read(run_dir.join("archive-live.json"))?;
    let archive_replay = fs::read(run_dir.join("archive-replay.json"))?;
    let report_live = fs::read(run_dir.join("campaign-report.json"))?;
    let report_replay = fs::read(run_dir.join("campaign-report-replay.json"))?;
    let snapshots_live = fs::read(run_dir.join("snapshots-live.bin"))?;
    let snapshots_replay = fs::read(run_dir.join("snapshots-replay.bin"))?;
    let replay_verified = archive_live == archive_replay
        && report_live == report_replay
        && snapshots_live == snapshots_replay;
    let verdict = serde_json::json!({
        "replay_verified": replay_verified,
        "archive_sha256": format!("{:x}", Sha256::digest(&archive_live)),
        "report_sha256": format!("{:x}", Sha256::digest(&report_live)),
        "stream_sha256": report.stream_sha256,
        "executions_completed": report.executions_completed,
    });
    fs::write(
        run_dir.join("replay-verdict.json"),
        serde_json::to_vec_pretty(&verdict)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&verdict)?);
    if !replay_verified {
        return Err("campaign replay diverged from the recorded run".into());
    }
    Ok(())
}

fn load_origin(
    origin_arg: &str,
    checkpoint_path: Option<&std::path::Path>,
) -> Result<SmbCampaignOrigin, Box<dyn Error>> {
    if origin_arg == "genesis" {
        if checkpoint_path.is_some() {
            return Err("a snapshot checkpoint needs a source archive origin".into());
        }
        return Ok(SmbCampaignOrigin::Genesis);
    }
    let bytes = fs::read(origin_arg)?;
    let report: SmbArchiveReport = serde_json::from_slice(&bytes)?;
    Ok(SmbCampaignOrigin::Archive {
        path: origin_arg.to_owned(),
        file_sha256: format!("{:x}", Sha256::digest(&bytes)),
        report: Box::new(report),
        checkpoint: checkpoint_path.map(load_checkpoint).transpose()?,
    })
}

fn load_checkpoint(path: &std::path::Path) -> Result<SmbCampaignCheckpoint, Box<dyn Error>> {
    let bytes = fs::read(path)?;
    Ok(SmbCampaignCheckpoint {
        path: path.to_string_lossy().into_owned(),
        file_sha256: format!("{:x}", Sha256::digest(&bytes)),
        snapshots: SmbSnapshotCheckpoint::from_bytes(&bytes, SNAPSHOT_CHECKPOINT_FORMAT)?,
    })
}

fn write_report_files(
    directory: &std::path::Path,
    report: &SmbCampaignModeReport,
    archive_name: &str,
    report_name: &str,
) -> Result<(), Box<dyn Error>> {
    fs::write(
        directory.join(archive_name),
        serde_json::to_vec_pretty(&report.archive)?,
    )?;
    fs::write(
        directory.join(report_name),
        serde_json::to_vec_pretty(report)?,
    )?;
    Ok(())
}

fn summary(report: &SmbCampaignModeReport) -> serde_json::Value {
    serde_json::json!({
        "mode": report.mode,
        "campaign_seed": report.campaign_seed,
        "workers": report.workers,
        "host": report.host,
        "origin": report.origin.kind,
        "executions_completed": report.executions_completed,
        "execution_budget": report.execution_budget,
        "milestones": report.archive.milestones,
        "progress_watermark": report.archive.progress_watermark,
        "entries": report.archive.entries.len(),
        "retained": report.archive.retained,
        "rejected": report.archive.rejected,
        "deaths": report.archive.deaths,
        "victories": report.victories,
        "probe_refused": report.probe_refused,
        "duplicates_skipped": report.duplicates_skipped,
        "frames_emulated": report.frames_emulated,
        "jobs_per_worker": report.jobs_per_worker,
        "stream_sha256": report.stream_sha256,
    })
}

fn read_rom() -> Result<Vec<u8>, Box<dyn Error>> {
    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    Ok(fs::read(rom_path)?)
}

#[allow(clippy::cast_precision_loss)] // Throughput display only; counts stay far below 2^52.
fn rate(count: u64, wall_seconds: f64) -> f64 {
    if wall_seconds > 0.0 {
        count as f64 / wall_seconds
    } else {
        0.0
    }
}

fn parse_u64(value: &str) -> Result<u64, Box<dyn Error>> {
    let normalized = value.replace('_', "");
    if let Some(hex) = normalized.strip_prefix("0x") {
        Ok(u64::from_str_radix(hex, 16)?)
    } else {
        Ok(normalized.parse()?)
    }
}
