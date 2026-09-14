// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    error::Error,
    fs,
    io::{BufWriter, Write},
    path::Path,
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::search::{archive::Input, campaign::Workload};

pub const RENDER_TAIL_FRAMES: u32 = 180;
pub const FRAME_WIDTH: u32 = 256;
pub const FRAME_HEIGHT: u32 = 224;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FilmMetadata {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub audio_sample_rate: u32,
    pub audio_channels: u8,
    pub audio_frames: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RenderedFilm {
    pub video: FilmMetadata,
    pub tail_frames: u32,
    pub input_actions: usize,
    pub input_frames: u64,
    pub audio_pcm_sha256: String,
    pub mp4_sha256: String,
}

pub trait FilmSource: Workload {
    fn render_film(
        &self,
        input: &Input<Self::Action>,
        tail_frames: u32,
        video: &mut dyn Write,
        audio: &mut dyn Write,
    ) -> Result<FilmMetadata, Box<dyn Error>>;

    fn film_action_frames(&self, input: &Input<Self::Action>) -> u64;
}

pub fn render_witness_film<G: FilmSource>(
    game: &G,
    input: &Input<G::Action>,
    out: &Path,
    tail_frames: u32,
) -> Result<RenderedFilm, Box<dyn Error>> {
    let encoded_path = out.join("witness-video.mp4");
    let audio_path = out.join("witness.s16le");
    let mp4_path = out.join("witness.mp4");
    let mut audio_output = BufWriter::new(fs::File::create(&audio_path)?);
    let size = format!("{FRAME_WIDTH}x{FRAME_HEIGHT}");
    let mut encoder = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "rawvideo",
            "-pixel_format",
            "rgb24",
            "-video_size",
            &size,
            "-framerate",
            "60",
            "-i",
            "pipe:0",
            "-an",
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-crf",
            "18",
            "-pix_fmt",
            "yuv420p",
            "-y",
        ])
        .arg(&encoded_path)
        .stdin(Stdio::piped())
        .spawn()?;
    let mut video_output = BufWriter::new(encoder.stdin.take().ok_or("missing encoder input")?);
    let rendered = game.render_film(input, tail_frames, &mut video_output, &mut audio_output);
    let flushed = video_output.flush();
    drop(video_output);
    let status = encoder.wait()?;
    let video = rendered?;
    flushed?;
    if !status.success() {
        return Err(format!("video encoder failed with {status}").into());
    }
    if (video.width, video.height) != (FRAME_WIDTH, FRAME_HEIGHT) {
        return Err("unexpected pinned QuickNES video geometry".into());
    }
    let input_frames = game.film_action_frames(input);
    if video.frames != input_frames.saturating_add(u64::from(tail_frames)) {
        return Err("video replay did not render the full input and tail".into());
    }
    if video.audio_frames == 0 {
        return Err("video replay produced no audio".into());
    }
    audio_output.flush()?;
    drop(audio_output);
    let audio_pcm_sha256 = sha256(&fs::read(&audio_path)?);
    let sample_rate = video.audio_sample_rate.to_string();
    let channels = video.audio_channels.to_string();
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&encoded_path)
        .args(["-f", "s16le", "-ar", &sample_rate, "-ac", &channels, "-i"])
        .arg(&audio_path)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c:v",
            "copy",
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
        return Err(format!("audio mux failed with {status}").into());
    }
    fs::remove_file(encoded_path)?;
    fs::remove_file(audio_path)?;
    let film = RenderedFilm {
        video,
        tail_frames,
        input_actions: input.actions.len(),
        input_frames,
        audio_pcm_sha256,
        mp4_sha256: sha256(&fs::read(&mp4_path)?),
    };
    fs::write(out.join("film.json"), serde_json::to_vec_pretty(&film)?)?;
    Ok(film)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_film_records_the_tape_it_rendered() {
        let film = RenderedFilm {
            video: FilmMetadata {
                width: FRAME_WIDTH,
                height: FRAME_HEIGHT,
                frames: 300,
                audio_sample_rate: 48_000,
                audio_channels: 2,
                audio_frames: 240_000,
            },
            tail_frames: RENDER_TAIL_FRAMES,
            input_actions: 7,
            input_frames: 120,
            audio_pcm_sha256: "a".repeat(64),
            mp4_sha256: "b".repeat(64),
        };
        let round_trip: RenderedFilm =
            serde_json::from_value(serde_json::to_value(&film).expect("encode"))
                .expect("film metadata round trips");
        assert_eq!(round_trip, film);
        assert_eq!(
            film.video.frames,
            film.input_frames + u64::from(film.tail_frames),
            "a film is exactly its tape plus the neutral tail"
        );
    }
}
