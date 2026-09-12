// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fmt;

use crate::chord::joypad::START;
use crate::core_seam::Core;
use crate::ram::{self, RamError, SmbState, WORK_RAM_LEN};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StartScript {
    pub press_frames: u32,
    pub release_frames: u32,
    pub settle_frames: u32,
    pub max_frames: u32,
}

impl Default for StartScript {
    fn default() -> Self {
        StartScript {
            press_frames: 4,
            release_frames: 4,
            settle_frames: 16,
            max_frames: 1800,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StartReport {
    pub frames_run: u32,
    pub state: SmbState,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StartError {
    BadScript,
    WorkRamFailed,
    Ram(RamError),
    NeverReachedGameplay {
        frames: u32,
    },
    SettleExceedsBudget {
        observed_at: u32,
        settle_frames: u32,
        max_frames: u32,
    },
    GameplayDidNotSettle {
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

pub fn run_start_script<C: Core>(
    core: &mut C,
    script: &StartScript,
) -> Result<StartReport, StartError> {
    if script.press_frames == 0 {
        return Err(StartError::BadScript);
    }
    let cycle = script
        .press_frames
        .checked_add(script.release_frames)
        .ok_or(StartError::BadScript)?;
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

    #[test]
    fn never_reaching_gameplay_is_loud() {
        let mut core = MockCore::new();
        let script = StartScript {
            max_frames: 2,
            settle_frames: 1,
            ..StartScript::default()
        };
        assert!(matches!(
            run_start_script(&mut core, &script),
            Err(StartError::NeverReachedGameplay { frames: 2 })
        ));
    }

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

    #[test]
    fn a_settle_that_would_overrun_the_budget_is_loud() {
        let mut core = MockCore::new();
        let script = StartScript {
            settle_frames: 16,
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

        let mut core = MockCore::new();
        let ok = StartScript {
            max_frames: 20,
            ..script
        };
        let report = run_start_script(&mut core, &ok).expect("4 press frames + a 16-frame settle");
        assert_eq!(report.frames_run, 20);
    }

    #[test]
    fn a_budget_that_cannot_hold_the_settle_is_rejected_up_front() {
        let mut core = MockCore::new();
        let script = StartScript {
            settle_frames: 16,
            max_frames: 16,
            ..StartScript::default()
        };
        assert!(matches!(
            run_start_script(&mut core, &script),
            Err(StartError::BadScript)
        ));
        assert_eq!(core.frames_run(), 0, "rejected before the first frame");

        let overflowing = StartScript {
            settle_frames: u32::MAX,
            ..StartScript::default()
        };
        assert!(matches!(
            run_start_script(&mut core, &overflowing),
            Err(StartError::BadScript)
        ));
    }

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
