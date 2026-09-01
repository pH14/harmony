// SPDX-License-Identifier: AGPL-3.0-or-later

//! Run a bounded Nova the Squirrel campaign and render its champion.

use std::{
    env,
    error::Error,
    ffi::OsString,
    fs,
    io::{self, BufWriter, Write},
    path::PathBuf,
    process::Command,
};

use searcher::{
    nova::{
        archive::MAX_ARCHIVE_ENTRIES,
        campaign::{
            NovaCampaignConfig, NovaCampaignOrigin, NovaGame, replay_nova_campaign_checkpointed,
            run_nova_campaign_checkpointed,
        },
        target::{NovaInput, NovaLevel, NovaMechanicalState, NovaVideoMetadata},
    },
    search::{
        archive::{RetentionPolicy, RetireThresholds, SelectorPolicy},
        campaign::Game,
        draw::{DrawMixture, SuffixShape},
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
    level: NovaLevel,
    marketing_soak: bool,
}

struct RenderedMedia {
    video: NovaVideoMetadata,
    audio_pcm_sha256: String,
    mp4_sha256: String,
}

impl Args {
    fn parse() -> Result<Self, Box<dyn Error>> {
        let mut core = None;
        let mut rom = None;
        let mut output = None;
        let mut seed = 1_u64;
        let mut executions = 4_000_u64;
        let mut workers = 2_u32;
        let mut action_limit = 512_usize;
        let mut level_number = 1_u8;
        let mut marketing_soak = false;
        let mut args = env::args_os().skip(1);
        while let Some(flag) = args.next() {
            if flag == "--marketing-soak" {
                marketing_soak = true;
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
                "--level" => level_number = parse_number("level", value)?,
                other => return Err(format!("unknown argument {other:?}").into()),
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
            level: NovaLevel::from_number(level_number)?,
            marketing_soak,
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
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&args.core)?));
    let game = NovaGame::new_at_level(&rom, &args.core, &core_sha256, args.level);
    let config = NovaCampaignConfig {
        campaign_seed: args.seed,
        workers: args.workers,
        execution_budget: args.executions,
        action_limit: args.action_limit,
        host: "github-actions".to_owned(),
        wall_budget: None,
        archive_entry_limit: MAX_ARCHIVE_ENTRIES,
        retention: RetentionPolicy::AdmitAlive,
        selector: SelectorPolicy::EnergyFrontierCheapest(RetireThresholds {
            entry: 3,
            groups: vec![6, 12, 2],
        }),
        suffix: SuffixShape::OneToSix,
        mixture: DrawMixture::AlphabetOnly,
        victory_input_path: Some(args.output.join("victory-input.json")),
    };

    if args.marketing_soak {
        run_marketing_soak(&game, &config, &args.output)
    } else {
        run_qualified_campaign(&game, &config, &args.output)
    }
}

fn run_marketing_soak(
    game: &NovaGame,
    config: &NovaCampaignConfig,
    output: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    let mut stream = io::sink();
    let mut progress = BufWriter::new(fs::File::create(output.join("progress.jsonl"))?);
    let (live, checkpoint) = run_nova_campaign_checkpointed(
        game,
        config,
        &NovaCampaignOrigin::Genesis,
        &mut stream,
        Some(&mut progress),
    )?;
    progress.flush()?;
    drop(progress);
    drop(checkpoint);

    let best_input = live
        .victory_input
        .as_ref()
        .unwrap_or(&live.archive.champion_input)
        .clone();
    let campaign = json!({
        "mode": "marketing_soak",
        "verification": "champion_endpoint_reported",
        "level": game.level().number(),
        "campaign_seed": live.campaign_seed,
        "workers": live.workers,
        "execution_budget": live.execution_budget,
        "executions": live.executions_completed,
        "frames_emulated": live.frames_emulated,
        "stream_sha256": &live.stream_sha256,
        "archive_entries": live.archive.entries.len(),
        "retained": live.archive.retained,
        "rejected": live.archive.rejected,
        "deaths": live.archive.deaths,
        "duplicates_skipped": live.duplicates_skipped,
        "victories": live.victories,
        "jobs_per_worker": &live.jobs_per_worker,
        "progress": live.archive.progress_watermark,
        "milestones": live.archive.milestones,
        "first_reached": live.archive.first_reached,
        "progress_curve": &live.archive.progress_curve,
    });
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

fn run_qualified_campaign(
    game: &NovaGame,
    config: &NovaCampaignConfig,
    output: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    let stream_path = output.join("stream.jsonl");
    let stream_file = fs::File::create(&stream_path)?;
    let mut stream = BufWriter::new(stream_file);
    let (live, checkpoint) = run_nova_campaign_checkpointed(
        game,
        config,
        &NovaCampaignOrigin::Genesis,
        &mut stream,
        None,
    )?;
    drop(stream);

    let stream_bytes = fs::read(&stream_path)?;
    let (replayed, replayed_checkpoint) =
        replay_nova_campaign_checkpointed(game, &stream_bytes, None, None)?;
    let report_bytes = serde_json::to_vec_pretty(&live)?;
    let replayed_report_bytes = serde_json::to_vec_pretty(&replayed)?;
    let checkpoint_bytes = checkpoint.to_bytes()?;
    let replayed_checkpoint_bytes = replayed_checkpoint.to_bytes()?;
    let replay_verified =
        report_bytes == replayed_report_bytes && checkpoint_bytes == replayed_checkpoint_bytes;
    if !replay_verified {
        return Err("Nova campaign replay diverged".into());
    }

    fs::write(output.join("campaign-report.json"), &report_bytes)?;
    fs::write(
        output.join("archive.json"),
        serde_json::to_vec_pretty(&live.archive)?,
    )?;
    fs::write(output.join("snapshots.bin"), &checkpoint_bytes)?;

    let best_input = live
        .victory_input
        .as_ref()
        .unwrap_or(&live.archive.champion_input);
    let best_endpoint = write_best_observation(game, best_input, output)?;
    let media = render_video(game, best_input, output, 180)?;
    if media.video.input_endpoint != best_endpoint {
        return Err("video-enabled replay changed Nova's decoded input endpoint".into());
    }
    let verdict = json!({
        "replay_verified": replay_verified,
        "level": game.level().number(),
        "stream_sha256": live.stream_sha256,
        "report_sha256": sha256(&report_bytes),
        "checkpoint_sha256": sha256(&checkpoint_bytes),
        "executions": live.executions_completed,
        "retained_representatives": live.archive.entries.len(),
        "progress": live.archive.progress_watermark,
        "milestones": live.archive.milestones,
        "victories": live.victories,
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
    game: &NovaGame,
    input: &NovaInput,
    output: &std::path::Path,
) -> Result<NovaMechanicalState, Box<dyn Error>> {
    let mut target = game
        .new_target()
        .map_err(|error| -> Box<dyn Error> { error.into() })?;
    target.reset();
    for action in &input.actions {
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok {
            return Err("best Nova input crashed during replay".into());
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
    game: &NovaGame,
    input: &NovaInput,
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
