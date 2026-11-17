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
    shrink::{until_terminal, with_level_end_waits},
    target::SmbInput,
};
use nes_workload::{
    search::{
        campaign::TargetExecution,
        shrink::window::{WindowConfig, WindowEvent, search_window},
    },
    smb::campaign::SmbGame,
};
use serde_json::json;

struct Arguments {
    input: PathBuf,
    out: PathBuf,
    window: usize,
    expansion_budget: Option<u64>,
    frontier_limit: usize,
    progress_every: u64,
    terminal: SmbTerminalPredicate,
    root_at: Option<SmbTerminalPredicate>,
    route: SmbRoute,
}

const USAGE: &str = "usage: smb-window --input <tape.json> --out <dir> --window <actions> [--expansion-budget N] [--frontier-limit N] [--terminal game_victory|control_in:W,L] [--root-at control_in:W,L] [--route IDENTIFIER]";

fn parse() -> Result<Arguments, Box<dyn Error>> {
    let mut arguments = Arguments {
        input: PathBuf::new(),
        out: PathBuf::new(),
        window: 0,
        expansion_budget: None,
        frontier_limit: 100_000,
        progress_every: 1_000,
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
            "--window" => arguments.window = value.parse()?,
            "--expansion-budget" => arguments.expansion_budget = Some(value.parse()?),
            "--frontier-limit" => arguments.frontier_limit = value.parse()?,
            "--progress-every" => arguments.progress_every = value.parse()?,
            "--terminal" => arguments.terminal = SmbTerminalPredicate::from_identifier(&value)?,
            "--route" => arguments.route = SmbRoute::from_identifier(&value)?,
            "--root-at" => {
                arguments.root_at = Some(SmbTerminalPredicate::from_identifier(&value)?);
            }
            _ => return Err(format!("unknown flag {flag}").into()),
        }
    }
    if arguments.input.as_os_str().is_empty()
        || arguments.out.as_os_str().is_empty()
        || arguments.window == 0
    {
        return Err(USAGE.into());
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
    let canonical = until_terminal(&mut target, &canonical, arguments.terminal)?;
    let root = arguments
        .root_at
        .map(|root_at| until_terminal(&mut target, &canonical, root_at))
        .transpose()?
        .unwrap_or_default();
    drop(target);
    let incumbent = SmbInput {
        actions: canonical.actions[root.actions.len()..].to_vec(),
    };
    let game = SmbGame::from_environment(&rom)?
        .with_root(root.clone())
        .with_route(arguments.route);
    let boot_frames = game.new_target()?.boot_frames();
    let run = SmbCampaignRun {
        vocabulary: SmbButtonVocabulary::default(),
        terminal: Some(arguments.terminal),
    };
    let rooted = |tail: &SmbInput| SmbInput {
        actions: root.actions.iter().chain(&tail.actions).copied().collect(),
    };
    let config = WindowConfig {
        start: incumbent.actions.len().saturating_sub(arguments.window),
        expansion_budget: arguments.expansion_budget,
        frontier_limit: arguments.frontier_limit,
    };
    write_json(
        arguments.out.join("incumbent-input.json"),
        &rooted(&incumbent),
    )?;
    let started = Instant::now();
    let mut progress = BufWriter::new(File::create(arguments.out.join("progress.jsonl"))?);
    let mut improvements = BufWriter::new(File::create(arguments.out.join("improvements.jsonl"))?);
    let every = arguments.progress_every.max(1);
    let out = arguments.out.clone();
    let outcome = search_window(&game, &run, &incumbent, &config, |event| {
        match event {
            WindowEvent::Improved { input, progress } => {
                write_json(out.join("best-input.json"), &rooted(input))?;
                let line = json!({
                    "seconds": started.elapsed().as_secs_f64(),
                    "frames_from_power_on": boot_frames + progress.cost.primary,
                    "progress": progress,
                });
                writeln!(improvements, "{line}")?;
                improvements.flush()?;
            }
            WindowEvent::Expanded { progress: state } => {
                if state.expansions.is_multiple_of(every) {
                    let seconds = started.elapsed().as_secs_f64();
                    let line = json!({
                        "seconds": seconds,
                        "expansions_per_second": state.expansions as f64 / seconds.max(1e-9),
                        "progress": state,
                    });
                    writeln!(progress, "{line}")?;
                    progress.flush()?;
                }
            }
        }
        Ok(())
    })?;
    let seconds = started.elapsed().as_secs_f64();
    write_json(
        arguments.out.join("best-input.json"),
        &rooted(&outcome.input),
    )?;
    write_json(
        arguments.out.join("outcome.json"),
        &json!({
            "seconds": seconds,
            "expansions_per_second": outcome.progress.expansions as f64 / seconds.max(1e-9),
            "boot_frames": boot_frames,
            "terminal": arguments.terminal.identifier(),
            "root_at": arguments.root_at.map(SmbTerminalPredicate::identifier),
            "root_actions": root.actions.len(),
            "route": arguments.route.identifier(),
            "window_start": config.start,
            "window_actions": incumbent.actions.len() - config.start,
            "initial_frames_from_power_on": boot_frames + outcome.progress.initial_cost.primary,
            "frames_from_power_on": boot_frames + outcome.progress.cost.primary,
            "lower_bound_frames_from_power_on": boot_frames + outcome.progress.lower_bound.primary,
            "stop": outcome.stop,
            "progress": outcome.progress,
        }),
    )?;
    Ok(())
}
