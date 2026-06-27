// SPDX-License-Identifier: AGPL-3.0-or-later
//! Kani proof harness for the bounded integer invariant the host plane (and so
//! `EnvCodec`'s `SetClockRate` proposals) rests on, split out of `envcodec.rs` so
//! it is `#[cfg(kani)]`-only: verified by the dedicated `kani` CI job, never
//! compiled into the normal/test build and never seen by the mutation oracle.
//! Declared as `#[cfg(kani)] #[path = "envcodec_proofs.rs"] mod proofs;` in
//! `envcodec.rs`, so it is a child of `envcodec` and `use super::*` reaches the
//! imported `Ratio`.
//!
//! This is a "law holds for ALL inputs" claim — strictly stronger than the
//! proptest sampling in `tests/` — over pure `u64` arithmetic, so CBMC discharges
//! it with fully symbolic inputs (no value bounds needed).

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
