// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **Antithesis SDK JSON** decoder, normalized into [`Normalized`].
//!
//! Normal applications consume the Antithesis SDK surface unmodified and send its
//! JSON through `/dev/harmony` (`docs/LAYERS.md` §R-L3 items 1–4); this is the one
//! Antithesis-JSON decoder serving all device traffic. Each record is one JSON
//! object (one device write), host-`Moment`-stamped:
//!
//! - `antithesis_assert` → **occurrence/property evidence**. The aggregated
//!   property (its `id`/`message`) is the [observation identity](ObservationId);
//!   the `location` is preserved as the assertion [`SiteId`](crate::SiteId) —
//!   provenance and coverage, never a separate property verdict.
//! - `antithesis_guidance` (numeric) → a **monotone extremum** only. `maximize`
//!   selects [`UpdateOp::Max`]/[`UpdateOp::Min`]; the numeric metric is kept as its
//!   original token, report-only (the SDK may filter reports to new watermarks, so
//!   this stream can never be reinterpreted as arbitrary current `set` state).
//! - `antithesis_setup` → a lifecycle occurrence.
//!
//! The decoder is **total over frame structure**: an unparseable frame, an
//! unrecognized wrapper, or a recognized wrapper missing its identity becomes a
//! [`Payload::Unknown`] carrying the raw bytes — nothing is dropped and nothing
//! panics. The only error path is a per-identity coherence conflict (a property
//! used as both occurrence and state, or numeric guidance flipping its extremum).
//! Judgment stays out: this produces evidence, it does not decide pass/fail.

use std::collections::BTreeSet;
use std::fmt;

use explorer::Moment;
use serde::de::{Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

use crate::error::SdkError;
use crate::event::{AssertType, Normalized, Payload, SdkEvent, SiteId};
use crate::numeric::NumericToken;
use crate::schema::{
    Classification, Expectation, ObservationId, OrderingScope, Raw, SchemaEntry, SdkSchema,
    SourceFormat, UpdateOp, ValueShape,
};

/// The identity given to an `antithesis_setup` lifecycle record.
const SETUP_IDENTITY: &str = "antithesis.setup";

/// A whole-frame **recursive duplicate-key** detector. `serde_json::Value`
/// silently collapses a repeated key to the last member — at *any* depth — which
/// would let malformed input choose semantics (two `maximize` fields making a
/// `true`-then-`false` guidance normalize as `Min`). Deserializing into
/// `DupCheck` walks the entire tree and reports whether any object carries a
/// duplicate key, so such a frame can be preserved raw instead of normalized.
///
/// This is robust under serde_json's `arbitrary_precision` feature: a number is
/// represented internally as a single-key map, and a single key can never be a
/// duplicate, so numbers are scanned as `dup = false` with no special-casing.
struct DupCheck(bool);

impl<'de> serde::Deserialize<'de> for DupCheck {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(DupVisitor)
    }
}

struct DupVisitor;

impl<'de> Visitor<'de> for DupVisitor {
    type Value = DupCheck;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> Result<DupCheck, E> {
        Ok(DupCheck(false))
    }
    fn visit_i64<E>(self, _: i64) -> Result<DupCheck, E> {
        Ok(DupCheck(false))
    }
    fn visit_u64<E>(self, _: u64) -> Result<DupCheck, E> {
        Ok(DupCheck(false))
    }
    fn visit_i128<E>(self, _: i128) -> Result<DupCheck, E> {
        Ok(DupCheck(false))
    }
    fn visit_u128<E>(self, _: u128) -> Result<DupCheck, E> {
        Ok(DupCheck(false))
    }
    fn visit_f64<E>(self, _: f64) -> Result<DupCheck, E> {
        Ok(DupCheck(false))
    }
    fn visit_str<E>(self, _: &str) -> Result<DupCheck, E> {
        Ok(DupCheck(false))
    }
    fn visit_none<E>(self) -> Result<DupCheck, E> {
        Ok(DupCheck(false))
    }
    fn visit_unit<E>(self) -> Result<DupCheck, E> {
        Ok(DupCheck(false))
    }
    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<DupCheck, D::Error> {
        d.deserialize_any(DupVisitor)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<DupCheck, A::Error> {
        let mut dup = false;
        while let Some(DupCheck(child)) = seq.next_element()? {
            dup |= child;
        }
        Ok(DupCheck(dup))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<DupCheck, A::Error> {
        let mut seen = BTreeSet::new();
        let mut dup = false;
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key) {
                dup = true;
            }
            let DupCheck(child) = map.next_value()?;
            dup |= child;
        }
        Ok(DupCheck(dup))
    }
}

/// Whether a JSON frame contains a duplicate key at any depth. `Err` means the
/// frame is not parseable JSON at all (also treated as raw-unknown by the caller).
fn has_duplicate_key(bytes: &[u8]) -> Result<bool, ()> {
    serde_json::from_slice::<DupCheck>(bytes)
        .map(|DupCheck(dup)| dup)
        .map_err(|_| ())
}

/// Decode a batch of Antithesis JSON records (each `(Moment, object bytes)`) into
/// normalized schema + ordered events. `ordinal` is the record's persisted vector
/// position (the rollout-local source ordinal).
pub fn decode_antithesis(records: &[(Moment, Vec<u8>)]) -> Result<Normalized, SdkError> {
    let mut schema = SdkSchema::new(
        SourceFormat::AntithesisJson,
        OrderingScope::RolloutLocalSourceOrdinal,
    );
    let mut events = Vec::with_capacity(records.len());

    for (ordinal, (moment, bytes)) in records.iter().enumerate() {
        let raw = Raw {
            source: SourceFormat::AntithesisJson,
            event_id: None,
            bytes: bytes.clone(),
        };
        let event = decode_record(*moment, ordinal as u64, bytes, raw, &mut schema)?;
        events.push(event);
    }

    Ok(Normalized::seal(schema, events))
}

/// Decode one record. On any structural miss the record is preserved as an
/// [`Payload::Unknown`] event; a coherence conflict is the only error.
fn decode_record(
    moment: Moment,
    ordinal: u64,
    bytes: &[u8],
    raw: Raw,
    schema: &mut SdkSchema,
) -> Result<SdkEvent, SdkError> {
    let unknown = |raw: Raw| SdkEvent {
        moment,
        ordinal,
        source: SourceFormat::AntithesisJson,
        id: ObservationId::Property(String::new()),
        site: None,
        payload: Payload::Unknown,
        raw,
    };

    // A duplicate key anywhere in the frame means `serde_json::Value` would
    // silently drop a member (top level *or* nested), so a normalized reading
    // could be built from data that isn't unambiguously present. Preserve raw.
    // (An unparseable frame is likewise preserved raw.)
    match has_duplicate_key(bytes) {
        Ok(false) => {}
        _ => return Ok(unknown(raw)),
    }
    // No duplicate keys, so `Value` now faithfully reflects the frame.
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return Ok(unknown(raw));
    };
    let Some(object) = value.as_object() else {
        return Ok(unknown(raw));
    };

    // A recognized record carries *exactly one* Antithesis wrapper key. Zero
    // recognized wrappers is unknown data; more than one is an ambiguous record
    // whose intent is undefined — preserve it raw rather than silently pick a
    // branch and drop the rest.
    let assert = object.get("antithesis_assert");
    let guidance = object.get("antithesis_guidance");
    let setup = object.get("antithesis_setup");
    let recognized = assert.is_some() as u8 + guidance.is_some() as u8 + setup.is_some() as u8;
    if recognized != 1 {
        return Ok(unknown(raw));
    }
    // The wrapper value must be a JSON object; a scalar/null wrapper (e.g.
    // `{"antithesis_setup":7}`) is malformed and must not fabricate normalized
    // evidence from field defaults.
    if let Some(assert) = assert {
        if !assert.is_object() {
            return Ok(unknown(raw));
        }
        decode_assert(moment, ordinal, assert, raw, schema)
    } else if let Some(guidance) = guidance {
        if !guidance.is_object() {
            return Ok(unknown(raw));
        }
        decode_guidance(moment, ordinal, guidance, raw, schema)
    } else {
        // `recognized == 1` and the other two are `None`, so this is the setup case.
        let setup = setup.expect("setup wrapper");
        if !setup.is_object() {
            return Ok(unknown(raw));
        }
        decode_setup(moment, ordinal, setup, raw, schema)
    }
}

/// Decode an `antithesis_assert` wrapper into occurrence/property evidence.
fn decode_assert(
    moment: Moment,
    ordinal: u64,
    assert: &Value,
    raw: Raw,
    schema: &mut SdkSchema,
) -> Result<SdkEvent, SdkError> {
    let unknown = SdkEvent {
        moment,
        ordinal,
        source: SourceFormat::AntithesisJson,
        id: ObservationId::Property(String::new()),
        site: None,
        payload: Payload::Unknown,
        raw: raw.clone(),
    };
    // A property must be identifiable; without an id or message the record is
    // preserved raw rather than dropped or invented.
    let Some(property) = property_identity(assert) else {
        return Ok(unknown);
    };
    // A malformed location (a coordinate that is not a non-negative integer, etc.)
    // cannot be represented without fabricating a colliding site — keep the whole
    // record raw rather than corrupt its site identity.
    let Ok(site) = site_of(assert) else {
        return Ok(unknown);
    };

    let assert_type = assert_type(assert);
    let condition = assert.get("condition").and_then(Value::as_bool);
    let expectation = assert_expectation(assert, assert_type);

    let id = ObservationId::Property(property);
    schema.merge_entry(SchemaEntry {
        id: id.clone(),
        classification: Classification::Occurrence,
        value_shape: None,
        base_op: None,
        expectation,
        name: message_of(assert),
    })?;

    Ok(SdkEvent {
        moment,
        ordinal,
        source: SourceFormat::AntithesisJson,
        id,
        site,
        payload: Payload::Assertion {
            assert_type,
            condition,
        },
        raw,
    })
}

/// Decode a numeric `antithesis_guidance` wrapper into a monotone-extremum event.
/// Non-numeric guidance is preserved raw (only the numeric verb has the explicit
/// extremal contract this boundary normalizes).
fn decode_guidance(
    moment: Moment,
    ordinal: u64,
    guidance: &Value,
    raw: Raw,
    schema: &mut SdkSchema,
) -> Result<SdkEvent, SdkError> {
    let unknown = SdkEvent {
        moment,
        ordinal,
        source: SourceFormat::AntithesisJson,
        id: ObservationId::Property(String::new()),
        site: None,
        payload: Payload::Unknown,
        raw: raw.clone(),
    };

    let numeric = guidance.get("guidance_type").and_then(Value::as_str) == Some("numeric");
    let maximize = guidance.get("maximize").and_then(Value::as_bool);
    let property = property_identity(guidance);
    // Numeric guidance needs an identity and an explicit optimization direction;
    // anything else is report-only raw evidence.
    let (Some(property), Some(maximize), true) = (property, maximize, numeric) else {
        return Ok(unknown);
    };

    let op = if maximize {
        UpdateOp::Max
    } else {
        UpdateOp::Min
    };
    // A malformed location keeps the whole record raw (as for assertions).
    let Ok(site) = site_of(guidance) else {
        return Ok(unknown);
    };
    // The extremum metric is preserved as its original token when the record
    // carries a scalar; an operand-pair `guidance_data` stays report-only (operands
    // survive in `raw`) until a later derivation reduces it.
    let token = guidance
        .get("guidance_data")
        .and_then(number_token)
        .map(NumericToken::new);

    let id = ObservationId::Property(property);
    schema.merge_entry(SchemaEntry {
        id: id.clone(),
        classification: Classification::State,
        value_shape: Some(ValueShape::Numeric),
        base_op: Some(op),
        expectation: None,
        name: message_of(guidance),
    })?;

    Ok(SdkEvent {
        moment,
        ordinal,
        source: SourceFormat::AntithesisJson,
        id,
        site,
        payload: Payload::Guidance { op, token },
        raw,
    })
}

/// Decode an `antithesis_setup` wrapper into a lifecycle occurrence, registering
/// its fixed occurrence identity in the schema so a setup event can be validated
/// and materialized against `SdkSchema` like an assertion or guidance event.
fn decode_setup(
    moment: Moment,
    ordinal: u64,
    setup: &Value,
    raw: Raw,
    schema: &mut SdkSchema,
) -> Result<SdkEvent, SdkError> {
    // An absent `status` defaults to `complete`; a present but non-string status
    // (e.g. `7`) is malformed and must not fabricate a `setup_complete` occurrence
    // (mirrors `site_of`'s present-but-malformed handling). Preserve the frame raw.
    let status = match setup.get("status") {
        None => "complete",
        Some(v) => match v.as_str() {
            Some(s) => s,
            None => {
                return Ok(SdkEvent {
                    moment,
                    ordinal,
                    source: SourceFormat::AntithesisJson,
                    id: ObservationId::Property(String::new()),
                    site: None,
                    payload: Payload::Unknown,
                    raw,
                });
            }
        },
    };
    // A disjoint `Lifecycle` identity — never `Property`, so a user-forgeable
    // assertion/guidance message equal to the sentinel cannot alias it.
    let id = ObservationId::Lifecycle(SETUP_IDENTITY.to_string());
    schema.merge_entry(SchemaEntry {
        id: id.clone(),
        classification: Classification::Occurrence,
        value_shape: None,
        base_op: None,
        expectation: None,
        name: Some(SETUP_IDENTITY.to_string()),
    })?;
    Ok(SdkEvent {
        moment,
        ordinal,
        source: SourceFormat::AntithesisJson,
        id,
        site: None,
        payload: Payload::Lifecycle {
            name: format!("setup_{status}"),
        },
        raw,
    })
}

/// The aggregated property identity. `docs/DISSONANCE-STRATEGY.md` rules that "the
/// assertion message identifies the property and multiple sites may contribute to
/// it," so the **message** is the identity — records from different sites (and
/// with different per-site `id`s) that share a message aggregate into one property.
/// The per-site `id` is preserved as site metadata (see [`site_of`]), not identity.
/// `id` is a defensive fallback only when a record carries no message.
fn property_identity(v: &Value) -> Option<String> {
    v.get("message")
        .and_then(Value::as_str)
        .or_else(|| v.get("id").and_then(Value::as_str))
        .map(str::to_string)
}

/// The human message, if present.
fn message_of(v: &Value) -> Option<String> {
    v.get("message").and_then(Value::as_str).map(str::to_string)
}

/// Map the Antithesis `assert_type` (+ `display_type` for reachability) to a verb.
fn assert_type(v: &Value) -> Option<AssertType> {
    match v.get("assert_type").and_then(Value::as_str)? {
        "always" => Some(AssertType::Always),
        "sometimes" => Some(AssertType::Sometimes),
        "reachable" => Some(AssertType::Reachable),
        "unreachable" => Some(AssertType::Unreachable),
        // The Rust SDK spells both reachability verbs `"reachability"` and
        // distinguishes them by `display_type`.
        "reachability" => match v.get("display_type").and_then(Value::as_str) {
            Some("Unreachable") => Some(AssertType::Unreachable),
            _ => Some(AssertType::Reachable),
        },
        _ => None,
    }
}

/// The absence-based expectation an assertion declares. `unreachable` is a
/// must-not-hit; a `must_hit: true` property is a must-hit; otherwise none.
fn assert_expectation(v: &Value, assert_type: Option<AssertType>) -> Option<Expectation> {
    if assert_type == Some(AssertType::Unreachable) {
        return Some(Expectation::MustNotHit);
    }
    match v.get("must_hit").and_then(Value::as_bool) {
        Some(true) => Some(Expectation::MustHit),
        _ => None,
    }
}

/// Build a [`SiteId`] carrying the per-site provenance — the source's `id` field
/// and the `location`. Returns `None` only when neither is present. The `id` is
/// site metadata here, kept out of the property identity ([`property_identity`]).
/// `Err(())` means the `location` is present but **malformed** — a field with the
/// wrong shape (a `begin_line`/`begin_column` that is not a non-negative integer
/// fitting `u64`, a non-string `file`/`function`, or a non-object `location`). A
/// malformed location cannot be represented as a `SiteId` without fabricating a
/// coordinate that would collide distinct sites, so the caller preserves the whole
/// record raw. An *absent* field defaults (`0` / `""`) — that is a location that
/// simply does not specify it, not a malformed one.
fn site_of(v: &Value) -> Result<Option<SiteId>, ()> {
    // An absent `id` is `None`; a present but non-string `id` (e.g. `7`) is
    // malformed — reject rather than silently collapse distinct bad ids to `None`.
    let id = match v.get("id") {
        None => None,
        Some(id) => Some(id.as_str().ok_or(())?.to_string()),
    };
    let location = v.get("location");
    if id.is_none() && location.is_none() {
        return Ok(None);
    }
    let (file, function, begin_line, begin_column) = match location {
        None => (String::new(), String::new(), 0, 0),
        Some(loc) => {
            // A present but non-object `location` is malformed.
            if !loc.is_object() {
                return Err(());
            }
            (
                opt_str(loc, "file")?,
                opt_str(loc, "function")?,
                opt_u64(loc, "begin_line")?,
                opt_u64(loc, "begin_column")?,
            )
        }
    };
    Ok(Some(SiteId {
        id,
        file,
        function,
        begin_line,
        begin_column,
    }))
}

/// A `location` string field: absent → `""`; present and a string → its value;
/// present but not a string → `Err(())` (malformed).
fn opt_str(loc: &Value, key: &str) -> Result<String, ()> {
    match loc.get(key) {
        None => Ok(String::new()),
        Some(v) => v.as_str().map(str::to_string).ok_or(()),
    }
}

/// A `location` coordinate field: absent → `0`; present and a `u64` → its value
/// (preserved exactly, never truncated); present but not a non-negative integer
/// fitting `u64` (out of range, negative, or non-integer) → `Err(())` (malformed).
fn opt_u64(loc: &Value, key: &str) -> Result<u64, ()> {
    match loc.get(key) {
        None => Ok(0),
        Some(v) => v.as_u64().ok_or(()),
    }
}

/// The original token of a JSON number value, or `None` if it is not a scalar
/// number. Uses serde_json's `arbitrary_precision` representation, so the exact
/// digits survive and **no `f64` is ever constructed**.
fn number_token(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `DupVisitor::expecting` is only invoked by serde when formatting a type
    // error, which `deserialize_any` never produces for well-formed JSON — so it is
    // unreachable through the decode path. Drive it directly through a `Display`
    // wrapper so its message (and hence its body) is pinned.
    #[test]
    fn dup_visitor_expecting_describes_a_json_value() {
        struct Expecting;
        impl fmt::Display for Expecting {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                Visitor::expecting(&DupVisitor, f)
            }
        }
        assert_eq!(Expecting.to_string(), "any JSON value");
    }

    #[test]
    fn has_duplicate_key_detects_and_clears() {
        assert_eq!(has_duplicate_key(br#"{"a":1,"b":2}"#), Ok(false));
        assert_eq!(has_duplicate_key(br#"{"a":1,"a":2}"#), Ok(true));
        assert_eq!(has_duplicate_key(br#"{"a":{"b":1,"b":2}}"#), Ok(true));
        assert_eq!(has_duplicate_key(br#"[1,2,3]"#), Ok(false));
        assert_eq!(has_duplicate_key(b"not json"), Err(()));
    }
}
