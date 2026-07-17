// SPDX-License-Identifier: AGPL-3.0-or-later
//! Artifact-level load validation: a persisted [`Normalized`] is only obtainable
//! through its validated `try_from`, which **re-decodes the artifact's own preserved
//! bytes and requires structural equality** — so *loadable* is definitionally *what a
//! live decode produces* (the crate root's decoder-pinning invariant).
//!
//! These are the r14 judge probes, **inverted**: each once demonstrated that an
//! artifact carrying a value the decoders never mint still loaded; each now asserts
//! that the same artifact is rejected with a typed error. A tampered stream the
//! decoder itself refuses surfaces that decoder's own error (`MixedOperations`);
//! everything else that a live decode would not have produced is a typed
//! `ArtifactDivergedFromDecode`. The setup status-fabrication guard (F2, `hm-jyj`) and
//! the compile-time proof that only `Normalized` is publicly deserializable ride
//! along.

use explorer::Moment;
use sdk_events::{
    Classification, DeclaredPoint, Expectation, NS_ASSERT, NS_STATE, Normalized, Payload, SdkError,
    UpdateOp, ValueShape, decode_antithesis, decode_binary, encode_v2_declaration,
};
use serde_json::json;

/// A valid binary-v2 artifact: one `max`-declared state coordinate with two `max`
/// firings (live ordinals 1 and 2). The starting point for the mutation probes.
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

/// Assert an artifact fails to load because it is not what a live decode of its own
/// bytes produces (the `ArtifactDivergedFromDecode` message).
fn assert_diverges(v: serde_json::Value) {
    let err = load(v).expect_err("must reject").to_string();
    assert!(
        err.contains("diverges from a live decode"),
        "expected a decode-divergence rejection, got: {err}"
    );
}

// --- The r14 probes, inverted: each defect is now a typed rejection --------------

/// Probe A — a binary state event's payload swapped to `Guidance`. The raw record is
/// still a `max` state firing, so re-decoding it mints `State`, and the persisted
/// `Guidance` (which only the Antithesis decoder produces) diverges. Closes the
/// `payload_classification` collapse the enumerative check could not see (`State` and
/// `Guidance` share one `Classification`).
#[test]
fn probe_a_binary_state_payload_swapped_to_guidance_is_rejected() {
    let mut v = max_state_artifact();
    assert!(v["events"][0]["payload"]["State"].is_object());
    v["events"][0]["payload"] = json!({"Guidance": {"op": "Max", "token": "123"}});
    assert_diverges(v);
}

/// Probe B1 — a `min` firing at an *undeclared* state coordinate, "upgraded" from the
/// raw the live decoder keeps (no declaration blesses `min`). Re-decoding the same
/// raw yields `Unknown`, so the persisted `State{min}` diverges.
#[test]
fn probe_b1_undeclared_min_firing_upgraded_from_raw_is_rejected() {
    let decl = encode_v2_declaration(&[DeclaredPoint {
        namespace: NS_ASSERT,
        local: 1,
        name: "a".into(),
        classification: Classification::Occurrence,
        value_shape: None,
        base_op: None,
        expectation: None,
    }])
    .expect("valid declaration");
    let mut min_firing = vec![2u8]; // STATE_MIN
    min_firing.extend_from_slice(&7u64.to_le_bytes());
    let state_id = ((NS_STATE as u32) << 24) | 5;
    let n =
        decode_binary(&[(Moment(0), 0, decl), (Moment(5), state_id, min_firing)]).expect("decodes");
    assert_eq!(n.events[0].payload, Payload::Unknown, "live keeps it raw");

    let mut v = serde_json::to_value(&n).unwrap();
    v["events"][0]["payload"] = json!({"State": {"op": "Min", "value": 7}});
    assert_diverges(v);
}

/// Probe B2 — an Antithesis setup lifecycle event with its required schema entry
/// deleted. `decode_setup` always registers the entry alongside the event, so
/// re-decoding the setup record restores it and the entry-less persisted schema
/// diverges.
#[test]
fn probe_b2_setup_event_without_its_lifecycle_entry_is_rejected() {
    let n =
        decode_antithesis(&[(Moment(1), br#"{"antithesis_setup":{}}"#.to_vec())]).expect("decodes");
    assert_eq!(
        n.schema.entries().len(),
        1,
        "live registers the setup entry"
    );

    let mut v = serde_json::to_value(&n).unwrap();
    v["schema"]["entries"] = json!([]);
    assert_diverges(v);
}

/// Probe C — shifted, non-contiguous ordinals (99, 200) where a live decode assigns
/// the persisted vector positions (1, 2). Re-decoding the reconstructed stream
/// re-numbers them contiguously, so the shifted ordinals diverge — the contiguous
/// rollout-local-ordinal contract, enforced by construction.
#[test]
fn probe_c_shifted_noncontiguous_ordinals_are_rejected() {
    let mut v = max_state_artifact();
    assert_eq!(v["events"][0]["ordinal"], 1);
    assert_eq!(v["events"][1]["ordinal"], 2);
    v["events"][0]["ordinal"] = json!(99);
    v["events"][1]["ordinal"] = json!(200);
    assert_diverges(v);
}

/// Probe D — a `raw` record contradicting the evidence it vouches for (a different
/// source, no event id, unrelated bytes). Because the load reconstructs the stream
/// *from* `raw`, a binary event with no `event_id` cannot be placed back on the wire
/// at all — it can be no live decode's output.
#[test]
fn probe_d_corrupted_raw_provenance_is_rejected() {
    let mut v = max_state_artifact();
    v["events"][0]["raw"] = json!({
        "source": "AntithesisJson",
        "event_id": null,
        "bytes": [1, 2, 3],
    });
    assert_diverges(v);
}

/// A `raw` with an intact `event_id` but *unrelated bytes* also diverges: re-decoding
/// those bytes yields a different payload (or `Unknown`) than the one persisted.
#[test]
fn probe_d2_raw_bytes_contradicting_the_payload_are_rejected() {
    let mut v = max_state_artifact();
    // Keep a valid state event_id, but replace the firing bytes with garbage that
    // decodes to `Unknown`, not the persisted `State{max, 7}`.
    v["events"][0]["raw"]["bytes"] = json!([0xFF, 0xFF]);
    assert_diverges(v);
}

// --- The decoder's own error still surfaces on load ------------------------------

#[test]
fn a_raw_set_firing_at_a_max_declared_coordinate_is_mixed_operations() {
    // Here the raw bytes themselves are a `set` firing at a `max`-declared coordinate,
    // so re-decoding the reconstructed stream makes the *decoder* raise
    // `MixedOperations` — its own error propagates, no divergence needed.
    let mut v = max_state_artifact();
    let mut set_firing = vec![0u8]; // STATE_SET
    set_firing.extend_from_slice(&7u64.to_le_bytes());
    v["events"][0]["payload"] = json!({"State": {"op": "Set", "value": 7}});
    v["events"][0]["raw"]["bytes"] = json!(set_firing);
    let err = load(v).expect_err("must reject").to_string();
    assert!(
        err.contains("conflicting base operations"),
        "the decoder's own MixedOperations must surface, got: {err}"
    );
}

// --- Encode/decode symmetry (independent of load) --------------------------------

#[test]
fn an_expectation_on_a_state_point_fails_to_encode() {
    // The byte codec must not mint a declaration its own decoder would refuse.
    let encoded = encode_v2_declaration(&[DeclaredPoint {
        namespace: NS_STATE,
        local: 1,
        name: "reg".into(),
        classification: Classification::State,
        value_shape: Some(ValueShape::U64),
        base_op: Some(UpdateOp::Set),
        expectation: Some(Expectation::MustHit),
    }]);
    assert!(matches!(
        encoded,
        Err(SdkError::UnsupportedDeclaration { .. })
    ));
}

// --- Positive control: a valid artifact loads ------------------------------------

#[test]
fn a_live_decoded_artifact_loads_unchanged() {
    let n = max_state_artifact();
    let loaded = load(n.clone()).expect("a live-decode output loads");
    // And re-serializing the loaded artifact reproduces the same bytes.
    assert_eq!(serde_json::to_value(&loaded).unwrap(), n);
}

// --- F2: setup status fabrication (bead hm-jyj) ----------------------------------

#[test]
fn f2_present_but_non_string_setup_status_stays_raw() {
    // A setup record whose `status` is present but not a string is malformed; rather
    // than fabricate a `complete`/named lifecycle point, it is preserved raw (mirrors
    // `site_of`), so no lifecycle schema entry is minted.
    let n = decode_antithesis(&[(Moment(1), br#"{"antithesis_setup":{"status":7}}"#.to_vec())])
        .expect("decodes without panicking");
    assert_eq!(n.events.len(), 1);
    assert_eq!(n.events[0].payload, Payload::Unknown);
    assert!(
        n.schema.entries().is_empty(),
        "a fabricated setup status mints no lifecycle entry"
    );
    assert_eq!(
        n.events[0].raw.bytes,
        br#"{"antithesis_setup":{"status":7}}"#
    );
    // The raw-carrying artifact round-trips (re-decoding its raw yields itself).
    let back = serde_json::from_value::<Normalized>(serde_json::to_value(&n).unwrap()).unwrap();
    assert_eq!(back, n);
}

// --- API ruling: `Normalized` is the only publicly deserializable artifact --------

/// A compile-time detector for `T: DeserializeOwned` as a runtime bool (autoref
/// specialization): `ViaDeserialize::probe` binds on `Probe<T>` directly and is chosen
/// when `T: DeserializeOwned`; otherwise resolution falls back through an autoref to
/// `ViaFallback::probe` on `&Probe<T>`.
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

/// The API ruling, enforced mechanically: the validated [`Normalized`] artifact is the
/// *only* publicly-deserializable entry. `SdkEvent`/`SdkSchema` must not carry a bare
/// `Deserialize` — re-deriving one flips a constant here and fails the test. (The
/// `cargo public-api` snapshot runs at `-sss`, which omits auto-derived impls, so the
/// removal is invisible there and can only be enforced by a bound like this.)
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
    // Component value types still deserialize — they have no independent load path.
    assert!(is_deserializable!(sdk_events::SchemaEntry));
    assert!(is_deserializable!(Payload));
}
