// SPDX-License-Identifier: AGPL-3.0-or-later

//! Runnable Phase 0 byte-input fuzzer.

use std::{env, error::Error, path::PathBuf};

use fuzzer::phase0::run;

fn main() -> Result<(), Box<dyn Error>> {
    let output_dir = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("phase0-output"));
    let report = run(&output_dir, 256)?;
    println!(
        "phase0: resumed={} executions={} corpus={} crashes={} output={}",
        report.resumed,
        report.executions,
        report.corpus_count,
        report.solutions_count,
        output_dir.display()
    );
    Ok(())
}
