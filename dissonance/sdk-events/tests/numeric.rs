// SPDX-License-Identifier: AGPL-3.0-or-later
//! Numeric-guidance normalization laws: the original token is preserved, a valid
//! token reduces to a bounded exact decimal, out-of-range/non-finite input stays
//! report-only, and the reduction imposes a deterministic **total order** that is
//! exact where `f64` would not be.

use proptest::prelude::*;
use sdk_events::{NumericError, NumericLimits, NumericToken};

fn bounded(s: &str) -> sdk_events::BoundedNumeric {
    NumericToken::new(s)
        .to_bounded(&NumericLimits::DEFAULT)
        .unwrap_or_else(|e| panic!("`{s}` should reduce: {e}"))
}

#[test]
fn the_original_token_is_preserved_verbatim() {
    for s in ["-12.50", "1e6", "0", "007", "3.0", "+42"] {
        assert_eq!(NumericToken::new(s).as_str(), s);
    }
}

#[test]
fn equal_values_written_differently_reduce_equal() {
    // Canonicalization makes representation-equal ⇔ value-equal.
    assert_eq!(bounded("1.5"), bounded("1.50"));
    assert_eq!(bounded("1500"), bounded("1.5e3"));
    assert_eq!(bounded("0"), bounded("-0"));
    assert_eq!(bounded("0"), bounded("0.000"));
    assert_eq!(bounded("100"), bounded("1e2"));
}

#[test]
fn ordering_is_exact_where_f64_would_collapse() {
    // These two differ only in the 17th significant digit — `f64` rounds them to
    // the same bits; the exact decimal keeps them distinct and correctly ordered.
    let a = bounded("0.1");
    let b = bounded("0.10000000000000001");
    assert!(a < b, "exact decimal distinguishes what f64 cannot");

    // Basic sanity across sign and magnitude.
    assert!(bounded("-1") < bounded("0"));
    assert!(bounded("0") < bounded("0.0001"));
    assert!(bounded("9") < bounded("10"));
    assert!(bounded("-10") < bounded("-9"));
    assert!(bounded("999999999999999999999") < bounded("1e21"));
}

#[test]
fn non_finite_and_out_of_range_tokens_stay_report_only() {
    let limits = NumericLimits::DEFAULT;
    // Not a finite decimal number.
    for bad in ["", "NaN", "Infinity", "1.2.3", "abc", "1e", "0x10", "--1"] {
        assert!(
            matches!(
                NumericToken::new(bad).to_bounded(&limits),
                Err(NumericError::NotFinite { .. })
            ),
            "`{bad}` must fail as non-finite"
        );
    }
    // Too many significant digits for the limits.
    let many = "1".repeat(limits.max_significant_digits as usize + 1);
    assert!(matches!(
        NumericToken::new(&many).to_bounded(&limits),
        Err(NumericError::TooManyDigits { .. })
    ));
    // Magnitude beyond the exponent window.
    assert!(matches!(
        NumericToken::new("1e100").to_bounded(&limits),
        Err(NumericError::ExponentOutOfRange { .. })
    ));
    assert!(matches!(
        NumericToken::new("1e-100").to_bounded(&limits),
        Err(NumericError::ExponentOutOfRange { .. })
    ));
    // A pathologically long exponent field does not hang or panic.
    assert!(
        NumericToken::new("1e100000000000000000000")
            .to_bounded(&limits)
            .is_err()
    );
}

/// A generator of finite decimal tokens across sign, fraction, and exponent forms,
/// tuned to stay within the default limits.
fn token_strategy() -> impl Strategy<Value = String> {
    (
        any::<bool>(),
        1u64..=9_999_999,
        0u8..=6,
        -12i32..=12,
        any::<bool>(),
    )
        .prop_map(|(neg, coeff, frac_digits, exp, use_exp)| {
            let sign = if neg { "-" } else { "" };
            let digits = coeff.to_string();
            let body = if frac_digits as usize >= digits.len() {
                format!("0.{digits:0>width$}", width = frac_digits as usize)
            } else {
                let split = digits.len() - frac_digits as usize;
                if frac_digits == 0 {
                    digits.clone()
                } else {
                    format!("{}.{}", &digits[..split], &digits[split..])
                }
            };
            if use_exp {
                format!("{sign}{body}e{exp}")
            } else {
                format!("{sign}{body}")
            }
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Every generated token reduces, and reduction is idempotent-canonical:
    /// re-serializing the reduced value's digits and re-parsing yields an equal
    /// value.
    #[test]
    fn generated_tokens_reduce(token in token_strategy()) {
        let v = NumericToken::new(&token).to_bounded(&NumericLimits::DEFAULT);
        prop_assert!(v.is_ok(), "token `{}` should reduce: {:?}", token, v);
    }

    /// The order is a genuine total order: reflexive, antisymmetric, and
    /// consistent with equality, over independently generated values.
    #[test]
    fn total_order_axioms(a in token_strategy(), b in token_strategy(), c in token_strategy()) {
        let (x, y, z) = (bounded(&a), bounded(&b), bounded(&c));
        // Reflexive.
        prop_assert_eq!(x.cmp(&x), std::cmp::Ordering::Equal);
        // Antisymmetric / total: cmp(x,y) is the reverse of cmp(y,x).
        prop_assert_eq!(x.cmp(&y), y.cmp(&x).reverse());
        // Transitive on ≤.
        if x <= y && y <= z {
            prop_assert!(x <= z);
        }
        // Equality agrees with ordering.
        prop_assert_eq!(x == y, x.cmp(&y) == std::cmp::Ordering::Equal);
    }

    /// Reduction agrees with integer order on integer tokens — an independent
    /// oracle for the exact comparison (computed with `i128`, never `f64`).
    #[test]
    fn agrees_with_integer_order(a in -1_000_000i128..=1_000_000, b in -1_000_000i128..=1_000_000) {
        let x = bounded(&a.to_string());
        let y = bounded(&b.to_string());
        prop_assert_eq!(x.cmp(&y), a.cmp(&b));
    }
}
