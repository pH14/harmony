// SPDX-License-Identifier: AGPL-3.0-or-later
//! The deterministic scripted start (round-4 P1): from power-on SMB sits on
//! its TITLE screen, and the campaign alphabet deliberately excludes `START`
//! (branches explore gameplay inputs, not menu resets) — so without a start
//! sequence every branch would explore the title screen and the exploration
//! data would be vacuous. This script runs **before** the billboard is
//! published and `setup_complete` is signalled, pressing `START` in a fixed
//! press/release cadence (the game latches button *edges* on its per-frame
//! poll) until the console RAM shows gameplay (`OperMode == 1`), then settling
//! a few neutral frames — so the base seal lands at **gameplay start**, and
//! every branch inherits it.
//!
//! The script draws **no entropy** and takes **no input** beyond the fixed
//! cadence: it is a pure function of the core's power-on state, so it runs
//! the same frames every time (the portable determinism test below pins
//! that). Failure to reach gameplay within the frame bound is a loud error —
//! never a silently-vacuous campaign. The box smoke additionally verifies the
//! billboard shows in-gameplay state at the seal point (the vacuity check,
//! IMPLEMENTATION-task86.md).

use std::fmt;

use crate::chord::joypad::START;
use crate::core_seam::Core;
use crate::ram::{self, RamError, SmbState, WORK_RAM_LEN};

/// The fixed start cadence. All frame counts; the whole script is bounded by
/// `max_frames`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StartScript {
    /// Frames `START` is held per press (the game latches the edge).
    pub press_frames: u32,
    /// Frames released between presses (so repeated presses stay edges).
    pub release_frames: u32,
    /// Neutral frames run after gameplay is first observed (level load
    /// settles; the state is re-verified after).
    pub settle_frames: u32,
    /// The loud-failure bound on the whole script.
    pub max_frames: u32,
}

impl Default for StartScript {
    fn default() -> Self {
        StartScript {
            press_frames: 4,
            release_frames: 4,
            // SMB loads 1-1 well inside a second; 16 frames settles the
            // level-load transition the first gameplay observation sits in.
            settle_frames: 16,
            // 30 s at 60 fps — far past any title/menu path SMB has, so a
            // failure here is a broken core/ROM, not a slow menu.
            max_frames: 1800,
        }
    }
}

/// What the start script did — logged by the binary and asserted by the
/// portable determinism test.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StartReport {
    /// Total frames the script ran (presses + settle).
    pub frames_run: u32,
    /// The decoded state at the end (verified in-gameplay).
    pub state: SmbState,
}

/// Why the start script failed. Every variant is fatal: sealing a base on the
/// title screen would make the whole campaign vacuous.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StartError {
    /// The cadence is unusable: zero press frames can never produce an edge,
    /// and an overflowing `press + release` cycle would wrap.
    BadScript,
    /// The core could not expose its work RAM.
    WorkRamFailed,
    /// The work RAM did not decode.
    Ram(RamError),
    /// Gameplay was never observed within the frame bound.
    NeverReachedGameplay {
        /// Frames run before giving up.
        frames: u32,
    },
    /// Gameplay was observed but did not survive the settle frames (a demo /
    /// transient state, not a real game start).
    GameplayDidNotSettle {
        /// The mode observed after settling.
        mode: u8,
    },
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::BadScript => write!(
                f,
                "start script needs press_frames >= 1 and a non-overflowing press+release cycle"
            ),
            StartError::WorkRamFailed => write!(f, "core work RAM unavailable during start"),
            StartError::Ram(e) => write!(f, "work RAM decode failed during start: {e}"),
            StartError::NeverReachedGameplay { frames } => write!(
                f,
                "gameplay (OperMode 1) never observed within {frames} start frames — a base \
                 sealed here would make the campaign vacuous (title-screen exploration)"
            ),
            StartError::GameplayDidNotSettle { mode } => write!(
                f,
                "gameplay did not survive the settle frames (mode {mode} after settling)"
            ),
        }
    }
}

impl std::error::Error for StartError {}

/// Drive the core from power-on to gameplay start: press `START` on the fixed
/// cadence, checking the console RAM every frame; on the first gameplay
/// observation run the settle frames (neutral input) and re-verify. Draws no
/// entropy — a pure function of the core's power-on state.
pub fn run_start_script<C: Core>(
    core: &mut C,
    script: &StartScript,
) -> Result<StartReport, StartError> {
    if script.press_frames == 0 {
        return Err(StartError::BadScript);
    }
    // Public library input (rule 4): an overflowing press+release cycle must
    // reject, not wrap (a wrap to 0 would panic at `frames % cycle`).
    let cycle = script
        .press_frames
        .checked_add(script.release_frames)
        .ok_or(StartError::BadScript)?;
    let mut ram = [0u8; WORK_RAM_LEN];
    let mut frames = 0u32;
    while frames < script.max_frames {
        let held = if frames % cycle < script.press_frames {
            START
        } else {
            0
        };
        core.run_frame(held);
        frames += 1;
        let state = observe(core, &mut ram)?;
        if state.in_gameplay() {
            for _ in 0..script.settle_frames {
                core.run_frame(0);
                frames += 1;
            }
            let settled = observe(core, &mut ram)?;
            if !settled.in_gameplay() {
                return Err(StartError::GameplayDidNotSettle {
                    mode: settled.game_mode,
                });
            }
            return Ok(StartReport {
                frames_run: frames,
                state: settled,
            });
        }
    }
    Err(StartError::NeverReachedGameplay { frames })
}

/// Read + decode the console RAM (the same fields the billboard carries — the
/// box smoke re-verifies them host-side at the seal point).
fn observe<C: Core>(core: &mut C, ram: &mut [u8; WORK_RAM_LEN]) -> Result<SmbState, StartError> {
    if !core.read_work_ram(ram) {
        return Err(StartError::WorkRamFailed);
    }
    ram::decode(ram).map_err(StartError::Ram)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_seam::MockCore;

    /// The vacuity fix itself: from power-on (title screen), the script
    /// reaches gameplay — and it is deterministic: two fresh runs execute the
    /// identical number of frames to the identical state.
    #[test]
    fn start_script_reaches_gameplay_deterministically() {
        let run = || {
            let mut core = MockCore::new();
            let report = run_start_script(&mut core, &StartScript::default()).unwrap();
            (report, core.frames_run())
        };
        let (a, frames_a) = run();
        let (b, frames_b) = run();
        assert_eq!(a, b, "same frames, same state, every run");
        assert_eq!(frames_a, frames_b);
        assert_eq!(a.frames_run, frames_a, "the report counts every frame run");
        assert!(
            a.state.in_gameplay(),
            "the seal point is gameplay, not title"
        );
        assert_eq!(a.state.depth_ordinal(), 0, "gameplay STARTS at 1-1");
    }

    /// An exhausted frame bound is a loud error, never a title-screen seal.
    #[test]
    fn never_reaching_gameplay_is_loud() {
        let mut core = MockCore::new();
        let script = StartScript {
            max_frames: 2, // less than the mock's press+load latency
            ..StartScript::default()
        };
        assert!(matches!(
            run_start_script(&mut core, &script),
            Err(StartError::NeverReachedGameplay { frames: 2 })
        ));
    }

    /// A cadence that can never produce an edge — or whose cycle overflows —
    /// is rejected up front (round-5 P2: no wrap, no `% 0` panic).
    #[test]
    fn unusable_cadences_are_rejected() {
        let mut core = MockCore::new();
        let zero_press = StartScript {
            press_frames: 0,
            ..StartScript::default()
        };
        assert!(matches!(
            run_start_script(&mut core, &zero_press),
            Err(StartError::BadScript)
        ));
        let overflowing = StartScript {
            press_frames: 1,
            release_frames: u32::MAX,
            ..StartScript::default()
        };
        assert!(matches!(
            run_start_script(&mut core, &overflowing),
            Err(StartError::BadScript)
        ));
    }

    /// A core already in gameplay (a branch resumed mid-game) passes through
    /// after one frame + settle — the script converges, it never resets.
    #[test]
    fn already_in_gameplay_passes_straight_through() {
        let mut core = MockCore::in_gameplay();
        let report = run_start_script(&mut core, &StartScript::default()).unwrap();
        assert!(report.state.in_gameplay());
        assert_eq!(
            report.frames_run,
            1 + StartScript::default().settle_frames,
            "one observation frame + the settle"
        );
    }
}
