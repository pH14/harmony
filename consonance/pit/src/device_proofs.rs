// SPDX-License-Identifier: AGPL-3.0-or-later
//! Kani proof harnesses for the i8254 countdown arithmetic (gate 1), split out of
//! `device.rs` so cargo-mutants can glob-exclude them: they are `#[cfg(kani)]` and
//! verified by the dedicated `kani` CI job, not the mutation oracle. Declared as
//! `#[cfg(kani)] #[path = "device_proofs.rs"] mod proofs;` in `device.rs`, so it is
//! a child of `device` (`use super::*` reaches the private helpers it verifies).
//!
//! ## Why the bounds (the CBMC cost model)
//!
//! CBMC bit-blasts arithmetic into a SAT instance whose cost is driven by operator
//! width and kind (the lesson `vtime::clock_proofs` / `lapic::device_proofs`
//! document). The countdown's `÷ freq` and `n · 1e9` / `Δ · freq` products are
//! `u128`, so the harnesses pin `freq` to a concrete representative
//! ([`PROOF_FREQ`], the contract's 1.193182 MHz, or `1` for the no-panic harness)
//! and bound the symbolic `n`/`Δ` operand for the exact-equality harnesses. The
//! reload caps at 65536, so `n` never needs the full `u32` width.

use super::*;

/// Representative concrete input frequency: the contract's 1.193182 MHz.
const PROOF_FREQ: u64 = PIT_FREQ_HZ;

/// Tight bound (12 bits) for symbolic `n`/`Δ` in the exact-equality harnesses —
/// enough to drive every rounding/carry path of the `u128` constant-divisor
/// divide while staying fast for CI.
const EXACT_BOUND: u64 = (1 << 12) - 1;

/// Build a loaded periodic (mode 2) counter with reload `reload`, armed at `arm`.
fn mode2_counter(reload: u16, arm: u64) -> Counter {
    Counter {
        mode: 2,
        bcd: false,
        access: 3,
        reload,
        arm_vns: arm,
        loaded: true,
        oneshot_fired: false,
        null_count: false,
        ..Default::default()
    }
}

/// `decode_count` is total and bounded for **every** `u16` and both bases: it
/// never panics and always returns a count in `1..=modulus` (binary `1..=65536`,
/// BCD `1..=10000` — though out-of-range BCD nibbles sum higher, see the impl).
#[kani::proof]
fn decode_count_binary_in_range() {
    let raw: u16 = kani::any();
    let n = decode_count(raw, false);
    assert!((1..=BIN_MODULUS).contains(&n));
}

/// The period computation **never panics for any reload**: the `u128` product
/// `n · 1e9` cannot overflow (`65536 · 1e9 < 2^128`) and `÷ freq` never traps.
/// Proven over all `u16` reloads; `freq` is pinned to the trivial `1` (the property
/// is the multiply, not the division), so it stays fast at full width.
#[kani::proof]
fn period_never_panics() {
    let raw: u16 = kani::any();
    let n = decode_count(raw, false);
    let _ = Counter::period_ns(n, 1); // must not panic / overflow for any input
}

/// The period is the **exact ceiling** `ceil(n · 1e9 / freq)` for the concrete
/// contract frequency over a 12-bit reload, and covers at least `n` whole ticks.
#[kani::proof]
fn period_exact_ceil() {
    let raw: u16 = kani::any();
    kani::assume(u64::from(raw) <= EXACT_BOUND);
    let n = decode_count(raw, false);
    let got = Counter::period_ns(n, PROOF_FREQ);
    let numer = u128::from(n) * NS_PER_SEC;
    assert_eq!(got, numer.div_ceil(u128::from(PROOF_FREQ)));
    // Ceiling: a full period covers at least n whole ticks.
    assert!(got * u128::from(PROOF_FREQ) >= numer);
}

/// A periodic counter's current count is **always in `1..=n`** (a rate generator
/// never reads 0) for any reload and any elapsed V-time. Concrete frequency; `now`
/// fully symbolic (the `÷ 1e9` floor folds — `1e9` is constant).
#[kani::proof]
fn periodic_current_count_in_range() {
    let raw: u16 = kani::any();
    let now: u64 = kani::any();
    let c = mode2_counter(raw, 0);
    let n = decode_count(raw, false);
    let cur = c.current_dec(now, PROOF_FREQ);
    assert!((1..=n).contains(&cur));
}

/// `advance` is **idempotent at the `u64::MAX` saturation boundary**: with
/// `now == u64::MAX` and the counter armed within a few periods of the maximum
/// V-time, a repeat `advance` at the same `now` never re-fires or changes the arm
/// anchor — the regime a naive `now >= deadline` fire would loop on. (General
/// idempotence over arbitrary `arm`/`now` is covered by the proptest.)
#[kani::proof]
fn advance_idempotent_at_saturation_boundary() {
    // period for reload 1000 at 1.193182 MHz ≈ 838 095 ns; keep arm within ~3.
    let arm: u64 = kani::any();
    kani::assume(arm >= u64::MAX - 3_000_000);
    let now = u64::MAX;
    let mut c = mode2_counter(1000, arm);

    let _ = c.advance(now, PROOF_FREQ);
    let arm1 = c.arm_vns;
    let second = c.advance(now, PROOF_FREQ);
    assert!(!second);
    assert_eq!(c.arm_vns, arm1);
}

/// A counter whose period would push the deadline past `u64` reports **no
/// deadline** rather than a clamped value `advance` would never reach — the
/// saturating contract. With `arm` near `u64::MAX` the deadline `arm + period`
/// overflows `u64`, so `next_expiry` returns `None`.
#[kani::proof]
fn huge_arm_reports_no_deadline() {
    let arm: u64 = kani::any();
    kani::assume(arm > u64::MAX - 1_000_000); // period(1000) ≈ 838 095 ns > gap
    let c = mode2_counter(1000, arm);
    assert!(u128::from(arm) + Counter::period_ns(1000, PROOF_FREQ) > u128::from(u64::MAX));
    assert_eq!(c.next_expiry(PROOF_FREQ), None);
}

/// The mode decode maps the alias encodings `6`/`7` to modes 2/3 and is total for
/// every `u8` (the `& 0b111` keeps it in range; no panic).
#[kani::proof]
fn decoded_mode_total() {
    let m: u8 = kani::any();
    let c = Counter {
        mode: m,
        ..Default::default()
    };
    let d = c.decoded_mode();
    assert!(d <= 5);
    // 6/7 alias 2/3; 0..=5 pass through.
    match m & 0b111 {
        6 => assert_eq!(d, 2),
        7 => assert_eq!(d, 3),
        other => assert_eq!(d, other),
    }
}
