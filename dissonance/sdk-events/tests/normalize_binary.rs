// SPDX-License-Identifier: AGPL-3.0-or-later
//! Binary Event-wire ingress: v1 identity/operation preservation without guessing
//! never-fired reducers, the wire-v2 declaration round-trip, typed errors for
//! malformed lengths and mixed operations/shapes, and decode totality.

use explorer::Moment;
use proptest::prelude::*;
use sdk_events::{
    Classification, DeclaredPoint, Expectation, NS_ASSERT, NS_BUGGIFY, NS_STATE, ObservationId,
    Payload, SdkError, SourceFormat, UpdateOp, ValueShape, decode_binary, encode_v2_declaration,
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
// Op bytes.
const STATE_SET: u8 = 0;
const STATE_MAX: u8 = 1;
// Assertion dispositions.
const DISP_HIT: u8 = 0;

fn at(m: u64, id: u32, bytes: Vec<u8>) -> (Moment, u32, Vec<u8>) {
    (Moment(m), id, bytes)
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
    let raw = vec![
        at(0, 0, v1_catalog(&[(KIND_ALWAYS, 3, "inv")])),
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
            assert_eq!(*assert_type, Some(sdk_events::AssertType::Always));
            assert_eq!(*condition, Some(true));
        }
        other => panic!("expected assertion, got {other:?}"),
    }
    // The always point declares a must-hit expectation, preserved for reporting.
    let id = ObservationId::Point {
        namespace: NS_ASSERT,
        local: 3,
    };
    assert_eq!(
        n.schema.entry(&id).unwrap().expectation,
        Some(Expectation::MustHit)
    );
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
    let decl = encode_v2_declaration(&points);
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
    // `accumulate` requires a source verb/version that declares it — v2 can.
    let mut p = v2_state(5, "species", UpdateOp::Accumulate);
    p.value_shape = Some(ValueShape::Bytes);
    let n = decode_binary(&[at(0, 0, encode_v2_declaration(&[p]))]).expect("decodes");
    let id = ObservationId::Point {
        namespace: NS_STATE,
        local: 5,
    };
    assert_eq!(
        n.schema.entry(&id).unwrap().base_op,
        Some(UpdateOp::Accumulate)
    );
}

#[test]
fn v2_firing_conflicting_with_the_declared_operation_is_rejected() {
    let decl = encode_v2_declaration(&[v2_state(2, "hw", UpdateOp::Max)]);
    let raw = vec![
        at(0, 0, decl),
        // The declaration says max; a set firing contradicts it.
        at(9, event_id(NS_STATE, 2), state_firing(STATE_SET, 4)),
    ];
    let err = decode_binary(&raw).expect_err("declared-op conflict must fail");
    assert!(matches!(err, SdkError::MixedOperations { .. }));
}

#[test]
fn v2_incompatible_shapes_for_one_identity_are_a_typed_error() {
    let a = v2_state(1, "x", UpdateOp::Set);
    let mut b = v2_state(1, "x", UpdateOp::Set); // same coordinate…
    b.value_shape = Some(ValueShape::Bool); // …different shape
    let err = decode_binary(&[at(0, 0, encode_v2_declaration(&[a, b]))])
        .expect_err("shape conflict must fail");
    assert!(matches!(err, SdkError::IncompatibleShapes { .. }));
}

#[test]
fn v2_classification_conflict_is_a_typed_error() {
    let state = v2_state(1, "x", UpdateOp::Set);
    let occ = DeclaredPoint {
        namespace: NS_STATE,
        local: 1,
        name: "x".into(),
        classification: Classification::Occurrence,
        value_shape: None,
        base_op: None,
        expectation: None,
    };
    let err = decode_binary(&[at(0, 0, encode_v2_declaration(&[state, occ]))])
        .expect_err("classification conflict must fail");
    assert!(matches!(err, SdkError::ClassificationConflict { .. }));
}

// --- malformed lengths are typed errors, never panics ------------------------

#[test]
fn truncated_v2_record_is_a_malformed_length_error() {
    let decl = encode_v2_declaration(&[v2_state(1, "commit_index", UpdateOp::Set)]);
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
}
