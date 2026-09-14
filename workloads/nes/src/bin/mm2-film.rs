// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use nes_workload::{
    film::{FPS, Film},
    mm2::target::{Mm2Input, Mm2Stage, Mm2Target},
    target::{ExitKind, Target},
};
use sha2::{Digest, Sha256};

const USAGE: &str = "usage: mm2-film <stage> <chain-prefix.json> <input.json> <output.mp4>";

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let stage = Mm2Stage::parse(&args.next().ok_or(USAGE)?)?;
    let prefix_path = PathBuf::from(args.next().ok_or(USAGE)?);
    let input_path = PathBuf::from(args.next().ok_or(USAGE)?);
    let video = PathBuf::from(args.next().ok_or(USAGE)?);
    if args.next().is_some() {
        return Err(USAGE.into());
    }
    if let Some(parent) = video.parent() {
        fs::create_dir_all(parent)?;
    }

    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_MM2_ROM").ok_or("HARMONY_MM2_ROM must name the external ROM")?,
    ))?;
    let core_path = PathBuf::from(
        env::var_os("HARMONY_QUICKNES_CORE")
            .ok_or("HARMONY_QUICKNES_CORE must name the pinned libretro core")?,
    );
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core_path)?));
    let prefix: Mm2Input = serde_json::from_slice(&fs::read(&prefix_path)?)?;
    let input: Mm2Input = serde_json::from_slice(&fs::read(&input_path)?)?;

    let mut target =
        Mm2Target::from_rom_bytes_after(&rom, &core_path, &core_sha256, &prefix.actions, stage)?;
    target.start_capturing();

    let first_action = input.actions.first().ok_or("the input has no actions")?;
    target.apply(first_action);
    let opening = target.drain_frames();
    let first = opening
        .first()
        .ok_or("the first action captured no video")?;
    let (width, height) = (first.width, first.height);

    let mut film = Film::start(&video, width, height)?;
    film.write(&opening, &target.drain_audio())?;
    drop(opening);

    let mut applied = 1_usize;
    for action in &input.actions[1..] {
        if target.is_dead() || target.defeated_a_boss() || target.exit_kind() != ExitKind::Ok {
            break;
        }
        target.apply(action);
        applied += 1;
        film.write(&target.drain_frames(), &target.drain_audio())?;
        if applied.is_multiple_of(250) {
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
            "endpoint": target.mechanical_state(),
            "weapon_energies": target.diagnostic_weapon_energies()?,
            "bosses_beaten": target.mechanical_state().bosses_beaten(),
            "dead": target.is_dead(),
        })
    );
    Ok(())
}
