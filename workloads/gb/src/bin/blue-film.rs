// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use blue_workload::{
    archive::BlueInput,
    film::{FPS, Film},
    fixtures::setup_prefix,
    target::BlueTarget,
};
use searcher::target::{ExitKind, Target};
use sha2::{Digest, Sha256};

const USAGE: &str = "usage: blue-film <input.json> <output.mp4>";
const PROGRESS_EVERY: usize = 50;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let input_path = PathBuf::from(args.next().ok_or(USAGE)?);
    let video = PathBuf::from(args.next().ok_or(USAGE)?);
    if args.next().is_some() {
        return Err(USAGE.into());
    }
    if let Some(parent) = video.parent() {
        fs::create_dir_all(parent)?;
    }

    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_BLUE_ROM")
            .ok_or("HARMONY_BLUE_ROM must name the external Pokemon Blue ROM")?,
    ))?;
    let core_path = PathBuf::from(
        env::var_os("HARMONY_GAMBATTE_CORE")
            .ok_or("HARMONY_GAMBATTE_CORE must name the pinned libretro core")?,
    );
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core_path)?));
    let input: BlueInput = serde_json::from_slice(&fs::read(&input_path)?)?;

    let mut target =
        BlueTarget::from_rom_bytes_capturing(&rom, &core_path, &core_sha256, &setup_prefix()?)?;

    let opening = target.drain_frames();
    let first = opening
        .first()
        .ok_or("the setup prefix captured no video")?;
    let (width, height) = (first.width, first.height);
    let mut film = Film::start(&video, width, height)?;
    film.write(&opening, &target.drain_audio())?;
    drop(opening);

    let mut applied = 0_usize;
    for action in &input.actions {
        if target.is_dead() || target.exit_kind() != ExitKind::Ok {
            break;
        }
        target.apply(action);
        applied += 1;
        film.write(&target.drain_frames(), &target.drain_audio())?;
        if applied.is_multiple_of(PROGRESS_EVERY) {
            eprintln!(
                "action {applied}/{} frames={}",
                input.actions.len(),
                film.frames()
            );
        }
    }
    let film = film.finish()?;

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
            "endpoint": target.state(),
            "victory": target.is_victory(),
            "dead": target.is_dead(),
        })
    );
    Ok(())
}
