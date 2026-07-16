// SPDX-License-Identifier: AGPL-3.0-or-later
//! Malformed-fixture rejection: the structural contracts `Fixture::validate`
//! enforces (r1 review): lineage must be a forest (a cycle would keep the
//! ancestry iteration from converging), revisions must be advanceable, cuts
//! must respect the physical branch-point contract and persisted extents,
//! and positions must be the contiguous suffix range.

use differential_lineage::data::{Cut, LineageRec, Payload, Revision, SdkEventRec};
use differential_lineage::fixtures;

fn ok_event(rollout: u32, pos: u64, moment: u64) -> SdkEventRec {
    SdkEventRec {
        rev: 2,
        config: 0,
        rollout,
        source: 0,
        pos,
        moment,
        payload: Payload::Note { tag: 1 },
    }
}

fn lineage(child: u32, parent: u32, count: u64) -> LineageRec {
    LineageRec {
        rev: 2,
        config: 0,
        child,
        parent,
        cut: Cut { moment: 0, count },
    }
}

#[test]
fn hand_fixtures_are_valid() {
    for (fx, _) in [
        fixtures::tree_lineage(),
        fixtures::two_pass(),
        fixtures::retention_properties(),
    ] {
        assert_eq!(fx.validate(), Ok(()), "{}", fx.name);
    }
}

#[test]
fn self_parent_rejected() {
    let (mut fx, _) = fixtures::two_pass();
    fx.lineage.push(lineage(0, 0, 0));
    let err = fx.validate().unwrap_err();
    assert!(err.contains("its own parent"), "{err}");
}

#[test]
fn lineage_cycle_rejected() {
    // 1 -> 2 -> 3 -> 1: without validation the ancestry iteration would
    // never reach a fixed point (depth grows forever).
    let (mut fx, _) = fixtures::two_pass();
    fx.lineage.push(lineage(1, 2, 0));
    fx.lineage.push(lineage(2, 3, 0));
    fx.lineage.push(lineage(3, 1, 0));
    let err = fx.validate().unwrap_err();
    assert!(err.contains("cycle"), "{err}");
}

#[test]
fn two_parents_rejected() {
    let (mut fx, _) = fixtures::tree_lineage();
    // Rollout 3 (D) already has parent 1 (B); add a second parent.
    fx.lineage.push(lineage(3, 2, 4));
    let err = fx.validate().unwrap_err();
    assert!(err.contains("two parents"), "{err}");
}

#[test]
fn revision_max_rejected() {
    let (mut fx, _) = fixtures::two_pass();
    let mut e = ok_event(0, 3, 40);
    e.rev = Revision::MAX;
    fx.events.push(e);
    let err = fx.validate().unwrap_err();
    assert!(err.contains("Revision::MAX"), "{err}");
}

#[test]
fn seal_beyond_evidence_rejected() {
    // two_pass rollout 0 persists positions 0..3; a seal at count 9 has no
    // evidence behind it (this is the malformed input that used to reach a
    // slice panic in the referee).
    let (mut fx, _) = fixtures::two_pass();
    fx.seals.push(differential_lineage::data::SealRec {
        rev: 3,
        config: 0,
        rollout: 0,
        seal: 9,
        cut: Cut {
            moment: 99,
            count: 9,
        },
    });
    let err = fx.validate().unwrap_err();
    assert!(err.contains("seal cut 9"), "{err}");
}

#[test]
fn cut_before_branch_point_rejected() {
    // Rollout 3 (D) starts at count 8; an obs cut at 2 precedes its branch
    // point — the physical cut contract violation the parity harness
    // originally caught in the random generator.
    let (mut fx, _) = fixtures::tree_lineage();
    fx.obs_cuts.push(differential_lineage::data::ObsCutRec {
        rev: 5,
        config: 0,
        rollout: 3,
        cut: Cut {
            moment: 30,
            count: 2,
        },
    });
    let err = fx.validate().unwrap_err();
    assert!(err.contains("obs cut 2"), "{err}");
}

#[test]
fn non_contiguous_positions_rejected() {
    let (mut fx, _) = fixtures::two_pass();
    fx.events.push(ok_event(0, 7, 40)); // rollout 0 holds 0..3; 7 leaves a gap
    let err = fx.validate().unwrap_err();
    assert!(err.contains("non-contiguous"), "{err}");
}

#[test]
#[should_panic(expected = "malformed fixture")]
fn run_refuses_malformed_fixture() {
    let (mut fx, _) = fixtures::two_pass();
    fx.lineage.push(lineage(0, 5, 0));
    fx.lineage.push(lineage(5, 0, 0));
    differential_lineage::dataflow::run(
        &fx,
        differential_lineage::dataflow::BuildOpts::default(),
        1,
    );
}
