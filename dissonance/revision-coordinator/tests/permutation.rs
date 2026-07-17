// SPDX-License-Identifier: AGPL-3.0-or-later
//! M0 property gates (proptest >= 256 cases):
//!
//! - **permutation-invariance** — any completion arrival order yields the
//!   identical `drain_ready` sequence and byte-identical consolidated
//!   artifacts (`DrainedView::encode`, `StateProjection::encode`);
//! - **no-frontier-holes** — `drain_ready` never emits a revision with an
//!   unmet predecessor (checked at every drain of every run);
//! - **cohort-freeze** — no partial-cohort result is observable at any
//!   step: every probe view is exactly the closed, fully-committed prefix
//!   the reference model predicts.
//!
//! The scenario space interleaves proposal minting across up to four
//! cohorts and interleaves cohort *closing* with completion arrival, so
//! visibility is genuinely gated on both closure and full commitment.

use proptest::prelude::*;
use revision_coordinator::{
    CampaignConfigId, CohortId, Completion, Coordinator, EvidenceBatchId, MemLedger,
    PendingProposal, Revision, TerminalRecord,
};
use std::collections::BTreeSet;

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

/// One generated scenario: a mint schedule over cohort indices, two
/// completion-arrival permutations of the minted proposals, and a
/// close-point per cohort (after how many completions it closes).
#[derive(Clone, Debug)]
struct Scenario {
    mint: Vec<usize>,
    perm_a: Vec<usize>,
    perm_b: Vec<usize>,
    close_at: Vec<usize>,
}

fn scenario() -> impl Strategy<Value = Scenario> {
    prop::collection::vec(1..=3usize, 1..=4)
        .prop_flat_map(|sizes| {
            let mut mint: Vec<usize> = Vec::new();
            for (cohort, &size) in sizes.iter().enumerate() {
                mint.extend(std::iter::repeat_n(cohort, size));
            }
            let n = mint.len();
            let cohorts = sizes.len();
            (
                Just(mint).prop_shuffle(),
                Just((0..n).collect::<Vec<_>>()).prop_shuffle(),
                Just((0..n).collect::<Vec<_>>()).prop_shuffle(),
                prop::collection::vec(0..=n, cohorts),
            )
        })
        .prop_map(|(mint, perm_a, perm_b, close_at)| Scenario {
            mint,
            perm_a,
            perm_b,
            close_at,
        })
}

/// Everything one run produces that must be permutation-invariant.
struct RunArtifacts {
    drained: Vec<(Revision, EvidenceBatchId)>,
    final_view: Vec<u8>,
    final_state: Vec<u8>,
}

/// Drive one full scenario with the given completion order, checking the
/// no-frontier-holes and cohort-freeze properties at every step against a
/// pure reference model.
fn run(s: &Scenario, order: &[usize]) -> RunArtifacts {
    let mut c = Coordinator::genesis(Box::new(MemLedger::new()), config()).unwrap();

    // Mint phase: open each cohort at its first mint, in schedule order.
    let cohort_count = s.close_at.len();
    let mut ids: Vec<Option<CohortId>> = vec![None; cohort_count];
    let mut pendings: Vec<PendingProposal> = Vec::new();
    for &ci in &s.mint {
        let id = match ids[ci] {
            Some(id) => id,
            None => {
                let id = c.open_cohort().unwrap();
                ids[ci] = Some(id);
                id
            }
        };
        pendings.push(c.assign(id).unwrap());
    }
    let n = pendings.len();

    // Reference model state.
    let mut completed: BTreeSet<usize> = BTreeSet::new();
    let mut closed: Vec<bool> = vec![false; cohort_count];
    let mut drained: Vec<(Revision, EvidenceBatchId)> = Vec::new();
    let mut next_slot = 1u64;

    // Cohort members by scenario index (mint order == revision order).
    let members: Vec<Vec<usize>> = (0..cohort_count)
        .map(|ci| (0..n).filter(|&i| s.mint[i] == ci).collect())
        .collect();

    let mut step = |c: &mut Coordinator,
                    completed: &BTreeSet<usize>,
                    closed: &[bool],
                    drained: &mut Vec<(Revision, EvidenceBatchId)>,
                    next_slot: &mut u64| {
        // No-frontier-holes: drains are exactly contiguous slots.
        for (rev, b) in c.drain_ready() {
            assert_eq!(rev.get(), *next_slot, "drain_ready emitted past a hole");
            assert_eq!(b, batch(rev.get()));
            *next_slot += 1;
            drained.push((rev, b));
        }
        // Cohort-freeze: the model's visible frontier is the largest prefix
        // of revisions whose cohorts are closed and fully committed; the
        // probe view must be exactly that prefix.
        let mut expected_visible = 0usize;
        for i in 0..n {
            let ci = s.mint[i];
            let done = closed[ci] && members[ci].iter().all(|m| completed.contains(m));
            if done {
                expected_visible = i + 1;
            } else {
                break;
            }
        }
        let view = c.probe_drive(Revision::ZERO).unwrap();
        assert_eq!(view.frontier, Revision::new(expected_visible as u64));
        let expected_rows: Vec<(Revision, EvidenceBatchId)> = (1..=expected_visible as u64)
            .map(|r| (Revision::new(r), batch(r)))
            .collect();
        assert_eq!(view.rows, expected_rows, "partial-cohort result leaked");
    };

    // Close-at-0 cohorts close before any completion arrives.
    for ci in 0..cohort_count {
        if s.close_at[ci] == 0 && ids[ci].is_some() {
            c.close_cohort(ids[ci].unwrap()).unwrap();
            closed[ci] = true;
        }
    }
    step(&mut c, &completed, &closed, &mut drained, &mut next_slot);

    for (t, &pi) in order.iter().enumerate() {
        c.complete(completion(pendings[pi])).unwrap();
        completed.insert(pi);
        for ci in 0..cohort_count {
            if !closed[ci] && s.close_at[ci] == t + 1 && ids[ci].is_some() {
                c.close_cohort(ids[ci].unwrap()).unwrap();
                closed[ci] = true;
            }
        }
        step(&mut c, &completed, &closed, &mut drained, &mut next_slot);
    }

    // Close any cohort whose close point exceeded the run length.
    for ci in 0..cohort_count {
        if !closed[ci] && ids[ci].is_some() {
            c.close_cohort(ids[ci].unwrap()).unwrap();
            closed[ci] = true;
        }
    }
    step(&mut c, &completed, &closed, &mut drained, &mut next_slot);

    let final_view = c.probe_drive(Revision::new(n as u64)).unwrap();
    assert_eq!(final_view.frontier, Revision::new(n as u64));
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
        let a = run(&s, &s.perm_a);
        let b = run(&s, &s.perm_b);
        prop_assert_eq!(&a.drained, &b.drained);
        prop_assert_eq!(&a.final_view, &b.final_view);
        prop_assert_eq!(&a.final_state, &b.final_state);
    }
}
