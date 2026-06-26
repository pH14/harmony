// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 1 — timer round-trip property tests.
//!
//! For arbitrary `(timer_hz, divide, initial_count, now_vns, mode)`: arming at
//! `t0` makes `next_timer_deadline()` exactly `t0 + ceil(N·divide·1e9/timer_hz)`;
//! the Current Count read at `t0` is exactly `N` (the arming-instant round trip —
//! must hold for *every* `timer_hz`, including non-dividing ones like
//! `24_000_000`); the Current Count at `t0 + Δ` equals `N − floor(Δ·timer_hz /
//! (divide·1e9))`, monotonically non-increasing and 0 at/after the deadline; and
//! a periodic timer lands its vector in IRR once per period with exact re-arm
//! instants.

use lapic::{APIC_LVT_TIMER, APIC_SVR, APIC_TDCR, APIC_TMCCT, APIC_TMICT, Lapic, LapicConfig};
use proptest::prelude::*;

const NS_PER_SEC: u128 = 1_000_000_000;
const SVR_ENABLE: u32 = 1 << 8;

// LVT-timer mode field values (bits 18:17).
const MODE_ONESHOT: u32 = 0b00;
const MODE_PERIODIC: u32 = 0b01;

/// Reference divide-config decoder (mirrors the crate-private one): bits [3,1,0]
/// select the divisor, bit 2 ignored; `0b111` is ÷1.
fn divide_value(tdcr: u32) -> u64 {
    let sel = ((tdcr & 0b1000) >> 1) | (tdcr & 0b11);
    if sel == 0b111 { 1 } else { 2u64 << sel }
}

/// Reference period: `ceil(N·divide·1e9 / timer_hz)`, saturating to `u64::MAX`.
fn period_vns(timer_hz: u64, divide: u64, n: u32) -> u64 {
    let numer = u128::from(n) * u128::from(divide) * NS_PER_SEC;
    u64::try_from(numer.div_ceil(u128::from(timer_hz))).unwrap_or(u64::MAX)
}

/// Reference elapsed ticks over `delta` ns: `floor(Δ·timer_hz / (divide·1e9))`,
/// saturating to `u32::MAX`.
fn elapsed_ticks(timer_hz: u64, divide: u64, delta: u64) -> u32 {
    let ticks = (u128::from(delta) * u128::from(timer_hz)) / (u128::from(divide) * NS_PER_SEC);
    u32::try_from(ticks).unwrap_or(u32::MAX)
}

/// Build an LVT-timer register value.
fn lvt_timer(vector: u8, mode: u32, masked: bool) -> u32 {
    u32::from(vector) | (mode << 17) | (if masked { 1 << 16 } else { 0 })
}

/// A software-enabled LAPIC with the timer programmed (divide, LVT mode/vector)
/// but not yet armed.
fn armed_setup(timer_hz: u64, tdcr: u32, vector: u8, mode: u32) -> Lapic {
    let mut l = Lapic::new(LapicConfig {
        apic_id: 0,
        timer_hz,
    })
    .expect("non-zero timer_hz");
    l.mmio_write(APIC_SVR, 0xFF | SVR_ENABLE, 0).unwrap();
    l.mmio_write(APIC_TDCR, tdcr, 0).unwrap();
    l.mmio_write(APIC_LVT_TIMER, lvt_timer(vector, mode, false), 0)
        .unwrap();
    l
}

/// A strategy biased toward tricky frequencies (non-dividing, the 25 MHz
/// crystal, extremes) plus a broad random span.
fn timer_hz_strategy() -> impl Strategy<Value = u64> {
    prop_oneof![
        Just(1u64),
        Just(24_000_000u64), // non-dividing — the historical round-trip trap
        Just(25_000_000u64), // the frozen crystal
        Just(1_000_000_000u64),
        Just(u64::MAX),
        1u64..=4_000_000_000u64,
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Arming-instant round trip + deadline formula + count decay, for arbitrary
    /// inputs and *every* `timer_hz`.
    #[test]
    fn deadline_and_count_round_trip(
        timer_hz in timer_hz_strategy(),
        tdcr in 0u32..=0xF,
        n in 1u32..=u32::MAX,
        t0 in 0u64..=1_000_000_000_000u64,
        delta in 0u64..=u64::MAX,
        vector in 16u8..=255,
        periodic in any::<bool>(),
    ) {
        let mode = if periodic { MODE_PERIODIC } else { MODE_ONESHOT };
        let divide = divide_value(tdcr);
        let mut l = armed_setup(timer_hz, tdcr, vector, mode);

        // Arm at t0.
        l.mmio_write(APIC_TMICT, n, t0).unwrap();

        // Deadline is the exact `t0 + ceil(...)`, or `None` when that overflows
        // u64 (an unrepresentable, never-firing deadline — PR #38 re-review).
        let true_period = (u128::from(n) * u128::from(divide) * NS_PER_SEC)
            .div_ceil(u128::from(timer_hz));
        let true_deadline = u128::from(t0) + true_period;
        let period_saturated = true_deadline > u128::from(u64::MAX);
        if period_saturated {
            prop_assert_eq!(l.next_timer_deadline(), None);
        } else {
            prop_assert_eq!(l.next_timer_deadline(), Some(true_deadline as u64));
        }

        // Arming-instant round trip: count at t0 is exactly N, for every timer_hz.
        prop_assert_eq!(l.mmio_read(APIC_TMCCT, t0).unwrap(), n);

        // Count at t0 + delta equals N - floor(elapsed ticks).
        let now = t0.saturating_add(delta);
        let real_delta = now - t0; // saturating add may have clamped delta
        let want_count = n.saturating_sub(elapsed_ticks(timer_hz, divide, real_delta));
        prop_assert_eq!(l.mmio_read(APIC_TMCCT, now).unwrap(), want_count);

        // Monotonic non-increasing in elapsed time.
        let earlier = l.mmio_read(APIC_TMCCT, t0).unwrap();
        prop_assert!(earlier >= want_count);

        // Zero at/after the deadline (unless the deadline overflowed u64, in
        // which case fewer than N ticks can elapse within representable time).
        if !period_saturated && u128::from(now) >= true_deadline {
            prop_assert_eq!(l.mmio_read(APIC_TMCCT, now).unwrap(), 0);
        }
    }

    /// Writing initial-count 0 disarms; a masked or software-disabled timer does
    /// not arm and produces no deadline.
    #[test]
    fn disarm_and_masked_paths(
        timer_hz in timer_hz_strategy(),
        tdcr in 0u32..=0xF,
        n in 1u32..=u32::MAX,
        vector in 16u8..=255,
    ) {
        // Zero count disarms even when otherwise armable. (Use TMCCT == N at the
        // arming instant as the "armed and counting" witness — it holds for every
        // timer_hz, whereas next_timer_deadline can be None on a saturated one.)
        let mut l = armed_setup(timer_hz, tdcr, vector, MODE_ONESHOT);
        l.mmio_write(APIC_TMICT, n, 0).unwrap();
        prop_assert_eq!(l.mmio_read(APIC_TMCCT, 0).unwrap(), n);
        l.mmio_write(APIC_TMICT, 0, 0).unwrap();
        prop_assert_eq!(l.next_timer_deadline(), None);
        prop_assert_eq!(l.mmio_read(APIC_TMCCT, 1_000_000).unwrap(), 0);

        // Masked LVT timer: a non-zero write does not arm.
        let mut m = Lapic::new(LapicConfig { apic_id: 0, timer_hz }).unwrap();
        m.mmio_write(APIC_SVR, 0xFF | SVR_ENABLE, 0).unwrap();
        m.mmio_write(APIC_TDCR, tdcr, 0).unwrap();
        m.mmio_write(APIC_LVT_TIMER, lvt_timer(vector, MODE_ONESHOT, true), 0).unwrap();
        m.mmio_write(APIC_TMICT, n, 0).unwrap();
        prop_assert_eq!(m.next_timer_deadline(), None);

        // Software-disabled APIC: a non-zero write does not arm.
        let mut d = Lapic::new(LapicConfig { apic_id: 0, timer_hz }).unwrap();
        d.mmio_write(APIC_TDCR, tdcr, 0).unwrap();
        d.mmio_write(APIC_LVT_TIMER, lvt_timer(vector, MODE_ONESHOT, false), 0).unwrap();
        d.mmio_write(APIC_TMICT, n, 0).unwrap();
        prop_assert_eq!(d.next_timer_deadline(), None);
    }

    /// `advance_to` is idempotent for a given `now_vns`: a repeat call at the
    /// same V-time never re-fires or changes state — including at the `u64::MAX`
    /// saturation boundary, where the deadline clamps and a naive `now >=
    /// deadline` fire would loop (PR #38 regression).
    #[test]
    fn advance_to_is_idempotent(
        timer_hz in timer_hz_strategy(),
        tdcr in 0u32..=0xF,
        n in 1u32..=u32::MAX,
        vector in 16u8..=255,
        periodic in any::<bool>(),
        arm in prop_oneof![
            Just(u64::MAX),
            Just(u64::MAX - 1),
            Just(u64::MAX - 100_000),
            0u64..=u64::MAX,
        ],
        now in prop_oneof![Just(u64::MAX), Just(u64::MAX - 1), 0u64..=u64::MAX],
    ) {
        let mode = if periodic { MODE_PERIODIC } else { MODE_ONESHOT };
        let mut l = armed_setup(timer_hz, tdcr, vector, mode);
        l.mmio_write(APIC_TMICT, n, arm).unwrap();

        let _ = l.advance_to(now);
        let after_first = l.snapshot();
        let second = l.advance_to(now);
        prop_assert!(!second, "repeat advance_to at the same now_vns must be a no-op");
        prop_assert_eq!(l.snapshot(), after_first);
    }

    /// At the saturation boundary, `next_timer_deadline` reports `None` (not a
    /// clamped `u64::MAX`) and `advance_to` never fires — so a `TimerQueue`
    /// caller cannot loop on a due-but-never-firing timer (PR #38 re-review).
    #[test]
    fn unrepresentable_deadline_is_none_and_never_fires(
        timer_hz in timer_hz_strategy(),
        tdcr in 0u32..=0xF,
        n in 1u32..=u32::MAX,
        vector in 16u8..=255,
        periodic in any::<bool>(),
        arm in (u64::MAX - 4_000_000_000)..=u64::MAX,
    ) {
        let mode = if periodic { MODE_PERIODIC } else { MODE_ONESHOT };
        let divide = divide_value(tdcr);
        let mut l = armed_setup(timer_hz, tdcr, vector, mode);
        l.mmio_write(APIC_TMICT, n, arm).unwrap();

        let true_period = (u128::from(n) * u128::from(divide) * NS_PER_SEC)
            .div_ceil(u128::from(timer_hz));
        let true_deadline = u128::from(arm) + true_period;

        if true_deadline > u128::from(u64::MAX) {
            prop_assert_eq!(l.next_timer_deadline(), None);
            prop_assert!(!l.advance_to(u64::MAX));
            prop_assert!(!l.has_deliverable());
        } else {
            prop_assert_eq!(l.next_timer_deadline(), Some(true_deadline as u64));
        }
    }

    /// A count loaded while the LVT timer is masked arms on unmask, counting from
    /// the unmask instant (PR #38 re-review).
    #[test]
    fn masked_load_then_unmask_arms(
        timer_hz in timer_hz_strategy(),
        tdcr in 0u32..=0xF,
        n in 1u32..=u32::MAX,
        vector in 16u8..=255,
        unmask_at in 0u64..=1_000_000_000u64,
    ) {
        let mut l = Lapic::new(LapicConfig { apic_id: 0, timer_hz }).unwrap();
        l.mmio_write(APIC_SVR, 0xFF | SVR_ENABLE, 0).unwrap();
        l.mmio_write(APIC_TDCR, tdcr, 0).unwrap();
        // Masked one-shot; the count loaded while masked must not arm yet.
        l.mmio_write(APIC_LVT_TIMER, lvt_timer(vector, MODE_ONESHOT, true), 0).unwrap();
        l.mmio_write(APIC_TMICT, n, 0).unwrap();
        prop_assert_eq!(l.next_timer_deadline(), None);
        // Unmask: arms at `unmask_at`, so the count reads exactly N there.
        l.mmio_write(APIC_LVT_TIMER, lvt_timer(vector, MODE_ONESHOT, false), unmask_at)
            .unwrap();
        prop_assert_eq!(l.mmio_read(APIC_TMCCT, unmask_at).unwrap(), n);
    }

    /// One-shot fires exactly once and then stops.
    #[test]
    fn oneshot_fires_once(
        timer_hz in timer_hz_strategy(),
        tdcr in 0u32..=0xF,
        n in 1u32..=1_000_000u32,
        vector in 16u8..=255,
    ) {
        let divide = divide_value(tdcr);
        let mut l = armed_setup(timer_hz, tdcr, vector, MODE_ONESHOT);
        l.mmio_write(APIC_TMICT, n, 0).unwrap();
        let deadline = period_vns(timer_hz, divide, n); // t0 = 0

        // Before the deadline: not fired.
        prop_assert!(!l.has_deliverable() || deadline == 0);

        // At the deadline: fires once, raising the vector, then stops.
        let changed = l.advance_to(deadline);
        prop_assert!(changed);
        prop_assert_eq!(l.take_interrupt(), Some(vector));
        l.eoi();
        prop_assert_eq!(l.next_timer_deadline(), None);

        // Advancing further does nothing (idempotent / stopped).
        prop_assert!(!l.advance_to(deadline.saturating_add(1)));
    }

    /// Periodic: the LVT-timer vector lands in IRR exactly once per period, and
    /// each re-arm instant is exact (drift-free cadence).
    #[test]
    fn periodic_fires_each_period(
        timer_hz in timer_hz_strategy(),
        tdcr in 0u32..=0xF,
        n in 1u32..=1_000_000u32,
        vector in 16u8..=255,
    ) {
        let divide = divide_value(tdcr);
        let period = period_vns(timer_hz, divide, n); // t0 = 0
        // Keep the multi-period walk inside u64 (period <= 1.28e17, * 5 fits).
        prop_assume!(period <= u64::MAX / 8);

        let mut l = armed_setup(timer_hz, tdcr, vector, MODE_PERIODIC);
        l.mmio_write(APIC_TMICT, n, 0).unwrap();

        for i in 1u64..=5 {
            let deadline = period * i;
            // Re-arm instant is exact each period.
            prop_assert_eq!(l.next_timer_deadline(), Some(deadline));

            // Just before the deadline: not yet fired this period.
            prop_assert_eq!(l.take_interrupt(), None);

            // At the deadline: fires once.
            prop_assert!(l.advance_to(deadline));
            prop_assert_eq!(l.take_interrupt(), Some(vector));
            l.eoi();
        }
        // After the walk the timer is still armed for the next period.
        prop_assert_eq!(l.next_timer_deadline(), Some(period * 6));
    }
}

// --- Full timer-lifecycle model check (PR #38, the 6th timer bug) -----------
//
// Drives an arbitrary interleaving of {Initial-Count, LVT-timer, Divide-Config,
// SVR-enable, advance_to} against an independent reference model implementing the
// unified timer semantics (one re-arm path; a mid-count divide change re-anchors
// from the *current remaining count* rather than applying the new divisor
// retroactively). Comparing every `advance_to` fire-decision, the advertised
// deadline, and the Current Count after every op catches the whole timer
// lifecycle class. It also asserts the invariants directly: remaining count is
// monotonic non-increasing while only time advances; the vector fires exactly
// once per arm; a config change never makes a not-yet-due timer fire (no
// retroactive deadline jump).

/// Independent reference for the unified timer model. `count_at_arm` is the count
/// remaining at `arm_vns` (the anchor); the deadline / Current Count are measured
/// from it, so a divide change re-anchors instead of rewriting history.
struct TimerRef {
    timer_hz: u64,
    enabled: bool,
    masked: bool,
    mode: u32,
    divide_config: u32,
    initial_count: u32,
    count_at_arm: u32,
    pending: bool,
    arm_vns: u64,
    running: bool,
    now: u64,
}

impl TimerRef {
    /// A fresh `Lapic::new`: software-disabled, LVT timer masked (mode 0), no
    /// count loaded.
    fn fresh(timer_hz: u64) -> Self {
        TimerRef {
            timer_hz,
            enabled: false,
            masked: true,
            mode: 0,
            divide_config: 0,
            initial_count: 0,
            count_at_arm: 0,
            pending: false,
            arm_vns: 0,
            running: false,
            now: 0,
        }
    }

    fn armable(&self) -> bool {
        self.pending && self.enabled && !self.masked && (self.mode == 0 || self.mode == 1)
    }

    fn period_for(&self, count: u32) -> u128 {
        let d = u128::from(divide_value(self.divide_config));
        (u128::from(count) * d * NS_PER_SEC).div_ceil(u128::from(self.timer_hz))
    }

    fn elapsed_ticks(&self, delta: u64) -> u32 {
        let d = u128::from(divide_value(self.divide_config));
        let ticks = (u128::from(delta) * u128::from(self.timer_hz)) / (d * NS_PER_SEC);
        u32::try_from(ticks).unwrap_or(u32::MAX)
    }

    /// Current Count: remaining when running, else 0.
    fn current_count(&self) -> u32 {
        if !self.running {
            return 0;
        }
        let elapsed = self.now.saturating_sub(self.arm_vns);
        self.count_at_arm
            .saturating_sub(self.elapsed_ticks(elapsed))
    }

    fn running_remaining(&self) -> Option<u32> {
        self.running.then(|| self.current_count())
    }

    /// The one re-arm path (mirrors `Lapic::retime`).
    fn retime(&mut self, prior_remaining: Option<u32>, old_divide: u64) {
        if !self.armable() {
            self.running = false;
            return;
        }
        match prior_remaining {
            Some(rem) if divide_value(self.divide_config) != old_divide => {
                self.count_at_arm = rem;
                self.arm_vns = self.now;
            }
            Some(_) => {}
            None => {
                self.count_at_arm = self.initial_count;
                self.arm_vns = self.now;
            }
        }
        self.running = true;
    }

    /// Initial Count write: a fresh arm of the new count.
    fn arm(&mut self, n: u32) {
        self.initial_count = n;
        self.pending = n != 0;
        self.retime(None, divide_value(self.divide_config));
    }

    /// A config change (LVT mask/mode, SVR enable, or Divide): capture the prior
    /// remaining + divisor, apply, then re-time.
    fn config_write(&mut self, apply: impl FnOnce(&mut Self)) {
        let prior = self.running_remaining();
        let old_divide = divide_value(self.divide_config);
        apply(self);
        self.retime(prior, old_divide);
    }

    fn deadline(&self) -> Option<u64> {
        if !self.running {
            return None;
        }
        u64::try_from(u128::from(self.arm_vns) + self.period_for(self.count_at_arm)).ok()
    }

    /// Returns whether the timer fired at `t` (mirrors `advance_to`'s bool).
    fn advance(&mut self, t: u64) -> bool {
        self.now = t;
        if !self.running {
            return false;
        }
        let seg = self.period_for(self.count_at_arm);
        let elapsed = u128::from(t.saturating_sub(self.arm_vns));
        if elapsed < seg {
            return false;
        }
        if self.mode == 1 {
            let full = self.period_for(self.initial_count);
            let after_first = u128::from(self.arm_vns) + seg;
            let k = (u128::from(t).saturating_sub(after_first)) / full;
            self.arm_vns = u64::try_from(after_first + k * full).unwrap_or(u64::MAX);
            self.count_at_arm = self.initial_count;
        } else {
            self.running = false;
            self.pending = false;
        }
        true
    }
}

#[derive(Clone, Debug)]
enum LifecycleOp {
    Arm(u32),
    Lvt { masked: bool, mode: u32 },
    Enable(bool),
    Divide(u32),
    Advance(u64),
}

fn lifecycle_op() -> impl Strategy<Value = LifecycleOp> {
    prop_oneof![
        3 => (0u32..=100_000).prop_map(LifecycleOp::Arm), // includes 0 (disarm)
        2 => (any::<bool>(), 0u32..=3).prop_map(|(masked, mode)| LifecycleOp::Lvt { masked, mode }),
        2 => any::<bool>().prop_map(LifecycleOp::Enable),
        2 => (0u32..=0xF).prop_map(LifecycleOp::Divide),
        3 => (0u64..=600_000_000).prop_map(LifecycleOp::Advance),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn timer_lifecycle_matches_reference(
        timer_hz in timer_hz_strategy(),
        ops in prop::collection::vec(lifecycle_op(), 1..60),
    ) {
        const V: u8 = 0x40; // fixed timer vector across the lifecycle
        let mut l = Lapic::new(LapicConfig { apic_id: 0, timer_hz }).unwrap();
        let mut r = TimerRef::fresh(timer_hz);
        let mut now = 0u64;

        for op in ops.iter().cloned() {
            r.now = now;
            // Whether this op is itself a (re-)arm — the only time the remaining
            // count is allowed to increase.
            let is_advance = matches!(op, LifecycleOp::Advance(_));
            let before_remaining = l.mmio_read(APIC_TMCCT, now).unwrap();
            let before_deadline = l.next_timer_deadline();

            match op {
                LifecycleOp::Arm(n) => {
                    l.mmio_write(APIC_TMICT, n, now).unwrap();
                    r.arm(n);
                }
                LifecycleOp::Lvt { masked, mode } => {
                    l.mmio_write(APIC_LVT_TIMER, lvt_timer(V, mode, masked), now).unwrap();
                    r.config_write(|s| { s.masked = masked; s.mode = mode; });
                }
                LifecycleOp::Enable(e) => {
                    let svr = if e { 0xFF | SVR_ENABLE } else { 0xFF };
                    l.mmio_write(APIC_SVR, svr, now).unwrap();
                    r.config_write(|s| s.enabled = e);
                }
                LifecycleOp::Divide(dc) => {
                    l.mmio_write(APIC_TDCR, dc, now).unwrap();
                    // Bit 2 is decode-ignored and dropped at storage, mirroring
                    // the device's `TDCR_WRITE_MASK` (the divisor is unaffected).
                    r.config_write(|s| s.divide_config = dc & 0xB);
                }
                LifecycleOp::Advance(dt) => {
                    now = now.saturating_add(dt);
                    r.now = now;
                    let real = l.advance_to(now);
                    let model = r.advance(now);
                    prop_assert_eq!(real, model, "fire mismatch at now={}", now);
                    // Invariant: while only time advances and the timer does not
                    // fire, the remaining count is monotonic non-increasing.
                    if !real {
                        let after = l.mmio_read(APIC_TMCCT, now).unwrap();
                        prop_assert!(after <= before_remaining,
                            "remaining increased without a re-arm: {before_remaining} -> {after}");
                    }
                }
            }

            // The reference model matches on the deadline and the Current Count.
            prop_assert_eq!(l.next_timer_deadline(), r.deadline());
            prop_assert_eq!(l.mmio_read(APIC_TMCCT, now).unwrap(), r.current_count());

            // No retroactive deadline jump: a *config* change that leaves the
            // timer running must not make a not-yet-due timer due at `now` (the
            // 6th-bug class — a TDCR change must reschedule, never fire in the
            // past). If it wasn't due before, advancing to the same `now` is a
            // no-op.
            if !is_advance {
                let was_due = before_deadline.is_some_and(|d| d <= now);
                if !was_due {
                    let mut probe = l.clone();
                    prop_assert!(!probe.advance_to(now),
                        "a config change made a not-yet-due timer fire retroactively");
                }
            }
        }
    }
}
