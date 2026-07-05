// SPDX-License-Identifier: AGPL-3.0-or-later
//! STADS — *Software Testing As Discovery of Species* (Böhme, TOSEM 2018).
//!
//! A **progression-blind** fold over an opaque stream of [`CellKey`] discovery
//! events. It answers two questions a search must not answer for itself: *is the
//! signal still discovering?* (Good–Turing discovery probability) and *how much
//! is estimated left?* (Chao1 richness). Species are cells; samples are branches.
//!
//! Nothing here inspects *what* a cell means or *how* it was reached — it folds
//! counts of opaque [`CellKey`]s and nothing else. That is deliberate: task 70's
//! Selector v3 consumes this estimator for a state-affecting stopping rule, so it
//! must be exactly as blind as the archive is (invariant 5). Per conventions rule
//! 4 the arithmetic is integer/rational only: Good–Turing is the exact fraction
//! `f1/n`, Chao1 an exact rational built by integer multiplication, and every
//! threshold comparison is a cross-multiplication. **No floating point appears in
//! this module** — the estimator never renders; only a report does (task 69's
//! `benchmark` crate), and it converts [`Frac`] itself.

use crate::CellKey;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An exact non-negative rational, kept in lowest terms. The estimator's return
/// currency: callers compare [`Frac`]s (via [`Ord`], which cross-multiplies) or
/// read [`Frac::num`]/[`Frac::den`] and do their own rendering. Keeping the ratio
/// exact — rather than collapsing to an `f64` here — is what keeps a
/// float out of anything a search could key on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Frac {
    num: u128,
    den: u128,
}

impl Frac {
    /// The rational `num/den`, reduced to lowest terms. `den` of 0 is clamped to
    /// 1 (an estimator division by zero is defined as `0`, never a panic — this
    /// is library code on a live campaign, rule 4).
    pub fn new(num: u128, den: u128) -> Self {
        if den == 0 {
            return Frac { num: 0, den: 1 };
        }
        let g = gcd(num, den);
        let g = if g == 0 { 1 } else { g };
        Frac {
            num: num / g,
            den: den / g,
        }
    }

    /// A whole number as a rational.
    pub fn whole(n: u128) -> Self {
        Frac { num: n, den: 1 }
    }

    /// The (reduced) numerator.
    pub fn num(&self) -> u128 {
        self.num
    }

    /// The (reduced, always ≥ 1) denominator.
    pub fn den(&self) -> u128 {
        self.den
    }
}

impl PartialOrd for Frac {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Frac {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // a/b ? c/d  ⟺  a·d ? c·b  (all non-negative; u128 headroom is ample for
        // campaign magnitudes — cells ≲ 10⁴, counts ≲ 10⁶).
        (self.num * other.den).cmp(&(other.num * self.den))
    }
}

/// Euclid's algorithm, the only helper the estimator needs.
fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// A serializable snapshot of the frequency-count spectrum at one point in a
/// campaign — everything the estimators are derived from, and nothing else. A
/// report serializes a sequence of these to draw species curves without reaching
/// into the accumulator's private map.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct SpeciesStats {
    /// Number of samples (branches) folded so far.
    pub samples: u64,
    /// Total individuals (discovery events) folded so far — the `n` of `f1/n`.
    pub individuals: u64,
    /// Observed species richness `S_obs` — distinct cells seen.
    pub s_obs: u64,
    /// `f1`: species observed exactly once (singletons).
    pub f1: u64,
    /// `f2`: species observed exactly twice (doubletons).
    pub f2: u64,
}

impl SpeciesStats {
    /// Good–Turing discovery probability: the estimated probability that the next
    /// individual belongs to a *new* species, `f1 / n`. Exactly the missing
    /// probability mass. `0` when nothing has been folded.
    pub fn discovery_probability(&self) -> Frac {
        Frac::new(self.f1 as u128, self.individuals as u128)
    }

    /// Chao1 lower-bound richness estimate. With doubletons,
    /// `S_obs + f1²/(2·f2)`; with none, the bias-corrected `S_obs + f1·(f1−1)/2`.
    /// Returned exactly as a [`Frac`] (`S_obs` folds in over the common
    /// denominator) so the estimate never rounds before a report renders it.
    pub fn chao1(&self) -> Frac {
        let s = self.s_obs as u128;
        let f1 = self.f1 as u128;
        let f2 = self.f2 as u128;
        if f2 > 0 {
            // S_obs + f1²/(2 f2) = (2·f2·S_obs + f1²) / (2 f2)
            Frac::new(2 * f2 * s + f1 * f1, 2 * f2)
        } else {
            // S_obs + f1(f1-1)/2, an integer. f1.saturating_sub keeps f1==0 at 0.
            Frac::whole(s + f1 * f1.saturating_sub(1) / 2)
        }
    }

    /// Is discovery estimated to have fallen below the threshold `eps_num/eps_den`?
    /// The prototype STADS stopping rule (task 70's Selector v3 consumes it):
    /// `f1/n < eps` decided by cross-multiplication, never a float compare.
    /// Before any individual is folded discovery is *not* below threshold (the
    /// campaign has not started), matching `f1/n = 0/0 ≜ 1` intent.
    pub fn discovery_below(&self, eps_num: u64, eps_den: u64) -> bool {
        if self.individuals == 0 || eps_den == 0 {
            return false;
        }
        // f1/n < eps_num/eps_den  ⟺  f1·eps_den < eps_num·n
        (self.f1 as u128) * (eps_den as u128) < (eps_num as u128) * (self.individuals as u128)
    }
}

/// A fold over a stream of opaque cell discoveries, accumulating the frequency
/// spectrum (`f_k`) and the species-accumulation curve (`S_obs` per sample).
///
/// A **sample** is a branch; an **individual** is one discovery event. Fold each
/// branch's discovered cells with [`observe`](Self::observe), then close the
/// branch with [`end_sample`](Self::end_sample) (or use
/// [`observe_branch`](Self::observe_branch) for both). The estimators live on the
/// [`SpeciesStats`] snapshot from [`stats`](Self::stats).
#[derive(Clone, Debug, Default)]
pub struct SpeciesAccumulator {
    // Abundance per species: how many individuals (discovery events) named it.
    // BTreeMap, never a HashMap — even though only counts (not order) reach an
    // output, the determinism bar here is a hard project invariant.
    counts: BTreeMap<CellKey, u64>,
    // f_k spectrum, maintained incrementally so `stats()` is O(1). spectrum[k-1]
    // = number of species with abundance exactly k, for k ≥ 1; only f1/f2 are
    // read but the full spectrum is cheap and keeps the fold honest.
    f1: u64,
    f2: u64,
    samples: u64,
    individuals: u64,
    // curve[i] = S_obs after (i+1) samples — the species-accumulation curve.
    curve: Vec<u64>,
}

impl SpeciesAccumulator {
    /// A fresh, empty fold.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one discovery event for `cell`, updating its abundance and the
    /// `f1`/`f2` spectrum. Does **not** advance the sample counter — call
    /// [`end_sample`](Self::end_sample) at the branch boundary.
    pub fn observe(&mut self, cell: CellKey) {
        let c = self.counts.entry(cell).or_insert(0);
        let before = *c;
        *c = before + 1;
        let after = before + 1;
        // Maintain f1/f2 as the abundance crosses 1→2→3.
        match after {
            1 => self.f1 += 1,
            2 => {
                self.f1 -= 1;
                self.f2 += 1;
            }
            3 => self.f2 -= 1,
            _ => {}
        }
        self.individuals += 1;
    }

    /// Close the current branch: record a point on the species-accumulation curve
    /// and advance the sample counter. Idempotent-safe to call after zero
    /// observations (records an empty sample, as a barren branch genuinely is).
    pub fn end_sample(&mut self) {
        self.samples += 1;
        self.curve.push(self.s_obs());
    }

    /// Fold a whole branch: observe every discovered cell, then close the sample.
    pub fn observe_branch(&mut self, cells: impl IntoIterator<Item = CellKey>) {
        for c in cells {
            self.observe(c);
        }
        self.end_sample();
    }

    /// Observed species richness `S_obs` — distinct cells folded so far.
    pub fn s_obs(&self) -> u64 {
        self.counts.len() as u64
    }

    /// The current frequency-count snapshot the estimators read from.
    pub fn stats(&self) -> SpeciesStats {
        SpeciesStats {
            samples: self.samples,
            individuals: self.individuals,
            s_obs: self.s_obs(),
            f1: self.f1,
            f2: self.f2,
        }
    }

    /// The species-accumulation curve: `S_obs` after each closed sample. Index `i`
    /// is the richness after `i + 1` branches — the STADS species curve a report
    /// plots against branch count.
    pub fn curve(&self) -> &[u64] {
        &self.curve
    }

    /// Good–Turing discovery probability at the current fold. See
    /// [`SpeciesStats::discovery_probability`].
    pub fn discovery_probability(&self) -> Frac {
        self.stats().discovery_probability()
    }

    /// Chao1 richness at the current fold. See [`SpeciesStats::chao1`].
    pub fn chao1(&self) -> Frac {
        self.stats().chao1()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn key(n: u64) -> CellKey {
        n.to_be_bytes().to_vec()
    }

    #[test]
    fn empty_fold_is_defined() {
        let a = SpeciesAccumulator::new();
        let s = a.stats();
        assert_eq!(s.s_obs, 0);
        assert_eq!(s.f1, 0);
        assert_eq!(a.discovery_probability(), Frac::new(0, 1));
        assert_eq!(a.chao1(), Frac::whole(0));
        // No individual folded ⇒ never "below" any positive threshold.
        assert!(!s.discovery_below(1, 1000));
    }

    #[test]
    fn frac_reduces_and_orders() {
        assert_eq!(Frac::new(2, 4), Frac::new(1, 2));
        assert_eq!(Frac::new(5, 0), Frac::new(0, 1)); // div-by-zero ≜ 0
        assert!(Frac::new(1, 3) < Frac::new(1, 2));
        assert!(Frac::new(2, 3) > Frac::new(1, 2));
        assert_eq!(Frac::new(3, 3), Frac::whole(1));
    }

    #[test]
    fn spectrum_tracks_abundance_transitions() {
        let mut a = SpeciesAccumulator::new();
        a.observe(key(1)); // {1:1}   f1=1 f2=0
        assert_eq!((a.stats().f1, a.stats().f2), (1, 0));
        a.observe(key(1)); // {1:2}   f1=0 f2=1
        assert_eq!((a.stats().f1, a.stats().f2), (0, 1));
        a.observe(key(2)); // +2:1    f1=1 f2=1
        assert_eq!((a.stats().f1, a.stats().f2), (1, 1));
        a.observe(key(1)); // {1:3}   f1=1 f2=0
        assert_eq!((a.stats().f1, a.stats().f2), (1, 0));
        assert_eq!(a.s_obs(), 2);
        assert_eq!(a.stats().individuals, 4);
    }

    #[test]
    fn good_turing_is_f1_over_n() {
        // 3 singletons out of 5 individuals: U = 3/5.
        let mut a = SpeciesAccumulator::new();
        for c in [1, 1, 2, 3, 4] {
            a.observe(key(c));
        }
        // abundances: 1→2, 2→1, 3→1, 4→1  ⇒ f1 = 3, n = 5
        assert_eq!(a.discovery_probability(), Frac::new(3, 5));
    }

    #[test]
    fn chao1_known_answer_with_doubletons() {
        // Construct S_obs=4, f1=2, f2=1  ⇒ Chao1 = 4 + 2²/(2·1) = 6.
        let mut a = SpeciesAccumulator::new();
        // species A: abundance 3, B: 3 (not singleton/doubleton),
        // C: abundance 2 (doubleton), D & E: abundance 1 (singletons)
        for _ in 0..3 {
            a.observe(key(10));
        }
        for _ in 0..3 {
            a.observe(key(11));
        }
        for _ in 0..2 {
            a.observe(key(12));
        }
        a.observe(key(13));
        a.observe(key(14));
        let s = a.stats();
        assert_eq!((s.s_obs, s.f1, s.f2), (5, 2, 1));
        // Chao1 = 5 + 2²/(2·1) = 5 + 2 = 7.
        assert_eq!(a.chao1(), Frac::whole(7));
    }

    #[test]
    fn chao1_no_doubletons_uses_bias_correction() {
        // f1=3, f2=0, S_obs=3 ⇒ S_obs + f1(f1-1)/2 = 3 + 3 = 6.
        let mut a = SpeciesAccumulator::new();
        a.observe(key(1));
        a.observe(key(2));
        a.observe(key(3));
        let s = a.stats();
        assert_eq!((s.s_obs, s.f1, s.f2), (3, 3, 0));
        assert_eq!(a.chao1(), Frac::whole(6));
    }

    #[test]
    fn accumulation_curve_is_monotone_nondecreasing() {
        let mut a = SpeciesAccumulator::new();
        a.observe_branch([key(1), key(2)]); // S_obs=2
        a.observe_branch([key(2)]); // S_obs=2 (no new)
        a.observe_branch([key(3)]); // S_obs=3
        assert_eq!(a.curve(), &[2, 2, 3]);
        assert_eq!(a.stats().samples, 3);
    }

    #[test]
    fn discovery_below_cross_multiplies() {
        // f1=1, n=100 ⇒ U = 1/100 = 0.01. Below 0.02 (2/100), not below 0.005.
        let mut a = SpeciesAccumulator::new();
        a.observe(key(1)); // singleton
        for _ in 0..99 {
            a.observe(key(2)); // one heavily-hit species
        }
        let s = a.stats();
        assert_eq!(s.f1, 1);
        assert_eq!(s.individuals, 100);
        assert!(s.discovery_below(2, 100));
        assert!(!s.discovery_below(5, 1000));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(if cfg!(miri) { 8 } else { 512 }))]

        /// Against a synthetic community of KNOWN richness, the estimator's
        /// invariants hold: S_obs never exceeds true richness; Chao1 is a lower
        /// bound (≥ S_obs); discovery probability lies in [0,1]; and folding is
        /// order-independent (determinism).
        #[test]
        fn stads_invariants_over_known_richness(
            // up to 30 species with abundances 1..=8, then a shuffle permutation
            abundances in prop::collection::vec(1u64..=8, 1..30),
        ) {
            let true_richness = abundances.len() as u64;
            // Build the individual stream: species i repeated abundance[i] times.
            let mut stream: Vec<CellKey> = Vec::new();
            for (i, &ab) in abundances.iter().enumerate() {
                for _ in 0..ab {
                    stream.push(key(i as u64));
                }
            }

            let mut a = SpeciesAccumulator::new();
            for c in &stream {
                a.observe(c.clone());
            }
            let s = a.stats();

            // S_obs equals true richness once every species is sampled ≥1×.
            prop_assert_eq!(s.s_obs, true_richness);
            // Chao1 is a lower-bound richness estimate: never below what we saw.
            prop_assert!(a.chao1() >= Frac::whole(s.s_obs as u128));
            // Discovery probability in [0,1].
            prop_assert!(a.discovery_probability() >= Frac::new(0, 1));
            prop_assert!(a.discovery_probability() <= Frac::whole(1));
            // f1 + f2 ≤ S_obs and both ≤ individuals.
            prop_assert!(s.f1 + s.f2 <= s.s_obs);
            prop_assert!(s.individuals >= s.s_obs);

            // Order independence: reversing the stream yields identical stats.
            let mut b = SpeciesAccumulator::new();
            for c in stream.iter().rev() {
                b.observe(c.clone());
            }
            prop_assert_eq!(b.stats(), s);
        }

        /// Good–Turing equals exactly f1/n, recomputed independently from the
        /// abundance histogram (known-answer, integer).
        #[test]
        fn good_turing_matches_independent_count(
            abundances in prop::collection::vec(1u64..=6, 1..25),
        ) {
            let mut a = SpeciesAccumulator::new();
            let mut n = 0u128;
            for (i, &ab) in abundances.iter().enumerate() {
                for _ in 0..ab {
                    a.observe(key(i as u64));
                    n += 1;
                }
            }
            let f1 = abundances.iter().filter(|&&x| x == 1).count() as u128;
            prop_assert_eq!(a.discovery_probability(), Frac::new(f1, n));
        }
    }
}
