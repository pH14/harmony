// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`SeededEnv`] — the pure DST backing: a seed and a [`FaultPolicy`] answer
//! every decision from two local PRNG streams, with no host round-trip.

use crate::catalog::DecisionPoint;
use crate::policy::FaultPolicy;
use crate::prng::Prng;
use crate::{Answer, DecisionClass, Environment, MAX_SUPPLY_LEN, Outcome};

/// Domain-separation constant for the fault stream, so fault sampling is
/// **independent of the guest entropy/payload/scheduler supply stream**: the two
/// streams are derived from the same seed but never share state, so tuning the
/// [`FaultPolicy`] cannot shift the entropy a guest pulls, and vice versa.
const FAULT_DOMAIN: u64 = 0xD1B5_4A32_D192_ED03;

/// A pure deterministic backing. One stream supplies entropy/payload/scheduler
/// values; an independent stream samples faults under the [`FaultPolicy`]. Given
/// the same `(seed, policy)` and the same [`DecisionPoint`] sequence it produces
/// the same [`Answer`] sequence — and it never suspends, so
/// [`decide`](SeededEnv::decide) always returns [`Outcome::Resolved`].
#[derive(Clone, Debug)]
pub struct SeededEnv {
    supply: Prng,
    fault: Prng,
    policy: FaultPolicy,
}

impl SeededEnv {
    /// Build a backing from a `seed` and a `policy`.
    pub fn new(seed: u64, policy: FaultPolicy) -> Self {
        Self {
            supply: Prng::new(seed),
            fault: Prng::new(seed ^ FAULT_DOMAIN),
            policy,
        }
    }

    /// Serialize the **dynamic stream state** — both PRNG positions — so a
    /// snapshot can resume the exact same supply and fault streams (task 73's
    /// SDK-channel snapshot). 16 bytes: the supply state, then the fault state,
    /// little-endian. The [`FaultPolicy`] is static (carried by the reproducer),
    /// so it is deliberately **not** part of this.
    pub fn stream_state(&self) -> [u8; 16] {
        let mut out = [0_u8; 16];
        out[..8].copy_from_slice(&self.supply.raw_state().to_le_bytes());
        out[8..].copy_from_slice(&self.fault.raw_state().to_le_bytes());
        out
    }

    /// Restore the dynamic stream state captured by
    /// [`stream_state`](SeededEnv::stream_state). Total: a zero word is remapped
    /// (the nonzero-state invariant), so a corrupted blob never panics.
    pub fn restore_stream_state(&mut self, state: &[u8; 16]) {
        let supply = u64::from_le_bytes([
            state[0], state[1], state[2], state[3], state[4], state[5], state[6], state[7],
        ]);
        let fault = u64::from_le_bytes([
            state[8], state[9], state[10], state[11], state[12], state[13], state[14], state[15],
        ]);
        self.supply = Prng::from_raw_state(supply);
        self.fault = Prng::from_raw_state(fault);
    }

    /// The [`Answer`] this backing gives for `point`, advancing the relevant
    /// stream. Shared by [`RecordedEnv`](crate::RecordedEnv) for its seeded
    /// fallback.
    pub(crate) fn answer(&mut self, point: &DecisionPoint) -> Answer {
        match point {
            DecisionPoint::Entropy { bytes } | DecisionPoint::Payload { bytes } => {
                Answer::Supply(self.supply_bytes(*bytes))
            }
            DecisionPoint::Scheduler { ready } => Answer::Supply(self.scheduler_pick(*ready)),
            DecisionPoint::NetFlow { .. } => {
                self.policy.sample(DecisionClass::NetFlow, &mut self.fault)
            }
            DecisionPoint::BlockIo { .. } => {
                self.policy.sample(DecisionClass::BlockIo, &mut self.fault)
            }
            DecisionPoint::Process { .. } => {
                self.policy.sample(DecisionClass::Process, &mut self.fault)
            }
            DecisionPoint::Buggify { point } => self.policy.sample_buggify(*point, &mut self.fault),
        }
    }

    /// Fill `bytes` (clamped to [`MAX_SUPPLY_LEN`]) from the supply stream. The
    /// clamp is defensive — the seam guarantees `bytes ≤ MAX_SUPPLY_LEN` — so a
    /// hostile point can never force an unbounded allocation.
    fn supply_bytes(&mut self, bytes: u32) -> Vec<u8> {
        let n = bytes.min(MAX_SUPPLY_LEN) as usize;
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            let word = self.supply.next_u64().to_le_bytes();
            let take = (n - out.len()).min(8);
            out.extend_from_slice(&word[..take]);
        }
        out
    }

    /// Pick a runnable index in `0..ready` (as a 4-byte little-endian `u32`). A
    /// degenerate `ready == 0` yields `0` rather than dividing by zero, so the
    /// path is total; the service is expected to clamp `ready ≥ 1`.
    fn scheduler_pick(&mut self, ready: u32) -> Vec<u8> {
        let w = self.supply.next_u64();
        let idx = if ready == 0 {
            0
        } else {
            (w % ready as u64) as u32
        };
        idx.to_le_bytes().to_vec()
    }
}

impl Environment for SeededEnv {
    fn decide(&mut self, point: &DecisionPoint) -> Outcome {
        Outcome::Resolved(self.answer(point))
    }
}
