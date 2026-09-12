// SPDX-License-Identifier: AGPL-3.0-or-later

mod host;
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
    Preflight {
        #[arg(long)]
        json: bool,
    },
    #[command(subcommand)]
    Oci(OciCommand),
}

#[derive(Subcommand)]
enum OciCommand {
    Run(oci::RunArgs),
}

mod oci;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Search(args) => search::run(args),
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
                "faultlab.puts=20",
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
}
