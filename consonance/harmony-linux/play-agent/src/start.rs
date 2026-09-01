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
//! docs/history/IMPLEMENTATION-task86.md).

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
    /// settles; the state is re-verified after). Spent from the **same**
    /// [`max_frames`](Self::max_frames) budget as the press cadence.
    pub settle_frames: u32,
    /// The loud-failure bound on the whole script — presses **and** settle.
    /// The script never runs a frame past it (task 103 finding 3): a settle
    /// that cannot fit under the bound is a loud error, never a silent
    /// overrun.
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
    /// The script is unusable as configured: zero press frames can never
    /// produce an edge, an overflowing `press + release` cycle would wrap, and
    /// a `max_frames` that cannot hold one observation frame plus the settle
    /// can never succeed within its own bound.
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
    /// Gameplay was observed so late that settling it would run past
    /// `max_frames` (task 103 finding 3). The bound covers the whole script,
    /// so an overrun is a loud failure, not a few extra frames taken quietly:
    /// the frames a rollout spends are the budget the box gate paid for.
    SettleExceedsBudget {
        /// The frame gameplay was first observed at.
        observed_at: u32,
        /// The settle the script still owed.
        settle_frames: u32,
        /// The bound it would have crossed.
        max_frames: u32,
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
                "start script needs press_frames >= 1, a non-overflowing press+release cycle, and \
                 max_frames > settle_frames (the settle is spent from the same budget)"
            ),
            StartError::SettleExceedsBudget {
                observed_at,
                settle_frames,
                max_frames,
            } => write!(
                f,
                "gameplay reached at frame {observed_at}, but its {settle_frames}-frame settle \
                 would run past the {max_frames}-frame bound — refusing to overrun the start \
                 budget (raise max_frames, or fix whatever made the start this slow)"
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
///
/// `max_frames` bounds the **whole** script, settle included (task 103
/// finding 3): a script whose settle cannot fit under the bound is rejected
/// before the first frame, and gameplay observed too late to settle inside it
/// fails with [`StartError::SettleExceedsBudget`]. `frames` therefore never
/// exceeds `max_frames` — it cannot overrun the bound and cannot overflow.
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
    // The cheapest possible success is one observation frame plus the settle;
    // a budget that cannot hold even that can never succeed, so say so before
    // burning a single frame rather than running to an overrun.
    let least = script
        .settle_frames
        .checked_add(1)
        .ok_or(StartError::BadScript)?;
    if least > script.max_frames {
        return Err(StartError::BadScript);
    }
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
            // The settle is spent from the same budget. `frames <= max_frames`
            // holds here (the loop only entered below the bound and added one),
            // so this subtraction cannot wrap — and refusing when the settle
            // does not fit keeps the whole script inside `max_frames`.
            if script.settle_frames > script.max_frames - frames {
                return Err(StartError::SettleExceedsBudget {
                    observed_at: frames,
                    settle_frames: script.settle_frames,
                    max_frames: script.max_frames,
                });
            }
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
            // The settle is spent from `max_frames` too, so a 2-frame budget
            // only admits a 1-frame settle; the bound under test is the one
            // gameplay is never reached within.
            settle_frames: 1,
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

    /// Task 103 finding 3: gameplay observed too late to settle inside
    /// `max_frames` is a loud refusal — the settle must never run the script
    /// past its own bound. Fixture: a budget that fits the presses and one
    /// observation frame, but not the settle behind it.
    #[test]
    fn a_settle_that_would_overrun_the_budget_is_loud() {
        let mut core = MockCore::new();
        let script = StartScript {
            settle_frames: 16,
            // The mock reaches gameplay at frame 4; 4 + 16 = 20 > 17, so the
            // settle cannot fit — but the script still passes the up-front
            // "could this ever succeed?" check (17 >= 16 + 1).
            max_frames: 17,
            ..StartScript::default()
        };
        assert!(matches!(
            run_start_script(&mut core, &script),
            Err(StartError::SettleExceedsBudget {
                observed_at: 4,
                settle_frames: 16,
                max_frames: 17,
            })
        ));
        assert!(
            core.frames_run() <= 17,
            "the refused script must not have run a frame past its bound, ran {}",
            core.frames_run()
        );

        // Widen the budget by the three frames the settle was short and the
        // identical script succeeds — the bound is what refused it, nothing else.
        let mut core = MockCore::new();
        let ok = StartScript {
            max_frames: 20,
            ..script
        };
        let report = run_start_script(&mut core, &ok).expect("4 press frames + a 16-frame settle");
        assert_eq!(report.frames_run, 20);
    }

    /// A budget that cannot hold one observation frame plus the settle can
    /// never succeed: refuse it before running any frame at all, rather than
    /// discovering it mid-settle.
    #[test]
    fn a_budget_that_cannot_hold_the_settle_is_rejected_up_front() {
        let mut core = MockCore::new();
        let script = StartScript {
            settle_frames: 16,
            max_frames: 16, // needs 17: the observation frame + the settle
            ..StartScript::default()
        };
        assert!(matches!(
            run_start_script(&mut core, &script),
            Err(StartError::BadScript)
        ));
        assert_eq!(core.frames_run(), 0, "rejected before the first frame");

        // The overflow edge of the same check: settle_frames + 1 wraps.
        let overflowing = StartScript {
            settle_frames: u32::MAX,
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
