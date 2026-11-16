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
    plan::SmbRoute,
    shrink::{reaching, until_terminal, with_level_end_waits},
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
    window: Option<usize>,
    ordinary_length: Option<u8>,
    job_budget: Option<u64>,
    work_budget: Option<u64>,
    memory_mib: Option<usize>,
    stream: Option<PathBuf>,
    progress_every: u64,
    terminal: SmbTerminalPredicate,
    root_at: Option<SmbTerminalPredicate>,
    route: SmbRoute,
}

fn parse() -> Result<Arguments, Box<dyn Error>> {
    let mut arguments = Arguments {
        input: PathBuf::new(),
        out: PathBuf::new(),
        seed: 1,
        workers: 1,
        window: None,
        ordinary_length: Some(64),
        job_budget: None,
        work_budget: None,
        memory_mib: Some(8_192),
        stream: None,
        progress_every: 16,
        terminal: SmbTerminalPredicate::GameVictory,
        root_at: None,
        route: SmbRoute::AllLevels,
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
            "--window" => arguments.window = Some(value.parse()?),
            "--ordinary-length" => arguments.ordinary_length = Some(value.parse()?),
            "--job-budget" => arguments.job_budget = Some(value.parse()?),
            "--work-budget" => arguments.work_budget = Some(value.parse()?),
            "--memory-mib" => arguments.memory_mib = Some(value.parse()?),
            "--stream" => arguments.stream = Some(PathBuf::from(value)),
            "--progress-every" => arguments.progress_every = value.parse()?,
            "--terminal" => arguments.terminal = SmbTerminalPredicate::from_identifier(&value)?,
            "--route" => arguments.route = SmbRoute::from_identifier(&value)?,
            "--root-at" => {
                arguments.root_at = Some(SmbTerminalPredicate::from_identifier(&value)?);
            }
            _ => return Err(format!("unknown flag {flag}").into()),
        }
    }
    if arguments.input.as_os_str().is_empty() || arguments.out.as_os_str().is_empty() {
        return Err("usage: smb-shrink --input <tape.json> --out <dir> [--seed N] [--workers N] [--window N] [--ordinary-length N] [--job-budget N] [--work-budget N] [--memory-mib N] [--stream PATH] [--progress-every N] [--terminal game_victory|control_in:W,L] [--root-at control_in:W,L] [--route IDENTIFIER]".into());
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
    let input: SmbInput = serde_json::from_slice(&fs::read(&arguments.input)?)?;
    let mut target = SmbGame::from_environment(&rom)?.new_target()?;
    let canonical = with_level_end_waits(&mut target, &input)?;
    let tape = reaching(&mut target, &canonical, arguments.terminal)?.unwrap_or(canonical);
    let root = arguments
        .root_at
        .map(|root_at| until_terminal(&mut target, &tape, root_at))
        .transpose()?
        .unwrap_or_default();
    drop(target);
    let game = SmbGame::from_environment(&rom)?.with_route(arguments.route);
    let boot_frames = game.new_target()?.boot_frames();
    let run = SmbCampaignRun {
        vocabulary: SmbButtonVocabulary::default(),
        terminal: Some(arguments.terminal),
    };
    write_json(arguments.out.join("root-input.json"), &root)?;
    write_json(arguments.out.join("incumbent-input.json"), &tape)?;
    let mut config = ShrinkConfig::new(run, arguments.seed);
    config.root = root.actions.len();
    config.workers = arguments.workers;
    config.window = arguments.window;
    config.ordinary_length = arguments.ordinary_length;
    config.job_budget = arguments.job_budget;
    config.work_budget = arguments.work_budget;
    config.memory_budget_mib = arguments.memory_mib;
    config.stream = arguments.stream.clone();
    let started = Instant::now();
    let mut progress = BufWriter::new(File::create(arguments.out.join("progress.jsonl"))?);
    let mut improvements = BufWriter::new(File::create(arguments.out.join("improvements.jsonl"))?);
    let every = arguments.progress_every.max(1);
    let out = arguments.out.clone();
    let mut written = 0;
    let outcome = shrink(&game, &config, &tape, |event| {
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
            ShrinkEvent::Progress { progress: state } => {
                let jobs = state.jobs.total();
                if jobs >= written + every {
                    written = jobs;
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
            "terminal": arguments.terminal.identifier(),
            "root_at": arguments.root_at.map(SmbTerminalPredicate::identifier),
            "root_actions": root.actions.len(),
            "route": arguments.route.identifier(),
            "initial_frames_from_power_on": outcome.progress.initial_cost.map(|cost| boot_frames + cost.primary),
            "frames_from_power_on": outcome.progress.cost.map(|cost| boot_frames + cost.primary),
            "stop": outcome.stop,
            "progress": outcome.progress,
            "improvements": outcome.improvements,
        }),
    )?;
    Ok(())
}
