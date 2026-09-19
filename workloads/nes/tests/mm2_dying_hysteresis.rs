// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use nes_workload::{
    mm2::target::{ButtonChord, Mm2Input, Mm2Observations, Mm2Stage, Mm2Target},
    target::{ExitKind, Target},
};
use sha2::{Digest, Sha256};

type TestResult = Result<(), Box<dyn Error>>;

struct Harness {
    rom: Vec<u8>,
    core: PathBuf,
    core_sha256: String,
    prefix: Vec<ButtonChord>,
    root: Vec<ButtonChord>,
    stage: Mm2Stage,
}

impl Harness {
    fn target(&self) -> Result<Mm2Target, Box<dyn Error>> {
        let mut target = Mm2Target::from_rom_bytes_after(
            &self.rom,
            &self.core,
            &self.core_sha256,
            &self.prefix,
            self.stage,
        )?;
        if !self.root.is_empty() {
            target.advance_genesis(&self.root)?;
        }
        Ok(target)
    }
}

struct ReplayReport {
    first_dead_frame: Option<u64>,
    final_dead: bool,
    saw_short_flash: bool,
    short_flash_dead: bool,
}

fn env_path(names: &[&str]) -> Result<PathBuf, Box<dyn Error>> {
    names
        .iter()
        .find_map(|name| env::var_os(name).map(PathBuf::from))
        .ok_or_else(|| format!("one of {} must name a fixture", names.join(", ")).into())
}

fn optional_env_path(names: &[&str]) -> Option<PathBuf> {
    names
        .iter()
        .find_map(|name| env::var_os(name).map(PathBuf::from))
}

fn read_input(path: &PathBuf) -> Result<Mm2Input, Box<dyn Error>> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn load_harness() -> Result<(Harness, Mm2Input, Option<Mm2Input>), Box<dyn Error>> {
    let rom_path = env_path(&["HARMONY_MM2_ROM"])?;
    let core = env_path(&["HARMONY_QUICKNES_CORE"])?;
    let prefix_path = env_path(&["HARMONY_MM2_STAGE_PREFIX", "HARMONY_MM2_PREFIX_INPUT"])?;
    let suffix_path = env_path(&["HARMONY_MM2_HISTORICAL_SUFFIX", "HARMONY_MM2_DEATH_INPUT"])?;
    let root_path = optional_env_path(&["HARMONY_MM2_ROOT_INPUT"]);
    let flash_path = optional_env_path(&["HARMONY_MM2_FLASH_INPUT"]);
    let stage = Mm2Stage::parse(
        &env::var("HARMONY_MM2_STAGE").map_err(|_| "HARMONY_MM2_STAGE is required")?,
    )?;
    let prefix = read_input(&prefix_path)?;
    let suffix = read_input(&suffix_path)?;
    let root = root_path
        .as_ref()
        .map(read_input)
        .transpose()?
        .unwrap_or_default();
    let flash = flash_path.as_ref().map(read_input).transpose()?;
    let rom = fs::read(&rom_path)?;
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core)?));
    Ok((
        Harness {
            rom,
            core,
            core_sha256,
            prefix: prefix.actions,
            root: root.actions,
            stage,
        },
        suffix,
        flash,
    ))
}

fn record_dead(report: &mut ReplayReport, observations: &[Mm2Observations], base_frame: u64) {
    if report.first_dead_frame.is_none() {
        report.first_dead_frame = observations
            .iter()
            .find(|observation| observation.dead)
            .map(|observation| observation.frame_count.saturating_sub(base_frame));
    }
}

fn replay(
    harness: &Harness,
    input: &Mm2Input,
    segmented: bool,
) -> Result<ReplayReport, Box<dyn Error>> {
    let mut target = harness.target()?;
    let base_frame = target.observe().frame_count;
    let mut report = ReplayReport {
        first_dead_frame: None,
        final_dead: false,
        saw_short_flash: false,
        short_flash_dead: false,
    };
    let mut dying_run = 0_u32;
    let mut dying_run_dead = false;

    'actions: for action in &input.actions {
        let frame_actions = if segmented {
            action.bounded_hold_frames()
        } else {
            1
        };
        for _ in 0..frame_actions {
            let one = if segmented {
                ButtonChord::new(action.buttons, 1)
            } else {
                *action
            };
            target.apply(&one);
            let observations = target.last_action_observations();
            record_dead(&mut report, observations, base_frame);
            let observation = target.observe();
            if segmented {
                if observation.decoded.is_dying() {
                    dying_run = dying_run.saturating_add(1);
                    dying_run_dead |= observation.dead;
                } else if dying_run != 0 {
                    if !dying_run_dead && !observation.dead {
                        report.saw_short_flash = true;
                    } else if dying_run_dead {
                        report.short_flash_dead = true;
                    }
                    dying_run = 0;
                    dying_run_dead = false;
                }
            }
            if target.is_dead() {
                break 'actions;
            }
            if !segmented {
                break;
            }
        }
    }
    report.final_dead = target.is_dead();
    if target.exit_kind() == ExitKind::Crash {
        return Err("real-input replay crashed the MM2 target".into());
    }
    Ok(report)
}

fn apply_remaining_frames(
    target: &mut Mm2Target,
    input: &Mm2Input,
    action_index: usize,
    first_frame: u8,
    base_frame: u64,
) -> Option<u64> {
    for (index, action) in input.actions.iter().enumerate().skip(action_index) {
        let start = if index == action_index {
            first_frame
        } else {
            0
        };
        for _ in start..action.bounded_hold_frames() {
            target.apply(&ButtonChord::new(action.buttons, 1));
            if let Some(observation) = target
                .last_action_observations()
                .iter()
                .find(|observation| observation.dead)
            {
                return Some(observation.frame_count.saturating_sub(base_frame));
            }
        }
    }
    None
}

fn snapshot_restore_check(harness: &Harness, input: &Mm2Input) -> TestResult {
    let mut target = harness.target()?;
    let base_frame = target.observe().frame_count;
    let mut snapshot = None;
    let mut snapshot_position = None;
    for (action_index, action) in input.actions.iter().enumerate() {
        for frame in 0..action.bounded_hold_frames() {
            target.apply(&ButtonChord::new(action.buttons, 1));
            let observation = target.observe();
            if observation.dying_run > 0 && !observation.dead {
                snapshot = target.snapshot();
                snapshot_position = Some((action_index, frame.saturating_add(1)));
                break;
            }
        }
        if snapshot.is_some() {
            break;
        }
    }
    let snapshot = snapshot.ok_or("historical suffix never exposed a nonterminal dying frame")?;
    let position = snapshot_position.ok_or("snapshot position missing")?;
    let saved_observation = target.observe();
    let first_dead = apply_remaining_frames(&mut target, input, position.0, position.1, base_frame);
    let first_dead = first_dead.ok_or("historical suffix never reached terminal death")?;
    target.restore(&snapshot)?;
    let restored = target.observe();
    assert_eq!(restored.frame_count, saved_observation.frame_count);
    assert_eq!(restored.dying_run, saved_observation.dying_run);
    assert_eq!(restored.decoded, saved_observation.decoded);
    let restored_dead =
        apply_remaining_frames(&mut target, input, position.0, position.1, base_frame)
            .ok_or("restored historical suffix never reached terminal death")?;
    assert_eq!(restored_dead, first_dead);
    Ok(())
}

#[test]
#[ignore = "requires the external MM2 ROM, QuickNES core, and lab fixture tapes"]
fn real_mm2_dying_run_is_segmentation_and_snapshot_stable() -> TestResult {
    let (harness, suffix, flash) = load_harness()?;
    let contiguous = replay(&harness, &suffix, false)?;
    let segmented = replay(&harness, &suffix, true)?;
    assert!(
        contiguous.final_dead,
        "contiguous historical suffix did not die"
    );
    assert!(
        segmented.final_dead,
        "one-frame historical suffix did not die"
    );
    assert_eq!(
        contiguous.first_dead_frame, segmented.first_dead_frame,
        "death timing changed when actions were segmented into one-frame actions"
    );
    snapshot_restore_check(&harness, &suffix)?;

    let flash_input = flash.as_ref().unwrap_or(&suffix);
    let flash_report = replay(&harness, flash_input, true)?;
    if flash.is_some() {
        assert!(
            !flash_report.final_dead,
            "the known-surviving flash fixture died"
        );
    }
    assert!(
        flash_report.saw_short_flash,
        "fixture did not contain a nonterminal transient player-state-zero run"
    );
    assert!(
        !flash_report.short_flash_dead,
        "a transient player-state-zero run was classified as terminal death"
    );
    Ok(())
}
