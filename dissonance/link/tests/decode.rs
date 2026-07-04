// SPDX-License-Identifier: AGPL-3.0-or-later
//! Decode goldens + totality (task 73 gate 2).
//!
//! The goldens pin the byte format the guest SDK actually emits (the mirror
//! golden: these are the exact bytes `guest/sdk` writes for each verb). The
//! totality proptest (>=256 cases) proves decode never panics and always yields a
//! well-formed [`GuestEvent`] over arbitrary event ids and bytes.

use explorer::{GuestEvent, Moment, Value};
use link::{
    KIND_ASSERT_HIT, KIND_ASSERT_VIOLATION, KIND_BUGGIFY, KIND_CATALOG, KIND_SETUP_COMPLETE,
    KIND_STATE, KIND_UNKNOWN, decode_event, decode_events,
};
use proptest::prelude::*;

// The wire constants, restated here as the golden's ground truth (they must match
// `guest/sdk/src/wire.rs`). Any drift breaks a golden on one side or the other.
const NS_SHIFT: u32 = 24;
const NS_ASSERT: u32 = 1;
const NS_STATE: u32 = 2;
const NS_BUGGIFY: u32 = 3;
const NS_LIFECYCLE: u32 = 4;

fn id(ns: u32, local: u32) -> u32 {
    (ns << NS_SHIFT) | local
}

fn attr<'a>(ev: &'a GuestEvent, k: &str) -> &'a Value {
    ev.attrs.get(k).expect("attribute present")
}

/// An `assert_sometimes`/`reachable` hit: `[DISP_HIT=0, detail_len=0]`.
#[test]
fn golden_assert_hit() {
    let ev = decode_event(id(NS_ASSERT, 5), &[0, 0, 0]);
    assert_eq!(ev.kind, KIND_ASSERT_HIT);
    assert_eq!(attr(&ev, "point"), &Value::UInt(5));
}

/// An `assert_always`/`unreachable` violation: `[DISP_VIOLATION=1, detail_len=0]`.
#[test]
fn golden_assert_violation() {
    let ev = decode_event(id(NS_ASSERT, 20), &[1, 0, 0]);
    assert_eq!(ev.kind, KIND_ASSERT_VIOLATION);
    assert_eq!(attr(&ev, "point"), &Value::UInt(20));
    assert_eq!(attr(&ev, "detail"), &Value::Bytes(vec![]));
}

/// A state register: `[op, value_le]`.
#[test]
fn golden_state_set_and_max() {
    let mut set = vec![0u8]; // STATE_SET
    set.extend_from_slice(&0x0102_0304_0506_0708u64.to_le_bytes());
    let ev = decode_event(id(NS_STATE, 40), &set);
    assert_eq!(ev.kind, KIND_STATE);
    assert_eq!(attr(&ev, "reg"), &Value::UInt(40));
    assert_eq!(attr(&ev, "op"), &Value::Str("set".into()));
    assert_eq!(attr(&ev, "value"), &Value::UInt(0x0102_0304_0506_0708));

    let mut max = vec![1u8]; // STATE_MAX
    max.extend_from_slice(&99u64.to_le_bytes());
    let ev = decode_event(id(NS_STATE, 40), &max);
    assert_eq!(attr(&ev, "op"), &Value::Str("max".into()));
    assert_eq!(attr(&ev, "value"), &Value::UInt(99));
}

/// A buggify result: `[fired]`.
#[test]
fn golden_buggify() {
    let ev = decode_event(id(NS_BUGGIFY, 50), &[1]);
    assert_eq!(ev.kind, KIND_BUGGIFY);
    assert_eq!(attr(&ev, "point"), &Value::UInt(50));
    assert_eq!(attr(&ev, "fired"), &Value::Bool(true));

    let ev = decode_event(id(NS_BUGGIFY, 50), &[0]);
    assert_eq!(attr(&ev, "fired"), &Value::Bool(false));
}

/// The lifecycle setup_complete event (empty payload).
#[test]
fn golden_setup_complete() {
    let ev = decode_event(id(NS_LIFECYCLE, 0), &[]);
    assert_eq!(ev.kind, KIND_SETUP_COMPLETE);
    assert!(ev.attrs.is_empty());
}

/// A `setup_complete` id with a **non-empty** payload is NOT a setup_complete —
/// it decodes to `unknown` (round A1: the event carries no payload, so stray
/// bytes are a malformed emission, not silently accepted).
#[test]
fn setup_complete_with_a_payload_is_unknown() {
    let ev = decode_event(id(NS_LIFECYCLE, 0), &[0xAB, 0xCD]);
    assert_eq!(ev.kind, KIND_UNKNOWN);
    assert_ne!(ev.kind, KIND_SETUP_COMPLETE);
}

/// The catalog declaration: `SDKC` magic + version + count.
#[test]
fn golden_catalog_header() {
    let mut blob = b"SDKC".to_vec();
    blob.push(1); // version
    blob.extend_from_slice(&3u32.to_le_bytes()); // count
    // (point entries follow but the decoded event surfaces only the header)
    let ev = decode_event(0, &blob);
    assert_eq!(ev.kind, KIND_CATALOG);
    assert_eq!(attr(&ev, "version"), &Value::UInt(1));
    assert_eq!(attr(&ev, "count"), &Value::UInt(3));
}

/// A catalog blob with an unrecognized wire version decodes to `unknown`, not a
/// catalog parsed under this version's field layout (round A3: the decode path
/// gates on `SDK_WIRE_VERSION`, matching the catalog fold in `catalog.rs`).
#[test]
fn catalog_with_a_future_version_is_unknown() {
    let mut blob = b"SDKC".to_vec();
    blob.push(2); // a future wire version — current is 1
    blob.extend_from_slice(&3u32.to_le_bytes());
    let ev = decode_event(0, &blob);
    assert_eq!(ev.kind, KIND_UNKNOWN);
    assert_ne!(ev.kind, KIND_CATALOG);
}

/// Malformed payloads for known namespaces, and unknown namespaces, fall back to
/// `unknown` carrying the raw id + bytes — nothing is dropped or panics.
#[test]
fn malformed_and_unknown_fall_back() {
    // A state event one byte short of its u64 value.
    let ev = decode_event(id(NS_STATE, 1), &[0, 0, 0]);
    assert_eq!(ev.kind, KIND_UNKNOWN);
    assert_eq!(attr(&ev, "event_id"), &Value::UInt(id(NS_STATE, 1) as u64));

    // An unknown namespace.
    let ev = decode_event(id(9, 7), &[1, 2, 3]);
    assert_eq!(ev.kind, KIND_UNKNOWN);
    assert_eq!(attr(&ev, "data"), &Value::Bytes(vec![1, 2, 3]));

    // An unknown assertion disposition (2) is not a hit or violation.
    let ev = decode_event(id(NS_ASSERT, 1), &[2, 0, 0]);
    assert_eq!(ev.kind, KIND_UNKNOWN);

    // Trailing bytes after a buggify flag are rejected to unknown.
    let ev = decode_event(id(NS_BUGGIFY, 1), &[1, 99]);
    assert_eq!(ev.kind, KIND_UNKNOWN);
}

/// decode_events preserves order and stamps.
#[test]
fn decode_events_preserves_order_and_stamps() {
    let raw = vec![
        (Moment(10), id(NS_ASSERT, 1), vec![0, 0, 0]),
        (Moment(20), id(NS_BUGGIFY, 2), vec![1]),
    ];
    let decoded = decode_events(&raw);
    assert_eq!(decoded.len(), 2);
    assert_eq!(decoded[0].0, Moment(10));
    assert_eq!(decoded[0].1.kind, KIND_ASSERT_HIT);
    assert_eq!(decoded[1].0, Moment(20));
    assert_eq!(decoded[1].1.kind, KIND_BUGGIFY);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Decode is total: any `(event_id, bytes)` yields a well-formed event and
    /// never panics. The decoded kind is always one of the known strings (the
    /// `unknown` fallback for anything the format does not recognize).
    #[test]
    fn decode_never_panics_and_is_well_formed(
        event_id in any::<u32>(),
        bytes in proptest::collection::vec(any::<u8>(), 0..512),
    ) {
        let ev = decode_event(event_id, &bytes);
        let known = [
            KIND_CATALOG, KIND_ASSERT_HIT, KIND_ASSERT_VIOLATION, KIND_STATE,
            KIND_BUGGIFY, KIND_SETUP_COMPLETE, KIND_UNKNOWN,
        ];
        prop_assert!(known.contains(&ev.kind.as_str()), "unexpected kind {}", ev.kind);
    }

    /// decode_events over an arbitrary raw stream never panics and preserves length.
    #[test]
    fn decode_events_is_total(
        raw in proptest::collection::vec(
            (any::<u64>(), any::<u32>(), proptest::collection::vec(any::<u8>(), 0..64)),
            0..32,
        ),
    ) {
        let raw: Vec<(Moment, u32, Vec<u8>)> =
            raw.into_iter().map(|(m, id, b)| (Moment(m), id, b)).collect();
        let decoded = decode_events(&raw);
        prop_assert_eq!(decoded.len(), raw.len());
    }
}
