// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recorded campaign-mode conquest runs and their exact replays.

use std::{
    env,
    error::Error,
    fs,
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

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
        SmbButtonVocabulary, SmbCampaignCheckpoint, SmbCampaignConfig, SmbCampaignModeReport,
        SmbCampaignOrigin, SmbGame, SmbSnapshotCheckpoint, SmbTerminalPredicate,
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
    // The chord draw takes its button sequences from only the most recent
    // retained window, so the visible table tracks the current level's
    // successful presses and old regimes age out on their own.
    // Every run uses it: replaced draw policies survive only as stream
    // identifiers, never as run options.
    // Retire thresholds are measured search statistics (99th-percentile
    // picks-before-first-keeper per class) and should be re-measured for a
    // new game rather than treated as universal constants.
    let chord = chord_policy_from_identifier("chord_draw_recorded_52:all,0,128,3,1,64,1024")?;
    let mut retention = RetentionPolicy::AdmitAlive;
    let mut selector = SelectorPolicy::EnergyFrontierCheapest(RetireThresholds {
        entry: 3,
        groups: vec![6, 12, 2],
    });
    let mut vocabulary = SmbButtonVocabulary::default();
    let mut terminal = SmbTerminalPredicate::GameVictory;
    let mut suffix = SuffixShape::default();
    let mut mixture = DrawMixture::EnergySplice { scale: 6 };
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
        } else if flag == "--terminal" {
            terminal = SmbTerminalPredicate::from_identifier(
                &args
                    .next()
                    .ok_or("missing --terminal value")?
                    .to_string_lossy(),
            )?;
        } else {
            return Err("unexpected run argument".into());
        }
    }
    if action_limit > MAX_SMB_COMPLETION_ACTIONS {
        return Err("action limit exceeds the compiled completion bound".into());
    }
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let game = selected_game(&rom)?;
    let checkpoint_format = game.snapshot_checkpoint_format();
    let origin = load_origin(
        &origin_arg.to_string_lossy(),
        checkpoint_path.as_deref(),
        checkpoint_format,
    )?;
    let config = SmbCampaignConfig {
        vocabulary,
        terminal,
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
        run_smb_campaign_checkpointed(&game, &config, &origin, &mut stream, Some(&mut progress))?;
    let wall_seconds = started.elapsed().as_secs_f64();
    drop(stream);

    write_report_files(
        &output,
        &report,
        "archive-live.json",
        "campaign-report.json",
    )?;
    write_snapshot_file(&output.join("snapshots-live.bin"), &checkpoint)?;
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
    let game = selected_game(&rom)?;
    let checkpoint_format = game.snapshot_checkpoint_format();
    let stream_bytes = fs::read(run_dir.join("stream.jsonl"))?;
    let origin_name = origin_arg.to_string_lossy();
    let (origin_report, origin_checkpoint) = if origin_name == "genesis" {
        if checkpoint_arg.is_some() {
            return Err("genesis replay does not accept a snapshot checkpoint".into());
        }
        (None, None)
    } else if let Some(logical_path) = origin_name.strip_prefix("snapshot-root:") {
        if logical_path.is_empty() {
            return Err("snapshot-root origin needs a nonempty logical checkpoint path".into());
        }
        let checkpoint_path = checkpoint_arg
            .as_deref()
            .ok_or("snapshot-root replay needs a checkpoint file")?;
        (
            None,
            Some(load_checkpoint_as(
                checkpoint_path,
                logical_path.to_owned(),
                checkpoint_format,
            )?),
        )
    } else {
        (
            Some(serde_json::from_slice::<SmbArchiveReport>(&fs::read(
                origin_name.as_ref(),
            )?)?),
            checkpoint_arg
                .as_deref()
                .map(|path| load_checkpoint(path, checkpoint_format))
                .transpose()?,
        )
    };
    let (report, checkpoint) = replay_smb_campaign_checkpointed(
        &game,
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
    write_snapshot_file(&run_dir.join("snapshots-replay.bin"), &checkpoint)?;

    let stream_sha256 = report.stream_sha256.clone();
    let executions_completed = report.executions_completed;
    drop(checkpoint);
    drop(report);
    let archive_live = run_dir.join("archive-live.json");
    let archive_replay = run_dir.join("archive-replay.json");
    let report_live = run_dir.join("campaign-report.json");
    let report_replay = run_dir.join("campaign-report-replay.json");
    let snapshots_live = run_dir.join("snapshots-live.bin");
    let snapshots_replay = run_dir.join("snapshots-replay.bin");
    let replay_verified = files_equal(&archive_live, &archive_replay)?
        && files_equal(&report_live, &report_replay)?
        && files_equal(&snapshots_live, &snapshots_replay)?;
    let verdict = serde_json::json!({
        "replay_verified": replay_verified,
        "archive_sha256": sha256_file(&archive_live)?,
        "report_sha256": sha256_file(&report_live)?,
        "stream_sha256": stream_sha256,
        "executions_completed": executions_completed,
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

fn files_equal(left: &Path, right: &Path) -> Result<bool, Box<dyn Error>> {
    if fs::metadata(left)?.len() != fs::metadata(right)?.len() {
        return Ok(false);
    }
    let mut left = BufReader::new(fs::File::open(left)?);
    let mut right = BufReader::new(fs::File::open(right)?);
    let mut left_buffer = vec![0_u8; 1024 * 1024];
    let mut right_buffer = vec![0_u8; 1024 * 1024];
    loop {
        let left_read = left.read(&mut left_buffer)?;
        let right_read = right.read(&mut right_buffer)?;
        if left_read != right_read || left_buffer[..left_read] != right_buffer[..right_read] {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
    }
}

fn sha256_file(path: &Path) -> Result<String, Box<dyn Error>> {
    let mut file = BufReader::new(fs::File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn load_origin(
    origin_arg: &str,
    checkpoint_path: Option<&std::path::Path>,
    checkpoint_format: &str,
) -> Result<SmbCampaignOrigin, Box<dyn Error>> {
    if origin_arg == "genesis" {
        if checkpoint_path.is_some() {
            return Err("genesis does not accept a snapshot checkpoint".into());
        }
        return Ok(SmbCampaignOrigin::Genesis);
    }
    if let Some(logical_path) = origin_arg.strip_prefix("snapshot-root:") {
        if logical_path.is_empty() {
            return Err("snapshot-root origin needs a nonempty logical checkpoint path".into());
        }
        let checkpoint_path = checkpoint_path.ok_or("snapshot-root origin needs --checkpoint")?;
        return Ok(SmbCampaignOrigin::SnapshotRoot {
            checkpoint: load_checkpoint_as(
                checkpoint_path,
                logical_path.to_owned(),
                checkpoint_format,
            )?,
        });
    }
    let file_sha256 = sha256_file(Path::new(origin_arg))?;
    let report: SmbArchiveReport =
        serde_json::from_reader(BufReader::new(fs::File::open(origin_arg)?))?;
    Ok(SmbCampaignOrigin::Archive {
        path: origin_arg.to_owned(),
        file_sha256,
        report: Box::new(report),
        checkpoint: checkpoint_path
            .map(|path| load_checkpoint(path, checkpoint_format))
            .transpose()?,
    })
}

fn load_checkpoint(
    path: &std::path::Path,
    checkpoint_format: &str,
) -> Result<SmbCampaignCheckpoint, Box<dyn Error>> {
    load_checkpoint_as(path, path.to_string_lossy().into_owned(), checkpoint_format)
}

fn load_checkpoint_as(
    path: &std::path::Path,
    logical_path: String,
    checkpoint_format: &str,
) -> Result<SmbCampaignCheckpoint, Box<dyn Error>> {
    let mut scratch = vec![0_u8; 1024 * 1024];
    let (snapshots, _) = postcard::from_io((
        BufReader::new(fs::File::open(path)?),
        scratch.as_mut_slice(),
    ))?;
    let snapshots: SmbSnapshotCheckpoint = snapshots;
    if snapshots.format != checkpoint_format {
        return Err("snapshot checkpoint format is not recognized".into());
    }
    Ok(SmbCampaignCheckpoint {
        path: logical_path,
        file_sha256: sha256_file(path)?,
        snapshots,
    })
}

fn write_report_files(
    directory: &std::path::Path,
    report: &SmbCampaignModeReport,
    archive_name: &str,
    report_name: &str,
) -> Result<(), Box<dyn Error>> {
    write_json_pretty(&directory.join(archive_name), &report.archive)?;
    write_json_pretty(&directory.join(report_name), report)?;
    Ok(())
}

fn write_json_pretty(path: &Path, value: &impl Serialize) -> Result<(), Box<dyn Error>> {
    let mut writer = BufWriter::new(fs::File::create(path)?);
    serde_json::to_writer_pretty(&mut writer, value)?;
    writer.flush()?;
    Ok(())
}

fn write_snapshot_file(
    path: &Path,
    checkpoint: &SmbSnapshotCheckpoint,
) -> Result<(), Box<dyn Error>> {
    let mut writer = BufWriter::new(fs::File::create(path)?);
    postcard::to_io(checkpoint, &mut writer)?;
    writer.flush()?;
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

fn selected_game(rom: &[u8]) -> Result<SmbGame, Box<dyn Error>> {
    let core_path = PathBuf::from(
        env::var_os("HARMONY_QUICKNES_CORE")
            .ok_or("HARMONY_QUICKNES_CORE must name the pinned libretro core")?,
    );
    let core = fs::read(&core_path)?;
    let core_sha256 = format!("{:x}", Sha256::digest(&core));
    Ok(SmbGame::new(rom, &core_path, &core_sha256))
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
