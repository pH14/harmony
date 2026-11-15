// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    env,
    error::Error,
    fs::{self, File},
    io::{BufWriter, Write},
    path::PathBuf,
    time::Instant,
};

use nes_games::smb::{
    campaign::{SmbButtonVocabulary, SmbCampaignRun, SmbTerminalPredicate},
    shrink::with_level_end_waits,
    target::SmbInput,
};
use nes_workload::{
    search::{
        campaign::TargetExecution,
        shrink::{ShrinkConfig, ShrinkEvent, shrink},
    },
    smb::campaign::SmbGame,
};
use serde_json::json;

struct Arguments {
    input: PathBuf,
    out: PathBuf,
    seed: u64,
    workers: usize,
    batch: usize,
    redraw_length: usize,
    job_budget: Option<u64>,
    work_budget: Option<u64>,
    progress_every: u64,
}

fn parse() -> Result<Arguments, Box<dyn Error>> {
    let mut arguments = Arguments {
        input: PathBuf::new(),
        out: PathBuf::new(),
        seed: 1,
        workers: 1,
        batch: 32,
        redraw_length: 64,
        job_budget: None,
        work_budget: None,
        progress_every: 16,
    };
    let mut values = env::args().skip(1);
    while let Some(flag) = values.next() {
        let value = values
            .next()
            .ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--input" => arguments.input = PathBuf::from(value),
            "--out" => arguments.out = PathBuf::from(value),
            "--seed" => arguments.seed = value.parse()?,
            "--workers" => arguments.workers = value.parse()?,
            "--batch" => arguments.batch = value.parse()?,
            "--redraw-length" => arguments.redraw_length = value.parse()?,
            "--job-budget" => arguments.job_budget = Some(value.parse()?),
            "--work-budget" => arguments.work_budget = Some(value.parse()?),
            "--progress-every" => arguments.progress_every = value.parse()?,
            _ => return Err(format!("unknown flag {flag}").into()),
        }
    }
    if arguments.input.as_os_str().is_empty() || arguments.out.as_os_str().is_empty() {
        return Err("usage: smb-shrink --input <tape.json> --out <dir> [--seed N] [--workers N] [--batch N] [--redraw-length N] [--job-budget N] [--work-budget N]".into());
    }
    Ok(arguments)
}

fn write_json(path: PathBuf, value: &impl serde::Serialize) -> Result<(), Box<dyn Error>> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[allow(clippy::disallowed_methods)]
fn main() -> Result<(), Box<dyn Error>> {
    let arguments = parse()?;
    fs::create_dir_all(&arguments.out)?;
    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_SMB_ROM").ok_or("HARMONY_SMB_ROM must name the ROM")?,
    ))?;
    let game = SmbGame::from_environment(&rom)?;
    let run = SmbCampaignRun {
        vocabulary: SmbButtonVocabulary::default(),
        terminal: Some(SmbTerminalPredicate::GameVictory),
    };
    let input: SmbInput = serde_json::from_slice(&fs::read(&arguments.input)?)?;
    let mut target = game.new_target()?;
    let boot_frames = target.boot_frames();
    let incumbent = with_level_end_waits(&mut target, &input)?;
    drop(target);
    write_json(arguments.out.join("incumbent-input.json"), &incumbent)?;
    let mut config = ShrinkConfig::new(run, arguments.seed);
    config.workers = arguments.workers;
    config.batch = arguments.batch;
    config.redraw_length = arguments.redraw_length;
    config.job_budget = arguments.job_budget;
    config.work_budget = arguments.work_budget;
    let started = Instant::now();
    let mut progress = BufWriter::new(File::create(arguments.out.join("progress.jsonl"))?);
    let mut improvements = BufWriter::new(File::create(arguments.out.join("improvements.jsonl"))?);
    let every = arguments.progress_every.max(1);
    let out = arguments.out.clone();
    let outcome = shrink(&game, &config, &incumbent, |event| {
        match event {
            ShrinkEvent::Improved { input, improvement } => {
                write_json(out.join("best-input.json"), input)?;
                let line = json!({
                    "seconds": started.elapsed().as_secs_f64(),
                    "frames_from_power_on": boot_frames + improvement.cost.primary,
                    "improvement": improvement,
                });
                writeln!(improvements, "{line}")?;
                improvements.flush()?;
            }
            ShrinkEvent::Batch { progress: state } => {
                if state.batches % every == 0 {
                    let line = json!({
                        "seconds": started.elapsed().as_secs_f64(),
                        "progress": state,
                    });
                    writeln!(progress, "{line}")?;
                    progress.flush()?;
                }
            }
        }
        Ok(())
    })?;
    write_json(arguments.out.join("best-input.json"), &outcome.input)?;
    write_json(
        arguments.out.join("outcome.json"),
        &json!({
            "seconds": started.elapsed().as_secs_f64(),
            "boot_frames": boot_frames,
            "initial_frames_from_power_on": boot_frames + outcome.progress.initial_cost.primary,
            "frames_from_power_on": boot_frames + outcome.progress.cost.primary,
            "stop": outcome.stop,
            "progress": outcome.progress,
            "improvements": outcome.improvements,
        }),
    )?;
    Ok(())
}
