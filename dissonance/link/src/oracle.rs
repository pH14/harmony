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
//! The fingerprint is byte-identical to the explorer's own bug fingerprint
//! (`dissonance.explorer.bug.v1`, the `Assertion` arm) so a link-minted [`Bug`]
//! dedups against an explorer-minted one across the many environments that reach
//! the same assertion. (A shared helper would be cleaner; the explorer's
//! `fingerprint` is private, so the scheme is restated here with a cross-
//! reference — noted for the integrator.)

use explorer::{Bug, Oracle, RunTrace, StopReason};
use sha2::{Digest, Sha256};

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
                fingerprint: fingerprint(&t.terminal),
            }),
            _ => None,
        }
    }
}

/// A stable 32-byte digest of an [`Assertion`](StopReason::Assertion) stop,
/// byte-identical to the explorer's `Assertion` fingerprint so the two dedup.
/// Total over every variant (only `Assertion` is expected here); a non-assertion
/// stop hashes its tag so the function never panics.
fn fingerprint(stop: &StopReason) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"dissonance.explorer.bug.v1");
    match stop {
        StopReason::Assertion { vtime, id, data } => {
            h.update([0xA1]);
            h.update(vtime.0.to_le_bytes());
            h.update(id.to_le_bytes());
            h.update(data);
        }
        // The oracle only mints on `Assertion`; other variants are never
        // fingerprinted in practice but hash totally for safety.
        other => {
            h.update([0x00]);
            h.update(other.vtime().0.to_le_bytes());
        }
    }
    h.finalize().into()
}
