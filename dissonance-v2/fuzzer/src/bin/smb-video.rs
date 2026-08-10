// SPDX-License-Identifier: AGPL-3.0-or-later

//! Stream every emulated frame of a recorded SMB film input into an H.264 MP4.

use std::{
    env,
    error::Error,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use fuzzer::phase4b::{SmbInput, SmbTarget};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const WIDTH: usize = 256;
const HEIGHT: usize = 240;
const FPS: u64 = 60;

#[derive(Debug, Deserialize)]
struct FilmManifest {
    source_report: PathBuf,
    milestone: String,
    rom_sha256: String,
    input: SmbInput,
}

#[derive(Debug, Serialize)]
struct VideoManifest {
    source_manifest: PathBuf,
    source_report: PathBuf,
    milestone: String,
    rom_sha256: String,
    video: PathBuf,
    codec: &'static str,
    width: usize,
    height: usize,
    fps: u64,
    frame_count: u64,
    duration_millis: u64,
    audio: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let source_manifest = PathBuf::from(
        args.next()
            .ok_or("usage: smb-video <film-manifest.json> <output.mp4> <video-manifest.json>")?,
    );
    let video = PathBuf::from(args.next().ok_or("missing output video")?);
    let video_manifest = PathBuf::from(args.next().ok_or("missing video manifest")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }

    let film: FilmManifest = serde_json::from_slice(&fs::read(&source_manifest)?)?;
    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    let rom = fs::read(rom_path)?;
    let rom_sha256 = format!("{:x}", Sha256::digest(&rom));
    if rom_sha256 != film.rom_sha256 {
        return Err("ROM SHA-256 does not match the film manifest".into());
    }
    create_parent(&video)?;
    create_parent(&video_manifest)?;

    let mut ffmpeg = Command::new("ffmpeg")
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
            "256x240",
            "-framerate",
            "60",
            "-i",
            "-",
            "-an",
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
        .arg(&video)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()?;
    let mut encoder = ffmpeg.stdin.take().ok_or("FFmpeg has no input pipe")?;
    let mut target = SmbTarget::from_smb_rom_bytes(&rom)?;
    let mut frame_count = 0_u64;
    write_frame(&mut encoder, &mut target, &mut frame_count)?;
    for action in &film.input.actions {
        for _ in 0..action.bounded_hold_frames() {
            target.clock_frame_for_film(action.buttons)?;
            write_frame(&mut encoder, &mut target, &mut frame_count)?;
        }
        target.release_buttons_for_film();
    }
    drop(encoder);
    let status = ffmpeg.wait()?;
    if !status.success() {
        return Err(format!("FFmpeg failed with {status}").into());
    }

    let manifest = VideoManifest {
        source_manifest,
        source_report: film.source_report,
        milestone: film.milestone,
        rom_sha256,
        video,
        codec: "H.264",
        width: WIDTH,
        height: HEIGHT,
        fps: FPS,
        frame_count,
        duration_millis: frame_count.saturating_mul(1_000) / FPS,
        audio: false,
    };
    fs::write(&video_manifest, serde_json::to_vec_pretty(&manifest)?)?;
    println!("{}", serde_json::to_string_pretty(&manifest)?);
    Ok(())
}

fn create_parent(path: &Path) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn write_frame(
    encoder: &mut impl Write,
    target: &mut SmbTarget,
    frame_count: &mut u64,
) -> Result<(), Box<dyn Error>> {
    let rgba = target.frame_rgba();
    let expected = WIDTH
        .checked_mul(HEIGHT)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or("video dimensions overflow")?;
    if rgba.len() != expected {
        return Err("unexpected TetaNES RGBA frame length".into());
    }
    encoder.write_all(&rgba)?;
    *frame_count = frame_count.saturating_add(1);
    Ok(())
}
