// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

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
