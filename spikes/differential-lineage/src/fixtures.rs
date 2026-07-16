//! The three committed hand fixtures. Each is built through the validating
//! `Builder` so the genesis-complete replay authority comes along by
//! construction; `examples/gen_fixtures.rs` writes their JSON under
//! `fixtures/`, and the exact tests assert the committed bytes still match.

use crate::data::{Cut, Fixture, OrderScope, Payload, ReduceOp, Replay};
use crate::generate::Builder;

/// Fixture 1 — a four-rollout branch tree exercising lineage-complete
/// prefixes, half-open same-`Moment` cuts, sibling-safe identity, canonical
/// order, and all four register reductions plus history.
///
/// ```text
/// A (genesis, pos 0..=6)
/// ├── B @ cut (30,4)  (pos 4..=8)     — sibling 1
/// │   └── D @ cut (44,8) (pos 8..=10)
/// └── C @ cut (30,4)  (pos 4..=6)     — sibling 2, same cut as B
/// ```
///
/// Same-`Moment` straddles: A pos 2,3 (m30) inside the fork cut, pos 4 (m30)
/// outside; D pos 8 (m61) inside seal S3, pos 9 (m61) outside.
pub fn tree_lineage() -> (Fixture, Replay) {
    let mut b = Builder::new("tree_lineage", 0);
    b.reg(1, 10, ReduceOp::Set)
        .reg(1, 11, ReduceOp::Max)
        .reg(1, 12, ReduceOp::Min)
        .reg(1, 13, ReduceOp::Accumulate)
        .source(1, 0, OrderScope::RolloutGlobal)
        .source(1, 1, OrderScope::RolloutGlobal)
        .source(1, 2, OrderScope::SourceLocal);

    let a = b.genesis();
    b.push(2, a, 0, 10, Payload::Register { reg: 10, value: 1 });
    b.push(2, a, 0, 20, Payload::Register { reg: 11, value: 5 });
    b.push(2, a, 1, 30, Payload::Note { tag: 7 });
    b.push(2, a, 0, 30, Payload::Register { reg: 10, value: 2 });
    b.push(2, a, 0, 30, Payload::Register { reg: 12, value: 9 });
    b.push(2, a, 0, 40, Payload::Register { reg: 13, value: 3 });
    b.push(2, a, 0, 50, Payload::Register { reg: 13, value: 3 });
    b.obs_cut(2, a, Cut { moment: 20, count: 2 });
    b.obs_cut(2, a, Cut { moment: 50, count: 7 });

    let fork = Cut { moment: 30, count: 4 };
    let bb = b.fork(3, a, fork);
    b.push(3, bb, 0, 35, Payload::Register { reg: 10, value: 7 });
    b.push(3, bb, 1, 35, Payload::Note { tag: 7 });
    b.push(3, bb, 0, 44, Payload::Register { reg: 11, value: 2 });
    b.push(3, bb, 0, 44, Payload::Register { reg: 12, value: 1 });
    b.push(3, bb, 0, 60, Payload::Register { reg: 13, value: 8 });
    b.obs_cut(3, bb, Cut { moment: 35, count: 5 });
    b.obs_cut(3, bb, Cut { moment: 44, count: 8 });

    let c = b.fork(4, a, fork);
    b.push(4, c, 0, 35, Payload::Register { reg: 10, value: 100 });
    b.push(4, c, 0, 36, Payload::Register { reg: 13, value: 4 });
    b.push(4, c, 0, 70, Payload::Register { reg: 11, value: 50 });
    b.obs_cut(4, c, Cut { moment: 35, count: 5 });

    let d = b.fork(5, bb, Cut { moment: 44, count: 8 });
    b.push(5, d, 0, 61, Payload::Register { reg: 10, value: 3 });
    b.push(5, d, 0, 61, Payload::Register { reg: 13, value: 5 });
    b.push(5, d, 0, 80, Payload::Register { reg: 12, value: 0 });
    b.obs_cut(5, d, Cut { moment: 61, count: 10 });

    b.seal(6, a, 0, Cut { moment: 30, count: 4 });
    b.seal(6, bb, 1, Cut { moment: 60, count: 9 });
    b.seal(6, c, 2, Cut { moment: 36, count: 6 });
    b.seal(6, d, 3, Cut { moment: 61, count: 9 });

    b.finish()
}

/// Fixture 2 — the two-pass materialization economics: a provisional
/// transition at an unsealed cut (first pass, revision 2), a candidate seal
/// with state drift past the observed cut (second pass, revision 3), entry
/// commits with a quality tie (revision 4), and a later domination flip
/// (revision 5). The provisional cell must never appear in occupancy.
pub fn two_pass() -> (Fixture, Replay) {
    let mut b = Builder::new("two_pass", 0);
    b.reg(1, 10, ReduceOp::Set).source(1, 0, OrderScope::RolloutGlobal);

    let g = b.genesis();
    b.push(2, g, 0, 10, Payload::Register { reg: 10, value: 1 });
    b.push(2, g, 0, 20, Payload::Register { reg: 10, value: 5 });
    b.push(2, g, 0, 30, Payload::Register { reg: 10, value: 9 });
    b.obs_cut(2, g, Cut { moment: 20, count: 2 });

    b.seal(3, g, 0, Cut { moment: 30, count: 3 });
    b.seal(3, g, 1, Cut { moment: 10, count: 1 });

    b.commit_entry(4, 100, g, 0, 5);
    b.commit_entry(4, 101, g, 0, 5);
    b.commit_entry(4, 102, g, 1, 9);

    b.commit_entry(5, 103, g, 0, 7);

    b.finish()
}

/// Fixture 3 — retention and property semantics: property-level aggregation
/// across sites, a never-satisfied `must_hit` property, working-set admission
/// then expiration (which must move only the working view), terminal scrape
/// evidence, one eligible cross-source sequence query, and one rejected
/// (scrape) sequence query.
pub fn retention_properties() -> (Fixture, Replay) {
    let mut b = Builder::new("retention_properties", 0);
    b.reg(1, 10, ReduceOp::Set)
        .source(1, 0, OrderScope::RolloutGlobal)
        .source(1, 1, OrderScope::RolloutGlobal)
        .source(1, 2, OrderScope::SourceLocal)
        .property(1, 500, true)
        .property(1, 501, true)
        .seq_query(1, 0, 0, 1)
        .seq_query(1, 1, 0, 2);

    let r = b.genesis();
    b.push(2, r, 0, 10, Payload::Assertion { site: 900, property: 500, passed: true });
    b.push(2, r, 1, 20, Payload::Note { tag: 77 });
    b.push(2, r, 0, 20, Payload::Assertion { site: 901, property: 500, passed: true });
    b.push(2, r, 0, 30, Payload::Assertion { site: 900, property: 500, passed: false });
    b.push(2, r, 0, 40, Payload::Note { tag: 88 });
    b.push(2, r, 0, 50, Payload::Register { reg: 10, value: 42 });
    b.push(2, r, 1, 60, Payload::Note { tag: 99 });
    b.scrape(2, r, 40).scrape(2, r, 41);

    b.seal(3, r, 0, Cut { moment: 50, count: 6 });
    b.commit_entry(3, 200, r, 0, 1);
    b.working(3, r, 0, 1).working(3, r, 1, 1).working(3, r, 4, 1);

    b.working(4, r, 1, -1);

    b.finish()
}
