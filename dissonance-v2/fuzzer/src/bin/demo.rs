// SPDX-License-Identifier: AGPL-3.0-or-later

//! Single-command demonstration of Phase 2 steering and Phase 3 rescue.

use std::{error::Error, path::PathBuf};

use fuzzer::{
    phase2::{run_null, run_scripted},
    phase3::{install_build_restart, run_blind_baseline},
};

const PHASE2_BUDGET: u64 = 100_000;
const PHASE3_BASELINE_BUDGET: u64 = 2_000;
const PHASE3_RESCUE_BUDGET: u64 = 20_000;

fn median(values: &mut [u64]) -> u64 {
    values.sort_unstable();
    values[values.len() / 2]
}

fn main() -> Result<(), Box<dyn Error>> {
    let output = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/demo-output")
        .join(std::process::id().to_string());
    std::fs::create_dir_all(&output)?;

    let mut null_times = Vec::new();
    let mut scripted_times = Vec::new();
    for offset in 0_u64..6 {
        let seed = 0x5eed_d000 + offset;
        let null = run_null(
            &output.join(format!("phase2-null-{offset}")),
            seed,
            PHASE2_BUDGET,
        )?;
        let scripted = run_scripted(
            &output.join(format!("phase2-scripted-{offset}")),
            seed,
            PHASE2_BUDGET,
        )?;
        null_times.push(null.time_to_target.unwrap_or(PHASE2_BUDGET + 1));
        scripted_times.push(scripted.time_to_target.unwrap_or(PHASE2_BUDGET + 1));
    }
    let null_median = median(&mut null_times);
    let scripted_median = median(&mut scripted_times);

    let phase3_output = output.join("phase3-campaign");
    let baseline = run_blind_baseline(&phase3_output, 0x5eed_d300, PHASE3_BASELINE_BUDGET)?;
    let rescued = install_build_restart(
        &phase3_output,
        &output.join("phase3-build"),
        PHASE3_RESCUE_BUDGET,
    )?;

    println!("Dissonance v2 LibAFL demo");
    println!("phase 2 null triage time-to-target:     {null_median} executions (median, 6 seeds)");
    println!(
        "phase 2 scripted triage time-to-target: {scripted_median} executions (median, 6 seeds)"
    );
    println!(
        "phase 3 baseline time-to-target:        not reached; proven plateau after {} executions at position {}",
        baseline.executions, baseline.maximum_position
    );
    println!(
        "phase 3 detector-rescue time-to-target: {} executions after restart (position {})",
        rescued.invocation_executions, rescued.maximum_position
    );
    println!("artifacts: {}", output.display());
    Ok(())
}
