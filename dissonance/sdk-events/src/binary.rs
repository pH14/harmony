// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **internal binary Event wire** decoder, normalized into [`Normalized`].
//!
//! `harmony-linux/sdk`'s byte-deterministic Event wire (`docs/LAYERS.md` §R-L3 item 5)
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

use std::collections::{BTreeMap, BTreeSet};

use crate::moment::Moment;

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
/// of the v2 branch of [`decode_binary`]: `decode_binary(encode_v2_declaration(p)?)`
/// recovers a schema whose entries equal `p` (modulo canonical id sorting).
///
/// Each point is validated first ([`validate_v2_point`]) — the same checks
/// [`decode_binary`] applies — so an un-fireable local id or a declaration the
/// binary emission path cannot honor fails here rather than minting evidence that
/// contradicts its firings.
///
/// The emitted bytes are **canonical**: points are serialized sorted by
/// `(namespace, local)`, never in the caller's incidental order. The caller may
/// have collected them from a `HashMap` or any unordered source, and the persisted
/// `original_declaration` bytes must not carry that host-order nondeterminism
/// (conventions rule 4). The canonical guest-side encoder is a future `harmony-linux/sdk`
/// deliverable; this host-side encoder exists so declarations round-trip and so
/// fixtures/tools can build them.
pub fn encode_v2_declaration(points: &[DeclaredPoint]) -> Result<Vec<u8>, SdkError> {
    let records = encode_v2_records(points)?;
    let mut out = Vec::with_capacity(5 + records.len());
    out.extend_from_slice(&wire::CATALOG_MAGIC.to_le_bytes());
    out.push(wire::SDK_WIRE_VERSION_V2);
    out.extend_from_slice(&records);
    Ok(out)
}

/// Validate and encode a v2 catalog **body** (`count + records`, no header) —
/// shared by [`encode_v2_declaration`] (the plain form) and
/// [`resolve_v1_declaration`] (the upgraded, provenance-carrying form).
fn encode_v2_records(points: &[DeclaredPoint]) -> Result<Vec<u8>, SdkError> {
    let mut seen: BTreeSet<(u8, u32)> = BTreeSet::new();
    for p in points {
        validate_v2_point(
            p.namespace,
            p.local,
            p.classification,
            p.value_shape,
            p.base_op,
            p.expectation,
        )?;
        // A firing cannot distinguish two entries at one coordinate, so a
        // declaration must not list one twice.
        if !seen.insert((p.namespace, p.local)) {
            return Err(SdkError::DuplicateCoordinate {
                namespace: p.namespace,
                local: p.local,
            });
        }
        // A name longer than the u16 length prefix cannot be encoded without
        // corrupting the identity label; refuse rather than truncate.
        if p.name.len() > u16::MAX as usize {
            return Err(SdkError::NameTooLong {
                namespace: p.namespace,
                local: p.local,
                len: p.name.len(),
                max: u16::MAX as usize,
            });
        }
    }
    // Serialize in canonical coordinate order so shuffled input yields identical
    // bytes. Coordinates are unique (checked above), so the order is total.
    let mut ordered: Vec<&DeclaredPoint> = points.iter().collect();
    ordered.sort_by_key(|p| (p.namespace, p.local));

    let mut out = Vec::new();
    out.extend_from_slice(&(points.len() as u32).to_le_bytes());
    for p in ordered {
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
        // Length-checked above, so the cast is exact.
        let name = p.name.as_bytes();
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(name);
    }
    Ok(out)
}

/// Upgrade a **wire-v1 catalog declaration** to wire v2 with resolved base
/// operations for the listed identities — the host-side **explicit workload
/// instrumentation declaration** (task 132, `hm-e6q`).
///
/// The strategy forbids silently blessing v1 state firings with reducers and
/// sanctions exactly one alternative to a guest wire-v2 rebuild: "an equally
/// explicit workload instrumentation declaration." This helper materializes
/// that declaration **through the decoder's own v1 parsing** — the guest's
/// declared points (classification, expectation, name) are read back exactly
/// as [`decode_binary`] normalizes them, so nothing can drift — and each
/// identity named in `ops` is resolved to its declared base operation (with
/// the [`ValueShape::U64`] shape the initial cooperative vertical uses).
/// A declared **occurrence** point not named in `ops` carries over untouched.
///
/// A declared v1 **state** point NOT named in `ops` fails loudly on
/// re-encode: wire v2 cannot express unresolved state (a v2 state point must
/// declare its base operation), so the instrumentation declaration must
/// resolve **every** state register the guest declares — an incomplete
/// resolution table is a contract error, never a silently dropped or
/// silently blessed register.
///
/// Errors with the decoder's own typed error on a malformed catalog, and
/// with the encoder's validation on re-encode if the resolution contradicts
/// or fails to cover the declaration.
///
/// The emitted blob is the **upgraded** catalog form
/// ([`wire::SDK_WIRE_VERSION_V2_UPGRADED`], hm-dd39): the resolved v2
/// records **plus the original v1 declaration bytes embedded verbatim** as
/// audit provenance. The decoder validates the embedded original against
/// the v2 records (so provenance can never drift from the schema it vouches
/// for) and the raw guest bytes stay recoverable from the persisted
/// artifact via
/// [`SdkSchema::original_v1_declaration`](crate::SdkSchema::original_v1_declaration)
/// — which is what keeps the `original_declaration` audit promise true for
/// an upgraded guest catalog. Decode *semantics* are exactly the plain-v2
/// records' (the embedded bytes change nothing at decode; in particular a
/// v1 assertion verb stays normalized away, as this upgrade has always
/// done).
pub fn resolve_v1_declaration(
    catalog: &[u8],
    ops: &[(ObservationId, UpdateOp)],
) -> Result<Vec<u8>, SdkError> {
    let normalized = decode_binary(&[(Moment(0), wire::CATALOG_EVENT_ID, catalog.to_vec())])?;
    let mut points = Vec::with_capacity(normalized.schema.len());
    for entry in normalized.schema.entries() {
        // A v1 catalog declares only point identities; the decode above can
        // mint nothing else from a bare declaration tuple.
        let ObservationId::Point { namespace, local } = entry.id else {
            continue;
        };
        let resolved = ops
            .iter()
            .find(|(id, _)| *id == entry.id)
            .map(|(_, op)| *op);
        points.push(DeclaredPoint {
            namespace,
            local,
            name: entry.name.clone().unwrap_or_default(),
            classification: entry.classification,
            value_shape: entry
                .value_shape
                .or(resolved.map(|_| crate::schema::ValueShape::U64)),
            base_op: entry.base_op.or(resolved),
            expectation: entry.expectation,
        });
    }
    let records = encode_v2_records(&points)?;
    // The length prefix is a u32; a larger original could not round-trip.
    // No real guest catalog approaches this (the v1 wire itself is
    // u16-name-prefixed records), so this is a pure bounds refusal.
    let orig_len = u32::try_from(catalog.len()).map_err(|_| SdkError::UpgradeProvenance {
        detail: format!(
            "original v1 declaration of {} bytes exceeds the u32 length prefix",
            catalog.len()
        ),
    })?;
    let mut out = Vec::with_capacity(9 + catalog.len() + records.len());
    out.extend_from_slice(&wire::CATALOG_MAGIC.to_le_bytes());
    out.push(wire::SDK_WIRE_VERSION_V2_UPGRADED);
    out.extend_from_slice(&orig_len.to_le_bytes());
    out.extend_from_slice(catalog);
    out.extend_from_slice(&records);
    Ok(out)
}

/// The embedded original v1 declaration inside an **upgraded** catalog blob
/// ([`wire::SDK_WIRE_VERSION_V2_UPGRADED`]), or `None` for any other bytes
/// (a plain v1/v2 catalog, or junk). Pure slicing — no validation beyond the
/// header shape; callers that need the provenance *checked* get that from
/// [`decode_binary`], which refuses an upgraded blob whose embedded original
/// does not correspond to its v2 records.
pub(crate) fn embedded_original_v1(bytes: &[u8]) -> Option<&[u8]> {
    let mut r = Reader::new(bytes);
    if r.u32()? != wire::CATALOG_MAGIC || r.u8()? != wire::SDK_WIRE_VERSION_V2_UPGRADED {
        return None;
    }
    let len = r.u32()? as usize;
    r.take(len)
}

/// The classification a namespace's firings actually decode to on the binary path
/// ([`decode_event`]), or `None` for a namespace that produces no reportable firing.
/// This is the ground truth a v2 declaration must agree with.
fn namespace_classification(namespace: u8) -> Option<Classification> {
    match namespace {
        // Assert / buggify / lifecycle firings decode to occurrence payloads…
        wire::NS_ASSERT | wire::NS_BUGGIFY | wire::NS_LIFECYCLE => Some(Classification::Occurrence),
        // …and state firings to state payloads.
        wire::NS_STATE => Some(Classification::State),
        // Any other namespace decodes only to `Unknown` — nothing to report.
        _ => None,
    }
}

/// Validate a v2 declared point against what the binary emission path can encode
/// and what a runtime `event_id` can address. Enforced identically on encode and
/// on decode so the two never disagree.
///
/// - the local id must fit the 24-bit runtime field ([`SdkError::LocalIdOutOfRange`]);
/// - the classification must match the one the namespace's firings actually decode
///   to ([`namespace_classification`]) — an `NS_ASSERT` point cannot be declared
///   state, an `NS_STATE` point cannot be declared occurrence;
/// - a **state** point must declare a base operation and a `u64` value shape — the
///   only state value the binary firing wire carries ([`SdkError::UnsupportedDeclaration`]);
/// - an **occurrence** point carries no base operation and no reducible value shape;
/// - an **expectation** is legal only on an assertion (`NS_ASSERT`) point — a state,
///   buggify, or lifecycle point carrying one is a declaration the schema choke
///   point ([`SchemaEntry::validate`](crate::SchemaEntry)) would reject on decode, so
///   encoding it would let the byte codec mint a blob its own decoder refuses.
fn validate_v2_point(
    namespace: u8,
    local: u32,
    classification: Classification,
    value_shape: Option<ValueShape>,
    base_op: Option<UpdateOp>,
    expectation: Option<Expectation>,
) -> Result<(), SdkError> {
    if local > wire::LOCAL_MASK {
        return Err(SdkError::LocalIdOutOfRange { namespace, local });
    }
    let unsupported = |reason| SdkError::UnsupportedDeclaration {
        namespace,
        local,
        reason,
    };
    // An expectation is absence-based assertion evidence; only an assertion point
    // can carry it. Checked here so encode and decode agree — the schema choke point
    // rejects a mis-placed expectation on load, and this keeps the codec from ever
    // producing such a blob.
    if expectation.is_some() && namespace != wire::NS_ASSERT {
        return Err(unsupported(
            "an expectation is legal only on an assertion point",
        ));
    }
    // The classification must be exactly what this namespace's firings decode to,
    // else schema and event evidence would disagree with no error surfaced.
    match namespace_classification(namespace) {
        None => {
            return Err(unsupported(
                "namespace has no reportable firing on the binary emission path",
            ));
        }
        Some(expected) if expected != classification => {
            return Err(unsupported(
                "declared classification disagrees with the namespace's firing payload",
            ));
        }
        Some(_) => {}
    }
    // Lifecycle firings decode only at the `setup_complete` local id; any other
    // lifecycle local would decode to `Unknown`, so it is not reportable.
    if namespace == wire::NS_LIFECYCLE && local != wire::LIFECYCLE_SETUP_COMPLETE {
        return Err(unsupported(
            "only the setup_complete lifecycle point (local 0) is reportable",
        ));
    }
    match classification {
        Classification::State => {
            if base_op.is_none() {
                return Err(unsupported(
                    "a v2 state point must declare a base operation",
                ));
            }
            if value_shape != Some(ValueShape::U64) {
                return Err(unsupported(
                    "the binary emission path encodes only u64 state values",
                ));
            }
        }
        Classification::Occurrence => {
            if base_op.is_some() {
                return Err(unsupported("an occurrence point has no base operation"));
            }
            if value_shape.is_some() {
                return Err(unsupported(
                    "an occurrence point has no reducible value shape",
                ));
            }
        }
    }
    Ok(())
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
    // 1. Declaration → schema + per-coordinate assert verbs + declared source. A
    //    rollout declares its schema exactly once; more than one catalog record is
    //    ambiguous about which governs the events between them (and a later record
    //    could claim a different wire version), so refuse rather than silently
    //    decode under the first.
    let declaration_count = raw
        .iter()
        .filter(|(_, id, _)| *id == wire::CATALOG_EVENT_ID)
        .count();
    if declaration_count > 1 {
        return Err(SdkError::MultipleDeclarations {
            count: declaration_count,
        });
    }
    // The declaration governs the whole batch, so it must precede every firing:
    // otherwise a later format claim (e.g. a v2 catalog) would retroactively
    // reinterpret prior bytes (a `min` firing that is unknown in a v1/declaration-
    // less stream). The first record before the declaration is the position of the
    // (sole) declaration; if it is not first, firings preceded it.
    let declaration_pos = raw
        .iter()
        .position(|(_, id, _)| *id == wire::CATALOG_EVENT_ID);
    if let Some(pos) = declaration_pos
        && pos > 0
    {
        return Err(SdkError::DeclarationAfterFirings {
            firings_before: pos,
        });
    }
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

    Ok(Normalized::seal(ctx.schema, events))
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

/// Parse a catalog declaration blob into a [`DeclContext`]. A blob whose magic is
/// absent or wrong is not a recognizable catalog at all, so it yields an empty v1
/// context leniently — the stream simply has no usable declaration. Once the `SDKC`
/// magic *is* present the blob is a declaration and is parsed strictly: an unknown
/// version is refused, and a record truncated before (or within) any field is a
/// [`MalformedLength`](SdkError) rather than a silently-dropped declaration.
fn parse_declaration(bytes: &[u8]) -> Result<DeclContext, SdkError> {
    let mut r = Reader::new(bytes);
    let magic = r.u32();
    if magic != Some(wire::CATALOG_MAGIC) {
        return Ok(DeclContext::empty());
    }
    // Magic present ⇒ a declaration is present; its version byte governs the layout.
    let available = r.remaining();
    let version = r.u8();
    match version {
        Some(wire::SDK_WIRE_VERSION) => parse_v1(&mut r),
        Some(wire::SDK_WIRE_VERSION_V2) => parse_v2(&mut r),
        Some(wire::SDK_WIRE_VERSION_V2_UPGRADED) => parse_v2_upgraded(&mut r),
        // A *present* but unrecognized version is a deliberate future-format claim:
        // its event payloads may be laid out differently, so refuse rather than
        // decode them under this decoder's layout (mirrors `decode_events`'s taint,
        // but as a typed error since this decoder returns `Result`).
        Some(other) => Err(SdkError::UnsupportedVersion { version: other }),
        // Valid catalog magic but truncated before the version byte: a declaration
        // is present (the `SDKC` magic marks it) yet unreadable. Emptying it silently
        // would erase every never-fired point the catalog declared, so treat the
        // truncation as the malformed record it is — consistent with the strict
        // field reads inside `parse_v1`/`parse_v2` and with the `event_id == 0`
        // record being an actual declaration, not just noise.
        None => Err(SdkError::MalformedLength {
            context: "catalog version",
            needed: 1,
            available,
        }),
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
    let mut seen: BTreeSet<(u8, u32)> = BTreeSet::new();
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
        // A local id that does not fit the 24-bit runtime field could never match a
        // firing — refuse it rather than mint a permanently never-fired identity.
        if local > wire::LOCAL_MASK {
            return Err(SdkError::LocalIdOutOfRange { namespace, local });
        }
        // One coordinate, one entry: a firing cannot distinguish two kinds at one
        // `(namespace, local)`, and collapsing them (last verb wins, first name and
        // expectation kept) would normalize contradictory evidence. The guest SDK
        // rejects such coordinates; so does the host decoder.
        if !seen.insert((namespace, local)) {
            return Err(SdkError::DuplicateCoordinate { namespace, local });
        }
        let id = ObservationId::Point { namespace, local };
        if let Some(at) = assert_type {
            ctx.assert_types.insert((namespace, local), at);
        }
        ctx.schema.merge_entry(SchemaEntry {
            id,
            classification,
            // v1 declares neither value shape nor base operation — both are left
            // unresolved on purpose (the v1 catalog carries only kind, id, name);
            // inventing `U64` would claim semantics the source never declared.
            value_shape: None,
            base_op: None,
            expectation,
            name: Some(name),
        })?;
    }
    // The blob must end exactly at the last declared record; trailing bytes mean a
    // miscounted/corrupted catalog whose declared identities the schema would omit.
    if !r.at_end() {
        return Err(SdkError::TrailingDeclarationBytes {
            context: "v1 catalog",
            extra: r.remaining(),
        });
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
    let mut seen: BTreeSet<(u8, u32)> = BTreeSet::new();
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
        // Accept the declaration only if the binary emission path can actually
        // report it (and the id can be addressed) — identical to the encode check,
        // so a decoded declaration is never richer than a valid encodable one.
        validate_v2_point(
            namespace,
            local,
            classification,
            value_shape,
            base_op,
            expectation,
        )?;
        // One coordinate, one entry: reject a duplicate rather than let
        // `merge_entry` silently collapse it and drop the shadowed name.
        if !seen.insert((namespace, local)) {
            return Err(SdkError::DuplicateCoordinate { namespace, local });
        }
        ctx.schema.merge_entry(SchemaEntry {
            id: ObservationId::Point { namespace, local },
            classification,
            value_shape,
            base_op,
            expectation,
            name: Some(name),
        })?;
    }
    // Reject trailing bytes past the declared record count (see `parse_v1`).
    if !r.at_end() {
        return Err(SdkError::TrailingDeclarationBytes {
            context: "v2 catalog",
            extra: r.remaining(),
        });
    }
    Ok(ctx)
}

/// Parse an **upgraded** v2 catalog (header already consumed;
/// [`wire::SDK_WIRE_VERSION_V2_UPGRADED`], hm-dd39): the embedded original
/// v1 declaration bytes, then the plain v2 record body. Semantics come from
/// the v2 records alone; the embedded original is audit provenance —
/// validated here so it can never silently drift from the schema it vouches
/// for:
///
/// - the embedded blob must itself parse as a **v1 catalog** (magic +
///   version 1 + records) — anything else is a
///   [`UpgradeProvenance`](SdkError::UpgradeProvenance) refusal (nesting an
///   upgraded blob inside an upgraded blob included);
/// - the v2 records must be exactly the [`resolve_v1_declaration`] image of
///   the embedded declaration: the same identities in the same canonical
///   order, each keeping its classification, expectation, and name (the
///   base op / value shape are what the resolution adds, so they are not
///   compared — [`parse_v2`]'s own validation already constrains them).
fn parse_v2_upgraded(r: &mut Reader<'_>) -> Result<DeclContext, SdkError> {
    let len = need_u32(r, "upgraded catalog original length")? as usize;
    let available = r.remaining();
    let original = r.take(len).ok_or(SdkError::MalformedLength {
        context: "upgraded catalog original bytes",
        needed: len,
        available,
    })?;
    let ctx = parse_v2(r)?;
    // Provenance: the embedded original must be a v1 catalog…
    let provenance = |detail: String| SdkError::UpgradeProvenance { detail };
    let mut er = Reader::new(original);
    if er.u32() != Some(wire::CATALOG_MAGIC) {
        return Err(provenance(
            "embedded original is not a catalog declaration (bad magic)".to_owned(),
        ));
    }
    if er.u8() != Some(wire::SDK_WIRE_VERSION) {
        return Err(provenance(
            "embedded original is not a v1 catalog declaration".to_owned(),
        ));
    }
    let v1 = parse_v1(&mut er)?;
    // …whose declared identities the v2 records resolve 1:1. Both entry sets
    // are sorted by identity, so a zipped walk is the exact comparison.
    if v1.schema.len() != ctx.schema.len() {
        return Err(provenance(format!(
            "embedded v1 declares {} identit(ies), the v2 records carry {}",
            v1.schema.len(),
            ctx.schema.len()
        )));
    }
    for (v1e, v2e) in v1.schema.entries().iter().zip(ctx.schema.entries()) {
        if v1e.id != v2e.id {
            return Err(provenance(format!(
                "identity mismatch: embedded v1 declares {:?}, v2 records carry {:?}",
                v1e.id, v2e.id
            )));
        }
        if v1e.classification != v2e.classification
            || v1e.expectation != v2e.expectation
            || v1e.name != v2e.name
        {
            return Err(provenance(format!(
                "entry {:?} disagrees between the embedded v1 declaration and the v2 records",
                v1e.id
            )));
        }
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
        // `always` is special on this wire: `harmony-linux/sdk` emits only violations, so a
        // *passing* always assertion produces no event at all. Marking it must-hit
        // would make a correct run look like an unsatisfied absence expectation, so
        // an always point carries no expectation — only its failure is observable.
        wire::KIND_ALWAYS => (
            Some(wire::NS_ASSERT),
            Classification::Occurrence,
            None,
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
/// comes from a v1 declaration (if the coordinate was declared); a v2 declaration
/// carries no verb but does carry an expectation, and both constrain which
/// disposition is admissible at the coordinate (a contradicting firing stays raw).
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
    // A declared verb (populated for v1 catalogs) constrains which disposition the
    // guest SDK can emit: hits come only from `sometimes`/`reachable`, violations
    // only from `always`/`unreachable`. A kind-inconsistent disposition (e.g. a hit
    // on an `always` point) is a forged/malformed record the guest would never emit
    // — keep it raw rather than mint credible normalized evidence.
    if let Some(at) = assert_type {
        let consistent = match at {
            AssertType::Sometimes | AssertType::Reachable => disp == wire::DISP_HIT,
            AssertType::Always | AssertType::Unreachable => disp == wire::DISP_VIOLATION,
        };
        if !consistent {
            return None;
        }
    }
    // A wire-v2 catalog declares no verb — only an `expectation` — so `assert_types`
    // is empty for a v2 point and the verb guard above never fires. Enforce the same
    // disposition discipline from the declared expectation instead, so the v2 guard
    // is not silently skipped: a `MustHit` point (sometimes/reachable) emits only
    // hits and a `MustNotHit` point (unreachable) only violations, so a disposition
    // contradicting the declared expectation is a forged/malformed firing and stays
    // raw. (A v1 catalog also records an expectation; this check agrees with — and is
    // subsumed by — the verb check above, so it changes nothing on the v1 path.)
    let id = ObservationId::Point {
        namespace: wire::NS_ASSERT,
        local,
    };
    if let Some(expectation) = ctx.schema.entry(&id).and_then(|e| e.expectation) {
        let consistent = match expectation {
            Expectation::MustHit => disp == wire::DISP_HIT,
            Expectation::MustNotHit => disp == wire::DISP_VIOLATION,
        };
        if !consistent {
            return None;
        }
    }
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
    let id = ObservationId::Point { namespace, local };
    // `min`/`accumulate` are wire-v2 firing extensions; v1 defines only set/max.
    // They require a v2 state DECLARATION *at this coordinate* (a resolved base op)
    // — a v2 stream alone is not enough. A min/accumulate firing at an undeclared
    // coordinate has no declaration blessing that op, so it stays raw rather than
    // fabricating a state update that bypasses the explicit-declaration
    // requirement. (A set/max firing is a v1 wire op; it is recorded as evidence
    // even without a declaration, but stays non-reducible without a schema entry.)
    let declared = ctx.schema.entry(&id).and_then(|e| e.base_op);
    let op = match op_byte {
        wire::STATE_SET => UpdateOp::Set,
        wire::STATE_MAX => UpdateOp::Max,
        wire::STATE_MIN if declared.is_some() => UpdateOp::Min,
        wire::STATE_ACCUMULATE if declared.is_some() => UpdateOp::Accumulate,
        _ => return Ok(None),
    };

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
    if let Some(declared) = declared
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

/// Decode a buggify firing `[fired u8]`. The guest encoder emits only `0`/`1`; any
/// other byte is malformed wire data and stays raw rather than coercing to a
/// boolean (mirrors the assertion-disposition and state-op checks).
fn decode_buggify(bytes: &[u8]) -> Option<Payload> {
    let mut r = Reader::new(bytes);
    let fired = match r.u8()? {
        0 => false,
        1 => true,
        _ => return None,
    };
    if !r.at_end() {
        return None;
    }
    Some(Payload::Buggify { fired })
}
