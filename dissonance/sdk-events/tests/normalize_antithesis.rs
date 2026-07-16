// SPDX-License-Identifier: AGPL-3.0-or-later
//! Antithesis SDK JSON ingress: assertions as occurrence/property evidence with
//! separate site and property identity, numeric guidance normalized to a monotone
//! extremum only (original token preserved, no `f64`), per-identity coherence
//! errors, and decode totality with raw preservation.

use explorer::Moment;
use proptest::prelude::*;
use sdk_events::{
    AssertType, Classification, Expectation, ObservationId, Payload, SdkError, SourceFormat,
    UpdateOp, ValueShape, decode_antithesis,
};

fn rec(m: u64, json: &str) -> (Moment, Vec<u8>) {
    (Moment(m), json.as_bytes().to_vec())
}

// --- assertions: occurrence/property evidence, site ≠ property ---------------

#[test]
fn assertion_normalizes_to_occurrence_property_evidence() {
    let json = r#"{"antithesis_assert":{
        "assert_type":"always","display_type":"Always","condition":true,
        "message":"balance stays non-negative","id":"prop-balance","must_hit":true,
        "location":{"file":"src/bank.rs","function":"withdraw","begin_line":42,"begin_column":9},
        "details":{}}}"#;
    let n = decode_antithesis(&[rec(100, json)]).expect("decodes");

    assert_eq!(n.schema.source, SourceFormat::AntithesisJson);
    let ev = &n.events[0];
    // The aggregated property is the *message* (DISSONANCE-STRATEGY: "the assertion
    // message identifies the property"); the per-site `id` is site metadata.
    assert_eq!(
        ev.id,
        ObservationId::Property("balance stays non-negative".into())
    );
    // …and the site is separate provenance, not a property verdict.
    let site = ev.site.as_ref().expect("site preserved");
    assert_eq!(site.id.as_deref(), Some("prop-balance"));
    assert_eq!(site.file, "src/bank.rs");
    assert_eq!(site.function, "withdraw");
    assert_eq!(site.begin_line, 42);
    assert_eq!(site.begin_column, 9);
    assert_eq!(
        ev.payload,
        Payload::Assertion {
            assert_type: Some(AssertType::Always),
            condition: Some(true)
        }
    );

    // The property is occurrence-classified with a preserved must-hit expectation;
    // `sdk-events` records it but never derives the absence claim.
    let entry = n
        .schema
        .entry(&ObservationId::Property(
            "balance stays non-negative".into(),
        ))
        .unwrap();
    assert_eq!(entry.classification, Classification::Occurrence);
    assert_eq!(entry.base_op, None);
    assert_eq!(entry.expectation, Some(Expectation::MustHit));
}

#[test]
fn sites_with_differing_ids_aggregate_by_message_into_one_property() {
    // Two records at different sites, with different per-site `id`s, but the same
    // message — the strategy's "multiple sites may contribute to one property".
    let a = r#"{"antithesis_assert":{"assert_type":"sometimes","condition":true,
        "id":"site-a","message":"progress made","must_hit":true,
        "location":{"file":"a.rs","function":"f","begin_line":1,"begin_column":1}}}"#;
    let b = r#"{"antithesis_assert":{"assert_type":"sometimes","condition":false,
        "id":"site-b","message":"progress made","must_hit":true,
        "location":{"file":"b.rs","function":"g","begin_line":2,"begin_column":2}}}"#;
    let n = decode_antithesis(&[rec(1, a), rec(2, b)]).expect("decodes");

    // Two events with two distinct site ids, one aggregated property.
    assert_eq!(n.events.len(), 2);
    assert_eq!(
        n.events[0].site.as_ref().unwrap().id.as_deref(),
        Some("site-a")
    );
    assert_eq!(
        n.events[1].site.as_ref().unwrap().id.as_deref(),
        Some("site-b")
    );
    assert_eq!(
        n.events[0].id, n.events[1].id,
        "same message → same property"
    );
    assert_eq!(n.schema.len(), 1);
    assert!(
        n.schema
            .entry(&ObservationId::Property("progress made".into()))
            .is_some()
    );
}

#[test]
fn unreachable_assertion_declares_a_must_not_hit_expectation() {
    let json = r#"{"antithesis_assert":{"assert_type":"reachability","display_type":"Unreachable",
        "condition":false,"id":"never","message":"never","must_hit":false,
        "location":{"file":"c.rs","function":"h","begin_line":3,"begin_column":3}}}"#;
    let n = decode_antithesis(&[rec(1, json)]).expect("decodes");
    match n.events[0].payload {
        Payload::Assertion { assert_type, .. } => {
            assert_eq!(assert_type, Some(AssertType::Unreachable))
        }
        ref other => panic!("{other:?}"),
    }
    let entry = n
        .schema
        .entry(&ObservationId::Property("never".into()))
        .unwrap();
    assert_eq!(entry.expectation, Some(Expectation::MustNotHit));
}

// --- numeric guidance: monotone extremum only, exact token, no f64 -----------

#[test]
fn numeric_guidance_max_normalizes_to_the_declared_maximum() {
    let json = r#"{"antithesis_guidance":{"guidance_type":"numeric","maximize":true,
        "id":"depth","message":"maze depth","guidance_data":123,
        "location":{"file":"m.rs","function":"step","begin_line":10,"begin_column":4}}}"#;
    let n = decode_antithesis(&[rec(5, json)]).expect("decodes");

    match &n.events[0].payload {
        Payload::Guidance { op, token } => {
            assert_eq!(*op, UpdateOp::Max, "maximize → max, never set");
            assert_eq!(token.as_ref().unwrap().as_str(), "123");
        }
        other => panic!("{other:?}"),
    }
    // The guidance property is state-bearing with a resolved max op and numeric
    // shape — a monotone extremum, not arbitrary `set` state. Its identity is the
    // message ("maze depth"), with the per-site `id` ("depth") kept as site data.
    assert_eq!(
        n.events[0].site.as_ref().unwrap().id.as_deref(),
        Some("depth")
    );
    let entry = n
        .schema
        .entry(&ObservationId::Property("maze depth".into()))
        .unwrap();
    assert_eq!(entry.classification, Classification::State);
    assert_eq!(entry.base_op, Some(UpdateOp::Max));
    assert_eq!(entry.value_shape, Some(ValueShape::Numeric));
}

#[test]
fn numeric_guidance_min_preserves_the_exact_fractional_token() {
    let json = r#"{"antithesis_guidance":{"guidance_type":"numeric","maximize":false,
        "id":"latency","guidance_data":-12.50}}"#;
    let n = decode_antithesis(&[rec(5, json)]).expect("decodes");
    match &n.events[0].payload {
        Payload::Guidance { op, token } => {
            assert_eq!(*op, UpdateOp::Min);
            // The original token survives exactly — trailing zero and all.
            assert_eq!(token.as_ref().unwrap().as_str(), "-12.50");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn boolean_guidance_is_report_only_raw_evidence() {
    // Only the numeric verb has the explicit extremal contract we normalize.
    let json = r#"{"antithesis_guidance":{"guidance_type":"boolean","maximize":true,"id":"b"}}"#;
    let n = decode_antithesis(&[rec(1, json)]).expect("decodes");
    assert_eq!(n.events[0].payload, Payload::Unknown);
    assert_eq!(n.events[0].raw.bytes, json.as_bytes());
    assert!(n.schema.is_empty());
}

// --- per-identity coherence: typed errors ------------------------------------

#[test]
fn guidance_flipping_its_extremum_is_a_mixed_operations_error() {
    let up = r#"{"antithesis_guidance":{"guidance_type":"numeric","maximize":true,"id":"g","guidance_data":1}}"#;
    let down = r#"{"antithesis_guidance":{"guidance_type":"numeric","maximize":false,"id":"g","guidance_data":2}}"#;
    let err = decode_antithesis(&[rec(1, up), rec(2, down)]).expect_err("op flip must fail");
    assert!(matches!(
        err,
        SdkError::MixedOperations {
            first: UpdateOp::Max,
            second: UpdateOp::Min,
            ..
        }
    ));
}

#[test]
fn an_identity_used_as_both_assertion_and_guidance_conflicts() {
    let assertion = r#"{"antithesis_assert":{"assert_type":"always","condition":true,"id":"dual","message":"dual"}}"#;
    let guidance = r#"{"antithesis_guidance":{"guidance_type":"numeric","maximize":true,"id":"dual","guidance_data":1}}"#;
    let err = decode_antithesis(&[rec(1, assertion), rec(2, guidance)])
        .expect_err("classification conflict must fail");
    assert!(matches!(err, SdkError::ClassificationConflict { .. }));
}

// --- totality and raw preservation -------------------------------------------

#[test]
fn setup_record_is_a_lifecycle_occurrence() {
    let json = r#"{"antithesis_setup":{"status":"complete","details":{}}}"#;
    let n = decode_antithesis(&[rec(0, json)]).expect("decodes");
    assert_eq!(
        n.events[0].payload,
        Payload::Lifecycle {
            name: "setup_complete".into()
        }
    );
}

#[test]
fn unrecognized_wrapper_and_invalid_json_are_preserved_raw() {
    let unknown_wrapper = r#"{"antithesis_frobnicate":{"x":1}}"#;
    let not_json = "this is not json {";
    let n = decode_antithesis(&[rec(1, unknown_wrapper), rec(2, not_json)]).expect("total");
    assert_eq!(n.events[0].payload, Payload::Unknown);
    assert_eq!(n.events[0].raw.bytes, unknown_wrapper.as_bytes());
    assert_eq!(n.events[1].payload, Payload::Unknown);
    assert_eq!(n.events[1].raw.bytes, not_json.as_bytes());
    assert!(n.schema.is_empty());
}

#[test]
fn assertion_without_identity_is_preserved_raw_not_invented() {
    let json = r#"{"antithesis_assert":{"assert_type":"always","condition":true}}"#;
    let n = decode_antithesis(&[rec(1, json)]).expect("decodes");
    assert_eq!(n.events[0].payload, Payload::Unknown);
    assert!(n.schema.is_empty());
}

#[test]
fn a_record_with_two_wrappers_is_ambiguous_and_preserved_raw() {
    // Exactly one wrapper is the contract; a record carrying both an assertion and
    // guidance is ambiguous — preserved raw, never silently resolved to one branch
    // with the other dropped.
    let json = r#"{
        "antithesis_assert":{"assert_type":"always","condition":true,"id":"a","message":"a"},
        "antithesis_guidance":{"guidance_type":"numeric","maximize":true,"id":"g","guidance_data":1}
    }"#;
    let n = decode_antithesis(&[rec(1, json)]).expect("decodes");
    assert_eq!(n.events[0].payload, Payload::Unknown);
    assert_eq!(n.events[0].raw.bytes, json.as_bytes());
    assert!(
        n.schema.is_empty(),
        "neither wrapper contributes a schema entry from an ambiguous record"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Decode is total and panic-free over arbitrary byte frames.
    #[test]
    fn decode_antithesis_never_panics(
        records in prop::collection::vec(
            (any::<u64>(), prop::collection::vec(any::<u8>(), 0..64)),
            0..24,
        )
    ) {
        let recs: Vec<_> = records.into_iter().map(|(m, b)| (Moment(m), b)).collect();
        if let Ok(n) = decode_antithesis(&recs) {
            // Every event, recognized or not, keeps its original bytes.
            prop_assert_eq!(n.events.len(), recs.len());
            for (ev, (_, bytes)) in n.events.iter().zip(&recs) {
                prop_assert_eq!(&ev.raw.bytes, bytes);
            }
        }
    }

    /// Property identity is the message: assertions aggregate by message
    /// regardless of their per-site ids. Each event's identity is its message, and
    /// the schema holds exactly one entry per distinct message.
    #[test]
    fn assertions_aggregate_by_message_regardless_of_id(
        // Small alphabets so messages and ids collide across records.
        specs in prop::collection::vec(("[a-c]", "[x-z]"), 1..12),
    ) {
        let records: Vec<_> = specs
            .iter()
            .enumerate()
            .map(|(i, (msg, id))| {
                let json = format!(
                    r#"{{"antithesis_assert":{{"assert_type":"sometimes","condition":true,"message":"{msg}","id":"{id}"}}}}"#
                );
                (Moment(i as u64), json.into_bytes())
            })
            .collect();
        let n = decode_antithesis(&records).expect("decodes");

        for ((msg, _), ev) in specs.iter().zip(&n.events) {
            prop_assert_eq!(&ev.id, &ObservationId::Property(msg.clone()));
        }
        let distinct: std::collections::BTreeSet<&String> = specs.iter().map(|(m, _)| m).collect();
        prop_assert_eq!(n.schema.len(), distinct.len());
    }
}
