// SPDX-License-Identifier: AGPL-3.0-or-later
//! Optional target-lifetime accounting, outside campaign decisions and bytes.
use super::campaign::TargetExecution;
use serde::{Deserialize, Serialize};
use std::{
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex},
};

/// A meter's cumulative receipt. While targets are live, post-constructor work
/// is incomplete. Failed construction has unknown cost because no target clock
/// is returned. Process termination can prevent a final receipt entirely.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PhysicalWorkReceipt {
    pub construction_attempts: u64,
    pub targets_created: u64,
    pub targets_closed: u64,
    pub constructor_frames: u64,
    pub closed_post_constructor_frames: u64,
    pub invalid_counter: bool,
}

impl PhysicalWorkReceipt {
    /// Complete physical work through each target's last clock read, before
    /// target destruction. Requires all constructors and target lifetimes to
    /// close successfully and a nonregressing, nonoverflowing lifetime clock.
    #[must_use]
    pub fn complete_frames(self) -> Option<u64> {
        if self.invalid_counter
            || self.construction_attempts != self.targets_created
            || self.targets_created != self.targets_closed
        {
            return None;
        }
        self.constructor_frames
            .checked_add(self.closed_post_constructor_frames)
    }
}

/// Cloneable reporting-only meter. Use a fresh meter for each measured phase.
/// It does not stop work or feed any state into selection, admission or replay.
/// Its host overhead can still change a wall-time cutoff, like other telemetry.
#[derive(Clone, Default)]
pub struct PhysicalWorkMeter(Arc<Mutex<PhysicalWorkReceipt>>);

impl PhysicalWorkMeter {
    /// Read a receipt; live targets and failed constructors remain explicit.
    #[must_use]
    pub fn receipt(&self) -> PhysicalWorkReceipt {
        match self.0.lock() {
            Ok(receipt) => *receipt,
            Err(error) => PhysicalWorkReceipt {
                invalid_counter: true,
                ..*error.into_inner()
            },
        }
    }

    fn update(&self, change: impl FnOnce(&mut PhysicalWorkReceipt)) {
        match self.0.lock() {
            Ok(mut receipt) => change(&mut receipt),
            Err(error) => error.into_inner().invalid_counter = true,
        }
    }
}

fn add(counter: &mut u64, amount: u64) -> bool {
    let Some(sum) = counter.checked_add(amount) else {
        return false;
    };
    *counter = sum;
    true
}

/// A scoped clock guard. With no meter it performs no additional clock reads.
/// The target must report a lifetime clock which restore/reset never rewinds.
/// Work performed inside a target destructor is outside this contract.
pub(crate) struct TrackedTarget<'a, G: TargetExecution> {
    game: &'a G,
    target: G::Target,
    meter: Option<PhysicalWorkMeter>,
    constructor_frames: u64,
}

impl<'a, G: TargetExecution> TrackedTarget<'a, G> {
    pub(crate) fn new(game: &'a G, meter: Option<&PhysicalWorkMeter>) -> Result<Self, String> {
        if let Some(meter) = meter {
            meter.update(|r| r.invalid_counter |= !add(&mut r.construction_attempts, 1));
        }
        let target = game.new_target()?;
        let constructor_frames = if let Some(meter) = meter {
            let frames = game.frames_clocked(&target);
            meter.update(|r| {
                r.invalid_counter |= !add(&mut r.targets_created, 1);
                r.invalid_counter |= !add(&mut r.constructor_frames, frames);
                r.invalid_counter |= frames == u64::MAX;
            });
            frames
        } else {
            0
        };
        Ok(Self {
            game,
            target,
            meter: meter.cloned(),
            constructor_frames,
        })
    }
}

impl<G: TargetExecution> Deref for TrackedTarget<'_, G> {
    type Target = G::Target;
    fn deref(&self) -> &Self::Target {
        &self.target
    }
}

impl<G: TargetExecution> DerefMut for TrackedTarget<'_, G> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.target
    }
}

impl<G: TargetExecution> Drop for TrackedTarget<'_, G> {
    fn drop(&mut self) {
        if let Some(meter) = &self.meter {
            let final_clock = self.game.frames_clocked(&self.target);
            let elapsed = final_clock.checked_sub(self.constructor_frames);
            meter.update(|r| {
                r.invalid_counter |= !add(&mut r.targets_closed, 1);
                r.invalid_counter |= final_clock == u64::MAX;
                match elapsed {
                    Some(frames) => {
                        r.invalid_counter |= !add(&mut r.closed_post_constructor_frames, frames);
                    }
                    None => r.invalid_counter = true,
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_construction_incomplete_lifetimes_and_overflow_are_not_zero_work() {
        for receipt in [
            PhysicalWorkReceipt {
                construction_attempts: 1,
                ..Default::default()
            },
            PhysicalWorkReceipt {
                construction_attempts: 1,
                targets_created: 1,
                constructor_frames: 7,
                ..Default::default()
            },
            PhysicalWorkReceipt {
                constructor_frames: u64::MAX,
                closed_post_constructor_frames: 1,
                ..Default::default()
            },
            PhysicalWorkReceipt {
                invalid_counter: true,
                ..Default::default()
            },
        ] {
            assert_eq!(receipt.complete_frames(), None);
        }
        assert_eq!(PhysicalWorkReceipt::default().complete_frames(), Some(0));
        let meter = PhysicalWorkMeter::default();
        meter.update(|r| {
            r.construction_attempts = u64::MAX;
            r.invalid_counter |= !add(&mut r.construction_attempts, 1);
        });
        assert!(meter.receipt().invalid_counter);
        assert_eq!(meter.receipt().complete_frames(), None);
    }
}
