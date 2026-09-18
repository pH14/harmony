// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use nes_workload::{
    film::{FPS, Film},
    metroid::target::{GenesisDepth, MetroidInput, MetroidTarget, MetroidTerminalPolicy},
    target::{ExitKind, Target},
};
use sha2::{Digest, Sha256};

const USAGE: &str = "usage: metroid-film <input.json> <output.mp4> \
     [--root ROOT_INPUT.json] [--set-resources HEALTH,MISSILES@ACTIONS] \
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
    let mut root_input = None;
    let mut terminal_policy = MetroidTerminalPolicy::Legacy;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--root" => {
                root_input = Some(PathBuf::from(args.next().ok_or(USAGE)?));
            }
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

    let mut target = match &root_input {
        None => MetroidTarget::from_rom_bytes_capturing(&rom, &core_path, &core_sha256)?,
        Some(path) => {
            let root: MetroidInput = serde_json::from_slice(&fs::read(path)?)?;
            if root.actions.is_empty() {
                return Err("root input carries no actions".into());
            }
            let mut prefix =
                MetroidTarget::from_rom_bytes_headless(&rom, &core_path, &core_sha256)?
                    .genesis_prefix()
                    .to_vec();
            prefix.extend(root.actions.iter().copied());
            let mut rooted = MetroidTarget::from_rom_bytes_rooted(
                &rom,
                &core_path,
                &core_sha256,
                &prefix,
                GenesisDepth::Rooted,
            )?;
            rooted.start_capturing();
            rooted
        }
    }
    .with_terminal_policy(terminal_policy);

    let mut opening = target.drain_frames();
    let mut consumed = 0_usize;
    if opening.is_empty() {
        let first_action = input.actions.first().ok_or("the input has no actions")?;
        target.apply(first_action);
        consumed = 1;
        opening = target.drain_frames();
    }
    let first = opening.first().ok_or("the run-up captured no video")?;
    let (width, height) = (first.width, first.height);

    let mut film = Film::start(&video, width, height)?;
    film.write(&opening, &target.drain_audio())?;
    drop(opening);

    let mut applied = consumed;
    let mut intervened = false;
    for action in &input.actions[consumed..] {
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
            "terminal_policy": terminal_policy.identifier(),
            "actions_applied": applied,
            "actions_recorded": input.actions.len(),
            "root_input": root_input.as_ref().map(|path| path.display().to_string()),
            "intervention_applied": intervened,
            "endpoint": target.mechanical_state(),
            "victory": target.is_victory(),
            "dead": target.is_dead(),
        })
    );
    if intervention.is_some() && !intervened {
        return Err(format!(
            "the run ended after {applied} actions, before the intervention point"
        )
        .into());
    }
    Ok(())
}
