// SPDX-License-Identifier: AGPL-3.0-or-later
//! Binary Event-wire ingress: v1 identity/operation preservation without guessing
//! never-fired reducers, the wire-v2 declaration round-trip, typed errors for
//! malformed lengths and mixed operations/shapes, and decode totality.

use explorer::Moment;
use proptest::prelude::*;
use sdk_events::{
    Classification, DeclaredPoint, Expectation, NS_ASSERT, NS_BUGGIFY, NS_LIFECYCLE, NS_STATE,
    ObservationId, Payload, SdkError, SourceFormat, UpdateOp, ValueShape, decode_binary,
    encode_v2_declaration,
};

// --- wire-byte builders (mirror `guest/sdk/src/wire.rs`; the canonical v1 side) ---

const CATALOG_MAGIC: u32 = u32::from_le_bytes(*b"SDKC");

fn v1_catalog(points: &[(u8, u32, &str)]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&CATALOG_MAGIC.to_le_bytes());
    b.push(1); // version 1
    b.extend_from_slice(&(points.len() as u32).to_le_bytes());
    for (kind, local, name) in points {
        b.push(*kind);
        b.extend_from_slice(&local.to_le_bytes());
        b.extend_from_slice(&(name.len() as u16).to_le_bytes());
        b.extend_from_slice(name.as_bytes());
    }
    b
}

fn event_id(ns: u8, local: u32) -> u32 {
    ((ns as u32) << 24) | (local & 0x00FF_FFFF)
}

fn state_firing(op: u8, value: u64) -> Vec<u8> {
    let mut b = vec![op];
    b.extend_from_slice(&value.to_le_bytes());
    b
}

fn assert_firing(disp: u8, detail: &[u8]) -> Vec<u8> {
    let mut b = vec![disp];
    b.extend_from_slice(&(detail.len() as u16).to_le_bytes());
    b.extend_from_slice(detail);
    b
}

// Kind bytes from the v1 catalog format.
const KIND_ALWAYS: u8 = 0;
const KIND_STATE: u8 = 4;
// State firing op bytes (aligned with `UpdateOp`'s wire bytes).
const STATE_SET: u8 = 0;
const STATE_MAX: u8 = 1;
const STATE_MIN: u8 = 2;
const STATE_ACCUMULATE: u8 = 3;
// Assertion dispositions.
const DISP_HIT: u8 = 0;
const DISP_VIOLATION: u8 = 1;
// v1 catalog point-kind bytes.
const KIND_SOMETIMES: u8 = 1;
const KIND_REACHABLE: u8 = 2;
const KIND_UNREACHABLE: u8 = 3;
const KIND_BUGGIFY: u8 = 5;
// The 24-bit local-id ceiling.
const LOCAL_MAX: u32 = 0x00FF_FFFF;

fn at(m: u64, id: u32, bytes: Vec<u8>) -> (Moment, u32, Vec<u8>) {
    (Moment(m), id, bytes)
}

fn encode_ok(points: &[DeclaredPoint]) -> Vec<u8> {
    encode_v2_declaration(points).expect("valid v2 declaration")
}

// --- v1: never-fired state is reportable but non-reducible -------------------

#[test]
fn v1_never_fired_state_is_reportable_but_not_reducible() {
    let raw = vec![at(0, 0, v1_catalog(&[(KIND_STATE, 7, "leader_term")]))];
    let n = decode_binary(&raw).expect("decodes");

    assert_eq!(n.schema.source, SourceFormat::BinaryV1);
    let id = ObservationId::Point {
        namespace: NS_STATE,
        local: 7,
    };
    let entry = n.schema.entry(&id).expect("declared");
    assert_eq!(entry.classification, Classification::State);
    assert_eq!(entry.base_op, None, "v1 declares no base op — unresolved");
    assert_eq!(
        entry.value_shape, None,
        "v1 declares no value shape either — not invented as U64"
    );
    assert!(
        !entry.is_reducible_state(),
        "an unresolved v1 state point must not be reducible"
    );
    assert!(n.events.is_empty(), "declaration is schema, not an event");
    // The original declaration is recoverable.
    assert!(n.schema.original_declaration.is_some());
}

#[test]
fn v1_fired_state_preserves_its_operation_without_promoting_the_schema() {
    let raw = vec![
        at(0, 0, v1_catalog(&[(KIND_STATE, 7, "leader_term")])),
        at(10, event_id(NS_STATE, 7), state_firing(STATE_MAX, 3)),
        at(20, event_id(NS_STATE, 7), state_firing(STATE_MAX, 9)),
    ];
    let n = decode_binary(&raw).expect("decodes");

    // The fired operation rides the event…
    assert_eq!(n.events.len(), 2);
    assert_eq!(
        n.events[0].payload,
        Payload::State {
            op: UpdateOp::Max,
            value: 3
        }
    );
    // …but the schema base op stays unresolved: v1 firings never bless a reducer.
    let id = ObservationId::Point {
        namespace: NS_STATE,
        local: 7,
    };
    assert_eq!(n.schema.entry(&id).unwrap().base_op, None);
}

#[test]
fn v1_mixed_operations_for_one_identity_are_malformed_evidence() {
    let raw = vec![
        at(0, 0, v1_catalog(&[(KIND_STATE, 7, "reg")])),
        at(10, event_id(NS_STATE, 7), state_firing(STATE_SET, 1)),
        at(20, event_id(NS_STATE, 7), state_firing(STATE_MAX, 2)),
    ];
    let err = decode_binary(&raw).expect_err("mixed ops must fail");
    assert!(matches!(
        err,
        SdkError::MixedOperations {
            first: UpdateOp::Set,
            second: UpdateOp::Max,
            ..
        }
    ));
}

#[test]
fn v1_assert_firing_carries_the_declared_verb_and_condition() {
    // A `sometimes` point validly emits a HIT when satisfied.
    let raw = vec![
        at(0, 0, v1_catalog(&[(KIND_SOMETIMES, 3, "progress")])),
        at(5, event_id(NS_ASSERT, 3), assert_firing(DISP_HIT, b"ok")),
    ];
    let n = decode_binary(&raw).expect("decodes");
    let ev = &n.events[0];
    assert_eq!(
        ev.id,
        ObservationId::Point {
            namespace: NS_ASSERT,
            local: 3
        }
    );
    match &ev.payload {
        Payload::Assertion {
            assert_type,
            condition,
        } => {
            assert_eq!(*assert_type, Some(sdk_events::AssertType::Sometimes));
            assert_eq!(*condition, Some(true));
        }
        other => panic!("expected assertion, got {other:?}"),
    }
}

#[test]
fn v1_sometimes_point_keeps_its_must_hit_expectation() {
    // Unlike `always`, a `sometimes` emits a hit when satisfied, so never-firing is
    // a genuine coverage gap — the must-hit expectation is correct here.
    let raw = vec![at(0, 0, v1_catalog(&[(KIND_SOMETIMES, 4, "reached")]))];
    let n = decode_binary(&raw).expect("decodes");
    let id = ObservationId::Point {
        namespace: NS_ASSERT,
        local: 4,
    };
    assert_eq!(
        n.schema.entry(&id).unwrap().expectation,
        Some(Expectation::MustHit)
    );
}

#[test]
fn v1_assertion_dispositions_must_match_the_declared_kind() {
    // The guest SDK emits HITs only for sometimes/reachable and VIOLATIONs only for
    // always/unreachable. A kind-inconsistent disposition is a forged/malformed
    // record — kept raw, never normalized as a credible assertion. Each VALID pair
    // decodes; each WRONG pair is preserved raw.
    use sdk_events::AssertType;
    let cases = [
        (
            KIND_SOMETIMES,
            DISP_HIT,
            Some((AssertType::Sometimes, true)),
        ),
        (KIND_SOMETIMES, DISP_VIOLATION, None),
        (
            KIND_REACHABLE,
            DISP_HIT,
            Some((AssertType::Reachable, true)),
        ),
        (KIND_REACHABLE, DISP_VIOLATION, None),
        (
            KIND_ALWAYS,
            DISP_VIOLATION,
            Some((AssertType::Always, false)),
        ),
        (KIND_ALWAYS, DISP_HIT, None),
        (
            KIND_UNREACHABLE,
            DISP_VIOLATION,
            Some((AssertType::Unreachable, false)),
        ),
        (KIND_UNREACHABLE, DISP_HIT, None),
    ];
    for (kind, disp, expected) in cases {
        let raw = vec![
            at(0, 0, v1_catalog(&[(kind, 7, "p")])),
            at(1, event_id(NS_ASSERT, 7), assert_firing(disp, b"")),
        ];
        let n = decode_binary(&raw).expect("decodes");
        match expected {
            Some((at, cond)) => assert_eq!(
                n.events[0].payload,
                Payload::Assertion {
                    assert_type: Some(at),
                    condition: Some(cond)
                },
                "kind {kind} + disp {disp} should decode"
            ),
            None => assert_eq!(
                n.events[0].payload,
                Payload::Unknown,
                "kind {kind} + disp {disp} is inconsistent → raw"
            ),
        }
    }
}

#[test]
fn a_v1_stream_does_not_normalize_v2_only_operations() {
    // op bytes 2/3 (min/accumulate) are wire-v2 firing extensions; under a v1 (or
    // declaration-less) stream they are unknown bytes and stay raw.
    for op_byte in [STATE_MIN, STATE_ACCUMULATE] {
        let raw = vec![
            at(0, 0, v1_catalog(&[(KIND_STATE, 7, "reg")])),
            at(1, event_id(NS_STATE, 7), state_firing(op_byte, 5)),
        ];
        let n = decode_binary(&raw).expect("decodes");
        assert_eq!(
            n.events[0].payload,
            Payload::Unknown,
            "op {op_byte} is not a v1 state op — preserve raw, don't fabricate a state update"
        );
    }
}

// --- wire v2: declarations resolve semantics and round-trip before firing ----

fn v2_state(local: u32, name: &str, op: UpdateOp) -> DeclaredPoint {
    DeclaredPoint {
        namespace: NS_STATE,
        local,
        name: name.to_string(),
        classification: Classification::State,
        value_shape: Some(ValueShape::U64),
        base_op: Some(op),
        expectation: None,
    }
}

#[test]
fn v2_set_max_min_declarations_round_trip_before_firing() {
    let points = vec![
        v2_state(1, "commit_index", UpdateOp::Set),
        v2_state(2, "high_watermark", UpdateOp::Max),
        v2_state(3, "low_watermark", UpdateOp::Min),
    ];
    let decl = encode_ok(&points);
    let n = decode_binary(&[at(0, 0, decl)]).expect("decodes");

    assert_eq!(n.schema.source, SourceFormat::BinaryV2);
    assert!(n.events.is_empty(), "no firings yet");
    // Every declared op is resolved and reducible before a single event fires.
    for p in &points {
        let id = ObservationId::Point {
            namespace: p.namespace,
            local: p.local,
        };
        let entry = n.schema.entry(&id).expect("declared");
        assert_eq!(entry.base_op, p.base_op);
        assert_eq!(entry.value_shape, p.value_shape);
        assert!(entry.is_reducible_state());
        assert_eq!(entry.name.as_deref(), Some(p.name.as_str()));
    }
}

#[test]
fn v2_accumulate_is_declarable_by_a_versioned_source() {
    // `accumulate` requires a source verb/version that declares it — v2 can (over
    // the u64 the binary emission path encodes).
    let p = v2_state(5, "species", UpdateOp::Accumulate);
    let n = decode_binary(&[at(0, 0, encode_ok(&[p]))]).expect("decodes");
    let id = ObservationId::Point {
        namespace: NS_STATE,
        local: 5,
    };
    assert_eq!(
        n.schema.entry(&id).unwrap().base_op,
        Some(UpdateOp::Accumulate)
    );
}

/// The firing codec honors **every** declared operation, not just set/max, and a
/// firing decodes to the same op it was declared with (finding-2 round-trip).
#[test]
fn every_declared_operation_fires_and_decodes_consistently() {
    for (op, op_byte) in [
        (UpdateOp::Set, STATE_SET),
        (UpdateOp::Max, STATE_MAX),
        (UpdateOp::Min, STATE_MIN),
        (UpdateOp::Accumulate, STATE_ACCUMULATE),
    ] {
        let decl = encode_ok(&[v2_state(1, "reg", op)]);
        let raw = vec![
            at(0, 0, decl),
            at(1, event_id(NS_STATE, 1), state_firing(op_byte, 42)),
        ];
        let n = decode_binary(&raw).unwrap_or_else(|e| panic!("{op:?} should decode: {e}"));
        assert_eq!(
            n.events[0].payload,
            Payload::State { op, value: 42 },
            "{op:?} firing must decode to its declared op"
        );
    }
}

#[test]
fn v2_firing_conflicting_with_the_declared_operation_is_rejected() {
    let decl = encode_ok(&[v2_state(2, "hw", UpdateOp::Max)]);
    let raw = vec![
        at(0, 0, decl),
        // The declaration says max; a set firing contradicts it.
        at(9, event_id(NS_STATE, 2), state_firing(STATE_SET, 4)),
    ];
    let err = decode_binary(&raw).expect_err("declared-op conflict must fail");
    assert!(matches!(err, SdkError::MixedOperations { .. }));
}

#[test]
fn v2_non_u64_state_shape_is_rejected_on_encode_and_decode() {
    // The binary emission path encodes only u64 state values, so a state point
    // declaring any other shape is refused rather than silently reported as u64.
    let mut p = v2_state(1, "x", UpdateOp::Set);
    p.value_shape = Some(ValueShape::Bytes);
    let err = encode_v2_declaration(&[p]).expect_err("non-u64 state shape must fail on encode");
    assert!(matches!(err, SdkError::UnsupportedDeclaration { .. }));

    // …and a hand-built declaration carrying that shape is rejected on decode too.
    let mut decl = Vec::new();
    decl.extend_from_slice(&CATALOG_MAGIC.to_le_bytes());
    decl.push(2); // version 2
    decl.extend_from_slice(&1u32.to_le_bytes());
    decl.push(NS_STATE);
    decl.extend_from_slice(&1u32.to_le_bytes());
    decl.push(1); // classification: state
    decl.push(2); // value shape: bytes (byte 2) — unsupported for binary state
    decl.push(0); // base op: set (byte 0)
    decl.push(255); // expectation: none
    decl.extend_from_slice(&1u16.to_le_bytes());
    decl.extend_from_slice(b"x");
    let err = decode_binary(&[at(0, 0, decl)]).expect_err("must fail on decode");
    assert!(matches!(err, SdkError::UnsupportedDeclaration { .. }));
}

#[test]
fn v2_occurrence_carrying_a_value_or_operation_is_rejected() {
    let mut p = DeclaredPoint {
        namespace: NS_ASSERT,
        local: 1,
        name: "a".into(),
        classification: Classification::Occurrence,
        value_shape: Some(ValueShape::U64), // an occurrence has no reducible value
        base_op: None,
        expectation: None,
    };
    assert!(matches!(
        encode_v2_declaration(&[p.clone()]),
        Err(SdkError::UnsupportedDeclaration { .. })
    ));
    p.value_shape = None;
    p.base_op = Some(UpdateOp::Set); // …and no base operation
    assert!(matches!(
        encode_v2_declaration(&[p]),
        Err(SdkError::UnsupportedDeclaration { .. })
    ));
}

#[test]
fn v2_classification_must_agree_with_the_namespace() {
    // A state point at an assert namespace would decode as an assertion, not state;
    // an occurrence at the state namespace would decode as state — both refused so
    // schema and event evidence cannot disagree.
    let mut state_at_assert = v2_state(1, "x", UpdateOp::Set);
    state_at_assert.namespace = NS_ASSERT;
    assert!(matches!(
        encode_v2_declaration(&[state_at_assert]),
        Err(SdkError::UnsupportedDeclaration { .. })
    ));

    let occ_at_state = DeclaredPoint {
        namespace: NS_STATE,
        local: 1,
        name: "x".into(),
        classification: Classification::Occurrence,
        value_shape: None,
        base_op: None,
        expectation: None,
    };
    assert!(matches!(
        encode_v2_declaration(&[occ_at_state]),
        Err(SdkError::UnsupportedDeclaration { .. })
    ));
}

#[test]
fn v2_duplicate_coordinate_is_rejected_on_encode_and_decode() {
    // Two points at one runtime coordinate — a firing cannot distinguish them, so
    // the declaration is refused rather than silently collapsing one away.
    let a = v2_state(1, "commit_index", UpdateOp::Set);
    let b = v2_state(1, "other_name", UpdateOp::Set); // same (namespace, local)
    let err = encode_v2_declaration(&[a, b]).expect_err("duplicate coord must fail on encode");
    assert!(matches!(err, SdkError::DuplicateCoordinate { .. }));

    // Hand-build a declaration with two records at the same coordinate to exercise
    // the decode-side check (encode won't emit one).
    let mut decl = Vec::new();
    decl.extend_from_slice(&CATALOG_MAGIC.to_le_bytes());
    decl.push(2);
    decl.extend_from_slice(&2u32.to_le_bytes()); // 2 records…
    for name in ["a", "b"] {
        decl.push(NS_STATE);
        decl.extend_from_slice(&7u32.to_le_bytes()); // …at the same local
        decl.push(1); // state
        decl.push(0); // u64
        decl.push(0); // set
        decl.push(255); // no expectation
        decl.extend_from_slice(&(name.len() as u16).to_le_bytes());
        decl.extend_from_slice(name.as_bytes());
    }
    let err = decode_binary(&[at(0, 0, decl)]).expect_err("duplicate coord must fail on decode");
    assert!(matches!(err, SdkError::DuplicateCoordinate { .. }));
}

#[test]
fn v2_lifecycle_declaration_is_restricted_to_the_decodable_local() {
    // Only the setup_complete point (local 0) has a decodable lifecycle firing; a
    // lifecycle declaration at any other local would decode to Unknown, so it is
    // refused (declaration/emission agreement).
    let occ = |local: u32| DeclaredPoint {
        namespace: NS_LIFECYCLE,
        local,
        name: "life".into(),
        classification: Classification::Occurrence,
        value_shape: None,
        base_op: None,
        expectation: None,
    };
    assert!(
        encode_v2_declaration(&[occ(0)]).is_ok(),
        "local 0 is reportable"
    );
    assert!(matches!(
        encode_v2_declaration(&[occ(1)]),
        Err(SdkError::UnsupportedDeclaration { .. })
    ));
}

#[test]
fn v1_duplicate_coordinate_is_rejected() {
    // Two assertion kinds at one local id: the guest SDK rejects such a catalog,
    // and so does the host decoder — collapsing them would normalize a verb from
    // one kind under an expectation from another.
    const KIND_SOMETIMES: u8 = 1;
    const KIND_UNREACHABLE: u8 = 3;
    let raw = vec![at(
        0,
        0,
        v1_catalog(&[(KIND_SOMETIMES, 5, "a"), (KIND_UNREACHABLE, 5, "b")]),
    )];
    let err = decode_binary(&raw).expect_err("duplicate v1 coordinate must fail");
    assert!(matches!(err, SdkError::DuplicateCoordinate { .. }));
}

#[test]
fn v2_oversized_name_is_rejected_not_truncated() {
    let mut p = v2_state(1, "x", UpdateOp::Set);
    p.name = "n".repeat(u16::MAX as usize + 1);
    let err = encode_v2_declaration(&[p]).expect_err("oversized name must fail");
    assert!(matches!(err, SdkError::NameTooLong { .. }));
}

#[test]
fn a_stream_with_two_catalog_declarations_is_rejected() {
    // The second control-0 record (here a future-version claim) must not be
    // silently ignored while events decode under the first.
    let first = v1_catalog(&[(KIND_STATE, 1, "reg")]);
    let mut second = Vec::new();
    second.extend_from_slice(&CATALOG_MAGIC.to_le_bytes());
    second.push(99); // a future version
    second.extend_from_slice(&0u32.to_le_bytes());
    let raw = vec![at(0, 0, first), at(1, 0, second)];
    let err = decode_binary(&raw).expect_err("two declarations must fail");
    assert!(matches!(err, SdkError::MultipleDeclarations { count: 2 }));
}

#[test]
fn a_declaration_after_a_firing_is_rejected() {
    // A `min` firing precedes a v2 catalog. Applying the catalog would retroactively
    // reinterpret that prior byte as a v2 `Min` update (it is unknown in a
    // declaration-less stream). The declaration must come first.
    let decl = encode_ok(&[v2_state(7, "reg", UpdateOp::Min)]);
    let raw = vec![
        at(0, event_id(NS_STATE, 7), state_firing(STATE_MIN, 5)), // firing first
        at(1, 0, decl),                                           // declaration after
    ];
    let err = decode_binary(&raw).expect_err("declaration after firing must fail");
    assert!(matches!(
        err,
        SdkError::DeclarationAfterFirings { firings_before: 1 }
    ));

    // The same holds for a v1 catalog arriving after a firing.
    let raw = vec![
        at(0, event_id(NS_STATE, 7), state_firing(STATE_SET, 1)),
        at(1, 0, v1_catalog(&[(KIND_STATE, 7, "reg")])),
    ];
    assert!(matches!(
        decode_binary(&raw),
        Err(SdkError::DeclarationAfterFirings { .. })
    ));
}

#[test]
fn a_declaration_before_its_firings_decodes_normally() {
    // The correct ordering: declaration first, then the min firing decodes as Min.
    let decl = encode_ok(&[v2_state(7, "reg", UpdateOp::Min)]);
    let raw = vec![
        at(0, 0, decl),
        at(1, event_id(NS_STATE, 7), state_firing(STATE_MIN, 5)),
    ];
    let n = decode_binary(&raw).expect("decodes");
    assert_eq!(
        n.events[0].payload,
        Payload::State {
            op: UpdateOp::Min,
            value: 5
        }
    );
}

#[test]
fn a_catalog_with_trailing_bytes_past_its_count_is_rejected() {
    // v2: count 0 but a full record follows — the trailing bytes would be silently
    // dropped, omitting a declared identity.
    let mut v2 = Vec::new();
    v2.extend_from_slice(&CATALOG_MAGIC.to_le_bytes());
    v2.push(2);
    v2.extend_from_slice(&0u32.to_le_bytes()); // count 0…
    v2.push(NS_STATE); // …but a complete v2 record follows
    v2.extend_from_slice(&1u32.to_le_bytes());
    v2.push(1); // state
    v2.push(0); // u64
    v2.push(0); // set
    v2.push(255); // no expectation
    v2.extend_from_slice(&1u16.to_le_bytes());
    v2.extend_from_slice(b"x");
    // The record is 12 bytes (ns 1 + local 4 + class/shape/op/expect 4 + len 2 +
    // name 1); `extra` reports exactly those, pinning `Reader::remaining`.
    assert_eq!(
        decode_binary(&[at(0, 0, v2)]),
        Err(SdkError::TrailingDeclarationBytes {
            context: "v2 catalog",
            extra: 12,
        })
    );

    // v1: count 0 but a full record follows (kind 1 + local 4 + len 2 + "reg" 3).
    let mut v1 = Vec::new();
    v1.extend_from_slice(&CATALOG_MAGIC.to_le_bytes());
    v1.push(1);
    v1.extend_from_slice(&0u32.to_le_bytes()); // count 0…
    v1.push(KIND_STATE); // …but a complete v1 record follows
    v1.extend_from_slice(&7u32.to_le_bytes());
    v1.extend_from_slice(&3u16.to_le_bytes());
    v1.extend_from_slice(b"reg");
    assert_eq!(
        decode_binary(&[at(0, 0, v1)]),
        Err(SdkError::TrailingDeclarationBytes {
            context: "v1 catalog",
            extra: 10,
        })
    );
}

// --- v1 kinds, dispositions, lifecycle, and boundaries -----------------------

#[test]
fn v1_local_id_boundary_is_enforced() {
    // The maximal 24-bit id is accepted…
    let ok = decode_binary(&[at(0, 0, v1_catalog(&[(KIND_STATE, LOCAL_MAX, "reg")]))]);
    assert!(ok.is_ok());
    // …and one past it is refused (a firing's coordinate is masked to 24 bits, so
    // this identity could never fire).
    let over = LOCAL_MAX + 1;
    let mut decl = Vec::new();
    decl.extend_from_slice(&CATALOG_MAGIC.to_le_bytes());
    decl.push(1);
    decl.extend_from_slice(&1u32.to_le_bytes());
    decl.push(KIND_STATE);
    decl.extend_from_slice(&over.to_le_bytes());
    decl.extend_from_slice(&3u16.to_le_bytes());
    decl.extend_from_slice(b"reg");
    assert!(matches!(
        decode_binary(&[at(0, 0, decl)]),
        Err(SdkError::LocalIdOutOfRange { .. })
    ));
}

#[test]
fn v1_reachable_point_is_declared_and_its_firing_carries_the_verb() {
    let raw = vec![
        at(0, 0, v1_catalog(&[(KIND_REACHABLE, 9, "reached")])),
        at(1, event_id(NS_ASSERT, 9), assert_firing(DISP_HIT, b"")),
    ];
    let n = decode_binary(&raw).expect("decodes");
    // The declared point is in the schema (occurrence, must-hit)…
    let id = ObservationId::Point {
        namespace: NS_ASSERT,
        local: 9,
    };
    let entry = n.schema.entry(&id).expect("reachable point declared");
    assert_eq!(entry.classification, Classification::Occurrence);
    assert_eq!(entry.expectation, Some(Expectation::MustHit));
    // …and its firing carries the `reachable` verb.
    match n.events[0].payload {
        Payload::Assertion { assert_type, .. } => {
            assert_eq!(assert_type, Some(sdk_events::AssertType::Reachable));
        }
        ref other => panic!("{other:?}"),
    }
}

#[test]
fn v1_buggify_point_is_declared_as_an_occurrence() {
    let raw = vec![at(0, 0, v1_catalog(&[(KIND_BUGGIFY, 4, "flip")]))];
    let n = decode_binary(&raw).expect("decodes");
    let id = ObservationId::Point {
        namespace: NS_BUGGIFY,
        local: 4,
    };
    let entry = n.schema.entry(&id).expect("buggify point declared");
    assert_eq!(entry.classification, Classification::Occurrence);
    assert_eq!(entry.expectation, None);
}

#[test]
fn v1_assert_violation_firing_reports_a_false_condition() {
    let raw = vec![
        at(0, 0, v1_catalog(&[(KIND_ALWAYS, 3, "inv")])),
        at(
            5,
            event_id(NS_ASSERT, 3),
            assert_firing(DISP_VIOLATION, b"boom"),
        ),
    ];
    let n = decode_binary(&raw).expect("decodes");
    assert_eq!(
        n.events[0].payload,
        Payload::Assertion {
            assert_type: Some(sdk_events::AssertType::Always),
            condition: Some(false)
        }
    );
    // An `always` point carries NO expectation on this wire: `guest/sdk` emits only
    // violations, so a passing always produces no event and must not read as an
    // unsatisfied must-hit.
    let id = ObservationId::Point {
        namespace: NS_ASSERT,
        local: 3,
    };
    assert_eq!(n.schema.entry(&id).unwrap().expectation, None);
}

#[test]
fn binary_lifecycle_only_decodes_setup_complete_at_local_zero_empty() {
    // The setup_complete point (local 0, empty payload) decodes to a lifecycle.
    let n = decode_binary(&[at(0, event_id(NS_LIFECYCLE, 0), vec![])]).expect("decodes");
    assert_eq!(
        n.events[0].payload,
        Payload::Lifecycle {
            name: "setup_complete".into()
        }
    );
    // A non-empty payload at local 0 is not setup_complete → raw.
    let n = decode_binary(&[at(0, event_id(NS_LIFECYCLE, 0), vec![1])]).expect("decodes");
    assert_eq!(n.events[0].payload, Payload::Unknown);
    // A different lifecycle local (even empty) is not decodable → raw.
    let n = decode_binary(&[at(0, event_id(NS_LIFECYCLE, 5), vec![])]).expect("decodes");
    assert_eq!(n.events[0].payload, Payload::Unknown);
}

#[test]
fn a_name_exactly_at_the_u16_limit_encodes() {
    // The `>` boundary: a name of exactly u16::MAX bytes is encodable, not too long.
    let mut p = v2_state(1, "x", UpdateOp::Set);
    p.name = "n".repeat(u16::MAX as usize);
    assert!(encode_v2_declaration(&[p]).is_ok());
}

#[test]
fn v2_bool_and_numeric_state_shapes_are_unsupported_declarations() {
    // A state point declaring shape byte 1 (bool) or 3 (numeric) decodes the shape
    // (so `from_byte` recognizes it) and is then rejected as unsupported — not an
    // UnknownDeclarationByte, which is what a dropped `from_byte` arm would yield.
    for shape_byte in [1u8, 3u8] {
        let mut decl = Vec::new();
        decl.extend_from_slice(&CATALOG_MAGIC.to_le_bytes());
        decl.push(2);
        decl.extend_from_slice(&1u32.to_le_bytes());
        decl.push(NS_STATE);
        decl.extend_from_slice(&1u32.to_le_bytes());
        decl.push(1); // classification: state
        decl.push(shape_byte); // value shape: bool / numeric
        decl.push(0); // base op: set
        decl.push(255); // no expectation
        decl.extend_from_slice(&1u16.to_le_bytes());
        decl.extend_from_slice(b"x");
        assert!(
            matches!(
                decode_binary(&[at(0, 0, decl)]),
                Err(SdkError::UnsupportedDeclaration { .. })
            ),
            "shape byte {shape_byte} must decode then be rejected as unsupported"
        );
    }
}

// --- unsupported version + out-of-range ids are typed errors -----------------

#[test]
fn a_future_catalog_version_is_refused_not_decoded_under_a_guessed_layout() {
    let mut decl = Vec::new();
    decl.extend_from_slice(&CATALOG_MAGIC.to_le_bytes());
    decl.push(99); // a future/unknown version
    decl.extend_from_slice(&0u32.to_le_bytes());
    // A following state firing must NOT be decoded under this decoder's layout.
    let raw = vec![
        at(0, 0, decl),
        at(1, event_id(NS_STATE, 1), state_firing(STATE_SET, 7)),
    ];
    let err = decode_binary(&raw).expect_err("future version must be refused");
    assert!(matches!(err, SdkError::UnsupportedVersion { version: 99 }));
}

#[test]
fn out_of_range_local_ids_are_rejected_on_encode_and_decode() {
    // The maximal 24-bit id is fine.
    assert!(encode_v2_declaration(&[v2_state(LOCAL_MAX, "ok", UpdateOp::Set)]).is_ok());
    // One past it can never match a masked firing coordinate — refused on encode…
    let err = encode_v2_declaration(&[v2_state(LOCAL_MAX + 1, "bad", UpdateOp::Set)])
        .expect_err("out-of-range local must fail on encode");
    assert!(matches!(err, SdkError::LocalIdOutOfRange { .. }));

    // …and on decode, from a hand-built declaration.
    let mut decl = Vec::new();
    decl.extend_from_slice(&CATALOG_MAGIC.to_le_bytes());
    decl.push(2);
    decl.extend_from_slice(&1u32.to_le_bytes());
    decl.push(NS_STATE);
    decl.extend_from_slice(&(LOCAL_MAX + 1).to_le_bytes());
    decl.push(1); // classification: state
    decl.push(0); // value shape: u64 (byte 0)
    decl.push(0); // base op: set (byte 0)
    decl.push(255); // expectation: none
    decl.extend_from_slice(&3u16.to_le_bytes());
    decl.extend_from_slice(b"bad");
    let err = decode_binary(&[at(0, 0, decl)]).expect_err("out-of-range local must fail on decode");
    assert!(matches!(err, SdkError::LocalIdOutOfRange { .. }));
}

// --- malformed lengths are typed errors, never panics ------------------------

#[test]
fn truncated_v2_record_is_a_malformed_length_error() {
    let decl = encode_ok(&[v2_state(1, "commit_index", UpdateOp::Set)]);
    // Cut the declaration off inside the sole record (after the header).
    let truncated = decl[..decl.len() - 5].to_vec();
    let err = decode_binary(&[at(0, 0, truncated)]).expect_err("truncation must fail");
    assert!(matches!(err, SdkError::MalformedLength { .. }));
}

#[test]
fn v1_catalog_claiming_more_records_than_it_carries_is_a_malformed_length_error() {
    let mut decl = Vec::new();
    decl.extend_from_slice(&CATALOG_MAGIC.to_le_bytes());
    decl.push(1);
    decl.extend_from_slice(&2u32.to_le_bytes()); // claims 2 records…
    // …but carries only one.
    decl.push(KIND_STATE);
    decl.extend_from_slice(&7u32.to_le_bytes());
    decl.extend_from_slice(&3u16.to_le_bytes());
    decl.extend_from_slice(b"reg");
    let err = decode_binary(&[at(0, 0, decl)]).expect_err("under-length must fail");
    assert!(matches!(err, SdkError::MalformedLength { .. }));
}

#[test]
fn a_garbled_header_is_lenient_not_an_error() {
    // Bad magic → no usable declaration; events still decode, schema is empty.
    let raw = vec![
        at(0, 0, vec![0xDE, 0xAD, 0xBE, 0xEF, 9, 9]),
        at(1, event_id(NS_STATE, 1), state_firing(STATE_SET, 1)),
    ];
    let n = decode_binary(&raw).expect("lenient");
    assert!(n.schema.is_empty());
    assert_eq!(n.events.len(), 1);
}

// --- unknown data is preserved raw, and decode is total ----------------------

#[test]
fn unknown_namespace_event_is_preserved_raw() {
    let bytes = vec![1, 2, 3, 4];
    let raw = vec![at(0, event_id(9, 5), bytes.clone())];
    let n = decode_binary(&raw).expect("decodes");
    assert_eq!(n.events[0].payload, Payload::Unknown);
    assert_eq!(n.events[0].raw.bytes, bytes);
    assert_eq!(n.events[0].raw.event_id, Some(event_id(9, 5)));
}

#[test]
fn buggify_firing_decodes_as_an_occurrence() {
    let raw = vec![at(0, event_id(NS_BUGGIFY, 2), vec![1])];
    let n = decode_binary(&raw).expect("decodes");
    assert_eq!(n.events[0].payload, Payload::Buggify { fired: true });
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Decode is total and panic-free over arbitrary `(Moment, id, bytes)` streams.
    #[test]
    fn decode_binary_never_panics(
        stream in prop::collection::vec(
            (any::<u64>(), any::<u32>(), prop::collection::vec(any::<u8>(), 0..48)),
            0..24,
        )
    ) {
        let raw: Vec<_> = stream.into_iter().map(|(m, id, b)| (Moment(m), id, b)).collect();
        // Either a clean normalization or a typed error — never a panic, and every
        // emitted event carries its raw bytes back.
        if let Ok(n) = decode_binary(&raw) {
            for ev in &n.events {
                prop_assert!(ev.raw.event_id.is_some());
            }
        }
    }

    /// Declared-vs-decoded round-trip for every operation and any in-range id: the
    /// decoded schema resolves the declared op and a firing under it decodes to the
    /// same op (finding-2 completeness).
    #[test]
    fn v2_state_declarations_round_trip_for_every_op(
        op_idx in 0usize..4,
        local in 0u32..=LOCAL_MAX,
        value in any::<u64>(),
    ) {
        let ops = [UpdateOp::Set, UpdateOp::Max, UpdateOp::Min, UpdateOp::Accumulate];
        let op_bytes = [STATE_SET, STATE_MAX, STATE_MIN, STATE_ACCUMULATE];
        let op = ops[op_idx];
        let decl = encode_v2_declaration(&[v2_state(local, "r", op)]).expect("valid");
        let raw = vec![
            at(0, 0, decl),
            at(1, event_id(NS_STATE, local), state_firing(op_bytes[op_idx], value)),
        ];
        let n = decode_binary(&raw).expect("decodes");
        let id = ObservationId::Point { namespace: NS_STATE, local };
        let entry = n.schema.entry(&id).expect("declared");
        prop_assert_eq!(entry.base_op, Some(op));
        prop_assert!(entry.is_reducible_state());
        prop_assert_eq!(&n.events[0].payload, &Payload::State { op, value });
    }

    /// The 24-bit local-id boundary is enforced on encode across the whole u32.
    #[test]
    fn local_id_boundary_is_enforced(local in any::<u32>()) {
        let r = encode_v2_declaration(&[v2_state(local, "r", UpdateOp::Set)]);
        if local <= LOCAL_MAX {
            prop_assert!(r.is_ok());
        } else {
            let out_of_range = matches!(r, Err(SdkError::LocalIdOutOfRange { .. }));
            prop_assert!(out_of_range);
        }
    }

    /// A random unrecognized catalog version is refused, never decoded under a
    /// guessed layout; only the two known versions decode.
    #[test]
    fn unknown_catalog_versions_are_refused(version in any::<u8>()) {
        let mut decl = Vec::new();
        decl.extend_from_slice(&CATALOG_MAGIC.to_le_bytes());
        decl.push(version);
        decl.extend_from_slice(&0u32.to_le_bytes()); // count 0
        let raw = vec![
            at(0, 0, decl),
            at(1, event_id(NS_STATE, 1), state_firing(STATE_SET, 1)),
        ];
        let r = decode_binary(&raw);
        match version {
            1 | 2 => prop_assert!(r.is_ok()),
            _ => {
                let unsupported = matches!(r, Err(SdkError::UnsupportedVersion { .. }));
                prop_assert!(unsupported);
            }
        }
    }

    /// A v2 declaration is accepted iff its classification matches the one the
    /// namespace's firings actually decode to — the same mapping the decoder uses,
    /// so schema and event evidence can never disagree.
    #[test]
    fn namespace_classification_agreement_is_enforced(
        namespace in any::<u8>(),
        is_state in any::<bool>(),
    ) {
        let (classification, value_shape, base_op) = if is_state {
            (Classification::State, Some(ValueShape::U64), Some(UpdateOp::Set))
        } else {
            (Classification::Occurrence, None, None)
        };
        let p = DeclaredPoint {
            namespace,
            // Local 0 is decodable in every reportable namespace (including the
            // sole lifecycle point), so it isolates the namespace/classification
            // check from the lifecycle local-id restriction.
            local: 0,
            name: "p".into(),
            classification,
            value_shape,
            base_op,
            expectation: None,
        };
        let accepted = encode_v2_declaration(&[p]).is_ok();
        // State firings arrive only under NS_STATE; occurrence firings under the
        // assert/buggify/lifecycle namespaces. Every other namespace is refused.
        let should_accept = if namespace == NS_STATE {
            is_state
        } else if namespace == NS_ASSERT || namespace == NS_BUGGIFY || namespace == NS_LIFECYCLE {
            !is_state
        } else {
            false
        };
        prop_assert_eq!(accepted, should_accept);
    }

    /// The emitted declaration is canonical: the same point set in any input order
    /// (e.g. from a `HashMap`) yields byte-identical declaration bytes, so no
    /// host-order nondeterminism reaches the persisted `original_declaration`.
    #[test]
    fn shuffled_declaration_points_encode_identically(
        // A map keys the points by unique local id (values pick the op).
        by_local in prop::collection::btree_map(0u32..=64, 0usize..4, 1..8),
        order in prop::collection::vec(any::<u8>(), 8),
    ) {
        let ops = [UpdateOp::Set, UpdateOp::Max, UpdateOp::Min, UpdateOp::Accumulate];
        let base: Vec<DeclaredPoint> = by_local
            .iter()
            .map(|(&local, &oi)| v2_state(local, "r", ops[oi]))
            .collect();
        // Reorder by an independent key so the input order differs from `base`.
        let mut tagged: Vec<(u8, DeclaredPoint)> = base
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, p)| (order.get(i).copied().unwrap_or(0), p))
            .collect();
        tagged.sort_by_key(|(k, _)| *k);
        let shuffled: Vec<DeclaredPoint> = tagged.into_iter().map(|(_, p)| p).collect();

        prop_assert_eq!(
            encode_v2_declaration(&base).unwrap(),
            encode_v2_declaration(&shuffled).unwrap(),
        );
    }
}
