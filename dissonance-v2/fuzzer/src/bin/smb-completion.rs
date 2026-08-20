// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deterministic champion–challenger campaigns for the SMB completion experiment.

use std::{env, error::Error, fs, path::PathBuf};

use fuzzer::{
    smb::archive::{
        SmbArchiveReport, SmbControlCensusReport, SmbPlayerColumnSelection, SmbSteerScanReport,
        audit_smb_frontier_viability, audit_smb_player_column_contrast,
        audit_smb_player_column_derived, audit_smb_player_column_from_ids,
        audit_smb_player_column_separation, audit_smb_player_column_spread,
        audit_smb_player_column_verified, audit_smb_player_column_with_selection,
        audit_smb_terminal_death, census_smb_control_authority, census_smb_frame_cost,
        census_smb_lineage_levels, derive_smb_ladder, diagnose_smb_film_columns,
        diagnose_smb_film_measurements, diagnose_smb_film_measurements_derived,
        diagnose_smb_frame_slack, diagnose_smb_left_direction, diagnose_smb_player_column,
        diagnose_smb_span, diagnose_smb_stall_slack, gate_smb_live_control,
        measure_smb_viable_progress, readmit_smb_archive, replay_smb_claim_lineage,
        select_smb_responsive_audit_ids, select_smb_span_audit_ids, select_smb_spread_audit_ids,
        select_smb_steered_audit_ids,
    },
    smb::target::encode_smb_frame_png,
};
use sha2::Digest;

const CENSUS_AUDIT_ENTRIES: usize = 8;
const TWO_STATE_AUDIT_ENTRIES: usize = 2;
fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let mode = args
        .next()
        .ok_or("usage: smb-completion <diagnostic-mode> ...")?;
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
    if mode == "diagnose-loop-differential" {
        return run_loop_differential_mode(&mut args);
    }
    if mode == "diagnose-wram-diff" {
        return run_wram_diff_mode(&mut args);
    }
    if mode == "derive-origin-pair" {
        return run_derive_origin_pair_mode(&mut args);
    }
    if mode == "derive-origin-entrance" {
        return run_derive_origin_entrance_mode(&mut args);
    }
    if mode == "report-resume-pick" {
        return run_resume_pick_mode(&mut args);
    }
    if mode == "census-suffix-chords" {
        return run_census_suffix_chords_mode(&mut args);
    }
    if mode == "measure-viable-progress" {
        return run_viable_progress_mode(&mut args);
    }
    if mode == "gate-claim-replay" {
        return run_claim_replay_mode(&mut args);
    }
    if mode == "census-frame-cost" {
        return run_frame_cost_mode(&mut args);
    }
    if mode == "census-lineage-levels" {
        return run_lineage_level_mode(&mut args);
    }
    if mode == "diagnose-stall-slack" {
        return run_stall_slack_mode(&mut args);
    }
    if mode == "diagnose-frame-slack" {
        return run_frame_slack_mode(&mut args);
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
    Err("unknown smb-completion mode".into())
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

fn run_census_suffix_chords_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let world = u8::try_from(parse_u64(
        &args.next().ok_or("missing world")?.to_string_lossy(),
    )?)?;
    let level = u8::try_from(parse_u64(
        &args.next().ok_or("missing level")?.to_string_lossy(),
    )?)?;
    let min_progress = u16::try_from(parse_u64(
        &args.next().ok_or("missing min progress")?.to_string_lossy(),
    )?)?;
    let prefix_len = usize::try_from(parse_u64(
        &args
            .next()
            .ok_or("missing prefix length")?
            .to_string_lossy(),
    )?)?;
    let output = PathBuf::from(args.next().ok_or("missing output file")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(&source_path)?)?;
    let mut chords: Vec<(u8, u8)> = Vec::new();
    let mut entries_used = 0_u64;
    for entry in &source.entries {
        if (entry.key.world, entry.key.level) != (world, level)
            || entry.key.progress < min_progress
            || entry.input.actions.len() <= prefix_len
        {
            continue;
        }
        entries_used += 1;
        for action in &entry.input.actions[prefix_len..] {
            chords.push((action.buttons, action.hold_frames));
        }
    }
    let mut mask_hist = std::collections::BTreeMap::<u8, u64>::new();
    let mut hold_hist = std::collections::BTreeMap::<u8, u64>::new();
    for (mask, hold) in &chords {
        *mask_hist.entry(*mask).or_insert(0) += 1;
        *hold_hist.entry(hold / 12).or_insert(0) += 1;
    }
    let report = serde_json::json!({
        "entries_used": entries_used,
        "chords": chords,
        "mask_histogram": mask_hist,
        "hold_histogram_by_12s": hold_hist,
    });
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!("entries {} chords {}", entries_used, chords.len());
    println!("masks: {:?}", mask_hist);
    println!("holds/12: {:?}", hold_hist);
    Ok(())
}

/// Derive an origin whose deepest entries stand at a level's entrance, so a
/// campaign searches that level forward instead of inheriting a route through
/// it. Retains only entries of the named pair at or below `max_progress`;
/// everything deeper — including the carried route — is dropped, which is the
/// point and is why the source archive is left untouched on disk.
/// Report which entry each resume rule would select from an archive, so a
/// registration can state its resume entry before the run exists — and so a
/// rule that picks wrongly is caught without spending a campaign on it.
fn run_resume_pick_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(&source_path)?)?;
    let frontier = source
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level, entry.key.progress))
        .max()
        .ok_or("source archive contains no retained entries")?;
    println!("frontier {:?}; entries {}", frontier, source.entries.len());
    for policy in [
        fuzzer::smb::campaign::SmbCampaignResumePolicy::FrontierShortest,
        fuzzer::smb::campaign::SmbCampaignResumePolicy::FastestInLevelWithin32,
        fuzzer::smb::campaign::SmbCampaignResumePolicy::FastestToDepth32,
    ] {
        let input = fuzzer::smb::campaign::select_frontier_resume_input(&source, policy)?;
        let sha = format!("{:x}", sha2::Sha256::digest(serde_json::to_vec(&input)?));
        // Name the entry the picked input belongs to, so the bucket it stands
        // on is on the record beside the hash.
        let entry = source.entries.iter().find(|entry| entry.input == input);
        let frames: u64 = input
            .actions
            .iter()
            .map(|action| u64::from(action.bounded_hold_frames()))
            .sum();
        println!(
            "  {:24} bucket {:>4} actions {:>5} total_frames {:>7} sha {}",
            fuzzer::smb::campaign::resume_identifier(policy),
            entry.map_or(-1_i32, |entry| i32::from(entry.key.progress)),
            input.actions.len(),
            frames,
            sha
        );
    }
    Ok(())
}

fn run_derive_origin_entrance_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let world = u8::try_from(parse_u64(
        &args.next().ok_or("missing world")?.to_string_lossy(),
    )?)?;
    let level = u8::try_from(parse_u64(
        &args.next().ok_or("missing level")?.to_string_lossy(),
    )?)?;
    let max_progress = u16::try_from(parse_u64(
        &args
            .next()
            .ok_or("missing maximum progress")?
            .to_string_lossy(),
    )?)?;
    let output = PathBuf::from(args.next().ok_or("missing output file")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let mut source: SmbArchiveReport = serde_json::from_slice(&fs::read(&source_path)?)?;
    let before = source.entries.len();
    source.entries.retain(|entry| {
        (entry.key.world, entry.key.level) == (world, level) && entry.key.progress <= max_progress
    });
    let after = source.entries.len();
    if after == 0 {
        return Err("origin derivation kept no entries at the requested entrance".into());
    }
    // Report the resume input each policy would pick, so the registration can
    // name the one it launches under and replay can be checked against it.
    let frozen = fuzzer::smb::campaign::select_frontier_resume_input(
        &source,
        fuzzer::smb::campaign::SmbCampaignResumePolicy::FrontierShortest,
    )?;
    let fastest = fuzzer::smb::campaign::select_frontier_resume_input(
        &source,
        fuzzer::smb::campaign::SmbCampaignResumePolicy::FastestInLevelWithin32,
    )?;
    let digest = |input: &fuzzer::smb::target::SmbInput| -> Result<String, Box<dyn Error>> {
        Ok(format!(
            "{:x}",
            sha2::Sha256::digest(serde_json::to_vec(input)?)
        ))
    };
    let deepest = source
        .entries
        .iter()
        .map(|entry| entry.key.progress)
        .max()
        .unwrap_or(0);
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, serde_json::to_vec(&source)?)?;
    println!(
        "kept {after} of {before} entries; deepest bucket {deepest}; \
frontier_shortest resume {} actions sha {}; fastest_in_level_32 resume {} actions sha {}",
        frozen.actions.len(),
        digest(&frozen)?,
        fastest.actions.len(),
        digest(&fastest)?
    );
    Ok(())
}

fn run_derive_origin_pair_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let world = u8::try_from(parse_u64(
        &args.next().ok_or("missing world")?.to_string_lossy(),
    )?)?;
    let level = u8::try_from(parse_u64(
        &args.next().ok_or("missing level")?.to_string_lossy(),
    )?)?;
    let output = PathBuf::from(args.next().ok_or("missing output file")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let mut source: SmbArchiveReport = serde_json::from_slice(&fs::read(&source_path)?)?;
    let resume_before = fuzzer::smb::campaign::select_frontier_resume_input(
        &source,
        fuzzer::smb::campaign::SmbCampaignResumePolicy::FrontierShortest,
    )?;
    let before = source.entries.len();
    source
        .entries
        .retain(|entry| (entry.key.world, entry.key.level) == (world, level));
    let after = source.entries.len();
    let resume_after = fuzzer::smb::campaign::select_frontier_resume_input(
        &source,
        fuzzer::smb::campaign::SmbCampaignResumePolicy::FrontierShortest,
    )?;
    if resume_before != resume_after {
        return Err("origin slimming changed the resume input".into());
    }
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, serde_json::to_vec(&source)?)?;
    println!("kept {after} of {before} entries; resume input unchanged");
    Ok(())
}

fn run_wram_diff_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let a_low = u16::try_from(parse_u64(
        &args.next().ok_or("missing group-a low")?.to_string_lossy(),
    )?)?;
    let a_high = u16::try_from(parse_u64(
        &args.next().ok_or("missing group-a high")?.to_string_lossy(),
    )?)?;
    let b_low = u16::try_from(parse_u64(
        &args.next().ok_or("missing group-b low")?.to_string_lossy(),
    )?)?;
    let b_high = u16::try_from(parse_u64(
        &args.next().ok_or("missing group-b high")?.to_string_lossy(),
    )?)?;
    let cap = usize::try_from(parse_u64(
        &args
            .next()
            .ok_or("missing per-group cap")?
            .to_string_lossy(),
    )?)?;
    let output = PathBuf::from(args.next().ok_or("missing output file")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let report = fuzzer::smb::campaign::diagnose_wram_diff(
        &rom,
        &source,
        (a_low, a_high),
        (b_low, b_high),
        cap,
        32,
    )?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!("sampled A={} B={}", report.sampled.0, report.sampled.1);
    for b in report.bytes.iter().take(12) {
        println!(
            "offset {:#06x} separates {} a_modes {} A {:?} B {:?}",
            b.offset,
            b.separates,
            b.group_a_modes,
            &b.group_a_values[..b.group_a_values.len().min(6)],
            &b.group_b_values[..b.group_b_values.len().min(6)]
        );
    }
    Ok(())
}

fn run_loop_differential_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let bucket_low = u16::try_from(parse_u64(
        &args.next().ok_or("missing bucket low")?.to_string_lossy(),
    )?)?;
    let bucket_high = u16::try_from(parse_u64(
        &args.next().ok_or("missing bucket high")?.to_string_lossy(),
    )?)?;
    let advance_threshold = u16::try_from(parse_u64(
        &args
            .next()
            .ok_or("missing advance threshold")?
            .to_string_lossy(),
    )?)?;
    let sample_cap = usize::try_from(parse_u64(
        &args.next().ok_or("missing sample cap")?.to_string_lossy(),
    )?)?;
    let probe_chords = u16::try_from(parse_u64(
        &args.next().ok_or("missing probe chords")?.to_string_lossy(),
    )?)?;
    let output = PathBuf::from(args.next().ok_or("missing output file")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let report = fuzzer::smb::campaign::diagnose_loop_differential(
        &rom,
        &source,
        (bucket_low, bucket_high),
        advance_threshold,
        sample_cap,
        probe_chords,
        24,
    )?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    let (advanced, looped, dead, held) = report.outcomes;
    println!(
        "probed {} advanced {} looped {} dead {} held {}",
        report.probed, advanced, looped, dead, held
    );
    for d in report.discriminators.iter().take(10) {
        println!(
            "offset {:#06x} separates {} advanced {:?} looped {:?}",
            d.offset, d.separates, d.advanced_values, d.looped_values
        );
    }
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
    let vertical = std::env::var("HARMONY_TRANSIT_VERTICAL").is_ok();
    let origin: Option<SmbArchiveReport> = match std::env::var("HARMONY_TRANSIT_ORIGIN") {
        Ok(path) => Some(serde_json::from_slice(&fs::read(path)?)?),
        Err(_) => None,
    };
    let report = fuzzer::smb::campaign::diagnose_x_transit(
        &rom,
        &stream_text,
        &source,
        origin.as_ref(),
        (parent_low, parent_high),
        sample_cap,
        vertical,
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
    let report = fuzzer::smb::campaign::diagnose_down_census(
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
    let report = fuzzer::smb::campaign::diagnose_refused_grid(
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

fn run_frame_slack_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let world = u8::try_from(parse_u64(
        &args.next().ok_or("missing world")?.to_string_lossy(),
    )?)?;
    let level = u8::try_from(parse_u64(
        &args.next().ok_or("missing level")?.to_string_lossy(),
    )?)?;
    let output = PathBuf::from(args.next().ok_or("missing output file")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let report = diagnose_smb_frame_slack(&rom, &source, world, level)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn run_frame_cost_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let world = u8::try_from(parse_u64(
        &args.next().ok_or("missing world")?.to_string_lossy(),
    )?)?;
    let level = u8::try_from(parse_u64(
        &args.next().ok_or("missing level")?.to_string_lossy(),
    )?)?;
    let output = PathBuf::from(args.next().ok_or("missing output file")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let report = census_smb_frame_cost(&source, world, level);
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "buckets {} entries {}",
        report.buckets.len(),
        report.entries
    );
    Ok(())
}

fn run_stall_slack_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let world = u8::try_from(parse_u64(
        &args.next().ok_or("missing world")?.to_string_lossy(),
    )?)?;
    let level = u8::try_from(parse_u64(
        &args.next().ok_or("missing level")?.to_string_lossy(),
    )?)?;
    let minimum_frames = parse_u64(
        &args
            .next()
            .ok_or("missing minimum stall frames")?
            .to_string_lossy(),
    )?;
    let maximum_buckets = u16::try_from(parse_u64(
        &args
            .next()
            .ok_or("missing maximum stall buckets")?
            .to_string_lossy(),
    )?)?;
    let output = PathBuf::from(args.next().ok_or("missing output file")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let report = diagnose_smb_stall_slack(
        &rom,
        &source,
        world,
        level,
        minimum_frames,
        maximum_buckets,
        true,
    )?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "stalls {} costing {} frames; recoverable {}",
        report.stalls.len(),
        report.stall_frames,
        report.recoverable_frames
    );
    Ok(())
}

fn run_lineage_level_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let output = PathBuf::from(args.next().ok_or("missing output file")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let report = census_smb_lineage_levels(&rom, &source)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "entry {} actions {} frames {} segments {}",
        report.entry_id,
        report.actions,
        report.frames,
        report.segments.len()
    );
    Ok(())
}

fn run_claim_replay_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let output = PathBuf::from(args.next().ok_or("missing output file")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source_path)?)?;
    let report = replay_smb_claim_lineage(&rom, &source)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
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
