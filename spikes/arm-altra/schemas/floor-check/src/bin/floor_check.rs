// SPDX-License-Identifier: AGPL-3.0-or-later
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

use arm_harness::evidence::ARM64_WORK_CLOCK_BINDING;
use clap::Parser;
use floor_check::{ALL_CHECK_IDS, CheckId, Floors, Status, check_run_sets};

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
    /// One or more run-set directories (each contains `run-set.json` and its records
    /// file). Passing several sums them into ONE stage verdict — AA-1's million-overflow
    /// floor is cumulative across the required condition matrix, one run-set per
    /// condition, so the conditions must be summable rather than each meeting the floor
    /// alone.
    #[arg(required = true)]
    run_set_dir: Vec<PathBuf>,

    /// Fail unless at least this many armed overflows are present. The normative
    /// AA-1/AA-3 floor is 1_000_000; a smaller value fails closed unless
    /// `--sub-normative` is also passed.
    #[arg(long)]
    min_armed_overflows: Option<u64>,

    /// Fail unless at least this many same-seed repetitions are present. The normative
    /// AA-6 floor is 1_000; a smaller value fails closed unless `--sub-normative`.
    #[arg(long)]
    min_reps: Option<u64>,

    /// Fail unless the armed deadlines span at least this many DISTINCT target/seed cases.
    /// Binds the `cases` plan dimension separately from the deadline total, so an AA-1/AA-3
    /// run cannot meet the ≥10⁶ armed floor by cloning a handful of targets across reps.
    /// Absent for an armed AA-1/AA-3 run reads NOT-REQUESTED, never a silent pass.
    #[arg(long)]
    min_cases: Option<u64>,

    /// Permit a floor BELOW the stage-normative minimum. Off by default so a weakened
    /// verdict is never produced by accident; when on, sub-normative outcomes are marked
    /// `[SUB-NORMATIVE]` and can never be read as a normative acceptance. For fixtures
    /// and dev runs, not a disposition.
    #[arg(long)]
    sub_normative: bool,

    /// Rest the exit code on ONLY the named checks (repeatable, kebab-case check ids, e.g.
    /// `--scope rep-floor --scope replay-identity`). A SCOPED run exits 0 iff every named
    /// check PASSED; out-of-scope checks — including any that FAILED — are still printed for
    /// evidence but do not gate. This is what makes a manifest's "the AA-6 mini determinism
    /// gate is DEMONSTRATED" machine-checkable rather than prose-reconciled against a full-run
    /// FAIL (bead hm-7q0). A scoped verdict is marked `[SCOPED]` and is NEVER a full stage
    /// acceptance: it is fail-closed on an unknown/never-ran check and on an empty scope.
    #[arg(long = "scope", value_name = "CHECK-ID")]
    scope: Vec<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let floors = Floors {
        min_armed_overflows: cli.min_armed_overflows,
        min_reps: cli.min_reps,
        min_cases: cli.min_cases,
        sub_normative: cli.sub_normative,
    };

    let dirs: Vec<&std::path::Path> = cli.run_set_dir.iter().map(PathBuf::as_path).collect();
    let report = match check_run_sets(&dirs, &floors) {
        Ok(report) => report,
        Err(e) => {
            eprintln!("floor-check: cannot load run-set: {e}");
            // A load failure is not a pass. Exit nonzero.
            return ExitCode::from(2);
        }
    };

    // stdout: the per-check summary — clean, deterministic, capture-friendly. The DETAIL
    // is printed too, so the exact floor a disposition rests on ("meets the floor of N")
    // is on the face of the retained verdict, never only in a status word.
    println!("work clock binding: {ARM64_WORK_CLOCK_BINDING}");
    println!(
        "floor-check {} stage={}",
        report.run_set_id,
        stage_slug(report.stage)
    );
    for o in &report.outcomes {
        println!("  [{}] {}: {}", o.status, o.id, o.detail);
    }

    // stderr: the detail behind every failure, and behind every floor the evidence
    // needed but nobody named.
    for o in &report.outcomes {
        match o.status {
            Status::Fail => eprintln!("FAIL {}: {}", o.id, o.detail),
            Status::NotRequested => eprintln!("NOT-REQUESTED {}: {}", o.id, o.detail),
            Status::Pass => {}
        }
    }

    // A SCOPED invocation rests the exit code on ONLY the named checks (bead hm-7q0). It is a
    // distinct disposition from a full run, so it is handled here and returns before the
    // full-run exit logic — a scoped verdict is never a full stage acceptance.
    if !cli.scope.is_empty() {
        let mut scope: Vec<CheckId> = Vec::new();
        for name in &cli.scope {
            match CheckId::from_name(name) {
                Some(id) => scope.push(id),
                None => {
                    eprintln!(
                        "floor-check: --scope names an unknown check {name:?}; valid ids: {}",
                        names(ALL_CHECK_IDS)
                    );
                    // A scope that names a check that does not exist cannot be satisfied.
                    return ExitCode::from(2);
                }
            }
        }
        let verdict = report.scoped_verdict(&scope);
        // The out-of-scope FAILs are on the face of the verdict (a scoped run hides nothing),
        // and so is every reason the scope was or was not demonstrated.
        if !verdict.out_of_scope_failed.is_empty() {
            eprintln!(
                "SCOPED: {} out-of-scope check(s) FAILED but do not gate this scoped verdict: {}",
                verdict.out_of_scope_failed.len(),
                names(&verdict.out_of_scope_failed)
            );
        }
        if verdict.accepted() {
            println!(
                "RESULT: [SCOPED] PASS — the scoped set is DEMONSTRATED ({} check(s): {}). NOT a \
                 full stage acceptance: {} out-of-scope check(s) not gated{}.",
                verdict.passed.len(),
                names(&verdict.passed),
                verdict.out_of_scope_failed.len(),
                if verdict.out_of_scope_failed.is_empty() {
                    String::new()
                } else {
                    format!(" (incl. FAILs: {})", names(&verdict.out_of_scope_failed))
                }
            );
            return ExitCode::SUCCESS;
        }
        let mut reasons = Vec::new();
        if !verdict.not_passed.is_empty() {
            reasons.push(format!(
                "{} scoped check(s) did not pass: {}",
                verdict.not_passed.len(),
                names(&verdict.not_passed)
            ));
        }
        if !verdict.missing.is_empty() {
            reasons.push(format!(
                "{} scoped check(s) did not run (fail-closed): {}",
                verdict.missing.len(),
                names(&verdict.missing)
            ));
        }
        if scope.is_empty() {
            reasons.push("an empty scope demonstrates nothing".to_string());
        }
        println!("RESULT: [SCOPED] FAIL — {}.", reasons.join("; "));
        return ExitCode::FAILURE;
    }

    let failed = report.failed();
    let not_requested = report.not_requested();

    if failed.is_empty() && not_requested.is_empty() {
        println!("RESULT: PASS ({} checks)", report.outcomes.len());
        return ExitCode::SUCCESS;
    }

    if failed.is_empty() {
        // Nothing failed — but the verdict is silent about a floor the evidence
        // needs, and a silent verdict is not an accepting one. Exit nonzero: a
        // disposition may not rest on this.
        println!(
            "RESULT: INCOMPLETE ({} of {} checks could not run: {}). No check FAILED, but this \
             verdict does not accept the run-set: name the floor(s) explicitly and re-run.",
            not_requested.len(),
            report.outcomes.len(),
            names(&not_requested)
        );
        return ExitCode::FAILURE;
    }

    println!(
        "RESULT: FAIL ({} of {} checks failed: {}){}",
        failed.len(),
        report.outcomes.len(),
        names(&failed),
        if not_requested.is_empty() {
            String::new()
        } else {
            format!(
                "; {} not requested: {}",
                not_requested.len(),
                names(&not_requested)
            )
        }
    );
    ExitCode::FAILURE
}

/// Render a list of check ids for the verdict line.
fn names(ids: &[floor_check::CheckId]) -> String {
    ids.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
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
