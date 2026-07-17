// SPDX-License-Identifier: AGPL-3.0-or-later
//! Persistence laws: the normalized `SdkSchema`/`SdkEvent` model round-trips
//! through serde, its serialization is canonical and deterministic (the
//! macOS/Linux-identical requirement), original JSON number tokens and unknown raw
//! bytes survive the round-trip, and the wire-v2 declaration round-trips through
//! the byte codec.

use explorer::Moment;
use proptest::prelude::*;
use sdk_events::{
    Classification, DeclaredPoint, NS_STATE, Normalized, ObservationId, Payload, UpdateOp,
    ValueShape, decode_antithesis, decode_binary, encode_v2_declaration,
};

fn roundtrip(n: &Normalized) -> Normalized {
    let json = serde_json::to_string(n).expect("serialize");
    serde_json::from_str(&json).expect("deserialize")
}

#[test]
fn normalized_serde_round_trips_and_is_deterministic() {
    let json = r#"{"antithesis_assert":{"assert_type":"always","condition":true,
        "id":"p","message":"p","must_hit":true,
        "location":{"file":"a.rs","function":"f","begin_line":1,"begin_column":2}}}"#;
    let n = decode_antithesis(&[(Moment(1), json.as_bytes().to_vec())]).expect("decodes");

    // Round-trips to an equal value.
    assert_eq!(roundtrip(&n), n);
    // Deterministic: serializing twice yields identical bytes (no HashMap order).
    let a = serde_json::to_string(&n).unwrap();
    let b = serde_json::to_string(&n).unwrap();
    assert_eq!(a, b);
}

#[test]
fn original_number_token_survives_the_round_trip() {
    let json = r#"{"antithesis_guidance":{"guidance_type":"numeric","maximize":true,
        "id":"g","guidance_data":1000000.000}}"#;
    let n = decode_antithesis(&[(Moment(1), json.as_bytes().to_vec())]).expect("decodes");
    let back = roundtrip(&n);

    match &back.events[0].payload {
        Payload::Guidance { token, .. } => {
            assert_eq!(token.as_ref().unwrap().as_str(), "1000000.000");
        }
        other => panic!("{other:?}"),
    }
    // The raw JSON bytes survive verbatim too.
    assert_eq!(back.events[0].raw.bytes, json.as_bytes());
}

#[test]
fn unknown_raw_bytes_survive_the_round_trip() {
    let garbage = vec![0xFFu8, 0x00, 0x13, 0x37];
    let raw = vec![(Moment(9), ((9u32) << 24) | 5, garbage.clone())];
    let n = decode_binary(&raw).expect("decodes");
    let back = roundtrip(&n);
    assert_eq!(back.events[0].payload, Payload::Unknown);
    assert_eq!(back.events[0].raw.bytes, garbage);
}

#[test]
fn wire_v2_declaration_round_trips_through_the_byte_codec() {
    let points = vec![
        DeclaredPoint {
            namespace: NS_STATE,
            local: 1,
            name: "commit_index".into(),
            classification: Classification::State,
            value_shape: Some(ValueShape::U64),
            base_op: Some(UpdateOp::Set),
            expectation: None,
        },
        DeclaredPoint {
            namespace: NS_STATE,
            local: 2,
            name: "high_watermark".into(),
            classification: Classification::State,
            value_shape: Some(ValueShape::U64),
            base_op: Some(UpdateOp::Max),
            expectation: None,
        },
    ];
    let decl = encode_v2_declaration(&points).expect("valid declaration");
    let normalized = decode_binary(&[(Moment(0), 0, decl.clone())]).expect("decodes");
    let schema = &normalized.schema;

    // Each declared point re-emerges from the decoded schema.
    for p in &points {
        let id = ObservationId::Point {
            namespace: p.namespace,
            local: p.local,
        };
        let e = schema.entry(&id).expect("declared");
        assert_eq!(e.base_op, p.base_op);
        assert_eq!(e.value_shape, p.value_shape);
        assert_eq!(e.name.as_deref(), Some(p.name.as_str()));
    }
    // The original declaration bytes are recoverable for audit/migration.
    assert_eq!(schema.original_declaration.as_ref().unwrap().bytes, decl);
    // And the whole normalized artifact round-trips through serde. The schema is only
    // loadable *inside* a validated `Normalized`; its `original_declaration` re-parses
    // to exactly these entries, so the artifact survives the round-trip unchanged.
    let json = serde_json::to_string(&normalized).unwrap();
    assert_eq!(
        serde_json::from_str::<Normalized>(&json).unwrap(),
        normalized
    );
}

#[test]
fn a_noncanonical_or_tampered_schema_does_not_load() {
    // `SdkSchema::entry` binary-searches its entries, so unsorted or duplicated
    // entries would make declared identities unfindable. A live decode always emits
    // them sorted and unique; re-decode-and-compare rejects any persisted schema that
    // is not, with no separate sort check to defeat.
    let point = |local, op| DeclaredPoint {
        namespace: NS_STATE,
        local,
        name: "reg".into(),
        classification: Classification::State,
        value_shape: Some(ValueShape::U64),
        base_op: Some(op),
        expectation: None,
    };
    let decl = encode_v2_declaration(&[point(1, UpdateOp::Set), point(5, UpdateOp::Max)])
        .expect("valid declaration");
    let n = decode_binary(&[(Moment(0), 0, decl)]).expect("decodes");
    let canonical = serde_json::to_value(&n).unwrap();

    // The canonical artifact loads, and binary search finds a declared entry.
    let loaded = serde_json::from_value::<Normalized>(canonical.clone()).expect("canonical loads");
    assert!(
        loaded
            .schema
            .entry(&ObservationId::Point {
                namespace: NS_STATE,
                local: 5
            })
            .is_some(),
        "binary search finds the entry after a canonical load"
    );

    let diverges = |v: serde_json::Value| {
        let e = serde_json::from_value::<Normalized>(v)
            .expect_err("rejected")
            .to_string();
        assert!(e.contains("diverges from a live decode"), "got: {e}");
    };
    let entries = canonical["schema"]["entries"].as_array().unwrap().clone();
    // Entries reversed out of sort order — the declaration re-parses to sorted entries.
    let mut unsorted = canonical.clone();
    unsorted["schema"]["entries"] = serde_json::json!([entries[1], entries[0]]);
    diverges(unsorted);
    // A duplicated entry — the declaration re-parses to the two distinct entries.
    let mut duplicated = canonical.clone();
    duplicated["schema"]["entries"] = serde_json::json!([entries[0], entries[0]]);
    diverges(duplicated);
}

#[test]
fn only_a_resolved_u64_state_is_reducible() {
    use sdk_events::SchemaEntry;

    let state = |shape: Option<ValueShape>, op: Option<UpdateOp>| SchemaEntry {
        id: ObservationId::Point {
            namespace: NS_STATE,
            local: 1,
        },
        classification: Classification::State,
        value_shape: shape,
        base_op: op,
        expectation: None,
        name: None,
    };

    // Only a resolved `u64` state is reducible.
    assert!(state(Some(ValueShape::U64), Some(UpdateOp::Set)).is_reducible_state());
    // A resolved state with no supported concrete shape (shape-less / bool / bytes)
    // or the report-only numeric shape is NOT reducible.
    for shape in [
        None,
        Some(ValueShape::Bool),
        Some(ValueShape::Bytes),
        Some(ValueShape::Numeric),
    ] {
        assert!(
            !state(shape, Some(UpdateOp::Set)).is_reducible_state(),
            "resolved state with shape {shape:?} must not be reducible"
        );
    }
    // An unresolved state (no base op) is never reducible.
    assert!(!state(Some(ValueShape::U64), None).is_reducible_state());
}

// The source-specific schema-entry invariant family (occurrence-inert, v1-unresolved,
// v2-u64, antithesis-guidance, id↔source, namespace↔classification, 24-bit local,
// lifecycle local 0, expectation legality) is enforced by `SchemaEntry::validate`
// where entries are actually minted — at DECODE, via `merge_entry`. Those rejections
// are tested against the byte codec in `tests/normalize_binary.rs` (e.g.
// `v2_non_u64_state_shape_is_rejected_on_encode_and_decode`,
// `v2_classification_must_agree_with_the_namespace`,
// `v2_occurrence_carrying_a_value_or_operation_is_rejected`). On the LOAD path the
// same guarantee holds by construction: `tests/load_validation.rs` shows a persisted
// schema carrying an entry a live decode never mints diverges from the re-decode.

/// Build a v1 catalog declaration blob (`SDKC` magic + version 1 + records), the
/// only way to give a binary-v1 schema its declaration.
fn v1_catalog(points: &[(u8, u32, &str)]) -> Vec<u8> {
    let mut b = u32::from_le_bytes(*b"SDKC").to_le_bytes().to_vec();
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

/// A valid decoded artifact for every (source × role) round-trips through the load —
/// the acceptance direction, proving re-decode-and-compare does not over-reject.
#[test]
fn valid_entries_load_for_every_source() {
    use sdk_events::{NS_ASSERT, NS_LIFECYCLE};
    // v1 catalog point-kind bytes.
    const KIND_SOMETIMES: u8 = 1;
    const KIND_STATE: u8 = 4;
    const KIND_BUGGIFY: u8 = 5;

    let loads = |n: &Normalized| {
        let json = serde_json::to_string(n).unwrap();
        assert_eq!(&serde_json::from_str::<Normalized>(&json).unwrap(), n);
    };

    // Antithesis: an assertion, numeric guidance, and setup — every role in one stream.
    let ant = decode_antithesis(&[
        (Moment(1), br#"{"antithesis_assert":{"assert_type":"always","condition":true,"id":"p","message":"p","must_hit":true,"location":{"file":"a.rs","function":"f","begin_line":1,"begin_column":2}}}"#.to_vec()),
        (Moment(2), br#"{"antithesis_guidance":{"guidance_type":"numeric","maximize":true,"id":"g","guidance_data":5}}"#.to_vec()),
        (Moment(3), br#"{"antithesis_setup":{"status":"complete"}}"#.to_vec()),
    ])
    .expect("antithesis decodes");
    assert_eq!(
        ant.schema.entries().len(),
        3,
        "assertion + guidance + setup entries"
    );
    loads(&ant);

    // Binary v1: an assert, an (unresolved) state, and a buggify point.
    let v1 = decode_binary(&[(
        Moment(0),
        0,
        v1_catalog(&[
            (KIND_SOMETIMES, 1, "s"),
            (KIND_STATE, 2, "reg"),
            (KIND_BUGGIFY, 3, "bug"),
        ]),
    )])
    .expect("v1 decodes");
    assert_eq!(v1.schema.entries().len(), 3);
    loads(&v1);

    // Binary v2: a resolved state, an occurrence (assert), and the lifecycle point.
    let dp = |ns, local, name: &str, class, shape, op| DeclaredPoint {
        namespace: ns,
        local,
        name: name.into(),
        classification: class,
        value_shape: shape,
        base_op: op,
        expectation: None,
    };
    let v2 = decode_binary(&[(
        Moment(0),
        0,
        encode_v2_declaration(&[
            dp(
                NS_STATE,
                1,
                "reg",
                Classification::State,
                Some(ValueShape::U64),
                Some(UpdateOp::Set),
            ),
            dp(NS_ASSERT, 2, "a", Classification::Occurrence, None, None),
            dp(
                NS_LIFECYCLE,
                0,
                "setup",
                Classification::Occurrence,
                None,
                None,
            ),
        ])
        .expect("valid v2"),
    )])
    .expect("v2 decodes");
    assert_eq!(v2.schema.entries().len(), 3);
    loads(&v2);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Any decoded binary stream round-trips through serde unchanged.
    #[test]
    fn arbitrary_binary_decode_round_trips(
        stream in prop::collection::vec(
            (any::<u64>(), any::<u32>(), prop::collection::vec(any::<u8>(), 0..40)),
            0..16,
        )
    ) {
        let raw: Vec<_> = stream.into_iter().map(|(m, id, b)| (Moment(m), id, b)).collect();
        if let Ok(n) = decode_binary(&raw) {
            prop_assert_eq!(roundtrip(&n), n);
        }
    }
}
