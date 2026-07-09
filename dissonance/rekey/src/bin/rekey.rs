// SPDX-License-Identifier: AGPL-3.0-or-later
//! `rekey` — build the corpus manifest, verify the corpus, or score the
//! candidate space and render `REKEY-REPORT.md`.
//!
//! The report is a pure function of the manifest and the pinned corpus bytes:
//! running `rekey score` twice writes byte-identical output, on any host.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// The default corpus root, relative to the repository root.
const DEFAULT_CORPUS: &str = "dissonance/benchmark/campaign-data";
/// The default report path, relative to the repository root.
const DEFAULT_REPORT: &str = "dissonance/benchmark/REKEY-REPORT.md";

#[derive(Parser)]
#[command(
    name = "rekey",
    about = "The E-fails re-key harness: offline CellFn iteration over the retained trace corpus"
)]
struct Cli {
    /// The corpus root (the `campaign-data` directory).
    #[arg(long, default_value = DEFAULT_CORPUS, global = true)]
    corpus: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Rebuild `rekey-corpus.json` by hashing the corpus.
    Manifest {
        /// Write the manifest; otherwise print it and check it against the
        /// committed one.
        #[arg(long)]
        write: bool,
    },
    /// Verify the corpus hashes and run the harness-correctness gates.
    Verify,
    /// Score every candidate and render the report.
    Score {
        /// Where to write `REKEY-REPORT.md`.
        #[arg(long, default_value = DEFAULT_REPORT)]
        out: PathBuf,
        /// Print the report instead of writing it.
        #[arg(long)]
        stdout: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rekey: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    match &cli.command {
        Command::Manifest { write } => {
            let manifest = rekey::manifest::build(&cli.corpus)?;
            let rendered = rekey::manifest::render(&manifest);
            let path = rekey::manifest::manifest_path(&cli.corpus);
            if *write {
                std::fs::write(&path, &rendered)?;
                eprintln!(
                    "wrote {} ({} traces, {} branches, {} excluded)",
                    path.display(),
                    manifest.totals.trace_files,
                    manifest.totals.branches,
                    manifest.totals.excluded_traces
                );
            } else {
                print!("{rendered}");
                if let Ok(committed) = std::fs::read_to_string(&path)
                    && committed != rendered
                {
                    return Err("the committed manifest is stale (rerun with --write)".into());
                }
            }
            Ok(())
        }
        Command::Verify => {
            let analysis = rekey::analyze(&cli.corpus)?;
            eprintln!(
                "corpus {} verified: {} traces / {} branches; control reproduces every recorded \
                 campaign; ancestry reproduces every recorded environment",
                &analysis.manifest_sha256[..16],
                analysis.totals.trace_files,
                analysis.totals.branches
            );
            Ok(())
        }
        Command::Score { out, stdout } => {
            let analysis = rekey::analyze(&cli.corpus)?;
            let markdown = rekey::report::render(&analysis);
            if *stdout {
                print!("{markdown}");
            } else {
                std::fs::write(out, &markdown)?;
                eprintln!("wrote {}", out.display());
            }
            Ok(())
        }
    }
}
