// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    env,
    error::Error,
    ffi::OsString,
    fs,
    io::{BufWriter, Write},
    path::PathBuf,
    process::Command,
};

use nes_workload::{
    mm2::{
        archive::{
            MAX_ARCHIVE_ENTRIES, MAX_MM2_ACTIONS, Mm2ArchiveReport, selector_policy_from_identifier,
        },
        campaign::{
            Mm2CampaignCheckpoint, Mm2CampaignConfig, Mm2CampaignOrigin, Mm2Game,
            Mm2SnapshotCheckpoint, SNAPSHOT_CHECKPOINT_FORMAT, replay_mm2_campaign_checkpointed,
            run_mm2_campaign_checkpointed,
        },
        target::{Mm2Input, Mm2MechanicalState, Mm2Stage, Mm2VideoMetadata, power_on_walk},
    },
    search::{
        archive::{
            RetentionPolicy, RetireThresholds, SelectorPolicy, retention_policy_from_identifier,
        },
        campaign::{Reporting, TargetExecution},
        draw::{DrawMixture, SuffixShape, draw_mixture_from_identifier},
    },
    target::{ExitKind, Target},
};
use serde_json::json;
use sha2::{Digest, Sha256};

struct Args {
    core: PathBuf,
    rom: PathBuf,
    output: PathBuf,
    seed: u64,
    executions: u64,
    workers: u32,
    action_limit: usize,
    stage: Mm2Stage,
    marketing_soak: bool,
    fixed_execution_soak: bool,
    coherent_world: bool,
    whole_game: bool,
    host: String,
    memory_budget_mib: Option<usize>,
    prefix_input: Option<PathBuf>,
    root_input: Option<PathBuf>,
    resume_archive: Option<PathBuf>,
    resume_snapshots: Option<PathBuf>,
    save_checkpoint: bool,
    mixture: DrawMixture,
    selector: SelectorPolicy,
    retention: RetentionPolicy,
}

struct RenderedMedia {
    video: Mm2VideoMetadata,
    audio_pcm_sha256: String,
    mp4_sha256: String,
}

impl Args {
    fn parse() -> Result<Self, Box<dyn Error>> {
        Self::parse_from(env::args_os().skip(1))
    }

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
        let mut supplied_action_limit = false;
        let mut stage = Mm2Stage::default();
        let mut marketing_soak = false;
        let mut fixed_execution_soak = false;
        let mut coherent_world = false;
        let mut whole_game = false;
        let mut diagnostic_policy = false;
        let mut supplied_stage = false;
        let mut host = "github-actions".to_owned();
        let mut memory_budget_mib = None;
        let mut prefix_input = None;
        let mut root_input = None;
        let mut resume_archive = None;
        let mut resume_snapshots = None;
        let mut save_checkpoint = false;
        let mut mixture = DrawMixture::AlphabetOnly;
        let mut selector = SelectorPolicy::EnergyFrontierCheapest(RetireThresholds {
            entry: 3,
            groups: vec![6, 12, 2, 16],
        });
        let mut retention = RetentionPolicy::Unprobed;
        let mut args = values.into_iter();
        while let Some(flag) = args.next() {
            if flag == "--whole-game" {
                whole_game = true;
                continue;
            }
            if matches!(
                flag.to_str(),
                Some("--selector" | "--retention" | "--mixture")
            ) {
                diagnostic_policy = true;
            }
            if flag == "--marketing-soak" {
                marketing_soak = true;
                continue;
            }
            if flag == "--fixed-execution-soak" {
                fixed_execution_soak = true;
                continue;
            }
            if flag == "--coherent-world" {
                coherent_world = true;
                continue;
            }
            if flag == "--save-checkpoint" {
                save_checkpoint = true;
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
                "--action-limit" => {
                    action_limit = parse_number("action-limit", value)?;
                    supplied_action_limit = true;
                }
                "--stage" => {
                    supplied_stage = true;
                    stage =
                        Mm2Stage::parse(&value.into_string().map_err(|_| "stage is not UTF-8")?)?;
                }
                "--host" => {
                    host = value.into_string().map_err(|_| "host is not UTF-8")?;
                }
                "--memory-budget-mib" => {
                    memory_budget_mib = Some(parse_number("memory-budget-mib", value)?);
                }
                "--prefix-input" => {
                    prefix_input = Some(PathBuf::from(value));
                }
                "--root-input" => root_input = Some(PathBuf::from(value)),
                "--resume-archive" => resume_archive = Some(PathBuf::from(value)),
                "--resume-snapshots" => resume_snapshots = Some(PathBuf::from(value)),
                "--mixture" => {
                    mixture = draw_mixture_from_identifier(
                        &value.into_string().map_err(|_| "mixture is not UTF-8")?,
                    )?;
                }
                "--selector" => {
                    selector = selector_policy_from_identifier(
                        &value.into_string().map_err(|_| "selector is not UTF-8")?,
                    )?;
                }
                "--retention" => {
                    retention = retention_policy_from_identifier(
                        &value.into_string().map_err(|_| "retention is not UTF-8")?,
                    )?;
                }
                other => return Err(format!("unknown argument {other:?}").into()),
            }
        }
        if whole_game && !supplied_action_limit {
            action_limit = MAX_MM2_ACTIONS;
        }
        if whole_game
            && (supplied_stage
                || coherent_world
                || prefix_input.is_some()
                || root_input.is_some()
                || diagnostic_policy)
        {
            return Err("--whole-game uses power-on stage selection and the fixed search policy; stage, prefix, root, coherent-world, and policy overrides are diagnostic-only".into());
        }
        if resume_snapshots.is_some() && resume_archive.is_none() {
            return Err("--resume-snapshots requires --resume-archive".into());
        }
        if save_checkpoint && !marketing_soak {
            return Err("--save-checkpoint requires --marketing-soak".into());
        }
        Ok(Self {
            core: core.ok_or("missing --core")?,
            rom: rom.ok_or("missing --rom")?,
            output: output.ok_or("missing --output")?,
            seed,
            executions,
            workers,
            action_limit,
            stage,
            marketing_soak,
            fixed_execution_soak,
            coherent_world,
            whole_game,
            host,
            memory_budget_mib,
            prefix_input,
            root_input,
            resume_archive,
            resume_snapshots,
            save_checkpoint,
            mixture,
            selector,
            retention,
        })
    }
}

fn parse_number<T>(name: &str, value: OsString) -> Result<T, Box<dyn Error>>
where
    T: std::str::FromStr,
    T::Err: Error + 'static,
{
    Ok(value
        .into_string()
        .map_err(|_| format!("{name} is not UTF-8"))?
        .replace('_', "")
        .parse()?)
}

fn resume_origin(args: &Args) -> Result<Mm2CampaignOrigin, Box<dyn Error>> {
    let Some(archive_path) = args.resume_archive.as_ref() else {
        return Ok(Mm2CampaignOrigin::Genesis);
    };
    let archive_bytes = fs::read(archive_path)?;
    let archive: Mm2ArchiveReport = serde_json::from_slice(&archive_bytes)?;
    let checkpoint = args
        .resume_snapshots
        .as_ref()
        .map(|path| -> Result<Mm2CampaignCheckpoint, Box<dyn Error>> {
            let bytes = fs::read(path)?;
            let snapshots = Mm2SnapshotCheckpoint::from_bytes(&bytes, SNAPSHOT_CHECKPOINT_FORMAT)?;
            Ok(Mm2CampaignCheckpoint {
                path: path.to_string_lossy().into_owned(),
                file_sha256: sha256(&bytes),
                snapshots,
            })
        })
        .transpose()?;
    Ok(Mm2CampaignOrigin::Archive {
        path: archive_path.to_string_lossy().into_owned(),
        file_sha256: sha256(&archive_bytes),
        report: Box::new(archive),
        checkpoint,
    })
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse()?;
    fs::create_dir_all(&args.output)?;
    let rom = fs::read(&args.rom)?;
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&args.core)?));
    let prefix = match &args.prefix_input {
        Some(path) => serde_json::from_slice::<Mm2Input>(&fs::read(path)?)?.actions,
        None => power_on_walk(),
    };
    let mut game = if args.whole_game {
        Mm2Game::new_whole_game(&rom, &args.core, &core_sha256)
    } else {
        Mm2Game::new_at_stage_after_with_coherent_world(
            &rom,
            &args.core,
            &core_sha256,
            prefix,
            args.stage,
            args.coherent_world,
        )
    }
    .with_champion_input_path(args.output.join("champion-input.json"));
    if let Some(path) = &args.root_input {
        let root: Mm2Input = serde_json::from_slice(&fs::read(path)?)?;
        fs::write(
            args.output.join("root-input.json"),
            serde_json::to_vec_pretty(&root)?,
        )?;
        game = game.with_root_actions(root.actions);
    }
    let origin = resume_origin(&args)?;
    let config = campaign_config(&args);
    if args.marketing_soak {
        run_marketing_soak(&game, &config, &args.output, &origin, args.save_checkpoint)
    } else {
        run_qualified_campaign(&game, &config, &args.output, &origin)
    }
}

fn campaign_config(args: &Args) -> Mm2CampaignConfig {
    Mm2CampaignConfig {
        campaign_seed: args.seed,
        workers: args.workers,
        execution_budget: args.executions,
        action_limit: args.action_limit,
        host: args.host.clone(),
        wall_budget: None,
        continue_after_victory: args.fixed_execution_soak,
        archive_entry_limit: MAX_ARCHIVE_ENTRIES,
        memory_budget_mib: args.memory_budget_mib,
        materialize_final_artifacts: true,
        retention: args.retention,
        selector: args.selector.clone(),
        suffix: SuffixShape::OneToSix,
        mixture: args.mixture,
        victory_input_path: Some(args.output.join("victory-input.json")),
    }
}

fn run_marketing_soak(
    game: &Mm2Game,
    config: &Mm2CampaignConfig,
    output: &std::path::Path,
    origin: &Mm2CampaignOrigin,
    save_checkpoint: bool,
) -> Result<(), Box<dyn Error>> {
    let mut stream = BufWriter::new(fs::File::create(output.join("stream.jsonl"))?);
    let mut progress = BufWriter::new(fs::File::create(output.join("progress.jsonl"))?);
    let (live, checkpoint) =
        run_mm2_campaign_checkpointed(game, config, origin, &mut stream, Some(&mut progress))?;
    stream.flush()?;
    drop(stream);
    progress.flush()?;
    drop(progress);
    let checkpoint_sha256 = if save_checkpoint {
        let bytes = checkpoint.to_bytes()?;
        fs::write(output.join("snapshots.bin"), &bytes)?;
        Some(sha256(&bytes))
    } else {
        None
    };
    drop(checkpoint);

    let best_input = live
        .objective_witness
        .as_ref()
        .unwrap_or(&live.archive.champion_input)
        .clone();
    if let Some(victory) = live.objective_witness.as_ref() {
        let genesis_prefix = game.new_target()?.genesis_prefix().to_vec();
        let mut next = genesis_prefix.clone();
        next.extend(victory.actions.iter().copied());
        if !game.is_whole_game() && !game.stage().is_wily() {
            next.extend(game.walk_to_stage_select(&next)?);
        }
        fs::write(
            output.join("next-prefix.json"),
            serde_json::to_vec_pretty(&Mm2Input { actions: next })?,
        )?;
        let mut full = genesis_prefix;
        full.extend(victory.actions.iter().copied());
        fs::write(
            output.join("full-victory-input.json"),
            serde_json::to_vec_pretty(&Mm2Input { actions: full })?,
        )?;
    }
    let campaign = json!({
        "mode": if config.continue_after_victory {
            "direct_quicknes_fixed_execution_soak"
        } else {
            "marketing_soak"
        },
        "fixed_execution_soak": config.continue_after_victory,
        "verification": "champion_endpoint_reported",
        "origin": &live.origin,
        "workload_identity_sha256": game.workload_identity_sha256(),
        "checkpoint_sha256": checkpoint_sha256,
        "stage": game.stage().number(),
        "whole_game": game.is_whole_game(),
        "campaign_seed": live.campaign_seed,
        "workers": live.workers,
        "execution_budget": live.execution_budget,
        "executions": live.executions_completed,
        "execution_budget_exact": live.executions_completed == live.execution_budget,
        "frames_emulated": live.execution_work,
        "stream_sha256": &live.stream_sha256,
        "archive_entries": live.archive.entries.len(),
        "retained": live.archive.retained,
        "rejected": live.archive.rejected,
        "deaths": live.archive.deaths,
        "duplicates_skipped": live.duplicates_skipped,
        "victories": live.objectives_reached,
        "jobs_per_worker": &live.jobs_per_worker,
        "progress": live.archive.progress_watermark,
        "milestones": live.archive.milestones,
        "first_reached": live.archive.first_reached,
        "progress_curve": &live.archive.progress_curve,
    });
    fs::write(
        output.join("archive.json"),
        serde_json::to_vec_pretty(&live.archive)?,
    )?;
    drop(live);

    let best_endpoint = write_best_observation(game, &best_input, output)?;
    let media = render_video(game, &best_input, output, 180)?;
    let champion_endpoint_verified = media.video.input_endpoint == best_endpoint;
    let result = json!({
        "campaign": campaign,
        "champion_endpoint_verified": champion_endpoint_verified,
        "headless_input_endpoint": best_endpoint,
        "video": media.video,
        "audio_pcm_sha256": media.audio_pcm_sha256,
        "mp4_sha256": media.mp4_sha256,
    });
    fs::write(
        output.join("run-summary.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    let console_summary = json!({
        "executions": &result["campaign"]["executions"],
        "archive_entries": &result["campaign"]["archive_entries"],
        "progress": &result["campaign"]["progress"],
        "milestones": &result["campaign"]["milestones"],
        "champion_endpoint_verified": &result["champion_endpoint_verified"],
        "video": &result["video"],
        "audio_pcm_sha256": &result["audio_pcm_sha256"],
        "mp4_sha256": &result["mp4_sha256"],
    });
    println!("{}", serde_json::to_string_pretty(&console_summary)?);
    Ok(())
}

fn replay_origin(
    origin: &Mm2CampaignOrigin,
) -> (Option<&Mm2ArchiveReport>, Option<&Mm2CampaignCheckpoint>) {
    match origin {
        Mm2CampaignOrigin::Archive {
            report, checkpoint, ..
        } => (Some(report.as_ref()), checkpoint.as_ref()),
        Mm2CampaignOrigin::Genesis | Mm2CampaignOrigin::SnapshotRoot { .. } => (None, None),
    }
}

fn run_qualified_campaign(
    game: &Mm2Game,
    config: &Mm2CampaignConfig,
    output: &std::path::Path,
    origin: &Mm2CampaignOrigin,
) -> Result<(), Box<dyn Error>> {
    let stream_path = output.join("stream.jsonl");
    let stream_file = fs::File::create(&stream_path)?;
    let mut stream = BufWriter::new(stream_file);
    let (live, checkpoint) =
        run_mm2_campaign_checkpointed(game, config, origin, &mut stream, None)?;
    drop(stream);

    let stream_bytes = fs::read(&stream_path)?;
    let (origin_report, origin_checkpoint) = replay_origin(origin);
    let (replayed, replayed_checkpoint) =
        replay_mm2_campaign_checkpointed(game, &stream_bytes, origin_report, origin_checkpoint)?;
    let report_bytes = serde_json::to_vec_pretty(&live)?;
    let replayed_report_bytes = serde_json::to_vec_pretty(&replayed)?;
    let checkpoint_bytes = checkpoint.to_bytes()?;
    let replayed_checkpoint_bytes = replayed_checkpoint.to_bytes()?;
    let replay_verified =
        report_bytes == replayed_report_bytes && checkpoint_bytes == replayed_checkpoint_bytes;
    if !replay_verified {
        return Err("Mega Man 2 campaign replay diverged".into());
    }

    fs::write(output.join("campaign-report.json"), &report_bytes)?;
    fs::write(
        output.join("archive.json"),
        serde_json::to_vec_pretty(&live.archive)?,
    )?;
    fs::write(output.join("snapshots.bin"), &checkpoint_bytes)?;

    let best_input = live
        .objective_witness
        .as_ref()
        .unwrap_or(&live.archive.champion_input);
    let best_endpoint = write_best_observation(game, best_input, output)?;
    let media = render_video(game, best_input, output, 180)?;
    if media.video.input_endpoint != best_endpoint {
        return Err("video-enabled replay changed Nova's decoded input endpoint".into());
    }
    let verdict = json!({
        "mode": if config.continue_after_victory {
            "direct_quicknes_fixed_execution_soak"
        } else {
            "qualified_campaign"
        },
        "fixed_execution_soak": config.continue_after_victory,
        "replay_verified": replay_verified,
        "stage": game.stage().number(),
        "stream_sha256": live.stream_sha256,
        "report_sha256": sha256(&report_bytes),
        "checkpoint_sha256": sha256(&checkpoint_bytes),
        "execution_budget": live.execution_budget,
        "executions": live.executions_completed,
        "execution_budget_exact": live.executions_completed == live.execution_budget,
        "retained_representatives": live.archive.entries.len(),
        "progress": live.archive.progress_watermark,
        "milestones": live.archive.milestones,
        "victories": live.objectives_reached,
        "video": media.video,
        "audio_pcm_sha256": media.audio_pcm_sha256,
        "mp4_sha256": media.mp4_sha256,
    });
    fs::write(
        output.join("replay-verdict.json"),
        serde_json::to_vec_pretty(&verdict)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&verdict)?);
    Ok(())
}

fn write_best_observation(
    game: &Mm2Game,
    input: &Mm2Input,
    output: &std::path::Path,
) -> Result<Mm2MechanicalState, Box<dyn Error>> {
    let mut target = game
        .new_target()
        .map_err(|error| -> Box<dyn Error> { error.into() })?;
    target.reset();
    for action in &input.actions {
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok {
            return Err("best Mega Man 2 input crashed during replay".into());
        }
    }
    fs::write(
        output.join("best-input.json"),
        serde_json::to_vec_pretty(input)?,
    )?;
    let observation = target.observe();
    fs::write(
        output.join("best-observation.json"),
        serde_json::to_vec_pretty(&observation)?,
    )?;
    Ok(observation.decoded)
}

fn render_video(
    game: &Mm2Game,
    input: &Mm2Input,
    output: &std::path::Path,
    tail_frames: u32,
) -> Result<RenderedMedia, Box<dyn Error>> {
    let video_path = output.join("best.rgb");
    let audio_path = output.join("best.s16le");
    let mut video_output = BufWriter::new(fs::File::create(&video_path)?);
    let mut audio_output = BufWriter::new(fs::File::create(&audio_path)?);
    let mut target = game
        .new_target()
        .map_err(|error| -> Box<dyn Error> { error.into() })?;
    let video = target.render_input(input, tail_frames, &mut video_output, &mut audio_output)?;
    drop(video_output);
    drop(audio_output);
    let audio_pcm_sha256 = sha256(&fs::read(&audio_path)?);
    let geometry = format!("{}x{}", video.width, video.height);
    let sample_rate = video.audio_sample_rate.to_string();
    let channels = video.audio_channels.to_string();
    let mp4_path = output.join("best.mp4");
    let status = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "rawvideo",
            "-pixel_format",
            "rgb24",
            "-video_size",
        ])
        .arg(geometry)
        .args(["-framerate", "60", "-i"])
        .arg(&video_path)
        .args(["-f", "s16le", "-ar", &sample_rate, "-ac", &channels, "-i"])
        .arg(&audio_path)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-crf",
            "18",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-af",
            "apad",
            "-shortest",
            "-movflags",
            "+faststart",
            "-y",
        ])
        .arg(&mp4_path)
        .status()?;
    if !status.success() {
        return Err(format!("ffmpeg failed with {status}").into());
    }
    let mp4_sha256 = sha256(&fs::read(&mp4_path)?);
    fs::write(
        output.join("video.json"),
        serde_json::to_vec_pretty(&json!({
            "video": video,
            "audio_pcm_sha256": &audio_pcm_sha256,
            "mp4_sha256": &mp4_sha256,
        }))?,
    )?;
    fs::remove_file(video_path)?;
    fs::remove_file(audio_path)?;
    Ok(RenderedMedia {
        video,
        audio_pcm_sha256,
        mp4_sha256,
    })
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::{Args, OsString, PathBuf, campaign_config};

    fn required_args(extra: &[&str]) -> Vec<OsString> {
        let mut args = ["--core", "core", "--rom", "rom", "--output", "output"]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        args.extend(extra.iter().map(OsString::from));
        args
    }

    #[test]
    fn fixed_execution_soak_is_explicit_and_valueless() {
        let ordinary = Args::parse_from(required_args(&[])).expect("ordinary arguments parse");
        assert!(!ordinary.fixed_execution_soak);

        let soak = Args::parse_from(required_args(&["--fixed-execution-soak", "--seed", "42"]))
            .expect("soak arguments parse");
        assert!(soak.fixed_execution_soak);
        assert_eq!(soak.seed, 42);
    }

    #[test]
    fn coherent_world_is_explicit_and_valueless() {
        let ordinary = Args::parse_from(required_args(&[])).expect("ordinary arguments parse");
        assert!(!ordinary.coherent_world);

        let diagnostic = Args::parse_from(required_args(&["--coherent-world", "--seed", "42"]))
            .expect("coherent-world arguments parse");
        assert!(diagnostic.coherent_world);
        assert_eq!(diagnostic.seed, 42);
    }

    #[test]
    fn whole_game_rejects_guided_origins_and_policy_overrides() {
        let whole = Args::parse_from(required_args(&["--whole-game"])).expect("whole game");
        assert!(whole.whole_game);
        for flags in [
            vec!["--stage", "air"],
            vec!["--prefix-input", "prefix.json"],
            vec!["--root-input", "root.json"],
            vec!["--coherent-world"],
            vec![
                "--selector",
                "hierarchy_uniform_128_energy_frontier_cheapest:3,6,12,2,16",
            ],
            vec!["--retention", "unprobed"],
            vec!["--mixture", "alphabet_only"],
        ] {
            let mut arguments = vec!["--whole-game"];
            arguments.extend(flags);
            assert!(Args::parse_from(required_args(&arguments)).is_err());
        }
    }

    #[test]
    fn unknown_arguments_are_rejected() {
        assert!(Args::parse_from(required_args(&["--unknown", "value"])).is_err());
    }

    #[test]
    fn fixed_execution_soak_wires_campaign_config() {
        let ordinary = Args::parse_from(required_args(&[])).expect("ordinary arguments parse");
        assert!(!campaign_config(&ordinary).continue_after_victory);

        let soak = Args::parse_from(required_args(&["--fixed-execution-soak"]))
            .expect("soak arguments parse");
        assert!(campaign_config(&soak).continue_after_victory);
    }

    #[test]
    fn archive_resume_flags_parse_and_checkpoint_save_is_marketing_only() {
        let args = Args::parse_from(required_args(&[
            "--resume-archive",
            "archive.json",
            "--resume-snapshots",
            "snapshots.bin",
            "--marketing-soak",
            "--save-checkpoint",
        ]))
        .expect("archive resume arguments parse");
        assert_eq!(args.resume_archive, Some(PathBuf::from("archive.json")));
        assert_eq!(args.resume_snapshots, Some(PathBuf::from("snapshots.bin")));
        assert!(args.save_checkpoint);
    }

    #[test]
    fn malformed_archive_resume_flags_are_rejected() {
        assert!(
            Args::parse_from(required_args(&["--resume-snapshots", "snapshots.bin",])).is_err()
        );
        assert!(Args::parse_from(required_args(&["--save-checkpoint"])).is_err());
    }
}
