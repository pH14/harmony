// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    env,
    error::Error,
    ffi::OsString,
    fs,
    io::{BufWriter, Write},
    path::PathBuf,
};

use nes_workload::{
    metroid::{
        archive::MAX_ARCHIVE_ENTRIES,
        campaign::{
            MetroidCampaignConfig, MetroidCampaignOrigin, MetroidGame,
            replay_metroid_campaign_checkpointed, run_metroid_campaign_checkpointed,
        },
    },
    search::{
        archive::{RetentionPolicy, RetireThresholds, SelectorPolicy},
        draw::{DrawMixture, SuffixShape, draw_mixture_from_identifier},
    },
};
use sha2::{Digest, Sha256};

struct Args {
    core: PathBuf,
    rom: PathBuf,
    output: PathBuf,
    seed: u64,
    executions: u64,
    workers: u32,
    action_limit: usize,
    host: String,
    memory_budget_mib: Option<usize>,
    mixture: DrawMixture,
    verify_replay: bool,
    selector: SelectorPolicy,
}

impl Args {
    fn parse_from<I>(values: I) -> Result<Self, Box<dyn Error>>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut core = None;
        let mut rom = None;
        let mut output = None;
        let mut seed = 1_u64;
        let mut executions = 4_000_u64;
        let mut workers = 2_u32;
        let mut action_limit = 4096_usize;
        let mut host = "local".to_owned();
        let mut memory_budget_mib = None;
        let mut mixture = DrawMixture::AlphabetOnly;
        let mut verify_replay = false;
        let mut selector = SelectorPolicy::EnergyFrontierCheapest(RetireThresholds {
            entry: 3,
            groups: vec![6, 12, 2],
        });
        let mut args = values.into_iter();
        while let Some(flag) = args.next() {
            if flag == "--verify-replay" {
                verify_replay = true;
                continue;
            }
            let value = args
                .next()
                .ok_or_else(|| format!("missing value after {}", flag.to_string_lossy()))?;
            match flag.to_string_lossy().as_ref() {
                "--core" => core = Some(PathBuf::from(value)),
                "--rom" => rom = Some(PathBuf::from(value)),
                "--output" => output = Some(PathBuf::from(value)),
                "--seed" => seed = parse_number("seed", value)?,
                "--executions" => executions = parse_number("executions", value)?,
                "--workers" => workers = parse_number("workers", value)?,
                "--action-limit" => action_limit = parse_number("action-limit", value)?,
                "--host" => host = value.into_string().map_err(|_| "host is not UTF-8")?,
                "--memory-budget-mib" => {
                    memory_budget_mib = Some(parse_number("memory-budget-mib", value)?);
                }
                "--selector" => {
                    let thresholds = RetireThresholds {
                        entry: 3,
                        groups: vec![6, 12, 2],
                    };
                    selector = match value.to_string_lossy().as_ref() {
                        "energy" => SelectorPolicy::Energy(thresholds),
                        "frontier" => SelectorPolicy::EnergyFrontier(thresholds),
                        "frontier-cheapest" => SelectorPolicy::EnergyFrontierCheapest(thresholds),
                        "pareto-cheapest" => SelectorPolicy::EnergyFrontierCheapest(thresholds),
                        other => return Err(format!("unknown selector {other}").into()),
                    };
                }
                "--mixture" => {
                    mixture = draw_mixture_from_identifier(
                        &value.into_string().map_err(|_| "mixture is not UTF-8")?,
                    )?;
                }
                other => return Err(format!("unknown flag {other}").into()),
            }
        }
        Ok(Self {
            core: core.ok_or("missing --core")?,
            rom: rom.ok_or("missing --rom")?,
            output: output.ok_or("missing --output")?,
            seed,
            executions,
            workers,
            action_limit,
            host,
            memory_budget_mib,
            mixture,
            verify_replay,
            selector,
        })
    }
}

fn parse_number<T>(name: &str, value: OsString) -> Result<T, Box<dyn Error>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .into_string()
        .map_err(|_| format!("{name} is not UTF-8"))?
        .parse()
        .map_err(|error| format!("{name}: {error}").into())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse_from(env::args_os().skip(1))?;
    fs::create_dir_all(&args.output)?;
    let rom = fs::read(&args.rom)?;
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&args.core)?));
    let game = MetroidGame::new(&rom, &args.core, &core_sha256)
        .with_champion_input_path(args.output.join("champion-input.json"));
    let config = MetroidCampaignConfig {
        campaign_seed: args.seed,
        workers: args.workers,
        execution_budget: args.executions,
        action_limit: args.action_limit,
        host: args.host.clone(),
        wall_budget: None,
        continue_after_victory: true,
        archive_entry_limit: MAX_ARCHIVE_ENTRIES,
        memory_budget_mib: args.memory_budget_mib,
        materialize_final_artifacts: true,
        retention: RetentionPolicy::Unprobed,
        selector: args.selector.clone(),
        suffix: SuffixShape::OneToSix,
        mixture: args.mixture,
        victory_input_path: None,
    };
    let mut stream = BufWriter::new(fs::File::create(args.output.join("stream.jsonl"))?);
    let mut progress = BufWriter::new(fs::File::create(args.output.join("progress.jsonl"))?);
    let (report, _checkpoint) = run_metroid_campaign_checkpointed(
        &game,
        &config,
        &MetroidCampaignOrigin::Genesis,
        &mut stream,
        Some(&mut progress),
    )?;
    stream.flush()?;
    progress.flush()?;
    fs::write(
        args.output.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    if args.verify_replay {
        let stream_bytes = fs::read(args.output.join("stream.jsonl"))?;
        let (replayed, _) = replay_metroid_campaign_checkpointed(&game, &stream_bytes, None, None)?;
        if replayed != report {
            return Err("Metroid replay diverged from the recorded campaign".into());
        }
        println!("replay verified");
    }
    let archive = &report.archive;
    let cells: std::collections::BTreeSet<(u8, u8, u8)> = archive
        .entries
        .iter()
        .map(|entry| (entry.key.area, entry.key.map_x, entry.key.map_y))
        .collect();
    println!(
        "executions={} retained={} rejected={} deaths={} items={} tanks={} areas={:08b} \
         distinct_map_cells={} cells={:?}",
        archive.executions,
        archive.retained,
        archive.rejected,
        archive.deaths,
        archive.milestones.items,
        archive.milestones.tanks,
        archive.milestones.areas,
        cells.len(),
        cells,
    );
    Ok(())
}
