// SPDX-License-Identifier: AGPL-3.0-or-later
//! `cpu-qualification` CLI.
//!
//! Exit codes: `0` when every check passed, `2` when a check failed (the suite is
//! a detector, not a failure), `1` on an operational error — a missing evidence
//! directory, an unreadable pack, a stage that cannot run here.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use cpu_qualification::pack::Pack;
use cpu_qualification::report::{Floors, Record, Verdict, parse_records, recompute, record_line};
use cpu_qualification::stage0::{Stage0Error, Stage0Outcome};
use cpu_qualification::stage1::{MeasurementPlan, Stage1Error, Stage1Outcome, derive_margin};

#[derive(Parser)]
#[command(name = "cpu-qualification", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the suite up to `--stage` and write the run's records.
    Run {
        /// The highest stage to run.
        #[arg(long, value_parser = clap::value_parser!(u8).range(0..=3))]
        stage: u8,
        /// The chip baseline whose pack the run is checked against.
        #[arg(long)]
        baseline: String,
        /// Where to write the run's records.
        #[arg(long)]
        evidence_dir: PathBuf,
    },
    /// Check the chip and the standing host conditions, and print the rows.
    Check {
        /// The chip baseline whose pack the host is checked against.
        #[arg(long)]
        baseline: String,
    },
    /// Recompute every floor from the retained raw records and print the verdict.
    Report {
        /// The run's evidence directory.
        #[arg(long)]
        evidence_dir: PathBuf,
    },
}

/// Read every record stream in `dir`. Record streams are `*.jsonl` files; one
/// JSON object per line.
fn read_records(dir: &Path) -> Result<Vec<Record>, Box<dyn std::error::Error>> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read evidence directory {}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    // Sorted so the report is a function of the directory's content, not of the
    // order the filesystem happened to return.
    paths.sort();
    if paths.is_empty() {
        return Err(format!(
            "no record streams (*.jsonl) in {}: there is nothing to recompute from",
            dir.display()
        )
        .into());
    }
    let mut records = Vec::new();
    for path in paths {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let parsed = parse_records(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        records.extend(parsed);
    }
    Ok(records)
}

fn cmd_report(evidence_dir: &Path) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let records = read_records(evidence_dir)?;
    let verdict: Verdict = recompute(&records);
    println!("{}", serde_json::to_string(&verdict)?);
    Ok(if verdict.passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

/// Run stage 0 against `pack`.
#[cfg(target_os = "linux")]
fn stage0(pack: &Pack) -> Result<Stage0Outcome, Stage0Error> {
    cpu_qualification::stage0_sys::run(pack)
}

/// Stage 0 reads Linux host state, so everywhere else it refuses rather than
/// reporting a host it never looked at.
#[cfg(not(target_os = "linux"))]
fn stage0(_pack: &Pack) -> Result<Stage0Outcome, Stage0Error> {
    Err(Stage0Error::WrongPlatform {
        target: std::env::consts::OS,
    })
}

/// Print one expect-versus-found row per line, deviations marked.
fn print_rows(outcome: &Stage0Outcome) {
    println!(
        "chip {} matched table entry {}",
        outcome.identity_text(),
        outcome.entry_name
    );
    for row in &outcome.rows {
        let mark = match (row.confirmed, row.disposition.as_deref()) {
            (true, _) => "ok      ".to_string(),
            (false, Some(d)) => format!("deviation ({d})"),
            (false, None) => "DEVIATION".to_string(),
        };
        println!(
            "{mark}  {}[{}]  expect {:?}  found {:?}",
            row.condition, row.scope, row.expect, row.found
        );
    }
}

fn cmd_check(baseline: &str) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let pack = Pack::builtin(baseline)?;
    let outcome = stage0(&pack)?;
    print_rows(&outcome);
    let deviations = outcome.deviations();
    if deviations.is_empty() {
        println!("every required condition confirmed against pack {baseline}");
        return Ok(ExitCode::SUCCESS);
    }
    eprintln!(
        "{} condition(s) deviate from pack {baseline} with no disposition",
        deviations.len()
    );
    Ok(ExitCode::from(2))
}

/// Run stage 1 on `plan`.
#[cfg(target_os = "linux")]
fn stage1(config: u64, plan: &MeasurementPlan) -> Result<Stage1Outcome, Stage1Error> {
    cpu_qualification::stage1_sys::run(config, plan)
}

/// Stage 1 measures on Linux, so everywhere else it refuses rather than
/// reporting a counter it never opened.
#[cfg(not(target_os = "linux"))]
fn stage1(_config: u64, _plan: &MeasurementPlan) -> Result<Stage1Outcome, Stage1Error> {
    Err(Stage1Error::WrongPlatform {
        target: std::env::consts::OS,
    })
}

/// The core stage 1 measures on: the one this process is pinned to.
#[cfg(target_os = "linux")]
fn measurement_core() -> Result<usize, Stage1Error> {
    use cpu_qualification::perf_sys::{allowed_core_count, current_core};
    let allowed = allowed_core_count().map_err(|e| Stage1Error::Read {
        what: "the thread's CPU affinity".to_string(),
        detail: e.to_string(),
    })?;
    if allowed != 1 {
        return Err(Stage1Error::Unavailable {
            what: "the measurement core".to_string(),
            detail: format!(
                "this process may run on {allowed} cores; run it pinned to one, so the \
                 counter measures a core rather than whichever one the scheduler picked"
            ),
        });
    }
    usize::try_from(current_core()).map_err(|e| Stage1Error::Read {
        what: "the current core".to_string(),
        detail: e.to_string(),
    })
}

#[cfg(not(target_os = "linux"))]
fn measurement_core() -> Result<usize, Stage1Error> {
    Err(Stage1Error::WrongPlatform {
        target: std::env::consts::OS,
    })
}

/// Write one stage's records as a JSON-lines stream.
fn write_records(
    evidence_dir: &Path,
    stage: u8,
    records: &[Record],
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = evidence_dir.join(format!("stage{stage}.jsonl"));
    let mut text = String::new();
    for record in records {
        text.push_str(&record_line(record)?);
        text.push('\n');
    }
    std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

/// Stage 0, its records written to `evidence_dir`. Returns the run's code.
fn run_stage0(
    pack: &Pack,
    baseline: &str,
    evidence_dir: &Path,
) -> Result<i32, Box<dyn std::error::Error>> {
    // Stage 0 measures nothing that carries a floor, so its plan commits to
    // none. Writing the plan first keeps the recomputation honest about what the
    // run set out to do.
    let mut records = vec![Record::Plan {
        baseline: baseline.to_string(),
        stage: 0,
        floors: Floors {
            min_clean_reps: 0,
            min_overflow_arms: 0,
            skid_margin: 0,
        },
    }];
    let outcome = stage0(pack);
    let rc = match &outcome {
        Ok(outcome) => {
            records.extend(outcome.to_records());
            i32::from(!outcome.deviations().is_empty()) * 2
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    };
    records.push(Record::End { stage: 0, rc });
    let path = write_records(evidence_dir, 0, &records)?;
    if let Ok(outcome) = &outcome {
        print_rows(outcome);
    }
    println!("stage 0 records written to {}", path.display());
    Ok(rc)
}

/// Stage 1, its records written to `evidence_dir`. Returns the run's code.
fn run_stage1(
    pack: &Pack,
    baseline: &str,
    evidence_dir: &Path,
) -> Result<i32, Box<dyn std::error::Error>> {
    let config = pack.work_clock.config()?;
    // A skid margin the pack does not record is a margin nothing can be judged
    // against, so the run refuses rather than choosing one.
    let skid_margin = *pack.skid.margin.require("skid.margin")?;
    // The pack states the simultaneous-multithreading policy this baseline
    // requires; a baseline measured with it off has no sibling thread, and the
    // plan says so before the run rather than after it.
    let smt_off = pack.host_conditions.value().is_some_and(|conditions| {
        conditions
            .iter()
            .any(|c| c.condition == "smt-policy" && c.expect == "off")
    });
    let plan = MeasurementPlan::for_baseline(measurement_core()?, smt_off);
    let payloads = cpu_qualification::payload::runnable().len() as u64;

    let mut records = vec![Record::Plan {
        baseline: baseline.to_string(),
        stage: 1,
        floors: Floors {
            min_clean_reps: plan.clean_reps_floor(),
            min_overflow_arms: plan.overflow_floor(payloads),
            skid_margin,
        },
    }];
    let outcome = stage1(config, &plan);
    let rc = match &outcome {
        Ok(outcome) => {
            records.extend(outcome.records.iter().cloned());
            // A stage that did not make every measurement it is specified to
            // make has not run, however well the measurements it did make came
            // out.
            i32::from(!outcome.unmeasured.is_empty())
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    };
    records.push(Record::End { stage: 1, rc });
    let path = write_records(evidence_dir, 1, &records)?;
    println!("stage 1 records written to {}", path.display());

    match outcome {
        Ok(outcome) => {
            println!("skid: {}", outcome.skid.summary());
            let (margin, derivation) = derive_margin(outcome.skid.max);
            println!("derived skid margin: {margin} ({derivation})");
            for missing in &outcome.unmeasured {
                eprintln!("not measured: {missing}");
            }
            Ok(rc)
        }
        Err(e) => Err(Box::new(e)),
    }
}

fn cmd_run(
    stage: u8,
    baseline: &str,
    evidence_dir: &Path,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    if stage > 1 {
        return Err(format!("stage {stage} is not built in this crate; stages 0 and 1 are").into());
    }
    let pack = Pack::builtin(baseline)?;
    std::fs::create_dir_all(evidence_dir).map_err(|e| {
        format!(
            "cannot create evidence directory {}: {e}",
            evidence_dir.display()
        )
    })?;

    let mut rc = run_stage0(&pack, baseline, evidence_dir)?;
    if stage >= 1 {
        // Stage 1 runs on a host stage 0 confirmed. Measuring a counter on a
        // host whose standing conditions are unknown measures the conditions,
        // not the counter.
        if rc != 0 {
            return Err(
                "stage 0 did not confirm this host, so stage 1 would measure the host \
                 rather than the counter"
                    .into(),
            );
        }
        rc = run_stage1(&pack, baseline, evidence_dir)?;
    }
    Ok(if rc == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

fn run(cli: Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    match cli.cmd {
        Cmd::Run {
            stage,
            baseline,
            evidence_dir,
        } => cmd_run(stage, &baseline, &evidence_dir),
        Cmd::Check { baseline } => cmd_check(&baseline),
        Cmd::Report { evidence_dir } => cmd_report(&evidence_dir),
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
