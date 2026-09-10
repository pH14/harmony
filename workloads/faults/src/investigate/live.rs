// SPDX-License-Identifier: AGPL-3.0-or-later

//! The shipping CLI's lazy Consonance continuation. The action controller owns
//! transition position; this adapter reads one stopped endpoint for publication.

use std::{
    error::Error,
    time::{Duration, Instant},
};

use consonance_client::session::Session;

use crate::{
    consonance::{FaultConfig, boot_continuation},
    continuation::{ContinuationStop, RecordedContinuation},
    retained::RetainedContinuation,
    target::{FaultAction, FaultObservations, FaultStop, decode_sdk_events},
    workspace::StopReason,
};

use super::{
    Advance, CapturedEndpoint, Condition, Continuation, DEFAULT_WALL_SECONDS, Endpoint,
    SdkEventRecord,
};

/// Artifact bytes are prepared once, but no VM exists until a new operation
/// restores or reproduces a point. Committed retries need neither operation.
pub struct ConsonanceGuest {
    kernel: Vec<u8>,
    initramfs: Vec<u8>,
    config: FaultConfig,
    current: Option<RecordedContinuation<Session>>,
    endpoint: Option<Endpoint>,
}

impl ConsonanceGuest {
    pub fn new(kernel: Vec<u8>, initramfs: Vec<u8>, config: FaultConfig) -> Self {
        Self {
            kernel,
            initramfs,
            config,
            current: None,
            endpoint: None,
        }
    }

    fn current(&mut self) -> Result<&mut RecordedContinuation<Session>, Box<dyn Error>> {
        self.current
            .as_mut()
            .ok_or_else(|| "no usable continuation; restore a saved point first".into())
    }

    fn finish<T>(&mut self, result: Result<T, Box<dyn Error>>) -> Result<T, String> {
        result.map_err(|error| {
            // Failed observations and encoders are as fatal to publication as
            // a failed runtime request. Never reuse a partially inspected point.
            self.current = None;
            self.endpoint = None;
            error.to_string()
        })
    }

    fn drive(&mut self, target: Option<u64>, bound: &Advance) -> Result<Endpoint, Box<dyn Error>> {
        // Host time only abandons work; it never schedules an input or
        // contributes to the guest state, observations, or execution identity.
        #[allow(clippy::disallowed_methods)]
        let started = Instant::now();
        let duration = Duration::from_secs(if bound.wall_seconds == 0 {
            DEFAULT_WALL_SECONDS
        } else {
            bound.wall_seconds
        });
        let current = self.current()?;
        let baseline = current.sdk_events()?.len();
        remaining(started, duration)?;
        let mut condition_met = false;
        let stop = current.drive_observed(
            target,
            bound.extend,
            &mut |session| {
                session.set_wall_limit(remaining(started, duration)?);
                Ok(())
            },
            &mut |session, _| {
                let Some(condition) = bound.until else {
                    return Ok(false);
                };
                let events = session.sdk_events()?;
                let fresh = events
                    .get(baseline..)
                    .ok_or("guest event stream shrank during advance")?;
                let reports = decode_sdk_events(fresh)?;
                remaining(started, duration)?;
                condition_met = match condition {
                    Condition::AssertionFail(id) => reports.violations.contains(&id),
                    Condition::AssertionHit(id) => reports.sometimes.contains(&id),
                };
                Ok(condition_met)
            },
        )?;
        let fault_stop = match &stop {
            ContinuationStop::Guest(stop) => FaultStop::from_stop_reason(stop),
            ContinuationStop::BoundReached | ContinuationStop::RecordedEnd => FaultStop::Deadline,
        };
        let reason = if condition_met {
            StopReason::ConditionMet
        } else {
            match &stop {
                ContinuationStop::BoundReached => StopReason::VirtualDeadline,
                ContinuationStop::RecordedEnd => StopReason::ContinuationEnd,
                ContinuationStop::Guest(_) => match fault_stop {
                    FaultStop::Deadline => StopReason::VirtualDeadline,
                    FaultStop::Assertion { point } => StopReason::Assertion { point },
                    FaultStop::Crash => StopReason::Crash,
                    FaultStop::Quiescent => StopReason::Quiescent,
                    FaultStop::Unexpected => {
                        return Err("unexpected control stop during continuation".into());
                    }
                },
            }
        };
        let events = current.sdk_events()?;
        let observations =
            FaultObservations::new(current.at(), &decode_sdk_events(&events)?, fault_stop);
        remaining(started, duration)?;
        let endpoint = Endpoint {
            virtual_time: current.at(),
            stop: reason,
            observations,
            condition_met,
        };
        self.endpoint = Some(endpoint.clone());
        Ok(endpoint)
    }
}

impl Continuation for ConsonanceGuest {
    fn open_recorded(
        &mut self,
        actions: &[FaultAction],
        source_moment: u64,
        rewind_nanos: u64,
    ) -> Result<Endpoint, String> {
        let result = (|| {
            self.current = None;
            self.endpoint = None;
            let current = boot_continuation(
                &self.kernel,
                &self.initramfs,
                &self.config,
                actions.to_vec(),
                None,
            )?;
            let setup = current.at();
            if source_moment < setup {
                return Err("source finding precedes the sealed setup moment".into());
            }
            self.current = Some(current);
            let target =
                (rewind_nanos != 0).then(|| source_moment.saturating_sub(rewind_nanos).max(setup));
            let endpoint = self.drive(
                target,
                &Advance {
                    wall_seconds: DEFAULT_WALL_SECONDS,
                    ..Advance::default()
                },
            )?;
            if rewind_nanos == 0 && endpoint.virtual_time != source_moment {
                return Err(
                    "reproduction did not reach the recorded finding's exact moment".into(),
                );
            }
            if endpoint.virtual_time > source_moment {
                return Err("rewind passed the recorded finding's moment".into());
            }
            Ok(endpoint)
        })();
        self.finish(result)
    }

    fn restore(
        &mut self,
        checkpoint: &[u8],
        actions: &[FaultAction],
        endpoint: &Endpoint,
    ) -> Result<Endpoint, String> {
        let result = (|| {
            let retained = RetainedContinuation::decode(checkpoint)?;
            if retained.checkpoint.at != endpoint.virtual_time {
                return Err("retained checkpoint names a different endpoint".into());
            }
            self.current = None;
            self.endpoint = None;
            let mut current = boot_continuation(
                &self.kernel,
                &self.initramfs,
                &self.config,
                actions.to_vec(),
                Some(&retained),
            )?;
            let events = current.sdk_events()?;
            let observations = FaultObservations::new(
                current.at(),
                &decode_sdk_events(&events)?,
                endpoint.observations.stop,
            );
            if observations != endpoint.observations {
                return Err("restored guest observations differ from the retained endpoint".into());
            }
            self.current = Some(current);
            self.endpoint = Some(endpoint.clone());
            Ok(endpoint.clone())
        })();
        self.finish(result)
    }

    fn advance(&mut self, bound: &Advance) -> Result<Endpoint, String> {
        let result = (|| {
            let target = self
                .current()?
                .at()
                .checked_add(bound.within_nanos)
                .ok_or("advance virtual-time bound overflows")?;
            self.drive(Some(target), bound)
        })();
        self.finish(result)
    }

    fn capture(&mut self) -> Result<CapturedEndpoint, String> {
        let result = (|| {
            let endpoint = self.endpoint.clone().ok_or("no endpoint to capture")?;
            let current = self.current()?;
            let raw_events = current.sdk_events()?;
            let observations = FaultObservations::new(
                current.at(),
                &decode_sdk_events(&raw_events)?,
                endpoint.observations.stop,
            );
            if endpoint.virtual_time != current.at() || endpoint.observations != observations {
                return Err("guest observations changed after the stopped endpoint".into());
            }
            let state_hash = current.state_hash()?;
            let console = current.console_tail()?;
            let checkpoint = current.checkpoint()?.encode()?;
            if current.state_hash()? != state_hash {
                return Err("saving changed the stopped execution state".into());
            }
            let events = raw_events
                .into_iter()
                .enumerate()
                .map(|(position, (virtual_time, event, payload))| {
                    Ok(SdkEventRecord {
                        position: u64::try_from(position)?,
                        virtual_time,
                        event,
                        payload: hex(&payload),
                    })
                })
                .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
            Ok(CapturedEndpoint {
                endpoint,
                checkpoint,
                state_hash: hex(&state_hash),
                console,
                events,
            })
        })();
        self.finish(result)
    }
}

fn remaining(started: Instant, duration: Duration) -> Result<Duration, Box<dyn Error>> {
    duration
        .checked_sub(started.elapsed())
        .filter(|left| !left.is_zero())
        .ok_or_else(|| "host watchdog expired while advancing or observing the guest".into())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a String cannot fail");
    }
    out
}
