// SPDX-License-Identifier: AGPL-3.0-or-later
//! The [`AlwaysViolation`] [`Oracle`]: a run that terminated in
//! [`StopReason::Assertion`] is a [`Bug`].
//!
//! An SDK `assert_always` violation (or an `assert_unreachable` reached) stops the
//! vCPU as [`StopReason::Assertion`] (the vmm-core run-loop seam). This oracle
//! turns that terminal into a [`Bug`] with the run's genesis-complete reproducer
//! `env` and a stable fingerprint, so the campaign reports and dedups it exactly
//! as it does a crash.
//!
//! The fingerprint is byte-identical to the explorer's own bug fingerprint so a
//! link-minted [`Bug`] dedups against an explorer-minted one across the many
//! environments that reach the same assertion. It mints through the **shared**
//! [`explorer::terminal_fingerprint`] (the task-75 v2 scheme) rather than
//! restating it, so the two can never drift.

use explorer::{Bug, Oracle, RunTrace, StopReason, terminal_fingerprint};

/// The always-violation oracle: `Some(Bug)` iff the run terminated in
/// [`StopReason::Assertion`].
#[derive(Clone, Debug, Default)]
pub struct AlwaysViolation;

impl AlwaysViolation {
    /// The always-violation oracle (stateless).
    pub fn new() -> AlwaysViolation {
        AlwaysViolation
    }
}

impl Oracle for AlwaysViolation {
    fn judge(&self, t: &RunTrace) -> Option<Bug> {
        match &t.terminal {
            StopReason::Assertion { .. } => Some(Bug {
                env: t.env.clone(),
                stop: t.terminal.clone(),
                fingerprint: terminal_fingerprint(&t.terminal),
            }),
            _ => None,
        }
    }
}
