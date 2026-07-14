//! `floor-check` — recompute a run-set's acceptance floors from its retained
//! records and print a per-check PASS/FAIL verdict.
//!
//! The output is deterministic evidence in its own right (`docs/ARM-ALTRA.md`
//! §Evidence integrity #2: "the checker's output is itself retained evidence"):
//! no timestamps, no wall-clock, no iteration-order. Exit 0 iff every check
//! passed. The per-check summary goes to stdout; the failure detail goes to
//! stderr, so a `> verdict.txt` capture is the clean summary and the noise stays
//! on the console.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use floor_check::{Floors, Status, check_run_set};

/// Recompute a run-set's acceptance floors from its retained per-sample records.
#[derive(Parser, Debug)]
#[command(
    name = "floor-check",
    about = "Recompute an ARM-spike run-set's acceptance floors from its retained records",
    long_about = "Given a run-set directory (a run-set.json manifest plus a records.jsonl file), \
                  recompute every acceptance floor from the raw per-sample records and never trust \
                  a summary. Exit 0 only if every check passes. A stage disposition may rest on \
                  this verdict; a harness done-marker may not."
)]
struct Cli {
    /// The run-set directory (contains `run-set.json` and its records file).
    run_set_dir: PathBuf,

    /// Fail unless at least this many armed overflows are present in the records.
    /// The real AA-1/AA-3 floor is 1_000_000; pass it explicitly so the number a
    /// disposition rests on is visible in the command.
    #[arg(long)]
    min_armed_overflows: Option<u64>,

    /// Fail unless at least this many samples (same-seed repetitions) are present.
    /// The real AA-6 mini-gate floor is 1_000.
    #[arg(long)]
    min_reps: Option<u64>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let floors = Floors {
        min_armed_overflows: cli.min_armed_overflows,
        min_reps: cli.min_reps,
    };

    let report = match check_run_set(&cli.run_set_dir, &floors) {
        Ok(report) => report,
        Err(e) => {
            eprintln!("floor-check: cannot load run-set: {e}");
            // A load failure is not a pass. Exit nonzero.
            return ExitCode::from(2);
        }
    };

    // stdout: the per-check summary — clean, deterministic, capture-friendly.
    println!(
        "floor-check {} stage={}",
        report.run_set_id,
        stage_slug(report.stage)
    );
    for o in &report.outcomes {
        println!("  [{}] {}", o.status, o.id);
    }

    // stderr: the detail behind every failure.
    for o in &report.outcomes {
        if o.status == Status::Fail {
            eprintln!("FAIL {}: {}", o.id, o.detail);
        }
    }

    let failed = report.failed();
    if failed.is_empty() {
        println!("RESULT: PASS ({} checks)", report.outcomes.len());
        ExitCode::SUCCESS
    } else {
        println!(
            "RESULT: FAIL ({} of {} checks failed: {})",
            failed.len(),
            report.outcomes.len(),
            failed
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
        ExitCode::FAILURE
    }
}

fn stage_slug(stage: arm_harness::evidence::Stage) -> &'static str {
    use arm_harness::evidence::Stage;
    match stage {
        Stage::Aa0 => "aa0",
        Stage::Aa1 => "aa1",
        Stage::Aa2 => "aa2",
        Stage::Aa3 => "aa3",
        Stage::Aa4 => "aa4",
        Stage::Aa5 => "aa5",
        Stage::Aa6 => "aa6",
    }
}
