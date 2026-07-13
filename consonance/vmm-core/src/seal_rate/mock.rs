// SPDX-License-Identifier: AGPL-3.0-or-later
//! A deterministic **mock snapshot oracle** for the portable seal-rate tests (task 63
//! gate 1) and the calibrated simulation the report quotes when the box run is deferred.
//!
//! It models the substrate's observed behavior without KVM:
//! - A `run` to a target lands at the next **synchronized boundary** at/after it (the
//!   `run` deadline only advances V-time at synchronized intercepts — see
//!   [`crate::vmm::Vmm::effective_vns`]), so a boundary-addressed ([`SampleKind::Uniform`])
//!   target lands synchronized and seals unless it draws a rare mid-RNG / unrepresentable
//!   / branch-nondeterministic outcome.
//! - A busy-window ([`SampleKind::Busy`]) target models the "less convenient" landing of
//!   §1/§3: a configurable chance of landing at a non-synchronized interior exit, plus a
//!   higher in-flight-injection rate (which task 41 nonetheless seals through).
//!
//! Every outcome is a pure `splitmix64` of the target V-time and a per-oracle `seed`, so a
//! whole sweep is reproducible and a proptest can drive thousands of configurations. Tune
//! [`MockConfig`] to the box's measured numbers to produce the report's simulation row.

use super::{
    BusyWindow, CpuSnapshot, FailureReason, Moment, SampleKind, SamplingSchedule, SealAttempt,
    SealResult, Span, Target, splitmix64,
};

/// Knobs describing the modeled substrate. All rates are integer parts-per-million.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockConfig {
    /// Spacing of synchronized V-time boundaries (a target lands at the next multiple of
    /// this at/after it). Smaller ⇒ a denser, more-addressable grid.
    pub sync_stride: Span,
    /// Chance a synchronized landing has a staged RNG completion (mid-exit fail-closed).
    pub rng_mid_exit_ppm: u32,
    /// Chance a landing carries unrepresentable CPU state (≈ 0 for the real 64-bit guest).
    pub unrepresentable_ppm: u32,
    /// Chance a *sealed* point then fails to branch deterministically (reclassified as a
    /// failure per §2). ≈ 0 if task 41 holds.
    pub branch_nondet_ppm: u32,
    /// Chance a busy-window target lands at a non-synchronized interior exit.
    pub busy_desync_ppm: u32,
    /// Chance any landing carries (captured, non-disqualifying) in-flight injection —
    /// higher inside busy windows.
    pub inflight_ppm: u32,
    /// Per-oracle seed folded into every draw.
    pub seed: u64,
}

impl Default for MockConfig {
    /// A "task-41 holds" default: a dense grid, essentially no unrepresentable or
    /// branch-nondeterministic points, a small mid-RNG rate, and busy windows that
    /// sometimes strand the guest at a non-synchronized interior exit.
    fn default() -> Self {
        MockConfig {
            sync_stride: 2_048,
            rng_mid_exit_ppm: 4_000,
            unrepresentable_ppm: 0,
            branch_nondet_ppm: 0,
            busy_desync_ppm: 300_000,
            inflight_ppm: 200_000,
            seed: 0x5EA1_A7E5_EED0_0063,
        }
    }
}

/// A deterministic seal oracle over a [`MockConfig`].
#[derive(Debug, Clone)]
pub struct MockOracle {
    /// The modeled substrate parameters.
    pub cfg: MockConfig,
    windows: Vec<BusyWindow>,
}

impl MockOracle {
    /// Build an oracle. `windows` are the busy windows the sweep targets (used to raise the
    /// in-flight rate for interior points that fall inside one).
    #[must_use]
    pub fn new(cfg: MockConfig, windows: &[BusyWindow]) -> Self {
        MockOracle {
            cfg,
            windows: windows.to_vec(),
        }
    }

    /// The next synchronized boundary at/after `target` (the modeled `run` landing).
    fn next_boundary(&self, target: Moment) -> Moment {
        let stride = self.cfg.sync_stride.max(1);
        // ceil(target / stride) * stride
        target.div_ceil(stride).saturating_mul(stride)
    }

    /// A `[0, PPM)` draw keyed on `(target, salt)` — deterministic, RNG-free.
    fn draw(&self, target: Moment, salt: u64) -> u32 {
        (splitmix64(target ^ self.cfg.seed ^ salt.wrapping_mul(0x100_0001B3)) % super::PPM as u64)
            as u32
    }

    fn in_a_window(&self, vtime: Moment) -> bool {
        self.windows
            .iter()
            .any(|w| vtime >= w.start && vtime < w.end)
    }

    /// Model one seal attempt at `target`.
    #[must_use]
    pub fn attempt(&self, target: Target) -> SealAttempt {
        let busy = matches!(target.kind, SampleKind::Busy(_));

        // Where does the run land, and is it synchronized?
        let (landed, synchronized) =
            if busy && self.draw(target.vtime, 0xB005) < self.cfg.busy_desync_ppm {
                // A busy target that stranded at a non-synchronized interior exit: the run
                // stepped a little way in without hitting a V-time intercept. Saturating so a
                // target near `u64::MAX` cannot overflow (library totality; `next_boundary`
                // above is already `saturating_mul`).
                let interior = target.vtime.saturating_add(
                    self.draw(target.vtime, 0x1D1E) as u64 % self.cfg.sync_stride.max(1),
                );
                (interior, false)
            } else {
                (self.next_boundary(target.vtime), true)
            };

        let inflight_bar = if busy || self.in_a_window(landed) {
            self.cfg.inflight_ppm.saturating_mul(2).min(super::PPM)
        } else {
            self.cfg.inflight_ppm
        };
        let inflight = self.draw(target.vtime, 0x1F17) < inflight_bar;
        let rng_mid = synchronized && self.draw(target.vtime, 0x2A2A) < self.cfg.rng_mid_exit_ppm;
        let unrep = self.draw(target.vtime, 0x3B3B) < self.cfg.unrepresentable_ppm;

        let snapshot = CpuSnapshot {
            synchronized,
            rng_mid_exit: rng_mid,
            unrepresentable: unrep,
            inflight_injection: inflight,
            // Half of in-flight points carry a *genuine* active injection (rest are residuals).
            active_injection: inflight && (self.draw(target.vtime, 0x4C4C) & 1 == 0),
            pending_guest_interrupt: inflight
                || (busy && self.draw(target.vtime, 0x5D5D) < 400_000),
        };

        // Seal outcome: fail closed exactly where save_vm_state would, then apply the
        // dynamic branch-determinism reclassification to points that sealed.
        let result = if !synchronized {
            SealResult::Failed(FailureReason::NonSynchronized)
        } else if unrep {
            SealResult::Failed(FailureReason::Unrepresentable)
        } else if rng_mid {
            SealResult::Failed(FailureReason::RngMidExit)
        } else if self.draw(target.vtime, 0x6E6E) < self.cfg.branch_nondet_ppm {
            SealResult::Failed(FailureReason::BranchNondeterministic)
        } else {
            SealResult::Sealed
        };

        SealAttempt {
            target,
            landed_vtime: landed,
            snapshot,
            result,
        }
    }

    /// Run the whole schedule through the oracle.
    #[must_use]
    pub fn sweep(&self, schedule: &SamplingSchedule) -> Vec<SealAttempt> {
        schedule
            .targets()
            .iter()
            .map(|&t| self.attempt(t))
            .collect()
    }
}
