// SPDX-License-Identifier: AGPL-3.0-or-later
//! Kani proof harnesses for the bounded integer invariants `EnvCodec::compose`
//! (and the host plane) rests on, split out of `envcodec.rs` so they are
//! `#[cfg(kani)]`-only: verified by the dedicated `kani` CI job, never compiled
//! into the normal/test build and never seen by the mutation oracle. Declared as
//! `#[cfg(kani)] #[path = "envcodec_proofs.rs"] mod proofs;` in `envcodec.rs`, so
//! they are children of `envcodec` and `use super::*` reaches the private
//! `rekey_moment` and the imported `Ratio` / `EnvError`.
//!
//! These are "law holds for ALL inputs" claims — strictly stronger than the
//! proptest sampling in `tests/` — over pure `u64` arithmetic, so CBMC discharges
//! them with fully symbolic inputs (no value bounds needed).

use super::*;

/// `Ratio::new` is total and rejects **exactly** a zero denominator: it returns
/// `Some` iff `den != 0`, and every constructed `Ratio` round-trips its fields and
/// has `den() != 0` — the no-divide-by-zero invariant the `SetClockRate` consumer
/// (and the codec's `den == 0` rejection) depend on.
#[kani::proof]
fn ratio_new_rejects_exactly_zero_denominator() {
    let num: u64 = kani::any();
    let den: u64 = kani::any();
    match Ratio::new(num, den) {
        Some(r) => {
            assert!(den != 0);
            assert!(r.num() == num);
            assert!(r.den() == den);
            assert!(r.den() != 0);
        }
        None => assert!(den == 0),
    }
}

/// `rekey_moment` is **overflow-safe and exact**: `Ok(k)` iff `m + at` fits in
/// `u64` (and then `k` is the exact, non-wrapping sum), `Err(Overflow)` iff it
/// would exceed `u64::MAX`. It never wraps — a wrap is what would silently
/// collapse two overrides onto one key.
#[kani::proof]
fn rekey_moment_is_exact_or_rejects_overflow() {
    let m: u64 = kani::any();
    let at: u64 = kani::any();
    let sum = (m as u128) + (at as u128);
    match rekey_moment(m, at) {
        Ok(k) => assert!(k as u128 == sum),
        Err(e) => {
            assert!(matches!(e, EnvError::Overflow));
            assert!(sum > u64::MAX as u128);
        }
    }
}

/// `rekey_moment` is **injective** for a fixed `at`: distinct source `Moment`s
/// that both re-key successfully map to distinct keys — the collision-free
/// guarantee that makes the override re-keying genesis-complete (exactly the
/// property `saturating_add` would violate).
#[kani::proof]
fn rekey_moment_is_injective() {
    let m1: u64 = kani::any();
    let m2: u64 = kani::any();
    let at: u64 = kani::any();
    kani::assume(m1 != m2);
    if let (Ok(k1), Ok(k2)) = (rekey_moment(m1, at), rekey_moment(m2, at)) {
        assert!(k1 != k2);
    }
}
