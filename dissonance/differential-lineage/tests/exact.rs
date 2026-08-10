// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fixture-backed tests with exact, hand-written expected outputs — one per
//! query family (task gate). These load the *committed* JSON fixtures,
//! assert the committed bytes still equal the generator's output, run the
//! dataflow, and compare against constants derived by hand from the fixture
//! definitions (independently of the referee).

use differential_lineage::data::{
    CellKey, Dim, Fixture, ObsOut, Payload, PointId, Pos, PrefixEv, ReduceOp, Revision, RolloutId,
    SealId, Transition,
};
use differential_lineage::dataflow::{BuildOpts, Captured, run};
use differential_lineage::fixtures;

fn load(name: &str, committed: &str, built: &Fixture) -> Fixture {
    let loaded: Fixture = serde_json::from_str(committed).expect("parse committed fixture");
    assert_eq!(
        &loaded, built,
        "committed fixture {name} != generator output"
    );
    let mut rebuilt = serde_json::to_string_pretty(built).expect("serialize");
    rebuilt.push('\n');
    assert_eq!(committed, rebuilt, "committed fixture {name} bytes drifted");
    loaded
}

fn tree() -> (Fixture, Captured) {
    let (built, _replay) = fixtures::tree_lineage();
    let fx = load(
        "tree_lineage",
        include_str!("../fixtures/tree_lineage.json"),
        &built,
    );
    let cap = run(&fx, BuildOpts::default(), 1).expect("valid fixture");
    (fx, cap)
}

/// Observations at one evaluation point, canonically ordered by dimension.
fn obs_at(cap: &Captured, rev: Revision, rollout: RolloutId, point: PointId) -> Vec<(Dim, ObsOut)> {
    Captured::flat(&cap.obs_shared, rev)
        .into_iter()
        .filter(|((_c, r, p, _d), _)| *r == rollout && *p == point)
        .map(|((_c, _r, _p, d), out)| (d, out))
        .collect()
}

/// The lineage-complete prefix of one seal, in READER order — no re-sort
/// here (r6): `Captured::flat` orders rows by the row type's `Ord`, and
/// `PrefixEv` declares `(moment, pos)` first, so the reader itself
/// reconstructs the canonical sequence.
fn prefix_of(cap: &Captured, rev: Revision, seal: SealId) -> Vec<PrefixEv> {
    Captured::flat(&cap.seal_prefix, rev)
        .into_iter()
        .filter(|((_c, _r, s), _)| *s == seal)
        .map(|(_, ev)| ev)
        .collect()
}

fn ev(owner: RolloutId, source: u32, pos: Pos, moment: u64, payload: Payload) -> PrefixEv {
    PrefixEv {
        owner,
        source,
        pos,
        moment,
        payload,
    }
}

fn reg(reg: u32, value: i64) -> Payload {
    Payload::Register { reg, value }
}

// Hand-derived cells for the tree fixture.
fn cell_a_cut2() -> CellKey {
    vec![(0, 10, 1), (0, 11, 5)]
}
fn cell_a_cut7() -> CellKey {
    vec![(0, 10, 2), (0, 11, 5), (0, 12, 9), (0, 13, 1), (1, 7, 1)]
}
fn cell_a_fork4() -> CellKey {
    vec![(0, 10, 2), (0, 11, 5), (1, 7, 1)]
}
fn cell_b_cut5() -> CellKey {
    vec![(0, 10, 7), (0, 11, 5), (1, 7, 1)]
}
fn cell_b_cut8() -> CellKey {
    vec![(0, 10, 7), (0, 11, 5), (0, 12, 1), (1, 7, 2)]
}
fn cell_b_seal() -> CellKey {
    vec![(0, 10, 7), (0, 11, 5), (0, 12, 1), (0, 13, 1), (1, 7, 2)]
}
fn cell_c_cut5() -> CellKey {
    vec![(0, 10, 100), (0, 11, 5), (1, 7, 1)]
}
fn cell_c_seal() -> CellKey {
    vec![(0, 10, 100), (0, 11, 5), (0, 13, 1), (1, 7, 1)]
}
fn cell_d_cut10() -> CellKey {
    vec![(0, 10, 3), (0, 11, 5), (0, 12, 1), (0, 13, 1), (1, 7, 2)]
}
fn cell_d_seal() -> CellKey {
    vec![(0, 10, 3), (0, 11, 5), (0, 12, 1), (1, 7, 2)]
}

/// Family 1 — lineage-complete observation prefixes at candidate seals:
/// exact composition of ancestor segments (through their fork cuts) plus the
/// child's suffix, with ancestor evidence identity preserved.
#[test]
fn family1_lineage_complete_seal_prefixes() {
    let (fx, cap) = tree();
    let rev = fx.max_rev();

    // Seal S1 on B at cut (60, 9): A's segment through the fork cut (pos < 4)
    // plus B's whole suffix (pos 4..=8).
    assert_eq!(
        prefix_of(&cap, rev, 1),
        vec![
            ev(0, 0, 0, 10, reg(10, 1)),
            ev(0, 0, 1, 20, reg(11, 5)),
            ev(0, 1, 2, 30, Payload::Note { tag: 7 }),
            ev(0, 0, 3, 30, reg(10, 2)),
            ev(1, 0, 4, 35, reg(10, 7)),
            ev(1, 1, 5, 35, Payload::Note { tag: 7 }),
            ev(1, 0, 6, 44, reg(11, 2)),
            ev(1, 0, 7, 44, reg(12, 1)),
            ev(1, 0, 8, 60, reg(13, 8)),
        ]
    );

    // Seal S3 on D at cut (61, 9): three segments — A through 4, B through 8,
    // D's own pos 8 — composed exactly once each.
    assert_eq!(
        prefix_of(&cap, rev, 3),
        vec![
            ev(0, 0, 0, 10, reg(10, 1)),
            ev(0, 0, 1, 20, reg(11, 5)),
            ev(0, 1, 2, 30, Payload::Note { tag: 7 }),
            ev(0, 0, 3, 30, reg(10, 2)),
            ev(1, 0, 4, 35, reg(10, 7)),
            ev(1, 1, 5, 35, Payload::Note { tag: 7 }),
            ev(1, 0, 6, 44, reg(11, 2)),
            ev(1, 0, 7, 44, reg(12, 1)),
            ev(3, 0, 8, 61, reg(10, 3)),
        ]
    );
}

/// Family 4 — same-`Moment` cuts are half-open on the vector position: the
/// exact subset emitted at the cut's `Moment` with positions below the count
/// is included; positions at or past the count are excluded, even at the
/// same `Moment`. Boundary events are neither duplicated nor dropped.
#[test]
fn family4_same_moment_half_open_cuts() {
    let (fx, cap) = tree();
    let rev = fx.max_rev();

    // Seal S0 on A at cut (30, 4): moment-30 events at pos 2 and 3 are
    // inside; the moment-30 event at pos 4 is outside.
    assert_eq!(
        prefix_of(&cap, rev, 0),
        vec![
            ev(0, 0, 0, 10, reg(10, 1)),
            ev(0, 0, 1, 20, reg(11, 5)),
            ev(0, 1, 2, 30, Payload::Note { tag: 7 }),
            ev(0, 0, 3, 30, reg(10, 2)),
        ]
    );

    // Seal S3 on D at cut (61, 9): D's moment-61 event at pos 8 is inside,
    // its moment-61 sibling at pos 9 is outside — visible in the reductions:
    // reg 13 (accumulate, updated only at pos 9) is ABSENT at the seal, and
    // reg 10 is the pos-8 value.
    let obs = obs_at(&cap, rev, 3, PointId::Seal(3));
    assert_eq!(
        obs,
        vec![
            (Dim::Reg(10, ReduceOp::Set), ObsOut::Scalar(3)),
            (Dim::Reg(11, ReduceOp::Max), ObsOut::Scalar(5)),
            (Dim::Reg(12, ReduceOp::Min), ObsOut::Scalar(1)),
            (
                Dim::Tag(7),
                ObsOut::Hist {
                    count: 2,
                    ever: true,
                    latest: (35, 5)
                }
            ),
        ]
    );

    // The ancestor boundary is half-open the same way: A's moment-30 event at
    // pos 4 (reg 12 = 9) sits at the fork count and belongs to no child
    // prefix, while B's own reg-12 update (pos 7) does — so D inherits
    // min = 1, never 9. Meanwhile A's own Cut(7) still sees 9.
    let a7 = obs_at(&cap, rev, 0, PointId::Cut(7));
    assert!(a7.contains(&(Dim::Reg(12, ReduceOp::Min), ObsOut::Scalar(9))));
}

/// Family 3 — sibling-safe rollout identity: B and C fork from the same
/// parent at the same cut; their own suffixes reuse the same vector
/// positions, and nothing collides or leaks across siblings.
#[test]
fn family3_sibling_safe_identity() {
    let (fx, cap) = tree();
    let rev = fx.max_rev();

    // Same coordinate (pos 4, moment 35), different owners, different values.
    let s1 = prefix_of(&cap, rev, 1);
    let s2 = prefix_of(&cap, rev, 2);
    assert!(s1.contains(&ev(1, 0, 4, 35, reg(10, 7))));
    assert!(s2.contains(&ev(2, 0, 4, 35, reg(10, 100))));
    // Identical inherited prefixes from A (owner 0), disjoint own suffixes.
    let inherited = |rows: &[PrefixEv]| -> Vec<PrefixEv> {
        rows.iter().filter(|e| e.owner == 0).cloned().collect()
    };
    assert_eq!(inherited(&s1), inherited(&s2));
    assert!(s1.iter().all(|e| e.owner == 0 || e.owner == 1));
    assert!(s2.iter().all(|e| e.owner == 0 || e.owner == 2));

    // Observations at the same point coordinate (count 5) differ per sibling.
    let b5 = obs_at(&cap, rev, 1, PointId::Cut(5));
    let c5 = obs_at(&cap, rev, 2, PointId::Cut(5));
    assert!(b5.contains(&(Dim::Reg(10, ReduceOp::Set), ObsOut::Scalar(7))));
    assert!(c5.contains(&(Dim::Reg(10, ReduceOp::Set), ObsOut::Scalar(100))));
}

/// Family 5 — canonical order reconstruction: the runtime retains a
/// multiset; ordered views are rebuilt by sorting on the explicit evidence
/// coordinates `(Moment, pos)`, with the persisted vector position deciding
/// same-`Moment` order.
#[test]
fn family5_canonical_order_reconstruction() {
    let (fx, cap) = tree();
    let rev = fx.max_rev();
    let coords: Vec<(u64, Pos)> = prefix_of(&cap, rev, 1)
        .iter()
        .map(|e| (e.moment, e.pos))
        .collect();
    assert_eq!(
        coords,
        vec![
            (10, 0),
            (20, 1),
            (30, 2),
            (30, 3),
            (35, 4),
            (35, 5),
            (44, 6),
            (44, 7),
            (60, 8)
        ]
    );
    // Same-moment pairs (30,2)/(30,3), (35,4)/(35,5), (44,6)/(44,7) are
    // ordered by the contractual vector position, and equal payloads at
    // different coordinates (the two tag-7 notes) stayed distinct records.
}

/// Family 6 — `set`/`max`/`min`/`accumulate` and history derivations, exact
/// at every evaluation point of the A rollout plus the deep D points.
#[test]
fn family6_reductions_and_history() {
    let (fx, cap) = tree();
    let rev = fx.max_rev();

    assert_eq!(
        obs_at(&cap, rev, 0, PointId::Cut(2)),
        vec![
            (Dim::Reg(10, ReduceOp::Set), ObsOut::Scalar(1)),
            (Dim::Reg(11, ReduceOp::Max), ObsOut::Scalar(5)),
        ]
    );
    // Cut (50, 7): set takes the LATEST update (pos 3), accumulate dedups the
    // two identical reg-13 values into one, history counts the single note.
    assert_eq!(
        obs_at(&cap, rev, 0, PointId::Cut(7)),
        vec![
            (Dim::Reg(10, ReduceOp::Set), ObsOut::Scalar(2)),
            (Dim::Reg(11, ReduceOp::Max), ObsOut::Scalar(5)),
            (Dim::Reg(12, ReduceOp::Min), ObsOut::Scalar(9)),
            (Dim::Reg(13, ReduceOp::Accumulate), ObsOut::Values(vec![3])),
            (
                Dim::Tag(7),
                ObsOut::Hist {
                    count: 1,
                    ever: true,
                    latest: (30, 2)
                }
            ),
        ]
    );
    // D Cut(10) composes three segments: set overridden twice down the
    // lineage (2 -> 7 -> 3), min from B only (A's 9 is outside the fork cut),
    // accumulate from D's own suffix, history across A and B.
    assert_eq!(
        obs_at(&cap, rev, 3, PointId::Cut(10)),
        vec![
            (Dim::Reg(10, ReduceOp::Set), ObsOut::Scalar(3)),
            (Dim::Reg(11, ReduceOp::Max), ObsOut::Scalar(5)),
            (Dim::Reg(12, ReduceOp::Min), ObsOut::Scalar(1)),
            (Dim::Reg(13, ReduceOp::Accumulate), ObsOut::Values(vec![5])),
            (
                Dim::Tag(7),
                ObsOut::Hist {
                    count: 2,
                    ever: true,
                    latest: (35, 5)
                }
            ),
        ]
    );
    // The naive and shared formulations agree exactly, everywhere.
    assert_eq!(
        Captured::flat(&cap.obs_naive, rev),
        Captured::flat(&cap.obs_shared, rev)
    );

    // Every cell, exactly.
    assert_eq!(
        Captured::flat(&cap.cells, rev),
        vec![
            ((0, 0, PointId::Cut(2)), cell_a_cut2()),
            ((0, 0, PointId::Cut(7)), cell_a_cut7()),
            ((0, 0, PointId::Fork(4)), cell_a_fork4()),
            ((0, 0, PointId::Seal(0)), cell_a_fork4()),
            ((0, 1, PointId::Cut(5)), cell_b_cut5()),
            ((0, 1, PointId::Cut(8)), cell_b_cut8()),
            ((0, 1, PointId::Fork(8)), cell_b_cut8()),
            ((0, 1, PointId::Seal(1)), cell_b_seal()),
            ((0, 2, PointId::Cut(5)), cell_c_cut5()),
            ((0, 2, PointId::Seal(2)), cell_c_seal()),
            ((0, 3, PointId::Cut(10)), cell_d_cut10()),
            ((0, 3, PointId::Seal(3)), cell_d_seal()),
        ]
    );
}

/// Family 2 (first pass) — provisional transitions at configured unsealed
/// cuts, baselined at the inherited branch-point cell: the replay-nomination
/// view, exact.
#[test]
fn family2_provisional_transitions() {
    let (fx, cap) = tree();
    let rev = fx.max_rev();
    assert_eq!(
        Captured::flat(&cap.transitions, rev),
        vec![
            (
                (0, 0),
                Transition {
                    at_count: 2,
                    from: None,
                    to: cell_a_cut2()
                }
            ),
            (
                (0, 0),
                Transition {
                    at_count: 7,
                    from: Some(cell_a_cut2()),
                    to: cell_a_cut7()
                }
            ),
            (
                (0, 1),
                Transition {
                    at_count: 5,
                    from: Some(cell_a_fork4()),
                    to: cell_b_cut5()
                }
            ),
            (
                (0, 1),
                Transition {
                    at_count: 8,
                    from: Some(cell_b_cut5()),
                    to: cell_b_cut8()
                }
            ),
            (
                (0, 2),
                Transition {
                    at_count: 5,
                    from: Some(cell_a_fork4()),
                    to: cell_c_cut5()
                }
            ),
            (
                (0, 3),
                Transition {
                    at_count: 10,
                    from: Some(cell_b_cut8()),
                    to: cell_d_cut10()
                }
            ),
        ]
    );
    // No entries were committed in this fixture: occupancy is empty at every
    // revision even though provisional transitions abound.
    for r in 0..=rev {
        assert!(Captured::net(&cap.occupancy, r).is_empty());
    }
}

/// Family 2 (second pass) + quality domination — the two-revision
/// materialization barrier on the `two_pass` fixture: provisional at rev 2,
/// sealed with drift at rev 3, committed at rev 4 (quality tie broken by
/// entry id), dominated at rev 5. The provisional cell never reaches
/// occupancy at any revision.
#[test]
fn family2_two_pass_occupancy_and_domination() {
    let (built, _replay) = fixtures::two_pass();
    let fx = load(
        "two_pass",
        include_str!("../fixtures/two_pass.json"),
        &built,
    );
    let cap = run(&fx, BuildOpts::default(), 1).expect("valid fixture");

    let provisional: CellKey = vec![(0, 10, 5)];
    let sealed: CellKey = vec![(0, 10, 9)];
    let early: CellKey = vec![(0, 10, 1)];

    // Rev 2 (first pass): the transition nominates; nothing is sealed.
    assert_eq!(
        Captured::flat(&cap.transitions, 2),
        vec![(
            (0, 0),
            Transition {
                at_count: 2,
                from: None,
                to: provisional.clone()
            }
        )]
    );
    assert_eq!(
        Captured::flat(&cap.cells, 2),
        vec![((0, 0, PointId::Cut(2)), provisional.clone())]
    );
    assert!(Captured::net(&cap.occupancy, 2).is_empty());

    // Rev 3 (second pass): candidate seals enter at a later revision; the
    // cell actually true at sealed_at (count 3) differs from the provisional
    // cell observed at count 2 — the drift the two-pass design exists for.
    assert_eq!(
        Captured::flat(&cap.cells, 3),
        vec![
            ((0, 0, PointId::Cut(2)), provisional.clone()),
            ((0, 0, PointId::Seal(0)), sealed.clone()),
            ((0, 0, PointId::Seal(1)), early.clone()),
        ]
    );
    assert!(
        Captured::net(&cap.occupancy, 3).is_empty(),
        "no commits yet"
    );

    // Rev 4: commits land. Entries 100 and 101 tie on quality 5 for the
    // sealed cell; the stable tie-break picks the lower id. Entry 102 owns
    // the other cell.
    assert_eq!(
        Captured::flat(&cap.occupancy, 4),
        vec![((0, early.clone()), 102), ((0, sealed.clone()), 100)]
    );

    // Rev 5: entry 103 (quality 7) dominates the sealed cell.
    assert_eq!(
        Captured::flat(&cap.occupancy, 5),
        vec![((0, early.clone()), 102), ((0, sealed.clone()), 103)]
    );

    // The provisional cell can nominate replay but never occupy: at no
    // revision is it an occupancy key.
    for r in 0..=fx.max_rev() {
        assert!(
            Captured::net(&cap.occupancy, r)
                .iter()
                .all(|(((_c, cell), _e), _m)| *cell != provisional),
            "provisional cell reached occupancy at rev {r}"
        );
    }
}

/// Family 7 — property-level assertion aggregation: evaluations aggregate by
/// property across sites; site identity stays a separate coverage view; a
/// never-satisfied `must_hit` property is a finalized absence finding.
#[test]
fn family7_property_aggregation() {
    let (built, _replay) = fixtures::retention_properties();
    let fx = load(
        "retention_properties",
        include_str!("../fixtures/retention_properties.json"),
        &built,
    );
    let cap = run(&fx, BuildOpts::default(), 1).expect("valid fixture");
    let rev = fx.max_rev();

    // Property counts are scoped by source schema (r6): source 0's 500
    // aggregates its two sites into one row (2 passes + 1 fail); source 1's
    // 500 is a DISTINCT property whose lone failed evaluation never merges.
    assert_eq!(
        Captured::flat(&cap.property_results, rev),
        vec![((0, 0, 500), (2, 1)), ((0, 1, 500), (0, 1))]
    );
    // Coverage stays per site, within its source scope.
    assert_eq!(
        Captured::flat(&cap.site_coverage, rev),
        vec![
            ((0, 0, 500, 900), 2),
            ((0, 0, 500, 901), 1),
            ((0, 1, 500, 902), 1)
        ]
    );
    // Absence is a FINALIZED fact: before the campaign closes (revision 3)
    // there are no absence rows at all — an intermediate "not yet satisfied"
    // is not a finding. From the finalization onward, exactly the
    // never-satisfied property is absent, and the row never retracts
    // (evidence is validated to precede finalization).
    for r in 0..3 {
        assert!(
            Captured::net(&cap.absence, r).is_empty(),
            "no absence before closure"
        );
    }
    for r in 3..=rev {
        // Source 1's must_hit 500 stays absent — source 0's passing 500 is a
        // different property and must not suppress it — alongside the
        // never-fired 501.
        assert_eq!(
            Captured::flat(&cap.absence, r),
            vec![(0, 1, 500), (0, 1, 501)]
        );
    }
}

/// Family 8 — separation of the four record classes: expiring working-set
/// membership changes only the declared working view; committed Entry cells,
/// occupancy, and finalized property facts are bit-identical across the
/// retraction revision. Cross-source sequencing rejects the source-local
/// scrape and answers the rollout-global pair exactly.
#[test]
fn family8_retention_separation_and_ordering_scope() {
    let (built, _replay) = fixtures::retention_properties();
    let fx = load(
        "retention_properties",
        include_str!("../fixtures/retention_properties.json"),
        &built,
    );
    let cap = run(&fx, BuildOpts::default(), 1).expect("valid fixture");
    use differential_lineage::data::Species;

    // Working view: three admitted coordinates at rev 3; the tag-77 note
    // expires at rev 4.
    assert_eq!(
        Captured::flat(&cap.working_species, 3),
        vec![
            ((0, Species::Assertion(500)), 1),
            ((0, Species::Note(77)), 1),
            ((0, Species::Note(88)), 1),
        ]
    );
    assert_eq!(
        Captured::flat(&cap.working_species, 4),
        vec![
            ((0, Species::Assertion(500)), 1),
            ((0, Species::Note(88)), 1)
        ]
    );

    // Committed and finalized views are identical across the retraction.
    let sealed_cell: CellKey = vec![(0, 10, 42), (1, 77, 1), (1, 88, 1)];
    let occupancy3 = Captured::flat(&cap.occupancy, 3);
    assert_eq!(occupancy3, vec![((0, sealed_cell), 200)]);
    assert_eq!(occupancy3, Captured::flat(&cap.occupancy, 4));
    assert_eq!(
        Captured::flat(&cap.property_results, 3),
        Captured::flat(&cap.property_results, 4)
    );
    assert_eq!(
        Captured::flat(&cap.absence, 3),
        Captured::flat(&cap.absence, 4)
    );
    assert_eq!(Captured::flat(&cap.cells, 3), Captured::flat(&cap.cells, 4));

    // Cross-source sequences: the rollout-global pair answers exactly; the
    // query naming the source-local scrape is rejected, not answered.
    assert_eq!(
        Captured::flat(&cap.seq_pairs, 4),
        vec![((0, 0, 0), ((4, 40, 88), (6, 60, 99)))]
    );
    assert_eq!(Captured::flat(&cap.seq_rejections, 4), vec![((0, 1), 2)]);
    // Scrape lines remain reportable terminal evidence under their
    // source-local ordinals.
    assert_eq!(
        Captured::flat(&cap.scrape_terminal, 4),
        vec![((0, 0), (0, 40)), ((0, 0), (1, 41))]
    );
}

/// Probe/consolidate/canonical-sort discipline: views are read only after the
/// probe passes a revision (`run` steps until it has); reads consolidate
/// update streams and canonically sort; multiplicities are exactly one on
/// set-like views; and a revision that only retracts (rev 4's working
/// expiration) nets records away without disturbing anything else.
#[test]
fn probe_consolidate_canonical_sort_discipline() {
    let (built, _replay) = fixtures::retention_properties();
    let fx = load(
        "retention_properties",
        include_str!("../fixtures/retention_properties.json"),
        &built,
    );
    let cap = run(&fx, BuildOpts::default(), 1).expect("valid fixture");

    // The captured update stream for the working view nets a retraction: raw
    // updates at rev 4 contain a negative diff, and consolidation at rev 4
    // removes the tag-77 row entirely (net zero) rather than leaving a
    // zero-count row.
    use differential_lineage::data::Species;
    let raw_rev4: Vec<_> = cap
        .working_species
        .iter()
        .filter(|(_, t, _)| *t == 4)
        .collect();
    assert!(
        raw_rev4.iter().any(|(_, _, r)| *r < 0),
        "expected a retraction diff at rev 4, got {raw_rev4:?}"
    );
    let net4 = Captured::net(&cap.working_species, 4);
    assert!(net4.iter().all(|(_, m)| *m == 1), "canonical multiplicity");
    assert!(
        net4.iter()
            .all(|(((_c, s), _n), _m)| *s != Species::Note(77)),
        "net-zero row must vanish under consolidation"
    );

    // Canonical sort: `flat` output is strictly ascending (no duplicates, no
    // ordering surprises), for every view with rows.
    let cells = Captured::flat(&cap.cells, fx.max_rev());
    assert!(cells.windows(2).all(|w| w[0] < w[1]));
    let obs = Captured::flat(&cap.obs_shared, fx.max_rev());
    assert!(obs.windows(2).all(|w| w[0] < w[1]));
}

/// The committed fixture files regenerate bit-identically (fixture
/// determinism gate).
#[test]
fn committed_fixtures_regenerate_identically() {
    for (fx, _replay) in [
        fixtures::tree_lineage(),
        fixtures::two_pass(),
        fixtures::retention_properties(),
    ] {
        let committed = match fx.name.as_str() {
            "tree_lineage" => include_str!("../fixtures/tree_lineage.json"),
            "two_pass" => include_str!("../fixtures/two_pass.json"),
            "retention_properties" => include_str!("../fixtures/retention_properties.json"),
            other => panic!("unknown fixture {other}"),
        };
        let mut rebuilt = serde_json::to_string_pretty(&fx).expect("serialize");
        rebuilt.push('\n');
        assert_eq!(committed, rebuilt, "fixture {} drifted", fx.name);
    }
}
