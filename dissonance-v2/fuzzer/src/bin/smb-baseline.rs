// SPDX-License-Identifier: AGPL-3.0-or-later

//! M5 ratchet-versus-random Super Mario Bros campaign runner.

use std::{env, error::Error, fs, path::PathBuf};

use fuzzer::phase4b::{SmbCampaignReport, run_smb_random_mash, run_smb_ratchet};
use serde::Serialize;
use sha2::{Digest, Sha256};

const FIRST_SEED: u64 = 0x5eed_d700;

#[derive(Debug, Serialize)]
struct BaselineReport {
    rom_sha256: String,
    execution_budget: u64,
    seeds: Vec<u64>,
    ratchet: Vec<SmbCampaignReport>,
    random_mash: Vec<SmbCampaignReport>,
    ratchet_median_progress_execution: u64,
    random_median_progress_execution: u64,
    ratchet_median_max_scroll_bucket: u16,
    random_median_max_scroll_bucket: u16,
    ratchet_beats_random: bool,
    ratchet_makes_real_progress: bool,
    plateau: String,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let output = PathBuf::from(
        args.next()
            .ok_or("usage: smb-baseline <output-directory> <budget> <seed-count>")?,
    );
    let execution_budget = args
        .next()
        .ok_or("missing execution budget")?
        .to_string_lossy()
        .parse::<u64>()?;
    let seed_count = args
        .next()
        .ok_or("missing seed count")?
        .to_string_lossy()
        .parse::<usize>()?;
    if seed_count == 0 {
        return Err("seed count must be positive".into());
    }
    fs::create_dir_all(&output)?;
    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    let rom = fs::read(&rom_path)?;
    let rom_sha256 = format!("{:x}", Sha256::digest(&rom));
    let seeds = (0..seed_count)
        .map(|offset| FIRST_SEED.saturating_add(offset as u64))
        .collect::<Vec<_>>();

    let mut ratchet = Vec::new();
    let mut random_mash = Vec::new();
    for seed in &seeds {
        let ratchet_run = run_smb_ratchet(&rom, *seed, execution_budget)?;
        println!(
            "ratchet seed={seed:#x} executions={} max_bucket={} flag={} 1-2={} onward={} corpus={}",
            ratchet_run.executions,
            ratchet_run.milestones.max_1_1_scroll_bucket,
            ratchet_run.milestones.reached_1_1_flag,
            ratchet_run.milestones.reached_1_2,
            ratchet_run.milestones.reached_onward,
            ratchet_run.corpus.len(),
        );
        fs::write(
            output.join(format!("ratchet-{seed:016x}.json")),
            serde_json::to_vec_pretty(&ratchet_run)?,
        )?;
        ratchet.push(ratchet_run);

        let random_run = run_smb_random_mash(&rom, *seed, execution_budget)?;
        println!(
            "random seed={seed:#x} executions={} max_bucket={} flag={} 1-2={} onward={}",
            random_run.executions,
            random_run.milestones.max_1_1_scroll_bucket,
            random_run.milestones.reached_1_1_flag,
            random_run.milestones.reached_1_2,
            random_run.milestones.reached_onward,
        );
        fs::write(
            output.join(format!("random-{seed:016x}.json")),
            serde_json::to_vec_pretty(&random_run)?,
        )?;
        random_mash.push(random_run);
    }

    let censored = execution_budget.saturating_add(1);
    let ratchet_median_progress_execution = median_u64(
        ratchet
            .iter()
            .map(|run| run.first_reached.progress_into_1_1.unwrap_or(censored))
            .collect(),
    );
    let random_median_progress_execution = median_u64(
        random_mash
            .iter()
            .map(|run| run.first_reached.progress_into_1_1.unwrap_or(censored))
            .collect(),
    );
    let ratchet_median_max_scroll_bucket = median_u16(
        ratchet
            .iter()
            .map(|run| run.milestones.max_1_1_scroll_bucket)
            .collect(),
    );
    let random_median_max_scroll_bucket = median_u16(
        random_mash
            .iter()
            .map(|run| run.milestones.max_1_1_scroll_bucket)
            .collect(),
    );
    let ratchet_makes_real_progress = ratchet_median_max_scroll_bucket > 0;
    let ratchet_beats_random = ratchet_median_max_scroll_bucket > random_median_max_scroll_bucket
        || (ratchet_median_max_scroll_bucket == random_median_max_scroll_bucket
            && ratchet_median_progress_execution < random_median_progress_execution);
    let plateau = describe_plateau(&ratchet);
    let report = BaselineReport {
        rom_sha256,
        execution_budget,
        seeds,
        ratchet,
        random_mash,
        ratchet_median_progress_execution,
        random_median_progress_execution,
        ratchet_median_max_scroll_bucket,
        random_median_max_scroll_bucket,
        ratchet_beats_random,
        ratchet_makes_real_progress,
        plateau,
    };
    fs::write(
        output.join("smb-baseline-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.ratchet_makes_real_progress {
        return Err("M5 ratchet did not make progress into 1-1".into());
    }
    if !report.ratchet_beats_random {
        return Err("M5 ratchet did not beat pure random mash".into());
    }
    Ok(())
}

fn median_u64(mut values: Vec<u64>) -> u64 {
    values.sort_unstable();
    values[values.len() / 2]
}

fn median_u16(mut values: Vec<u16>) -> u16 {
    values.sort_unstable();
    values[values.len() / 2]
}

fn describe_plateau(runs: &[SmbCampaignReport]) -> String {
    if runs.iter().any(|run| run.milestones.reached_onward) {
        "onward beyond 1-2".to_owned()
    } else if runs.iter().any(|run| run.milestones.reached_1_2) {
        "reached 1-2 but not onward".to_owned()
    } else if runs.iter().any(|run| run.milestones.reached_1_1_flag) {
        "reached the 1-1 flag but not 1-2".to_owned()
    } else {
        let max_bucket = runs
            .iter()
            .map(|run| run.milestones.max_1_1_scroll_bucket)
            .max()
            .unwrap_or(0);
        format!("1-1 scroll bucket {max_bucket}; flag not reached")
    }
}
