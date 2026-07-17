// SPDX-License-Identifier: AGPL-3.0-or-later
//! M0 property gates (proptest >= 256 cases), strengthened per the PR #124
//! FAM-COHORT ruling:
//!
//! - **permutation-invariance** — any completion arrival order yields the
//!   identical `drain_ready` sequence and byte-identical consolidated
//!   artifacts (`DrainedView::encode`, `StateProjection::encode`);
//! - **no-frontier-holes** — `drain_ready` never emits a revision with an
//!   unmet predecessor (checked at every drain of every run);
//! - **cohort-atomic visibility** — the probe frontier only ever sits on a
//!   cohort boundary, computed by an INDEPENDENT schedule-position oracle
//!   (prior cohorts' total + this cohort's size iff it is done), never by
//!   mirroring the implementation's per-revision rule;
//! - **frozen views are constants** — cohort k's view equals the total
//!   size of cohorts 1..k at open and never moves afterwards (later
//!   cohorts' opens necessarily interleave with earlier completions under
//!   the barrier, so views are non-zero from the second cohort on);
//! - **the barrier holds** — while the current cohort is not both closed
//!   and fully committed, `open_cohort` refuses with `CohortBarrier`
//!   (probed at every arrival step, not just at scenario edges).

use proptest::prelude::*;
use revision_coordinator::{
    CampaignConfigId, Completion, CoordError, Coordinator, EvidenceBatchId, MemLedger,
    PendingProposal, Revision, TerminalRecord,
};

fn config() -> CampaignConfigId {
    CampaignConfigId::digest(b"tests/permutation")
}

fn batch(rev: u64) -> EvidenceBatchId {
    EvidenceBatchId::digest(&rev.to_le_bytes())
}

fn terminal(rev: u64) -> TerminalRecord {
    TerminalRecord {
        moment: 1_000 + rev,
        work: 10 * rev,
    }
}

fn completion(p: PendingProposal) -> Completion {
    Completion {
        proposal: p.proposal,
        batch: batch(p.revision.get()),
        terminal: terminal(p.revision.get()),
    }
}

/// One generated scenario: sequential cohorts (the barrier's only legal
/// shape), each with a size, a close point interleaved among its own
/// completions, and two within-cohort arrival permutations.
#[derive(Clone, Debug)]
struct Scenario {
    /// Per cohort: (size, close_at in 0..=size, perm_a, perm_b).
    cohorts: Vec<(usize, usize, Vec<usize>, Vec<usize>)>,
}

fn scenario() -> impl Strategy<Value = Scenario> {
    prop::collection::vec(1..=3usize, 1..=4)
        .prop_flat_map(|sizes| {
            let per_cohort: Vec<_> = sizes
                .iter()
                .map(|&size| {
                    (
                        Just(size),
                        0..=size,
                        Just((0..size).collect::<Vec<_>>()).prop_shuffle(),
                        Just((0..size).collect::<Vec<_>>()).prop_shuffle(),
                    )
                })
                .collect();
            per_cohort
        })
        .prop_map(|cohorts| Scenario { cohorts })
}

/// Everything one run produces that must be permutation-invariant.
struct RunArtifacts {
    drained: Vec<(Revision, EvidenceBatchId)>,
    final_view: Vec<u8>,
    final_state: Vec<u8>,
}

/// Drive one full scenario using arrival permutation `a_side ? perm_a :
/// perm_b` per cohort, checking every property oracle at every step.
fn run(s: &Scenario, a_side: bool) -> RunArtifacts {
    let mut c = Coordinator::genesis(Box::new(MemLedger::new()), config()).unwrap();

    let mut drained: Vec<(Revision, EvidenceBatchId)> = Vec::new();
    let mut next_slot = 1u64;
    let mut prior_total = 0u64; // revisions minted by fully-done earlier cohorts

    // The independent oracle: the frontier may only ever be a cohort
    // boundary — `prior_total` while the current cohort is not done,
    // `prior_total + size` the instant it is (closed AND all arrived).
    let check = |c: &mut Coordinator,
                 prior_total: u64,
                 size: usize,
                 done: bool,
                 drained: &mut Vec<(Revision, EvidenceBatchId)>,
                 next_slot: &mut u64| {
        for (rev, b) in c.drain_ready() {
            assert_eq!(rev.get(), *next_slot, "drain_ready emitted past a hole");
            assert_eq!(b, batch(rev.get()));
            *next_slot += 1;
            drained.push((rev, b));
        }
        let expected = prior_total + if done { size as u64 } else { 0 };
        let view = c.probe_drive(Revision::ZERO).unwrap();
        assert_eq!(
            view.frontier,
            Revision::new(expected),
            "frontier off the cohort boundary (cohort-atomicity violated)"
        );
        let expected_rows: Vec<(Revision, EvidenceBatchId)> = (1..=expected)
            .map(|r| (Revision::new(r), batch(r)))
            .collect();
        assert_eq!(view.rows, expected_rows, "partial-cohort result leaked");
    };

    for (size, close_at, perm_a, perm_b) in &s.cohorts {
        let (size, close_at) = (*size, *close_at);
        let perm = if a_side { perm_a } else { perm_b };

        let id = c.open_cohort().unwrap();
        // Frozen view = everything before this cohort, always.
        assert_eq!(c.cohort_view(id), Some(Revision::new(prior_total)));

        let pendings: Vec<PendingProposal> = (0..size).map(|_| c.assign(id).unwrap()).collect();
        let mut closed = close_at == 0;
        if closed {
            c.close_cohort(id).unwrap();
        }
        check(
            &mut c,
            prior_total,
            size,
            closed && size == 0,
            &mut drained,
            &mut next_slot,
        );

        for (t, &pi) in perm.iter().enumerate() {
            c.complete(completion(pendings[pi])).unwrap();
            if !closed && close_at == t + 1 {
                c.close_cohort(id).unwrap();
                closed = true;
            }
            let done = closed && t + 1 == size;
            check(
                &mut c,
                prior_total,
                size,
                done,
                &mut drained,
                &mut next_slot,
            );
            // The barrier: no new cohort may open until this one is done.
            if !done {
                assert!(
                    matches!(
                        c.open_cohort(),
                        Err(CoordError::CohortBarrier { blocking }) if blocking == id
                    ),
                    "open_cohort crossed the barrier"
                );
            }
            // The frozen view never moves.
            assert_eq!(c.cohort_view(id), Some(Revision::new(prior_total)));
        }
        if !closed {
            // close_at == size arrives here only when size == 0 (handled
            // above); for non-empty cohorts every close point <= size was
            // consumed in the loop. Defensive:
            c.close_cohort(id).unwrap();
        }
        prior_total += size as u64;
        check(&mut c, prior_total, 0, false, &mut drained, &mut next_slot);
    }

    let final_view = c.probe_drive(Revision::new(prior_total)).unwrap();
    assert_eq!(final_view.frontier, Revision::new(prior_total));
    RunArtifacts {
        drained,
        final_view: final_view.encode(),
        final_state: c.state_projection().encode(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Two arbitrary completion-arrival permutations of the same scenario
    /// produce the identical drain sequence and byte-identical artifacts.
    #[test]
    fn permutation_invariance(s in scenario()) {
        let a = run(&s, true);
        let b = run(&s, false);
        prop_assert_eq!(&a.drained, &b.drained);
        prop_assert_eq!(&a.final_view, &b.final_view);
        prop_assert_eq!(&a.final_state, &b.final_state);
    }
}
