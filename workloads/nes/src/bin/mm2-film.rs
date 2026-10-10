// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use machine::quicknes::VideoFrame;
use nes_workload::{
    film::{FPS, Film},
    mm2::target::{Mm2Input, target_from_args},
    target::{ExitKind, Target},
};
use sha2::{Digest, Sha256};

const USAGE: &str = "usage: mm2-film (<stage> <chain-prefix.json> | whole-game [--root ROOT.json | --tape TAPE.json]) <input.json> <output.mp4>";

fn main() -> Result<(), Box<dyn Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();

    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_MM2_ROM").ok_or("HARMONY_MM2_ROM must name the external ROM")?,
    ))?;
    let core_path = PathBuf::from(
        env::var_os("HARMONY_QUICKNES_CORE")
            .ok_or("HARMONY_QUICKNES_CORE must name the pinned libretro core")?,
    );
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core_path)?));
    let (mut target, rest) = target_from_args(&args, &rom, &core_path, &core_sha256)?;
    let [input_path, video] = rest else {
        return Err(USAGE.into());
    };
    let video = PathBuf::from(video);
    if let Some(parent) = video.parent() {
        fs::create_dir_all(parent)?;
    }
    let input: Mm2Input = serde_json::from_slice(&fs::read(input_path)?)?;
    target.start_capturing();

    let film: SharedFilm = Arc::new(Mutex::new(None));
    let sink_film = Arc::clone(&film);
    let sink_video = video.clone();
    target.set_frame_sink(Some(Box::new(move |frames, audio| {
        write_film(&sink_film, &sink_video, frames, audio)
    })));

    let mut applied = 0_usize;
    for action in &input.actions {
        if target.is_terminal() || target.exit_kind() != ExitKind::Ok {
            break;
        }
        target.apply(action);
        applied += 1;
        let (frames, audio) = (target.drain_frames(), target.drain_audio());
        write_film(&film, &video, &frames, &audio)?;
        if applied.is_multiple_of(250) {
            eprintln!(
                "action {applied}/{} frames={}",
                input.actions.len(),
                film.lock()
                    .map_err(|_| "the film lock is poisoned")?
                    .as_ref()
                    .map_or(0, Film::frames)
            );
        }
    }

    target.set_frame_sink(None);
    let film = Arc::try_unwrap(film)
        .map_err(|_| "the film is still shared")?
        .into_inner()
        .map_err(|_| "the film lock is poisoned")?
        .ok_or("the input captured no video")?
        .finish()?;

    println!(
        "{}",
        serde_json::json!({
            "video": film.video,
            "video_fast": film.video_fast,
            "frames": film.frames,
            "duration_seconds": film.frames as f64 / FPS as f64,
            "audio_sample_frames": film.sample_frames,
            "actions_applied": applied,
            "actions_recorded": input.actions.len(),
            "endpoint": target.mechanical_state(),
            "weapon_energies": target.diagnostic_weapon_energies()?,
            "bosses_beaten": target.mechanical_state().bosses_beaten(),
            "dead": target.is_dead(),
            "failed": target.exit_kind() != ExitKind::Ok,
            "objective_reached": target.objective_reached(),
        })
    );
    Ok(())
}

type SharedFilm = Arc<Mutex<Option<Film>>>;

fn write_film(
    film: &SharedFilm,
    video: &Path,
    frames: &[VideoFrame],
    audio: &[i16],
) -> Result<(), Box<dyn Error>> {
    let mut film = film.lock().map_err(|_| "the film lock is poisoned")?;
    if film.is_none() {
        let Some(first) = frames.first() else {
            return Ok(());
        };
        *film = Some(Film::start(video, first.width, first.height)?);
    }
    film.as_mut()
        .ok_or("the film did not start")?
        .write(frames, audio)
}
