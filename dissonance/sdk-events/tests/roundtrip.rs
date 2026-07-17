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

/// Load a bare schema (with no events) as the validated [`Normalized`] artifact —
/// the only public deserialization entry. The schema-entry choke point runs *before*
/// the declaration-provenance check, so a malformed entry is refused here regardless
/// of the (here absent) declaration.
fn load_schema(schema: serde_json::Value) -> Result<Normalized, serde_json::Error> {
    let artifact = serde_json::json!({ "schema": schema, "events": [] }).to_string();
    serde_json::from_str::<Normalized>(&artifact)
}

#[test]
fn deserializing_a_noncanonical_schema_is_rejected() {
    // `SdkSchema::entry` binary-searches its entries, so a persisted schema whose
    // entries are unsorted or duplicated would make declared identities unfindable.
    // Loading must reject it rather than accept silently corrupt evidence — and the
    // sort/uniqueness check runs before the declaration-provenance check, so a null
    // declaration (below) reaches it.
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
    };

    // Out-of-order entries (local 5 before local 1) are rejected.
    assert!(load_schema(make(serde_json::json!([entry(5), entry(1)]))).is_err());
    // Duplicate ids are rejected.
    assert!(load_schema(make(serde_json::json!([entry(1), entry(1)]))).is_err());

    // A well-ordered, unique schema — as the decoder actually produces it, declaration
    // and all — still deserializes, and binary search finds a declared entry after the
    // round-trip.
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
    let loaded =
        serde_json::from_str::<Normalized>(&serde_json::to_string(&n).unwrap()).expect("canonical");
    assert!(
        loaded
            .schema
            .entry(&ObservationId::Point {
                namespace: NS_STATE,
                local: 5
            })
            .is_some(),
        "binary search finds the entry after a canonical deserialize"
    );
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

/// The full source-specific schema-entry invariant family, enforced on load through
/// the one `SchemaEntry::validate` choke point. Loading routes only through the
/// validated [`Normalized`] artifact; the choke point runs before the
/// declaration-provenance check, so a null-declaration wrapper (`load_schema`)
/// reaches it and a malformed entry is refused — one rejection per invariant.
///
/// The acceptance direction (a valid entry per source × role is *not* over-rejected)
/// is proved separately, in `valid_entries_load_for_every_source`, against real
/// decoded artifacts — a binary schema is only loadable with a matching declaration,
/// which a hand-written null-declaration schema cannot carry.
#[test]
fn schema_invariant_family_is_refused_on_load() {
    use sdk_events::{NS_ASSERT, NS_LIFECYCLE};

    fn schema(
        source: &str,
        id: serde_json::Value,
        class: &str,
        shape: serde_json::Value,
        op: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({
            "source": source,
            "ordering": "RolloutLocalSourceOrdinal",
            "original_declaration": null,
            "entries": [{
                "id": id, "classification": class,
                "value_shape": shape, "base_op": op,
                "expectation": null, "name": null,
            }],
        })
    }
    let point =
        |ns: u8, local: u32| serde_json::json!({ "Point": { "namespace": ns, "local": local } });
    let prop = |s: &str| serde_json::json!({ "Property": s });
    let null = || serde_json::Value::Null;
    let bad = |s: serde_json::Value| {
        assert!(load_schema(s.clone()).is_err(), "should REJECT: {s}");
    };

    // --- INV-1: an occurrence is inert (no reducer, no shape) ---
    bad(schema(
        "AntithesisJson",
        prop("p"),
        "Occurrence",
        serde_json::json!("U64"),
        null(),
    ));
    bad(schema(
        "AntithesisJson",
        prop("p"),
        "Occurrence",
        null(),
        serde_json::json!("Set"),
    ));

    // --- INV-2: binary v1 state never resolves a reducer/shape (the r13 catch) ---
    bad(schema(
        "BinaryV1",
        point(NS_STATE, 1),
        "State",
        serde_json::json!("U64"),
        serde_json::json!("Set"),
    ));
    bad(schema(
        "BinaryV1",
        point(NS_STATE, 1),
        "State",
        serde_json::json!("U64"),
        null(),
    ));
    bad(schema(
        "BinaryV1",
        point(NS_STATE, 1),
        "State",
        null(),
        serde_json::json!("Set"),
    ));

    // --- INV-3: binary v2 state needs a resolved op + the u64 shape ---
    bad(schema(
        "BinaryV2",
        point(NS_STATE, 1),
        "State",
        serde_json::json!("U64"),
        null(),
    )); // no op
    bad(schema(
        "BinaryV2",
        point(NS_STATE, 1),
        "State",
        serde_json::json!("Bool"),
        serde_json::json!("Set"),
    )); // wrong shape
    bad(schema(
        "BinaryV2",
        point(NS_STATE, 1),
        "State",
        serde_json::json!("Numeric"),
        serde_json::json!("Set"),
    )); // numeric is antithesis-only

    // --- INV-4: antithesis state is numeric max/min guidance ---
    bad(schema(
        "AntithesisJson",
        prop("g"),
        "State",
        serde_json::json!("Numeric"),
        serde_json::json!("Set"),
    )); // wrong op
    bad(schema(
        "AntithesisJson",
        prop("g"),
        "State",
        serde_json::json!("U64"),
        serde_json::json!("Max"),
    )); // wrong shape

    // --- INV-5: id variant matches the source ---
    bad(schema("BinaryV1", prop("p"), "Occurrence", null(), null())); // binary needs a Point
    bad(schema(
        "AntithesisJson",
        point(NS_ASSERT, 1),
        "Occurrence",
        null(),
        null(),
    )); // antithesis needs Property/Lifecycle

    // --- INV-6: a point's namespace matches its classification ---
    bad(schema(
        "BinaryV2",
        point(NS_ASSERT, 1),
        "State",
        serde_json::json!("U64"),
        serde_json::json!("Set"),
    )); // state at assert ns
    bad(schema(
        "BinaryV1",
        point(NS_STATE, 1),
        "Occurrence",
        null(),
        null(),
    )); // occurrence at state ns
    bad(schema(
        "BinaryV2",
        point(9, 1),
        "Occurrence",
        null(),
        null(),
    )); // unknown ns

    // --- INV-7: a point's local id is an addressable 24-bit coordinate ---
    bad(schema(
        "BinaryV1",
        point(NS_STATE, 0x0100_0000),
        "State",
        null(),
        null(),
    )); // 2^24

    // --- INV-8: a lifecycle point sits only at the setup_complete local (0) ---
    bad(schema(
        "BinaryV2",
        point(NS_LIFECYCLE, 5),
        "Occurrence",
        null(),
        null(),
    ));
}

/// Build a v1 catalog declaration blob (`SDKC` magic + version 1 + records), the
/// only way to give a binary-v1 schema the declaration its provenance check requires.
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

/// The acceptance direction of the invariant family: a valid entry for every
/// (source × role) is admitted by the choke point and its artifact round-trips. Built
/// from real decoded artifacts, since a binary schema only loads with a matching
/// declaration the null-declaration `load_schema` helper cannot supply.
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

    // Antithesis: assertion, numeric guidance, and setup all load (null declaration).
    let ant =
        |id: serde_json::Value, class: &str, shape: serde_json::Value, op: serde_json::Value| {
            serde_json::json!({
                "source": "AntithesisJson", "ordering": "RolloutLocalSourceOrdinal",
                "original_declaration": null,
                "entries": [{ "id": id, "classification": class,
                    "value_shape": shape, "base_op": op, "expectation": null, "name": null }],
            })
        };
    let n = serde_json::Value::Null;
    assert!(
        load_schema(ant(
            serde_json::json!({"Property": "p"}),
            "Occurrence",
            n.clone(),
            n.clone()
        ))
        .is_ok()
    );
    assert!(
        load_schema(ant(
            serde_json::json!({"Property": "g"}),
            "State",
            serde_json::json!("Numeric"),
            serde_json::json!("Max"),
        ))
        .is_ok()
    );
    assert!(
        load_schema(ant(
            serde_json::json!({"Lifecycle": "setup"}),
            "Occurrence",
            n.clone(),
            n
        ))
        .is_ok()
    );

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
