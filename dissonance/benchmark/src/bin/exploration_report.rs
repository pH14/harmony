// SPDX-License-Identifier: AGPL-3.0-or-later
//! `exploration-report` — read an exploration campaign's discovery-event logs
//! (a JSON array of [`ExplorationLog`]) plus the [`GameManifest`] the campaign
//! ran with, and render the committed exploration report
//! (`SMB-EXPLORATION-REPORT.md`, task 86 gate 3).
//!
//! The campaign driver emits the logs + manifest on the box; this offline tool
//! turns them into the report. It fails loudly on conflicting same-seed trials
//! (a determinism violation), a violated seed floor, or mixed workloads.

use std::path::PathBuf;
use std::process::ExitCode;

use benchmark::{ExplorationLog, ExplorationReport, GameManifest};
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "exploration-report",
    about = "Render the fault-free exploration report (tasks 84/86) from discovery-event logs"
)]
struct Cli {
    /// Path to the JSON array of exploration logs (one per (config, seed) run).
    #[arg(long)]
    logs: PathBuf,
    /// Path to the campaign's GameManifest JSON (records ROM sha256, input
    /// shaping, and the branch budget).
    #[arg(long)]
    manifest: PathBuf,
    /// Where to write the report (default: stdout).
    #[arg(long)]
    out: Option<PathBuf>,
    /// Stopping-rule ε numerator (discovery < num/den).
    #[arg(long, default_value_t = 1)]
    eps_num: u64,
    /// Stopping-rule ε denominator.
    #[arg(long, default_value_t = 1000)]
    eps_den: u64,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let manifest: GameManifest = match std::fs::read_to_string(&cli.manifest)
        .map_err(|e| e.to_string())
        .and_then(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
    {
        Ok(m) => m,
        Err(e) => {
            eprintln!("failed to read manifest {}: {e}", cli.manifest.display());
            return ExitCode::FAILURE;
        }
    };
    let logs: Vec<ExplorationLog> = match std::fs::read_to_string(&cli.logs)
        .map_err(|e| e.to_string())
        .and_then(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
    {
        Ok(l) => l,
        Err(e) => {
            eprintln!("failed to read logs {}: {e}", cli.logs.display());
            return ExitCode::FAILURE;
        }
    };
    let report = match ExplorationReport::compute(&manifest, &logs, (cli.eps_num, cli.eps_den)) {
        Ok(r) => r,
        Err(e) => {
            // Conflicting same-seed trials are a determinism violation — fail
            // loudly, never render a report over non-reproducible data.
            eprintln!("cannot compute report: {e}");
            return ExitCode::FAILURE;
        }
    };
    let md = report.render_markdown();
    match cli.out {
        Some(p) => {
            if let Err(e) = std::fs::write(&p, md) {
                eprintln!("failed to write {}: {e}", p.display());
                return ExitCode::FAILURE;
            }
            eprintln!("wrote {} ({:?})", p.display(), report.verdict);
        }
        None => print!("{md}"),
    }
    ExitCode::SUCCESS
}
