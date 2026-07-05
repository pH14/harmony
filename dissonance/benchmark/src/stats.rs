// SPDX-License-Identifier: AGPL-3.0-or-later
//! The correlation statistics — **the load-bearing math of a GO/NO-GO gate**.
//!
//! A wrong correlation is a wrong verdict, so every *decision* here is an exact
//! integer computation: Spearman's ρ is Pearson's correlation of tie-corrected
//! midranks, kept as the triple `(cov, dx, dy)` with `ρ = cov / √(dx·dy)`, and
//! any threshold comparison (`ρ ⋛ num/den`) is decided by squaring and
//! cross-multiplying `i128`s — never by comparing an `f64`. Only
//! [`RankCorr::rho_f64`] produces a float, and only for the report's prose
//! (conventions rule 4).
//!
//! Ranks are computed with **midranks** (tied values share the mean of the ranks
//! they span), scaled ×2 so a half-integer average stays an integer — the
//! standard tie correction, so this is the general Pearson-on-ranks ρ, not the
//! `1 − 6Σd²/…` shortcut (which is wrong under ties).

use explorer::stads::Frac;
use std::cmp::Ordering;

/// A computed rank correlation, held exactly. `ρ = cov / √(dx·dy)`; `dx`/`dy` are
/// the rank variances (always ≥ 0) and `cov` the rank covariance (signed — its
/// sign is the sign of ρ, i.e. the *direction* of the correlation).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RankCorr {
    /// Number of paired samples.
    pub n: u64,
    cov: i128,
    dx: i128,
    dy: i128,
}

impl RankCorr {
    /// Whether ρ is defined (both variables vary; a constant column makes ρ
    /// undefined — treated as 0 by the comparisons).
    pub fn is_defined(&self) -> bool {
        self.dx > 0 && self.dy > 0
    }

    /// The sign of ρ: `+1`, `0`, or `-1`. `+` means more-of-x ⇒ more-of-y.
    pub fn direction(&self) -> i8 {
        if !self.is_defined() {
            return 0;
        }
        match self.cov.cmp(&0) {
            Ordering::Greater => 1,
            Ordering::Less => -1,
            Ordering::Equal => 0,
        }
    }

    /// Compare ρ to the exact rational `num/den` (`den` must be > 0). Decided by
    /// integer squaring/cross-multiplication — no float. An undefined ρ compares
    /// as 0.
    pub fn cmp_rho(&self, num: i128, den: i128) -> Ordering {
        debug_assert!(den > 0);
        if !self.is_defined() {
            // ρ ≜ 0: sign of (0 − num/den) = −sign(num) ⇒ 0.cmp(num).
            return 0i128.cmp(&num);
        }
        let d = self.dx * self.dy; // > 0
        let a = self.cov * den; // sign(ρ − num/den) = sign(a − num·√d)
        match num.cmp(&0) {
            Ordering::Equal => a.cmp(&0),
            Ordering::Greater => {
                if a < 0 {
                    Ordering::Less
                } else {
                    // both ≥ 0: a ⋛ num·√d ⟺ a² ⋛ num²·d
                    (a * a).cmp(&(num * num * d))
                }
            }
            Ordering::Less => {
                if a > 0 {
                    Ordering::Greater
                } else {
                    // both ≤ 0: a ⋛ num·√d ⟺ a² ⋚ num²·d (magnitudes reversed)
                    (num * num * d).cmp(&(a * a))
                }
            }
        }
    }

    /// `ρ ≥ num/den`, exactly.
    pub fn at_least(&self, num: i128, den: i128) -> bool {
        self.cmp_rho(num, den) != Ordering::Less
    }

    /// `ρ ≤ num/den`, exactly.
    pub fn at_most(&self, num: i128, den: i128) -> bool {
        self.cmp_rho(num, den) != Ordering::Greater
    }

    /// ρ as an `f64`, for the report's prose **only** — never a decision input.
    pub fn rho_f64(&self) -> f64 {
        if !self.is_defined() {
            return 0.0;
        }
        let d = (self.dx as f64) * (self.dy as f64);
        (self.cov as f64) / d.sqrt()
    }
}

/// Tie-corrected midranks, scaled ×2 (so a half-integer mean rank stays an
/// integer). A tie group spanning 0-based sorted positions `i..=j` gets rank×2 =
/// `(i+1)+(j+1) = i+j+2`.
fn midranks_x2(xs: &[u64]) -> Vec<i128> {
    let n = xs.len();
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by_key(|&i| xs[i]);
    let mut r = vec![0i128; n];
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j + 1 < n && xs[idx[j + 1]] == xs[idx[i]] {
            j += 1;
        }
        let rank_x2 = (i + j + 2) as i128;
        for &t in &idx[i..=j] {
            r[t] = rank_x2;
        }
        i = j + 1;
    }
    r
}

/// Spearman's rank correlation of two equal-length samples, exact. `None` if the
/// lengths differ or `n < 2`.
pub fn spearman(xs: &[u64], ys: &[u64]) -> Option<RankCorr> {
    if xs.len() != ys.len() || xs.len() < 2 {
        return None;
    }
    let n = xs.len() as i128;
    let rx = midranks_x2(xs);
    let ry = midranks_x2(ys);
    let (mut sx, mut sy, mut sxx, mut syy, mut sxy) = (0i128, 0i128, 0i128, 0i128, 0i128);
    for k in 0..rx.len() {
        let (x, y) = (rx[k], ry[k]);
        sx += x;
        sy += y;
        sxx += x * x;
        syy += y * y;
        sxy += x * y;
    }
    Some(RankCorr {
        n: xs.len() as u64,
        cov: n * sxy - sx * sy,
        dx: n * sxx - sx * sx,
        dy: n * syy - sy * sy,
    })
}

/// The exact median of a sample as a rational (`den` ∈ {1, 2}). `None` if empty.
pub fn median(xs: &[u64]) -> Option<Frac> {
    if xs.is_empty() {
        return None;
    }
    let mut v = xs.to_vec();
    v.sort_unstable();
    let n = v.len();
    Some(if !n.is_multiple_of(2) {
        Frac::whole(v[n / 2] as u128)
    } else {
        Frac::new(v[n / 2 - 1] as u128 + v[n / 2] as u128, 2)
    })
}

/// The Tukey-hinge quartiles `(q1, q2, q3)`: `q2` is the median; `q1`/`q3` are the
/// medians of the lower/upper halves (excluding the overall median when `n` is
/// odd). Exact rationals. `None` if fewer than 2 samples.
pub fn quartiles(xs: &[u64]) -> Option<(Frac, Frac, Frac)> {
    if xs.len() < 2 {
        return None;
    }
    let mut v = xs.to_vec();
    v.sort_unstable();
    let n = v.len();
    let half = n / 2;
    let lower = &v[..half];
    let upper = if n.is_multiple_of(2) {
        &v[half..]
    } else {
        &v[half + 1..]
    };
    Some((median(lower)?, median(&v)?, median(upper)?))
}

/// The inter-quartile range `q3 − q1` as an exact rational (spread; report only).
/// `None` if fewer than 2 samples.
pub fn iqr(xs: &[u64]) -> Option<Frac> {
    let (q1, _, q3) = quartiles(xs)?;
    // q3 ≥ q1, so the difference is non-negative: (q3n·q1d − q1n·q3d)/(q3d·q1d).
    let num = q3.num() * q1.den() - q1.num() * q3.den();
    Some(Frac::new(num, q3.den() * q1.den()))
}

/// A rational as an `f64`, for report rendering only.
pub fn frac_f64(f: Frac) -> f64 {
    f.num() as f64 / f.den() as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // --- Known-answer tests (validated against the textbook Spearman formula and
    // cross-checked with scipy.stats.spearmanr in comments) ---

    #[test]
    fn spearman_no_ties_known_answer_0_8() {
        // x=[1,2,3,4,5], y=[2,1,3,5,4]: Σd²=4 ⇒ ρ = 1 − 6·4/(5·24) = 0.8 = 4/5.
        // scipy.stats.spearmanr → 0.8.
        let c = spearman(&[1, 2, 3, 4, 5], &[2, 1, 3, 5, 4]).unwrap();
        assert_eq!(c.cmp_rho(4, 5), Ordering::Equal, "ρ must be exactly 4/5");
        assert_eq!(c.cmp_rho(7, 10), Ordering::Greater);
        assert_eq!(c.cmp_rho(9, 10), Ordering::Less);
        assert!((c.rho_f64() - 0.8).abs() < 1e-12);
        assert_eq!(c.direction(), 1);
    }

    #[test]
    fn spearman_with_ties_known_answer_5_6() {
        // x=[1,1,2,3], y=[1,2,2,3]. Midranks×2: x=[3,3,6,8], y=[2,5,5,8].
        // Pearson-on-ranks ρ = 60/72 = 5/6 ≈ 0.8333.
        // scipy.stats.spearmanr([1,1,2,3],[1,2,2,3]) → 0.8333333.
        let c = spearman(&[1, 1, 2, 3], &[1, 2, 2, 3]).unwrap();
        assert_eq!(c.cmp_rho(5, 6), Ordering::Equal, "ρ must be exactly 5/6");
        assert!((c.rho_f64() - 5.0 / 6.0).abs() < 1e-12);
    }

    #[test]
    fn perfect_positive_and_negative() {
        let pos = spearman(&[1, 2, 3, 4], &[1, 2, 3, 4]).unwrap();
        assert_eq!(pos.cmp_rho(1, 1), Ordering::Equal, "ρ = +1");
        assert_eq!(pos.direction(), 1);
        let neg = spearman(&[1, 2, 3, 4], &[4, 3, 2, 1]).unwrap();
        assert_eq!(neg.cmp_rho(-1, 1), Ordering::Equal, "ρ = −1");
        assert_eq!(neg.direction(), -1);
        assert!(neg.at_most(-1, 2), "ρ = −1 ≤ −1/2");
    }

    #[test]
    fn constant_column_is_undefined_treated_as_zero() {
        let c = spearman(&[5, 5, 5, 5], &[1, 2, 3, 4]).unwrap();
        assert!(!c.is_defined());
        assert_eq!(c.direction(), 0);
        assert_eq!(c.cmp_rho(0, 1), Ordering::Equal);
        assert_eq!(c.cmp_rho(1, 10), Ordering::Less); // 0 < 0.1
        assert_eq!(c.cmp_rho(-1, 10), Ordering::Greater); // 0 > −0.1
    }

    #[test]
    fn too_few_or_mismatched_is_none() {
        assert!(spearman(&[1], &[1]).is_none());
        assert!(spearman(&[1, 2], &[1]).is_none());
    }

    #[test]
    fn median_and_iqr_exact() {
        assert_eq!(median(&[3, 1, 2]).unwrap(), Frac::whole(2));
        assert_eq!(median(&[4, 1, 2, 3]).unwrap(), Frac::new(5, 2)); // (2+3)/2
        // [1,2,3,4,5,6,7,8]: q1=median([1,2,3,4])=2.5, q3=median([5,6,7,8])=6.5,
        // IQR = 4.
        let (q1, q2, q3) = quartiles(&[8, 7, 6, 5, 4, 3, 2, 1]).unwrap();
        assert_eq!(q1, Frac::new(5, 2));
        assert_eq!(q2, Frac::new(9, 2));
        assert_eq!(q3, Frac::new(13, 2));
        assert_eq!(iqr(&[8, 7, 6, 5, 4, 3, 2, 1]).unwrap(), Frac::whole(4));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(if cfg!(miri) { 8 } else { 512 }))]

        /// ρ ∈ [−1, 1] always; symmetric in its arguments; and negating the y
        /// ordering flips the sign to exactly the negation.
        #[test]
        fn rho_invariants(
            pairs in prop::collection::vec((0u64..20, 0u64..20), 2..40),
        ) {
            let xs: Vec<u64> = pairs.iter().map(|p| p.0).collect();
            let ys: Vec<u64> = pairs.iter().map(|p| p.1).collect();
            let c = spearman(&xs, &ys).unwrap();
            // Bounds, exact.
            prop_assert!(c.at_most(1, 1), "ρ ≤ 1");
            prop_assert!(c.at_least(-1, 1), "ρ ≥ −1");
            // Symmetry.
            let cs = spearman(&ys, &xs).unwrap();
            prop_assert_eq!(c.rho_f64().to_bits(), cs.rho_f64().to_bits());
            // Negating y's order negates ρ (map y ↦ MAX−y is order-reversing).
            let ry: Vec<u64> = ys.iter().map(|&v| 1_000 - v).collect();
            let cn = spearman(&xs, &ry).unwrap();
            prop_assert!((c.rho_f64() + cn.rho_f64()).abs() < 1e-9);
        }

        /// The exact `cmp_rho` agrees with the float ρ (away from the boundary),
        /// proving the integer decision path tracks the real value.
        #[test]
        fn cmp_rho_agrees_with_float(
            pairs in prop::collection::vec((0u64..30, 0u64..30), 3..40),
            t in -9i128..=9,
        ) {
            let xs: Vec<u64> = pairs.iter().map(|p| p.0).collect();
            let ys: Vec<u64> = pairs.iter().map(|p| p.1).collect();
            let c = spearman(&xs, &ys).unwrap();
            let thr = t as f64 / 10.0;
            let rho = c.rho_f64();
            // Only assert when comfortably off the boundary (float noise).
            if (rho - thr).abs() > 1e-6 {
                let want = if rho > thr { Ordering::Greater } else { Ordering::Less };
                prop_assert_eq!(c.cmp_rho(t, 10), want, "rho={} thr={}", rho, thr);
            }
        }
    }
}
