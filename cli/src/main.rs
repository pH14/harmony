// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(feature = "hardware")]
mod host;
#[cfg(feature = "hardware")]
mod preflight;
mod search;

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
    Search(search::Args),
    #[cfg(feature = "hardware")]
    Preflight {
        #[arg(long)]
        json: bool,
    },
    #[cfg(feature = "hardware")]
    #[command(subcommand)]
    Oci(OciCommand),
    #[cfg(feature = "hardware")]
    #[command(hide = true)]
    SessionWorker,
}

#[cfg(feature = "hardware")]
#[derive(Subcommand)]
enum OciCommand {
    Run(oci::RunArgs),
}

#[cfg(feature = "hardware")]
mod oci;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Search(args) => {
            nes_workload::allocator::use_one_malloc_arena();
            search::run(args)
        }
        #[cfg(feature = "hardware")]
        Command::Preflight { json } => preflight::run(json),
        #[cfg(feature = "hardware")]
        Command::Oci(OciCommand::Run(args)) => oci::run(args),
        #[cfg(feature = "hardware")]
        Command::SessionWorker => {
            nes_workload::allocator::use_one_malloc_arena();
            search::serve_session_worker()
        }
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(all(test, feature = "hardware"))]
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
                "--seed",
                "1",
                "--executions",
                "1000",
                "--ram-mib",
                "1024",
                "--knobs",
                "faultlab.puts=20",
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
        for removed in ["--workers", "--snapshot-cache-mib"] {
            assert!(
                Cli::try_parse_from([
                    "harmony",
                    "search",
                    "--package",
                    "faults",
                    removed,
                    "4",
                    "foo.oci"
                ])
                .is_err()
            );
        }
    }
}

#[cfg(all(test, feature = "wasm"))]
mod wasm_cli_tests {
    use super::*;
    #[test]
    fn wasm_selection_requires_its_package_before_reading_input() {
        let cli = Cli::try_parse_from([
            "harmony",
            "search",
            "missing.nes",
            "--package",
            "nes",
            "--backend",
            "wasm",
        ])
        .unwrap();
        #[cfg(feature = "hardware")]
        let Command::Search(args) = cli.command else {
            panic!("search command");
        };
        #[cfg(not(feature = "hardware"))]
        let Command::Search(args) = cli.command;
        assert_eq!(
            search::run(args).unwrap_err().to_string(),
            "WASM NES search requires --wasm-package"
        );
        assert!(
            Cli::try_parse_from([
                "harmony",
                "search",
                "missing.nes",
                "--package",
                "nes",
                "--backend",
                "wasm",
                "--wasm-package",
                "package"
            ])
            .is_ok()
        );
    }
}
