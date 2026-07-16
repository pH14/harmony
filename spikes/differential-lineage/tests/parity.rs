// SPDX-License-Identifier: AGPL-3.0-or-later
//! The parity adjudicator: every dataflow view must equal the plain-Rust
//! genesis-replay referee — on the hand fixtures, on seeded random branch
//! trees, at every intermediate revision (the probe/two-pass staging
//! discipline), across reruns (determinism), and under permuted input feed
//! order within revisions.

use differential_lineage::data::{Cut, Fixture, OrderScope, Payload, ReduceOp, Replay, Revision};
use differential_lineage::dataflow::{BuildOpts, Captured, run};
use differential_lineage::fixtures;
use differential_lineage::generate::Builder;
use differential_lineage::generate::{TreeParams, random_tree};
use differential_lineage::referee::Referee;

/// Compare every captured view against the referee as of `rev`.
fn compare_all(fx: &Fixture, replay: &Replay, cap: &Captured, rev: Revision) {
    let referee = Referee::new(fx, replay).expect("valid fixture");
    let ctx = |view: &str| format!("{} @ rev {rev}: {view}", fx.name);

    assert_eq!(
        Captured::flat(&cap.obs_naive, rev),
        referee.obs(rev),
        "{}",
        ctx("obs (naive)")
    );
    assert_eq!(
        Captured::flat(&cap.obs_shared, rev),
        referee.obs(rev),
        "{}",
        ctx("obs (shared)")
    );
    assert_eq!(
        Captured::flat(&cap.seal_prefix, rev),
        referee.seal_prefix(rev),
        "{}",
        ctx("seal_prefix")
    );
    assert_eq!(
        Captured::flat(&cap.cells, rev),
        referee.cells(rev),
        "{}",
        ctx("cells")
    );
    assert_eq!(
        Captured::flat(&cap.transitions, rev),
        referee.transitions(rev),
        "{}",
        ctx("transitions")
    );
    assert_eq!(
        Captured::flat(&cap.occupancy, rev),
        referee.occupancy(rev),
        "{}",
        ctx("occupancy")
    );
    assert_eq!(
        Captured::flat(&cap.property_results, rev),
        referee.property_results(rev),
        "{}",
        ctx("property_results")
    );
    assert_eq!(
        Captured::flat(&cap.site_coverage, rev),
        referee.site_coverage(rev),
        "{}",
        ctx("site_coverage")
    );
    assert_eq!(
        Captured::flat(&cap.absence, rev),
        referee.absence(rev),
        "{}",
        ctx("absence")
    );
    assert_eq!(
        Captured::flat(&cap.working_species, rev),
        referee.working_species(rev),
        "{}",
        ctx("working_species")
    );
    assert_eq!(
        Captured::flat(&cap.seq_pairs, rev),
        referee.seq_pairs(rev),
        "{}",
        ctx("seq_pairs")
    );
    assert_eq!(
        Captured::flat(&cap.seq_rejections, rev),
        referee.seq_rejections(rev),
        "{}",
        ctx("seq_rejections")
    );
    assert_eq!(
        Captured::flat(&cap.scrape_terminal, rev),
        referee.scrape_terminal(rev),
        "{}",
        ctx("scrape_terminal")
    );
}

fn hand_fixtures() -> Vec<(Fixture, Replay)> {
    vec![
        fixtures::tree_lineage(),
        fixtures::two_pass(),
        fixtures::retention_properties(),
    ]
}

#[test]
fn hand_fixtures_parity_final() {
    for (fx, replay) in hand_fixtures() {
        let cap = run(&fx, BuildOpts::default(), 1).expect("valid fixture");
        compare_all(&fx, &replay, &cap, fx.max_rev());
    }
}

#[test]
fn hand_fixtures_parity_every_revision() {
    // The staged-read discipline: after the probe passes revision r, every
    // view equals the referee at r — including the two-pass staging (fixture
    // `two_pass`: transitions live at rev 2, occupancy empty until commits at
    // rev 4, domination flip at rev 5).
    for (fx, replay) in hand_fixtures() {
        let cap = run(&fx, BuildOpts::default(), 7).expect("valid fixture");
        for rev in 0..=fx.max_rev() {
            compare_all(&fx, &replay, &cap, rev);
        }
    }
}

#[test]
fn random_trees_parity() {
    let params = TreeParams {
        rollouts: 8,
        max_events: 24,
        registers: 4,
        tags: 2,
        cuts_per_rollout: 2,
        seals_per_rollout: 2,
    };
    for seed in 0..24u64 {
        let (fx, replay) = random_tree(&format!("random-{seed}"), seed, params);
        let cap = run(&fx, BuildOpts::default(), seed.wrapping_mul(31) + 5).expect("valid fixture");
        compare_all(&fx, &replay, &cap, fx.max_rev());
    }
}

#[test]
fn random_trees_parity_staged() {
    // A couple of seeds checked at every revision, not just the final one.
    let params = TreeParams {
        rollouts: 6,
        max_events: 16,
        registers: 4,
        tags: 2,
        cuts_per_rollout: 2,
        seals_per_rollout: 1,
    };
    for seed in [3u64, 11] {
        let (fx, replay) = random_tree(&format!("staged-{seed}"), seed, params);
        let cap = run(&fx, BuildOpts::default(), 9).expect("valid fixture");
        for rev in 0..=fx.max_rev() {
            compare_all(&fx, &replay, &cap, rev);
        }
    }
}

#[test]
fn reruns_are_deterministic() {
    let (fx, _) = fixtures::tree_lineage();
    let a = run(&fx, BuildOpts::default(), 42).expect("valid fixture");
    let b = run(&fx, BuildOpts::default(), 42).expect("valid fixture");
    // Identical raw update streams, not merely identical net views.
    assert_eq!(a.seal_prefix, b.seal_prefix);
    assert_eq!(a.obs_naive, b.obs_naive);
    assert_eq!(a.obs_shared, b.obs_shared);
    assert_eq!(a.cells, b.cells);
    assert_eq!(a.transitions, b.transitions);
    assert_eq!(a.occupancy, b.occupancy);
    assert_eq!(a.deltas, b.deltas);
}

#[test]
fn input_permutation_invariance() {
    // Different within-revision feed orders must produce identical net views
    // at every revision (the multiset is the contract, not arrival order).
    let (fx, replay) = fixtures::tree_lineage();
    let caps: Vec<Captured> = [1u64, 2, 3]
        .iter()
        .map(|s| run(&fx, BuildOpts::default(), *s).expect("valid fixture"))
        .collect();
    for rev in 0..=fx.max_rev() {
        for cap in &caps {
            compare_all(&fx, &replay, cap, rev);
        }
    }
    let (fx, replay) = fixtures::retention_properties();
    for seed in [4u64, 5] {
        let cap = run(&fx, BuildOpts::default(), seed).expect("valid fixture");
        for rev in 0..=fx.max_rev() {
            compare_all(&fx, &replay, &cap, rev);
        }
    }
}

#[test]
fn single_formulation_builds_agree() {
    // The benchmark isolates formulations; both isolated builds must still
    // agree with the referee.
    let (fx, replay) = fixtures::tree_lineage();
    let referee = Referee::new(&fx, &replay).expect("valid fixture");
    let rev = fx.max_rev();

    let naive_only = run(
        &fx,
        BuildOpts {
            naive: true,
            shared: false,
            prefix: false,
        },
        1,
    )
    .expect("valid fixture");
    assert_eq!(Captured::flat(&naive_only.obs_naive, rev), referee.obs(rev));
    assert!(naive_only.obs_shared.is_empty());

    let shared_only = run(
        &fx,
        BuildOpts {
            naive: false,
            shared: true,
            prefix: false,
        },
        1,
    )
    .expect("valid fixture");
    assert_eq!(
        Captured::flat(&shared_only.obs_shared, rev),
        referee.obs(rev)
    );
    assert!(shared_only.obs_naive.is_empty());
    // Cells ride whichever formulation is built; both must match the referee.
    assert_eq!(Captured::flat(&naive_only.cells, rev), referee.cells(rev));
    assert_eq!(Captured::flat(&shared_only.cells, rev), referee.cells(rev));
}

#[test]
fn late_covered_evidence_staged_parity() {
    // r2: an event inside a seal's cut range that commits AFTER the seal's
    // own revision. Production's durable-append-before-submit rule forbids
    // this ordering, but a fixture may express it — and staged parity must
    // still hold: DD has not ingested the event at the seal's revision, and
    // the referee's replay-backed views are revision-filtered to match. The
    // event enters both sides exactly when it commits.
    let mut b = Builder::new("late-evidence", 0);
    b.reg(1, 10, ReduceOp::Set)
        .source(1, 0, OrderScope::RolloutGlobal);
    let g = b.genesis();
    b.push(2, g, 0, 10, Payload::Register { reg: 10, value: 1 });
    b.push(2, g, 0, 20, Payload::Register { reg: 10, value: 5 });
    b.push(4, g, 0, 30, Payload::Register { reg: 10, value: 7 }); // pos 2: commits after the seal
    b.push(5, g, 0, 40, Payload::Register { reg: 10, value: 9 }); // pos 3: later still
    b.seal(
        3,
        g,
        0,
        Cut {
            moment: 40,
            count: 4,
        },
    ); // covers pos 0..4, two of them not yet committed at rev 3
    let (fx, replay) = b.finish();
    assert_eq!(fx.validate(), Ok(()));

    let cap = run(&fx, BuildOpts::default(), 3).expect("valid fixture");
    for rev in 0..=fx.max_rev() {
        compare_all(&fx, &replay, &cap, rev);
    }
    // The staged content is what it should be: two events at the seal's
    // revision, then the late ones as they commit.
    let referee = Referee::new(&fx, &replay).expect("valid fixture");
    assert_eq!(referee.seal_prefix(3).len(), 2);
    assert_eq!(referee.seal_prefix(4).len(), 3);
    assert_eq!(referee.seal_prefix(5).len(), 4);
}
