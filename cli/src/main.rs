// SPDX-License-Identifier: AGPL-3.0-or-later
//! # harmony — the product CLI
//!
//! One binary, two verbs. `harmony preflight` reports which support-matrix
//! cell (docs/DETERMINISM.md §4) the current host occupies and whether the
//! guest artifacts are installed. `harmony oci run` boots an OCI container
//! image inside the deterministic hypervisor and prints the run digest.
//! Hypervisor verbs fail closed: an unsupported or untested host is named,
//! never silently degraded.

mod host;
mod preflight;

use clap::{Parser, Subcommand};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "harmony", version, about = "Deterministic hypervisor testing")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Report host capabilities: support-matrix cell, hypervisor
    /// availability, and installed guest artifacts.
    Preflight {
        /// Emit the report as JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Run OCI container workloads deterministically.
    #[command(subcommand)]
    Oci(OciCommand),
}

#[derive(Subcommand)]
enum OciCommand {
    /// Boot an OCI image in the deterministic hypervisor and run it to
    /// completion. Prints the run digest; identical seed + image ⇒
    /// identical digest.
    Run(oci::RunArgs),
}

mod oci;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Preflight { json } => preflight::run(json),
        Command::Oci(OciCommand::Run(args)) => oci::run(args),
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
