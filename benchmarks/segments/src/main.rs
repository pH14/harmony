// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Parser, Subcommand};
use rand_chacha::{
    ChaCha8Rng,
    rand_core::{RngCore, SeedableRng},
};
use segment_bench::{runner, score, spec};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Parser)]
#[command(
    name = "segment-bench",
    about = "Compare search arms on fixed game segments by work to milestone"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run {
        #[arg(long)]
        specs: PathBuf,
        #[arg(long)]
        arms: PathBuf,
        #[arg(long = "arm", required = true)]
        arm: Vec<String>,
        #[arg(long, default_value = "1-16")]
        seeds: String,
        #[arg(long, default_value_t = 1)]
        jobs: usize,
        #[arg(long)]
        out: PathBuf,
    },
    Score {
        #[arg(long)]
        specs: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        control: String,
    },
    Calibrate {
        #[arg(long)]
        specs: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        control: String,
    },
    Determinism {
        #[arg(long)]
        specs: PathBuf,
        #[arg(long)]
        arms: PathBuf,
        #[arg(long)]
        arm: String,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        #[arg(long)]
        other_workers: Option<u32>,
        #[arg(long)]
        out: PathBuf,
    },
    #[command(hide = true)]
    StubAdapter {
        #[arg(long)]
        factor: f64,
        #[arg(long)]
        stream_key: String,
        #[arg(long, default_value_t = 4_194_304.0)]
        mean: f64,
        request: PathBuf,
        result: PathBuf,
    },
}

fn arm_command<'a>(
    arms: &'a std::collections::BTreeMap<String, runner::Arm>,
    name: &str,
) -> Result<&'a [String], String> {
    arms.get(name)
        .map(|arm| arm.command.as_slice())
        .ok_or_else(|| format!("arm {name} is not in the arms file"))
}

fn run(
    specs: &Path,
    arms: &Path,
    arm_names: &[String],
    seeds: &str,
    jobs: usize,
    out: &Path,
) -> Result<(), String> {
    let loaded = spec::load_all(specs)?;
    let arms = runner::load_arms(arms)?;
    let seeds = runner::parse_seeds(seeds)?;
    let mut tasks = Vec::new();
    for &seed in &seeds {
        for spec in &loaded {
            for arm in arm_names {
                let command = arm_command(&arms, arm)?;
                let dir = runner::run_dir(out, &spec.spec.name, arm, seed);
                tasks.push(runner::Task::new(spec, arm, command, seed, None, dir)?);
            }
        }
    }
    runner::run_all(tasks, jobs).map(|_| ())
}

fn score(specs: &Path, out: &Path, control: &str) -> Result<(), String> {
    let loaded = spec::load_all(specs)?;
    let collected = score::collect(&loaded, out, control)?;
    let verdicts = score::verdicts(&collected.rows, control);
    println!("{}", score::render(&collected, &verdicts, control));
    let value = json!({
        "control": control,
        "rows": collected.rows,
        "verdicts": verdicts,
        "failures": collected.failures,
        "stale": collected.stale,
    });
    fs::write(
        out.join("score.json"),
        serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn calibrate(specs: &Path, out: &Path, control: &str) -> Result<(), String> {
    let loaded = spec::load_all(specs)?;
    let collected = score::collect(&loaded, out, control)?;
    println!(
        "{}",
        score::render_calibration(&score::calibrate(&collected.rows, control))
    );
    Ok(())
}

fn determinism(
    specs: &Path,
    arms: &Path,
    arm: &str,
    seed: u64,
    other: Option<u32>,
    out: &Path,
) -> Result<bool, String> {
    let loaded = spec::load_all(specs)?;
    let [spec] = loaded.as_slice() else {
        return Err("determinism takes one spec file".into());
    };
    let arms = runner::load_arms(arms)?;
    let command = arm_command(&arms, arm)?;
    let first = spec.spec.workers;
    let second = other.unwrap_or(if first > 1 { first / 2 } else { 2 });
    if second == first {
        return Err("the second run needs a different worker count".into());
    }
    let base = out.join("determinism").join(&spec.spec.name).join(arm);
    let tasks = [first, second]
        .into_iter()
        .map(|workers| {
            runner::Task::new(
                spec,
                arm,
                command,
                seed,
                Some(workers),
                base.join(format!("s{seed}-w{workers}")),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    for task in &tasks {
        task.clear()?;
    }
    let dirs: Vec<PathBuf> = tasks.iter().map(|t| t.dir.clone()).collect();
    runner::run_all(tasks, 2)?;
    let outcomes: Vec<runner::Outcome> = dirs
        .iter()
        .map(|dir| {
            runner::read_outcome(dir).ok_or_else(|| format!("{} has no result", dir.display()))
        })
        .collect::<Result<_, _>>()?;
    for (workers, o) in [first, second].iter().zip(&outcomes) {
        println!(
            "workers {workers}: stop {:?} work {} job stream {}",
            o.stop, o.work, o.job_stream_sha256
        );
    }
    let (a, b) = (&outcomes[0], &outcomes[1]);
    let same = a.stop == b.stop
        && a.work == b.work
        && a.job_stream_sha256 == b.job_stream_sha256
        && !a.job_stream_sha256.is_empty()
        && !matches!(
            a.stop,
            runner::Stop::Error | runner::Stop::StartMismatch | runner::Stop::RomMismatch
        );
    println!("determinism: {}", if same { "PASS" } else { "FAIL" });
    Ok(same)
}

fn stub_adapter(
    factor: f64,
    stream_key: &str,
    mean: f64,
    request: &Path,
    result: &Path,
) -> Result<(), String> {
    let request: Value = serde_json::from_slice(&fs::read(request).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let name = request["name"].as_str().ok_or("request has no name")?;
    let seed = request["seed"].as_u64().ok_or("request has no seed")?;
    let budget = request["work_budget"]
        .as_u64()
        .ok_or("request has no work_budget")?;
    let digest: [u8; 32] = Sha256::digest(format!("{name}/{seed}")).into();
    let mut rng = ChaCha8Rng::from_seed(digest);
    let uniform = (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
    let draw = -(1.0 - uniform).ln() * mean * factor;
    let work = draw.ceil().max(1.0) as u64;
    let (stop, work) = if work <= budget {
        ("milestone", work)
    } else {
        ("budget", budget)
    };
    let stream = format!(
        "{:x}",
        Sha256::digest(format!("{stream_key}/{name}/{seed}"))
    );
    let value = json!({"stop": stop, "work": work, "job_stream_sha256": stream, "message": ""});
    fs::write(
        result,
        serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let outcome = match cli.command {
        Commands::Run {
            specs,
            arms,
            arm,
            seeds,
            jobs,
            out,
        } => run(&specs, &arms, &arm, &seeds, jobs, &out).map(|()| true),
        Commands::Score {
            specs,
            out,
            control,
        } => score(&specs, &out, &control).map(|()| true),
        Commands::Calibrate {
            specs,
            out,
            control,
        } => calibrate(&specs, &out, &control).map(|()| true),
        Commands::Determinism {
            specs,
            arms,
            arm,
            seed,
            other_workers,
            out,
        } => determinism(&specs, &arms, &arm, seed, other_workers, &out),
        Commands::StubAdapter {
            factor,
            stream_key,
            mean,
            request,
            result,
        } => stub_adapter(factor, &stream_key, mean, &request, &result).map(|()| true),
    };
    match outcome {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("segment-bench: {error}");
            ExitCode::from(2)
        }
    }
}
