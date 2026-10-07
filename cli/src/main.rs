// SPDX-License-Identifier: AGPL-3.0-or-later
mod adapters;
mod config;
mod host;
mod oci;
mod runners;
mod runs;
mod runtime;
mod selection;
mod workflow;
use adapters::{Operation, Request};
use clap::{Parser, Subcommand};
use config::{Budget, Execution, Result, Source};
use runs::Destination;
use selection::{Point, Selection};
use std::{path::PathBuf, process::ExitCode};
#[derive(Parser)]
#[command(name="harmony", version=runtime::RELEASE, about="Prepare workloads, explore failures, and investigate their histories")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    #[command(about = "Create a workload recipe; infer language when unambiguous")]
    Init {
        input: Option<String>,
        #[arg(long, default_value = "harmony.toml")]
        config: PathBuf,
        #[arg(long)]
        package: Option<String>,
        #[arg(long)]
        language: Option<String>,
    },
    #[command(about = "Check runner availability and provision missing runtime artifacts")]
    Check {
        #[command(flatten)]
        source: Execution,
        #[arg(long)]
        offline: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Build and validate the configured workload")]
    Prepare {
        #[command(flatten)]
        source: Source,
        #[arg(long)]
        offline: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Explore a workload, continue a search, or start from a branch")]
    Search {
        #[command(flatten)]
        source: Execution,
        #[command(flatten)]
        budget: Budget,
        #[command(flatten)]
        destination: Destination,
        #[arg(
            long,
            value_name = "BRANCH",
            help = "Start a new search from a saved branch"
        )]
        from: Option<String>,
        #[arg(
            long,
            value_name = "SEARCH",
            conflicts_with = "from",
            help = "Continue a search with additional budget"
        )]
        resume: Option<String>,
        #[arg(long)]
        offline: bool,
    },
    #[command(
        about = "Branch from a recorded point; optionally execute guest commands or open a shell"
    )]
    Branch {
        #[command(flatten)]
        selection: Selection,
        #[command(flatten)]
        point: Point,
        #[command(flatten)]
        destination: Destination,
        #[arg(long, value_name="COMMAND", help="Run shell text inside the guest", conflicts_with_all = ["exec_file", "shell"])]
        exec: Option<String>,
        #[arg(long, value_name="LOCAL_SCRIPT", help="Save a host script and execute it inside the guest", conflicts_with_all = ["exec", "shell"])]
        exec_file: Option<PathBuf>,
        #[arg(long, help="Open a guest terminal; exit to save state (implies --stop)", conflicts_with_all = ["exec", "exec_file"])]
        shell: bool,
        #[arg(long)]
        actions: Option<PathBuf>,
        #[arg(long, help = "Save the branch without executing the original suffix")]
        stop: bool,
    },
    #[command(about = "List saved searches and branches")]
    List {
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Examine a search, finding or branch")]
    Show {
        #[command(flatten)]
        selection: Selection,
        #[command(flatten)]
        point: Point,
        #[arg(long, conflicts_with = "timeline")]
        logs: bool,
        #[arg(long, conflicts_with = "logs")]
        timeline: bool,
        #[arg(long, requires = "logs")]
        contains: Option<String>,
        #[arg(long)]
        json: bool,
    },
    #[command(hide = true)]
    Debug {
        #[command(subcommand)]
        command: DebugCommand,
    },
    #[command(about = "Compare configuration, artifacts and observed outcomes")]
    Diff { left: String, right: String },
    #[command(hide = true)]
    SessionWorker,
}
#[derive(Subcommand)]
enum DebugCommand {
    Run {
        #[command(flatten)]
        source: Execution,
        #[command(flatten)]
        destination: Destination,
        #[arg(long)]
        seed: Option<u64>,
        #[arg(long = "for", value_parser=config::duration)]
        wall_seconds: Option<u64>,
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

    Replay {
        #[command(flatten)]
        selection: Selection,
        #[command(flatten)]
        destination: Destination,
        #[arg(long, default_value_t = 1)]
        repeat: u32,
    },
}
fn execute(command: Command) -> Result<u8> {
    let request = match command {
        Command::Init {
            input,
            config,
            package,
            language,
        } => {
            let workload = config::Workload {
                input: input.clone(),
                package: package.unwrap_or_default(),
                ..Default::default()
            };
            adapters::select(&workload)?.init(&config, language, input)?;
            println!("created {}", config.display());
            return Ok(0);
        }
        Command::Check {
            source,
            offline,
            json,
        } => Request {
            operation: Operation::Check,
            config: source.load()?,
            offline,
            json,
            ..Default::default()
        },
        Command::Prepare {
            source,
            offline,
            json,
        } => Request {
            operation: Operation::Prepare,
            config: source.load()?,
            offline,
            json,
            ..Default::default()
        },
        Command::Search {
            source,
            budget,
            destination,
            from,
            resume,
            offline,
        } => {
            if let Some(run) = resume {
                if source.specified() {
                    return Err("search --resume inherits its workload and runner".into());
                }
                return workflow::saved(Request {
                    operation: Operation::Resume,
                    selection: Some(Selection { run, finding: None }),
                    budget,
                    destination,
                    ..Default::default()
                });
            }
            if let Some(run) = from {
                if source.specified() {
                    return Err("search --from inherits its workload and runner; only search budgets and seed may change".into());
                }
                return workflow::saved(Request {
                    operation: Operation::Search,
                    selection: Some(Selection { run, finding: None }),
                    budget,
                    destination,
                    ..Default::default()
                });
            }
            let mut config = source.load()?;
            budget.apply(&mut config)?;
            Request {
                operation: Operation::Search,
                config,
                destination,
                offline,
                ..Default::default()
            }
        }
        Command::Branch {
            selection,
            point,
            destination,
            exec,
            exec_file,
            shell,
            actions,
            stop,
        } => {
            return workflow::saved(Request {
                operation: Operation::Branch,
                selection: Some(selection),
                point,
                destination,
                exec,
                exec_file,
                shell,
                actions,
                stop,
                ..Default::default()
            });
        }
        Command::List { json } => return workflow::list(json),
        Command::Show {
            selection,
            point,
            logs,
            timeline,
            contains,
            json,
        } => {
            if point.specified() && !logs && !timeline {
                return Err("point selectors require --logs or --timeline".into());
            }
            return workflow::saved(Request {
                operation: if logs {
                    Operation::Logs
                } else if timeline {
                    Operation::Timeline
                } else {
                    Operation::Inspect
                },
                selection: Some(selection),
                point,
                contains,
                json,
                ..Default::default()
            });
        }
        Command::Debug { command } => return execute_debug(command),
        Command::Diff { left, right } => return workflow::diff(&left, &right),
        Command::SessionWorker => return adapters::worker(),
    };
    adapters::dispatch(request)
}
fn execute_debug(command: DebugCommand) -> Result<u8> {
    let request = match command {
        DebugCommand::Run {
            source,
            seed,
            wall_seconds,
            destination,
            actions,
            repeat,
            console,
            offline,
            command,
        } => {
            let mut config = source.load()?;
            Budget {
                seed,
                wall_seconds,
                executions: None,
            }
            .apply(&mut config)?;
            Request {
                operation: Operation::Run,
                config,
                destination,
                actions,
                repeat,
                console,
                offline,
                command,
                ..Default::default()
            }
        }
        DebugCommand::Replay {
            selection,
            destination,
            repeat,
        } => {
            return workflow::saved(Request {
                operation: Operation::Replay,
                selection: Some(selection),
                destination,
                repeat,
                ..Default::default()
            });
        }
    };
    adapters::dispatch(request)
}
fn main() -> ExitCode {
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
    fn surface_exposes_search_show_and_branch_without_old_aliases() {
        for args in [
            vec![
                "harmony",
                "branch",
                "baseline",
                "--finding",
                "1",
                "--rewind",
                "10",
                "--exec",
                "cat /proc/locks",
            ],
            vec!["harmony", "branch", "baseline", "--shell"],
            vec!["harmony", "branch", "baseline", "--exec-file", "debug.sh"],
            vec!["harmony", "show", "baseline", "--logs", "--step", "4"],
            vec![
                "harmony",
                "show",
                "baseline",
                "--timeline",
                "--rewind-time",
                "2s",
            ],
            vec![
                "harmony",
                "search",
                "--resume",
                "baseline",
                "--executions",
                "5000",
            ],
            vec!["harmony", "search", "--from", "debugging"],
            vec!["harmony", "list"],
            vec!["harmony", "check"],
        ] {
            Cli::try_parse_from(args).unwrap();
        }
        for old in [
            "doctor", "run", "replay", "resume", "runs", "inspect", "findings", "logs", "timeline",
            "shell",
        ] {
            assert!(Cli::try_parse_from(["harmony", old]).is_err(), "{old}");
        }
        for args in [
            vec!["harmony", "branch", "baseline", "--exec", "true", "--shell"],
            vec!["harmony", "branch", "baseline", "--do", "verbose"],
            vec!["harmony", "branch", "baseline", "--before", "10steps"],
            vec!["harmony", "show", "baseline", "--logs", "--timeline"],
            vec![
                "harmony",
                "search",
                "--from",
                "debugging",
                "--resume",
                "baseline",
            ],
            vec!["harmony", "check", "--executions", "5"],
            vec!["harmony", "search", "game.nes", "--core", "core.so"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }
}
