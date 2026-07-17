// SPDX-License-Identifier: AGPL-3.0-or-later
//! The tribunal judge's repros (PR #121 verify event, adjudication J1/J4),
//! folded in from `/tmp/harmony-agents/pr121-judge_repro.rs` and INVERTED:
//! each case the judge proved was accepted must now be refused with the
//! typed error.

use differential_lineage::data::{
    Cut, FinalizeRec, LineageRec, Payload, PropertyDecl, SdkEventRec, SealRec, ValidationError,
};
use differential_lineage::fixtures;

/// J1 repro 1 (closer): register events at Moments 10/20/30; a seal
/// `{moment: 100, count: 1}` claims the sealed state AT Moment 100 was the
/// count-1 prefix, though events from Moments 20 and 30 (< 100) are
/// excluded — the derived cell would assert state that was not true at
/// `sealed_at`. The first excluded persisted event now bounds the cut.
#[test]
fn first_excluded_event_before_cut_moment_rejected() {
    let (mut fx, _) = fixtures::two_pass();
    fx.seals.push(SealRec {
        rev: 3,
        config: 0,
        rollout: 0,
        seal: 9,
        cut: Cut {
            moment: 100,
            count: 1,
        },
    });
    assert_eq!(
        fx.validate(),
        Err(ValidationError::CutMomentIncoherent {
            kind: "seal",
            rollout: 0,
            config: 0,
            count: 1,
            cut_moment: 100,
            event_moment: 20,
        })
    );
}

/// J1 repro 2 (consonance): a child forked at Moment 20 accepted a seal
/// claiming Moment 15 — before the machine existed. The excluded-event rule
/// cannot catch it (the excluded event belongs to the parent), which is
/// exactly why the birth-Moment rule exists.
#[test]
fn seal_before_rollout_birth_moment_rejected() {
    let (mut fx, _) = fixtures::two_pass();
    fx.lineage.push(LineageRec {
        rev: 2,
        config: 0,
        child: 1,
        parent: 0,
        cut: Cut {
            moment: 20,
            count: 1,
        },
    });
    fx.seals.push(SealRec {
        rev: 3,
        config: 0,
        rollout: 1,
        seal: 9,
        cut: Cut {
            moment: 15,
            count: 1,
        },
    });
    assert_eq!(
        fx.validate(),
        Err(ValidationError::CutBeforeBirth {
            kind: "seal",
            rollout: 1,
            config: 0,
            count: 1,
            cut_moment: 15,
            birth_moment: 20,
        })
    );
}

/// J4 repro (closer): an assertion under never-declared source 99 minted
/// source-scoped property results, and a `must_hit` declaration under
/// never-declared source 98 minted a FINALIZED absence fact for a schema
/// that does not exist. Both legs now refuse.
#[test]
fn undeclared_assertion_source_rejected() {
    let (mut fx, _) = fixtures::two_pass();
    fx.events.push(SdkEventRec {
        rev: 2,
        config: 0,
        rollout: 0,
        source: 99,
        pos: 3,
        moment: 40,
        payload: Payload::Assertion {
            site: 1,
            property: 500,
            passed: false,
        },
    });
    assert_eq!(
        fx.validate(),
        Err(ValidationError::UndeclaredSource {
            what: "assertion event",
            config: 0,
            src: 99,
        })
    );
}

#[test]
fn undeclared_property_declaration_source_rejected() {
    let (mut fx, _) = fixtures::two_pass();
    fx.properties.push(PropertyDecl {
        rev: 1,
        config: 0,
        source: 98,
        property: 501,
        must_hit: true,
    });
    fx.finalizations.push(FinalizeRec { rev: 6, config: 0 });
    assert_eq!(
        fx.validate(),
        Err(ValidationError::UndeclaredSource {
            what: "property declaration",
            config: 0,
            src: 98,
        })
    );
}

/// J4 declaration-before-use: a declared-but-later source is equally
/// refused for both legs (mirrors the sequence-query rule).
#[test]
fn assertion_and_property_before_source_declaration_rejected() {
    let (mut fx, _) = fixtures::retention_properties();
    // Source 1's declaration moves after the fixture's src-1 assertion
    // (revision 2).
    let idx = fx
        .sources
        .iter()
        .position(|s| s.source == 1)
        .expect("source 1 declared");
    fx.sources[idx].rev = 3;
    assert_eq!(
        fx.validate(),
        Err(ValidationError::DeclarationAfterUse {
            what: "assertion event",
            config: 0,
            id: 1,
            decl_rev: 3,
            use_rev: 2,
        })
    );

    // And a property declaration preceding its source's declaration.
    let (mut fx, _) = fixtures::two_pass();
    fx.sources.push(differential_lineage::data::SourceDecl {
        rev: 4,
        config: 0,
        source: 7,
        scope: differential_lineage::data::OrderScope::RolloutGlobal,
    });
    fx.properties.push(PropertyDecl {
        rev: 2,
        config: 0,
        source: 7,
        property: 900,
        must_hit: false,
    });
    assert_eq!(
        fx.validate(),
        Err(ValidationError::DeclarationAfterUse {
            what: "property declaration",
            config: 0,
            id: 7,
            decl_rev: 4,
            use_rev: 2,
        })
    );
}
