// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 4 — novelty scoring.
//!
//! `CoverageArchive::admit` admits exactly when a fork's coverage view claims a
//! cell the frontier has not seen, and admission is **order-stable**: the kept
//! frontier is a deterministic function of the admitted sequence, with no
//! `HashMap` order reaching it. (The exact fresh-cell rule is also unit tested
//! in `src/defaults.rs`; here we gate the integration-level invariants over the
//! archive as a fold.)

mod common;

use common::config;
use explorer::{
    Archive, CoverageArchive, CoverageView, EvidenceCut, Fork, IdentityCells, Moment, Reproducer,
    RunTrace, SnapId, StopReason, VirtualExemplar,
};
use proptest::prelude::*;

fn env() -> Reproducer {
    Reproducer {
        blob_version: 1,
        bytes: vec![],
    }
}

fn fork(at: u64, coverage: &[u8]) -> Fork {
    Fork {
        exemplar: VirtualExemplar {
            parent: SnapId(1),
            seed: 0,
            suffix: env(),
            cut: EvidenceCut {
                at: Moment(at),
                sdk_events: 0,
            },
        },
        env: env(),
        coverage: Some(CoverageView {
            map: coverage.to_vec(),
        }),
    }
}

fn trace() -> RunTrace {
    RunTrace {
        terminal: StopReason::Quiescent { vtime: Moment(80) },
        env: env(),
        coverage: None,
        events: vec![],
        records: vec![],
    }
}

/// Fold a coverage sequence into a fresh archive; return the kept frontier as
/// comparable bytes (per-entry moment + reward).
fn kept_after(seq: &[(u64, Vec<u8>)]) -> Vec<(u64, u64)> {
    let mut a = CoverageArchive::new();
    for (at, cov) in seq {
        a.admit(&trace(), &[fork(*at, cov)], &IdentityCells, &[]);
    }
    a.frontier()
        .iter()
        .map(|(_, e)| (e.exemplar.cut.at.0, e.reward.new_cells))
        .collect()
}

/// The archive admits exactly on a fresh cell (integration-level witness of the
/// unit-tested rule).
#[test]
fn admits_exactly_on_a_fresh_cell() {
    let mut a = CoverageArchive::new();
    let admit = |a: &mut CoverageArchive, cov: &[u8]| {
        a.admit(&trace(), &[fork(40, cov)], &IdentityCells, &[])
            .new_cells
    };
    // First non-zero map is always novel.
    assert!(admit(&mut a, &[0, 1, 0, 0]) > 0);
    // A strict subset of seen cells is not novel.
    assert_eq!(admit(&mut a, &[0, 1, 0, 0]), 0);
    assert_eq!(admit(&mut a, &[0, 0, 0, 0]), 0);
    // A new edge index is novel; a higher bucket on a seen edge is novel.
    assert!(admit(&mut a, &[0, 1, 0, 1]) > 0);
    assert!(admit(&mut a, &[0, 9, 0, 1]) > 0); // edge 1 jumps to bucket 5
    // Same cells again → not novel.
    assert_eq!(admit(&mut a, &[0, 9, 0, 1]), 0);
}

proptest! {
    #![proptest_config(config(256))]

    /// The kept frontier is a pure function of the admitted sequence.
    #[test]
    fn admission_is_a_pure_function_of_the_sequence(
        seq in prop::collection::vec(
            (1u64..100, prop::collection::vec(any::<u8>(), 1..16)),
            1..30,
        ),
    ) {
        prop_assert_eq!(kept_after(&seq), kept_after(&seq));
    }

    /// Whatever the admit *order*, the accumulated cell set covers exactly the
    /// union of all cells seen — so the final "is X novel?" answer does not
    /// depend on iteration order. After folding a whole sequence in,
    /// re-offering any member is never novel.
    #[test]
    fn nothing_already_admitted_remains_novel(
        seq in prop::collection::vec(
            prop::collection::vec(any::<u8>(), 1..16),
            1..20,
        ),
    ) {
        let mut a = CoverageArchive::new();
        for cov in &seq {
            a.admit(&trace(), &[fork(40, cov)], &IdentityCells, &[]);
        }
        for cov in &seq {
            let r = a.admit(&trace(), &[fork(40, cov)], &IdentityCells, &[]);
            prop_assert_eq!(r.new_cells, 0);
        }
    }
}
