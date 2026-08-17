// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deterministic champion–challenger campaigns for the SMB completion experiment.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    fs,
    path::PathBuf,
};

use fuzzer::{
    phase2::{Flag, Interest, TriageLabels},
    phase4b::{
        NullSmbDetector, NullSmbMacro, SmbArtifactConfig, SmbCampaignReport, SmbConfiguredReport,
        SmbInput, SmbLabeledCorpusEntry, SmbMilestones, encode_smb_frame_png, observe_smb_input,
        run_smb_restart_configured, smb_milestones_from_wram,
    },
    phase4c::{
        MAX_SMB_COMPLETION_ACTIONS, SmbArchiveDurationPolicy, SmbArchiveKeyPolicy,
        SmbArchiveLadderPolicy, SmbArchiveReport, SmbArchiveRetentionPolicy,
        SmbArchiveSelectorPolicy, SmbArchiveSuffixPolicy, SmbControlCensusReport,
        SmbPlayerColumnSelection, SmbSteerScanReport, audit_smb_frontier_viability,
        audit_smb_player_column_contrast, audit_smb_player_column_derived,
        audit_smb_player_column_from_ids, audit_smb_player_column_separation,
        audit_smb_player_column_spread, audit_smb_player_column_verified,
        audit_smb_player_column_with_selection, audit_smb_terminal_death,
        census_smb_control_authority, derive_smb_ladder, diagnose_smb_film_columns,
        diagnose_smb_film_measurements, diagnose_smb_film_measurements_derived,
        diagnose_smb_left_direction, diagnose_smb_player_column, diagnose_smb_span,
        gate_smb_live_control, measure_smb_viable_progress, readmit_smb_archive,
        run_smb_archive_search, run_smb_archive_search_with_policies,
        run_smb_archive_search_with_retention, run_smb_archive_search_with_selector,
        select_smb_responsive_audit_ids, select_smb_span_audit_ids, select_smb_spread_audit_ids,
        select_smb_steered_audit_ids,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const M12_PILOT_SEED: u64 = 0x5eed_dc00;
const M12_PILOT_EXECUTIONS: u64 = 500;
const FRONTIER_RESUME_INPUTS: usize = 64;
const CENSUS_AUDIT_ENTRIES: usize = 8;
const TWO_STATE_AUDIT_ENTRIES: usize = 2;

#[derive(Clone, Copy)]
enum ResumeSelection {
    Champion,
    Frontier,
    FrontierCellSet,
    FrontierSet,
    FrontierBandSet,
    /// M53 rule: shortest input at an operator-supplied play bucket of the
    /// source archive's maximal `(world, level)` pair.
    PlayBucket,
}

#[derive(Debug, Deserialize)]
struct M5Report {
    ratchet: Vec<SmbCampaignReport>,
}

#[derive(Debug, Serialize)]
struct BaselineReproduction {
    base_commit: &'static str,
    rom_sha256: String,
    source_seed: u64,
    source_executions: u64,
    source_max_x: u16,
    source_corpus_count: usize,
    pilot_seed: u64,
    pilot_executions: u64,
    pilot_max_x: u16,
    pilot_corpus_count: usize,
    no_model_campaign_replay_verified: bool,
    champion_milestones: SmbMilestones,
    champion_input_sha256: String,
    champion_observations_sha256: String,
    champion_observation_replay_verified: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let mode = args
        .next()
        .ok_or("usage: smb-completion <reproduce-baseline|control|archive> ...")?;
    if mode == "control" {
        return run_control_mode(&mut args);
    }
    if mode == "archive" {
        return run_archive_mode(&mut args);
    }
    if mode == "audit-frontier-viability" {
        return run_frontier_viability_mode(&mut args);
    }
    if mode == "audit-player-column" {
        return run_player_column_mode(&mut args, SmbPlayerColumnSelection::FirstOrdered);
    }
    if mode == "audit-steerable-player-column" {
        return run_player_column_mode(&mut args, SmbPlayerColumnSelection::FirstSteerable);
    }
    if mode == "audit-advancing-player-column" {
        return run_player_column_mode(&mut args, SmbPlayerColumnSelection::FirstCameraAdvancing);
    }
    if mode == "census-control-authority" {
        return run_control_census_mode(&mut args);
    }
    if mode == "audit-census-player-column" {
        return run_census_player_column_mode(&mut args, false);
    }
    if mode == "audit-spread-player-column" {
        return run_census_player_column_mode(&mut args, true);
    }
    if mode == "audit-terminal-death" {
        return run_terminal_death_mode(&mut args);
    }
    if mode == "audit-steered-player-column" {
        return run_steered_player_column_mode(&mut args);
    }
    if mode == "audit-responsive-player-column" {
        return run_responsive_player_column_mode(&mut args, false, CENSUS_AUDIT_ENTRIES, 0);
    }
    if mode == "audit-span-player-column" {
        return run_responsive_player_column_mode(&mut args, true, CENSUS_AUDIT_ENTRIES, 0);
    }
    if mode == "audit-two-state-player-column" {
        return run_responsive_player_column_mode(&mut args, true, TWO_STATE_AUDIT_ENTRIES, 0);
    }
    if mode == "audit-probed-player-column" {
        return run_responsive_player_column_mode(&mut args, true, CENSUS_AUDIT_ENTRIES, 1);
    }
    if mode == "audit-verified-player-column" {
        return run_responsive_player_column_mode(&mut args, true, CENSUS_AUDIT_ENTRIES, 2);
    }
    if mode == "audit-derived-player-column" {
        return run_responsive_player_column_mode(&mut args, true, CENSUS_AUDIT_ENTRIES, 3);
    }
    if mode == "audit-separation-player-column" {
        return run_responsive_player_column_mode(&mut args, true, TWO_STATE_AUDIT_ENTRIES, 1);
    }
    if mode == "diagnose-left-direction" {
        return run_left_direction_diagnosis_mode(&mut args);
    }
    if mode == "derive-ladder" {
        return run_derive_ladder_mode(&mut args);
    }
    if mode == "diagnose-span" {
        return run_span_diagnosis_mode(&mut args);
    }
    if mode == "diagnose-refused-grid" {
        return run_refused_grid_mode(&mut args);
    }
    if mode == "diagnose-down-census" {
        return run_down_census_mode(&mut args);
    }
    if mode == "diagnose-x-transit" {
        return run_x_transit_mode(&mut args);
    }
    if mode == "measure-viable-progress" {
        return run_viable_progress_mode(&mut args);
    }
    if mode == "gate-live-control" {
        return run_live_control_gate_mode(&mut args);
    }
    if mode == "readmit-archive" {
        return run_readmission_mode(&mut args);
    }
    if mode == "diagnose-film-measurements" {
        return run_film_measurement_mode(&mut args, false);
    }
    if mode == "diagnose-derived-measurements" {
        return run_film_measurement_mode(&mut args, true);
    }
    if mode == "diagnose-film-columns" {
        return run_film_column_mode(&mut args);
    }
    if mode == "diagnose-player-column" {
        return run_player_column_diagnosis_mode(&mut args);
    }
    if mode == "archive-resume" {
        return run_archive_resume_mode(
            &mut args,
            SmbArchiveDurationPolicy::Legacy,
            SmbArchiveSuffixPolicy::OneOrTwo,
            ResumeSelection::Champion,
            SmbArchiveRetentionPolicy::Frozen,
            SmbArchiveKeyPolicy::Frozen,
            SmbArchiveLadderPolicy::Frozen,
            SmbArchiveSelectorPolicy::ConcentratedRecency,
        );
    }
    if mode == "archive-resume-temporal" {
        return run_archive_resume_mode(
            &mut args,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::OneOrTwo,
            ResumeSelection::Champion,
            SmbArchiveRetentionPolicy::Frozen,
            SmbArchiveKeyPolicy::Frozen,
            SmbArchiveLadderPolicy::Frozen,
            SmbArchiveSelectorPolicy::ConcentratedRecency,
        );
    }
    if mode == "archive-resume-frontier" {
        return run_archive_resume_mode(
            &mut args,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::OneOrTwo,
            ResumeSelection::Frontier,
            SmbArchiveRetentionPolicy::Frozen,
            SmbArchiveKeyPolicy::Frozen,
            SmbArchiveLadderPolicy::Frozen,
            SmbArchiveSelectorPolicy::ConcentratedRecency,
        );
    }
    if mode == "archive-resume-frontier-viable-ladder" {
        return run_archive_resume_mode(
            &mut args,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::OneOrTwo,
            ResumeSelection::Frontier,
            SmbArchiveRetentionPolicy::ProbeAtAdmission,
            SmbArchiveKeyPolicy::Frozen,
            SmbArchiveLadderPolicy::Extended,
            SmbArchiveSelectorPolicy::ConcentratedRecency,
        );
    }
    if mode == "archive-resume-frontier-viable" {
        return run_archive_resume_mode(
            &mut args,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::OneOrTwo,
            ResumeSelection::Frontier,
            SmbArchiveRetentionPolicy::ProbeAtAdmission,
            SmbArchiveKeyPolicy::Frozen,
            SmbArchiveLadderPolicy::Frozen,
            SmbArchiveSelectorPolicy::ConcentratedRecency,
        );
    }
    if mode == "archive-resume-frontier-viable-page" {
        return run_archive_resume_mode(
            &mut args,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::OneOrTwo,
            ResumeSelection::Frontier,
            SmbArchiveRetentionPolicy::ProbeAtAdmission,
            SmbArchiveKeyPolicy::VerticalPage,
            SmbArchiveLadderPolicy::Frozen,
            SmbArchiveSelectorPolicy::ConcentratedRecency,
        );
    }
    if mode == "archive-resume-frontier-burst" {
        return run_archive_resume_mode(
            &mut args,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::BurstUpToFour,
            ResumeSelection::Frontier,
            SmbArchiveRetentionPolicy::Frozen,
            SmbArchiveKeyPolicy::Frozen,
            SmbArchiveLadderPolicy::Frozen,
            SmbArchiveSelectorPolicy::ConcentratedRecency,
        );
    }
    if mode == "archive-resume-frontier-cell-set" {
        return run_archive_resume_mode(
            &mut args,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::OneOrTwo,
            ResumeSelection::FrontierCellSet,
            SmbArchiveRetentionPolicy::Frozen,
            SmbArchiveKeyPolicy::Frozen,
            SmbArchiveLadderPolicy::Frozen,
            SmbArchiveSelectorPolicy::ConcentratedRecency,
        );
    }
    if mode == "archive-resume-frontier-set" {
        return run_archive_resume_mode(
            &mut args,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::OneOrTwo,
            ResumeSelection::FrontierSet,
            SmbArchiveRetentionPolicy::Frozen,
            SmbArchiveKeyPolicy::Frozen,
            SmbArchiveLadderPolicy::Frozen,
            SmbArchiveSelectorPolicy::ConcentratedRecency,
        );
    }
    if mode == "archive-resume-burst" {
        return run_archive_resume_mode(
            &mut args,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::BurstUpToFour,
            ResumeSelection::FrontierSet,
            SmbArchiveRetentionPolicy::Frozen,
            SmbArchiveKeyPolicy::Frozen,
            SmbArchiveLadderPolicy::Frozen,
            SmbArchiveSelectorPolicy::ConcentratedRecency,
        );
    }
    if mode == "archive-resume-play-viable-ladder" {
        return run_archive_resume_mode(
            &mut args,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::OneOrTwo,
            ResumeSelection::PlayBucket,
            SmbArchiveRetentionPolicy::ProbeAtAdmission,
            SmbArchiveKeyPolicy::Frozen,
            SmbArchiveLadderPolicy::Extended,
            SmbArchiveSelectorPolicy::ConcentratedRecency,
        );
    }
    if mode == "archive-resume-band-burst" {
        return run_archive_resume_mode(
            &mut args,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::BurstUpToFour,
            ResumeSelection::FrontierBandSet,
            SmbArchiveRetentionPolicy::Frozen,
            SmbArchiveKeyPolicy::Frozen,
            SmbArchiveLadderPolicy::Frozen,
            SmbArchiveSelectorPolicy::ConcentratedRecency,
        );
    }
    if mode != "reproduce-baseline" {
        return Err("unknown smb-completion mode".into());
    }
    let source_path = PathBuf::from(args.next().ok_or("missing M5 report")?);
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    fs::create_dir_all(&output)?;

    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    let rom = fs::read(rom_path)?;
    let source: M5Report = serde_json::from_slice(&fs::read(source_path)?)?;
    let source_run = source
        .ratchet
        .iter()
        .max_by_key(|run| milestone_key(run.milestones))
        .ok_or("M5 report contains no ratchet runs")?;
    let initial_corpus = source_run
        .corpus
        .iter()
        .cloned()
        .map(|input| SmbLabeledCorpusEntry {
            input,
            labels: neutral_labels(),
        })
        .collect::<Vec<_>>();
    fs::write(
        output.join("initial-corpus.json"),
        serde_json::to_vec_pretty(&initial_corpus)?,
    )?;

    let first = run_control(&rom, &initial_corpus)?;
    let replay = run_control(&rom, &initial_corpus)?;
    let no_model_campaign_replay_verified = first == replay;
    fs::write(
        output.join("pilot-live.json"),
        serde_json::to_vec_pretty(&first)?,
    )?;
    fs::write(
        output.join("pilot-replay.json"),
        serde_json::to_vec_pretty(&replay)?,
    )?;

    let (champion, champion_milestones) = first
        .campaign
        .corpus
        .iter()
        .map(|input| {
            let milestones = input_milestones(&rom, input)?;
            Ok::<_, Box<dyn Error>>((input, milestones))
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max_by_key(|(_, milestones)| milestone_key(*milestones))
        .ok_or("M12 pilot retained no inputs")?;
    let observations = observe_smb_input(&rom, champion)?;
    let replayed_observations = observe_smb_input(&rom, champion)?;
    let champion_observation_replay_verified = observations == replayed_observations;
    let input_bytes = serde_json::to_vec(champion)?;
    let observation_bytes = serde_json::to_vec(&observations)?;
    let champion_input_sha256 = format!("{:x}", Sha256::digest(&input_bytes));
    let champion_observations_sha256 = format!("{:x}", Sha256::digest(&observation_bytes));
    fs::write(
        output.join("starting-champion-input.json"),
        serde_json::to_vec_pretty(champion)?,
    )?;
    fs::write(
        output.join("starting-champion-observations.json"),
        serde_json::to_vec_pretty(&observations)?,
    )?;

    let report = BaselineReproduction {
        base_commit: "8f2b522c26c6f192f2db45a430bec03ed447cad7",
        rom_sha256: format!("{:x}", Sha256::digest(&rom)),
        source_seed: source_run.seed,
        source_executions: source_run.executions,
        source_max_x: source_run.milestones.max_1_1_scroll_bucket,
        source_corpus_count: source_run.corpus.len(),
        pilot_seed: M12_PILOT_SEED,
        pilot_executions: M12_PILOT_EXECUTIONS,
        pilot_max_x: first.campaign.milestones.max_1_1_scroll_bucket,
        pilot_corpus_count: first.campaign.corpus.len(),
        no_model_campaign_replay_verified,
        champion_milestones,
        champion_input_sha256,
        champion_observations_sha256,
        champion_observation_replay_verified,
    };
    fs::write(
        output.join("baseline-reproduction.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.no_model_campaign_replay_verified || !report.champion_observation_replay_verified {
        return Err("frozen baseline replay diverged".into());
    }
    Ok(())
}

fn run_frontier_viability_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    let replay_requested = parse_replay_flag(args, "audit-frontier-viability")?;
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let report = audit_smb_frontier_viability(&rom, &source)?;
    fs::write(
        output.join("viability-live.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    let replay_verified = if replay_requested {
        let replay = audit_smb_frontier_viability(&rom, &source)?;
        fs::write(
            output.join("viability-replay.json"),
            serde_json::to_vec_pretty(&replay)?,
        )?;
        Some(replay == report)
    } else {
        None
    };
    let summary = serde_json::json!({
        "continuation_frames": report.continuation_frames,
        "continuation_masks": report.continuation_masks,
        "frontier": report.frontier,
        "approach_band": report.approach_band,
        "replay_verified": replay_verified,
    });
    fs::write(
        output.join("viability-summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn run_player_column_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
    selection: SmbPlayerColumnSelection,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    let replay_requested = parse_replay_flag(args, "audit-player-column")?;
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let (report, frames) = audit_smb_player_column_with_selection(&rom, &source, selection)?;
    fs::write(
        output.join("player-column-live.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    let frame_directory = output.join("frames");
    fs::create_dir_all(&frame_directory)?;
    for frame in &frames {
        fs::write(
            frame_directory.join(&frame.name),
            encode_smb_frame_png(&frame.rgba)?,
        )?;
    }
    let replay_verified = if replay_requested {
        let (replay, replay_frames) =
            audit_smb_player_column_with_selection(&rom, &source, selection)?;
        fs::write(
            output.join("player-column-replay.json"),
            serde_json::to_vec_pretty(&replay)?,
        )?;
        Some(replay == report && replay_frames == frames)
    } else {
        None
    };
    let summary = serde_json::json!({
        "continuation_frames": report.continuation_frames,
        "continuation_masks": report.continuation_masks,
        "audited_entries": report.audited.len(),
        "scanned_per_slice": report.scanned_per_slice,
        "steerable_per_slice": report.steerable_per_slice,
        "distinct_value_survivors": report.distinct_value_survivors,
        "smooth_survivors": report.smooth_survivors,
        "left_direction_survivors": report.left_direction_survivors,
        "right_direction_survivors": report.right_direction_survivors,
        "qualifying_right_continuations": report.qualifying_right_continuations,
        "camera_relative_survivors": report.camera_relative_survivors,
        "film_survivors": report.film_survivors,
        "stride_rejected": report.stride_rejected,
        "selected": report.selected,
        "rendered_frames": frames.len(),
        "replay_verified": replay_verified,
    });
    fs::write(
        output.join("player-column-summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    if replay_verified == Some(false) {
        return Err("player-column audit replay diverged".into());
    }
    Ok(())
}

fn run_control_census_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    let replay_requested = parse_replay_flag(args, "census-control-authority")?;
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let report = census_smb_control_authority(&rom, &source)?;
    fs::write(
        output.join("census-live.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    let replay_verified = if replay_requested {
        let replay = census_smb_control_authority(&rom, &source)?;
        fs::write(
            output.join("census-replay.json"),
            serde_json::to_vec_pretty(&replay)?,
        )?;
        Some(replay == report)
    } else {
        None
    };
    let summary = serde_json::json!({
        "continuation_frames": report.continuation_frames,
        "camera_advance": report.camera_advance,
        "active": report.active,
        "admitted": report.admitted,
        "buckets": report.buckets,
        "replay_verified": replay_verified,
    });
    fs::write(
        output.join("census-summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    if replay_verified == Some(false) {
        return Err("control-authority census replay diverged".into());
    }
    Ok(())
}

fn run_census_player_column_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
    spread: bool,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let census_path = PathBuf::from(args.next().ok_or("missing census report")?);
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    let replay_requested = parse_replay_flag(args, "audit-census-player-column")?;
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let census: SmbControlCensusReport = serde_json::from_slice(&fs::read(census_path)?)?;
    let ids = if spread {
        select_smb_spread_audit_ids(&source, &census.admitted_ids, CENSUS_AUDIT_ENTRIES)?
    } else {
        census
            .admitted_ids
            .iter()
            .copied()
            .take(CENSUS_AUDIT_ENTRIES)
            .collect::<Vec<_>>()
    };
    if ids.len() < CENSUS_AUDIT_ENTRIES {
        let summary = serde_json::json!({
            "audited_entries": ids.len(),
            "admitted": census.admitted,
            "selected": serde_json::Value::Null,
            "inconclusive": "fewer than eight admitted entries",
        });
        fs::write(
            output.join("player-column-summary.json"),
            serde_json::to_vec_pretty(&summary)?,
        )?;
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }
    let audit = if spread {
        audit_smb_player_column_spread
    } else {
        audit_smb_player_column_from_ids
    };
    let (report, frames) = audit(&rom, &source, &ids)?;
    fs::write(
        output.join("player-column-live.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    let frame_directory = output.join("frames");
    fs::create_dir_all(&frame_directory)?;
    for frame in &frames {
        fs::write(
            frame_directory.join(&frame.name),
            encode_smb_frame_png(&frame.rgba)?,
        )?;
    }
    let replay_verified = if replay_requested {
        let (replay, replay_frames) = audit(&rom, &source, &ids)?;
        fs::write(
            output.join("player-column-replay.json"),
            serde_json::to_vec_pretty(&replay)?,
        )?;
        Some(replay == report && replay_frames == frames)
    } else {
        None
    };
    let summary = serde_json::json!({
        "audited_ids": ids,
        "audited_entries": report.audited.len(),
        "distinct_value_survivors": report.distinct_value_survivors,
        "smooth_survivors": report.smooth_survivors,
        "left_direction_survivors": report.left_direction_survivors,
        "right_direction_survivors": report.right_direction_survivors,
        "qualifying_right_continuations": report.qualifying_right_continuations,
        "camera_relative_survivors": report.camera_relative_survivors,
        "film_survivors": report.film_survivors,
        "stride_rejected": report.stride_rejected,
        "selected": report.selected,
        "rendered_frames": frames.len(),
        "replay_verified": replay_verified,
    });
    fs::write(
        output.join("player-column-summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    if replay_verified == Some(false) {
        return Err("census player-column audit replay diverged".into());
    }
    Ok(())
}

fn run_terminal_death_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    let replay_requested = parse_replay_flag(args, "audit-terminal-death")?;
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let report = audit_smb_terminal_death(&rom, &source)?;
    fs::write(
        output.join("terminal-death-live.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    let replay_verified = if replay_requested {
        let replay = audit_smb_terminal_death(&rom, &source)?;
        fs::write(
            output.join("terminal-death-replay.json"),
            serde_json::to_vec_pretty(&replay)?,
        )?;
        Some(replay == report)
    } else {
        None
    };
    let summary = serde_json::json!({
        "control_actions": report.control_actions,
        "control_frames": report.control_frames,
        "continuation_frames": report.continuation_frames,
        "scanned": report.scanned,
        "uncontrolled_ids": report.uncontrolled_ids,
        "already_below_genesis": report
            .uncontrolled_traces
            .iter()
            .filter(|trace| trace.life_counter_below_genesis_at_endpoint)
            .count(),
        "candidates": report
            .candidates
            .iter()
            .map(|candidate| {
                serde_json::json!({
                    "name": candidate.name,
                    "control_true_frames": candidate.control_true_frames,
                    "trip_frames": candidate.trip_frames,
                    "without_trip": candidate.without_trip.len(),
                    "median_trip_frame": candidate.median_trip_frame,
                    "max_trip_frame": candidate.max_trip_frame,
                    "passes": candidate.passes,
                })
            })
            .collect::<Vec<_>>(),
        "adoption_rule_selects": report.adoption_rule_selects,
        "replay_verified": replay_verified,
    });
    fs::write(
        output.join("terminal-death-summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    if replay_verified == Some(false) {
        return Err("terminal-death audit replay diverged".into());
    }
    Ok(())
}

fn run_steered_player_column_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    let replay_requested = parse_replay_flag(args, "audit-steered-player-column")?;
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let scan = select_smb_steered_audit_ids(&rom, &source, CENSUS_AUDIT_ENTRIES)?;
    fs::write(
        output.join("steer-scan.json"),
        serde_json::to_vec_pretty(&scan)?,
    )?;
    if scan.steered_ids.len() < CENSUS_AUDIT_ENTRIES {
        let summary = serde_json::json!({
            "scanned": scan.scanned,
            "camera_advancing": scan.camera_advancing,
            "answering": scan.answering,
            "steered": scan.steered,
            "audited_entries": scan.steered_ids.len(),
            "selected": serde_json::Value::Null,
            "inconclusive": "fewer than eight steered entries",
        });
        fs::write(
            output.join("player-column-summary.json"),
            serde_json::to_vec_pretty(&summary)?,
        )?;
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }
    let (report, frames) = audit_smb_player_column_spread(&rom, &source, &scan.steered_ids)?;
    fs::write(
        output.join("player-column-live.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    let frame_directory = output.join("frames");
    fs::create_dir_all(&frame_directory)?;
    for frame in &frames {
        fs::write(
            frame_directory.join(&frame.name),
            encode_smb_frame_png(&frame.rgba)?,
        )?;
    }
    let replay_verified = if replay_requested {
        let replay_scan = select_smb_steered_audit_ids(&rom, &source, CENSUS_AUDIT_ENTRIES)?;
        let (replay, replay_frames) =
            audit_smb_player_column_spread(&rom, &source, &replay_scan.steered_ids)?;
        fs::write(
            output.join("player-column-replay.json"),
            serde_json::to_vec_pretty(&replay)?,
        )?;
        Some(replay_scan == scan && replay == report && replay_frames == frames)
    } else {
        None
    };
    let summary = serde_json::json!({
        "scanned": scan.scanned,
        "camera_advancing": scan.camera_advancing,
        "answering": scan.answering,
        "steered": scan.steered,
        "audited_ids": scan.steered_ids,
        "audited_entries": report.audited.len(),
        "distinct_value_survivors": report.distinct_value_survivors,
        "smooth_survivors": report.smooth_survivors,
        "left_direction_survivors": report.left_direction_survivors,
        "film_survivors": report.film_survivors,
        "stride_rejected": report.stride_rejected,
        "selected": report.selected,
        "rendered_frames": frames.len(),
        "replay_verified": replay_verified,
    });
    fs::write(
        output.join("player-column-summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    if replay_verified == Some(false) {
        return Err("steered player-column audit replay diverged".into());
    }
    Ok(())
}

fn run_responsive_player_column_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
    by_span: bool,
    wanted: usize,
    direction_rule: u8,
) -> Result<(), Box<dyn Error>> {
    let audit = match direction_rule {
        3 => audit_smb_player_column_derived,
        2 => audit_smb_player_column_verified,
        1 => audit_smb_player_column_separation,
        _ => audit_smb_player_column_contrast,
    };
    let select = if by_span {
        select_smb_span_audit_ids
    } else {
        select_smb_responsive_audit_ids
    };
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    let replay_requested = parse_replay_flag(args, "audit-responsive-player-column")?;
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let scan = select(&rom, &source, wanted)?;
    fs::write(
        output.join("responsive-scan.json"),
        serde_json::to_vec_pretty(&scan)?,
    )?;
    if scan.steered_ids.len() < wanted {
        let summary = serde_json::json!({
            "scanned": scan.scanned,
            "responsive": scan.responsive,
            "audited_entries": scan.steered_ids.len(),
            "selected": serde_json::Value::Null,
            "inconclusive": "fewer than eight responsive entries",
        });
        fs::write(
            output.join("player-column-summary.json"),
            serde_json::to_vec_pretty(&summary)?,
        )?;
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }
    let (report, frames) = audit(&rom, &source, &scan.steered_ids)?;
    fs::write(
        output.join("player-column-live.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    let frame_directory = output.join("frames");
    fs::create_dir_all(&frame_directory)?;
    for frame in &frames {
        fs::write(
            frame_directory.join(&frame.name),
            encode_smb_frame_png(&frame.rgba)?,
        )?;
    }
    let replay_verified = if replay_requested {
        let replay_scan = select(&rom, &source, wanted)?;
        let (replay, replay_frames) = audit(&rom, &source, &replay_scan.steered_ids)?;
        fs::write(
            output.join("player-column-replay.json"),
            serde_json::to_vec_pretty(&replay)?,
        )?;
        Some(replay_scan == scan && replay == report && replay_frames == frames)
    } else {
        None
    };
    let summary = serde_json::json!({
        "scanned": scan.scanned,
        "responsive": scan.responsive,
        "audited_ids": scan.steered_ids,
        "audited_entries": report.audited.len(),
        "distinct_value_survivors": report.distinct_value_survivors,
        "smooth_survivors": report.smooth_survivors,
        "left_direction_survivors": report.left_direction_survivors,
        "camera_relative_survivors": report.camera_relative_survivors,
        "film_survivors": report.film_survivors,
        "stride_rejected": report.stride_rejected,
        "selected": report.selected,
        "rendered_frames": frames.len(),
        "replay_verified": replay_verified,
    });
    fs::write(
        output.join("player-column-summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    if replay_verified == Some(false) {
        return Err("responsive player-column audit replay diverged".into());
    }
    Ok(())
}

fn run_left_direction_diagnosis_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let scan_path = PathBuf::from(args.next().ok_or("missing steer scan report")?);
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let scan: SmbSteerScanReport = serde_json::from_slice(&fs::read(scan_path)?)?;
    let (entries, candidates) = diagnose_smb_left_direction(&rom, &source, &scan.steered_ids)?;
    fs::write(
        output.join("left-direction-entries.json"),
        serde_json::to_vec_pretty(&entries)?,
    )?;
    fs::write(
        output.join("left-direction-candidates.json"),
        serde_json::to_vec(&candidates)?,
    )?;
    println!("entries {} candidates {}", entries.len(), candidates.len());
    Ok(())
}

fn run_derive_ladder_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let output = PathBuf::from(args.next().ok_or("missing output file")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let ladder = derive_smb_ladder(&source);
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, serde_json::to_vec_pretty(&ladder)?)?;
    println!("{}", serde_json::to_string_pretty(&ladder)?);
    Ok(())
}

fn run_x_transit_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let stream_path = PathBuf::from(args.next().ok_or("missing stream path")?);
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let parent_low = u16::try_from(parse_u64(
        &args.next().ok_or("missing parent low")?.to_string_lossy(),
    )?)?;
    let parent_high = u16::try_from(parse_u64(
        &args.next().ok_or("missing parent high")?.to_string_lossy(),
    )?)?;
    let sample_cap = usize::try_from(parse_u64(
        &args.next().ok_or("missing sample cap")?.to_string_lossy(),
    )?)?;
    let output = PathBuf::from(args.next().ok_or("missing output file")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let rom = read_rom()?;
    let stream_text = fs::read_to_string(&stream_path)?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let report = fuzzer::campaign::diagnose_x_transit(
        &rom,
        &stream_text,
        &source,
        (parent_low, parent_high),
        sample_cap,
    )?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "jobs {} candidates {}",
        report.jobs_sampled, report.candidates
    );
    for (band, retained, rejected, refused, duplicate) in &report.bands {
        println!(
            "x {}-{}: retained {} rejected {} refused {} duplicate {}",
            band,
            band + 15,
            retained,
            rejected,
            refused,
            duplicate
        );
    }
    Ok(())
}

fn run_down_census_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let stream_path = PathBuf::from(args.next().ok_or("missing stream path")?);
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let parent_low = u16::try_from(parse_u64(
        &args.next().ok_or("missing parent low")?.to_string_lossy(),
    )?)?;
    let parent_high = u16::try_from(parse_u64(
        &args.next().ok_or("missing parent high")?.to_string_lossy(),
    )?)?;
    let sample_cap = usize::try_from(parse_u64(
        &args.next().ok_or("missing sample cap")?.to_string_lossy(),
    )?)?;
    let output = PathBuf::from(args.next().ok_or("missing output file")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let rom = read_rom()?;
    let stream_text = fs::read_to_string(&stream_path)?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let report = fuzzer::campaign::diagnose_down_census(
        &rom,
        &stream_text,
        &source,
        (parent_low, parent_high),
        sample_cap,
    )?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "jobs {} presses {} engine_changes {} world_changes {}",
        report.jobs_sampled, report.down_presses, report.engine_state_changes, report.world_changes
    );
    Ok(())
}

fn run_refused_grid_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let stream_path = PathBuf::from(args.next().ok_or("missing stream path")?);
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let parent_low = u16::try_from(parse_u64(
        &args.next().ok_or("missing parent low")?.to_string_lossy(),
    )?)?;
    let parent_high = u16::try_from(parse_u64(
        &args.next().ok_or("missing parent high")?.to_string_lossy(),
    )?)?;
    let candidate_low = u16::try_from(parse_u64(
        &args
            .next()
            .ok_or("missing candidate low")?
            .to_string_lossy(),
    )?)?;
    let candidate_high = u16::try_from(parse_u64(
        &args
            .next()
            .ok_or("missing candidate high")?
            .to_string_lossy(),
    )?)?;
    let sample_cap = usize::try_from(parse_u64(
        &args.next().ok_or("missing sample cap")?.to_string_lossy(),
    )?)?;
    let output = PathBuf::from(args.next().ok_or("missing output file")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let rom = read_rom()?;
    let stream_text = fs::read_to_string(&stream_path)?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let report = fuzzer::campaign::diagnose_refused_grid(
        &rom,
        &stream_text,
        &source,
        (parent_low, parent_high),
        (candidate_low, candidate_high),
        sample_cap,
    )?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "jobs {} refused {} probed {} mismatches {}",
        report.jobs_sampled,
        report.refused_candidates,
        report.probed_candidates,
        report.derivation_mismatches
    );
    for row in &report.aggregate {
        println!(
            "{}: 45={} 60={} 90={} 120={} of {}",
            row.mask,
            row.survived_at_45,
            row.survived_at_60,
            row.survived_at_90,
            row.survived_at_120,
            report.probed_candidates
        );
    }
    Ok(())
}

fn run_span_diagnosis_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let endpoint = parse_u64(
        &args
            .next()
            .ok_or("missing endpoint bucket")?
            .to_string_lossy(),
    )?;
    let low = parse_u64(&args.next().ok_or("missing span low")?.to_string_lossy())?;
    let high = parse_u64(&args.next().ok_or("missing span high")?.to_string_lossy())?;
    let output = PathBuf::from(args.next().ok_or("missing output file")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let boundaries = diagnose_smb_span(
        &rom,
        &source,
        u16::try_from(endpoint)?,
        u16::try_from(low)?,
        u16::try_from(high)?,
    )?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, serde_json::to_vec_pretty(&boundaries)?)?;
    println!("boundaries {}", boundaries.len());
    Ok(())
}

fn run_viable_progress_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let output = PathBuf::from(args.next().ok_or("missing output file")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let report = measure_smb_viable_progress(&rom, &source)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn run_live_control_gate_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let report = gate_smb_live_control(&rom, &source)?;
    fs::write(
        output.join("live-control-gate.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn run_readmission_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    let replay_requested = parse_replay_flag(args, "readmit-archive")?;
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let (report, rebuilt) = readmit_smb_archive(&rom, &source)?;
    fs::write(
        output.join("readmission-live.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    fs::write(
        output.join("archive-live.json"),
        serde_json::to_vec_pretty(&rebuilt)?,
    )?;
    let replay_verified = if replay_requested {
        let (replay, replay_rebuilt) = readmit_smb_archive(&rom, &source)?;
        Some(replay == report && replay_rebuilt == rebuilt)
    } else {
        None
    };
    let summary = serde_json::json!({
        "recorded": report.recorded,
        "surviving": report.surviving,
        "below_play_area_at_endpoint": report.below_play_area_at_endpoint,
        "max_surviving": report.max_surviving,
        "occupied_buckets": report.buckets.len(),
        "surviving_buckets": report
            .buckets
            .iter()
            .filter(|bucket| bucket.surviving > 0)
            .count(),
        "replay_verified": replay_verified,
    });
    fs::write(
        output.join("readmission-summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    if replay_verified == Some(false) {
        return Err("archive re-admission replay diverged".into());
    }
    Ok(())
}

fn run_film_measurement_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
    derived: bool,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let census_path = PathBuf::from(args.next().ok_or("missing census report")?);
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let census: SmbControlCensusReport = serde_json::from_slice(&fs::read(census_path)?)?;
    let ids = if census.admitted_ids.len() == CENSUS_AUDIT_ENTRIES {
        census.admitted_ids.clone()
    } else {
        select_smb_spread_audit_ids(&source, &census.admitted_ids, CENSUS_AUDIT_ENTRIES)?
    };
    let measurements = if derived {
        diagnose_smb_film_measurements_derived(&rom, &source, &ids)?
    } else {
        diagnose_smb_film_measurements(&rom, &source, &ids)?
    };
    fs::write(
        output.join("film-measurements.json"),
        serde_json::to_vec(&measurements)?,
    )?;
    println!("measurements {}", measurements.len());
    Ok(())
}

fn run_film_column_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let census_path = PathBuf::from(args.next().ok_or("missing census report")?);
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let census: SmbControlCensusReport = serde_json::from_slice(&fs::read(census_path)?)?;
    let ids = select_smb_spread_audit_ids(&source, &census.admitted_ids, CENSUS_AUDIT_ENTRIES)?;
    let traces = diagnose_smb_film_columns(&rom, &source, &ids)?;
    fs::write(
        output.join("film-columns.json"),
        serde_json::to_vec(&traces)?,
    )?;
    println!("traces {}", traces.len());
    Ok(())
}

fn run_player_column_diagnosis_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let (traces, frames) = diagnose_smb_player_column(&rom, &source)?;
    fs::write(
        output.join("player-column-traces.json"),
        serde_json::to_vec(&traces)?,
    )?;
    let frame_directory = output.join("frames");
    fs::create_dir_all(&frame_directory)?;
    for frame in &frames {
        fs::write(
            frame_directory.join(&frame.name),
            encode_smb_frame_png(&frame.rgba)?,
        )?;
    }
    println!("traces {} frames {}", traces.len(), frames.len());
    Ok(())
}

fn run_control_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source = PathBuf::from(args.next().ok_or("missing baseline pilot report")?);
    let seed = parse_u64(&args.next().ok_or("missing seed")?.to_string_lossy())?;
    let budget = parse_u64(
        &args
            .next()
            .ok_or("missing execution budget")?
            .to_string_lossy(),
    )?;
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    let replay_requested = parse_replay_flag(args, "control")?;
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let source: SmbConfiguredReport = serde_json::from_slice(&fs::read(source)?)?;
    let corpus = neutral_corpus(&source.campaign.corpus);
    let report = run_smb_restart_configured(
        &rom,
        &corpus,
        seed,
        budget,
        NullSmbDetector,
        NullSmbMacro,
        no_artifacts(),
    )?;
    fs::write(
        output.join("control-live.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    let replay_verified = if replay_requested {
        let replay = run_smb_restart_configured(
            &rom,
            &corpus,
            seed,
            budget,
            NullSmbDetector,
            NullSmbMacro,
            no_artifacts(),
        )?;
        fs::write(
            output.join("control-replay.json"),
            serde_json::to_vec_pretty(&replay)?,
        )?;
        Some(replay == report)
    } else {
        None
    };
    let summary = serde_json::json!({
        "seed": seed,
        "executions": budget,
        "milestones": report.campaign.milestones,
        "corpus_count": report.campaign.corpus.len(),
        "replay_verified": replay_verified,
    });
    fs::write(
        output.join("control-summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn run_archive_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source = PathBuf::from(args.next().ok_or("missing baseline pilot report")?);
    let seed = parse_u64(&args.next().ok_or("missing seed")?.to_string_lossy())?;
    let budget = parse_u64(
        &args
            .next()
            .ok_or("missing execution budget")?
            .to_string_lossy(),
    )?;
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    let replay_requested = parse_replay_flag(args, "archive")?;
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let source: SmbConfiguredReport = serde_json::from_slice(&fs::read(source)?)?;
    let report = run_smb_archive_search(&rom, &source.campaign.corpus, seed, budget)?;
    fs::write(
        output.join("archive-live.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    let replay_verified = if replay_requested {
        let replay = run_smb_archive_search(&rom, &source.campaign.corpus, seed, budget)?;
        fs::write(
            output.join("archive-replay.json"),
            serde_json::to_vec_pretty(&replay)?,
        )?;
        Some(replay == report)
    } else {
        None
    };
    let observations = observe_smb_input(&rom, &report.champion_input)?;
    let champion_input_sha256 = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&report.champion_input)?)
    );
    let champion_observations_sha256 =
        format!("{:x}", Sha256::digest(serde_json::to_vec(&observations)?));
    let summary = archive_summary(
        &report,
        replay_verified,
        champion_input_sha256,
        champion_observations_sha256,
    );
    fs::write(
        output.join("archive-summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_archive_resume_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
    duration_policy: SmbArchiveDurationPolicy,
    suffix_policy: SmbArchiveSuffixPolicy,
    selection: ResumeSelection,
    retention_policy: SmbArchiveRetentionPolicy,
    key_policy: SmbArchiveKeyPolicy,
    ladder_policy: SmbArchiveLadderPolicy,
    selector_policy: SmbArchiveSelectorPolicy,
) -> Result<(), Box<dyn Error>> {
    let source = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let play_bucket = if matches!(selection, ResumeSelection::PlayBucket) {
        Some(u16::try_from(parse_u64(
            &args
                .next()
                .ok_or("missing deepest play bucket")?
                .to_string_lossy(),
        )?)?)
    } else {
        None
    };
    let seed = parse_u64(&args.next().ok_or("missing seed")?.to_string_lossy())?;
    let budget = parse_u64(
        &args
            .next()
            .ok_or("missing execution budget")?
            .to_string_lossy(),
    )?;
    let action_limit_u64 = parse_u64(
        &args
            .next()
            .ok_or("missing completion action limit")?
            .to_string_lossy(),
    )?;
    let action_limit = usize::try_from(action_limit_u64)?;
    if action_limit > MAX_SMB_COMPLETION_ACTIONS {
        return Err("completion action limit exceeds the compiled bound".into());
    }
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    let replay_requested = parse_replay_flag(args, "archive-resume")?;
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source)?)?;
    let initial = match selection {
        ResumeSelection::Champion => vec![source.champion_input],
        ResumeSelection::Frontier => vec![
            frontier_entries(&source)?
                .first()
                .ok_or("source archive contains no frontier entries")?
                .input
                .clone(),
        ],
        ResumeSelection::FrontierCellSet | ResumeSelection::FrontierSet => {
            let mut distinct = BTreeMap::new();
            for entry in frontier_entries(&source)? {
                distinct
                    .entry((entry.key.player_y_bucket, entry.key.player_engine_state))
                    .or_insert_with(|| entry.input.clone());
            }
            distinct
                .into_values()
                .take(FRONTIER_RESUME_INPUTS)
                .collect()
        }
        ResumeSelection::FrontierBandSet => {
            let mut distinct = BTreeSet::new();
            let mut by_progress = BTreeMap::<u16, Vec<_>>::new();
            for entry in frontier_band_entries(&source)? {
                by_progress
                    .entry(entry.key.progress)
                    .or_default()
                    .push(entry);
            }
            by_progress
                .into_values()
                .flat_map(|entries| entries.into_iter().take(8))
                .filter_map(|entry| {
                    distinct
                        .insert(entry.input.clone())
                        .then_some(entry.input.clone())
                })
                .take(FRONTIER_RESUME_INPUTS)
                .collect()
        }
        ResumeSelection::PlayBucket => {
            let bucket = play_bucket.ok_or("play-bucket resume is missing its bucket")?;
            vec![play_bucket_resume_input(&source, bucket)?]
        }
    };
    let frozen_search = matches!(
        selection,
        ResumeSelection::Champion | ResumeSelection::Frontier | ResumeSelection::FrontierCellSet
    );
    let run = |seed| {
        if matches!(selection, ResumeSelection::PlayBucket) {
            run_smb_archive_search_with_selector(
                &rom,
                &initial,
                seed,
                budget,
                action_limit,
                duration_policy,
                suffix_policy,
                retention_policy,
                key_policy,
                ladder_policy,
                selector_policy,
            )
        } else if frozen_search {
            run_smb_archive_search_with_retention(
                &rom,
                &initial,
                seed,
                budget,
                action_limit,
                duration_policy,
                suffix_policy,
                retention_policy,
                key_policy,
                ladder_policy,
            )
        } else {
            run_smb_archive_search_with_policies(
                &rom,
                &initial,
                seed,
                budget,
                action_limit,
                duration_policy,
                suffix_policy,
            )
        }
    };
    let report = run(seed)?;
    fs::write(
        output.join("archive-live.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    let replay_verified = if replay_requested {
        let replay = run(seed)?;
        fs::write(
            output.join("archive-replay.json"),
            serde_json::to_vec_pretty(&replay)?,
        )?;
        Some(replay == report)
    } else {
        None
    };
    let observations = observe_smb_input(&rom, &report.champion_input)?;
    let summary = archive_summary(
        &report,
        replay_verified,
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&report.champion_input)?)
        ),
        format!("{:x}", Sha256::digest(serde_json::to_vec(&observations)?)),
    );
    let summary = serde_json::json!({
        "action_limit": action_limit,
        "source_selection": match selection {
            ResumeSelection::Champion => "champion",
            ResumeSelection::Frontier => "mechanical_frontier",
            ResumeSelection::FrontierCellSet => "mechanical_frontier_cell_set",
            ResumeSelection::FrontierSet => "mechanical_frontier_set",
            ResumeSelection::FrontierBandSet => "mechanical_frontier_band_set",
            ResumeSelection::PlayBucket => "deepest_play_bucket",
        },
        "source_inputs": initial.len(),
        "ladder_policy": match ladder_policy {
            SmbArchiveLadderPolicy::Frozen => "frozen",
            SmbArchiveLadderPolicy::Extended => "extended",
        },
        "key_policy": match key_policy {
            SmbArchiveKeyPolicy::Frozen => "frozen",
            SmbArchiveKeyPolicy::VerticalPage => "vertical_page",
        },
        "retention_policy": match retention_policy {
            SmbArchiveRetentionPolicy::Frozen => "frozen",
            SmbArchiveRetentionPolicy::ProbeAtAdmission => "probe_at_admission",
            SmbArchiveRetentionPolicy::ProbeAtAdmission45 => "probe_at_admission_45",
        },
        "suffix_policy": match suffix_policy {
            SmbArchiveSuffixPolicy::OneOrTwo => "one_or_two",
            SmbArchiveSuffixPolicy::BurstUpToFour => "burst_up_to_four",
        },
        "controller_vocabulary": "frozen_nine_mask",
        "parent_scheduler": if matches!(selection, ResumeSelection::PlayBucket) {
            match selector_policy {
                SmbArchiveSelectorPolicy::ConcentratedRecency => "concentrated_recency_128",
            }
        } else if frozen_search {
            "frozen_frontier_128"
        } else {
            "progress_band"
        },
        "executor_mode": "snapshot_resume_archive",
        "campaign": summary,
    });
    let mut summary = summary;
    if let Some(bucket) = play_bucket {
        summary["play_bucket"] = bucket.into();
    }
    fs::write(
        output.join("archive-summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn frontier_entries(
    source: &SmbArchiveReport,
) -> Result<Vec<&fuzzer::phase4c::SmbArchiveEntryReport>, Box<dyn Error>> {
    let frontier = source
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level, entry.key.progress))
        .max()
        .ok_or("source archive contains no retained entries")?;
    let mut entries = source
        .entries
        .iter()
        .filter(|entry| (entry.key.world, entry.key.level, entry.key.progress) == frontier)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| (entry.input.actions.len(), entry.id));
    Ok(entries)
}

/// M53 resume rule: the shortest input at the supplied play bucket of the
/// source archive's maximal `(world, level)` pair, earlier id on ties.
fn play_bucket_resume_input(
    source: &SmbArchiveReport,
    play_bucket: u16,
) -> Result<SmbInput, Box<dyn Error>> {
    let pair = source
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level))
        .max()
        .ok_or("source archive contains no retained entries")?;
    source
        .entries
        .iter()
        .filter(|entry| {
            (entry.key.world, entry.key.level) == pair && entry.key.progress == play_bucket
        })
        .min_by_key(|entry| (entry.input.actions.len(), entry.id))
        .map(|entry| entry.input.clone())
        .ok_or_else(|| "source archive contains no input at the supplied play bucket".into())
}

fn frontier_band_entries(
    source: &SmbArchiveReport,
) -> Result<Vec<&fuzzer::phase4c::SmbArchiveEntryReport>, Box<dyn Error>> {
    let frontier = source
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level, entry.key.progress))
        .max()
        .ok_or("source archive contains no retained entries")?;
    let mut entries = source
        .entries
        .iter()
        .filter(|entry| {
            entry.key.world == frontier.0
                && entry.key.level == frontier.1
                && entry.key.progress.saturating_add(7) >= frontier.2
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| (entry.input.actions.len(), entry.id));
    Ok(entries)
}

fn archive_summary(
    report: &SmbArchiveReport,
    replay_verified: Option<bool>,
    champion_input_sha256: String,
    champion_observations_sha256: String,
) -> serde_json::Value {
    serde_json::json!({
        "seed": report.seed,
        "executions": report.executions,
        "milestones": report.milestones,
        "entries": report.entries.len(),
        "retained": report.retained,
        "rejected": report.rejected,
        "deaths": report.deaths,
        "replay_verified": replay_verified,
        "champion_input_sha256": champion_input_sha256,
        "champion_observations_sha256": champion_observations_sha256,
    })
}

fn read_rom() -> Result<Vec<u8>, Box<dyn Error>> {
    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    Ok(fs::read(rom_path)?)
}

fn parse_u64(value: &str) -> Result<u64, Box<dyn Error>> {
    let normalized = value.replace('_', "");
    if let Some(hex) = normalized.strip_prefix("0x") {
        Ok(u64::from_str_radix(hex, 16)?)
    } else {
        Ok(normalized.parse()?)
    }
}

fn parse_replay_flag(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
    mode: &str,
) -> Result<bool, Box<dyn Error>> {
    let replay = match args.next() {
        None => false,
        Some(value) if value == "--replay" => true,
        Some(_) => return Err(format!("unexpected {mode} argument").into()),
    };
    if args.next().is_some() {
        return Err(format!("unexpected extra {mode} argument").into());
    }
    Ok(replay)
}

fn neutral_corpus(inputs: &[SmbInput]) -> Vec<SmbLabeledCorpusEntry> {
    inputs
        .iter()
        .cloned()
        .map(|input| SmbLabeledCorpusEntry {
            input,
            labels: neutral_labels(),
        })
        .collect()
}

fn no_artifacts() -> SmbArtifactConfig<'static> {
    SmbArtifactConfig {
        detector_name: "none",
        detector_retire_after: u64::MAX,
        macro_name: "none",
        macro_retire_after: u64::MAX,
        enable_macro: false,
    }
}

fn run_control(
    rom: &[u8],
    initial_corpus: &[SmbLabeledCorpusEntry],
) -> Result<SmbConfiguredReport, Box<dyn Error>> {
    run_smb_restart_configured(
        rom,
        initial_corpus,
        M12_PILOT_SEED,
        M12_PILOT_EXECUTIONS,
        NullSmbDetector,
        NullSmbMacro,
        SmbArtifactConfig {
            detector_name: "none",
            detector_retire_after: u64::MAX,
            macro_name: "none",
            macro_retire_after: u64::MAX,
            enable_macro: false,
        },
    )
}

fn input_milestones(rom: &[u8], input: &SmbInput) -> Result<SmbMilestones, Box<dyn Error>> {
    let mut aggregate = SmbMilestones::default();
    for observation in observe_smb_input(rom, input)? {
        let wram: &[u8; 2_048] = observation
            .wram
            .as_slice()
            .try_into()
            .map_err(|_| "SMB observation WRAM is not exactly 2 KiB")?;
        let current = smb_milestones_from_wram(wram);
        aggregate.max_1_1_scroll_bucket = aggregate
            .max_1_1_scroll_bucket
            .max(current.max_1_1_scroll_bucket);
        aggregate.reached_1_1_flag |= current.reached_1_1_flag;
        aggregate.reached_1_2 |= current.reached_1_2;
        aggregate.reached_onward |= current.reached_onward;
    }
    Ok(aggregate)
}

fn milestone_key(milestones: SmbMilestones) -> (bool, bool, bool, u16) {
    (
        milestones.reached_onward,
        milestones.reached_1_2,
        milestones.reached_1_1_flag,
        milestones.max_1_1_scroll_bucket,
    )
}

fn neutral_labels() -> TriageLabels {
    TriageLabels {
        interest: Interest::Neutral,
        duplicate_of: None,
        flags: Vec::<Flag>::new(),
        tags: Vec::new(),
        summary: "neutral frozen-baseline label".to_owned(),
        hypotheses: Vec::new(),
    }
}
