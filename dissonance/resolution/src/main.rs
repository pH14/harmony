// SPDX-License-Identifier: AGPL-3.0-or-later
//! `resolution` — the moment-addressed REPL over the in-crate mock server.
//!
//! Two modes, one renderer (`docs/RESOLUTION.md` §"The human layer"):
//!
//! - **Replay** — `resolution --transcript <file>`: re-render a recorded JSONL
//!   transcript identically and exit (the spec's documented replay form, task 82
//!   §The transcript). The file is **read-only** — it is never written back, so
//!   the recorded investigation can never be truncated by a replay.
//! - **Live** (default): read commands from stdin, one per line, driving a
//!   [`Shell`] over a scripted [`MockServer`]. Each command prints its human
//!   rendering; `--record <file>` (a distinct flag from the replay input) writes
//!   the JSONL record stream on exit.
//!
//! Both modes render through the same [`render_transcript`] / [`render_line`]
//! path (task 29's one renderer). The backend is the mock (`docs/RESOLUTION.md`:
//! v1 gates against an in-crate mock; the live box connection is the foreman's
//! box gate). The line protocol is designed to be wrapped by an agent harness,
//! not replaced.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use environment::{EnvCodec, FaultPolicy};
use resolution::{
    DispatchOutput, MockServer, Session, Shell, from_jsonl, render_line, render_transcript,
    to_jsonl,
};

/// The moment-addressed session REPL + transcript replayer.
#[derive(Parser, Debug)]
#[command(name = "resolution", version, about)]
struct Cli {
    /// Boot seed for the mock server's genesis environment.
    #[arg(long, default_value_t = 0)]
    seed: u64,

    /// Scripted guest RAM size in bytes (the `read` range ceiling).
    #[arg(long, default_value_t = resolution::DEFAULT_RAM_BYTES)]
    ram: u64,

    /// Re-render a recorded JSONL transcript and exit (the spec's replay mode).
    /// Read-only — never written to, so replaying can't truncate the recording.
    #[arg(long)]
    transcript: Option<PathBuf>,

    /// Write the live session's JSONL transcript here on exit (live mode).
    /// Distinct from `--transcript` so a replay input is never overwritten.
    #[arg(long)]
    record: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("resolution: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<(), String> {
    // Replay mode: re-render the recorded transcript through the one renderer.
    // The transcript file is read-only here — never written back.
    if let Some(path) = &cli.transcript {
        if cli.record.is_some() {
            return Err(
                "--transcript (replay) and --record (live) are mutually exclusive".to_string(),
            );
        }
        let text = fs::read_to_string(path).map_err(|e| format!("reading {path:?}: {e}"))?;
        let records = from_jsonl(&text).map_err(|e| format!("parsing {path:?}: {e}"))?;
        print!("{}", render_transcript(&records));
        return Ok(());
    }

    // Live mode: a scripted mock guest driven from stdin.
    let boot_env = EnvCodec::seeded(cli.seed, FaultPolicy::none());
    let server = MockServer::boot_with_ram(boot_env, cli.ram);
    let session = Session::connect(server).map_err(|e| format!("connect: {e}"))?;
    let mut shell = Shell::new(session);

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| format!("reading stdin: {e}"))?;
        // Blank lines and `#` comments are ignored (scriptable shell).
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        match shell.execute_line(&line) {
            DispatchOutput::Recorded(record) => {
                let _ = writeln!(out, "{}", render_line(&record));
            }
            DispatchOutput::View(view) => {
                let _ = write!(out, "{view}");
            }
        }
    }
    let _ = out.flush();

    if let Some(path) = &cli.record {
        let jsonl =
            to_jsonl(shell.records()).map_err(|e| format!("serializing transcript: {e}"))?;
        fs::write(path, jsonl).map_err(|e| format!("writing {path:?}: {e}"))?;
    }
    Ok(())
}
