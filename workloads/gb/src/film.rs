// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    error::Error,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
};

use machine::gambatte::VideoFrame;

pub const FPS: u64 = 60;
const OUTPUT_WIDTH: u32 = 640;
const OUTPUT_HEIGHT: u32 = 576;
const SPEED_FACTOR: u32 = 4;

pub struct FilmSummary {
    pub video: PathBuf,
    pub video_fast: PathBuf,
    pub frames: u64,
    pub sample_frames: u64,
}

pub struct Film {
    encoder: Child,
    sink: FilmSink,
    video: PathBuf,
    video_only: PathBuf,
    audio_raw: PathBuf,
}

impl Film {
    pub fn start(video: &Path, width: u32, height: u32) -> Result<Self, Box<dyn Error>> {
        let video_only = video.with_extension("silent.mp4");
        let audio_raw = video.with_extension("s16le");
        let mut encoder = spawn_encoder(width, height, &video_only)?;
        let sink = FilmSink::new(
            encoder.stdin.take().ok_or("FFmpeg has no input pipe")?,
            File::create(&audio_raw)?,
            usize::try_from(width)?
                .checked_mul(usize::try_from(height)?)
                .and_then(|pixels| pixels.checked_mul(3))
                .ok_or("frame dimensions overflow")?,
        );
        Ok(Self {
            encoder,
            sink,
            video: video.to_path_buf(),
            video_only,
            audio_raw,
        })
    }

    pub fn write(&mut self, frames: &[VideoFrame], audio: &[i16]) -> Result<(), Box<dyn Error>> {
        self.sink.write(frames, audio)
    }

    #[must_use]
    pub fn frames(&self) -> u64 {
        self.sink.frames
    }

    pub fn finish(self) -> Result<FilmSummary, Box<dyn Error>> {
        let Self {
            mut encoder,
            sink,
            video,
            video_only,
            audio_raw,
        } = self;
        let (frames, sample_frames) = sink.finish()?;
        let status = encoder.wait()?;
        if !status.success() {
            return Err(format!("FFmpeg video pass failed with {status}").into());
        }
        if sample_frames > 0 {
            mux_audio(&video_only, &audio_raw, &video, frames, sample_frames)?;
            fs::remove_file(&video_only)?;
        } else {
            fs::rename(&video_only, &video)?;
        }
        fs::remove_file(&audio_raw)?;
        let video_fast = speed_up(&video, SPEED_FACTOR)?;
        Ok(FilmSummary {
            video,
            video_fast,
            frames,
            sample_frames,
        })
    }
}

struct FilmSink {
    encoder: ChildStdin,
    audio: BufWriter<File>,
    pixels: usize,
    frames: u64,
    sample_frames: u64,
}

impl FilmSink {
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
                return Err("Gambatte changed its frame geometry mid-replay".into());
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
