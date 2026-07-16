// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **internal binary Event wire** decoder, normalized into [`Normalized`].
//!
//! `guest/sdk`'s byte-deterministic Event wire (`docs/LAYERS.md` §R-L3 item 5)
//! stays the internal surface for bare-metal payloads and guest-resident agents.
//! This module decodes both catalog versions:
//!
//! - **v1** — the legacy catalog declares a point's identity and role but *not*
//!   its value shape or a fixed base operation. A declared state point therefore
//!   normalizes to unresolved state (`base_op = None`): reportable coverage, but
//!   not eligible for temporal reduction. Each fired event's carried operation is
//!   preserved on the event; two different operations for one identity are
//!   [malformed evidence](SdkError::MixedOperations). The decoder never *promotes*
//!   a v1 firing into a declared reducer — the first Differential vertical cannot
//!   silently bless inference from v1 events.
//! - **v2** — the cooperative production declaration carries occurrence/state
//!   classification, value shape, and base update operation, so a v2 state point
//!   is reducible *before it ever fires*.
//!
//! The declaration is parsed **strictly** (a truncated record is a typed
//! [`MalformedLength`](SdkError::MalformedLength)); the event stream is decoded
//! **totally** (a garbled or unrecognized event becomes a [`Payload::Unknown`]
//! carrying its raw bytes, never a panic and never a dropped byte).

use std::collections::BTreeMap;

use explorer::Moment;

use crate::error::SdkError;
use crate::event::{AssertType, Normalized, Payload, SdkEvent};
use crate::read::Reader;
use crate::schema::{
    Classification, Expectation, ObservationId, OrderingScope, Raw, SchemaEntry, SdkSchema,
    SourceFormat, UpdateOp, ValueShape,
};
use crate::wire;

/// One point in a **wire-v2** declaration, for [`encode_v2_declaration`]. Mirrors
/// a [`SchemaEntry`] plus the runtime namespace its firings arrive under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredPoint {
    /// The event-id namespace this point's firings arrive under (e.g.
    /// [`wire::NS_STATE`](crate) via the re-exported constants).
    pub namespace: u8,
    /// The 24-bit local id within the namespace.
    pub local: u32,
    /// A human-readable name.
    pub name: String,
    /// Occurrence vs state.
    pub classification: Classification,
    /// The value shape, if any.
    pub value_shape: Option<ValueShape>,
    /// The base update operation for a state point (must be `None` for an
    /// occurrence).
    pub base_op: Option<UpdateOp>,
    /// The absence-based expectation, if any.
    pub expectation: Option<Expectation>,
}

/// Encode a wire-v2 catalog declaration from a set of declared points. The inverse
/// of the v2 branch of [`decode_binary`]: `decode_binary(encode_v2_declaration(p))`
/// recovers a schema whose entries equal `p` (modulo canonical id sorting). The
/// canonical guest-side encoder is a future `guest/sdk` deliverable; this host-side
/// encoder exists so declarations round-trip and so fixtures/tools can build them.
pub fn encode_v2_declaration(points: &[DeclaredPoint]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&wire::CATALOG_MAGIC.to_le_bytes());
    out.push(wire::SDK_WIRE_VERSION_V2);
    out.extend_from_slice(&(points.len() as u32).to_le_bytes());
    for p in points {
        out.push(p.namespace);
        out.extend_from_slice(&p.local.to_le_bytes());
        out.push(match p.classification {
            Classification::Occurrence => wire::V2_CLASS_OCCURRENCE,
            Classification::State => wire::V2_CLASS_STATE,
        });
        out.push(p.value_shape.map_or(wire::V2_NONE, ValueShape::to_byte));
        out.push(p.base_op.map_or(wire::V2_NONE, UpdateOp::to_byte));
        out.push(match p.expectation {
            None => wire::V2_NONE,
            Some(Expectation::MustHit) => wire::V2_EXPECT_MUST_HIT,
            Some(Expectation::MustNotHit) => wire::V2_EXPECT_MUST_NOT_HIT,
        });
        // Names are u16-length-prefixed; a name longer than that is truncated to
        // the prefix's reach by construction here (callers keep names short).
        let name = p.name.as_bytes();
        let len = name.len().min(u16::MAX as usize);
        out.extend_from_slice(&(len as u16).to_le_bytes());
        out.extend_from_slice(&name[..len]);
    }
    out
}

/// Decode a captured binary Event stream into normalized schema + ordered events.
///
/// `raw` is the host `EventSink`'s `(Moment, event_id, bytes)` capture. The
/// catalog declaration (`event_id == 0`) becomes the schema and the recoverable
/// original declaration; every other entry becomes an [`SdkEvent`] whose `ordinal`
/// is its persisted vector position (the rollout-local source ordinal). Errors are
/// declaration-structure or per-identity coherence failures only; the event stream
/// itself never errors.
pub fn decode_binary(raw: &[(Moment, u32, Vec<u8>)]) -> Result<Normalized, SdkError> {
    // 1. Declaration → schema + per-coordinate assert verbs + declared source.
    let declaration = raw.iter().find(|(_, id, _)| *id == wire::CATALOG_EVENT_ID);
    let mut ctx = match declaration {
        Some((_, _, bytes)) => parse_declaration(bytes)?,
        None => DeclContext::empty(),
    };
    if let Some((_, _, bytes)) = declaration {
        ctx.schema.set_original_declaration(Raw {
            source: ctx.schema.source,
            event_id: Some(wire::CATALOG_EVENT_ID),
            bytes: bytes.clone(),
        });
    }

    // 2. Events. The declaration event itself is schema, not an event; its stream
    //    position is skipped (ordinals stay faithful to persisted position).
    let mut events = Vec::new();
    for (ordinal, (moment, id, bytes)) in raw.iter().enumerate() {
        if *id == wire::CATALOG_EVENT_ID {
            continue;
        }
        let event = decode_event(*moment, ordinal as u64, *id, bytes, &mut ctx)?;
        events.push(event);
    }

    Ok(Normalized {
        schema: ctx.schema,
        events,
    })
}

/// Working state threaded through a binary decode: the schema under construction,
/// the declared assert verb per assertion coordinate, and the base operation
/// observed so far per state identity (for the mixed-operations check).
struct DeclContext {
    schema: SdkSchema,
    assert_types: BTreeMap<(u8, u32), AssertType>,
    observed_ops: BTreeMap<ObservationId, UpdateOp>,
}

impl DeclContext {
    fn empty() -> DeclContext {
        DeclContext {
            schema: SdkSchema::new(
                SourceFormat::BinaryV1,
                OrderingScope::RolloutLocalSourceOrdinal,
            ),
            assert_types: BTreeMap::new(),
            observed_ops: BTreeMap::new(),
        }
    }
}

/// Parse a catalog declaration blob into a [`DeclContext`]. A missing/garbled
/// header (bad magic or an unknown/absent version) yields an empty v1 context
/// leniently — the stream simply has no usable declaration. A *present, known*
/// version with a truncated record is a strict [`MalformedLength`](SdkError).
fn parse_declaration(bytes: &[u8]) -> Result<DeclContext, SdkError> {
    let mut r = Reader::new(bytes);
    let magic = r.u32();
    let version = r.u8();
    if magic != Some(wire::CATALOG_MAGIC) {
        return Ok(DeclContext::empty());
    }
    match version {
        Some(wire::SDK_WIRE_VERSION) => parse_v1(&mut r),
        Some(wire::SDK_WIRE_VERSION_V2) => parse_v2(&mut r),
        // An unknown or absent version says nothing trustworthy about the layout;
        // decode the events under their namespaces and leave the schema empty.
        _ => Ok(DeclContext::empty()),
    }
}

/// Read a fixed-width field or fail with a truthful [`MalformedLength`].
fn need_u8(r: &mut Reader<'_>, context: &'static str) -> Result<u8, SdkError> {
    let available = r.remaining();
    r.u8().ok_or(SdkError::MalformedLength {
        context,
        needed: 1,
        available,
    })
}
fn need_u16(r: &mut Reader<'_>, context: &'static str) -> Result<u16, SdkError> {
    let available = r.remaining();
    r.u16().ok_or(SdkError::MalformedLength {
        context,
        needed: 2,
        available,
    })
}
fn need_u32(r: &mut Reader<'_>, context: &'static str) -> Result<u32, SdkError> {
    let available = r.remaining();
    r.u32().ok_or(SdkError::MalformedLength {
        context,
        needed: 4,
        available,
    })
}
/// Read a u16-length-prefixed name, failing with [`MalformedLength`] on overrun.
fn need_name(r: &mut Reader<'_>, context: &'static str) -> Result<String, SdkError> {
    let len = need_u16(r, context)? as usize;
    let available = r.remaining();
    let bytes = r.take(len).ok_or(SdkError::MalformedLength {
        context,
        needed: len,
        available,
    })?;
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

/// Parse a v1 catalog (header already consumed). A v1 state point is declared with
/// an **unresolved** base operation.
fn parse_v1(r: &mut Reader<'_>) -> Result<DeclContext, SdkError> {
    let count = need_u32(r, "v1 catalog count")?;
    let mut ctx = DeclContext::empty();
    for _ in 0..count {
        let kind = need_u8(r, "v1 point kind")?;
        let local = need_u32(r, "v1 point local id")?;
        let name = need_name(r, "v1 point name")?;
        let (namespace, classification, expectation, assert_type) = classify_v1_kind(kind);
        let Some(namespace) = namespace else {
            // An unknown kind byte has no runtime namespace; it cannot be matched
            // by a firing and is not added to the schema (nothing to reduce or
            // report against a coordinate). It is simply skipped.
            continue;
        };
        let id = ObservationId::Point { namespace, local };
        if let Some(at) = assert_type {
            ctx.assert_types.insert((namespace, local), at);
        }
        ctx.schema.merge_entry(SchemaEntry {
            id,
            classification,
            value_shape: state_shape(classification),
            base_op: None, // v1 never declares a base op — unresolved on purpose
            expectation,
            name: Some(name),
        })?;
    }
    Ok(ctx)
}

/// Parse a v2 catalog (header already consumed).
fn parse_v2(r: &mut Reader<'_>) -> Result<DeclContext, SdkError> {
    let count = need_u32(r, "v2 catalog count")?;
    let mut ctx = DeclContext {
        schema: SdkSchema::new(
            SourceFormat::BinaryV2,
            OrderingScope::RolloutLocalSourceOrdinal,
        ),
        assert_types: BTreeMap::new(),
        observed_ops: BTreeMap::new(),
    };
    for _ in 0..count {
        let namespace = need_u8(r, "v2 point namespace")?;
        let local = need_u32(r, "v2 point local id")?;
        let classification = match need_u8(r, "v2 classification")? {
            wire::V2_CLASS_OCCURRENCE => Classification::Occurrence,
            wire::V2_CLASS_STATE => Classification::State,
            other => {
                return Err(SdkError::UnknownDeclarationByte {
                    field: "classification",
                    value: other,
                });
            }
        };
        let value_shape = decode_optional_byte(
            need_u8(r, "v2 value shape")?,
            ValueShape::from_byte,
            "value_shape",
        )?;
        let base_op =
            decode_optional_byte(need_u8(r, "v2 base op")?, UpdateOp::from_byte, "base_op")?;
        let expectation = match need_u8(r, "v2 expectation")? {
            wire::V2_NONE => None,
            wire::V2_EXPECT_MUST_HIT => Some(Expectation::MustHit),
            wire::V2_EXPECT_MUST_NOT_HIT => Some(Expectation::MustNotHit),
            other => {
                return Err(SdkError::UnknownDeclarationByte {
                    field: "expectation",
                    value: other,
                });
            }
        };
        let name = need_name(r, "v2 point name")?;
        ctx.schema.merge_entry(SchemaEntry {
            id: ObservationId::Point { namespace, local },
            classification,
            value_shape,
            base_op,
            expectation,
            name: Some(name),
        })?;
    }
    Ok(ctx)
}

/// Decode an optional enumerated byte (`V2_NONE` → `None`), erroring on an
/// unrecognized non-`None` value.
fn decode_optional_byte<T>(
    byte: u8,
    decode: impl FnOnce(u8) -> Option<T>,
    field: &'static str,
) -> Result<Option<T>, SdkError> {
    if byte == wire::V2_NONE {
        return Ok(None);
    }
    decode(byte)
        .map(Some)
        .ok_or(SdkError::UnknownDeclarationByte { field, value: byte })
}

/// Map a v1 catalog kind byte to `(namespace, classification, expectation,
/// assert_type)`. An unknown kind has no namespace.
fn classify_v1_kind(
    kind: u8,
) -> (
    Option<u8>,
    Classification,
    Option<Expectation>,
    Option<AssertType>,
) {
    match kind {
        wire::KIND_ALWAYS => (
            Some(wire::NS_ASSERT),
            Classification::Occurrence,
            Some(Expectation::MustHit),
            Some(AssertType::Always),
        ),
        wire::KIND_SOMETIMES => (
            Some(wire::NS_ASSERT),
            Classification::Occurrence,
            Some(Expectation::MustHit),
            Some(AssertType::Sometimes),
        ),
        wire::KIND_REACHABLE => (
            Some(wire::NS_ASSERT),
            Classification::Occurrence,
            Some(Expectation::MustHit),
            Some(AssertType::Reachable),
        ),
        wire::KIND_UNREACHABLE => (
            Some(wire::NS_ASSERT),
            Classification::Occurrence,
            Some(Expectation::MustNotHit),
            Some(AssertType::Unreachable),
        ),
        wire::KIND_STATE => (Some(wire::NS_STATE), Classification::State, None, None),
        wire::KIND_BUGGIFY => (
            Some(wire::NS_BUGGIFY),
            Classification::Occurrence,
            None,
            None,
        ),
        _ => (None, Classification::Occurrence, None, None),
    }
}

/// The value shape a classification implies for a binary point: state points carry
/// a `u64`; occurrences carry no reducible value.
fn state_shape(classification: Classification) -> Option<ValueShape> {
    match classification {
        Classification::State => Some(ValueShape::U64),
        Classification::Occurrence => None,
    }
}

/// Decode one non-declaration event totally: a recognized payload, or a
/// [`Payload::Unknown`] carrying the raw bytes. The only error path is a
/// per-identity base-operation conflict on a state firing.
fn decode_event(
    moment: Moment,
    ordinal: u64,
    event_id: u32,
    bytes: &[u8],
    ctx: &mut DeclContext,
) -> Result<SdkEvent, SdkError> {
    let source = ctx.schema.source;
    let raw = Raw {
        source,
        event_id: Some(event_id),
        bytes: bytes.to_vec(),
    };
    let (namespace, local) = wire::split(event_id);
    let unknown = |raw: Raw| SdkEvent {
        moment,
        ordinal,
        source,
        id: ObservationId::Point { namespace, local },
        site: None,
        payload: Payload::Unknown,
        raw,
    };

    let payload = match namespace {
        wire::NS_ASSERT => decode_assert(local, bytes, ctx),
        wire::NS_STATE => decode_state(namespace, local, bytes, ctx)?,
        wire::NS_BUGGIFY => decode_buggify(bytes),
        wire::NS_LIFECYCLE if local == wire::LIFECYCLE_SETUP_COMPLETE && bytes.is_empty() => {
            Some(Payload::Lifecycle {
                name: "setup_complete".to_string(),
            })
        }
        _ => None,
    };

    match payload {
        Some(payload) => Ok(SdkEvent {
            moment,
            ordinal,
            source,
            id: ObservationId::Point { namespace, local },
            site: None,
            payload,
            raw,
        }),
        None => Ok(unknown(raw)),
    }
}

/// Decode an assertion firing `[disposition u8][detail_len u16][detail]`. The verb
/// comes from the declaration (if the coordinate was declared).
fn decode_assert(local: u32, bytes: &[u8], ctx: &DeclContext) -> Option<Payload> {
    let mut r = Reader::new(bytes);
    let disp = r.u8()?;
    r.bytes_lp16()?; // detail is preserved via `raw`; its length must parse cleanly
    if !r.at_end() {
        return None;
    }
    let condition = match disp {
        wire::DISP_HIT => Some(true),
        wire::DISP_VIOLATION => Some(false),
        _ => return None,
    };
    let assert_type = ctx.assert_types.get(&(wire::NS_ASSERT, local)).copied();
    Some(Payload::Assertion {
        assert_type,
        condition,
    })
}

/// Decode a state firing `[op u8][value u64]`. Records the observed base operation
/// per identity and rejects a second, conflicting operation for the same identity
/// as [`MixedOperations`](SdkError). For a v2-declared point, the firing must also
/// match the declared operation.
fn decode_state(
    namespace: u8,
    local: u32,
    bytes: &[u8],
    ctx: &mut DeclContext,
) -> Result<Option<Payload>, SdkError> {
    let mut r = Reader::new(bytes);
    let (Some(op_byte), Some(value)) = (r.u8(), r.u64()) else {
        return Ok(None);
    };
    if !r.at_end() {
        return Ok(None);
    }
    let op = match op_byte {
        wire::STATE_SET => UpdateOp::Set,
        wire::STATE_MAX => UpdateOp::Max,
        _ => return Ok(None),
    };
    let id = ObservationId::Point { namespace, local };

    // Coherence: a firing must not contradict an earlier firing for this identity…
    if let Some(&first) = ctx.observed_ops.get(&id)
        && first != op
    {
        return Err(SdkError::MixedOperations {
            id,
            first,
            second: op,
        });
    }
    // …nor a resolved v2 declaration for it.
    if let Some(declared) = ctx.schema.entry(&id).and_then(|e| e.base_op)
        && declared != op
    {
        return Err(SdkError::MixedOperations {
            id,
            first: declared,
            second: op,
        });
    }
    ctx.observed_ops.insert(id, op);

    Ok(Some(Payload::State { op, value }))
}

/// Decode a buggify firing `[fired u8]`.
fn decode_buggify(bytes: &[u8]) -> Option<Payload> {
    let mut r = Reader::new(bytes);
    let fired = r.u8()?;
    if !r.at_end() {
        return None;
    }
    Some(Payload::Buggify { fired: fired != 0 })
}
