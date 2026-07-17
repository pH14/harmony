// SPDX-License-Identifier: AGPL-3.0-or-later
//! The normalized, ordered **`SdkEvent`** and the [`Normalized`] decode bundle.
//!
//! An `SdkEvent` is one decoded, timestamped observation. It carries the four
//! roles the strategy keeps separate — *source provenance* ([`SdkEvent::source`]),
//! *observation identity* ([`SdkEvent::id`]), *site provenance*
//! ([`SdkEvent::site`], for assertions), and the *value* ([`SdkEvent::payload`]) —
//! plus its ordering coordinates and the recoverable raw record. It does **not**
//! carry a cell, a reduction, or a verdict; those are above this boundary.

use std::collections::BTreeMap;

use explorer::Moment;
use serde::{Deserialize, Serialize};

use crate::error::SdkError;
use crate::numeric::NumericToken;
use crate::schema::{
    Classification, ObservationId, Raw, SdkSchema, SdkSchemaRepr, SourceFormat, UpdateOp,
};
use crate::wire;

/// The result of decoding one ingress stream: the normalized schema plus the
/// ordered events. The schema's entries and the events' ordinals are canonical and
/// identical across platforms.
///
/// `Normalized` is the persisted artifact and the **only** publicly-deserializable
/// entry point: its `#[serde(try_from)]` re-validates the whole contract on load
/// (schema-entry invariants, declaration provenance, and event↔schema coherence),
/// so component types like [`SdkEvent`]/[`SdkSchema`] carry no bare `Deserialize`
/// that could bypass it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "NormalizedRepr")]
pub struct Normalized {
    /// The normalized schema derived from (or declared by) the stream.
    pub schema: SdkSchema,
    /// The ordered events, in persisted (source-ordinal) order.
    pub events: Vec<SdkEvent>,
}

/// The kind of Antithesis assertion an [`Payload::Assertion`] evidences. These are
/// the verbs of the adopted Antithesis surface (`docs/LAYERS.md` §R-L3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AssertType {
    /// `always` — the condition must hold at every evaluation.
    Always,
    /// `sometimes` — the condition must hold at some evaluation.
    Sometimes,
    /// `reachable` — the point must be reached.
    Reachable,
    /// `unreachable` — the point must never be reached.
    Unreachable,
}

/// An assertion **site** — provenance and coverage, kept separate from the
/// aggregated property identity. Multiple sites may contribute to one property
/// (`docs/DISSONANCE-STRATEGY.md`: "the assertion message identifies the property
/// and multiple sites may contribute to it; site identity remains provenance and
/// coverage").
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SiteId {
    /// The source's per-site assertion `id`, if present — site metadata, not the
    /// property identity (the message is the property; see [`SdkEvent::id`]).
    pub id: Option<String>,
    /// The source file the assertion is in.
    pub file: String,
    /// The enclosing function/class path.
    pub function: String,
    /// The 1-based line of the assertion. `u64` so an untrusted coordinate is
    /// preserved exactly rather than truncated into a colliding site.
    pub begin_line: u64,
    /// The 1-based column of the assertion (`u64` for the same reason).
    pub begin_column: u64,
}

/// The normalized value an event carries. Occurrence and state payloads are kept
/// distinct so a downstream reducer never mistakes a one-shot hit for persistent
/// state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Payload {
    /// An assertion evaluation — occurrence/property evidence. `condition` is the
    /// asserted predicate's value at this evaluation, when the source reported it.
    Assertion {
        /// The assertion verb, if known (always known for Antithesis JSON; `None`
        /// for a binary firing at an undeclared coordinate).
        assert_type: Option<AssertType>,
        /// The evaluated condition, if reported.
        condition: Option<bool>,
    },
    /// A state-register update. `op` is the base reduction the value participates
    /// in; `value` is the reported integer (the initial cooperative-vertical shape).
    State {
        /// The base update operation.
        op: UpdateOp,
        /// The reported value.
        value: u64,
    },
    /// A numeric-guidance report — a monotone extremum only (never arbitrary `set`
    /// state, because the SDK may filter reports to new watermarks). `op` is `Max`
    /// or `Min`; `token` is the original numeric token, report-only until it
    /// normalizes into a bounded exact representation.
    Guidance {
        /// The extremum direction (`Max` for `maximize`, `Min` otherwise).
        op: UpdateOp,
        /// The reported extremum as its original token, if the record carried a
        /// scalar metric; `None` when only non-scalar operands were present (the
        /// operands survive in [`SdkEvent::raw`]).
        token: Option<NumericToken>,
    },
    /// A buggify decision outcome (occurrence): whether the fault fired.
    Buggify {
        /// Whether the buggify point fired.
        fired: bool,
    },
    /// A lifecycle point (e.g. `setup_complete`).
    Lifecycle {
        /// The lifecycle point name.
        name: String,
    },
    /// An unrecognized or opaque record — nothing normalized; the raw bytes in
    /// [`SdkEvent::raw`] are the whole of it.
    Unknown,
}

/// One decoded, timestamped observation.
///
/// Not independently deserializable: an `SdkEvent` is only ever loaded as part of
/// a [`Normalized`] artifact, whose `try_from` re-checks each event against the
/// schema (source, ordinal order, payload↔identity classification). Carrying a bare
/// `Deserialize` here would let a persisted event bypass that coherence check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SdkEvent {
    /// The V-time anchor this event surfaced at. Per the open issue `hm-ynt`, an
    /// SDK event `Moment` is a V-time-anchor **lower bound**, not necessarily the
    /// exact emission `Moment`; this boundary carries it through faithfully and
    /// neither tightens nor loosens that contract.
    pub moment: Moment,
    /// The rollout-local **source ordinal**: the event's persisted vector position.
    /// Contractual within this source (per [`OrderingScope`](crate::OrderingScope));
    /// cross-source sequencing needs a shared machine-event ordinal this boundary
    /// does not have.
    pub ordinal: u64,
    /// Which ingress format produced this event.
    pub source: SourceFormat,
    /// The stable observation identity.
    pub id: ObservationId,
    /// The assertion site (provenance/coverage), separate from the property
    /// identity; `None` for non-assertion events.
    pub site: Option<SiteId>,
    /// The normalized value.
    pub payload: Payload,
    /// The original source record, preserved verbatim for audit/migration.
    pub raw: Raw,
}

/// The on-the-wire shape of an [`SdkEvent`], deserialized before a [`Normalized`]
/// re-checks it against the schema. Mirrors `SdkEvent` field-for-field; component
/// value types keep their own `Deserialize` (they have no independent load path),
/// but `SdkEvent` itself does not, so this repr is the only way to read one back —
/// always through [`Normalized`]'s validated `try_from`.
#[derive(Deserialize)]
struct SdkEventRepr {
    moment: Moment,
    ordinal: u64,
    source: SourceFormat,
    id: ObservationId,
    site: Option<SiteId>,
    payload: Payload,
    raw: Raw,
}

impl From<SdkEventRepr> for SdkEvent {
    fn from(r: SdkEventRepr) -> SdkEvent {
        SdkEvent {
            moment: r.moment,
            ordinal: r.ordinal,
            source: r.source,
            id: r.id,
            site: r.site,
            payload: r.payload,
            raw: r.raw,
        }
    }
}

/// The on-the-wire shape of a [`Normalized`], deserialized before its whole
/// contract is re-validated. Private: the only way to obtain a `Normalized` from
/// persisted input is [`Normalized`]'s `#[serde(try_from)]`, so no caller can hold
/// an un-validated one.
#[derive(Deserialize)]
struct NormalizedRepr {
    schema: SdkSchemaRepr,
    events: Vec<SdkEventRepr>,
}

/// The classification a payload variant evidences, or `None` for an opaque
/// [`Payload::Unknown`] (which constrains nothing — it is preserved raw).
fn payload_classification(payload: &Payload) -> Option<Classification> {
    match payload {
        Payload::Assertion { .. } | Payload::Buggify { .. } | Payload::Lifecycle { .. } => {
            Some(Classification::Occurrence)
        }
        Payload::State { .. } | Payload::Guidance { .. } => Some(Classification::State),
        Payload::Unknown => None,
    }
}

/// The base operation a reducible (state/guidance) payload carries — the op a
/// persisted event must not contradict at its declared coordinate. `None` for a
/// payload that carries no reducible operation.
fn payload_op(payload: &Payload) -> Option<UpdateOp> {
    match payload {
        Payload::State { op, .. } | Payload::Guidance { op, .. } => Some(*op),
        _ => None,
    }
}

impl TryFrom<NormalizedRepr> for Normalized {
    type Error = SdkError;

    /// Re-validate a persisted decode bundle as a whole. Loading is held to the same
    /// contract the live decoders enforce, so a persisted artifact can never assert
    /// evidence a decode would have refused:
    ///
    /// - the **schema** passes the [`SchemaEntry::validate`](crate::SchemaEntry) choke
    ///   point for every entry (sorted, unique, source-specific invariants) via
    ///   [`SdkSchema::try_from`];
    /// - **declaration provenance**: `original_declaration` re-parses to exactly this
    ///   schema's source and entries (a binary firing adds no entry, so the
    ///   declaration fully determines them) — a null/garbage blob, a blob for the
    ///   wrong source, or one present where the source mints none is corrupt;
    /// - **event↔schema coherence**: each event agrees with the schema `source`, the
    ///   ordinals are strictly increasing (rollout-local order), a classified payload
    ///   sits only at a coordinate whose classification matches, and a reducible op
    ///   matches the declared base op and every earlier firing for its identity —
    ///   so a persisted `set` firing at a `max`-declared coordinate fails load with
    ///   the same [`MixedOperations`](SdkError::MixedOperations) the decoder raises.
    fn try_from(repr: NormalizedRepr) -> Result<Normalized, SdkError> {
        let schema = SdkSchema::try_from(repr.schema)?;

        // F1a — declaration provenance.
        let mismatch = |detail: String| SdkError::DeclarationMismatch { detail };
        match &schema.original_declaration {
            Some(raw) => {
                if !matches!(
                    schema.source,
                    SourceFormat::BinaryV1 | SourceFormat::BinaryV2
                ) {
                    return Err(mismatch(format!(
                        "source {:?} carries no separate declaration, yet one is present",
                        schema.source
                    )));
                }
                if raw.source != schema.source {
                    return Err(mismatch(format!(
                        "declaration source {:?} disagrees with schema source {:?}",
                        raw.source, schema.source
                    )));
                }
                if raw.event_id != Some(wire::CATALOG_EVENT_ID) {
                    return Err(mismatch(
                        "declaration is not tagged as the catalog record".to_string(),
                    ));
                }
                let reparsed = crate::binary::schema_from_declaration(&raw.bytes)
                    .map_err(|e| mismatch(format!("declaration does not re-parse: {e}")))?;
                if reparsed.source != schema.source {
                    return Err(mismatch(format!(
                        "declaration re-parses as source {:?}, not {:?}",
                        reparsed.source, schema.source
                    )));
                }
                if reparsed.entries() != schema.entries() {
                    return Err(mismatch(
                        "declaration re-parses to different entries than the schema records"
                            .to_string(),
                    ));
                }
            }
            None => match schema.source {
                // Antithesis declares implicitly through its records — never a blob.
                SourceFormat::AntithesisJson => {}
                // Binary-v1 entries come only from a catalog; with none, there are none.
                SourceFormat::BinaryV1 => {
                    if !schema.entries().is_empty() {
                        return Err(mismatch(
                            "binary v1 schema declares entries with no original declaration"
                                .to_string(),
                        ));
                    }
                }
                // The v2 source exists only because a v2 catalog was parsed.
                SourceFormat::BinaryV2 => {
                    return Err(mismatch(
                        "binary v2 schema has no original declaration".to_string(),
                    ));
                }
            },
        }

        // F1d — event↔schema coherence, mirroring the live decoders.
        let events: Vec<SdkEvent> = repr.events.into_iter().map(SdkEvent::from).collect();
        let incoherent = |detail: String| SdkError::IncoherentEvent { detail };
        let mut prev_ordinal: Option<u64> = None;
        let mut observed_ops: BTreeMap<ObservationId, UpdateOp> = BTreeMap::new();
        for ev in &events {
            if ev.source != schema.source {
                return Err(incoherent(format!(
                    "event source {:?} disagrees with schema source {:?}",
                    ev.source, schema.source
                )));
            }
            if let Some(prev) = prev_ordinal
                && ev.ordinal <= prev
            {
                return Err(incoherent(format!(
                    "event ordinal {} does not exceed the previous ordinal {prev}",
                    ev.ordinal
                )));
            }
            prev_ordinal = Some(ev.ordinal);

            // An opaque `Unknown` payload constrains nothing; preserved raw.
            let Some(pc) = payload_classification(&ev.payload) else {
                continue;
            };

            // Binary coordinate: a classified payload can only sit at a namespace
            // whose firings decode to that classification — true even undeclared.
            if let ObservationId::Point { namespace, .. } = &ev.id
                && matches!(
                    schema.source,
                    SourceFormat::BinaryV1 | SourceFormat::BinaryV2
                )
            {
                match crate::binary::namespace_classification(*namespace) {
                    Some(expected) if expected == pc => {}
                    Some(expected) => {
                        return Err(incoherent(format!(
                            "payload classification {pc:?} disagrees with namespace {namespace}'s firing classification {expected:?}"
                        )));
                    }
                    None => {
                        return Err(incoherent(format!(
                            "classified payload at namespace {namespace}, which decodes only to Unknown"
                        )));
                    }
                }
            }

            // Declared coordinate: agree with the entry's classification/op/shape.
            if let Some(entry) = schema.entry(&ev.id) {
                if entry.classification != pc {
                    return Err(incoherent(format!(
                        "payload classification {pc:?} disagrees with declared {:?} for {:?}",
                        entry.classification, ev.id
                    )));
                }
                if let (Some(op), Some(declared)) = (payload_op(&ev.payload), entry.base_op)
                    && op != declared
                {
                    return Err(SdkError::MixedOperations {
                        id: ev.id.clone(),
                        first: declared,
                        second: op,
                    });
                }
                // Value **shape** needs no separate event-level check: it is pinned
                // transitively. A reducible payload's shape follows from its variant
                // (`State`→`u64`, `Guidance`→`Numeric`), the classification match above
                // ties the variant to `entry.classification`, and the schema choke
                // point already validated `entry.value_shape` against that
                // classification and source (v2 state ⇒ `u64`, v1 state ⇒ unresolved,
                // antithesis guidance ⇒ `Numeric`). With `source` agreement, no
                // persisted payload can reach here carrying a shape the entry does not
                // already declare.
            }

            // Cross-event op coherence: two reducible firings for one identity must
            // not disagree (the decoder's per-identity `observed_ops` rule).
            if let Some(op) = payload_op(&ev.payload) {
                if let Some(&first) = observed_ops.get(&ev.id)
                    && first != op
                {
                    return Err(SdkError::MixedOperations {
                        id: ev.id.clone(),
                        first,
                        second: op,
                    });
                }
                observed_ops.insert(ev.id.clone(), op);
            }
        }

        Ok(Normalized { schema, events })
    }
}
