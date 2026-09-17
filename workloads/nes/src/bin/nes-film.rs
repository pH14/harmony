// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    error::Error,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use nes_workload::{
    film::{Capture, FilmMetadata, Filmable, Reel},
    nova::{
        campaign::{NovaCampaignRun, NovaGame},
        target::NovaLevel,
    },
    search::archive::Input,
    stb::{
        campaign::{StbCampaignRun, StbGame},
        target::StbAi,
    },
    witness::replay_witness,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const DEFAULT_TAIL_FRAMES: u32 = 180;
const DEFAULT_MAX_FRAMES: u64 = 72_000;

struct Args {
    game: String,
    rom: PathBuf,
    core: PathBuf,
    rom_sha256: String,
    core_sha256: String,
    level: Option<u8>,
    ai: Option<String>,
    whole_game: bool,
    input: PathBuf,
    evidence: PathBuf,
    out: PathBuf,
    tail_frames: u32,
    max_frames: u64,
    recorded_backend: String,
}

impl Args {
    fn parse_from<I: IntoIterator<Item = OsString>>(values: I) -> Result<Self> {
        let mut game = None;
        let mut rom = None;
        let mut core = None;
        let mut rom_sha256 = None;
        let mut core_sha256 = None;
        let mut level = None;
        let mut ai = None;
        let mut whole_game = false;
        let mut input = None;
        let mut evidence = None;
        let mut out = None;
        let mut tail_frames = DEFAULT_TAIL_FRAMES;
        let mut max_frames = DEFAULT_MAX_FRAMES;
        let mut recorded_backend = "native".to_owned();
        let mut args = values.into_iter();
        while let Some(flag) = args.next() {
            if flag == "--whole-game" {
                whole_game = true;
                continue;
            }
            let value = args
                .next()
                .ok_or_else(|| format!("missing value after {}", flag.to_string_lossy()))?;
            let text = || -> Result<String> {
                value
                    .clone()
                    .into_string()
                    .map_err(|_| format!("{} is not UTF-8", flag.to_string_lossy()).into())
            };
            match flag.to_string_lossy().as_ref() {
                "--game" => game = Some(text()?),
                "--rom" => rom = Some(PathBuf::from(&value)),
                "--core" => core = Some(PathBuf::from(&value)),
                "--rom-sha256" => rom_sha256 = Some(text()?),
                "--core-sha256" => core_sha256 = Some(text()?),
                "--level" => level = Some(text()?.parse()?),
                "--ai" => ai = Some(text()?),
                "--input" => input = Some(PathBuf::from(&value)),
                "--evidence" => evidence = Some(PathBuf::from(&value)),
                "--out" => out = Some(PathBuf::from(&value)),
                "--tail-frames" => tail_frames = text()?.parse()?,
                "--max-frames" => max_frames = text()?.replace('_', "").parse()?,
                "--recorded-backend" => recorded_backend = text()?,
                other => return Err(format!("unknown argument {other:?}").into()),
            }
        }
        let args = Self {
            game: game.ok_or("missing --game")?,
            rom: rom.ok_or("missing --rom")?,
            core: core.ok_or("missing --core")?,
            rom_sha256: rom_sha256.ok_or("missing --rom-sha256")?,
            core_sha256: core_sha256.ok_or("missing --core-sha256")?,
            level,
            ai,
            whole_game,
            input: input.ok_or("missing --input")?,
            evidence: evidence.ok_or("missing --evidence")?,
            out: out.ok_or("missing --out")?,
            tail_frames,
            max_frames,
            recorded_backend,
        };
        if u64::from(args.tail_frames) >= args.max_frames {
            return Err("the frame budget must leave room for more than the tail".into());
        }
        if !matches!(args.recorded_backend.as_str(), "native" | "consonance") {
            return Err("the recording backend is native or consonance".into());
        }
        Ok(args)
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn produce<G: Filmable>(game: G, run: G::Run, args: &Args, identity: Value) -> Result<()> {
    let input_bytes = fs::read(&args.input)?;
    let input: Input<G::Action> = serde_json::from_slice(&input_bytes)?;
    if input.actions.is_empty() {
        return Err("the recorded run produced no renderable input".into());
    }
    let recorded: Value = serde_json::from_slice(&fs::read(&args.evidence)?)?;
    let native = args.recorded_backend == "native";
    let replayed = replay_witness(&game, &run, &input)?;
    if native {
        let expected = recorded
            .get("witness")
            .ok_or("the recorded result carries no witness evidence")?;
        if &replayed != expected {
            return Err(format!(
                "the recorded input no longer reaches its recorded endpoint\nrecorded: {expected}\nreplayed: {replayed}"
            )
            .into());
        }
    }

    let input_frames = game.input_frames(&input);
    let requested_frames = input_frames + u64::from(args.tail_frames);
    let skip_frames = requested_frames.saturating_sub(args.max_frames);

    fs::create_dir_all(&args.out)?;
    let mp4 = args.out.join("witness.mp4");
    let mut reel = Reel::start(
        &mp4,
        nes_workload::film::NES_WIDTH,
        nes_workload::film::NES_HEIGHT,
    )?;
    let metadata: FilmMetadata = {
        let (video, audio) = reel.sinks()?;
        game.film(&input, args.tail_frames, skip_frames, video, audio)?
    };
    if metadata.skipped_frames != skip_frames {
        return Err("the renderer did not honour the declared frame budget".into());
    }
    if metadata.frames != requested_frames - skip_frames {
        return Err(format!(
            "the capture rendered {} of {} frames",
            metadata.frames,
            requested_frames - skip_frames
        )
        .into());
    }
    let endpoint = game.headless_endpoint(&input)?;
    if let Some(recorded_endpoint) = recorded.get("endpoint")
        && recorded_endpoint != &endpoint
    {
        return Err(format!(
            "the {} run recorded a different endpoint for this input\nrecorded: {recorded_endpoint}\nnative: {endpoint}",
            args.recorded_backend
        )
        .into());
    }
    if !native && recorded.get("endpoint").is_none() {
        return Err("a cross-backend film needs the recorded endpoint to bridge to".into());
    }
    if metadata.input_endpoint != endpoint {
        return Err(format!(
            "the captured replay reached a different endpoint\nheadless: {endpoint}\ncaptured: {}",
            metadata.input_endpoint
        )
        .into());
    }
    let capture: Capture = reel.finish(&metadata)?;

    write_json(
        &args.out.join("film.json"),
        &json!({
            "format": "nes-film-v1",
            "identity": identity,
            "rom_sha256": args.rom_sha256,
            "core_sha256": args.core_sha256,
            "input_sha256": sha256(&input_bytes),
            "input_actions": input.actions.len(),
            "input_frames": input_frames,
            "solved": recorded.get("solved").cloned().unwrap_or(Value::Null),
            "victory": replayed.get("victory").cloned().unwrap_or(Value::Null),
            "evidence_verified": native,
            "endpoint_bridged": !native,
            "endpoint_verified": true,
            "endpoint": endpoint,
            "recorded_backend": args.recorded_backend,
            "provenance": if native {
                "native QuickNES replay of the input this run recorded"
            } else {
                "native QuickNES replay of the input the Consonance whole-VM search recorded; the endpoint matches the one that search recorded, and nothing was captured inside the guest"
            },
            "clip": {
                "policy": if skip_frames == 0 { "whole input" } else { "trailing window ending at the recorded endpoint" },
                "max_frames": args.max_frames,
                "tail_frames": args.tail_frames,
                "requested_frames": requested_frames,
                "skipped_frames": skip_frames,
            },
            "video": metadata,
            "capture": capture,
        }),
    )?;
    println!("{}", serde_json::to_string(&capture.mp4)?);
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse_from(std::env::args_os().skip(1))?;
    let rom = fs::read(&args.rom)?;
    let core = fs::read(&args.core)?;
    if sha256(&rom) != args.rom_sha256 || sha256(&core) != args.core_sha256 {
        return Err("ROM/core checksum mismatch".into());
    }
    let identity = json!({
        "game": args.game,
        "level": args.level,
        "ai": args.ai,
        "whole_game": args.whole_game,
    });
    match args.game.as_str() {
        "nova" => {
            if args.ai.is_some() {
                return Err("Nova takes no ai option".into());
            }
            let game = NovaGame::new_at_level(
                &rom,
                &args.core,
                &args.core_sha256,
                NovaLevel::from_number(args.level.unwrap_or(1))?,
            );
            let game = if args.whole_game {
                game.with_whole_game()
            } else {
                game
            };
            produce(game, NovaCampaignRun, &args, identity)
        }
        "stb" => {
            if args.level.is_some() || args.whole_game {
                return Err("STB takes only an ai option".into());
            }
            let ai = match args.ai.as_deref().unwrap_or("hard") {
                "easy" => StbAi::Easy,
                "fair" => StbAi::Fair,
                "hard" => StbAi::Hard,
                _ => return Err("unknown STB difficulty".into()),
            };
            produce(
                StbGame::with_ai(&rom, &args.core, &args.core_sha256, ai),
                StbCampaignRun,
                &args,
                identity,
            )
        }
        _ => Err("nes-film renders the Nova and STB benchmark scenarios".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::{Args, DEFAULT_MAX_FRAMES, DEFAULT_TAIL_FRAMES, OsString};

    fn required(extra: &[&str]) -> Vec<OsString> {
        let mut args = [
            "--game",
            "nova",
            "--rom",
            "rom",
            "--core",
            "core",
            "--rom-sha256",
            "a",
            "--core-sha256",
            "b",
            "--input",
            "witness-input.json",
            "--evidence",
            "result.json",
            "--out",
            "film",
        ]
        .into_iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
        args.extend(extra.iter().map(OsString::from));
        args
    }

    #[test]
    fn the_frame_budget_defaults_to_a_reviewable_ceiling() {
        let args = Args::parse_from(required(&[])).expect("arguments parse");
        assert_eq!(args.max_frames, DEFAULT_MAX_FRAMES);
        assert_eq!(args.tail_frames, DEFAULT_TAIL_FRAMES);
        assert!(!args.whole_game);
    }

    #[test]
    fn a_budget_that_cannot_hold_the_tail_is_rejected() {
        assert!(Args::parse_from(required(&["--max-frames", "180"])).is_err());
        assert!(Args::parse_from(required(&["--max-frames", "120"])).is_err());
        assert!(Args::parse_from(required(&["--max-frames", "181"])).is_ok());
    }

    #[test]
    fn scenario_identity_is_explicit() {
        let args = Args::parse_from(required(&["--level", "9", "--whole-game"])).unwrap();
        assert_eq!(args.level, Some(9));
        assert!(args.whole_game);
        assert_eq!(
            Args::parse_from(required(&["--ai", "easy"]))
                .unwrap()
                .ai
                .as_deref(),
            Some("easy")
        );
    }

    #[test]
    fn only_a_registered_recording_backend_is_accepted() {
        assert_eq!(
            Args::parse_from(required(&[])).unwrap().recorded_backend,
            "native"
        );
        assert_eq!(
            Args::parse_from(required(&["--recorded-backend", "consonance"]))
                .unwrap()
                .recorded_backend,
            "consonance"
        );
        assert!(Args::parse_from(required(&["--recorded-backend", "vm"])).is_err());
    }

    #[test]
    fn unknown_and_incomplete_arguments_are_rejected() {
        assert!(Args::parse_from(required(&["--unknown", "value"])).is_err());
        assert!(Args::parse_from(vec![OsString::from("--game"), OsString::from("nova")]).is_err());
    }
}
