// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 4 — novelty scoring.
//!
//! `Corpus::admit` returns true exactly when the coverage map shows a new
//! edge/bucket versus the accumulated set, and admission is **order-stable**: the
//! kept set is a deterministic function of the admitted sequence, with no
//! `HashMap` order reaching it. (The exact "new edge/bucket" rule is also unit
//! tested in `src/corpus.rs`; here we gate the integration-level invariants.)

mod common;

use common::config;
use explorer::{Corpus, Environment, SnapId};
use proptest::prelude::*;

fn env() -> Environment {
    Environment {
        blob_version: 1,
        bytes: vec![],
    }
}

/// `admit` is true iff the coverage introduces a bucket the index has not seen.
#[test]
fn admits_exactly_on_a_new_bucket() {
    let mut c = Corpus::new();
    // First non-zero map is always novel.
    assert!(c.admit(SnapId(1), env(), &[0, 1, 0, 0]));
    // A strict subset of seen buckets is not novel.
    assert!(!c.admit(SnapId(2), env(), &[0, 1, 0, 0]));
    assert!(!c.admit(SnapId(3), env(), &[0, 0, 0, 0]));
    // A new edge index is novel; a higher bucket on a seen edge is novel.
    assert!(c.admit(SnapId(4), env(), &[0, 1, 0, 1]));
    assert!(c.admit(SnapId(5), env(), &[0, 9, 0, 1])); // edge 1 jumps to bucket 5
    // Same buckets again → not novel.
    assert!(!c.admit(SnapId(6), env(), &[0, 9, 0, 1]));
}

/// Replaying the same admit sequence yields a byte-identical kept set, regardless
/// of run — the novelty index is deterministic (no map-order dependence).
fn kept_after(seq: &[(u64, Vec<u8>)]) -> Vec<(SnapId, Vec<u8>)> {
    let mut c = Corpus::new();
    for (id, cov) in seq {
        c.admit(SnapId(*id), env(), cov);
    }
    (0..c.len())
        .map(|i| {
            let (snap, _, score) = c.entry(i).unwrap();
            (snap, score.0.to_le_bytes().to_vec())
        })
        .collect()
}

proptest! {
    #![proptest_config(config(256))]

    /// The kept set is a pure function of the admitted sequence.
    #[test]
    fn admission_is_a_pure_function_of_the_sequence(
        seq in prop::collection::vec(
            (any::<u64>(), prop::collection::vec(any::<u8>(), 1..16)),
            1..30,
        ),
    ) {
        prop_assert_eq!(kept_after(&seq), kept_after(&seq));
    }

    /// Whatever the admit *order*, the accumulated novelty set covers exactly the
    /// union of all buckets seen — so the final "is X novel?" answer does not
    /// depend on iteration order. We check the order-independence of the *set of
    /// seen buckets* by confirming that, after admitting a sequence, no member of
    /// the sequence is still considered novel.
    #[test]
    fn nothing_already_admitted_remains_novel(
        seq in prop::collection::vec(
            prop::collection::vec(any::<u8>(), 1..16),
            1..20,
        ),
    ) {
        let mut c = Corpus::new();
        for cov in &seq {
            c.admit(SnapId(0), env(), cov);
        }
        // After folding the whole sequence in, re-admitting any of them is never
        // novel (every bucket they carry is already in the index).
        for cov in &seq {
            prop_assert!(!c.admit(SnapId(0), env(), cov));
        }
    }
}
