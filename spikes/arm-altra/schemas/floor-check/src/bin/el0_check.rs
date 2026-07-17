// SPDX-License-Identifier: AGPL-3.0-or-later
//! `el0-check` — recompute the AA-1(a) EL0 counting floors from retained records.
//!
//! The EL0 sibling of `floor-check`: given one or more `arm-el0-count` run-set
//! directories (`el0-set.json` + `el0-records.jsonl`), recompute every check from
//! the raw records — the oracle-exactness verdict and its measured per-class
//! offsets are printed on the face of the output, because those offsets are the
//! stage's constants-pack deliverable. Exit 0 only if every check passes; a
//! harness done-marker is never success.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use floor_check::check::Status;
use floor_check::el0::{El0Floors, check_el0_sets};

#[derive(Parser, Debug)]
#[command(
    name = "el0-check",
    about = "Recompute an AA-1(a) EL0 run-set's acceptance floors from its retained records"
)]
struct Cli {
    /// One or more EL0 run-set directories. Passing several sums them into ONE
    /// verdict (they must be distinct evidence over one comparable environment).
    #[arg(required = true)]
    run_set_dir: Vec<PathBuf>,

    /// Fail unless every `(class, scale, seed)` case repeats at least this often.
    #[arg(long)]
    min_reps: Option<u64>,

    /// Fail unless every `(class, scale)` covers at least this many distinct seeds.
    #[arg(long)]
    min_cases: Option<u64>,

    /// Permit an incomplete 1e6/1e7/1e8 differential (dev/smoke runs). Tagged
    /// `[SUB-NORMATIVE]`; never a normative acceptance.
    #[arg(long)]
    sub_normative: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let floors = El0Floors {
        min_reps: cli.min_reps,
        min_cases: cli.min_cases,
        sub_normative: cli.sub_normative,
    };
    let dirs: Vec<&std::path::Path> = cli.run_set_dir.iter().map(PathBuf::as_path).collect();
    let report = match check_el0_sets(&dirs, &floors) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("el0-check: cannot load run-set: {e}");
            return ExitCode::from(2);
        }
    };

    println!("el0-check {}", report.run_set_ids.join("+"));
    for o in &report.outcomes {
        println!("  [{}] {}: {}", o.status, o.id, o.detail);
    }
    for o in &report.outcomes {
        match o.status {
            Status::Fail => eprintln!("FAIL {}: {}", o.id, o.detail),
            Status::NotRequested => eprintln!("NOT-REQUESTED {}: {}", o.id, o.detail),
            Status::Pass => {}
        }
    }

    let failed: Vec<_> = report
        .outcomes
        .iter()
        .filter(|o| o.status == Status::Fail)
        .collect();
    let not_requested: Vec<_> = report
        .outcomes
        .iter()
        .filter(|o| o.status == Status::NotRequested)
        .collect();
    if failed.is_empty() && not_requested.is_empty() {
        println!("RESULT: PASS ({} checks)", report.outcomes.len());
        ExitCode::SUCCESS
    } else if failed.is_empty() {
        println!(
            "RESULT: INCOMPLETE ({} of {} checks could not run). No check FAILED, but this \
             verdict does not accept the run-set.",
            not_requested.len(),
            report.outcomes.len()
        );
        ExitCode::FAILURE
    } else {
        println!(
            "RESULT: FAIL ({} of {} checks failed)",
            failed.len(),
            report.outcomes.len()
        );
        ExitCode::FAILURE
    }
}
