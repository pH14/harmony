// SPDX-License-Identifier: AGPL-3.0-or-later

//! Render a recorded Super Mario Bros input as an H.264 MP4 with game audio.
//!
//! The replay drives the same [`SmbTarget`] the searcher drives, so the film
//! is the recorded run rather than a re-derivation of it. The target is built
//! over a capture-enabled QuickNES core, and each applied chord's frames and
//! samples are drained straight into FFmpeg.

use std::{
    env,
    error::Error,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
};

use machine::quicknes::CapturedFrame;
use searcher::{
    smb::target::{SmbInput, SmbTarget},
    target::{ExitKind, Target},
};
use sha2::{Digest, Sha256};

const FPS: u64 = 60;
const OUTPUT_WIDTH: u32 = 768;
const OUTPUT_HEIGHT: u32 = 720;
const SPEED_FACTOR: u32 = 4;
const WORLD_NUMBER_OFFSET: usize = 0x075f;
const LEVEL_NUMBER_OFFSET: usize = 0x075c;
const FLAG_TASK_OFFSET: usize = 0x0746;
const OPERATING_MODE_OFFSET: usize = 0x0770;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let input_path = PathBuf::from(
        args.next()
            .ok_or("usage: smb-film <victory-input.json> <output.mp4>")?,
    );
    let video = PathBuf::from(args.next().ok_or("missing output video path")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    if let Some(parent) = video.parent() {
        fs::create_dir_all(parent)?;
    }

    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    ))?;
    let core_path = PathBuf::from(
        env::var_os("HARMONY_QUICKNES_CORE")
            .ok_or("HARMONY_QUICKNES_CORE must name the pinned libretro core")?,
    );
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core_path)?));
    let input: SmbInput = serde_json::from_slice(&fs::read(&input_path)?)?;

    let mut target = SmbTarget::from_smb_rom_bytes_capturing(&rom, &core_path, &core_sha256)?;
    target.reset();

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
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or("frame dimensions overflow")?,
    );
    film.write(&opening, &target.drain_audio())?;
    drop(opening);

    let mut applied = 0_usize;
    for action in &input.actions {
        if target.is_dead() || target.is_victory() || target.exit_kind() != ExitKind::Ok {
            break;
        }
        target.apply(action);
        applied += 1;
        film.write(&target.drain_frames(), &target.drain_audio())?;
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

    let wram = target.wram();
    println!(
        "{}",
        serde_json::json!({
            "video": video,
            "video_fast": fast,
            "frames": frame_count,
            "duration_seconds": frame_count as f64 / FPS as f64,
            "audio_sample_frames": sample_frames,
            "actions_applied": applied,
            "actions_recorded": input.actions.len(),
            "world_075f": wram[WORLD_NUMBER_OFFSET],
            "level_075c": wram[LEVEL_NUMBER_OFFSET],
            "flag_task_0746": wram[FLAG_TASK_OFFSET],
            "operating_mode_0770": wram[OPERATING_MODE_OFFSET],
            "victory": target.is_victory(),
            "dead": target.is_dead(),
        })
    );
    Ok(())
}

/// Sink for the streaming pass: RGBA frames go to FFmpeg's pipe, interleaved
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

    fn write(&mut self, frames: &[CapturedFrame], audio: &[i16]) -> Result<(), Box<dyn Error>> {
        for frame in frames {
            if frame.rgba.len() != self.pixels {
                return Err("QuickNES changed its frame geometry mid-replay".into());
            }
            self.encoder.write_all(&frame.rgba)?;
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
            "rgba",
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
