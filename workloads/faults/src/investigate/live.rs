// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Consonance guest an investigation drives.
//!
//! Advancement here is the package's own: a recorded execution is walked window
//! by window, each window branched from its predecessor's seal under the
//! standing-fault list of that prefix and the host-plane effect its action
//! stages — the same way the campaign that recorded the finding reached it. A
//! continuation therefore inherits the source's remaining inputs at their
//! original virtual times, and a cold restore in a fresh process reaches the
//! same state, events, and moment as one that never exited.
//!
//! Past the recorded execution's end there are no more windows. Running further
//! needs `extend`, which continues under the final environment and stages no
//! new actions.

use std::error::Error;

use consonance_client::session::{PortableSnapshot, Session, SessionError};
use control_proto::SnapId;

use crate::checkpoint::Checkpoint;
use crate::consonance::{
    FaultConfig, SETTLE_ALLOWANCE_NANOS, SETTLE_STEP_NANOS, branch_config, service_factory,
};
use crate::investigate::{
    Advance, CommandOutcome, Condition, Continuation, Endpoint, SdkEventRecord,
};
use crate::target::{
    ActionWindows, FaultAction, FaultObservations, FaultStop, action_delta, decode_sdk_events,
};
use crate::workspace::StopReason;

/// A live Consonance session positioned on one execution.
pub struct ConsonanceGuest {
    session: Session,
    windows: ActionWindows,
    /// The recorded action list this execution inherits. Advancing crosses its
    /// window boundaries the way the recording did.
    actions: Vec<FaultAction>,
    setup: SnapId,
    /// The handle of the point the guest currently stands on, when one is held.
    held: Option<SnapId>,
}

impl ConsonanceGuest {
    /// Boot a session for one workload image and position it at the sealed
    /// setup point.
    ///
    /// # Errors
    ///
    /// Returns an error when the guest cannot boot or never reaches setup.
    pub fn boot(
        kernel: &[u8],
        initramfs: &[u8],
        config: &FaultConfig,
    ) -> Result<Self, Box<dyn Error>> {
        let mut session = Session::new_with_config_and_payloads(
            kernel,
            initramfs,
            config.session_config(),
            Vec::new(),
        )?;
        session.set_service_factory(service_factory());
        let (setup, root_seal) = session.setup_handle();
        Ok(Self {
            session,
            windows: ActionWindows {
                root_seal,
                horizon_nanos: config.horizon_nanos,
            },
            actions: Vec::new(),
            setup,
            held: None,
        })
    }

    /// The sealed setup moment every window is measured from.
    #[must_use]
    pub fn root_seal(&self) -> u64 {
        self.windows.root_seal
    }

    /// The recorded execution this guest continues.
    pub fn set_recorded_actions(&mut self, actions: &[FaultAction]) {
        self.actions = actions.to_vec();
    }

    /// Virtual time at the recorded execution's end.
    #[must_use]
    pub fn recorded_end(&self) -> u64 {
        self.windows.root_seal.saturating_add(
            self.windows
                .horizon_nanos
                .saturating_mul(self.actions.len() as u64),
        )
    }

    /// The window index containing `moment`, which is `actions.len()` once the
    /// recorded execution has ended.
    fn window_index(&self, moment: u64) -> usize {
        if self.windows.horizon_nanos == 0 {
            return self.actions.len();
        }
        let elapsed = moment.saturating_sub(self.windows.root_seal);
        usize::try_from(elapsed / self.windows.horizon_nanos).unwrap_or(usize::MAX)
    }

    /// Seal the current point and hold its handle, releasing the previous one.
    fn seal_here(&mut self) -> Result<(SnapId, u64), Box<dyn Error>> {
        let (snapshot, at, _) = self
            .session
            .seal(SETTLE_STEP_NANOS, SETTLE_ALLOWANCE_NANOS)?;
        self.release_held();
        self.held = Some(snapshot);
        Ok((snapshot, at))
    }

    fn release_held(&mut self) {
        if let Some(previous) = self.held.take() {
            let _ = self.session.drop_snapshot(previous);
        }
    }

    /// Open window `index` from the point currently held: branch under that
    /// prefix's standing-fault list and stage the host-plane effect its action
    /// carries.
    fn open_window(&mut self, index: usize) -> Result<(), Box<dyn Error>> {
        let (parent, at) = self.seal_here()?;
        let prefix = &self.actions[..=index];
        let config = branch_config(self.windows, prefix)?;
        let effects = self.staged_effects(index, at)?;
        self.session
            .branch_with_service(parent, config, Vec::new(), effects)?;
        Ok(())
    }

    /// The host-plane effect the action at `index` stages, at its window start
    /// or at `floor` when settling carried the parent past that start.
    fn staged_effects(
        &self,
        index: usize,
        floor: u64,
    ) -> Result<Vec<(u64, environment::channel::Effect)>, Box<dyn Error>> {
        let Some(action) = self.actions.get(index) else {
            return Ok(Vec::new());
        };
        let Some(perturb) = action_delta(*action, self.windows.window(index)).perturb else {
            return Ok(Vec::new());
        };
        let fault = fault_policy::HostFault::decode(&perturb.fault)?;
        let effect = fault_policy::consonance::effect(&fault)?;
        Ok(vec![(perturb.at.max(floor), effect)])
    }

    /// Advance to `target`, crossing the recorded execution's window boundaries
    /// the way the recording crossed them.
    fn advance_to(
        &mut self,
        target: u64,
        bound: &Advance,
    ) -> Result<(StopReason, bool), Box<dyn Error>> {
        let mut condition_met = false;
        loop {
            let now = self.session.virtual_time()?;
            if now >= target {
                return Ok((StopReason::VirtualDeadline, condition_met));
            }
            let index = self.window_index(now);
            if index >= self.actions.len() && !bound.extend {
                return Ok((StopReason::ContinuationEnd, condition_met));
            }
            if index < self.actions.len() {
                let start = self
                    .windows
                    .root_seal
                    .saturating_add(self.windows.horizon_nanos.saturating_mul(index as u64));
                if now <= start {
                    self.open_window(index)?;
                }
            }
            let window_end = if index < self.actions.len() {
                self.windows.deadline(index)
            } else {
                target
            };
            let deadline = window_end.min(target);
            let before = self.event_count()?;
            let stop = self.session.run_until(deadline)?;
            let fault_stop = FaultStop::from_stop_reason(&stop);
            condition_met |= self.condition_reached(bound.until, before)?;
            if condition_met {
                return Ok((StopReason::ConditionMet, true));
            }
            match fault_stop {
                FaultStop::Deadline => {}
                FaultStop::Assertion { point } => {
                    return Ok((StopReason::Assertion { point }, condition_met));
                }
                FaultStop::Crash => return Ok((StopReason::Crash, condition_met)),
                FaultStop::Quiescent => return Ok((StopReason::Quiescent, condition_met)),
                FaultStop::Unexpected => {
                    return Ok((StopReason::VirtualDeadline, condition_met));
                }
            }
        }
    }

    fn event_count(&mut self) -> Result<usize, Box<dyn Error>> {
        Ok(self.session.sdk_events()?.len())
    }

    /// Whether a report satisfying `until` was published after `before`. Only
    /// new reports count: a failure already in the history is not a new one.
    fn condition_reached(
        &mut self,
        until: Option<Condition>,
        before: usize,
    ) -> Result<bool, Box<dyn Error>> {
        let Some(condition) = until else {
            return Ok(false);
        };
        let events = self.session.sdk_events()?;
        let fresh = events.get(before..).unwrap_or_default();
        let capture = decode_sdk_events(fresh)?;
        Ok(match condition {
            Condition::AssertionFail(id) => capture.violations.contains(&id),
            Condition::AssertionHit(id) => capture.sometimes.contains(&id),
        })
    }

    fn observations(&mut self, stop: FaultStop) -> Result<FaultObservations, Box<dyn Error>> {
        let events = self.session.sdk_events()?;
        let moment = events.last().map_or(0, |(moment, _, _)| *moment);
        let capture = decode_sdk_events(&events)?;
        Ok(FaultObservations::new(moment, &capture, stop))
    }

    fn endpoint(&mut self, stop: StopReason, condition_met: bool) -> Result<Endpoint, String> {
        let fault_stop = match &stop {
            StopReason::Assertion { point } => FaultStop::Assertion { point: *point },
            StopReason::Crash => FaultStop::Crash,
            StopReason::Quiescent => FaultStop::Quiescent,
            _ => FaultStop::Deadline,
        };
        let observations = self
            .observations(fault_stop)
            .map_err(|error| error.to_string())?;
        let virtual_time = self
            .session
            .virtual_time()
            .map_err(|error| error.to_string())?;
        Ok(Endpoint {
            virtual_time,
            stop,
            observations,
            condition_met,
        })
    }
}

impl Drop for ConsonanceGuest {
    fn drop(&mut self) {
        self.release_held();
    }
}

/// Quote one argv the way a guest shell reads it back as the same words.
///
/// The serial channel carries a command line, so argv has to survive shell
/// word splitting. Every word is single-quoted and embedded single quotes are
/// closed and re-opened, which leaves no character with a special meaning. A
/// shell expression therefore needs an explicit `sh -c`, as the CLI documents.
#[must_use]
pub fn quote_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|word| format!("'{}'", word.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

impl Continuation for ConsonanceGuest {
    fn open_recorded(
        &mut self,
        actions: &[FaultAction],
        rewind_nanos: u64,
    ) -> Result<Endpoint, String> {
        self.set_recorded_actions(actions);
        let end = self.recorded_end();
        let target = end.saturating_sub(rewind_nanos).max(self.windows.root_seal);
        let setup = self.setup;
        self.session
            .replay_snapshot(setup)
            .map_err(|error| format!("restore the setup point: {error}"))?;
        self.release_held();
        let bound = Advance {
            within_nanos: target.saturating_sub(self.windows.root_seal),
            until: None,
            extend: false,
            wall_seconds: 0,
        };
        let (stop, met) = self
            .advance_to(target, &bound)
            .map_err(|error| format!("reproduce the recorded execution: {error}"))?;
        self.endpoint(stop, met)
    }

    fn restore(&mut self, checkpoint: &[u8], actions: &[FaultAction]) -> Result<Endpoint, String> {
        // A restored point sits inside the recorded execution, so an advance
        // from it still has to cross the remaining windows under their own
        // standing lists.
        self.set_recorded_actions(actions);
        let decoded = Checkpoint::decode(checkpoint).map_err(|error| error.to_string())?;
        let portable = PortableSnapshot {
            setup: decoded.setup,
            image_identity: decoded.image_identity,
            at: decoded.at,
            pages: decoded.pages,
            sidecar: decoded.sidecar,
        };
        self.session
            .restore(&portable)
            .map_err(|error| format!("restore the checkpoint: {error}"))?;
        self.release_held();
        self.endpoint(StopReason::VirtualDeadline, false)
    }

    fn advance(&mut self, bound: &Advance) -> Result<Endpoint, String> {
        let now = self
            .session
            .virtual_time()
            .map_err(|error| error.to_string())?;
        let target = now.saturating_add(bound.within_nanos);
        let (stop, met) = self
            .advance_to(target, bound)
            .map_err(|error| format!("advance: {error}"))?;
        self.endpoint(stop, met)
    }

    fn exec(&mut self, argv: &[String], bound: &Advance) -> Result<CommandOutcome, String> {
        let now = self
            .session
            .virtual_time()
            .map_err(|error| error.to_string())?;
        let deadline = now.saturating_add(bound.within_nanos);
        let command = quote_argv(argv);
        let result = self
            .session
            .exec_command(&command, deadline)
            .map_err(|error| format!("deliver the command: {error}"))?;
        // The guest stopped before the command finished. Its capture stays live
        // in the server, so a later advance can still complete it.
        let stop = match &result.stop {
            Some(stop) => match FaultStop::from_stop_reason(stop) {
                FaultStop::Assertion { point } => StopReason::Assertion { point },
                FaultStop::Crash => StopReason::Crash,
                FaultStop::Quiescent => StopReason::Quiescent,
                _ => StopReason::VirtualDeadline,
            },
            None if result.completed => StopReason::CommandComplete,
            None => StopReason::VirtualDeadline,
        };
        let endpoint = self.endpoint(stop, false)?;
        Ok(CommandOutcome {
            endpoint,
            output: result.output,
            completed: result.completed,
            // The sentinel carries the shell's status, and the client reports
            // completion rather than the number. A command that did not
            // complete has no status at all.
            exit_status: result.completed.then_some(0),
        })
    }

    fn checkpoint(&mut self) -> Result<Vec<u8>, String> {
        let (portable, _modified) = self
            .session
            .checkpoint(SETTLE_STEP_NANOS, SETTLE_ALLOWANCE_NANOS)
            .map_err(|error| match error.downcast_ref::<SessionError>() {
                Some(SessionError::Settle { allowance }) => format!(
                    "the guest never reached a sealable point within {allowance} ns of settling, \
                     so this point cannot be retained"
                ),
                _ => format!("retain the checkpoint: {error}"),
            })?;
        Ok(Checkpoint {
            setup: portable.setup,
            at: portable.at,
            image_identity: portable.image_identity,
            pages: portable.pages,
            sidecar: portable.sidecar,
        }
        .encode())
    }

    fn console(&mut self) -> Result<Vec<u8>, String> {
        self.session
            .console_tail()
            .map_err(|error| format!("read the console: {error}"))
    }

    fn events(&mut self) -> Result<Vec<SdkEventRecord>, String> {
        let events = self
            .session
            .sdk_events()
            .map_err(|error| format!("read the SDK events: {error}"))?;
        Ok(events
            .into_iter()
            .enumerate()
            .map(|(index, (virtual_time, event, payload))| SdkEventRecord {
                position: index as u64,
                virtual_time,
                event,
                payload: payload.iter().map(|byte| format!("{byte:02x}")).collect(),
            })
            .collect())
    }

    fn state_hash(&mut self) -> Result<String, String> {
        let hash = self
            .session
            .state_hash()
            .map_err(|error| format!("hash the state: {error}"))?;
        Ok(hash.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    fn read_memory(&mut self, gpa: u64, len: u32) -> Result<Vec<u8>, String> {
        self.session
            .read(gpa, len)
            .map_err(|error| format!("read guest memory: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_survives_guest_shell_quoting() {
        assert_eq!(
            quote_argv(&["sh".to_owned(), "-c".to_owned(), "echo a b".to_owned()]),
            "'sh' '-c' 'echo a b'"
        );
        assert_eq!(
            quote_argv(&["psql".to_owned(), "-c".to_owned(), "SELECT 'x';".to_owned()]),
            "'psql' '-c' 'SELECT '\\''x'\\'';'"
        );
        assert_eq!(quote_argv(&[]), "");
    }
}
