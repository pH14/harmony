// SPDX-License-Identifier: AGPL-3.0-or-later
//! Artifact-level load validation: a persisted [`Normalized`] is only obtainable
//! through its validated `try_from`, and that re-check holds a persisted artifact to
//! the same contract the live decoders enforce. These are the adjudication probes,
//! kept as regression tests — one per invariant family (F1a–F1d), plus the setup
//! status-fabrication guard (F2).
//!
//! A binary schema is only loadable inside a validated artifact whose
//! `original_declaration` re-parses to its entries, so most probes start from a real
//! decoded artifact and mutate exactly one field of its serde form — the smallest
//! step from valid to corrupt — then assert the load fails with the *specific* typed
//! error the decoder would raise, not merely that it fails.

use explorer::Moment;
use sdk_events::{
    Classification, DeclaredPoint, Expectation, NS_ASSERT, NS_STATE, Normalized, SdkError,
    UpdateOp, ValueShape, decode_antithesis, decode_binary, encode_v2_declaration,
};

/// A valid binary-v2 artifact: one `max`-declared state coordinate with two `max`
/// firings (ordinals 1 and 2). The starting point for the F1a/F1d mutation probes.
fn max_state_artifact() -> serde_json::Value {
    let decl = encode_v2_declaration(&[DeclaredPoint {
        namespace: NS_STATE,
        local: 1,
        name: "reg".into(),
        classification: Classification::State,
        value_shape: Some(ValueShape::U64),
        base_op: Some(UpdateOp::Max),
        expectation: None,
    }])
    .expect("valid declaration");
    let firing = |v: u64| {
        let mut b = vec![1u8]; // STATE_MAX
        b.extend_from_slice(&v.to_le_bytes());
        b
    };
    let id = ((NS_STATE as u32) << 24) | 1;
    let n = decode_binary(&[
        (Moment(0), 0, decl),
        (Moment(5), id, firing(7)),
        (Moment(6), id, firing(9)),
    ])
    .expect("decodes");
    serde_json::to_value(&n).expect("serializes")
}

fn load(v: serde_json::Value) -> Result<Normalized, serde_json::Error> {
    serde_json::from_value::<Normalized>(v)
}

/// The error text a failed load surfaces (serde wraps the typed `SdkError`'s
/// `Display`), so a probe can assert it failed for the *right* reason.
fn load_err(v: serde_json::Value) -> String {
    load(v).expect_err("must reject").to_string()
}

// --- F1a: declaration provenance is re-parsed and cross-checked on load ---------

#[test]
fn f1a_declaration_provenance_is_cross_checked() {
    let base = max_state_artifact();
    // The unmutated artifact loads.
    assert!(load(base.clone()).is_ok());

    // A v2 schema with its declaration nulled out — the source mints one, so its
    // absence is corrupt provenance.
    let mut v = base.clone();
    v["schema"]["original_declaration"] = serde_json::Value::Null;
    assert!(
        load_err(v).contains("declaration provenance mismatch"),
        "nulled declaration must be a provenance mismatch"
    );

    // A declaration blob that no longer re-parses to these entries (garbage bytes).
    let mut v = base.clone();
    v["schema"]["original_declaration"]["bytes"] = serde_json::json!([0, 0, 0, 0]);
    assert!(
        load_err(v).contains("declaration provenance mismatch"),
        "a blob that re-parses to different entries is a mismatch"
    );

    // A declaration blob tagged with the wrong source.
    let mut v = base.clone();
    v["schema"]["original_declaration"]["source"] = serde_json::json!("BinaryV1");
    assert!(load_err(v).contains("declaration provenance mismatch"));

    // An antithesis schema carrying a declaration blob (it mints none).
    let n = decode_antithesis(&[(
        Moment(1),
        br#"{"antithesis_assert":{"assert_type":"always","condition":true,"id":"p",
            "message":"p","must_hit":true,
            "location":{"file":"a.rs","function":"f","begin_line":1,"begin_column":2}}}"#
            .to_vec(),
    )])
    .expect("decodes");
    let mut v = serde_json::to_value(&n).unwrap();
    v["schema"]["original_declaration"] = serde_json::json!({
        "source": "AntithesisJson", "event_id": 0, "bytes": [],
    });
    assert!(
        load_err(v).contains("declaration provenance mismatch"),
        "antithesis carries no separate declaration"
    );
}

#[test]
fn f1a_binary_v1_entries_require_a_declaration() {
    // A binary-v1 schema with declared entries but no declaration: v1 entries come
    // only from a catalog, so entries with a null declaration are impossible.
    let artifact = serde_json::json!({
        "schema": {
            "source": "BinaryV1",
            "ordering": "RolloutLocalSourceOrdinal",
            "original_declaration": null,
            "entries": [{
                "id": {"Point": {"namespace": NS_STATE, "local": 1}},
                "classification": "State", "value_shape": null,
                "base_op": null, "expectation": null, "name": null,
            }],
        },
        "events": [],
    });
    assert!(load_err(artifact).contains("declaration provenance mismatch"));
}

// --- F1b: a lifecycle identity is always an occurrence --------------------------

#[test]
fn f1b_lifecycle_identity_must_be_an_occurrence() {
    // An antithesis lifecycle identity persisted as `State` is forged — the decoder
    // only ever mints the setup lifecycle as an occurrence.
    let artifact = serde_json::json!({
        "schema": {
            "source": "AntithesisJson",
            "ordering": "RolloutLocalSourceOrdinal",
            "original_declaration": null,
            "entries": [{
                "id": {"Lifecycle": "antithesis.setup"},
                "classification": "State",
                "value_shape": "Numeric", "base_op": "Max",
                "expectation": null, "name": null,
            }],
        },
        "events": [],
    });
    assert!(load_err(artifact).contains("malformed schema entry"));
}

// --- F1c: an expectation is legal only on an assertion point --------------------

#[test]
fn f1c_expectation_is_legal_only_on_an_assertion_point() {
    let with_expectation = |source: &str, id: serde_json::Value, class, shape, op| {
        serde_json::json!({
            "schema": {
                "source": source,
                "ordering": "RolloutLocalSourceOrdinal",
                "original_declaration": null,
                "entries": [{
                    "id": id, "classification": class,
                    "value_shape": shape, "base_op": op,
                    "expectation": "MustHit", "name": null,
                }],
            },
            "events": [],
        })
    };
    // A binary state point carrying an expectation is malformed.
    assert!(
        load_err(with_expectation(
            "BinaryV2",
            serde_json::json!({"Point": {"namespace": NS_STATE, "local": 1}}),
            "State",
            serde_json::json!("U64"),
            serde_json::json!("Set"),
        ))
        .contains("malformed schema entry")
    );
    // An antithesis guidance (state) point carrying an expectation is malformed.
    assert!(
        load_err(with_expectation(
            "AntithesisJson",
            serde_json::json!({"Property": "g"}),
            "State",
            serde_json::json!("Numeric"),
            serde_json::json!("Max"),
        ))
        .contains("malformed schema entry")
    );

    // Encode consistency: the byte codec must not mint a declaration its own decoder
    // would refuse — an expectation on a state point fails to encode.
    let encoded = encode_v2_declaration(&[DeclaredPoint {
        namespace: NS_STATE,
        local: 1,
        name: "reg".into(),
        classification: Classification::State,
        value_shape: Some(ValueShape::U64),
        base_op: Some(UpdateOp::Set),
        expectation: Some(Expectation::MustHit),
    }]);
    assert!(
        matches!(encoded, Err(SdkError::UnsupportedDeclaration { .. })),
        "an expectation on a state point must fail to encode"
    );
}

// --- F1d: a persisted event must cohere with its schema -------------------------

#[test]
fn f1d_set_firing_at_a_max_declared_coordinate_is_mixed_operations() {
    // The canonical probe: a persisted `set` firing at a `max`-declared coordinate
    // must fail load with the same `MixedOperations` the decoder raises live.
    let mut v = max_state_artifact();
    v["events"][0]["payload"]["State"]["op"] = serde_json::json!("Set");
    assert!(
        load_err(v).contains("conflicting base operations"),
        "a set at a max coordinate is MixedOperations"
    );
}

#[test]
fn f1d_two_firings_with_conflicting_ops_are_mixed_operations() {
    // Two firings for one identity that disagree — the decoder's per-identity
    // `observed_ops` rule, re-checked on load (both entries stay `max`-legal, so the
    // conflict is event-to-event, not event-to-declaration).
    //
    // Redeclare the coordinate with `set` so `set` is the legal declared op, then
    // flip the *second* firing to `max`.
    let decl = encode_v2_declaration(&[DeclaredPoint {
        namespace: NS_STATE,
        local: 1,
        name: "reg".into(),
        classification: Classification::State,
        value_shape: Some(ValueShape::U64),
        base_op: Some(UpdateOp::Set),
        expectation: None,
    }])
    .unwrap();
    let firing = |op: u8, val: u64| {
        let mut b = vec![op];
        b.extend_from_slice(&val.to_le_bytes());
        b
    };
    let id = ((NS_STATE as u32) << 24) | 1;
    let n = decode_binary(&[
        (Moment(0), 0, decl),
        (Moment(5), id, firing(0, 7)), // STATE_SET
        (Moment(6), id, firing(0, 9)), // STATE_SET
    ])
    .unwrap();
    let mut v = serde_json::to_value(&n).unwrap();
    // Flip the second firing to `max`; the declared op is `set`, so this contradicts
    // both the declaration and the first firing.
    v["events"][1]["payload"]["State"]["op"] = serde_json::json!("Max");
    assert!(load_err(v).contains("conflicting base operations"));
}

#[test]
fn f1d_event_source_must_agree_with_the_schema() {
    let mut v = max_state_artifact();
    v["events"][0]["source"] = serde_json::json!("BinaryV1");
    assert!(
        load_err(v).contains("incoherent persisted event"),
        "an event source disagreeing with the schema is incoherent"
    );
}

#[test]
fn f1d_ordinals_must_be_strictly_increasing() {
    let mut v = max_state_artifact();
    // The two firings decode to ordinals 1 and 2; make the second not exceed the first.
    v["events"][1]["ordinal"] = serde_json::json!(1);
    assert!(
        load_err(v).contains("incoherent persisted event"),
        "a non-increasing ordinal is incoherent"
    );
}

#[test]
fn f1d_a_classified_payload_at_the_wrong_namespace_is_incoherent() {
    // Move a `State` firing to an assertion namespace: state payloads only decode
    // under `NS_STATE`, so one at `NS_ASSERT` is evidence the decoder never mints.
    let mut v = max_state_artifact();
    v["events"][0]["id"]["Point"]["namespace"] = serde_json::json!(NS_ASSERT);
    assert!(load_err(v).contains("incoherent persisted event"));
}

// --- F2: setup status fabrication (bead hm-jyj) --------------------------------

#[test]
fn f2_present_but_non_string_setup_status_stays_raw() {
    // A setup record whose `status` is present but not a string is malformed; rather
    // than fabricate a `complete`/named lifecycle point, it is preserved raw (mirrors
    // `site_of`), so no lifecycle schema entry is minted.
    let n = decode_antithesis(&[(Moment(1), br#"{"antithesis_setup":{"status":7}}"#.to_vec())])
        .expect("decodes without panicking");
    assert_eq!(n.events.len(), 1);
    assert_eq!(n.events[0].payload, sdk_events::Payload::Unknown);
    assert!(
        n.schema.entries().is_empty(),
        "a fabricated setup status mints no lifecycle entry"
    );
    // The raw record survives verbatim for audit.
    assert_eq!(
        n.events[0].raw.bytes,
        br#"{"antithesis_setup":{"status":7}}"#
    );
    // And the decoded artifact still round-trips (the raw event carries no schema
    // coherence obligation).
    let back = serde_json::from_value::<Normalized>(serde_json::to_value(&n).unwrap()).unwrap();
    assert_eq!(back, n);
}

// --- API ruling: `Normalized` is the only publicly deserializable artifact ------

/// A compile-time detector for `T: DeserializeOwned` returned as a runtime bool
/// (autoref specialization). `ViaDeserialize::probe` binds on `Probe<T>` directly
/// and is chosen when `T: DeserializeOwned`; otherwise method resolution falls back
/// through an autoref to `ViaFallback::probe` on `&Probe<T>`.
struct Probe<T>(core::marker::PhantomData<T>);
trait ViaDeserialize {
    fn probe(&self) -> bool;
}
impl<T: serde::de::DeserializeOwned> ViaDeserialize for Probe<T> {
    fn probe(&self) -> bool {
        true
    }
}
trait ViaFallback {
    fn probe(&self) -> bool;
}
impl<T> ViaFallback for &Probe<T> {
    fn probe(&self) -> bool {
        false
    }
}
macro_rules! is_deserializable {
    ($t:ty) => {
        (&Probe::<$t>(core::marker::PhantomData)).probe()
    };
}

/// The API ruling, enforced mechanically: the validated [`Normalized`] artifact is
/// the *only* publicly-deserializable entry. `SdkEvent`/`SdkSchema` must not carry a
/// bare `Deserialize` — re-deriving one would flip a constant here and fail the test.
///
/// This guard exists because the `cargo public-api` snapshot runs at `-sss`, which
/// omits every auto-derived impl (`Serialize`/`Deserialize`/`Clone`/…), so the
/// removal of a derived `Deserialize` is invisible in that snapshot — it can only be
/// enforced by a bound like this.
#[test]
fn only_normalized_is_publicly_deserializable() {
    assert!(
        is_deserializable!(Normalized),
        "Normalized must stay deserializable — the one validated load entry"
    );
    assert!(
        !is_deserializable!(sdk_events::SdkEvent),
        "SdkEvent must not carry a bare Deserialize (load only via Normalized)"
    );
    assert!(
        !is_deserializable!(sdk_events::SdkSchema),
        "SdkSchema must not carry a bare Deserialize (load only via Normalized)"
    );
    // Component value types still deserialize — they have no independent load path,
    // so they are only ever read back *inside* a validated `Normalized`.
    assert!(is_deserializable!(sdk_events::SchemaEntry));
    assert!(is_deserializable!(sdk_events::Payload));
}
