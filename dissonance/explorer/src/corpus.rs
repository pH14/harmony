// SPDX-License-Identifier: AGPL-3.0-or-later
//! The corpus and its **deterministic** coverage-novelty index.
//!
//! A corpus entry is `(SnapId, Environment, CovScore)`: a branchable snapshot, the
//! genesis-complete reproducer that produced it, and its novelty magnitude.
//! [`admit`](Corpus::admit) is the AFL-style frontier test — it returns `true`
//! exactly when a coverage map introduces an edge/bucket the accumulated set has
//! never seen — and the index is a [`BTreeSet`] so no `HashMap` iteration order
//! can reach the kept set or the admission decision (conventions rule 4).

use std::collections::BTreeSet;

use crate::{Environment, SnapId};

/// A run's coverage-novelty magnitude: the count of new edge/bucket pairs it
/// introduced versus the accumulated set at admission time. Deterministic and
/// order-independent; used to rank corpus entries and to break eviction ties.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct CovScore(pub u64);

/// One kept corpus entry.
#[derive(Clone, Debug)]
struct Entry {
    snap: SnapId,
    env: Environment,
    score: CovScore,
}

/// The default capacity — the number of entries kept before eviction begins.
/// Eviction (lowest score, ties by admission order) drives the corpus-GC path; the
/// integrator may re-pick the magnitude with [`Corpus::with_capacity`].
const DEFAULT_CAPACITY: usize = 64;

/// The deterministic corpus: kept entries plus the accumulated novelty index.
/// Eviction beyond capacity records the evicted [`SnapId`]s for the explorer to
/// `drop_snap` (corpus GC); no handle is reused after it is drained.
#[derive(Clone, Debug)]
pub struct Corpus {
    entries: Vec<Entry>,
    /// The accumulated `(edge index, bucket)` novelty set — sorted, so iteration
    /// order never reaches the kept set or the admission decision.
    seen: BTreeSet<(usize, u8)>,
    capacity: usize,
    /// Snapshots evicted since the last [`drain_evicted`](Corpus::drain_evicted),
    /// awaiting `drop_snap`.
    evicted: Vec<SnapId>,
}

impl Default for Corpus {
    fn default() -> Self {
        Self::new()
    }
}

impl Corpus {
    /// An empty corpus at the default capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// An empty corpus at an explicit capacity (clamped to at least one, so a
    /// single admitted entry is always retained). Additional helper for the
    /// corpus-GC gate, which forces eviction at a small capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Vec::new(),
            seen: BTreeSet::new(),
            capacity: capacity.max(1),
            evicted: Vec::new(),
        }
    }

    /// Admit `(snap, env)` iff its `coverage` shows a new edge/bucket versus the
    /// accumulated set; return whether it was novel. On admission the new pairs
    /// fold into the index and, if the corpus is now over capacity, the
    /// lowest-scoring older entry is evicted (its [`SnapId`] queued for
    /// `drop_snap`). The decision and the kept set are a pure function of the
    /// admitted sequence — never of map iteration order.
    pub fn admit(&mut self, snap: SnapId, env: Environment, coverage: &[u8]) -> bool {
        // Count the pairs this coverage introduces; collect them so we fold the
        // index only on a real admission (an order-independent walk of the slice).
        let mut fresh: Vec<(usize, u8)> = Vec::new();
        for (i, &count) in coverage.iter().enumerate() {
            let b = bucket(count);
            if b != 0 && !self.seen.contains(&(i, b)) {
                fresh.push((i, b));
            }
        }
        if fresh.is_empty() {
            return false;
        }
        let score = CovScore(fresh.len() as u64);
        for pair in fresh {
            self.seen.insert(pair);
        }
        self.entries.push(Entry { snap, env, score });
        self.evict_over_capacity();
        true
    }

    /// Score a coverage map against the accumulated set **without** admitting:
    /// the count of new edge/bucket pairs it would introduce. Used to fill a
    /// [`RunOutcome`](crate::RunOutcome)'s novelty before the admit decision.
    pub fn novelty(&self, coverage: &[u8]) -> CovScore {
        let mut n = 0u64;
        for (i, &count) in coverage.iter().enumerate() {
            let b = bucket(count);
            if b != 0 && !self.seen.contains(&(i, b)) {
                n += 1;
            }
        }
        CovScore(n)
    }

    /// The number of kept entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the corpus has no kept entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Deterministically select a kept entry to branch from, indexed by `salt`
    /// (returns its snapshot and genesis-complete env). `None` on an empty
    /// corpus. Additional helper the [`Strategy`](crate::Strategy) drives the
    /// Multiverse pick with.
    pub fn select(&self, salt: u64) -> Option<(SnapId, &Environment)> {
        if self.entries.is_empty() {
            return None;
        }
        let idx = (salt % self.entries.len() as u64) as usize;
        let e = &self.entries[idx];
        Some((e.snap, &e.env))
    }

    /// The `i`-th kept entry `(snap, env, score)`, in admission order. Additional
    /// helper for inspecting/comparing the corpus (e.g. the determinism gate's
    /// "identical set of admitted entries" check).
    pub fn entry(&self, i: usize) -> Option<(SnapId, &Environment, CovScore)> {
        self.entries.get(i).map(|e| (e.snap, &e.env, e.score))
    }

    /// The genesis-complete env of the entry holding `snap`, if any — the
    /// compose base when rebasing a [`Bug`](crate::Bug) found below `snap`.
    pub fn base_env(&self, snap: SnapId) -> Option<&Environment> {
        self.entries.iter().find(|e| e.snap == snap).map(|e| &e.env)
    }

    /// Take the snapshots evicted since the last drain, for the explorer to
    /// `drop_snap`. Leaves the queue empty.
    pub fn drain_evicted(&mut self) -> Vec<SnapId> {
        std::mem::take(&mut self.evicted)
    }

    /// Evict lowest-scoring older entries until within capacity. The just-pushed
    /// entry (the newest novelty) is retained — the search is over `..len()-1`.
    /// Among equal lowest scores the **earliest-admitted** is evicted (a strict
    /// `<` keeps the first as the victim), which equals lowest [`SnapId`] when
    /// snapshots mint monotonically; either way eviction is a pure function of the
    /// admitted sequence.
    fn evict_over_capacity(&mut self) {
        while self.entries.len() > self.capacity {
            let upto = self.entries.len() - 1;
            let mut victim = 0usize;
            for i in 1..upto {
                if self.entries[i].score < self.entries[victim].score {
                    victim = i;
                }
            }
            let e = self.entries.remove(victim);
            self.evicted.push(e.snap);
        }
    }
}

/// The AFL count-bucket classifier: collapse a raw edge hit-count into a small
/// bucket so "novel" means a coarse new behaviour, not every off-by-one count.
/// Bucket `0` is "edge never hit" and is not itself novelty.
fn bucket(count: u8) -> u8 {
    match count {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 3,
        4..=7 => 4,
        8..=15 => 5,
        16..=31 => 6,
        32..=127 => 7,
        _ => 8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Environment {
        Environment {
            blob_version: 1,
            bytes: vec![],
        }
    }

    #[test]
    fn first_nonzero_coverage_is_novel_then_subset_is_not() {
        let mut c = Corpus::new();
        assert!(c.admit(SnapId(1), env(), &[0, 1, 0, 0]));
        // Same single edge again — no new bucket.
        assert!(!c.admit(SnapId(2), env(), &[0, 1, 0, 0]));
        // A new edge index → novel.
        assert!(c.admit(SnapId(3), env(), &[0, 1, 0, 5]));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn all_zero_coverage_is_never_novel() {
        let mut c = Corpus::new();
        assert!(!c.admit(SnapId(1), env(), &[0, 0, 0]));
        assert!(c.is_empty());
    }

    #[test]
    fn a_higher_bucket_on_a_known_edge_is_novel() {
        let mut c = Corpus::new();
        assert!(c.admit(SnapId(1), env(), &[1])); // bucket 1
        assert!(c.admit(SnapId(2), env(), &[8])); // same edge, bucket 5 → novel
        assert!(!c.admit(SnapId(3), env(), &[9])); // bucket 5 again → not novel
    }

    #[test]
    fn eviction_drops_lowest_score_and_queues_it() {
        let mut c = Corpus::with_capacity(2);
        // Three distinct novel coverages; the second has the smallest novelty.
        assert!(c.admit(SnapId(1), env(), &[1, 1, 1])); // 3 new pairs
        assert!(c.admit(SnapId(2), env(), &[0, 0, 0, 1])); // 1 new pair (weakest)
        assert!(c.admit(SnapId(3), env(), &[0, 0, 0, 0, 1, 1])); // 2 new pairs → over cap
        assert_eq!(c.len(), 2);
        assert_eq!(c.drain_evicted(), vec![SnapId(2)]);
        assert!(c.drain_evicted().is_empty());
    }

    /// `novelty` is the exact count of new buckets — pins the `+= 1`, the
    /// `b != 0 && !seen` guard, and the non-default return.
    #[test]
    fn novelty_counts_exactly() {
        let c = Corpus::new();
        // Three nonzero edges on a fresh index → 3 (kills -> Default/0, != -> ==,
        // delete `!`, += -> *=, all of which would yield 0).
        assert_eq!(c.novelty(&[1, 1, 1]), CovScore(3));
        // A zero edge must NOT count, so only the one nonzero does (kills && -> ||,
        // which would also count the bucket-0 edge → 2).
        assert_eq!(c.novelty(&[0, 1]), CovScore(1));
        assert_eq!(c.novelty(&[0, 0, 0]), CovScore(0));
    }

    /// `select` returns the addressed entry on a non-empty corpus (kills -> None).
    #[test]
    fn select_returns_the_entry() {
        let mut c = Corpus::new();
        assert!(c.admit(SnapId(7), env(), &[1]));
        let (snap, _) = c.select(0).expect("non-empty corpus selects");
        assert_eq!(snap, SnapId(7));
        assert!(Corpus::new().select(0).is_none());
    }

    /// The AFL bucket classifier, pinned per range — one representative per arm,
    /// so deleting any arm changes a value.
    #[test]
    fn bucket_classifier_is_pinned_per_range() {
        assert_eq!(bucket(0), 0);
        assert_eq!(bucket(1), 1);
        assert_eq!(bucket(2), 2);
        assert_eq!(bucket(3), 3);
        assert_eq!(bucket(5), 4); // 4..=7
        assert_eq!(bucket(10), 5); // 8..=15
        assert_eq!(bucket(20), 6); // 16..=31
        assert_eq!(bucket(64), 7); // 32..=127
        assert_eq!(bucket(200), 8); // 128..
    }

    /// The newest entry is never the eviction victim, even when it is the global
    /// minimum score — pins the `len() - 1` upper bound.
    #[test]
    fn eviction_never_evicts_the_newest_entry() {
        let mut c = Corpus::with_capacity(2);
        assert!(c.admit(SnapId(1), env(), &[1, 1, 1, 1, 1])); // score 5
        assert!(c.admit(SnapId(2), env(), &[0, 0, 0, 0, 0, 1, 1, 1, 1])); // score 4
        // Newest has the *lowest* score; if the search included it (len, not
        // len-1) it would be wrongly evicted.
        assert!(c.admit(SnapId(3), env(), &[0, 0, 0, 0, 0, 0, 0, 0, 0, 1])); // score 1, newest
        assert_eq!(
            c.drain_evicted(),
            vec![SnapId(2)],
            "an older entry is evicted"
        );
        assert!(c.base_env(SnapId(3)).is_some(), "the newest entry is kept");
    }

    /// On a score tie among older entries, the earliest-admitted is evicted — pins
    /// the strict `<` (a `<=` would evict the later one instead).
    #[test]
    fn eviction_breaks_score_ties_by_admission_order() {
        let mut c = Corpus::with_capacity(2);
        assert!(c.admit(SnapId(10), env(), &[1])); // edge 0, score 1
        assert!(c.admit(SnapId(20), env(), &[0, 1])); // edge 1, score 1 (tie)
        assert!(c.admit(SnapId(30), env(), &[0, 0, 1, 1, 1, 1, 1])); // edges 2-6, score 5
        // The tie is between snap 10 (earliest) and snap 20; the earliest goes.
        assert_eq!(c.drain_evicted(), vec![SnapId(10)]);
    }
}
