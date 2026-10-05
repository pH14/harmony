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
    Doctor {
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
    #[command(about = "Explore a workload or start a fresh search from a recorded point")]
    Search {
        #[command(flatten)]
        source: Execution,
        #[command(flatten)]
        budget: Budget,
        #[command(flatten)]
        destination: Destination,
        #[arg(long)]
        from: Option<String>,
        #[arg(long, requires = "from")]
        finding: Option<usize>,
        #[command(flatten)]
        point: Point,
        #[arg(long)]
        offline: bool,
    },
    #[command(about = "Continue saved search history with an additional budget")]
    Resume {
        run: String,
        #[command(flatten)]
        budget: Budget,
        #[command(flatten)]
        destination: Destination,
    },
    #[command(about = "Execute a workload once or an explicit recorded input")]
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
    #[command(about = "Verify a saved execution using its recorded artifacts")]
    Replay {
        #[command(flatten)]
        selection: Selection,
        #[command(flatten)]
        destination: Destination,
        #[arg(long, default_value_t = 1)]
        repeat: u32,
    },
    #[command(about = "Apply interventions at an explicit point and execute the remaining inputs")]
    Branch {
        #[command(flatten)]
        selection: Selection,
        #[command(flatten)]
        point: Point,
        #[command(flatten)]
        destination: Destination,
        #[arg(long = "do")]
        interventions: Vec<String>,
        #[arg(long)]
        intervention_toml: Option<String>,
        #[arg(long)]
        actions: Option<PathBuf>,
        #[arg(
            long,
            help = "Save the prefix and interventions without executing the original suffix"
        )]
        stop: bool,
    },
    #[command(about = "List saved runs")]
    Runs {
        #[arg(long)]
        json: bool,
    },
    #[command(about = "List findings in a saved run")]
    Findings {
        run: String,
        #[arg(long)]
        json: bool,
    },
    Inspect {
        #[command(flatten)]
        selection: Selection,
        #[arg(long)]
        json: bool,
    },
    Timeline {
        #[command(flatten)]
        selection: Selection,
        #[command(flatten)]
        point: Point,
        #[arg(long)]
        json: bool,
    },
    Logs {
        #[command(flatten)]
        selection: Selection,
        #[command(flatten)]
        point: Point,
        #[arg(long)]
        contains: Option<String>,
    },
    Diff {
        left: String,
        right: String,
    },
    #[command(hide = true)]
    SessionWorker,
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
        Command::Doctor {
            source,
            offline,
            json,
        } => Request {
            operation: Operation::Doctor,
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
            finding,
            point,
            offline,
        } => {
            if let Some(run) = from {
                if source.specified() {
                    return Err("search --from inherits its workload and runner; only search budgets and seed may change".into());
                }
                return workflow::saved(Request {
                    operation: Operation::Search,
                    selection: Some(Selection { run, finding }),
                    point,
                    budget,
                    destination,
                    ..Default::default()
                });
            }
            if point.specified() {
                return Err("point selectors require --from".into());
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
        Command::Resume {
            run,
            budget,
            destination,
        } => {
            return workflow::saved(Request {
                operation: Operation::Resume,
                selection: Some(Selection { run, finding: None }),
                budget,
                destination,
                ..Default::default()
            });
        }
        Command::Run {
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
        Command::Replay {
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
        Command::Branch {
            selection,
            point,
            destination,
            interventions,
            intervention_toml,
            actions,
            stop,
        } => {
            if !point.specified() {
                return Err("branch requires --step, --rewind, or --rewind-time".into());
            }
            return workflow::saved(Request {
                operation: Operation::Branch,
                selection: Some(selection),
                point,
                destination,
                interventions,
                intervention_toml,
                actions,
                stop,
                ..Default::default()
            });
        }
        Command::Runs { json } => return workflow::list(json),
        Command::Findings { run, json } => {
            return workflow::saved(Request {
                operation: Operation::Findings,
                selection: Some(Selection { run, finding: None }),
                json,
                ..Default::default()
            });
        }
        Command::Inspect { selection, json } => {
            return workflow::saved(Request {
                operation: Operation::Inspect,
                selection: Some(selection),
                json,
                ..Default::default()
            });
        }
        Command::Timeline {
            selection,
            point,
            json,
        } => {
            return workflow::saved(Request {
                operation: Operation::Timeline,
                selection: Some(selection),
                point,
                json,
                ..Default::default()
            });
        }
        Command::Logs {
            selection,
            point,
            contains,
        } => {
            return workflow::saved(Request {
                operation: Operation::Logs,
                selection: Some(selection),
                point,
                contains,
                ..Default::default()
            });
        }
        Command::Diff { left, right } => return workflow::diff(&left, &right),
        Command::SessionWorker => return adapters::worker(),
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
    fn commands_share_plain_point_selectors_and_only_expose_relevant_options() {
        for args in [
            vec![
                "harmony",
                "branch",
                "baseline",
                "--finding",
                "1",
                "--rewind",
                "10",
                "--do",
                "verbose",
            ],
            vec!["harmony", "logs", "baseline", "--step", "4"],
            vec!["harmony", "timeline", "baseline", "--rewind-time", "2s"],
            vec!["harmony", "resume", "baseline", "--executions", "5000"],
            vec![
                "harmony",
                "search",
                "--from",
                "baseline",
                "--finding",
                "1",
                "--rewind",
                "10",
            ],
            vec!["harmony", "runs"],
            vec!["harmony", "findings", "baseline"],
        ] {
            Cli::try_parse_from(args).unwrap();
        }
        for args in [
            vec!["harmony", "branch", "baseline", "--before", "10steps"],
            vec![
                "harmony", "branch", "baseline", "--step", "1", "--rewind", "2",
            ],
            vec!["harmony", "doctor", "--executions", "5"],
            vec!["harmony", "prepare", "--seed", "5"],
            vec!["harmony", "search", "--resume", "baseline"],
            vec!["harmony", "replay", "baseline", "--bug", "1"],
            vec!["harmony", "search", "game.nes", "--core", "core.so"],
            vec!["harmony", "search", "game.nes", "--nes-image", "guest.oci"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }
}
