// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use machine::quicknes::VideoFrame;
use nes_workload::{
    film::{FPS, Film},
    metroid::target::{
        ButtonChord, GenesisDepth, MetroidInput, MetroidTarget, MetroidTerminalPolicy,
    },
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

struct Played {
    applied: usize,
    intervened: bool,
}

trait Playback {
    type Action;
    fn stopped(&self) -> bool;
    fn apply(&mut self, action: &Self::Action) -> Result<(), Box<dyn Error>>;
    fn set_resources(&mut self, health: u16, missiles: u8) -> Result<(), Box<dyn Error>>;
}

fn play<P: Playback>(
    playback: &mut P,
    actions: &[P::Action],
    intervention: Option<&Intervention>,
) -> Result<Played, Box<dyn Error>> {
    let mut played = Played {
        applied: 0,
        intervened: false,
    };
    for action in actions {
        if playback.stopped() {
            break;
        }
        playback.apply(action)?;
        played.applied += 1;
        if let Some(point) = intervention
            && point.after == played.applied
        {
            playback.set_resources(point.health, point.missiles)?;
            played.intervened = true;
        }
    }
    Ok(played)
}

struct Filming {
    film: Option<Film>,
    video: PathBuf,
    target: MetroidTarget,
    applied: usize,
    recorded: usize,
}

impl Filming {
    fn record(&mut self, frames: Vec<VideoFrame>) -> Result<(), Box<dyn Error>> {
        let film = match &mut self.film {
            Some(film) => film,
            None => {
                let Some(first) = frames.first() else {
                    return Ok(());
                };
                self.film
                    .insert(Film::start(&self.video, first.width, first.height)?)
            }
        };
        film.write(&frames, &self.target.drain_audio())
    }
}

impl Playback for Filming {
    type Action = ButtonChord;

    fn stopped(&self) -> bool {
        self.target.is_dead() || self.target.is_victory() || self.target.exit_kind() != ExitKind::Ok
    }

    fn apply(&mut self, action: &ButtonChord) -> Result<(), Box<dyn Error>> {
        self.target.apply(action);
        self.applied += 1;
        let frames = self.target.drain_frames();
        self.record(frames)?;
        if self.applied.is_multiple_of(250) {
            eprintln!(
                "action {}/{} frames={}",
                self.applied,
                self.recorded,
                self.film.as_ref().map_or(0, Film::frames)
            );
        }
        Ok(())
    }

    fn set_resources(&mut self, health: u16, missiles: u8) -> Result<(), Box<dyn Error>> {
        self.target.diagnostic_set_resources(health, missiles)
    }
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

    let opening = target.drain_frames();
    let mut filming = Filming {
        film: None,
        video: video.clone(),
        target,
        applied: 0,
        recorded: input.actions.len(),
    };
    filming.record(opening)?;
    let played = play(&mut filming, &input.actions, intervention.as_ref())?;
    let Filming { film, target, .. } = filming;
    let applied = played.applied;
    let intervened = played.intervened;
    let film = film.ok_or("the run captured no video")?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder {
        events: Vec<String>,
        stop_after: Option<usize>,
        applied: usize,
    }

    impl Playback for Recorder {
        type Action = u8;

        fn stopped(&self) -> bool {
            self.stop_after == Some(self.applied)
        }

        fn apply(&mut self, action: &u8) -> Result<(), Box<dyn Error>> {
            self.applied += 1;
            self.events.push(format!("apply {action}"));
            Ok(())
        }

        fn set_resources(&mut self, health: u16, missiles: u8) -> Result<(), Box<dyn Error>> {
            self.events.push(format!("set {health},{missiles}"));
            Ok(())
        }
    }

    #[test]
    fn an_intervention_at_the_first_action_applies_after_it() {
        let mut recorder = Recorder::default();
        let point = parse_intervention("30,5@1").unwrap();
        let played = play(&mut recorder, &[7, 8, 9], Some(&point)).unwrap();
        assert!(played.intervened);
        assert_eq!(played.applied, 3);
        assert_eq!(
            recorder.events,
            ["apply 7", "set 30,5", "apply 8", "apply 9"]
        );
    }

    #[test]
    fn a_stopped_run_ends_before_a_later_intervention() {
        let mut recorder = Recorder {
            stop_after: Some(1),
            ..Recorder::default()
        };
        let point = parse_intervention("30,5@2").unwrap();
        let played = play(&mut recorder, &[7, 8, 9], Some(&point)).unwrap();
        assert!(!played.intervened);
        assert_eq!(played.applied, 1);
        assert_eq!(recorder.events, ["apply 7"]);
    }
}
