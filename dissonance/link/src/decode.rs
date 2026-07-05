// SPDX-License-Identifier: AGPL-3.0-or-later
//! Link-tier event decode: raw `(Moment, event_id, bytes)` tuples (captured by
//! the host EventSink, `Moment`-stamped at the count they surfaced) → typed
//! `(Moment, GuestEvent)` for [`RunTrace::events`](explorer::RunTrace).
//!
//! **Total and panic-free on arbitrary bytes** (task 73 gate 2): every event id
//! and payload the guest could emit — or a hostile/garbage stream could forge —
//! decodes to *some* [`GuestEvent`], never a panic and never an out-of-bounds
//! read. An unrecognized namespace or a malformed payload for a known one falls
//! back to a `kind = "unknown"` event carrying the raw `event_id` and bytes, so
//! nothing is silently dropped.

use std::collections::BTreeMap;

use explorer::{GuestEvent, Moment, Value};

use crate::read::Reader;
use crate::wire;

/// Decoded event kind: the catalog declaration (informational).
pub const KIND_CATALOG: &str = "catalog";
/// Decoded event kind: a positive assertion hit (`sometimes`/`reachable`).
pub const KIND_ASSERT_HIT: &str = "assert_hit";
/// Decoded event kind: an assertion violation (`always`/`unreachable`).
pub const KIND_ASSERT_VIOLATION: &str = "assert_violation";
/// Decoded event kind: an IJON state-register report.
pub const KIND_STATE: &str = "state";
/// Decoded event kind: a buggify result.
pub const KIND_BUGGIFY: &str = "buggify";
/// Decoded event kind: the `setup_complete` lifecycle point.
pub const KIND_SETUP_COMPLETE: &str = "setup_complete";
/// Decoded event kind: an unrecognized namespace or malformed payload.
pub const KIND_UNKNOWN: &str = "unknown";

/// Decode a whole captured event stream into typed, timestamped events — the
/// value [`RunTrace::events`](explorer::RunTrace) holds. Order-preserving.
///
/// If the stream's catalog declaration names an **unsupported wire version**, the
/// WHOLE stream is decoded as `unknown` — not just the catalog event. A future
/// version may lay out every event's payload differently, so decoding the later
/// events under *this* version's field layout would silently mis-key them (e.g. a
/// v2 assert with an extra header field would decode as a v1 assert at the wrong
/// point). Tainting the whole stream keeps the raw `event_id`/bytes recoverable
/// and refuses to invent typed events under a layout we cannot read.
pub fn decode_events(raw: &[(Moment, u32, Vec<u8>)]) -> Vec<(Moment, GuestEvent)> {
    if stream_declares_unsupported_version(raw) {
        return raw
            .iter()
            .map(|(at, id, bytes)| (*at, unknown(*id, bytes)))
            .collect();
    }
    raw.iter()
        .map(|(at, id, bytes)| (*at, decode_event(*id, bytes)))
        .collect()
}

/// Whether the stream carries a catalog declaration with a **complete, parseable
/// header** (magic + version + count) that names a wire version other than
/// [`wire::SDK_WIRE_VERSION`]. A malformed catalog — bad magic OR a **truncated**
/// header (version or count missing) — does NOT taint the stream: a truncated
/// frame has said nothing trustworthy about the layout, so it decodes to a single
/// `unknown` catalog event via [`decode_catalog`] and the rest of the stream is
/// unaffected. Requiring the full header (mirroring `decode_catalog`'s own parse)
/// keeps the version-taint a deliberate, complete "future version" claim rather
/// than a byte-count accident.
fn stream_declares_unsupported_version(raw: &[(Moment, u32, Vec<u8>)]) -> bool {
    raw.iter().any(|(_, id, bytes)| {
        if *id != wire::CATALOG_EVENT_ID {
            return false;
        }
        let mut r = Reader::new(bytes);
        let magic = r.u32();
        let version = r.u8();
        let count = r.u32();
        magic == Some(wire::CATALOG_MAGIC)
            && count.is_some() // a complete header — not a truncated frame
            && matches!(version, Some(v) if v != wire::SDK_WIRE_VERSION)
    })
}

/// Decode one raw `(event_id, bytes)` into a typed [`GuestEvent`]. Total: any
/// input yields a `GuestEvent` (a `KIND_UNKNOWN` fallback for anything the format
/// does not recognize).
pub fn decode_event(event_id: u32, bytes: &[u8]) -> GuestEvent {
    let (ns, local) = wire::split(event_id);
    match ns {
        wire::NS_CONTROL if local == 0 => decode_catalog(bytes),
        wire::NS_ASSERT => decode_assert(local, bytes),
        wire::NS_STATE => decode_state(local, bytes),
        wire::NS_BUGGIFY => decode_buggify(local, bytes),
        // `setup_complete` carries NO payload; require it empty (like the other
        // arms' `at_end` check), else a stray-payload id is `unknown`, not a
        // silently-accepted setup_complete (round A1).
        wire::NS_LIFECYCLE if local == wire::LIFECYCLE_SETUP_COMPLETE && bytes.is_empty() => {
            event(KIND_SETUP_COMPLETE, [])
        }
        _ => unknown(event_id, bytes),
    }
}

/// The catalog declaration: magic + version + point count. The declared point
/// *set* is parsed separately by the catalog fold ([`crate::Catalog`]); here we
/// only surface an informational event for the stream.
fn decode_catalog(bytes: &[u8]) -> GuestEvent {
    let mut r = Reader::new(bytes);
    let ok = r.u32() == Some(wire::CATALOG_MAGIC);
    let version = r.u8();
    let count = r.u32();
    match (ok, version, count) {
        // Gate on the wire version (like the catalog fold in `catalog.rs`): a
        // future/unknown version is `unknown`, not a catalog decoded under this
        // version's field layout (which would mis-key the report).
        (true, Some(version), Some(count)) if version == wire::SDK_WIRE_VERSION => event(
            KIND_CATALOG,
            [
                ("version", Value::UInt(version as u64)),
                ("count", Value::UInt(count as u64)),
            ],
        ),
        _ => unknown(wire::CATALOG_EVENT_ID, bytes),
    }
}

/// An assertion event: `[disposition u8][detail_len u16][detail]`.
fn decode_assert(local: u32, bytes: &[u8]) -> GuestEvent {
    let mut r = Reader::new(bytes);
    let Some(disp) = r.u8() else {
        return unknown(assert_id(local), bytes);
    };
    let Some(detail) = r.bytes_lp16() else {
        return unknown(assert_id(local), bytes);
    };
    if !r.at_end() {
        return unknown(assert_id(local), bytes);
    }
    match disp {
        wire::DISP_HIT => event(KIND_ASSERT_HIT, [("point", Value::UInt(local as u64))]),
        wire::DISP_VIOLATION => event(
            KIND_ASSERT_VIOLATION,
            [
                ("point", Value::UInt(local as u64)),
                ("detail", Value::Bytes(detail.to_vec())),
            ],
        ),
        _ => unknown(assert_id(local), bytes),
    }
}

/// A state-register event: `[op u8][value u64]`.
fn decode_state(local: u32, bytes: &[u8]) -> GuestEvent {
    let mut r = Reader::new(bytes);
    let (Some(op), Some(value)) = (r.u8(), r.u64()) else {
        return unknown(state_id(local), bytes);
    };
    if !r.at_end() {
        return unknown(state_id(local), bytes);
    }
    let op_str = match op {
        wire::STATE_SET => "set",
        wire::STATE_MAX => "max",
        _ => return unknown(state_id(local), bytes),
    };
    event(
        KIND_STATE,
        [
            ("reg", Value::UInt(local as u64)),
            ("op", Value::Str(op_str.to_string())),
            ("value", Value::UInt(value)),
        ],
    )
}

/// A buggify result event: `[fired u8]`.
fn decode_buggify(local: u32, bytes: &[u8]) -> GuestEvent {
    let mut r = Reader::new(bytes);
    let Some(fired) = r.u8() else {
        return unknown(buggify_id(local), bytes);
    };
    if !r.at_end() {
        return unknown(buggify_id(local), bytes);
    }
    event(
        KIND_BUGGIFY,
        [
            ("point", Value::UInt(local as u64)),
            ("fired", Value::Bool(fired != 0)),
        ],
    )
}

/// The `KIND_UNKNOWN` fallback carrying the raw `event_id` and bytes.
fn unknown(event_id: u32, bytes: &[u8]) -> GuestEvent {
    event(
        KIND_UNKNOWN,
        [
            ("event_id", Value::UInt(event_id as u64)),
            ("data", Value::Bytes(bytes.to_vec())),
        ],
    )
}

/// Reconstruct a full `event_id` from a namespace + local (for the unknown
/// fallback, which needs the original id).
fn assert_id(local: u32) -> u32 {
    ((wire::NS_ASSERT as u32) << wire::NS_SHIFT) | (local & wire::LOCAL_MASK)
}
fn state_id(local: u32) -> u32 {
    ((wire::NS_STATE as u32) << wire::NS_SHIFT) | (local & wire::LOCAL_MASK)
}
fn buggify_id(local: u32) -> u32 {
    ((wire::NS_BUGGIFY as u32) << wire::NS_SHIFT) | (local & wire::LOCAL_MASK)
}

/// Read a `u64` out of a decoded event's `UInt`/non-negative-`Int` attribute
/// (the numeric attributes the catalog fold and the sensor key on). `None` if the
/// key is absent or the value is not a non-negative integer.
pub(crate) fn attr_u64(ev: &GuestEvent, key: &str) -> Option<u64> {
    match ev.attrs.get(key)? {
        Value::UInt(v) => Some(*v),
        Value::Int(v) if *v >= 0 => Some(*v as u64),
        _ => None,
    }
}

/// Read a `&str` out of a decoded event's `Str` attribute (e.g. the state
/// register's `op` — `"set"`/`"max"`). `None` if the key is absent or not a string.
pub(crate) fn attr_str<'a>(ev: &'a GuestEvent, key: &str) -> Option<&'a str> {
    match ev.attrs.get(key)? {
        Value::Str(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Build a [`GuestEvent`] from a kind and a fixed set of attributes.
fn event<const N: usize>(kind: &str, attrs: [(&str, Value); N]) -> GuestEvent {
    let attrs: BTreeMap<String, Value> =
        attrs.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    GuestEvent {
        kind: kind.to_string(),
        attrs,
    }
}
