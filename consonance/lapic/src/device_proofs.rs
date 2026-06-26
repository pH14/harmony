// SPDX-License-Identifier: AGPL-3.0-or-later
//! Kani proof harnesses for the timer arithmetic and priority logic
//! (gate 4), split out of `device.rs` so cargo-mutants can glob-exclude them:
//! they are `#[cfg(kani)]` and verified by the dedicated `kani` CI job, not the
//! mutation oracle. Declared as `#[cfg(kani)] #[path = "device_proofs.rs"] mod
//! proofs;` in `device.rs`, so it is a child of `device` (`use super::*` reaches
//! the private helpers it verifies).
//!
//! ## Why the bounds (the CBMC cost model)
//!
//! CBMC bit-blasts arithmetic into a SAT instance whose cost is driven by
//! **operator width and kind**, largely independent of value-range `assume`s
//! (the same lesson `vtime::clock_proofs` documents):
//!
//! - A **symbolic ÷ symbolic** or **symbolic × symbolic** at `u128` width
//!   explodes the instance. The timer divisions (`÷ timer_hz`,
//!   `÷ (divide·1e9)`) and the `N·divide·1e9` / `Δ·timer_hz` products are
//!   `u128`, so the harnesses pin `timer_hz` to a concrete representative
//!   (25 MHz — the frozen crystal of `docs/CPU-MSR-CONTRACT.md`, or 1 for the
//!   no-panic / huge-period harnesses) and either pin the divide config or
//!   iterate it over **concrete** values. Each product then has a constant
//!   operand and each division a constant divisor, which CBMC folds into cheap
//!   shift/reciprocal-multiply.
//! - **Exact-equality** assertions across a `u128` divide are far costlier than
//!   inequalities, so the exact-ceil / exact-count harnesses bound the symbolic
//!   `N`/`Δ` operand tightly (12 bits — enough to exercise every rounding/carry
//!   path; larger operands only repeat the same arithmetic), while the
//!   no-panic harness keeps `N` full-`u32` because its `÷ 1` is trivial for
//!   CBMC (no exact-value assertion to bit-pin).
//!
//! The divide-config decode is proven for **all** `u32` inputs separately
//! ([`divide_value_total`]), so the concrete-divisor arithmetic harnesses
//! compose to "for every legal divisor".
//!
//! Harness runtimes are recorded in `IMPLEMENTATION.md`.

use super::*;

/// Tight bound (12 bits) for the symbolic `N`/`Δ` operands in the exact-equality
/// harnesses; keeps the `u128` constant-divisor divides fast while still driving
/// the full multiply/divide/ceil/floor/saturate rounding path.
const EXACT_BOUND: u64 = (1 << 12) - 1;

/// Representative concrete timer frequency: 25 MHz, the frozen core-crystal /
/// LAPIC-timer input clock of `docs/CPU-MSR-CONTRACT.md` §5.
const PROOF_TIMER_HZ: u64 = 25_000_000;

/// The eight divide-config encodings (bit 2 cleared) that map to the eight
/// distinct divisors ÷2, ÷4, ÷8, ÷16, ÷32, ÷64, ÷128, ÷1 — iterated concretely
/// so each `divide_value` result is a constant in the surrounding `u128`
/// arithmetic. Setting bit 2 only duplicates these (proven in
/// [`divide_value_total`]).
const DIVIDE_CONFIGS: [u32; 8] = [
    0b0000, 0b0001, 0b0010, 0b0011, 0b1000, 0b1001, 0b1010, 0b1011,
];

/// Build a running-timer `Lapic` with the given timer parameters; all other
/// registers are at reset-ish zero values (irrelevant to the timer arithmetic).
fn timer_lapic(timer_hz: u64, divide_config: u32, initial_count: u32, arm_vns: u64) -> Lapic {
    Lapic {
        id: 0,
        timer_hz,
        tpr: 0,
        svr: 0,
        ldr: 0,
        dfr: 0,
        esr: 0,
        icr_low: 0,
        icr_high: 0,
        divide_config,
        isr: [0; 8],
        tmr: [0; 8],
        irr: [0; 8],
        lvt: [LVT_RESET; 6],
        initial_count,
        count_at_arm: initial_count, // fresh-armed: anchor count == initial count
        timer_arm_vns: arm_vns,
        timer_running: true,
        timer_pending: true,
    }
}

/// `divide_value` is total and bounded for **every** `u32` config: it never
/// panics (no shift overflow) and always returns one of the eight legal
/// divisors. Cheap because it contains no multiply or divide — just a shift and
/// a select — so the config stays fully symbolic.
#[kani::proof]
fn divide_value_total() {
    let cfg: u32 = kani::any();
    let d = divide_value(cfg);
    assert!(matches!(d, 1 | 2 | 4 | 8 | 16 | 32 | 64 | 128));
}

/// The period computation **never panics for any `u32` initial count**: the
/// `u128` multiply `N·divide·1e9` cannot overflow (`u32 · ≤128 · 1e9 < 2^128`)
/// and the `÷ timer_hz` never traps. Proven over **all** `u32` `N` and **all**
/// divide encodings; `timer_hz` is pinned to the trivial `1` so CBMC need not
/// build a symbolic 128-bit divider — the property under test is the multiply,
/// not the division. (No exact-value assertion here, so no bit-pinning across
/// the multiply: this stays fast at full `u32` width.)
#[kani::proof]
fn period_never_panics_any_count() {
    let n: u32 = kani::any();
    let divide_config: u32 = kani::any();
    let l = timer_lapic(1, divide_config, n, 0);
    let _ = l.period_for(l.count_at_arm); // must not panic / overflow for any input
}

/// A timer whose period exceeds `u64` reports **no deadline** (PR #38 fix #3):
/// with a large initial count at ÷128 and `timer_hz == 1`, the true period
/// `N·128·1e9` overflows `u64`, so `next_timer_deadline` returns `None` rather
/// than a clamped `u64::MAX` that `advance_to` would never fire. Concrete inputs
/// (cf. `vtime`'s `tsc_saturates`) so everything constant-folds.
#[kani::proof]
#[kani::unwind(4)]
fn huge_period_reports_no_deadline() {
    for n in [2_000_000_000u32, 3_000_000_000, u32::MAX] {
        // ÷128 (config 0b1010), timer_hz = 1: period = N·128·1e9 > u64::MAX.
        let l = running_periodic_lapic(1, 0b1010, n, 0);
        assert!(l.period_for(l.count_at_arm) > u128::from(u64::MAX));
        assert_eq!(l.next_timer_deadline(), None);
    }
}

/// A representative non-trivial divide config: ÷16 (selector `0b011`). The exact
/// arithmetic harnesses use this single concrete divisor so CBMC need not unroll
/// an 8-way loop around a 12-bit `u128` exact-equality (which is far too slow for
/// CI). Each divisor is just a different *constant* multiplier through the same
/// multiply/`div_ceil`/floor path, so ÷16 exercises every rounding and carry
/// behavior; the decode of all eight encodings is proven independently by
/// [`divide_value_total`], and the headline round-trip-at-arm property is proven
/// for **all** divisors by [`current_count_round_trips_at_arm`].
const REP_DIVIDE_CONFIG: u32 = 0b0011;

/// The period is the **exact ceiling** `ceil(N · divide · 1e9 / timer_hz)` for
/// the concrete 25 MHz clock and the representative ÷16 divisor, over a 12-bit
/// `N`. The ceiling guarantee (`period · timer_hz >= N · divide · 1e9`, i.e. the
/// deadline covers ≥ N whole ticks) is asserted directly.
#[kani::proof]
fn period_exact_ceil() {
    let n: u32 = kani::any();
    kani::assume(u64::from(n) <= EXACT_BOUND);

    let l = timer_lapic(PROOF_TIMER_HZ, REP_DIVIDE_CONFIG, n, 0);
    let got = l.period_for(l.count_at_arm);

    let divide = divide_value(REP_DIVIDE_CONFIG);
    let numer = u128::from(n) * u128::from(divide) * NS_PER_SEC;
    assert_eq!(got, numer.div_ceil(u128::from(PROOF_TIMER_HZ)));
    // Ceiling: a full period covers at least N whole ticks.
    assert!(got * u128::from(PROOF_TIMER_HZ) >= numer);
}

/// **Round-trip exactness** — the headline property: at the arming instant the
/// Current Count reads back exactly `N`, for **every** legal divisor and the full
/// `u32` count range. Cheap because at `Δ == 0` the elapsed-ticks numerator is 0,
/// so the divisor folds away and no `u128` divider is built — letting the divisor
/// stay looped over all eight configs and `N` stay full-`u32`.
#[kani::proof]
#[kani::unwind(9)]
fn current_count_round_trips_at_arm() {
    let n: u32 = kani::any();
    let arm: u64 = kani::any();
    for divide_config in DIVIDE_CONFIGS {
        let l = timer_lapic(PROOF_TIMER_HZ, divide_config, n, arm);
        assert_eq!(l.current_count(arm), n);
    }
}

/// The Current Count decays exactly: `N − floor(Δ · timer_hz / (divide · 1e9))`
/// for the concrete 25 MHz clock and the representative ÷16 divisor, over a
/// 12-bit `N` and `Δ`. Never panics (the divisor `divide · 1e9` is non-zero
/// because `divide >= 1`).
#[kani::proof]
fn current_count_exact_decay() {
    let n: u32 = kani::any();
    let delta: u64 = kani::any();
    kani::assume(u64::from(n) <= EXACT_BOUND);
    kani::assume(delta <= EXACT_BOUND);
    let arm: u64 = kani::any();
    kani::assume(arm <= EXACT_BOUND);
    let now = arm + delta; // both bounded, no overflow

    let l = timer_lapic(PROOF_TIMER_HZ, REP_DIVIDE_CONFIG, n, arm);
    let got = l.current_count(now);

    let divide = divide_value(REP_DIVIDE_CONFIG);
    let ticks =
        (u128::from(delta) * u128::from(PROOF_TIMER_HZ)) / (u128::from(divide) * NS_PER_SEC);
    let want = n.saturating_sub(u32::try_from(ticks).unwrap_or(u32::MAX));
    assert_eq!(got, want);
}

/// The Current Count is **monotonically non-increasing** in elapsed V-time: for
/// `Δ1 <= Δ2`, `current_count(arm + Δ1) >= current_count(arm + Δ2)`. Inequality
/// (cheaper than exact-eq), concrete ÷2 divisor, 25 MHz clock.
#[kani::proof]
fn current_count_monotone() {
    let n: u32 = kani::any();
    let arm: u64 = kani::any();
    let d1: u64 = kani::any();
    let d2: u64 = kani::any();
    kani::assume(d1 <= d2);
    kani::assume(arm <= EXACT_BOUND && d2 <= EXACT_BOUND);
    let l = timer_lapic(PROOF_TIMER_HZ, 0b0000, n, arm); // ÷2

    assert!(l.current_count(arm + d1) >= l.current_count(arm + d2));
}

/// The 256-bit register bit helpers never index out of bounds for **any** `u8`
/// vector: `set_vec`/`clear_vec`/`highest_vec` round-trip a single vector
/// without panicking (the word index `v >> 5` is always `< 8`).
#[kani::proof]
fn vec_index_in_bounds() {
    let v: u8 = kani::any();
    let mut bits = [0u32; 8];
    set_vec(&mut bits, v);
    assert_eq!(highest_vec(&bits), Some(v));
    clear_vec(&mut bits, v);
    assert!(highest_vec(&bits).is_none());
}

/// `highest_vec` is total for an arbitrary 256-bit register: it returns `None`
/// iff every word is zero, and otherwise a vector that is actually set whose
/// priority class fits in four bits. Never indexes out of bounds.
#[kani::proof]
#[kani::unwind(9)]
fn highest_vec_correct() {
    let bits: [u32; 8] = kani::any();
    match highest_vec(&bits) {
        None => {
            let mut any_set = false;
            for w in 0..8 {
                if bits[w] != 0 {
                    any_set = true;
                }
            }
            assert!(!any_set);
        }
        Some(v) => {
            // v is genuinely set.
            assert!(bits[(v >> 5) as usize] & (1u32 << (v & 31)) != 0);
            assert!(priority_class(v) <= 15);
        }
    }
}

/// PPR derivation and the delivery comparison are **total and panic-free** for
/// an arbitrary task priority and arbitrary ISR/IRR contents: PPR's priority
/// class is always a 4-bit value, and `has_deliverable` / `take_interrupt`
/// never panic regardless of register contents.
#[kani::proof]
fn ppr_and_delivery_total() {
    let tpr: u32 = kani::any();
    let isr: [u32; 8] = kani::any();
    let irr: [u32; 8] = kani::any();
    let mut l = timer_lapic(PROOF_TIMER_HZ, 0, 0, 0);
    l.svr = SVR_ENABLE_BIT; // software-enabled, so the comparison path runs
    l.tpr = tpr;
    l.isr = isr;
    l.irr = irr;

    // PPR's class is a total 4-bit quantity.
    assert!((l.ppr() >> 4) <= 15);

    // Delivery decisions never panic and are mutually consistent: if a vector
    // is deliverable, taking it yields exactly that vector.
    let deliverable = l.has_deliverable();
    let taken = l.take_interrupt();
    assert_eq!(deliverable, taken.is_some());
}

/// Build a software-enabled, unmasked, **periodic** running timer (vector 0x40)
/// with the given timer parameters, armed at `arm_vns` — a fully-active timer
/// (`timer_active()` is true) for the `next_timer_deadline`/`advance_to` proofs.
fn running_periodic_lapic(
    timer_hz: u64,
    divide_config: u32,
    initial_count: u32,
    arm_vns: u64,
) -> Lapic {
    let mut lvt = [LVT_RESET; 6];
    lvt[LVT_TIMER] = 0x40 | (TIMER_PERIODIC << 17); // periodic, unmasked, vector 0x40
    Lapic {
        id: 0,
        timer_hz,
        tpr: 0,
        svr: SVR_ENABLE_BIT,
        ldr: 0,
        dfr: 0,
        esr: 0,
        icr_low: 0,
        icr_high: 0,
        divide_config,
        isr: [0; 8],
        tmr: [0; 8],
        irr: [0; 8],
        lvt,
        initial_count,
        count_at_arm: initial_count, // fresh-armed
        timer_arm_vns: arm_vns,
        timer_running: true,
        timer_pending: true,
    }
}

/// `advance_to` is **idempotent at the `u64::MAX` saturation boundary** (PR #38
/// regression): with `now == u64::MAX` and the timer armed within a few periods
/// of the maximum V-time (so its deadline `arm + period` can saturate), a repeat
/// `advance_to` at the same `now_vns` never re-fires or changes state. This is
/// exactly the regime a naive `now >= deadline` fire looped on — the deadline
/// clamps to `now` while fewer than one period has elapsed. (General idempotence
/// over arbitrary `arm`/`now` is covered by the proptest; here `now` is the
/// concrete boundary and `arm` is bounded near it so the `u128 / 80_000_000`
/// divide stays small for CBMC.)
#[kani::proof]
fn advance_to_idempotent_at_saturation_boundary() {
    // period = ceil(1_000_000 · 2 · 1e9 / 25e6) = 80_000_000 ns; three periods.
    let arm: u64 = kani::any();
    kani::assume(arm >= u64::MAX - 240_000_000);
    let now = u64::MAX;
    let mut l = running_periodic_lapic(PROOF_TIMER_HZ, 0b0000, 1_000_000, arm);

    let _ = l.advance_to(now);
    let arm1 = l.timer_arm_vns;
    let irr1 = l.irr;
    let running1 = l.timer_running;

    let second = l.advance_to(now);
    assert!(!second);
    assert_eq!(l.timer_arm_vns, arm1);
    assert_eq!(l.irr, irr1);
    assert_eq!(l.timer_running, running1);
}

/// A mid-count **divide-config change never fires retroactively** (PR #38, the
/// 6th timer bug): for a running one-shot with ticks still remaining, switching
/// the divisor reschedules from the *current remaining count* — the remaining is
/// preserved (not recomputed retroactively), `advance_to` at the change instant
/// does not fire, and the new deadline is strictly in the future. Concrete
/// 25 MHz / N=1000 so the `u128` divides fold; `now` is bounded within the first
/// period so the remaining is non-zero.
#[kani::proof]
fn tdcr_change_no_retroactive_fire() {
    // ÷2, 25 MHz, N=1000 -> first-period span 80_000 ns.
    let mut lvt = [LVT_RESET; 6];
    lvt[LVT_TIMER] = 0x40; // one-shot, unmasked, vector 0x40
    let mut l = Lapic {
        id: 0,
        timer_hz: PROOF_TIMER_HZ,
        tpr: 0,
        svr: SVR_ENABLE_BIT,
        ldr: 0,
        dfr: 0,
        esr: 0,
        icr_low: 0,
        icr_high: 0,
        divide_config: 0b0000, // ÷2
        isr: [0; 8],
        tmr: [0; 8],
        irr: [0; 8],
        lvt,
        initial_count: 1000,
        count_at_arm: 1000,
        timer_arm_vns: 0,
        timer_running: true,
        timer_pending: true,
    };
    let now: u64 = kani::any();
    kani::assume(now < 80_000); // within the first period -> remaining > 0
    let remaining_before = l.current_count(now);
    kani::assume(remaining_before > 0);

    // A TDCR write to ÷128 goes through the unified re-arm path.
    l.mmio_write(APIC_TDCR, 0b1010, now).unwrap();

    // Remaining preserved (not recomputed retroactively), rescheduled forward.
    assert_eq!(l.current_count(now), remaining_before);
    assert!(!l.advance_to(now));
    if let Some(d) = l.next_timer_deadline() {
        assert!(d > now);
    }
}

/// A **fired one-shot must not be resurrected** by a gating change (PR #38 final
/// pass): with `timer_pending == false` but the Initial Count register still
/// non-zero, software-enabled and unmasked, the `reevaluate_timer` path (run on
/// every LVT-timer / SVR write) must leave the timer stopped — no deadline, no
/// fire — until a fresh Initial Count write. Proven for a symbolic count and an
/// arbitrary `now`.
#[kani::proof]
fn fired_oneshot_not_resurrected() {
    let n: u32 = kani::any();
    kani::assume(n != 0); // the count register retains its value after firing
    let now: u64 = kani::any();

    let mut lvt = [LVT_RESET; 6];
    lvt[LVT_TIMER] = 0x40; // one-shot (mode 0), unmasked, vector 0x40
    let mut l = Lapic {
        id: 0,
        timer_hz: PROOF_TIMER_HZ,
        tpr: 0,
        svr: SVR_ENABLE_BIT, // software-enabled
        ldr: 0,
        dfr: 0,
        esr: 0,
        icr_low: 0,
        icr_high: 0,
        divide_config: 0,
        isr: [0; 8],
        tmr: [0; 8],
        irr: [0; 8],
        lvt,
        initial_count: n, // count register still reads N after firing
        count_at_arm: n,  // (irrelevant while stopped)
        timer_arm_vns: 0,
        timer_running: false, // already fired
        timer_pending: false, // ...and the count was consumed
    };

    // A gating re-evaluation (the unified re-arm path, run on any LVT-timer / SVR
    // write) must not re-arm a consumed one-shot.
    let prior = l.running_remaining(now);
    l.retime(now, prior, divide_value(l.divide_config));
    assert!(!l.timer_running);
    assert_eq!(l.next_timer_deadline(), None);
    assert!(!l.advance_to(u64::MAX));
}

/// The Divide-Config write mask **drops the decode-ignored bit 2** for **any**
/// written value (the determinism fix): bit 2 selects no divisor, so storing it
/// would let two behaviorally-identical guests snapshot to different
/// `divide_config` and hash differently. The mask removes it while leaving the
/// decoded divisor unchanged. Cheap — purely bitwise, so the value stays fully
/// symbolic.
#[kani::proof]
fn tdcr_write_mask_drops_ignored_bit() {
    let value: u32 = kani::any();
    let stored = value & TDCR_WRITE_MASK;
    assert_eq!(stored & 0b100, 0); // bit 2 is never stored
    assert_eq!(stored & !0b1011, 0); // only bits 0,1,3 survive
    assert_eq!(divide_value(stored), divide_value(value)); // divisor unchanged
}

/// The LVT write masks **exclude reserved bits** for **any** written value and
/// **every** LVT index (PR #38 systematic pass): the read-only delivery-status
/// (bit 12) and remote-IRR (bit 14) are never stored, and the **Error** LVT
/// (index 5) specifically has no delivery-mode field (bits 8..=10). Cheap —
/// purely bitwise, so the value stays fully symbolic.
#[kani::proof]
#[kani::unwind(7)]
fn lvt_write_masks_exclude_reserved() {
    let value: u32 = kani::any();
    // Error LVT has no delivery-mode field.
    assert_eq!(value & lvt_write_mask(5) & 0x0000_0700, 0);
    for i in 0..6 {
        let stored = value & lvt_write_mask(i);
        assert_eq!(stored & (1 << 12), 0); // delivery-status is read-only
        assert_eq!(stored & (1 << 14), 0); // remote-IRR is read-only
        assert_eq!(stored & !lvt_write_mask(i), 0); // no bit outside the mask
    }
}
