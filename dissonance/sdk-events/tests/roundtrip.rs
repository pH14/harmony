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
    let schema = decode_binary(&[(Moment(0), 0, decl.clone())])
        .expect("decodes")
        .schema;

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
    // And the schema itself round-trips through serde.
    let json = serde_json::to_string(&schema).unwrap();
    assert_eq!(
        serde_json::from_str::<sdk_events::SdkSchema>(&json).unwrap(),
        schema
    );
}

#[test]
fn deserializing_a_noncanonical_schema_is_rejected() {
    // `SdkSchema::entry` binary-searches its entries, so a persisted schema whose
    // entries are unsorted or duplicated would make declared identities unfindable.
    // Deserialization must reject it rather than accept silently corrupt evidence.
    let entry = |local: u32| {
        serde_json::json!({
            "id": {"Point": {"namespace": NS_STATE, "local": local}},
            "classification": "State",
            "value_shape": "U64",
            "base_op": "Set",
            "expectation": null,
            "name": null,
        })
    };
    let make = |entries: serde_json::Value| {
        serde_json::json!({
            "source": "BinaryV2",
            "ordering": "RolloutLocalSourceOrdinal",
            "entries": entries,
            "original_declaration": null,
        })
        .to_string()
    };

    // Out-of-order entries (local 5 before local 1) are rejected.
    let unsorted = make(serde_json::json!([entry(5), entry(1)]));
    assert!(serde_json::from_str::<sdk_events::SdkSchema>(&unsorted).is_err());
    // Duplicate ids are rejected.
    let duplicate = make(serde_json::json!([entry(1), entry(1)]));
    assert!(serde_json::from_str::<sdk_events::SdkSchema>(&duplicate).is_err());
    // A well-ordered, unique schema still deserializes.
    let ok = make(serde_json::json!([entry(1), entry(5)]));
    let schema = serde_json::from_str::<sdk_events::SdkSchema>(&ok).expect("canonical");
    assert!(
        schema
            .entry(&ObservationId::Point {
                namespace: NS_STATE,
                local: 5
            })
            .is_some(),
        "binary search finds the entry after a canonical deserialize"
    );
}

#[test]
fn a_resolved_state_without_a_supported_shape_is_neither_reducible_nor_deserializable() {
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

    // Reducibility: only a resolved `u64` state is reducible.
    assert!(state(Some(ValueShape::U64), Some(UpdateOp::Set)).is_reducible_state());
    // A resolved state with no supported concrete shape (shape-less / bool / bytes)
    // or a report-only numeric shape is NOT reducible.
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

    // Deserialization rejects a resolved state whose shape is not U64/Numeric — a
    // persisted schema must not admit shape-less state into downstream reduction.
    let make = |shape: serde_json::Value| {
        serde_json::json!({
            "source": "BinaryV2",
            "ordering": "RolloutLocalSourceOrdinal",
            "original_declaration": null,
            "entries": [{
                "id": {"Point": {"namespace": NS_STATE, "local": 1}},
                "classification": "State",
                "value_shape": shape,
                "base_op": "Set",
                "expectation": null,
                "name": null,
            }],
        })
        .to_string()
    };
    for bad in [
        serde_json::Value::Null,
        serde_json::json!("Bool"),
        serde_json::json!("Bytes"),
    ] {
        assert!(
            serde_json::from_str::<sdk_events::SdkSchema>(&make(bad.clone())).is_err(),
            "resolved state with shape {bad} must be rejected on deserialize"
        );
    }
    // The reducible bounded integer and the report-only numeric shape still admit.
    for ok in [serde_json::json!("U64"), serde_json::json!("Numeric")] {
        assert!(serde_json::from_str::<sdk_events::SdkSchema>(&make(ok)).is_ok());
    }
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
