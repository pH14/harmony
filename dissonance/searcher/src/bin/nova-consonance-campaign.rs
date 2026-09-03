// SPDX-License-Identifier: AGPL-3.0-or-later

//! Run the game-blind Nova campaign with QuickNES inside Consonance.

#[cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    real::run()
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64", not(miri))))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("nova-consonance-campaign requires Linux/x86-64 KVM outside Miri".into())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]
mod real {

    use std::{
        env,
        error::Error,
        ffi::OsString,
        fs,
        io::{self, BufWriter},
        path::PathBuf,
        process::Command,
    };

    use searcher::{
        nova::{
            archive::MAX_ARCHIVE_ENTRIES,
            campaign::{
                NovaCampaignConfig, NovaCampaignOrigin, NovaGame, run_nova_campaign_checkpointed,
            },
            target::NovaInput,
        },
        search::campaign::Game,
        search::{
            archive::{RetentionPolicy, RetireThresholds, SelectorPolicy},
            draw::{DrawMixture, SuffixShape},
        },
    };
    use serde_json::json;
    use sha2::{Digest, Sha256};

    struct Args {
        kernel: PathBuf,
        initramfs: PathBuf,
        rom: PathBuf,
        core: PathBuf,
        output: PathBuf,
        seed: u64,
        executions: u64,
        workers: u32,
        action_limit: usize,
        wall_seconds: u64,
    }

    impl Args {
        fn parse() -> Result<Self, Box<dyn Error>> {
            let mut kernel = None;
            let mut initramfs = None;
            let mut rom = None;
            let mut core = None;
            let mut output = None;
            let mut seed = 1_u64;
            let mut executions = 10_000_u64;
            let mut workers = 1_u32;
            let mut action_limit = 512_usize;
            let mut wall_seconds = 14_400_u64;
            let mut args = env::args_os().skip(1);
            while let Some(flag) = args.next() {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value after {}", flag.to_string_lossy()))?;
                match flag.to_string_lossy().as_ref() {
                    "--kernel" => kernel = Some(PathBuf::from(value)),
                    "--initramfs" => initramfs = Some(PathBuf::from(value)),
                    "--rom" => rom = Some(PathBuf::from(value)),
                    "--core" => core = Some(PathBuf::from(value)),
                    "--output" => output = Some(PathBuf::from(value)),
                    "--seed" => seed = parse_number("seed", value)?,
                    "--executions" => executions = parse_number("executions", value)?,
                    "--workers" => workers = parse_number("workers", value)?,
                    "--action-limit" => action_limit = parse_number("action-limit", value)?,
                    "--wall-seconds" => wall_seconds = parse_number("wall-seconds", value)?,
                    other => return Err(format!("unknown argument {other:?}").into()),
                }
            }
            Ok(Self {
                kernel: kernel.ok_or("missing --kernel")?,
                initramfs: initramfs.ok_or("missing --initramfs")?,
                rom: rom.ok_or("missing --rom")?,
                core: core.ok_or("missing --core")?,
                output: output.ok_or("missing --output")?,
                seed,
                executions,
                workers,
                action_limit,
                wall_seconds,
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

    pub fn run() -> Result<(), Box<dyn Error>> {
        let args = Args::parse()?;
        fs::create_dir_all(&args.output)?;
        let rom = fs::read(&args.rom)?;
        let kernel = fs::read(&args.kernel)?;
        let initramfs = fs::read(&args.initramfs)?;
        let game = NovaGame::new_consonance(&rom, &kernel, &initramfs);
        let config = NovaCampaignConfig {
            campaign_seed: args.seed,
            workers: args.workers,
            execution_budget: args.executions,
            action_limit: args.action_limit,
            host: "github-actions-consonance".to_owned(),
            wall_budget: Some(std::time::Duration::from_secs(args.wall_seconds)),
            archive_entry_limit: MAX_ARCHIVE_ENTRIES,
            memory_budget_mib: Some(512),
            materialize_final_artifacts: true,
            retention: RetentionPolicy::AdmitAlive,
            selector: SelectorPolicy::EnergyFrontierCheapest(RetireThresholds {
                entry: 3,
                groups: vec![6, 12, 2],
            }),
            suffix: SuffixShape::OneToSix,
            mixture: DrawMixture::AlphabetOnly,
            victory_input_path: Some(args.output.join("victory-input.json")),
        };
        let mut progress = fs::File::create(args.output.join("progress.jsonl"))?;
        let (report, checkpoint) = run_nova_campaign_checkpointed(
            &game,
            &config,
            &NovaCampaignOrigin::Genesis,
            &mut io::sink(),
            Some(&mut progress),
        )?;
        drop(checkpoint);
        let best_input = report
            .victory_input
            .as_ref()
            .unwrap_or(&report.archive.champion_input);
        fs::write(
            args.output.join("best-input.json"),
            serde_json::to_vec_pretty(best_input)?,
        )?;
        let media = render(&args, &rom, best_input)?;
        let summary = json!({
            "mode": "consonance_marketing_campaign",
            "emulator_backend": game.emulator_identity(),
            "campaign_seed": report.campaign_seed,
            "workers": report.workers,
            "execution_budget": report.execution_budget,
            "executions": report.executions_completed,
            "frames_emulated": report.frames_emulated,
            "stream_sha256": report.stream_sha256,
            "archive_entries": report.archive.entries.len(),
            "retained": report.archive.retained,
            "rejected": report.archive.rejected,
            "deaths": report.archive.deaths,
            "victories": report.victories,
            "progress": report.archive.progress_watermark,
            "milestones": report.archive.milestones,
            "first_reached": report.archive.first_reached,
            "best_input_actions": best_input.actions.len(),
            "video": media,
        });
        fs::write(
            args.output.join("campaign-summary.json"),
            serde_json::to_vec_pretty(&summary)?,
        )?;
        println!("{}", serde_json::to_string_pretty(&summary)?);
        Ok(())
    }

    fn render(
        args: &Args,
        rom: &[u8],
        input: &NovaInput,
    ) -> Result<serde_json::Value, Box<dyn Error>> {
        let core_bytes = fs::read(&args.core)?;
        let core_sha256 = format!("{:x}", Sha256::digest(&core_bytes));
        let renderer = NovaGame::new(rom, &args.core, &core_sha256);
        let mut target = renderer
            .new_target()
            .map_err(|error| -> Box<dyn Error> { error.into() })?;
        let video_path = args.output.join("best.rgb");
        let audio_path = args.output.join("best.s16le");
        let mut video_output = BufWriter::new(fs::File::create(&video_path)?);
        let mut audio_output = BufWriter::new(fs::File::create(&audio_path)?);
        let video = target.render_input(input, 180, &mut video_output, &mut audio_output)?;
        drop(video_output);
        drop(audio_output);
        let mp4_path = args.output.join("best.mp4");
        let geometry = format!("{}x{}", video.width, video.height);
        let sample_rate = video.audio_sample_rate.to_string();
        let channels = video.audio_channels.to_string();
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
        let result = json!({
            "metadata": video,
            "audio_pcm_sha256": format!("{:x}", Sha256::digest(fs::read(&audio_path)?)),
            "mp4_sha256": format!("{:x}", Sha256::digest(fs::read(&mp4_path)?)),
        });
        fs::write(
            args.output.join("video.json"),
            serde_json::to_vec_pretty(&result)?,
        )?;
        fs::remove_file(video_path)?;
        fs::remove_file(audio_path)?;
        Ok(result)
    }
}
