// SPDX-License-Identifier: AGPL-3.0-or-later

mod config;
mod host;
mod investigate;
mod oci;
mod prepare;
mod runs;
mod runtime;
mod workflow;

use clap::{Parser, Subcommand};
use config::{Language, Source};
use runs::Destination;
use std::{path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(
    name = "harmony",
    version = runtime::RELEASE,
    about = "Prepare applications, explore failures, and investigate their histories"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Create a TOML application recipe")]
    Init {
        #[arg(long, default_value = "harmony.toml")]
        config: PathBuf,
        #[arg(long, value_enum)]
        language: Option<Language>,
        #[arg(long)]
        image: Option<String>,
    },
    #[command(about = "Check backend, fetch runtime artifacts, and inspect image admission")]
    Doctor {
        #[command(flatten)]
        source: Source,
        #[arg(long)]
        offline: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Build and instrument an application or inspect an existing image")]
    Prepare {
        #[command(flatten)]
        source: Source,
        #[arg(long)]
        offline: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Explore failures, continue a search, or search from a recorded point")]
    Search {
        #[command(flatten)]
        source: Source,
        #[command(flatten)]
        destination: Destination,
        #[arg(long, conflicts_with = "resume")]
        from: Option<String>,
        #[arg(long, conflicts_with = "from")]
        resume: Option<String>,
        #[arg(long, requires = "from")]
        bug: Option<usize>,
        #[arg(long)]
        offline: bool,
    },
    #[command(about = "Run a command or an explicit supervised action sequence")]
    Run {
        #[command(flatten)]
        source: Source,
        #[command(flatten)]
        destination: Destination,
        #[arg(long)]
        actions: Option<PathBuf>,
        #[arg(long, default_value_t = 1)]
        repeat: u32,
        #[arg(long)]
        console: bool,
        #[arg(long)]
        offline: bool,
        #[arg(last = true)]
        command: Vec<String>,
    },
    #[command(about = "Verify a saved execution in a fresh guest")]
    Replay {
        #[command(flatten)]
        selection: investigate::Selection,
        #[command(flatten)]
        destination: Destination,
        #[arg(long, default_value_t = 1)]
        repeat: u32,
    },
    #[command(
        about = "Rewind a saved input, inject interventions, and optionally follow its suffix"
    )]
    Branch {
        #[command(flatten)]
        selection: investigate::Selection,
        #[command(flatten)]
        destination: Destination,
        #[arg(long, conflicts_with = "before")]
        at_step: Option<usize>,
        #[arg(long, conflicts_with = "at_step")]
        before: Option<String>,
        #[arg(long)]
        inject: Vec<String>,
        #[arg(long)]
        actions: Option<PathBuf>,
        #[arg(long)]
        follow: bool,
    },
    #[command(about = "Summarize a saved run or finding")]
    Inspect {
        #[command(flatten)]
        selection: investigate::Selection,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Show recorded actions, virtual time, and assertions")]
    Timeline {
        #[command(flatten)]
        selection: investigate::Selection,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Read console evidence from a saved execution")]
    Logs {
        #[command(flatten)]
        selection: investigate::Selection,
        #[arg(long)]
        at_step: Option<u64>,
        #[arg(long)]
        contains: Option<String>,
    },
    #[command(about = "Compare configurations, actions, and outcomes")]
    Diff { left: String, right: String },
    #[command(hide = true)]
    SessionWorker,
}

fn execute(command: Command) -> config::Result<u8> {
    match command {
        Command::Init {
            config,
            language,
            image,
        } => {
            prepare::init(&config, language, image)?;
            println!("created {}", config.display());
            Ok(0)
        }
        Command::Doctor {
            source,
            offline,
            json,
        } => workflow::doctor(source.load()?, offline, json),
        Command::Prepare {
            source,
            offline,
            json,
        } => {
            let report = prepare::run(&mut source.load()?, offline)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "prepared {}\nnext: harmony doctor, then harmony search",
                    report["image"]
                );
            }
            Ok(0)
        }
        Command::Search {
            source,
            destination,
            from,
            resume,
            bug,
            offline,
        } => workflow::search(source, destination, from, resume, bug, offline),
        Command::Run {
            source,
            destination,
            actions,
            repeat,
            console,
            offline,
            command,
        } => workflow::run(
            source,
            destination,
            actions,
            repeat,
            console,
            offline,
            command,
        ),
        Command::Replay {
            selection,
            destination,
            repeat,
        } => investigate::replay(selection, destination, repeat),
        Command::Branch {
            selection,
            destination,
            at_step,
            before,
            inject,
            actions,
            follow,
        } => investigate::branch(
            selection,
            destination,
            at_step,
            before,
            inject,
            actions,
            follow,
        ),
        Command::Inspect { selection, json } => investigate::inspect(selection, json),
        Command::Timeline { selection, json } => investigate::timeline(selection, json),
        Command::Logs {
            selection,
            at_step,
            contains,
        } => investigate::logs(selection, at_step, contains),
        Command::Diff { left, right } => investigate::diff(&left, &right),
        Command::SessionWorker => {
            use faults_workload::consonance::{SESSION_SERVICE, service_factory};
            consonance_client::session::serve_inherited(|service| {
                (service == SESSION_SERVICE).then(service_factory)
            })?;
            Ok(0)
        }
    }
}

fn main() -> ExitCode {
    nes_workload::allocator::use_one_malloc_arena();
    match execute(Cli::parse().command) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_commands_use_inputs_configurations_and_named_runs() {
        for args in [
            vec!["harmony", "search", "game.nes"],
            vec!["harmony", "search", "image:tag"],
            vec![
                "harmony",
                "search",
                "--config-toml",
                "image='app'",
                "--for",
                "30s",
            ],
            vec!["harmony", "replay", "overnight", "--bug", "1"],
            vec![
                "harmony",
                "branch",
                "overnight",
                "--bug",
                "1",
                "--before",
                "5steps",
            ],
            vec!["harmony", "run", "alpine:3", "--", "/bin/echo", "hello"],
        ] {
            Cli::try_parse_from(args).unwrap();
        }
        for args in [
            vec!["harmony", "preflight"],
            vec!["harmony", "oci", "run", "x"],
            vec!["harmony", "search", "--package", "faults", "x"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }
}
