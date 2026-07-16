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

use explorer::Moment;
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
    if let Some(assert) = assert {
        decode_assert(moment, ordinal, assert, raw, schema)
    } else if let Some(guidance) = guidance {
        decode_guidance(moment, ordinal, guidance, raw, schema)
    } else {
        // `recognized == 1` and the other two are `None`, so this is the setup case.
        Ok(decode_setup(
            moment,
            ordinal,
            setup.expect("setup wrapper"),
            raw,
        ))
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

/// The aggregated property identity: the `id` field, falling back to `message`.
fn property_identity(v: &Value) -> Option<String> {
    v.get("id")
        .and_then(Value::as_str)
        .or_else(|| v.get("message").and_then(Value::as_str))
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

/// Build a [`SiteId`] from a `location` object, if present.
fn site_of(v: &Value) -> Option<SiteId> {
    let loc = v.get("location")?;
    Some(SiteId {
        file: loc
            .get("file")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        function: loc
            .get("function")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        begin_line: loc
            .get("begin_line")
            .and_then(Value::as_u64)
            .unwrap_or_default() as u32,
        begin_column: loc
            .get("begin_column")
            .and_then(Value::as_u64)
            .unwrap_or_default() as u32,
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
