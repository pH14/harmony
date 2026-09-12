// SPDX-License-Identifier: AGPL-3.0-or-later

use clap::{Parser, Subcommand};
use serde::Serialize;
use std::process::ExitCode;
use unison::flaky::{FlakyFactory, Perturbation};
use unison::toy::{ToyFactory, generate_program};
use unison::{CompareReport, DivergencePoint, Verdict, bisect_divergence, compare_runs};

const CLI_PERTURB: Perturbation = Perturbation::XorPrng {
    mask: 0x5EED_5EED_5EED_5EED,
};

#[derive(Parser)]
#[command(name = "unison", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    ToyCompare {
        #[arg(long)]
        seed: u64,
        #[arg(long)]
        diverge_at: u64,
        #[arg(long)]
        checkpoint_every: u64,
        #[arg(long)]
        limit: u64,
        #[arg(long, default_value_t = 0)]
        program_seed: u64,
        #[arg(long, default_value_t = 10_000)]
        min_work: u64,
    },
    ToyBisect {
        #[arg(long)]
        seed: u64,
        #[arg(long)]
        diverge_at: u64,
        #[arg(long)]
        limit: u64,
        #[arg(long, default_value_t = 0)]
        program_seed: u64,
        #[arg(long, default_value_t = 10_000)]
        min_work: u64,
    },
}

#[derive(Serialize)]
struct BisectOutput {
    compare: CompareReport,
    point: Option<DivergencePoint>,
}

fn factories(
    program_seed: u64,
    min_work: u64,
    diverge_at: u64,
) -> (ToyFactory, FlakyFactory<ToyFactory>) {
    let prog = generate_program(program_seed, min_work);
    let toy = ToyFactory {
        program: prog.instrs.clone(),
    };
    let flaky = FlakyFactory {
        inner: ToyFactory {
            program: prog.instrs,
        },
        diverge_at,
        perturb: CLI_PERTURB,
    };
    (toy, flaky)
}

fn exit_for_verdict(v: &Verdict) -> ExitCode {
    match v {
        Verdict::Identical => ExitCode::SUCCESS,
        Verdict::Diverged { .. } | Verdict::HaltMismatch { .. } => ExitCode::from(2),
    }
}

fn print_json<T: Serialize>(value: &T) -> Result<(), serde_json::Error> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn run(cli: Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    match cli.cmd {
        Cmd::ToyCompare {
            seed,
            diverge_at,
            checkpoint_every,
            limit,
            program_seed,
            min_work,
        } => {
            let (toy, flaky) = factories(program_seed, min_work, diverge_at);
            let report = compare_runs(&toy, &flaky, seed, checkpoint_every, limit)?;
            print_json(&report)?;
            Ok(exit_for_verdict(&report.verdict))
        }
        Cmd::ToyBisect {
            seed,
            diverge_at,
            limit,
            program_seed,
            min_work,
        } => {
            let (toy, flaky) = factories(program_seed, min_work, diverge_at);
            let checkpoint_every = (limit / 16).max(1);
            let compare = compare_runs(&toy, &flaky, seed, checkpoint_every, limit)?;
            let point = match compare.verdict {
                Verdict::Diverged {
                    last_match,
                    first_mismatch,
                } => Some(bisect_divergence(
                    &toy,
                    &flaky,
                    seed,
                    last_match.unwrap_or(0),
                    first_mismatch,
                )?),
                _ => None,
            };
            let code = exit_for_verdict(&compare.verdict);
            print_json(&BisectOutput { compare, point })?;
            Ok(code)
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
