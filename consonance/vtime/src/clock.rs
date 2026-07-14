// SPDX-License-Identifier: AGPL-3.0-or-later
//! Work→time arithmetic: the [`VClock`] virtual clock.
//!
//! All arithmetic is integer-only, performed in `u128` intermediates, and
//! saturates to `u64::MAX` instead of overflowing (see the crate docs).

use crate::error::VtimeError;

const NS_PER_SEC: u128 = 1_000_000_000;

/// Configuration for a [`VClock`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VClockConfig {
    /// Numerator of the work→nanosecond ratio.
    pub ratio_num: u64,
    /// Denominator of the work→nanosecond ratio. Must be non-zero.
    pub ratio_den: u64,
    /// Virtual TSC frequency in Hz (e.g. `2_000_000_000`).
    pub guest_hz: u64,
    /// TSC value corresponding to `vns == 0` (snapshot restore sets it).
    pub guest_base: u64,
    /// V-time offset in nanoseconds; `0` for a fresh machine. Snapshot
    /// restore sets it to the snapshot's [`VClock::snapshot_vns`] value.
    pub vns_base: u64,
}

/// The virtual clock: a pure function from *work performed* (retired counted
/// events, see the crate docs) to guest-visible time.
///
/// `vns(work) = vns_base + floor(work * ratio_num / ratio_den)` and
/// `tsc(work) = guest_base + floor(vns(work) * guest_hz / 1_000_000_000)`, both
/// computed in `u128` and saturating to `u64::MAX`. Both are monotonic
/// non-decreasing in `work` by construction, and remain so across
/// [`VClock::advance_idle`] (which only ever moves `vns_base` forward).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VClock {
    cfg: VClockConfig,
}

impl VClock {
    /// Builds a clock, validating the config.
    ///
    /// # Errors
    ///
    /// - [`VtimeError::ZeroRatioDen`] if `ratio_den == 0`;
    /// - [`VtimeError::ZeroRatioNum`] if `ratio_num == 0` (V-time would never
    ///   advance, so [`VClock::work_for_vns`] would have no answer for any
    ///   future deadline);
    /// - [`VtimeError::ImmediateSaturation`] if the clock would saturate at a
    ///   trivially small work count, i.e. already `vns(1)` exceeds
    ///   `u64::MAX` (`vns_base + floor(ratio_num / ratio_den) > u64::MAX`).
    ///   Note `vns_base = 0, ratio = u64::MAX / 1` is *accepted*: `vns(1)`
    ///   equals `u64::MAX` exactly and only `vns(2)` saturates.
    pub fn new(cfg: VClockConfig) -> Result<VClock, VtimeError> {
        if cfg.ratio_den == 0 {
            return Err(VtimeError::ZeroRatioDen);
        }
        if cfg.ratio_num == 0 {
            return Err(VtimeError::ZeroRatioNum);
        }
        let step_vns = cfg.ratio_num / cfg.ratio_den;
        if u128::from(cfg.vns_base) + u128::from(step_vns) > u128::from(u64::MAX) {
            return Err(VtimeError::ImmediateSaturation {
                vns_base: cfg.vns_base,
                step_vns,
            });
        }
        Ok(VClock { cfg })
    }

    /// V-time in nanoseconds at the given work count:
    /// `vns_base + floor(work * ratio_num / ratio_den)`, computed in `u128`,
    /// saturating to `u64::MAX`. Monotonic non-decreasing in `work`.
    pub fn vns(&self, work: u64) -> u64 {
        let scaled =
            u128::from(work) * u128::from(self.cfg.ratio_num) / u128::from(self.cfg.ratio_den);
        // Max value: (2^64-1) + (2^64-1)^2 < 2^128, so this cannot overflow u128.
        saturate(u128::from(self.cfg.vns_base) + scaled)
    }

    /// Guest-clock ticks at the given work count:
    /// `guest_base + floor(vns(work) * guest_hz / 1_000_000_000)`, computed in
    /// `u128` from the (saturated, `u64`) [`VClock::vns`] value, saturating to
    /// `u64::MAX`. Monotonic non-decreasing in `work` because `vns` is.
    pub fn guest_ticks(&self, work: u64) -> u64 {
        let ticks = u128::from(self.vns(work)) * u128::from(self.cfg.guest_hz) / NS_PER_SEC;
        // ticks < 2^128 / 10^9 < 2^99, so adding guest_base cannot overflow u128.
        saturate(u128::from(self.cfg.guest_base) + ticks)
    }

    /// Smallest work count `w` with `vns(w) >= vns`; returns 0 if
    /// `vns <= vns_base` (a deadline already in the past, e.g. right after an
    /// idle warp made it current).
    ///
    /// # Exact formula
    ///
    /// For `vns > vns_base`, let `d = vns - vns_base`. We need the smallest
    /// `w` with `floor(w * num / den) >= d`. Because `d` is an integer,
    /// `floor(x) >= d ⇔ x >= d`, so the condition is
    /// `w * num >= d * den ⇔ w >= d * den / num`, i.e.
    /// `w = ceil(d * den / num)`, computed as `(d * den).div_ceil(num)` in
    /// `u128` (no overflow: `d * den <= (2^64-1)^2 < 2^128`).
    ///
    /// # Edge cases
    ///
    /// - `vns <= vns_base` → `0` (already due; specified above).
    /// - `vns` exactly on the clock grid (`vns == self.vns(w)` for some `w`)
    ///   → the smallest such `w`, and `self.vns(result) == vns` exactly.
    /// - `vns` unreachable within `u64` work (possible when `num < den` or
    ///   when `vns(u64::MAX)` saturates below `vns`): the ceil division
    ///   exceeds `u64` and the result **saturates to `u64::MAX`**; for such
    ///   targets `vns(work_for_vns(t)) >= t` does not hold (no work count
    ///   satisfies it — documented best-effort, never a panic).
    pub fn work_for_vns(&self, vns: u64) -> u64 {
        if vns <= self.cfg.vns_base {
            return 0;
        }
        let d = u128::from(vns - self.cfg.vns_base);
        let num = u128::from(self.cfg.ratio_num);
        let den = u128::from(self.cfg.ratio_den);
        saturate((d * den).div_ceil(num))
    }

    /// Idle-skip: warp V-time forward by `vns_delta` nanoseconds while work
    /// is frozen (the guest HLTed and the next timer deadline is ahead).
    ///
    /// Saturating add on `vns_base`; all invariants hold afterwards: `vns`
    /// and `tsc` at any fixed work count never decrease (the base only grows)
    /// and `tsc` remains exactly its defining formula applied to the new
    /// total `vns` (it is derived, never tracked separately).
    pub fn advance_idle(&mut self, vns_delta: u64) {
        self.cfg.vns_base = self.cfg.vns_base.saturating_add(vns_delta);
    }

    /// Effective V-time at `work`, for storing in a snapshot (equals
    /// `self.vns(work)`). Restoring is [`VClock::new`] with `vns_base` set to
    /// this value and the same `ratio`/`guest_hz`/`guest_base` — the hardware
    /// counter restarts at 0, so the restored clock carries the whole
    /// effective V-time in `vns_base`, and `tsc` continues consistently for
    /// free because it is derived from `vns`.
    ///
    /// Note V-time is quantized to whole nanoseconds here: with a fractional
    /// ratio (`ratio_den > 1`) the sub-nanosecond remainder
    /// `(work * ratio_num) mod ratio_den` is discarded, so a restored clock
    /// may lag an un-snapshotted run by at most 1 ns at the same point in
    /// the instruction stream (exactly 0 when `ratio_den == 1`).
    pub fn snapshot_vns(&self, work: u64) -> u64 {
        self.vns(work)
    }
}

/// Saturates a `u128` intermediate to `u64`, the crate-wide overflow rule.
fn saturate(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Formal proof harnesses (bounded model checking via Kani).
///
/// Compiled only under `cargo kani` (the `kani` cfg). These prove the
/// "never panics / law holds for ALL inputs" claims that the `vtime` docs
/// make about the saturating `u128` arithmetic — strictly stronger than the
/// proptest sampling in `tests/`. Each harness documents the bound it places
/// on its symbolic inputs and why that bound preserves the property.
///
/// See `IMPLEMENTATION.md` ("Formal proofs (Kani)") for the harness catalogue.
#[cfg(kani)]
#[path = "clock_proofs.rs"]
mod proofs;

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(ratio_num: u64, ratio_den: u64, vns_base: u64) -> VClock {
        VClock::new(VClockConfig {
            ratio_num,
            ratio_den,
            guest_hz: 2_000_000_000,
            guest_base: 0,
            vns_base,
        })
        .expect("valid test config")
    }

    #[test]
    fn rejects_zero_den() {
        let err = VClock::new(VClockConfig {
            ratio_num: 1,
            ratio_den: 0,
            guest_hz: 1,
            guest_base: 0,
            vns_base: 0,
        })
        .unwrap_err();
        assert_eq!(err, VtimeError::ZeroRatioDen);
    }

    #[test]
    fn rejects_zero_num() {
        let err = VClock::new(VClockConfig {
            ratio_num: 0,
            ratio_den: 1,
            guest_hz: 1,
            guest_base: 0,
            vns_base: 0,
        })
        .unwrap_err();
        assert_eq!(err, VtimeError::ZeroRatioNum);
    }

    #[test]
    fn rejects_immediate_saturation_but_accepts_exact_max() {
        // vns(1) would be u64::MAX + 1.
        let err = VClock::new(VClockConfig {
            ratio_num: u64::MAX,
            ratio_den: 1,
            guest_hz: 1,
            guest_base: 0,
            vns_base: 1,
        })
        .unwrap_err();
        assert_eq!(
            err,
            VtimeError::ImmediateSaturation {
                vns_base: 1,
                step_vns: u64::MAX
            }
        );

        // vns(1) == u64::MAX exactly: accepted (only vns(2) saturates).
        let clk = clock(u64::MAX, 1, 0);
        assert_eq!(clk.vns(1), u64::MAX);
        assert_eq!(clk.vns(2), u64::MAX);
    }

    #[test]
    fn vns_matches_formula() {
        let clk = clock(3, 2, 100);
        assert_eq!(clk.vns(0), 100);
        assert_eq!(clk.vns(1), 101); // floor(3/2) = 1
        assert_eq!(clk.vns(2), 103);
        assert_eq!(clk.vns(3), 104);
    }

    #[test]
    fn tsc_matches_formula() {
        // 2 GHz: 1 ns = 2 ticks.
        let clk = clock(5, 1, 0);
        assert_eq!(clk.guest_ticks(10), 100); // vns = 50 ns -> 100 ticks
        let clk = VClock::new(VClockConfig {
            ratio_num: 1,
            ratio_den: 1,
            guest_hz: 1_500_000_000,
            guest_base: 7,
            vns_base: 0,
        })
        .unwrap();
        assert_eq!(clk.guest_ticks(2), 7 + 3); // floor(2 * 1.5)
        assert_eq!(clk.guest_ticks(3), 7 + 4); // floor(3 * 1.5)
    }

    #[test]
    fn work_for_vns_edges() {
        let clk = clock(3, 2, 100);
        // Past (or exactly current) deadlines map to work 0.
        assert_eq!(clk.work_for_vns(0), 0);
        assert_eq!(clk.work_for_vns(100), 0);
        // First future nanosecond needs ceil(1 * 2 / 3) = 1 work.
        assert_eq!(clk.work_for_vns(101), 1);
        // On-grid target is hit exactly by the smallest work.
        assert_eq!(clk.work_for_vns(103), 2);
        assert_eq!(clk.vns(clk.work_for_vns(103)), 103);
        // Off-grid target: smallest w with vns(w) >= t overshoots minimally.
        assert_eq!(clk.work_for_vns(102), 2);
        assert_eq!(clk.vns(2), 103);
        assert_eq!(clk.vns(1), 101);
    }

    #[test]
    fn work_for_vns_unreachable_saturates() {
        let clk = clock(1, 1000, 0);
        // vns(u64::MAX) ~ u64::MAX / 1000 < u64::MAX: target is unreachable.
        assert_eq!(clk.work_for_vns(u64::MAX), u64::MAX);
        assert!(clk.vns(u64::MAX) < u64::MAX);
    }

    #[test]
    fn advance_idle_saturates() {
        let mut clk = clock(1, 1, 10);
        clk.advance_idle(u64::MAX);
        assert_eq!(clk.vns(0), u64::MAX);
        assert_eq!(clk.vns(5), u64::MAX); // still monotone after saturation
    }

    #[test]
    fn snapshot_vns_equals_vns() {
        let clk = clock(7, 3, 42);
        assert_eq!(clk.snapshot_vns(1234), clk.vns(1234));
    }
}
