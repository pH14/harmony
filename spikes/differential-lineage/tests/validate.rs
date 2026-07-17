// SPDX-License-Identifier: AGPL-3.0-or-later
//! Malformed-fixture rejection: the structural contracts `Fixture::validate`
//! enforces (r1 + r2 reviews), each returned as a typed `ValidationError` —
//! never a panic or a hang — through the public `dataflow::run` and
//! `Referee::new` APIs. Covers: lineage forests (a cycle would keep the
//! ancestry iteration from converging), revision advanceability, checked
//! position arithmetic, the physical branch-point cut contract, contiguous
//! suffix positions, nondecreasing Moments (within a rollout and across
//! lineage boundaries), unique declarations, and declared query sources.

use differential_lineage::data::{
    Cut, LineageRec, Payload, Revision, SdkEventRec, ValidationError,
};
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
    assert_eq!(
        fx.validate(),
        Err(ValidationError::SelfParent { rollout: 0 })
    );
}

#[test]
fn lineage_cycle_rejected() {
    // 1 -> 2 -> 3 -> 1: without validation the ancestry iteration would
    // never reach a fixed point (depth grows forever).
    let (mut fx, _) = fixtures::two_pass();
    fx.lineage.push(lineage(1, 2, 0));
    fx.lineage.push(lineage(2, 3, 0));
    fx.lineage.push(lineage(3, 1, 0));
    assert!(matches!(
        fx.validate(),
        Err(ValidationError::LineageCycle { .. })
    ));
}

#[test]
fn two_parents_rejected() {
    let (mut fx, _) = fixtures::tree_lineage();
    // Rollout 3 (D) already has parent 1 (B); add a second parent.
    fx.lineage.push(lineage(3, 2, 4));
    assert_eq!(
        fx.validate(),
        Err(ValidationError::TwoParents { rollout: 3 })
    );
}

#[test]
fn revision_max_rejected() {
    let (mut fx, _) = fixtures::two_pass();
    let mut e = ok_event(0, 3, 40);
    e.rev = Revision::MAX;
    fx.events.push(e);
    assert!(matches!(
        fx.validate(),
        Err(ValidationError::RevisionMax { .. })
    ));
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
    assert_eq!(
        fx.validate(),
        Err(ValidationError::CutOutOfBounds {
            kind: "seal",
            rollout: 0,
            count: 9,
            lo: 0,
            hi: 3,
        })
    );
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
    assert_eq!(
        fx.validate(),
        Err(ValidationError::CutOutOfBounds {
            kind: "obs",
            rollout: 3,
            count: 2,
            lo: 8,
            hi: 11,
        })
    );
}

#[test]
fn non_contiguous_positions_rejected() {
    let (mut fx, _) = fixtures::two_pass();
    fx.events.push(ok_event(0, 7, 40)); // rollout 0 holds 0..3; 7 leaves a gap
    assert!(matches!(
        fx.validate(),
        Err(ValidationError::NonContiguousPositions { .. })
    ));
}

#[test]
fn position_overflow_is_checked_not_wrapped() {
    // r2: a hostile fork cut near u64::MAX must fail through checked
    // arithmetic (typed error), not overflow before the bound check. The
    // huge count also exceeds the parent's extent, so whichever check fires
    // first, the fixture is refused without a panic.
    let (mut fx, _) = fixtures::two_pass();
    fx.lineage.push(lineage(1, 0, u64::MAX));
    fx.events.push(SdkEventRec {
        rev: 3,
        config: 0,
        rollout: 1,
        source: 0,
        pos: u64::MAX,
        moment: 50,
        payload: Payload::Note { tag: 1 },
    });
    let err = fx.validate().unwrap_err();
    assert!(
        matches!(
            err,
            ValidationError::PositionOverflow { .. } | ValidationError::CutOutOfBounds { .. }
        ),
        "{err}"
    );
}

#[test]
fn decreasing_moments_within_a_rollout_rejected() {
    // r2: pos 0/1 with Moments 10/5 breaks canonical (Moment, pos) order.
    let (mut fx, _) = fixtures::two_pass();
    // two_pass rollout 0 has moments 10, 20, 30 at pos 0..3; corrupt pos 1.
    fx.events[1].moment = 5;
    assert_eq!(
        fx.validate(),
        Err(ValidationError::DecreasingMoments {
            config: 0,
            rollout: 0,
            pos: 1,
            moment: 5,
            prev: 10,
        })
    );
}

#[test]
fn decreasing_moments_across_lineage_rejected() {
    // The same contract across a fork: a child whose first own event
    // precedes the last moment it inherits breaks full-vector order.
    let (mut fx, _) = fixtures::two_pass();
    fx.lineage.push(lineage(1, 0, 3)); // inherits through moment 30
    fx.events.push(SdkEventRec {
        rev: 3,
        config: 0,
        rollout: 1,
        source: 0,
        pos: 3,
        moment: 7, // < 30
        payload: Payload::Note { tag: 1 },
    });
    assert_eq!(
        fx.validate(),
        Err(ValidationError::DecreasingMoments {
            config: 0,
            rollout: 1,
            pos: 3,
            moment: 7,
            prev: 30,
        })
    );
}

#[test]
fn duplicate_declarations_rejected() {
    // r2: conflicting (or duplicate) declarations make the dataflow's
    // declaration joins fan out and disagree with the referee's last-wins
    // map — for sources, and equally for registers and properties.
    use differential_lineage::data::{
        OrderScope, PropertyDecl, ReduceOp, RegisterDecl, SourceDecl,
    };

    let (mut fx, _) = fixtures::retention_properties();
    fx.sources.push(SourceDecl {
        rev: 1,
        config: 0,
        source: 0,
        scope: OrderScope::SourceLocal, // conflicts with RolloutGlobal
    });
    assert_eq!(
        fx.validate(),
        Err(ValidationError::DuplicateDeclaration {
            what: "source",
            config: 0,
            id: 0,
        })
    );

    let (mut fx, _) = fixtures::retention_properties();
    fx.registers.push(RegisterDecl {
        rev: 1,
        config: 0,
        reg: 10,
        op: ReduceOp::Max, // conflicts with Set
    });
    assert_eq!(
        fx.validate(),
        Err(ValidationError::DuplicateDeclaration {
            what: "register",
            config: 0,
            id: 10,
        })
    );

    let (mut fx, _) = fixtures::retention_properties();
    fx.properties.push(PropertyDecl {
        rev: 1,
        config: 0,
        property: 500,
        must_hit: false,
    });
    assert_eq!(
        fx.validate(),
        Err(ValidationError::DuplicateDeclaration {
            what: "property",
            config: 0,
            id: 500,
        })
    );
}

#[test]
fn undeclared_query_source_rejected() {
    let (mut fx, _) = fixtures::retention_properties();
    fx.seq_queries
        .push(differential_lineage::data::SeqQueryRec {
            rev: 1,
            config: 0,
            query: 7,
            src_a: 0,
            src_b: 99, // never declared
        });
    assert_eq!(
        fx.validate(),
        Err(ValidationError::UndeclaredQuerySource {
            config: 0,
            query: 7,
            src: 99,
        })
    );
}

#[test]
fn run_refuses_malformed_fixture_with_typed_error() {
    // r2: the public API returns the error; it does not panic (and, before
    // validation existed, this exact input hung the ancestry iteration).
    let (mut fx, _) = fixtures::two_pass();
    fx.lineage.push(lineage(0, 5, 0));
    fx.lineage.push(lineage(5, 0, 0));
    let err = differential_lineage::dataflow::run(
        &fx,
        differential_lineage::dataflow::BuildOpts::default(),
        1,
    )
    .unwrap_err();
    assert!(matches!(err, ValidationError::LineageCycle { .. }), "{err}");
}

#[test]
fn referee_refuses_short_replay_with_typed_error() {
    use differential_lineage::referee::Referee;
    let (fx, mut replay) = fixtures::two_pass();
    // Truncate the replay vector below the seal cut (count 3).
    replay.full[0].1.truncate(1);
    let err = Referee::new(&fx, &replay).err().expect("must refuse");
    assert_eq!(
        err,
        ValidationError::ReplayTooShort {
            rollout: 0,
            count: 3,
        }
    );
}

// ---------------------------------------------------------------------------
// r3: record-identity uniqueness, lineage-before-dependents, cross-record
// references, and the referee's parent-side replay coverage.
// ---------------------------------------------------------------------------

#[test]
fn duplicate_seal_id_rejected() {
    // A duplicate (config, seal) would emit multiplicity-2 seal-prefix rows
    // and break the canonical unit-multiplicity read.
    let (mut fx, _) = fixtures::two_pass();
    fx.seals.push(fx.seals[0].clone());
    assert_eq!(
        fx.validate(),
        Err(ValidationError::DuplicateRecord {
            what: "seal",
            config: 0,
            detail: "seal 0".to_owned(),
        })
    );
}

#[test]
fn duplicate_obs_cut_rejected() {
    let (mut fx, _) = fixtures::two_pass();
    let mut c = fx.obs_cuts[0].clone();
    c.cut.moment = 21; // same count, different moment: same identity
    fx.obs_cuts.push(c);
    assert_eq!(
        fx.validate(),
        Err(ValidationError::DuplicateRecord {
            what: "obs cut",
            config: 0,
            detail: "rollout 0 count 2".to_owned(),
        })
    );
}

#[test]
fn duplicate_scrape_ordinal_rejected() {
    let (mut fx, _) = fixtures::retention_properties();
    fx.scrape.push(fx.scrape[0].clone());
    assert_eq!(
        fx.validate(),
        Err(ValidationError::DuplicateRecord {
            what: "scrape line",
            config: 0,
            detail: "rollout 0 ordinal 0".to_owned(),
        })
    );
}

#[test]
fn duplicate_seq_query_rejected() {
    let (mut fx, _) = fixtures::retention_properties();
    fx.seq_queries.push(fx.seq_queries[0].clone());
    assert_eq!(
        fx.validate(),
        Err(ValidationError::DuplicateRecord {
            what: "sequence query",
            config: 0,
            detail: "query 0".to_owned(),
        })
    );
}

#[test]
fn duplicate_entry_commit_rejected() {
    let (mut fx, _) = fixtures::two_pass();
    fx.entry_commits.push(fx.entry_commits[0].clone());
    assert_eq!(
        fx.validate(),
        Err(ValidationError::DuplicateRecord {
            what: "entry commit",
            config: 0,
            detail: "entry 100".to_owned(),
        })
    );
}

#[test]
fn duplicate_event_position_rejected_via_contiguity() {
    // Event identity (config, rollout, pos) is enforced by the contiguity
    // rule: a duplicated position cannot form the strict range.
    let (mut fx, _) = fixtures::two_pass();
    fx.events.push(fx.events[0].clone());
    assert!(matches!(
        fx.validate(),
        Err(ValidationError::NonContiguousPositions { .. })
    ));
}

#[test]
fn point_before_lineage_rejected() {
    // r3: a child's obs cut or seal committing before its lineage record
    // would let the referee compose an inherited prefix the dataflow cannot
    // see yet (the edge is not committed) — every-revision parity would be
    // false there, so the ordering is refused by contract.
    let (mut fx, _) = fixtures::tree_lineage();
    // Rollout 1 (B) has its lineage at revision 3; back-date one of its cuts.
    let idx = fx
        .obs_cuts
        .iter()
        .position(|c| c.rollout == 1)
        .expect("B has cuts");
    fx.obs_cuts[idx].rev = 2;
    assert_eq!(
        fx.validate(),
        Err(ValidationError::RecordBeforeLineage {
            what: "obs cut",
            config: 0,
            child: 1,
            rev: 2,
            lineage_rev: 3,
        })
    );

    let (mut fx, _) = fixtures::tree_lineage();
    let idx = fx
        .seals
        .iter()
        .position(|s| s.rollout == 1)
        .expect("B has a seal");
    fx.seals[idx].rev = 2;
    assert_eq!(
        fx.validate(),
        Err(ValidationError::RecordBeforeLineage {
            what: "seal",
            config: 0,
            child: 1,
            rev: 2,
            lineage_rev: 3,
        })
    );
}

#[test]
fn child_event_before_lineage_rejected() {
    let (mut fx, _) = fixtures::tree_lineage();
    let idx = fx
        .events
        .iter()
        .position(|e| e.rollout == 1)
        .expect("B has events");
    fx.events[idx].rev = 1;
    assert_eq!(
        fx.validate(),
        Err(ValidationError::RecordBeforeLineage {
            what: "event",
            config: 0,
            child: 1,
            rev: 1,
            lineage_rev: 3,
        })
    );
}

#[test]
fn descendant_fork_before_lineage_rejected() {
    // D forks off B at revision 5; B's own lineage commits at revision 3.
    // Back-dating D's fork below 3 breaks chain-revision monotonicity.
    let (mut fx, _) = fixtures::tree_lineage();
    let idx = fx
        .lineage
        .iter()
        .position(|l| l.child == 3)
        .expect("D's lineage");
    fx.lineage[idx].rev = 2;
    // D's own records also violate their (now earlier) lineage revision, but
    // the descendant-fork rule must hold regardless; relax D's records too so
    // the fork rule itself is what fires.
    for e in &mut fx.events {
        if e.rollout == 3 {
            e.rev = 2;
        }
    }
    for c in &mut fx.obs_cuts {
        if c.rollout == 3 {
            c.rev = 2;
        }
    }
    for s in &mut fx.seals {
        if s.rollout == 3 {
            s.rev = 2;
        }
    }
    assert_eq!(
        fx.validate(),
        Err(ValidationError::RecordBeforeLineage {
            what: "descendant lineage",
            config: 0,
            child: 1,
            rev: 2,
            lineage_rev: 3,
        })
    );
}

#[test]
fn dangling_entry_commit_rejected() {
    let (mut fx, _) = fixtures::two_pass();
    fx.entry_commits
        .push(differential_lineage::data::EntryCommitRec {
            rev: 5,
            config: 0,
            entry: 999,
            rollout: 0,
            seal: 77, // no such seal at any revision
            quality: 1,
        });
    assert_eq!(
        fx.validate(),
        Err(ValidationError::DanglingEntryCommit {
            config: 0,
            entry: 999,
            rollout: 0,
            seal: 77,
        })
    );
}

#[test]
fn query_before_source_declaration_rejected() {
    // A committed query must not wait on its sources' scope declarations:
    // the dataflow's join sits silent until the declaration commits while a
    // revision-filtered reader already judges the query.
    let (mut fx, _) = fixtures::retention_properties();
    let idx = fx
        .sources
        .iter()
        .position(|s| s.source == 1)
        .expect("source 1 declared");
    fx.sources[idx].rev = 3; // query 0 uses source 1 at revision 1
    assert_eq!(
        fx.validate(),
        Err(ValidationError::DeclarationAfterUse {
            what: "sequence query",
            config: 0,
            id: 1,
            decl_rev: 3,
            use_rev: 1,
        })
    );
}

#[test]
fn dangling_working_ref_rejected() {
    let (mut fx, _) = fixtures::retention_properties();
    fx.working.push(differential_lineage::data::WorkingRec {
        rev: 3,
        config: 0,
        rollout: 0,
        pos: 99, // no such evidence coordinate
        delta: 1,
    });
    assert_eq!(
        fx.validate(),
        Err(ValidationError::DanglingWorkingRef {
            config: 0,
            rollout: 0,
            pos: 99,
        })
    );
}

#[test]
fn working_net_out_of_range_rejected() {
    // Double admission.
    let (mut fx, _) = fixtures::retention_properties();
    fx.working.push(differential_lineage::data::WorkingRec {
        rev: 3,
        config: 0,
        rollout: 0,
        pos: 0,
        delta: 1, // pos 0 already admitted at revision 3
    });
    assert_eq!(
        fx.validate(),
        Err(ValidationError::WorkingNetOutOfRange {
            config: 0,
            rollout: 0,
            pos: 0,
            rev: 3,
            net: 2,
        })
    );

    // Expiration of something never admitted.
    let (mut fx, _) = fixtures::retention_properties();
    fx.working.push(differential_lineage::data::WorkingRec {
        rev: 2,
        config: 0,
        rollout: 0,
        pos: 2,
        delta: -1,
    });
    assert_eq!(
        fx.validate(),
        Err(ValidationError::WorkingNetOutOfRange {
            config: 0,
            rollout: 0,
            pos: 2,
            rev: 2,
            net: -1,
        })
    );
}

#[test]
fn referee_refuses_short_parent_replay() {
    // r3: the referee evaluates Fork points by slicing the PARENT's replay
    // vector at the fork count; that side must be coverage-checked too.
    use differential_lineage::data::{Cut, OrderScope, Payload, ReduceOp};
    use differential_lineage::generate::Builder;
    use differential_lineage::referee::Referee;

    let mut b = Builder::new("short-parent", 0);
    b.reg(1, 10, ReduceOp::Set)
        .source(1, 0, OrderScope::RolloutGlobal);
    let a = b.genesis();
    b.push(2, a, 0, 10, Payload::Register { reg: 10, value: 1 });
    b.push(2, a, 0, 20, Payload::Register { reg: 10, value: 2 });
    let c = b.fork(
        3,
        a,
        Cut {
            moment: 20,
            count: 2,
        },
    );
    b.push(3, c, 0, 30, Payload::Register { reg: 10, value: 3 });
    b.seal(
        4,
        c,
        0,
        Cut {
            moment: 30,
            count: 3,
        },
    );
    let (fx, mut replay) = b.finish();
    assert_eq!(fx.validate(), Ok(()));
    // Truncate the PARENT's vector below the fork count; the child's own
    // vector (with its inherited copy) stays intact, so only the parent-side
    // check can catch this.
    let a_idx = replay
        .full
        .iter()
        .position(|(r, _)| *r == a)
        .expect("parent vector");
    replay.full[a_idx].1.truncate(1);
    assert_eq!(
        Referee::new(&fx, &replay).err(),
        Some(ValidationError::ReplayTooShort {
            rollout: a,
            count: 2,
        })
    );
}

#[test]
fn working_delta_overflow_pair_rejected() {
    // r4: delta 1 and delta i64::MAX on one (config, rollout, pos, rev) used
    // to overflow the net accumulation inside validation itself in debug
    // builds — before the promised typed error could be returned. The
    // non-membership delta is now rejected as a typed error first.
    let (mut fx, _) = fixtures::retention_properties();
    fx.working.push(differential_lineage::data::WorkingRec {
        rev: 3,
        config: 0,
        rollout: 0,
        pos: 2,
        delta: 1,
    });
    fx.working.push(differential_lineage::data::WorkingRec {
        rev: 3,
        config: 0,
        rollout: 0,
        pos: 2,
        delta: i64::MAX,
    });
    assert_eq!(
        fx.validate(),
        Err(ValidationError::WorkingDeltaOutOfRange {
            config: 0,
            rollout: 0,
            pos: 2,
            rev: 3,
            delta: i64::MAX,
        })
    );
    // And the public APIs return it rather than panicking.
    let err = differential_lineage::dataflow::run(
        &fx,
        differential_lineage::dataflow::BuildOpts::default(),
        1,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        ValidationError::WorkingDeltaOutOfRange { .. }
    ));
}

#[test]
fn working_delta_min_counterpart_rejected() {
    let (mut fx, _) = fixtures::retention_properties();
    fx.working.push(differential_lineage::data::WorkingRec {
        rev: 3,
        config: 0,
        rollout: 0,
        pos: 2,
        delta: -1,
    });
    fx.working.push(differential_lineage::data::WorkingRec {
        rev: 3,
        config: 0,
        rollout: 0,
        pos: 2,
        delta: i64::MIN,
    });
    assert_eq!(
        fx.validate(),
        Err(ValidationError::WorkingDeltaOutOfRange {
            config: 0,
            rollout: 0,
            pos: 2,
            rev: 3,
            delta: i64::MIN,
        })
    );
}
