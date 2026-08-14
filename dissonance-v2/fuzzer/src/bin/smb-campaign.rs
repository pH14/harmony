// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recorded campaign-mode conquest runs, their exact replays, and the serial
//! throughput arm.

use std::{env, error::Error, fs, io::BufWriter, path::PathBuf, time::Duration};

use fuzzer::{
    campaign::{
        SmbCampaignConfig, SmbCampaignModeReport, SmbCampaignOrigin, replay_smb_campaign,
        run_smb_campaign, select_frontier_resume_input, selector_from_identifier,
    },
    phase4b::SmbInput,
    phase4c::{
        MAX_SMB_COMPLETION_ACTIONS, SmbArchiveDurationPolicy, SmbArchiveReport,
        SmbArchiveRetentionPolicy, SmbArchiveSelectorPolicy, SmbArchiveSuffixPolicy,
        run_smb_archive_search_with_retention_and_work,
    },
};
use serde::Serialize;
use sha2::{Digest, Sha256};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let mode = args
        .next()
        .ok_or("usage: smb-campaign <run|replay|serial-arm> ...")?;
    if mode == "run" {
        return run_mode(&mut args);
    }
    if mode == "replay" {
        return replay_mode(&mut args);
    }
    if mode == "serial-arm" {
        return serial_arm_mode(&mut args);
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

/// Serial throughput arm: the frozen serial engine on the same origin.
#[derive(Debug, Serialize)]
struct SerialArmReport {
    seed: u64,
    execution_budget: u64,
    executions_completed: u64,
    frames_emulated: u64,
    progress_watermark: fuzzer::phase4b::SmbProgressWatermark,
    milestones: fuzzer::phase4b::SmbMilestones,
    entries: usize,
    retained: u64,
    rejected: u64,
    deaths: u64,
    wall_seconds: f64,
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
    let mut selector_policy = SmbArchiveSelectorPolicy::Frozen;
    while let Some(flag) = args.next() {
        if flag == "--wall-seconds" {
            let seconds = parse_u64(
                &args
                    .next()
                    .ok_or("missing --wall-seconds value")?
                    .to_string_lossy(),
            )?;
            wall_budget = Some(Duration::from_secs(seconds));
        } else if flag == "--selector" {
            selector_policy = selector_from_identifier(
                &args
                    .next()
                    .ok_or("missing --selector value")?
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
    let origin = load_origin(&origin_arg.to_string_lossy())?;
    let config = SmbCampaignConfig {
        campaign_seed,
        workers,
        execution_budget,
        action_limit,
        host,
        wall_budget,
        selector_policy,
    };

    let stream_path = output.join("stream.jsonl");
    let stream_file = fs::File::create(&stream_path)?;
    let mut stream = BufWriter::new(stream_file);
    let started = std::time::Instant::now();
    let report = run_smb_campaign(&rom, &config, &origin, &mut stream)?;
    let wall_seconds = started.elapsed().as_secs_f64();
    drop(stream);

    write_report_files(
        &output,
        &report,
        "archive-live.json",
        "campaign-report.json",
    )?;
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
    let report = replay_smb_campaign(&rom, &stream_bytes, origin_report.as_ref())?;
    write_report_files(
        &run_dir,
        &report,
        "archive-replay.json",
        "campaign-report-replay.json",
    )?;

    let archive_live = fs::read(run_dir.join("archive-live.json"))?;
    let archive_replay = fs::read(run_dir.join("archive-replay.json"))?;
    let report_live = fs::read(run_dir.join("campaign-report.json"))?;
    let report_replay = fs::read(run_dir.join("campaign-report-replay.json"))?;
    let replay_verified = archive_live == archive_replay && report_live == report_replay;
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

#[allow(clippy::disallowed_methods)] // not order-observable: wall time is live throughput evidence only.
fn serial_arm_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let origin_arg = args
        .next()
        .ok_or("missing origin (genesis or a source archive path)")?;
    let seed = parse_u64(&args.next().ok_or("missing seed")?.to_string_lossy())?;
    let execution_budget = parse_u64(
        &args
            .next()
            .ok_or("missing execution budget")?
            .to_string_lossy(),
    )?;
    let action_limit = usize::try_from(parse_u64(
        &args.next().ok_or("missing action limit")?.to_string_lossy(),
    )?)?;
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    if args.next().is_some() {
        return Err("unexpected extra serial-arm argument".into());
    }
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let initial = match load_origin(&origin_arg.to_string_lossy())? {
        SmbCampaignOrigin::Genesis => SmbInput::default(),
        SmbCampaignOrigin::Archive { report, .. } => select_frontier_resume_input(&report)?,
    };
    let started = std::time::Instant::now();
    let (report, frames_emulated) = run_smb_archive_search_with_retention_and_work(
        &rom,
        std::slice::from_ref(&initial),
        seed,
        execution_budget,
        action_limit,
        SmbArchiveDurationPolicy::Stratified,
        SmbArchiveSuffixPolicy::OneOrTwo,
        SmbArchiveRetentionPolicy::ProbeAtAdmission,
    )?;
    let wall_seconds = started.elapsed().as_secs_f64();
    fs::write(
        output.join("serial-archive-live.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    let arm = SerialArmReport {
        seed,
        execution_budget,
        executions_completed: report.executions,
        frames_emulated,
        progress_watermark: report.progress_watermark,
        milestones: report.milestones,
        entries: report.entries.len(),
        retained: report.retained,
        rejected: report.rejected,
        deaths: report.deaths,
        wall_seconds,
    };
    fs::write(
        output.join("serial-arm.json"),
        serde_json::to_vec_pretty(&arm)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&arm)?);
    Ok(())
}

fn load_origin(origin_arg: &str) -> Result<SmbCampaignOrigin, Box<dyn Error>> {
    if origin_arg == "genesis" {
        return Ok(SmbCampaignOrigin::Genesis);
    }
    let bytes = fs::read(origin_arg)?;
    let report: SmbArchiveReport = serde_json::from_slice(&bytes)?;
    Ok(SmbCampaignOrigin::Archive {
        path: origin_arg.to_owned(),
        file_sha256: format!("{:x}", Sha256::digest(&bytes)),
        report: Box::new(report),
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
