// SPDX-License-Identifier: AGPL-3.0-or-later

#![recursion_limit = "256"]

//! Run a bounded Super Tilt Bro campaign and render one retained witness.

use std::{
    env,
    error::Error,
    ffi::OsString,
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    process::Command,
};

use nes_workload::{
    search::{
        archive::{RetentionPolicy, RetireThresholds, SelectorPolicy},
        campaign::{Reporting, TargetExecution},
        draw::{DrawMixture, SuffixShape},
    },
    stb::{
        archive::MAX_ARCHIVE_ENTRIES,
        campaign::{
            StbCampaignConfig, StbCampaignOrigin, StbGame, replay_stb_campaign_checkpointed,
            run_stb_campaign_checkpointed,
        },
        target::{StbInput, StbMechanicalState, StbVideoMetadata},
    },
    target::{ExitKind, Target},
};
use serde_json::json;
use sha2::{Digest, Sha256};

/// Safe defaults for a first local qualification run. Larger explicit values
/// remain available for sustained evaluation under the generic campaign
/// limits. These defaults do not cap explicitly supplied run budgets.
const DEFAULT_EXECUTIONS: u64 = 2_000;
const DEFAULT_WORKERS: u32 = 2;
const MAX_RENDER_FRAMES: u64 = 600;

struct Args {
    core: PathBuf,
    rom: PathBuf,
    output: PathBuf,
    seed: u64,
    executions: u64,
    workers: u32,
    action_limit: usize,
    fixed_execution_soak: bool,
    host: String,
    memory_budget_mib: Option<usize>,
}

struct RenderedMedia {
    video: StbVideoMetadata,
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
        let mut executions = DEFAULT_EXECUTIONS;
        let mut workers = DEFAULT_WORKERS;
        let mut action_limit = 512_usize;
        let mut fixed_execution_soak = false;
        let mut host = "local-stb-trial".to_owned();
        let mut memory_budget_mib = None;
        let mut args = values.into_iter();
        while let Some(flag) = args.next() {
            if flag == "--fixed-execution-soak" {
                fixed_execution_soak = true;
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
                    memory_budget_mib = Some(parse_number("memory-budget-mib", value)?)
                }
                other => return Err(format!("unknown argument {other:?}").into()),
            }
        }
        let core = core.ok_or("missing --core")?;
        let rom = rom.ok_or("missing --rom")?;
        let output = output.ok_or("missing --output")?;
        if workers == 0 {
            return Err("workers must be at least 1".into());
        }
        if executions == 0 {
            return Err("executions must be at least 1".into());
        }
        if action_limit == 0 || action_limit > nes_workload::stb::archive::MAX_STB_ACTIONS {
            return Err(format!(
                "action-limit must be between 1 and {}",
                nes_workload::stb::archive::MAX_STB_ACTIONS
            )
            .into());
        }
        Ok(Self {
            core,
            rom,
            output,
            seed,
            executions,
            workers,
            action_limit,
            fixed_execution_soak,
            host,
            memory_budget_mib,
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

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse()?;
    fs::create_dir_all(&args.output)?;
    let rom = fs::read(&args.rom)?;
    let core_sha256 = sha256(&fs::read(&args.core)?);
    let game = StbGame::new(&rom, &args.core, &core_sha256);
    run_qualified_campaign(&game, &campaign_config(&args), &args.output)
}

fn campaign_config(args: &Args) -> StbCampaignConfig {
    StbCampaignConfig {
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
        // The control probe found no setup or admission failure. Keep the
        // primary run on ordinary live admission with no hidden lookahead.
        retention: RetentionPolicy::AdmitAlive,
        selector: SelectorPolicy::EnergyFrontierCheapest(RetireThresholds {
            entry: 3,
            groups: vec![6, 12, 2],
        }),
        suffix: SuffixShape::OneToSix,
        mixture: DrawMixture::AlphabetOnly,
        victory_input_path: Some(args.output.join("victory-input.json")),
    }
}

fn run_qualified_campaign(
    game: &StbGame,
    config: &StbCampaignConfig,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    let stream_path = output.join("stream.jsonl");
    let mut stream = BufWriter::new(fs::File::create(&stream_path)?);
    let progress_path = output.join("progress.jsonl");
    let mut progress = BufWriter::new(fs::File::create(&progress_path)?);
    let (live, checkpoint) = run_stb_campaign_checkpointed(
        game,
        config,
        &StbCampaignOrigin::Genesis,
        &mut stream,
        Some(&mut progress),
    )?;
    stream.flush()?;
    progress.flush()?;
    drop(stream);
    drop(progress);

    let stream_bytes = fs::read(&stream_path)?;
    let (replayed, replayed_checkpoint) =
        replay_stb_campaign_checkpointed(game, &stream_bytes, None, None)?;
    let report_bytes = serde_json::to_vec_pretty(&live)?;
    let replayed_report_bytes = serde_json::to_vec_pretty(&replayed)?;
    let checkpoint_bytes = checkpoint.to_bytes()?;
    let replayed_checkpoint_bytes = replayed_checkpoint.to_bytes()?;
    let replay_verified =
        report_bytes == replayed_report_bytes && checkpoint_bytes == replayed_checkpoint_bytes;
    if !replay_verified {
        return Err("STB campaign replay diverged".into());
    }

    fs::write(output.join("campaign-report.json"), &report_bytes)?;
    fs::write(
        output.join("replay-report.json"),
        serde_json::to_vec_pretty(&replayed)?,
    )?;
    fs::write(
        output.join("archive.json"),
        serde_json::to_vec_pretty(&live.archive)?,
    )?;
    fs::write(output.join("snapshots.bin"), &checkpoint_bytes)?;

    // Use the actual champion (or the first verified victory input) for
    // qualification. Full headless replay evidence is retained even when the
    // rendered excerpt must be bounded for disk and encoding cost.
    let champion = live
        .victory_input
        .clone()
        .unwrap_or_else(|| live.archive.champion_input.clone());
    let champion_endpoint = write_headless_observation(game, &champion, output, "champion")?;
    let render_input = bounded_render_excerpt(&champion, MAX_RENDER_FRAMES);
    let rendered_endpoint = write_headless_observation(game, &render_input, output, "render")?;
    let media = render_video(game, &render_input, output, 180)?;
    if media.video.input_endpoint != rendered_endpoint {
        return Err("video-enabled replay changed STB rendered endpoint".into());
    }

    let summary = json!({
        "mode": if config.continue_after_victory {
            "direct_quicknes_fixed_execution_soak"
        } else {
            "qualified_campaign"
        },
        "rom_sha256": game.image_sha256(),
        "emulator_identity": game.emulator_identity(),
        "campaign_seed": live.campaign_seed,
        "workers": live.workers,
        "execution_budget": live.execution_budget,
        "executions": live.executions_completed,
        "execution_budget_exact": live.executions_completed == live.execution_budget,
        "frames_emulated": live.frames_emulated,
        "stream_sha256": live.stream_sha256,
        "stream_file_sha256": sha256(&stream_bytes),
        "report_sha256": sha256(&report_bytes),
        "checkpoint_sha256": sha256(&checkpoint_bytes),
        "progress_file_sha256": sha256(&fs::read(&progress_path)?),
        "archive_entries": live.archive.entries.len(),
        "retained": live.archive.retained,
        "rejected": live.archive.rejected,
        "deaths": live.archive.deaths,
        "duplicates_skipped": live.duplicates_skipped,
        "probe_refused": live.probe_refused,
        "victories": live.victories,
        "progress": live.archive.progress_watermark,
        "milestones": live.archive.milestones,
        "first_reached": live.archive.first_reached,
        "replay_verified": replay_verified,
        "champion_input_actions": champion.actions.len(),
        "champion_endpoint": champion_endpoint,
        "rendered_input_actions": render_input.actions.len(),
        "rendered_input_frames": render_input
            .actions
            .iter()
            .map(|action| u64::from(action.bounded_hold_frames()))
            .sum::<u64>(),
        "render_excerpt_max_frames": MAX_RENDER_FRAMES,
        "render_excerpt": render_input != champion,
        "rendered_endpoint": rendered_endpoint,
        "rendered_endpoint_verified": true,
        "video": media.video,
        "audio_pcm_sha256": media.audio_pcm_sha256,
        "mp4_sha256": media.mp4_sha256,
        "artifact_files": {
            "stream": stream_path,
            "progress": progress_path,
            "campaign_report": output.join("campaign-report.json"),
            "archive": output.join("archive.json"),
            "snapshots": output.join("snapshots.bin"),
            "champion_input": output.join("champion-input.json"),
            "champion_observation": output.join("champion-observation.json"),
            "render_input": output.join("render-input.json"),
            "render_observation": output.join("render-observation.json"),
            "video_raw": output.join("witness.rgb24"),
            "audio_raw": output.join("witness.s16le"),
            "video_mp4": output.join("witness.mp4"),
        },
    });
    fs::write(
        output.join("run-summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn write_headless_observation(
    game: &StbGame,
    input: &StbInput,
    output: &Path,
    stem: &str,
) -> Result<StbMechanicalState, Box<dyn Error>> {
    let mut target = game
        .new_target()
        .map_err(|error| -> Box<dyn Error> { error.into() })?;
    target.reset();
    for action in &input.actions {
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok {
            return Err("retained STB witness crashed during replay".into());
        }
    }
    fs::write(
        output.join(format!("{stem}-input.json")),
        serde_json::to_vec_pretty(input)?,
    )?;
    let observation = target.observe();
    fs::write(
        output.join(format!("{stem}-observation.json")),
        serde_json::to_vec_pretty(&observation)?,
    )?;
    Ok(observation.decoded)
}

fn bounded_render_excerpt(input: &StbInput, max_frames: u64) -> StbInput {
    let mut frames_left = max_frames;
    let mut actions = Vec::new();
    for action in &input.actions {
        if frames_left == 0 {
            break;
        }
        let hold = u64::from(action.bounded_hold_frames()).min(frames_left);
        actions.push(machine::nes::ButtonChord::new(
            action.buttons,
            u8::try_from(hold).expect("ButtonChord hold is bounded"),
        ));
        frames_left -= hold;
    }
    StbInput { actions }
}

fn render_video(
    game: &StbGame,
    input: &StbInput,
    output: &Path,
    tail_frames: u32,
) -> Result<RenderedMedia, Box<dyn Error>> {
    let video_path = output.join("witness.rgb24");
    let audio_path = output.join("witness.s16le");
    let mut video_output = BufWriter::new(fs::File::create(&video_path)?);
    let mut audio_output = BufWriter::new(fs::File::create(&audio_path)?);
    let mut target = game
        .new_target()
        .map_err(|error| -> Box<dyn Error> { error.into() })?;
    let video = target.render_input(input, tail_frames, &mut video_output, &mut audio_output)?;
    video_output.flush()?;
    audio_output.flush()?;
    drop(video_output);
    drop(audio_output);
    let audio_pcm_sha256 = sha256(&fs::read(&audio_path)?);
    let geometry = format!("{}x{}", video.width, video.height);
    let sample_rate = video.audio_sample_rate.to_string();
    let channels = video.audio_channels.to_string();
    let mp4_path = output.join("witness.mp4");
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
    use super::{Args, OsString, campaign_config};

    fn required_args(extra: &[&str]) -> Vec<OsString> {
        let mut args = ["--core", "core", "--rom", "rom", "--output", "output"]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        args.extend(extra.iter().map(OsString::from));
        args
    }

    #[test]
    fn defaults_are_bounded_to_the_initial_pilot() {
        let args = Args::parse_from(required_args(&[])).expect("arguments parse");
        assert_eq!(args.executions, super::DEFAULT_EXECUTIONS);
        assert_eq!(args.workers, super::DEFAULT_WORKERS);
        assert_eq!(args.action_limit, 512);
        assert!(!args.fixed_execution_soak);
    }

    #[test]
    fn fixed_execution_soak_is_explicit_and_valueless() {
        let args = Args::parse_from(required_args(&["--fixed-execution-soak", "--seed", "42"]))
            .expect("soak arguments parse");
        assert!(args.fixed_execution_soak);
        assert_eq!(args.seed, 42);
        assert!(campaign_config(&args).continue_after_victory);
        assert!(matches!(
            campaign_config(&args).retention,
            nes_workload::search::archive::RetentionPolicy::AdmitAlive
        ));
    }

    #[test]
    fn zero_worker_and_execution_values_are_rejected() {
        assert!(Args::parse_from(required_args(&["--workers", "0"])).is_err());
        assert!(Args::parse_from(required_args(&["--executions", "0"])).is_err());
        assert!(Args::parse_from(required_args(&["--action-limit", "0"])).is_err());
    }

    #[test]
    fn explicit_sustained_limits_are_not_confused_with_trial_defaults() {
        let args = Args::parse_from(required_args(&["--workers", "8", "--executions", "50000"]))
            .expect("explicit sustained limits parse");
        assert_eq!(args.workers, 8);
        assert_eq!(args.executions, 50_000);
    }

    #[test]
    fn unknown_arguments_are_rejected() {
        assert!(Args::parse_from(required_args(&["--unknown", "value"])).is_err());
    }
}
