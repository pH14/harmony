// SPDX-License-Identifier: AGPL-3.0-or-later
//! # harmony — the product CLI
//!
//! `harmony preflight` reports which support-matrix cell
//! (docs/DETERMINISM.md §4) the current host occupies and whether the guest
//! artifacts are installed. `harmony oci run` boots an OCI container image
//! inside the deterministic hypervisor and prints the run digest. Search and
//! investigation commands operate on package-owned durable workspaces.
//! Hypervisor verbs fail closed: an unsupported or untested host is named,
//! never silently degraded.

mod host;
mod investigate;
mod preflight;
mod search;

use clap::{Parser, Subcommand};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "harmony",
    version,
    about = "Deterministic hypervisor testing",
    after_help = "A faults search writes a workspace; -w opens it.\n\
                  \n\
                  \x20 harmony search --package faults service.oci --out W --seed 7 --executions 100\n\
                  \x20 harmony -w W findings\n\
                  \x20 harmony -w W inspect bug-1\n\
                  \x20 harmony -w W fork bug-1 --rewind 3s --name trace\n\
                  \x20 harmony -w W run trace --for 4s\n\
                  \x20 harmony -w W export bug-1 --out shared --evidence"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    #[command(flatten)]
    common: investigate::Common,
}

#[derive(Subcommand)]
enum Command {
    /// Explore a workload with its package-owned semantics.
    Search(search::Args),
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
    /// List findings a search recorded, with their properties and verification.
    Findings,
    /// List named continuations and their saved endpoints.
    Branches,
    /// Describe the workspace, a finding, or a retained view of a point.
    Inspect(investigate::InspectArgs),
    /// Create a continuation before a finding and name its starting moment.
    Fork(investigate::ForkArgs),
    /// Advance a branch by a bounded amount of guest time.
    Run(investigate::RunArgs),
    /// Write the recorded reproducer and, separately, investigation evidence.
    Export(investigate::ExportArgs),
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
    let common = cli.common;
    let result = match cli.command {
        Command::Search(args) => search::run(args),
        Command::Preflight { json } => preflight::run(json),
        Command::Oci(OciCommand::Run(args)) => oci::run(args),
        Command::Findings => investigate::run(&common, investigate::Command::Findings),
        Command::Branches => investigate::run(&common, investigate::Command::Branches),
        Command::Inspect(args) => investigate::run(&common, investigate::Command::Inspect(args)),
        Command::Fork(args) => investigate::run(&common, investigate::Command::Fork(args)),
        Command::Run(args) => investigate::run(&common, investigate::Command::Run(args)),
        Command::Export(args) => investigate::run(&common, investigate::Command::Export(args)),
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod search_cli_tests {
    use super::*;
    #[test]
    fn package_search_examples_parse() {
        for args in [
            vec!["harmony", "search", "--package", "nes", "smb.nes"],
            vec![
                "harmony",
                "search",
                "--package",
                "nes",
                "--backend",
                "native",
                "smb.nes",
            ],
            vec![
                "harmony",
                "search",
                "--package",
                "nes",
                "--backend",
                "consonance",
                "smb.nes",
            ],
            vec!["harmony", "search", "--package", "faults", "foo.oci"],
            vec![
                "harmony",
                "search",
                "--package",
                "faults",
                "--backend",
                "consonance",
                "--kernel",
                "vmlinux",
                "--fault-agent",
                "fault-agent",
                "--seed",
                "1",
                "--workers",
                "8",
                "--executions",
                "1000",
                "--actions",
                "12",
                "--horizon-ms",
                "500",
                "--ram-mib",
                "1024",
                "--knobs",
                "service.requests=20",
                "--places",
                "places.txt",
                "--wall-minutes",
                "30",
                "--out",
                "run",
                "foo.oci",
            ],
            vec![
                "harmony",
                "search",
                "--package",
                "faults",
                "--kernel",
                "vmlinux",
                "--replay",
                "run/bug-1.json",
                "--repeat",
                "10",
                "--out",
                "confirm",
                "foo.oci",
            ],
        ] {
            assert!(matches!(
                Cli::try_parse_from(args).unwrap().command,
                Command::Search(_)
            ));
        }
        assert!(Cli::try_parse_from(["harmony", "search", "smb.nes"]).is_err());
        assert!(
            Cli::try_parse_from([
                "harmony",
                "search",
                "--package",
                "nes",
                "--backend",
                "unknown",
                "smb.nes"
            ])
            .is_err()
        );
    }

    #[test]
    fn command_execution_is_not_exposed_until_durable_state_exists() {
        assert!(Cli::try_parse_from(["harmony", "exec", "trace", "--", "true"]).is_err());
    }
}
