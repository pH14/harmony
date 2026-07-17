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
    // The entry's human name is the message, verbatim.
    assert_eq!(entry.name.as_deref(), Some("balance stays non-negative"));
    // The schema is non-empty and its `entries()` view carries the one entry.
    assert!(!n.schema.is_empty());
    assert_eq!(n.schema.entries().len(), 1);
    assert_eq!(&n.schema.entries()[0], entry);
}

#[test]
fn every_assertion_verb_maps_to_its_type() {
    for (verb, expected) in [
        ("always", AssertType::Always),
        ("sometimes", AssertType::Sometimes),
        ("reachable", AssertType::Reachable),
        ("unreachable", AssertType::Unreachable),
    ] {
        let json = format!(
            r#"{{"antithesis_assert":{{"assert_type":"{verb}","condition":true,"message":"m-{verb}"}}}}"#
        );
        let n = decode_antithesis(&[rec(1, &json)]).expect("decodes");
        match n.events[0].payload {
            Payload::Assertion { assert_type, .. } => {
                assert_eq!(assert_type, Some(expected), "verb `{verb}`");
            }
            ref other => panic!("{other:?}"),
        }
    }
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
    // …but it is NOT reducible: the numeric value is an unvalidated token and its
    // bounded representation / total order is not yet versioned in the schema, so
    // it stays report-only (else a consumer could reduce under its own limits and
    // replay differently).
    assert!(
        !entry.is_reducible_state(),
        "numeric guidance is report-only until its bounded representation is versioned"
    );
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
fn setup_record_is_a_lifecycle_occurrence_registered_in_the_schema() {
    let json = r#"{"antithesis_setup":{"status":"complete","details":{}}}"#;
    let n = decode_antithesis(&[rec(0, json)]).expect("decodes");
    assert_eq!(
        n.events[0].payload,
        Payload::Lifecycle {
            name: "setup_complete".into()
        }
    );
    // The setup event's identity is registered in the schema (occurrence), so the
    // batch can validate/materialize it like an assertion or guidance event.
    let entry = n
        .schema
        .entry(&n.events[0].id)
        .expect("setup identity registered in schema");
    assert_eq!(entry.classification, Classification::Occurrence);
    assert_eq!(entry.base_op, None);
    // The identity is the disjoint `Lifecycle` variant, never a `Property`.
    assert_eq!(
        n.events[0].id,
        ObservationId::Lifecycle("antithesis.setup".into())
    );
}

#[test]
fn the_setup_identity_cannot_be_aliased_by_a_property_message() {
    let setup = r#"{"antithesis_setup":{"status":"complete"}}"#;

    // Direction 1: an assertion whose MESSAGE equals the setup sentinel gets a
    // `Property` id, disjoint from the setup's `Lifecycle` id — no aliasing.
    let assertion = r#"{"antithesis_assert":{"assert_type":"always","condition":true,"message":"antithesis.setup"}}"#;
    let n = decode_antithesis(&[rec(1, assertion), rec(2, setup)]).expect("decodes");
    assert_ne!(n.events[0].id, n.events[1].id);
    assert_eq!(
        n.events[0].id,
        ObservationId::Property("antithesis.setup".into())
    );
    assert_eq!(
        n.events[1].id,
        ObservationId::Lifecycle("antithesis.setup".into())
    );
    assert_eq!(n.schema.len(), 2, "two disjoint identities → two entries");

    // Direction 2: a guidance record whose id equals the sentinel. Under a shared
    // property-string identity this would collide with the setup's occurrence entry
    // and raise a spurious ClassificationConflict; disjoint variants decode both.
    let guidance = r#"{"antithesis_guidance":{"guidance_type":"numeric","maximize":true,"id":"antithesis.setup","guidance_data":1}}"#;
    let n =
        decode_antithesis(&[rec(1, guidance), rec(2, setup)]).expect("disjoint ids → no conflict");
    assert_ne!(n.events[0].id, n.events[1].id);
    assert_eq!(n.schema.len(), 2);
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

#[test]
fn a_frame_repeating_a_wrapper_key_is_ambiguous_and_preserved_raw() {
    // `serde_json::Value` would keep only the last member; the decoder must instead
    // see the duplicate and preserve the frame raw rather than confidently emit one
    // assertion and drop the other.
    let json = r#"{"antithesis_assert":{"assert_type":"always","condition":true,"message":"first"},
                   "antithesis_assert":{"assert_type":"always","condition":false,"message":"second"}}"#;
    let n = decode_antithesis(&[rec(1, json)]).expect("decodes");
    assert_eq!(n.events[0].payload, Payload::Unknown);
    assert_eq!(n.events[0].raw.bytes, json.as_bytes());
    assert!(n.schema.is_empty());
}

#[test]
fn a_frame_with_any_duplicate_top_level_key_is_preserved_raw() {
    // Even a non-wrapper duplicate means a member was silently dropped on the
    // normalized path — the frame can't be faithfully normalized.
    let json = r#"{"antithesis_assert":{"assert_type":"always","condition":true,"message":"m"},"x":1,"x":2}"#;
    let n = decode_antithesis(&[rec(1, json)]).expect("decodes");
    assert_eq!(n.events[0].payload, Payload::Unknown);
    assert!(n.schema.is_empty());
}

#[test]
fn a_nested_duplicate_key_is_preserved_raw_not_last_write_wins() {
    // Duplicate `maximize` inside guidance: last-write-wins would turn true-then-
    // false into a Min update, letting malformed input choose state semantics. The
    // recursive detector catches it and preserves the frame raw.
    let json = r#"{"antithesis_guidance":{"guidance_type":"numeric","maximize":true,"maximize":false,"id":"g","guidance_data":1}}"#;
    let n = decode_antithesis(&[rec(1, json)]).expect("decodes");
    assert_eq!(n.events[0].payload, Payload::Unknown);
    assert!(
        n.schema.is_empty(),
        "no state semantics chosen from an ambiguous frame"
    );

    // A duplicate key nested even deeper (inside location) is caught too.
    let deep = r#"{"antithesis_assert":{"assert_type":"always","condition":true,"message":"m","location":{"file":"a","file":"b"}}}"#;
    let n = decode_antithesis(&[rec(1, deep)]).expect("decodes");
    assert_eq!(n.events[0].payload, Payload::Unknown);

    // A duplicate nested inside a JSON *array* is caught: the recursive walk must
    // OR the duplicate flag across array elements, not AND it away.
    let in_array = r#"{"antithesis_assert":{"assert_type":"always","condition":true,"message":"m","details":[{"k":1,"k":2}]}}"#;
    let n = decode_antithesis(&[rec(1, in_array)]).expect("decodes");
    assert_eq!(n.events[0].payload, Payload::Unknown);
}

/// A well-formed array (no duplicate keys) inside a wrapper does not falsely trip
/// the detector — the array element decodes normally.
#[test]
fn a_clean_nested_array_does_not_trip_the_duplicate_detector() {
    let json = r#"{"antithesis_assert":{"assert_type":"always","condition":true,"message":"m","details":[{"a":1},{"b":2}]}}"#;
    let n = decode_antithesis(&[rec(1, json)]).expect("decodes");
    match n.events[0].payload {
        Payload::Assertion { .. } => {}
        ref other => panic!("clean array should decode as an assertion, got {other:?}"),
    }
}

#[test]
fn a_large_numeric_token_does_not_trip_the_duplicate_detector() {
    // A precise 40-digit number must scan cleanly (no false duplicate under
    // serde_json's arbitrary_precision number representation).
    let json = r#"{"antithesis_guidance":{"guidance_type":"numeric","maximize":true,"id":"g","guidance_data":1234567890123456789012345678901234567890}}"#;
    let n = decode_antithesis(&[rec(1, json)]).expect("decodes");
    match &n.events[0].payload {
        Payload::Guidance { token, .. } => {
            assert_eq!(
                token.as_ref().unwrap().as_str(),
                "1234567890123456789012345678901234567890"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_oversized_site_coordinate_is_preserved_not_truncated() {
    // A `begin_line` above u32::MAX must not wrap to a colliding small value.
    let big = u32::MAX as u64 + 1; // 4294967296
    let json = format!(
        r#"{{"antithesis_assert":{{"assert_type":"always","condition":true,"message":"m","location":{{"file":"a.rs","function":"f","begin_line":{big},"begin_column":7}}}}}}"#
    );
    let n = decode_antithesis(&[rec(1, &json)]).expect("decodes");
    let site = n.events[0].site.as_ref().expect("site");
    assert_eq!(
        site.begin_line, big,
        "coordinate preserved exactly, not truncated"
    );
    assert_eq!(site.begin_column, 7);
}

#[test]
fn a_malformed_location_coordinate_keeps_the_record_raw_not_a_fabricated_zero() {
    let assertion = |loc: &str| {
        format!(
            r#"{{"antithesis_assert":{{"assert_type":"always","condition":true,"message":"m","location":{loc}}}}}"#
        )
    };

    // A genuine zero (or missing) coordinate is a valid site — it decodes.
    let n = decode_antithesis(&[rec(
        1,
        &assertion(r#"{"file":"a.rs","function":"f","begin_line":0,"begin_column":0}"#),
    )])
    .expect("decodes");
    let zero_site = n.events[0]
        .site
        .clone()
        .expect("real-zero location is a site");
    assert_eq!(zero_site.begin_line, 0);
    assert_eq!(zero_site.begin_column, 0);
    assert!(matches!(n.events[0].payload, Payload::Assertion { .. }));

    // Each malformed coordinate keeps the whole record RAW (never a fabricated 0
    // that would collide with the genuine-zero site above).
    for bad in [
        r#"{"begin_line":18446744073709551616}"#, // beyond u64::MAX
        r#"{"begin_line":-5}"#,                   // negative
        r#"{"begin_line":1.5}"#,                  // non-integer
        r#"{"begin_line":"x"}"#,                  // non-number
        r#"{"begin_column":18446744073709551616}"#, // column, too
        r#"{"file":42}"#,                         // non-string file
        r#"7"#,                                   // non-object location
    ] {
        let n = decode_antithesis(&[rec(1, &assertion(bad))]).expect("decodes");
        assert_eq!(
            n.events[0].payload,
            Payload::Unknown,
            "malformed location `{bad}` must stay raw, not a fabricated site"
        );
        assert!(
            n.schema.is_empty(),
            "a raw record contributes no schema entry for `{bad}`"
        );
    }

    // The same guard applies to guidance records.
    let guidance = r#"{"antithesis_guidance":{"guidance_type":"numeric","maximize":true,"id":"g","guidance_data":1,"location":{"begin_line":-1}}}"#;
    let n = decode_antithesis(&[rec(1, guidance)]).expect("decodes");
    assert_eq!(n.events[0].payload, Payload::Unknown);
    assert!(n.schema.is_empty());
}

#[test]
fn a_present_non_string_site_id_keeps_the_record_raw() {
    // A present but non-string `id` (with a valid message) is malformed — kept raw
    // rather than collapsed to a `None`-id site that would merge distinct bad ids.
    for bad_id in ["7", "true", "null", "[1]", "{}"] {
        let json = format!(
            r#"{{"antithesis_assert":{{"assert_type":"always","condition":true,"message":"m","id":{bad_id}}}}}"#
        );
        let n = decode_antithesis(&[rec(1, &json)]).expect("decodes");
        assert_eq!(
            n.events[0].payload,
            Payload::Unknown,
            "non-string id `{bad_id}` must stay raw"
        );
        assert!(n.schema.is_empty());
    }

    // A genuinely absent id still normalizes (no id, no location → no site)…
    let absent = r#"{"antithesis_assert":{"assert_type":"always","condition":true,"message":"m"}}"#;
    let n = decode_antithesis(&[rec(1, absent)]).expect("decodes");
    assert!(matches!(n.events[0].payload, Payload::Assertion { .. }));
    assert!(n.events[0].site.is_none());

    // …and a present string id normalizes with that id — both distinct from raw.
    let with_id = r#"{"antithesis_assert":{"assert_type":"always","condition":true,"message":"m","id":"site-x"}}"#;
    let n = decode_antithesis(&[rec(1, with_id)]).expect("decodes");
    assert_eq!(
        n.events[0].site.as_ref().unwrap().id.as_deref(),
        Some("site-x")
    );
}

#[test]
fn a_present_non_string_message_keeps_the_record_raw_not_id_derived(/* hm-b2g (2) */) {
    // A present-but-non-string `message` is malformed. It must NOT silently fall
    // back to the `id` as the aggregated property identity (which would let a
    // corrupt record masquerade as a well-formed property keyed by its site id);
    // the whole record is preserved raw (mirrors the malformed-`id` discipline).
    for bad_message in ["7", "true", "null", "[1]", "{}"] {
        let json = format!(
            r#"{{"antithesis_assert":{{"assert_type":"always","condition":true,"message":{bad_message},"id":"prop-x"}}}}"#
        );
        let n = decode_antithesis(&[rec(1, &json)]).expect("decodes");
        assert_eq!(
            n.events[0].payload,
            Payload::Unknown,
            "non-string message `{bad_message}` (even with a valid id) must stay raw"
        );
        assert!(
            n.schema.is_empty(),
            "a malformed message mints no id-derived property entry"
        );
    }

    // The same holds for numeric guidance: a malformed message keeps it raw rather
    // than falling back to the id as the guidance property.
    let guidance = r#"{"antithesis_guidance":{"guidance_type":"numeric","maximize":true,"message":7,"id":"g","guidance_data":1}}"#;
    let n = decode_antithesis(&[rec(1, guidance)]).expect("decodes");
    assert_eq!(n.events[0].payload, Payload::Unknown);
    assert!(n.schema.is_empty());

    // A genuinely absent message still falls back to the string `id` (the defensive
    // fallback the contract preserves) — distinct from the malformed case above.
    let absent = r#"{"antithesis_assert":{"assert_type":"always","condition":true,"id":"prop-x"}}"#;
    let n = decode_antithesis(&[rec(1, absent)]).expect("decodes");
    assert_eq!(n.events[0].id, ObservationId::Property("prop-x".into()));
}

#[test]
fn an_unsupported_or_malformed_assert_verb_keeps_the_record_raw(/* hm-b2g (3) */) {
    // A present `assert_type` that is not an exactly-supported verb — an unknown
    // string, a non-string value, or a `reachability` whose `display_type` is not
    // exactly `Reachable`/`Unreachable` — is malformed. The decoder must keep the
    // frame raw rather than mint an assertion with a `None` verb (or default a
    // reachability firing to `Reachable`, silently dropping a MustNotHit).
    let raw_frames = [
        // unknown verb string
        r#"{"antithesis_assert":{"assert_type":"maybe","condition":true,"message":"m"}}"#,
        // non-string assert_type
        r#"{"antithesis_assert":{"assert_type":7,"condition":true,"message":"m"}}"#,
        // reachability with an absent display_type (the old default-to-Reachable hole)
        r#"{"antithesis_assert":{"assert_type":"reachability","condition":false,"message":"m"}}"#,
        // reachability with an unrecognized display_type
        r#"{"antithesis_assert":{"assert_type":"reachability","display_type":"Somewhere","condition":false,"message":"m"}}"#,
        // reachability with a non-string display_type
        r#"{"antithesis_assert":{"assert_type":"reachability","display_type":7,"condition":false,"message":"m"}}"#,
    ];
    for json in raw_frames {
        let n = decode_antithesis(&[rec(1, json)]).expect("decodes");
        assert_eq!(
            n.events[0].payload,
            Payload::Unknown,
            "`{json}` must stay raw, not mint a defaulted/None-verb assertion"
        );
        assert!(
            n.schema.is_empty(),
            "an unsupported verb mints no schema entry: `{json}`"
        );
    }

    // The exactly-supported reachability combos still normalize to their verb —
    // `Reachable` no longer arrives by defaulting, only from the exact display token.
    for (display, expected) in [
        ("Reachable", AssertType::Reachable),
        ("Unreachable", AssertType::Unreachable),
    ] {
        let json = format!(
            r#"{{"antithesis_assert":{{"assert_type":"reachability","display_type":"{display}","condition":true,"message":"m"}}}}"#
        );
        let n = decode_antithesis(&[rec(1, &json)]).expect("decodes");
        match n.events[0].payload {
            Payload::Assertion { assert_type, .. } => {
                assert_eq!(assert_type, Some(expected), "display `{display}`")
            }
            ref other => panic!("{other:?}"),
        }
    }
}

#[test]
fn a_malformed_setup_wrapper_does_not_fabricate_lifecycle_evidence() {
    // A scalar/null setup wrapper must not normalize to a setup_complete occurrence
    // via field defaults.
    for json in [
        r#"{"antithesis_setup":null}"#,
        r#"{"antithesis_setup":7}"#,
        r#"{"antithesis_setup":"complete"}"#,
        r#"{"antithesis_setup":[]}"#,
    ] {
        let n = decode_antithesis(&[rec(1, json)]).expect("decodes");
        assert_eq!(
            n.events[0].payload,
            Payload::Unknown,
            "`{json}` must stay raw, not become a lifecycle occurrence"
        );
    }
    // A well-formed setup wrapper still decodes.
    let n = decode_antithesis(&[rec(1, r#"{"antithesis_setup":{"status":"complete"}}"#)])
        .expect("decodes");
    assert_eq!(
        n.events[0].payload,
        Payload::Lifecycle {
            name: "setup_complete".into()
        }
    );
}

#[test]
fn a_malformed_assert_or_guidance_wrapper_is_preserved_raw() {
    for json in [
        r#"{"antithesis_assert":7}"#,
        r#"{"antithesis_assert":null}"#,
        r#"{"antithesis_guidance":"numeric"}"#,
    ] {
        let n = decode_antithesis(&[rec(1, json)]).expect("decodes");
        assert_eq!(
            n.events[0].payload,
            Payload::Unknown,
            "`{json}` must stay raw"
        );
        assert!(n.schema.is_empty());
    }
}

#[test]
fn events_sharing_an_anchor_moment_are_ordered_by_ordinal_not_moment(/* hm-ynt */) {
    // An SDK event's `Moment` is a V-time *anchor lower bound*, not the emission
    // instant: several records drained at one `run_until` anchor share that Moment.
    // The total order and the included-count cut coordinate is the `ordinal` (the
    // rollout-local SDK-vector position), never the Moment — so distinct events at
    // one anchor get distinct, contiguous ordinals and the anchor never collapses
    // or reorders them.
    let anchor = 5; // one shared anchor Moment for every record
    let records = [
        r#"{"antithesis_assert":{"assert_type":"sometimes","condition":true,"message":"a"}}"#,
        r#"{"antithesis_assert":{"assert_type":"sometimes","condition":true,"message":"b"}}"#,
        r#"{"antithesis_assert":{"assert_type":"sometimes","condition":true,"message":"c"}}"#,
    ]
    .map(|j| rec(anchor, j));
    let n = decode_antithesis(&records).expect("decodes");

    assert_eq!(n.events.len(), 3);
    for (i, ev) in n.events.iter().enumerate() {
        assert_eq!(ev.moment, Moment(anchor), "all share the one anchor Moment");
        assert_eq!(
            ev.ordinal, i as u64,
            "ordinal is the total order — contiguous, source-vector position"
        );
    }
    // Identity is by message, in SDK-vector order — the shared Moment carries no
    // ordering weight.
    assert_eq!(n.events[0].id, ObservationId::Property("a".into()));
    assert_eq!(n.events[1].id, ObservationId::Property("b".into()));
    assert_eq!(n.events[2].id, ObservationId::Property("c".into()));
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

    /// A duplicate field injected anywhere in an otherwise-valid wrapper makes the
    /// whole frame ambiguous → `Payload::Unknown` (never a confident normalization
    /// built from a silently-dropped member).
    #[test]
    fn any_injected_duplicate_field_makes_the_frame_unknown(dupkey in "[a-z]{1,6}") {
        // The `dupkey` appears twice inside the guidance wrapper by construction.
        let json = format!(
            r#"{{"antithesis_guidance":{{"guidance_type":"numeric","maximize":true,"id":"g","guidance_data":1,"{dupkey}":1,"{dupkey}":2}}}}"#
        );
        let n = decode_antithesis(&[(Moment(0), json.into_bytes())]).expect("decodes");
        prop_assert_eq!(&n.events[0].payload, &Payload::Unknown);
    }
}
