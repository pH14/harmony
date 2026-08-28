// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic exit-count virtual clock.

use crate::VtimeError;

const NS_PER_SEC: u128 = 1_000_000_000;

/// Configuration for a [`VClock`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VClockConfig {
    /// Virtual counter frequency in Hz.
    pub guest_hz: u64,
    /// Guest counter value corresponding to virtual time zero.
    pub guest_base: u64,
    /// Initial virtual time in nanoseconds.
    pub vns_base: u64,
}

/// An integer-only virtual clock advanced explicitly by the VMM.
///
/// The clock has no host-time or instruction-count input. Each serviced VM
/// exit contributes its normalized, deterministic duration through
/// [`VClock::advance`]. Idle skipping uses the same operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VClock {
    cfg: VClockConfig,
    current_vns: u64,
}

impl VClock {
    /// Builds a clock from deterministic configuration.
    ///
    /// This constructor remains fallible so callers can propagate one stable
    /// initialization shape alongside [`TimerQueue`](crate::TimerQueue). The
    /// current configuration has no invalid bit pattern.
    pub fn new(cfg: VClockConfig) -> Result<Self, VtimeError> {
        Ok(Self {
            current_vns: cfg.vns_base,
            cfg,
        })
    }

    /// Current virtual time in nanoseconds.
    pub fn vns(&self) -> u64 {
        self.current_vns
    }

    /// Current guest-visible counter value.
    pub fn guest_ticks(&self) -> u64 {
        let ticks = u128::from(self.current_vns) * u128::from(self.cfg.guest_hz) / NS_PER_SEC;
        saturate(u128::from(self.cfg.guest_base) + ticks)
    }

    /// Advances virtual time by a deterministic delta, saturating at `u64::MAX`.
    pub fn advance(&mut self, delta_vns: u64) {
        self.current_vns = self.current_vns.saturating_add(delta_vns);
    }
}

fn saturate(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(kani)]
#[path = "clock_proofs.rs"]
mod proofs;

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(vns_base: u64) -> VClock {
        VClock::new(VClockConfig {
            guest_hz: 2_000_000_000,
            guest_base: 7,
            vns_base,
        })
        .expect("all clock configurations are valid")
    }

    #[test]
    fn explicit_advances_accumulate() {
        let mut clock = clock(100);
        clock.advance(3);
        clock.advance(9);
        assert_eq!(clock.vns(), 112);
        assert_eq!(clock.guest_ticks(), 231);
    }

    #[test]
    fn advance_saturates() {
        let mut clock = clock(u64::MAX - 1);
        clock.advance(2);
        assert_eq!(clock.vns(), u64::MAX);
        clock.advance(1);
        assert_eq!(clock.vns(), u64::MAX);
    }
}
