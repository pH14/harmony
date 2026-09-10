// SPDX-License-Identifier: AGPL-3.0-or-later

//! Cold continuation owns the recorded action position alongside the runtime.
//! Capturing or restoring a point never activates an action or runs the guest.

use std::error::Error;

use control_proto::{SnapId, StopReason};

use crate::{
    action_execution::{ActionExecution, ActionRuntime},
    checkpoint::Checkpoint,
    execution::ActionCursor,
    retained::RetainedContinuation,
    target::{ActionWindows, FaultAction},
};

/// Snapshot operations required in addition to ordinary action execution.
pub trait ContinuationRuntime: ActionRuntime {
    fn capture(&mut self) -> Result<(SnapId, u64), Box<dyn Error>>;
    fn release(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>>;
    fn export(&mut self, snapshot: SnapId, at: u64) -> Result<Checkpoint, Box<dyn Error>>;
    fn restore(&mut self, checkpoint: &Checkpoint) -> Result<(), Box<dyn Error>>;
}

/// Why a bounded continuation returned control to its caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContinuationStop {
    Guest(StopReason),
    BoundReached,
    RecordedEnd,
}

#[derive(Clone, Copy)]
enum Limit {
    Moment(u64),
    RecordedEnd,
}

/// An execution positioned at setup, a runtime stop, or a restored checkpoint.
pub struct RecordedContinuation<R> {
    runtime: R,
    actions: Vec<FaultAction>,
    execution: ActionExecution,
    windows: ActionWindows,
    at: u64,
    poisoned: bool,
}

impl<R: ContinuationRuntime> RecordedContinuation<R> {
    /// The supplied runtime must already be at its sealed setup point.
    pub fn from_setup(runtime: R, windows: ActionWindows, actions: Vec<FaultAction>) -> Self {
        Self {
            runtime,
            actions,
            execution: ActionExecution::new(windows, ActionCursor::default()),
            windows,
            at: windows.root_seal,
            poisoned: false,
        }
    }

    /// Restore an explicit action position without branch activation or guest execution.
    pub fn restore(
        mut runtime: R,
        retained: &RetainedContinuation,
    ) -> Result<Self, Box<dyn Error>> {
        retained.validate()?;
        let checkpoint = &retained.checkpoint;
        let windows = retained.windows;
        let actions = retained.actions.clone();
        let cursor = checkpoint.cursor.ok_or(
            "legacy checkpoint has no action position; reproduce its source before retaining a new continuation",
        )?;
        let execution = ActionExecution::new(windows, cursor);
        execution.validate(checkpoint.at, actions.len())?;
        runtime.restore(checkpoint)?;
        Ok(Self {
            runtime,
            actions,
            execution,
            windows,
            at: checkpoint.at,
            poisoned: false,
        })
    }

    pub fn at(&self) -> u64 {
        self.at
    }

    pub fn cursor(&self) -> ActionCursor {
        self.execution.cursor()
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ))]
    pub(crate) fn inspect<T>(
        &mut self,
        read: impl FnOnce(&mut R) -> Result<T, Box<dyn Error>>,
    ) -> Result<T, Box<dyn Error>> {
        self.ensure_usable()?;
        let result = read(&mut self.runtime);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    /// Reproduce the action list by ordinal, including transitions sharing a moment.
    pub fn reproduce(&mut self) -> Result<ContinuationStop, Box<dyn Error>> {
        self.drive(Limit::RecordedEnd, false)
    }

    /// Run to an absolute bound. Extending retains the final environment unchanged.
    pub fn advance_to(
        &mut self,
        target: u64,
        extend: bool,
    ) -> Result<ContinuationStop, Box<dyn Error>> {
        self.drive(Limit::Moment(target), extend)
    }

    /// Retain the VM and its cursor at exactly the current endpoint.
    pub fn checkpoint(&mut self) -> Result<RetainedContinuation, Box<dyn Error>> {
        self.ensure_usable()?;
        let result = (|| {
            let (snapshot, at) = self.runtime.capture()?;
            let exported = if at != self.at {
                Err("checkpoint capture changed the stopped moment".into())
            } else {
                self.runtime.export(snapshot, at)
            };
            let mut checkpoint = self.release_after(snapshot, exported)?;
            if checkpoint.at != at {
                return Err("checkpoint export names a different endpoint".into());
            }
            checkpoint.cursor = Some(self.execution.cursor());
            let retained = RetainedContinuation {
                checkpoint,
                windows: self.windows,
                actions: self.actions.clone(),
            };
            retained.validate()?;
            Ok(retained)
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn drive(&mut self, limit: Limit, extend: bool) -> Result<ContinuationStop, Box<dyn Error>> {
        self.ensure_usable()?;
        let result = self.drive_inner(limit, extend);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn drive_inner(
        &mut self,
        limit: Limit,
        extend: bool,
    ) -> Result<ContinuationStop, Box<dyn Error>> {
        loop {
            if matches!(limit, Limit::Moment(target) if self.at >= target) {
                return Ok(ContinuationStop::BoundReached);
            }
            let cursor = self.execution.cursor();
            let ended =
                cursor.completed() == u64::try_from(self.actions.len())? && !cursor.active();
            if ended && (!extend || matches!(limit, Limit::RecordedEnd)) {
                return Ok(ContinuationStop::RecordedEnd);
            }
            if !ended && !cursor.active() {
                let (parent, at) = self.runtime.capture()?;
                let activated = if at != self.at {
                    Err("activation snapshot changed the stopped moment".into())
                } else {
                    self.execution
                        .activate(&mut self.runtime, parent, at, &self.actions)
                };
                self.release_after(parent, activated)?;
            }
            let target = match limit {
                Limit::Moment(target) => target,
                Limit::RecordedEnd => self.windows.deadline(usize::try_from(cursor.completed())?),
            };
            let stop = if ended {
                self.runtime.run_action_until(target)?
            } else {
                self.execution.run_until(&mut self.runtime, target)?
            };
            let at = stop_moment(&stop);
            if at < self.at {
                return Err("runtime stop moved virtual time backwards".into());
            }
            let requested = if ended {
                target
            } else {
                target.min(self.windows.deadline(usize::try_from(cursor.completed())?))
            };
            if matches!(stop, StopReason::Deadline { .. }) && at < requested {
                return Err("runtime deadline returned before its requested moment".into());
            }
            self.at = at;
            if !matches!(stop, StopReason::Deadline { .. }) {
                return Ok(ContinuationStop::Guest(stop));
            }
        }
    }

    fn release_after<T>(
        &mut self,
        snapshot: SnapId,
        result: Result<T, Box<dyn Error>>,
    ) -> Result<T, Box<dyn Error>> {
        match (result, self.runtime.release(snapshot)) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(cleanup)) => {
                Err(format!("{error}; snapshot release also failed: {cleanup}").into())
            }
        }
    }

    fn ensure_usable(&self) -> Result<(), Box<dyn Error>> {
        if self.poisoned {
            Err("continuation failed; restore a retained checkpoint in a fresh runtime".into())
        } else {
            Ok(())
        }
    }
}

pub(crate) fn stop_moment(stop: &StopReason) -> u64 {
    match stop {
        StopReason::Deadline { vtime }
        | StopReason::Quiescent { vtime }
        | StopReason::Crash { vtime, .. }
        | StopReason::Decision { vtime, .. }
        | StopReason::SnapshotPoint { vtime }
        | StopReason::Assertion { vtime, .. } => vtime.0,
    }
}

#[cfg(test)]
mod tests;
