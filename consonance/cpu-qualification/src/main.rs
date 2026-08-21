// SPDX-License-Identifier: AGPL-3.0-or-later
//! `cpu-qualification` CLI.
//!
//! Exit codes: `0` when every check passed, `2` when a check failed (the suite is
//! a detector, not a failure), `1` on an operational error — a missing evidence
//! directory, an unreadable pack, a stage that cannot run here.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use cpu_qualification::report::{Record, Verdict, parse_records, recompute};

#[derive(Parser)]
#[command(name = "cpu-qualification", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
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

fn run(cli: Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    match cli.cmd {
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
