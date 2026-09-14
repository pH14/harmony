// SPDX-License-Identifier: AGPL-3.0-or-later

//! Render a recorded Metroid input as an H.264 MP4 with game audio.
//!
//! The replay drives the same [`MetroidTarget`] the searcher drives, so the
//! film is the recorded run rather than a re-derivation of it. The target is
//! built over a capture-enabled QuickNES core, and each applied chord's frames
//! and samples are drained straight into FFmpeg.
//!
//! `--set-resources N,M` after a given action count repeats a diagnostic
//! resource intervention, so an input searched from an intervened root plays
//! back as the searcher saw it.

use std::{
    env,
    error::Error,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
};

use machine::quicknes::VideoFrame;
use nes_workload::{
    metroid::target::{MetroidInput, MetroidTarget, MetroidTerminalPolicy},
    target::{ExitKind, Target},
};
use sha2::{Digest, Sha256};

const FPS: u64 = 60;
const OUTPUT_WIDTH: u32 = 768;
const OUTPUT_HEIGHT: u32 = 720;
const SPEED_FACTOR: u32 = 4;

const USAGE: &str = "usage: metroid-film <input.json> <output.mp4> [--set-resources HEALTH,MISSILES@ACTIONS] \
     [--terminal-policy IDENTIFIER]";

struct Intervention {
    health: u16,
    missiles: u8,
    after: usize,
}

fn parse_intervention(value: &str) -> Result<Intervention, Box<dyn Error>> {
    let (resources, after) = value.split_once('@').ok_or(USAGE)?;
    let (health, missiles) = resources.split_once(',').ok_or(USAGE)?;
    Ok(Intervention {
        health: health.parse()?,
        missiles: missiles.parse()?,
        after: after.parse()?,
    })
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let input_path = PathBuf::from(args.next().ok_or(USAGE)?);
    let video = PathBuf::from(args.next().ok_or(USAGE)?);
    let mut intervention = None;
    // A tape carries no policy header, and the recorded tapes this replays
    // predate the BCD-borrow predicate, so replaying under it would stop a
    // historical tape early. A newer tape names its own policy.
    let mut terminal_policy = MetroidTerminalPolicy::Legacy;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--set-resources" => {
                intervention = Some(parse_intervention(&args.next().ok_or(USAGE)?)?);
            }
            "--terminal-policy" => {
                terminal_policy = MetroidTerminalPolicy::parse(&args.next().ok_or(USAGE)?)?;
            }
            _ => return Err(USAGE.into()),
        }
    }
    if let Some(parent) = video.parent() {
        fs::create_dir_all(parent)?;
    }

    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_METROID_ROM")
            .ok_or("HARMONY_METROID_ROM must name the external Metroid ROM")?,
    ))?;
    let core_path = PathBuf::from(
        env::var_os("HARMONY_QUICKNES_CORE")
            .ok_or("HARMONY_QUICKNES_CORE must name the pinned libretro core")?,
    );
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core_path)?));
    let input: MetroidInput = serde_json::from_slice(&fs::read(&input_path)?)?;

    // Checked before any output file exists, so a rejected point leaves none.
    if let Some(point) = &intervention
        && !(1..=input.actions.len()).contains(&point.after)
    {
        return Err(format!(
            "the intervention point {} is outside the tape's {} actions",
            point.after,
            input.actions.len()
        )
        .into());
    }

    let mut target = MetroidTarget::from_rom_bytes_capturing(&rom, &core_path, &core_sha256)?
        .with_terminal_policy(terminal_policy);

    // The boot walk to gameplay genesis has already been emulated, so the film
    // opens on the title screen exactly as the machine saw it.
    let opening = target.drain_frames();
    let first = opening.first().ok_or("the boot walk captured no video")?;
    let (width, height) = (first.width, first.height);

    // FFmpeg refuses an in-place mux, so the streaming pass writes a silent
    // video and a second pass folds the audio track into the final file.
    let video_only = video.with_extension("silent.mp4");
    let audio_raw = video.with_extension("s16le");
    let mut encoder = spawn_encoder(width, height, &video_only)?;
    let mut film = FilmWriter::new(
        encoder.stdin.take().ok_or("FFmpeg has no input pipe")?,
        File::create(&audio_raw)?,
        usize::try_from(width)?
            .checked_mul(usize::try_from(height)?)
            .and_then(|pixels| pixels.checked_mul(3))
            .ok_or("frame dimensions overflow")?,
    );
    film.write(&opening, &target.drain_audio())?;
    drop(opening);

    let mut applied = 0_usize;
    let mut intervened = false;
    for action in &input.actions {
        if target.is_dead() || target.is_victory() || target.exit_kind() != ExitKind::Ok {
            break;
        }
        target.apply(action);
        applied += 1;
        film.write(&target.drain_frames(), &target.drain_audio())?;
        if let Some(point) = &intervention
            && point.after == applied
        {
            target.diagnostic_set_resources(point.health, point.missiles)?;
            intervened = true;
        }
        if applied.is_multiple_of(250) {
            eprintln!(
                "action {applied}/{} frames={}",
                input.actions.len(),
                film.frames
            );
        }
    }

    let (frame_count, sample_frames) = film.finish()?;
    let status = encoder.wait()?;
    if !status.success() {
        return Err(format!("FFmpeg video pass failed with {status}").into());
    }

    if sample_frames > 0 {
        mux_audio(&video_only, &audio_raw, &video, frame_count, sample_frames)?;
        fs::remove_file(&video_only)?;
    } else {
        fs::rename(&video_only, &video)?;
    }
    fs::remove_file(&audio_raw)?;
    let fast = speed_up(&video, SPEED_FACTOR)?;

    println!(
        "{}",
        serde_json::json!({
            "video": video,
            "video_fast": fast,
            "frames": frame_count,
            "duration_seconds": frame_count as f64 / FPS as f64,
            "audio_sample_frames": sample_frames,
            "terminal_policy": terminal_policy.identifier(),
            "actions_applied": applied,
            "actions_recorded": input.actions.len(),
            "intervention_applied": intervened,
            "endpoint": target.mechanical_state(),
            "victory": target.is_victory(),
            "dead": target.is_dead(),
        })
    );
    // The run ended before the requested point, so this film is the unintervened
    // run. The report above says where it stopped.
    if intervention.is_some() && !intervened {
        return Err(format!(
            "the run ended after {applied} actions, before the intervention point"
        )
        .into());
    }
    Ok(())
}

/// Sink for the streaming pass: RGB24 frames go to FFmpeg's pipe, interleaved
/// samples to a raw file the mux pass reads back.
struct FilmWriter {
    encoder: ChildStdin,
    audio: BufWriter<File>,
    pixels: usize,
    frames: u64,
    sample_frames: u64,
}

impl FilmWriter {
    fn new(encoder: ChildStdin, audio: File, pixels: usize) -> Self {
        Self {
            encoder,
            audio: BufWriter::new(audio),
            pixels,
            frames: 0,
            sample_frames: 0,
        }
    }

    fn write(&mut self, frames: &[VideoFrame], audio: &[i16]) -> Result<(), Box<dyn Error>> {
        for frame in frames {
            if frame.rgb24.len() != self.pixels {
                return Err("QuickNES changed its frame geometry mid-replay".into());
            }
            self.encoder.write_all(&frame.rgb24)?;
            self.frames = self.frames.saturating_add(1);
        }
        for sample in audio {
            self.audio.write_all(&sample.to_le_bytes())?;
        }
        self.sample_frames = self.sample_frames.saturating_add(audio.len() as u64 / 2);
        Ok(())
    }

    /// Close both sinks and report the frame and stereo-sample-frame counts.
    fn finish(mut self) -> Result<(u64, u64), Box<dyn Error>> {
        self.audio.flush()?;
        Ok((self.frames, self.sample_frames))
    }
}

fn spawn_encoder(width: u32, height: u32, output: &Path) -> Result<Child, Box<dyn Error>> {
    Ok(Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pixel_format",
            "rgb24",
            "-video_size",
            &format!("{width}x{height}"),
            "-framerate",
            "60",
            "-i",
            "-",
            "-an",
            "-vf",
            &format!("scale={OUTPUT_WIDTH}:{OUTPUT_HEIGHT}:flags=neighbor"),
            "-c:v",
            "libx264",
            "-preset",
            "slow",
            "-crf",
            "18",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
        ])
        .arg(output)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()?)
}

/// Mux the raw audio track into the silent video.
///
/// QuickNES mixes at 48 kHz against the NTSC frame cadence (~60.10 Hz) while
/// the video is timestamped at exactly 60 fps, so declaring 48 kHz would leave
/// the audio short over a long film. Declaring the rate that spreads the
/// recorded samples over the video's duration keeps the tracks aligned end to
/// end, at an inaudible pitch shift.
fn mux_audio(
    video_only: &Path,
    audio_raw: &Path,
    video: &Path,
    frame_count: u64,
    sample_frames: u64,
) -> Result<(), Box<dyn Error>> {
    if frame_count == 0 {
        return Err("cannot mux audio into a zero-frame video".into());
    }
    let declared_rate = sample_frames.saturating_mul(FPS) / frame_count;
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(video_only)
        .args([
            "-f",
            "s16le",
            "-ar",
            &declared_rate.to_string(),
            "-ac",
            "2",
            "-i",
        ])
        .arg(audio_raw)
        .args([
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-movflags",
            "+faststart",
        ])
        .arg(video)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        return Err(format!("FFmpeg audio mux failed with {status}").into());
    }
    Ok(())
}

/// Write a `factor`-times-faster copy beside the film as `<stem>-<factor>x.mp4`.
fn speed_up(video: &Path, factor: u32) -> Result<PathBuf, Box<dyn Error>> {
    let stem = video
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or("output video has no usable file stem")?;
    let fast = video.with_file_name(format!("{stem}-{factor}x.mp4"));
    let mut tempo = String::new();
    let mut remaining = factor;
    while remaining > 1 {
        if !tempo.is_empty() {
            tempo.push(',');
        }
        tempo.push_str("atempo=2.0");
        remaining /= 2;
    }
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(video)
        .args([
            "-filter_complex",
            &format!(
                "[0:v]setpts={}*PTS[v];[0:a]{tempo}[a]",
                1.0 / f64::from(factor)
            ),
            "-map",
            "[v]",
            "-map",
            "[a]",
            "-c:v",
            "libx264",
            "-preset",
            "slow",
            "-crf",
            "18",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-movflags",
            "+faststart",
        ])
        .arg(&fast)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        return Err(format!("FFmpeg speed pass failed with {status}").into());
    }
    Ok(fast)
}
