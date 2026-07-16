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
use serde::de::{Deserializer, MapAccess, Visitor};
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

/// The top-level members of one JSON frame, **preserving duplicate keys**.
/// `serde_json::Value` collapses a repeated key to the last member (silently
/// dropping the earlier one), which would let a `{"antithesis_assert":…,
/// "antithesis_assert":…}` frame decode as one confident assertion. Collecting
/// every member instead lets the decoder spot the ambiguity and preserve the
/// frame raw.
struct FrameMembers(Vec<(String, Value)>);

impl<'de> serde::Deserialize<'de> for FrameMembers {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MembersVisitor;
        impl<'de> Visitor<'de> for MembersVisitor {
            type Value = Vec<(String, Value)>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                // `next_entry` yields each member in order, including duplicate keys
                // (dedup happens only when building a `Map`, which we avoid here).
                let mut out = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, Value>()? {
                    out.push((k, v));
                }
                Ok(out)
            }
        }
        deserializer
            .deserialize_map(MembersVisitor)
            .map(FrameMembers)
    }
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

    Ok(Normalized { schema, events })
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

    // Parse preserving duplicate keys; a non-object frame (array/scalar/invalid)
    // fails here and is preserved raw.
    let Ok(FrameMembers(members)) = serde_json::from_slice::<FrameMembers>(bytes) else {
        return Ok(unknown(raw));
    };
    // A repeated top-level key means a member was silently overwritten on the
    // normalized path — the frame is ambiguous, so preserve it raw.
    let mut seen = BTreeSet::new();
    if members.iter().any(|(k, _)| !seen.insert(k.as_str())) {
        return Ok(unknown(raw));
    }
    let member = |name: &str| members.iter().find(|(k, _)| k == name).map(|(_, v)| v);

    // A recognized record carries *exactly one* Antithesis wrapper key. Zero
    // recognized wrappers is unknown data; more than one is an ambiguous record
    // whose intent is undefined — preserve it raw rather than silently pick a
    // branch and drop the rest.
    let assert = member("antithesis_assert");
    let guidance = member("antithesis_guidance");
    let setup = member("antithesis_setup");
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
        Ok(decode_setup(moment, ordinal, setup, raw))
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

    let assert_type = assert_type(assert);
    let condition = assert.get("condition").and_then(Value::as_bool);
    let site = site_of(assert);
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
    // The extremum metric is preserved as its original token when the record
    // carries a scalar; an operand-pair `guidance_data` stays report-only (operands
    // survive in `raw`) until a later derivation reduces it.
    let token = guidance
        .get("guidance_data")
        .and_then(number_token)
        .map(NumericToken::new);
    let site = site_of(guidance);

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

/// Decode an `antithesis_setup` wrapper into a lifecycle occurrence.
fn decode_setup(moment: Moment, ordinal: u64, setup: &Value, raw: Raw) -> SdkEvent {
    let status = setup
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("complete");
    SdkEvent {
        moment,
        ordinal,
        source: SourceFormat::AntithesisJson,
        id: ObservationId::Property(SETUP_IDENTITY.to_string()),
        site: None,
        payload: Payload::Lifecycle {
            name: format!("setup_{status}"),
        },
        raw,
    }
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
fn site_of(v: &Value) -> Option<SiteId> {
    let id = v.get("id").and_then(Value::as_str).map(str::to_string);
    let location = v.get("location");
    if id.is_none() && location.is_none() {
        return None;
    }
    let field = |key: &str| {
        location
            .and_then(|loc| loc.get(key))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let num = |key: &str| {
        location
            .and_then(|loc| loc.get(key))
            .and_then(Value::as_u64)
            .unwrap_or_default() as u32
    };
    Some(SiteId {
        id,
        file: field("file"),
        function: field("function"),
        begin_line: num("begin_line"),
        begin_column: num("begin_column"),
    })
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
