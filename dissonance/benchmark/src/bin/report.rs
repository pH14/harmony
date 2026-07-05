// SPDX-License-Identifier: AGPL-3.0-or-later
//! `benchmark-report` — read a campaign discovery-event log (a JSON array of
//! [`CampaignLog`]) and render `CORRELATION-REPORT.md` with the GO/NO-GO ruling.
//!
//! The campaign driver (the conductor / vmm-core campaign bin) emits the log on
//! the box; this offline tool turns it into the committed report (spec gate 3).

use std::path::PathBuf;
use std::process::ExitCode;

use benchmark::report::CorrelationReport;
use benchmark::{Benchmark, CampaignLog};
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "benchmark-report",
    about = "Render CORRELATION-REPORT.md from a campaign discovery-event log"
)]
struct Cli {
    /// Path to the JSON array of campaign logs (one per (config, seed) run).
    #[arg(long)]
    logs: PathBuf,
    /// Where to write the report (default: stdout).
    #[arg(long)]
    out: Option<PathBuf>,
    /// Fixed branch budget for measure 1 (cells discovered at this budget).
    #[arg(long, default_value_t = 256)]
    budget: u64,
    /// Effect-size floor numerator (|ρ| ≥ num/den, negative direction).
    #[arg(long, default_value_t = 3)]
    effect_num: i128,
    /// Effect-size floor denominator.
    #[arg(long, default_value_t = 10)]
    effect_den: i128,
    /// Stopping-rule ε numerator (discovery < num/den).
    #[arg(long, default_value_t = 1)]
    eps_num: u64,
    /// Stopping-rule ε denominator.
    #[arg(long, default_value_t = 1000)]
    eps_den: u64,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let raw = match std::fs::read_to_string(&cli.logs) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("failed to read {}: {e}", cli.logs.display());
            return ExitCode::FAILURE;
        }
    };
    let logs: Vec<CampaignLog> = match serde_json::from_str(&raw) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("failed to parse campaign logs: {e}");
            return ExitCode::FAILURE;
        }
    };
    let report = CorrelationReport::compute(
        &Benchmark::wave5(),
        &logs,
        cli.budget,
        (cli.effect_num, cli.effect_den),
        (cli.eps_num, cli.eps_den),
    );
    let md = report.render_markdown();
    match cli.out {
        Some(p) => {
            if let Err(e) = std::fs::write(&p, md) {
                eprintln!("failed to write {}: {e}", p.display());
                return ExitCode::FAILURE;
            }
            eprintln!("wrote {} ({:?})", p.display(), report.ruling);
        }
        None => print!("{md}"),
    }
    ExitCode::SUCCESS
}
