// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deterministic M7 PNG frame-strip generator for recorded SMB milestone inputs.

use std::{
    cmp::Reverse,
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use fuzzer::{
    phase4b::{
        SmbCampaignReport, SmbConfiguredReport, SmbExecutionWork, SmbExecutorMode, SmbInput,
        SmbMechanicalState, SmbMilestones, SmbObservations, SmbTarget, encode_smb_frame_png,
        observe_smb_input, smb_mechanical_state_from_wram, smb_milestones_from_wram,
    },
    phase4c::SmbArchiveReport,
    target::Target,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Deserialize)]
struct M5Report {
    ratchet: Vec<SmbCampaignReport>,
}

#[derive(Debug, Serialize)]
struct FilmManifest {
    source_report: PathBuf,
    milestone: String,
    rom_sha256: String,
    input: SmbInput,
    frames: Vec<String>,
    action_boundaries: Vec<FilmBoundary>,
    observation_events: Vec<FilmObservation>,
}

#[derive(Debug, Serialize)]
struct FilmBoundary {
    action_count: usize,
    raw_wram: Vec<u8>,
    decoded: SmbMechanicalState,
    milestones: SmbMilestones,
}

#[derive(Debug, Serialize)]
struct FilmObservation {
    action_count: usize,
    frame_count: u64,
    raw_wram: Vec<u8>,
    decoded: SmbMechanicalState,
    milestones: SmbMilestones,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let mode = args.next().ok_or(
        "usage: smb-film <m5|campaign|configured|archive|archive-frontier|archive-key|archive-id> <report> [run-index|world level progress|archive-id] <milestone> <output-dir>",
    )?;
    let source = PathBuf::from(args.next().ok_or("missing source report")?);
    let (campaign, milestone, output) = match mode.to_str() {
        Some("m5") => {
            let run_index: usize = args
                .next()
                .ok_or("missing M5 run index")?
                .to_string_lossy()
                .parse()?;
            let milestone = args
                .next()
                .ok_or("missing milestone")?
                .to_string_lossy()
                .into_owned();
            let output = PathBuf::from(args.next().ok_or("missing output directory")?);
            let report: M5Report = serde_json::from_slice(&fs::read(&source)?)?;
            let campaign = report
                .ratchet
                .get(run_index)
                .ok_or("M5 run index is out of bounds")?
                .clone();
            (campaign, milestone, output)
        }
        Some("campaign") => {
            let milestone = args
                .next()
                .ok_or("missing milestone")?
                .to_string_lossy()
                .into_owned();
            let output = PathBuf::from(args.next().ok_or("missing output directory")?);
            let campaign: SmbCampaignReport = serde_json::from_slice(&fs::read(&source)?)?;
            (campaign, milestone, output)
        }
        Some("configured") => {
            let milestone = args
                .next()
                .ok_or("missing milestone")?
                .to_string_lossy()
                .into_owned();
            let output = PathBuf::from(args.next().ok_or("missing output directory")?);
            let report: SmbConfiguredReport = serde_json::from_slice(&fs::read(&source)?)?;
            (report.campaign, milestone, output)
        }
        Some("archive") => {
            let milestone = args
                .next()
                .ok_or("missing milestone")?
                .to_string_lossy()
                .into_owned();
            let output = PathBuf::from(args.next().ok_or("missing output directory")?);
            let report: SmbArchiveReport = serde_json::from_slice(&fs::read(&source)?)?;
            let campaign = SmbCampaignReport {
                seed: report.seed,
                executions: report.executions,
                milestones: report.milestones,
                progress_watermark: report.progress_watermark,
                first_reached: report.first_reached,
                first_inputs: report.first_inputs,
                corpus: vec![report.champion_input],
                corpus_entries: Vec::new(),
                executor_mode: SmbExecutorMode::SnapshotResume,
                executor_work: SmbExecutionWork::default(),
            };
            (campaign, milestone, output)
        }
        Some("archive-frontier") => {
            let output = PathBuf::from(args.next().ok_or("missing output directory")?);
            let report: SmbArchiveReport = serde_json::from_slice(&fs::read(&source)?)?;
            let frontier = report
                .entries
                .iter()
                .max_by_key(|entry| {
                    (
                        entry.key.world,
                        entry.key.level,
                        entry.key.progress,
                        Reverse(entry.input.actions.len()),
                        Reverse(entry.id),
                    )
                })
                .map(|entry| entry.input.clone())
                .ok_or("source archive contains no retained entries")?;
            let mut first_inputs = report.first_inputs;
            first_inputs.progress_into_1_1 = Some(frontier);
            let campaign = SmbCampaignReport {
                seed: report.seed,
                executions: report.executions,
                milestones: report.milestones,
                progress_watermark: report.progress_watermark,
                first_reached: report.first_reached,
                first_inputs,
                corpus: vec![report.champion_input],
                corpus_entries: Vec::new(),
                executor_mode: SmbExecutorMode::SnapshotResume,
                executor_work: SmbExecutionWork::default(),
            };
            (campaign, "progress".to_owned(), output)
        }
        Some("archive-key") => {
            let world: u8 = args
                .next()
                .ok_or("missing archive-key world")?
                .to_string_lossy()
                .parse()?;
            let level: u8 = args
                .next()
                .ok_or("missing archive-key level")?
                .to_string_lossy()
                .parse()?;
            let progress: u16 = args
                .next()
                .ok_or("missing archive-key progress")?
                .to_string_lossy()
                .parse()?;
            let output = PathBuf::from(args.next().ok_or("missing output directory")?);
            let report: SmbArchiveReport = serde_json::from_slice(&fs::read(&source)?)?;
            let selected = report
                .entries
                .iter()
                .filter(|entry| {
                    entry.key.world == world
                        && entry.key.level == level
                        && entry.key.progress == progress
                })
                .min_by_key(|entry| (entry.input.actions.len(), entry.id))
                .map(|entry| entry.input.clone())
                .ok_or("source archive contains no entry at the requested mechanical key")?;
            let mut first_inputs = report.first_inputs;
            first_inputs.progress_into_1_1 = Some(selected);
            let campaign = SmbCampaignReport {
                seed: report.seed,
                executions: report.executions,
                milestones: report.milestones,
                progress_watermark: report.progress_watermark,
                first_reached: report.first_reached,
                first_inputs,
                corpus: vec![report.champion_input],
                corpus_entries: Vec::new(),
                executor_mode: SmbExecutorMode::SnapshotResume,
                executor_work: SmbExecutionWork::default(),
            };
            (campaign, "progress".to_owned(), output)
        }
        Some("archive-id") => {
            let archive_id: u64 = args
                .next()
                .ok_or("missing archive entry id")?
                .to_string_lossy()
                .parse()?;
            let output = PathBuf::from(args.next().ok_or("missing output directory")?);
            let report: SmbArchiveReport = serde_json::from_slice(&fs::read(&source)?)?;
            let selected = report
                .entries
                .iter()
                .find(|entry| entry.id == archive_id)
                .map(|entry| entry.input.clone())
                .ok_or("source archive contains no entry with the requested id")?;
            let mut first_inputs = report.first_inputs;
            first_inputs.progress_into_1_1 = Some(selected);
            let campaign = SmbCampaignReport {
                seed: report.seed,
                executions: report.executions,
                milestones: report.milestones,
                progress_watermark: report.progress_watermark,
                first_reached: report.first_reached,
                first_inputs,
                corpus: vec![report.champion_input],
                corpus_entries: Vec::new(),
                executor_mode: SmbExecutorMode::SnapshotResume,
                executor_work: SmbExecutionWork::default(),
            };
            (campaign, "progress".to_owned(), output)
        }
        _ => return Err("unknown film source mode".into()),
    };
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    let rom = fs::read(rom_path)?;
    let input = milestone_input(&campaign, &milestone, &rom)?;
    fs::create_dir_all(&output)?;
    let mut target = SmbTarget::from_smb_rom_bytes(&rom)?;
    let mut frames = Vec::new();
    let mut action_boundaries = vec![film_boundary(0, &target)];
    let mut observation_events = Vec::new();
    write_frame(&output, 0, &target.frame_rgba(), &mut frames)?;
    for (index, action) in input.actions.iter().enumerate() {
        target.apply(action);
        if target.exit_kind() != libafl::executors::ExitKind::Ok {
            return Err(format!("emulator failed while rendering action {index}").into());
        }
        for observation in target.last_action_observations() {
            observation_events.push(film_observation(index + 1, observation)?);
        }
        action_boundaries.push(film_boundary(index + 1, &target));
        write_frame(&output, index + 1, &target.frame_rgba(), &mut frames)?;
        if target.is_dead() {
            break;
        }
    }
    let manifest = FilmManifest {
        source_report: source,
        milestone,
        rom_sha256: format!("{:x}", Sha256::digest(&rom)),
        input,
        frames,
        action_boundaries,
        observation_events,
    };
    fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&manifest)?);
    Ok(())
}

fn film_observation(
    action_count: usize,
    observation: &SmbObservations,
) -> Result<FilmObservation, Box<dyn Error>> {
    let wram: &[u8; 2_048] = observation
        .wram
        .as_slice()
        .try_into()
        .map_err(|_| "film observation WRAM is not exactly 2 KiB")?;
    Ok(FilmObservation {
        action_count,
        frame_count: observation.frame_count,
        raw_wram: observation.wram.clone(),
        decoded: smb_mechanical_state_from_wram(wram),
        milestones: smb_milestones_from_wram(wram),
    })
}

fn film_boundary(action_count: usize, target: &SmbTarget) -> FilmBoundary {
    FilmBoundary {
        action_count,
        raw_wram: target.wram().to_vec(),
        decoded: smb_mechanical_state_from_wram(target.wram()),
        milestones: smb_milestones_from_wram(target.wram()),
    }
}

fn milestone_input(
    campaign: &SmbCampaignReport,
    milestone: &str,
    rom: &[u8],
) -> Result<SmbInput, Box<dyn Error>> {
    match milestone {
        "progress" => campaign.first_inputs.progress_into_1_1.clone(),
        "flag" => campaign.first_inputs.flag_1_1.clone(),
        "1-2" => campaign.first_inputs.level_1_2.clone(),
        "onward" => campaign.first_inputs.onward.clone(),
        "max-scroll" => {
            let target = campaign.milestones.max_1_1_scroll_bucket;
            for input in &campaign.corpus {
                let mut reached = 0_u16;
                for observation in observe_smb_input(rom, input)? {
                    let wram = observation
                        .wram
                        .as_slice()
                        .try_into()
                        .map_err(|_| "observation WRAM is not exactly 2 KiB")?;
                    reached = reached.max(smb_milestones_from_wram(wram).max_1_1_scroll_bucket);
                }
                if reached == target {
                    return Ok(input.clone());
                }
            }
            return Err(format!(
                "campaign corpus has no input reaching recorded max scroll bucket {target}"
            )
            .into());
        }
        _ => {
            return Err(
                "unknown milestone (expected progress, max-scroll, flag, 1-2, or onward)".into(),
            );
        }
    }
    .ok_or_else(|| format!("campaign has no first-reaching input for {milestone}").into())
}

fn write_frame(
    directory: &Path,
    index: usize,
    rgba: &[u8],
    frames: &mut Vec<String>,
) -> Result<(), Box<dyn Error>> {
    let filename = format!("frame-{index:04}.png");
    fs::write(directory.join(&filename), encode_smb_frame_png(rgba)?)?;
    frames.push(filename);
    Ok(())
}
