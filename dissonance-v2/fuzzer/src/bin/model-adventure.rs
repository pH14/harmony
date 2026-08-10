// SPDX-License-Identifier: AGPL-3.0-or-later

//! Live-model Phase 4a-L campaign runner; never executed by unit tests.

use std::{error::Error, ffi::OsString, fs, path::PathBuf};

use fuzzer::phase4a::{
    AdventureRunReport, SearchArm, TriageArm, replay_recorded_adventure_campaign,
    run_adventure_campaign, run_luna_adventure_campaign,
};
use serde::Serialize;

const EXECUTION_BUDGET: u64 = 10_000;
const SEEDS: [u64; 6] = [
    0x5eed_d400,
    0x5eed_d401,
    0x5eed_d402,
    0x5eed_d403,
    0x5eed_d404,
    0x5eed_d405,
];

#[derive(Debug, Serialize)]
struct TriageCell {
    triage: TriageArm,
    execution_counts: Vec<u64>,
    median_executions: u64,
    reached: usize,
    model_calls: usize,
    model_failures: u64,
}

#[derive(Debug, Serialize)]
struct ModelAdventureReport {
    execution_budget: u64,
    seeds: Vec<u64>,
    search: SearchArm,
    cells: Vec<TriageCell>,
    replay_verified: bool,
    clearly_better_threshold: String,
    luna_clearly_better_than_null: bool,
    base_plateau: AdventureRunReport,
    base_plateau_replay_verified: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let (output, triage_agent, extra_triage_args) = parse_args()?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(&output)?;

    let mut cells = Vec::new();
    for triage in [TriageArm::Null, TriageArm::Scripted] {
        let mut runs = Vec::new();
        for (index, seed) in SEEDS.into_iter().enumerate() {
            runs.push(run_adventure_campaign(
                &output.join(format!("{:?}-detector-{index}", triage)),
                seed,
                EXECUTION_BUDGET,
                triage,
                SearchArm::GeneratedDetectors,
            )?);
        }
        cells.push(cell(triage, &runs));
    }

    let mut luna_runs = Vec::new();
    let mut replay_verified = true;
    for (index, seed) in SEEDS.into_iter().enumerate() {
        let run = run_luna_adventure_campaign(
            &output.join(format!("Luna-detector-{index}")),
            seed,
            EXECUTION_BUDGET,
            SearchArm::GeneratedDetectors,
            &triage_agent,
            &extra_triage_args,
        )?;
        let replay = replay_recorded_adventure_campaign(
            &output.join(format!("Luna-detector-{index}-replay")),
            &run,
        )?;
        replay_verified &= run.corpus == replay.corpus
            && run.executions == replay.executions
            && run.time_to_target == replay.time_to_target;
        luna_runs.push(run);
    }
    cells.push(cell(TriageArm::Luna, &luna_runs));

    let base_plateau = run_luna_adventure_campaign(
        &output.join("Luna-base-plateau"),
        0x5eed_d4ff,
        EXECUTION_BUDGET,
        SearchArm::Base,
        &triage_agent,
        &extra_triage_args,
    )?;
    let base_replay = replay_recorded_adventure_campaign(
        &output.join("Luna-base-plateau-replay"),
        &base_plateau,
    )?;
    let base_plateau_replay_verified = base_plateau.corpus == base_replay.corpus
        && base_plateau.executions == base_replay.executions
        && base_plateau.time_to_target == base_replay.time_to_target;

    let null_median = cells
        .iter()
        .find(|cell| cell.triage == TriageArm::Null)
        .ok_or("null cell missing")?
        .median_executions;
    let luna = cells
        .iter()
        .find(|cell| cell.triage == TriageArm::Luna)
        .ok_or("Luna cell missing")?;
    let luna_clearly_better_than_null = luna.median_executions < null_median
        && luna.median_executions.saturating_mul(5) <= null_median.saturating_mul(4);
    let model_failures = luna
        .model_failures
        .saturating_add(base_plateau.triage_failures);
    let report = ModelAdventureReport {
        execution_budget: EXECUTION_BUDGET,
        seeds: SEEDS.to_vec(),
        search: SearchArm::GeneratedDetectors,
        cells,
        replay_verified,
        clearly_better_threshold: "Luna median <= 80% of null median".to_owned(),
        luna_clearly_better_than_null,
        base_plateau,
        base_plateau_replay_verified,
    };
    fs::write(
        output.join("model-adventure-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;

    println!("Phase 4a-L M1 detector-arm timing");
    for cell in &report.cells {
        println!(
            "{:?}: median={} reached={}/{} calls={} failures={}",
            cell.triage,
            cell.median_executions,
            cell.reached,
            SEEDS.len(),
            cell.model_calls,
            cell.model_failures
        );
    }
    println!("replay verified: {}", report.replay_verified);
    println!(
        "Luna clearly better than null: {}",
        report.luna_clearly_better_than_null
    );
    println!(
        "base plateau: progress={} target={:?} labels={} replay={}",
        report.base_plateau.maximum_progress,
        report.base_plateau.time_to_target,
        report.base_plateau.triage_events.len(),
        report.base_plateau_replay_verified
    );
    println!("artifacts: {}", output.display());

    if model_failures != 0 {
        return Err(format!("M1 recorded {model_failures} model call failures").into());
    }
    if !report.replay_verified || !report.base_plateau_replay_verified {
        return Err("M1 recorded-label replay mismatch".into());
    }
    if report.base_plateau.time_to_target.is_some() {
        return Err("M1 base campaign did not remain at its proven plateau".into());
    }
    if !report.luna_clearly_better_than_null {
        return Err("M1 Luna median did not meet the predeclared improvement threshold".into());
    }
    Ok(())
}

fn cell(triage: TriageArm, runs: &[AdventureRunReport]) -> TriageCell {
    let execution_counts: Vec<_> = runs
        .iter()
        .map(|run| {
            run.time_to_target
                .unwrap_or_else(|| EXECUTION_BUDGET.saturating_add(1))
        })
        .collect();
    let reached = runs
        .iter()
        .filter(|run| run.time_to_target.is_some())
        .count();
    let model_calls = runs.iter().map(|run| run.triage_events.len()).sum();
    let model_failures = runs.iter().fold(0_u64, |total, run| {
        total.saturating_add(run.triage_failures)
    });
    let mut sorted = execution_counts.clone();
    sorted.sort_unstable();
    let median_executions = sorted.get(sorted.len() / 2).copied().unwrap_or(0);
    TriageCell {
        triage,
        execution_counts,
        median_executions,
        reached,
        model_calls,
        model_failures,
    }
}

fn parse_args() -> Result<(PathBuf, PathBuf, Vec<OsString>), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let output =
        PathBuf::from(args.next().ok_or(
            "usage: model-adventure <output-dir> <triage-agent> [triage-agent arguments...]",
        )?);
    let triage_agent =
        PathBuf::from(args.next().ok_or(
            "usage: model-adventure <output-dir> <triage-agent> [triage-agent arguments...]",
        )?);
    Ok((output, triage_agent, args.collect()))
}
