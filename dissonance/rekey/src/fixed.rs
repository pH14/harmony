// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic fixed-point arithmetic for the scoring axes.
//!
//! Axis (b) is Go-Explore's re-tune objective `O = H_n(p) / √(|n/T − 1| + 1)`,
//! which wants a logarithm and a square root — and conventions rule 4 forbids
//! floating point anywhere a value reaches an output. `f64::ln` is a libm call
//! whose last bits are not guaranteed identical across platforms, so a report
//! rendered on macOS could differ from one rendered on Linux in its final digit.
//!
//! Everything here is therefore integer math on **Q32.32** (a `u64` holding a
//! value scaled by `2^32`), with `u128` intermediates:
//!
//! - [`log2_q32`] extracts the fractional bits by repeated squaring — exact
//!   integer operations, identical on every host;
//! - [`ln_q32`] rescales by a pinned `ln 2` constant;
//! - the square root is `u128::isqrt`, which is exact by definition.
//!
//! The result is a score that is byte-identical wherever the harness runs,
//! which is what the determinism gate demands of `REKEY-REPORT.md`.

/// `1.0` in Q32.32.
pub const ONE: u64 = 1 << 32;

/// `ln 2` in Q32.32: `round(0.693147180559945309 × 2^32)`.
const LN2_Q32: u128 = 2_977_044_472;

/// `log2(x)` in Q32.32, for `x >= 1`. `x == 0` yields `0` (the value is
/// undefined; callers never key on it).
///
/// The integer part is the bit length; the 32 fractional bits come from the
/// classic repeated-squaring extraction: normalize the mantissa `m ∈ [1, 2)` to
/// Q63, square it, and if the result reached `2` emit a `1` bit and halve.
pub fn log2_q32(x: u64) -> u64 {
    if x == 0 {
        return 0;
    }
    let e = 63 - x.leading_zeros(); // floor(log2 x)
    let mut result = u64::from(e) << 32;
    // The mantissa in Q63: `m / 2^63 ∈ [1, 2)`.
    let mut m: u128 = u128::from(x) << (63 - e);
    for i in 1..=32u32 {
        // `m` is in [2^63, 2^64), so `m*m` is in [2^126, 2^128) — inside u128.
        m = (m * m) >> 63;
        if m >= 1u128 << 64 {
            m >>= 1;
            result |= 1u64 << (32 - i);
        }
    }
    result
}

/// `ln(x)` in Q32.32, for `x >= 1`.
pub fn ln_q32(x: u64) -> u64 {
    // log2(x) ≤ 63 ⇒ the product is ≤ 2^38 × 2^32 ≈ 8×10^20: a u128 intermediate.
    ((u128::from(log2_q32(x)) * LN2_Q32) >> 32) as u64
}

/// `a / b` in Q32.32. `b == 0` yields `0`.
pub fn ratio_q32(a: u64, b: u64) -> u64 {
    if b == 0 {
        return 0;
    }
    ((u128::from(a) << 32) / u128::from(b)) as u64
}

/// The Shannon entropy `H(p)` in **nats**, Q32.32, of the distribution `p_i =
/// counts[i] / N` — computed as `ln N − (1/N) Σ cᵢ ln cᵢ`, which needs no
/// division inside the logarithm.
///
/// Zero counts contribute nothing (`0 · ln 0 ≡ 0`). An empty or all-zero input
/// yields `0`.
pub fn entropy_q32(counts: &[u64]) -> u64 {
    let n: u64 = counts.iter().sum();
    if n == 0 {
        return 0;
    }
    let mut acc: u128 = 0;
    for &c in counts {
        if c > 1 {
            acc += u128::from(c) * u128::from(ln_q32(c));
        }
    }
    let mean = (acc / u128::from(n)) as u64;
    // Saturating: the two logarithms round independently, so a uniform
    // distribution can land a bit below zero. Entropy is never negative.
    ln_q32(n).saturating_sub(mean)
}

/// The **normalized** entropy `H_n(p) = H(p) / ln n` in Q32.32, where `n` is the
/// number of cells. `n < 2` yields `0`: a single-cell (or empty) archive carries
/// no distributional information, and `ln 1 = 0` would divide by zero.
pub fn normalized_entropy_q32(counts: &[u64]) -> u64 {
    let n = counts.len();
    if n < 2 {
        return 0;
    }
    let denom = ln_q32(n as u64);
    if denom == 0 {
        return 0;
    }
    ((u128::from(entropy_q32(counts)) << 32) / u128::from(denom)) as u64
}

/// Go-Explore's cell-function re-tune objective (`docs/SCORING.md` R2, law 3):
///
/// ```text
/// O = H_n(p) / √( |n/T − 1| + 1 )
/// ```
///
/// `counts` is the arrival count per occupied cell, `n = counts.len()` the cell
/// count, and `target` the stated target cell count `T`. Maximal when the
/// arrivals are uniform over exactly `T` cells. `n < 2` or `target == 0` yields
/// `0`.
pub fn go_explore_objective_q32(counts: &[u64], target: u64) -> u64 {
    let n = counts.len() as u64;
    if n < 2 || target == 0 {
        return 0;
    }
    let h_n = normalized_entropy_q32(counts);
    // |n/T − 1| + 1, in Q32.
    let ratio = (u128::from(n) << 32) / u128::from(target);
    let deviation = ratio.abs_diff(u128::from(ONE)) + u128::from(ONE);
    // √(deviation) in Q32 = isqrt(deviation × 2^32).
    let root = (deviation << 32).isqrt();
    if root == 0 {
        return 0;
    }
    ((u128::from(h_n) << 32) / root) as u64
}

/// Render a Q32.32 value as a fixed 6-decimal string — integer arithmetic only,
/// so the digits are identical on every host (no float formatting).
///
/// Rounds half-up rather than truncating, so an exactly-representable `3.725`
/// prints as `3.725000` and not as `3.724999`. The sixth place is therefore
/// accurate to ±5×10⁻⁷; a carry out of the fraction propagates into the integer
/// part (`0.9999999` prints `1.000000`).
pub fn fmt_q32(v: u64) -> String {
    let scaled = (u128::from(v) * 1_000_000 + (1u128 << 31)) >> 32;
    format!("{}.{:06}", scaled / 1_000_000, scaled % 1_000_000)
}

/// The arithmetic mean of Q32.32 values; `0` for an empty slice.
pub fn mean_q32(vs: &[u64]) -> u64 {
    if vs.is_empty() {
        return 0;
    }
    let sum: u128 = vs.iter().map(|&v| u128::from(v)).sum();
    (sum / vs.len() as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact at the powers of two, and correct to six places between them.
    #[test]
    fn log2_is_exact_on_powers_of_two() {
        assert_eq!(log2_q32(1), 0);
        assert_eq!(log2_q32(2), ONE);
        assert_eq!(log2_q32(4), 2 * ONE);
        assert_eq!(log2_q32(1 << 40), 40 * ONE);
        // log2(3) = 1.5849625007…
        assert_eq!(fmt_q32(log2_q32(3)), "1.584963");
        // log2(10) = 3.3219280948…
        assert_eq!(fmt_q32(log2_q32(10)), "3.321928");
    }

    /// `ln` against pinned reference values.
    #[test]
    fn ln_matches_reference_values() {
        assert_eq!(ln_q32(1), 0);
        assert_eq!(fmt_q32(ln_q32(2)), "0.693147");
        assert_eq!(fmt_q32(ln_q32(10)), "2.302585");
        assert_eq!(fmt_q32(ln_q32(1_000_000)), "13.815511");
    }

    /// A uniform distribution over `n` cells has entropy `ln n` and normalized
    /// entropy exactly `1` (to six places).
    #[test]
    fn uniform_distributions_have_unit_normalized_entropy() {
        for n in [2usize, 3, 4, 17, 256] {
            let counts = vec![7u64; n];
            assert_eq!(
                fmt_q32(entropy_q32(&counts)),
                fmt_q32(ln_q32(n as u64)),
                "H of a uniform {n}-cell distribution is ln {n}"
            );
            assert_eq!(fmt_q32(normalized_entropy_q32(&counts)), "1.000000");
        }
    }

    /// A degenerate distribution (all mass in one cell) has zero entropy, and a
    /// one-cell archive has zero normalized entropy by definition.
    #[test]
    fn degenerate_distributions_score_zero() {
        assert_eq!(entropy_q32(&[9]), 0);
        assert_eq!(normalized_entropy_q32(&[9]), 0);
        assert_eq!(normalized_entropy_q32(&[]), 0);
        assert_eq!(entropy_q32(&[]), 0);
        assert_eq!(entropy_q32(&[0, 0]), 0);
        // A skewed two-cell distribution sits strictly between 0 and 1.
        let skewed = normalized_entropy_q32(&[999, 1]);
        assert!(skewed > 0 && skewed < ONE, "0 < H_n < 1, got {skewed}");
    }

    /// The objective peaks when the cell count equals the target and the
    /// arrivals are uniform: there `O = H_n = 1`.
    #[test]
    fn objective_peaks_at_the_target_with_uniform_arrivals() {
        let counts = vec![4u64; 64];
        assert_eq!(fmt_q32(go_explore_objective_q32(&counts, 64)), "1.000000");
        // Away from the target the penalty divides it down, symmetrically in
        // the deviation |n/T − 1|.
        let far = go_explore_objective_q32(&counts, 8); // n/T = 8 → deviation 7
        let near = go_explore_objective_q32(&counts, 32); // n/T = 2 → deviation 1
        assert!(far < near, "a larger deviation penalises harder");
        assert!(near < ONE);
        // n/T = 8 ⇒ O = 1/√8 = 0.353553…
        assert_eq!(fmt_q32(far), "0.353553");
    }

    /// Guards: no panic, no divide-by-zero, on the degenerate inputs the corpus
    /// can actually produce (a one-cell candidate, a zero target).
    #[test]
    fn objective_guards_degenerate_inputs() {
        assert_eq!(go_explore_objective_q32(&[5], 64), 0, "one cell");
        assert_eq!(go_explore_objective_q32(&[], 64), 0, "no cells");
        assert_eq!(go_explore_objective_q32(&[1, 1], 0), 0, "zero target");
        assert_eq!(ratio_q32(1, 0), 0);
        assert_eq!(log2_q32(0), 0);
    }

    /// Rendering is pure integer division: exact halves and sixths render the
    /// same on every host.
    #[test]
    fn fmt_renders_six_places_by_integer_division() {
        assert_eq!(fmt_q32(0), "0.000000");
        assert_eq!(fmt_q32(ONE), "1.000000");
        assert_eq!(fmt_q32(ONE / 2), "0.500000");
        assert_eq!(fmt_q32(3 * ONE + ONE / 4), "3.250000");
        // Round-half-up, not truncation: a mean of 149/40 = 3.725 must not
        // render as `3.724999` just because Q32 cannot hold it exactly.
        assert_eq!(
            fmt_q32(mean_q32(&[3 * ONE, 4 * ONE, 4 * ONE, 4 * ONE])),
            "3.750000"
        );
        assert_eq!(fmt_q32(((149u128 << 32) / 40) as u64), "3.725000");
        // A carry out of the fraction propagates into the integer part.
        assert_eq!(fmt_q32(ONE - 1), "1.000000");
    }

    #[test]
    fn mean_of_fixed_point_values() {
        assert_eq!(mean_q32(&[]), 0);
        assert_eq!(mean_q32(&[ONE, 3 * ONE]), 2 * ONE);
        assert_eq!(fmt_q32(mean_q32(&[0, ONE])), "0.500000");
    }

    /// `ratio_q32` is the QD-coverage normalizer: exact on the cases the report
    /// prints.
    #[test]
    fn ratio_is_exact_on_simple_fractions() {
        assert_eq!(fmt_q32(ratio_q32(1, 4)), "0.250000");
        assert_eq!(fmt_q32(ratio_q32(4, 4)), "1.000000");
        assert_eq!(fmt_q32(ratio_q32(3, 1000)), "0.003000");
    }
}
