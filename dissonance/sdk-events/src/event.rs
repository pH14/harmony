// SPDX-License-Identifier: AGPL-3.0-or-later
//! The normalized, ordered **`SdkEvent`** and the [`Normalized`] decode bundle.
//!
//! An `SdkEvent` is one decoded, timestamped observation. It carries the four
//! roles the strategy keeps separate — *source provenance* ([`SdkEvent::source`]),
//! *observation identity* ([`SdkEvent::id`]), *site provenance*
//! ([`SdkEvent::site`], for assertions), and the *value* ([`SdkEvent::payload`]) —
//! plus its ordering coordinates and the recoverable raw record. It does **not**
//! carry a cell, a reduction, or a verdict; those are above this boundary.

use explorer::Moment;
use serde::{Deserialize, Serialize};

use crate::error::SdkError;
use crate::numeric::NumericToken;
use crate::schema::{ObservationId, Raw, SdkSchema, SdkSchemaRepr, SourceFormat, UpdateOp};
use crate::wire;

/// An independent commitment over the ingress event stream: the event count and a
/// blake3 digest over each event's record (V-time `Moment` + length-prefixed raw
/// bytes) in ordinal order.
///
/// It closes the one completeness gap re-decode-and-compare cannot reach on its own:
/// a **subset** artifact (events truncated, or all deleted) re-decodes *to itself*,
/// because the raw stream it reconstructs from was truncated with it. The commitment
/// is computed once at decode over the whole stream and persisted, so a later load
/// recomputes it from the re-decoded stream and finds count/digest disagreeing with
/// the stored value — truncation, extension, and raw tampering all fail load.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamCommitment {
    /// The number of events in the committed stream.
    pub events: u64,
    /// A blake3 digest over the events' ingress records, in ordinal order.
    pub digest: [u8; 32],
}

impl StreamCommitment {
    /// Commit to `events` in order: their count, and a blake3 digest folding each
    /// event's `Moment` and length-prefixed raw bytes (the length prefix removes any
    /// concatenation ambiguity between adjacent records).
    pub(crate) fn of(events: &[SdkEvent]) -> StreamCommitment {
        let mut hasher = blake3::Hasher::new();
        for ev in events {
            hasher.update(&ev.moment.0.to_le_bytes());
            hasher.update(&(ev.raw.bytes.len() as u64).to_le_bytes());
            hasher.update(&ev.raw.bytes);
        }
        StreamCommitment {
            events: events.len() as u64,
            digest: *hasher.finalize().as_bytes(),
        }
    }
}

/// The result of decoding one ingress stream: the normalized schema, the ordered
/// events, and a [`StreamCommitment`] binding the artifact to the complete stream it
/// decoded from. The schema's entries and the events' ordinals are canonical and
/// identical across platforms.
///
/// `Normalized` is the persisted artifact and the **only** publicly-deserializable
/// entry point: its `#[serde(try_from)]` re-decodes the artifact's own bytes and
/// requires structural equality *and* stream-commitment agreement, so component types
/// like [`SdkEvent`]/[`SdkSchema`] carry no bare `Deserialize` that could bypass it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "NormalizedRepr")]
pub struct Normalized {
    /// The normalized schema derived from (or declared by) the stream.
    pub schema: SdkSchema,
    /// The ordered events, in persisted (source-ordinal) order.
    pub events: Vec<SdkEvent>,
    /// The commitment over the complete event stream (count + digest).
    pub commitment: StreamCommitment,
}

impl Normalized {
    /// Seal a freshly decoded stream: bind `events` with a [`StreamCommitment`] so a
    /// later load can detect truncation/extension/tampering that content re-decode
    /// alone cannot (a truncated stream re-decodes to itself). Every decoder path
    /// mints a `Normalized` through here, so the commitment is always present and
    /// correct by construction.
    pub(crate) fn seal(schema: SdkSchema, events: Vec<SdkEvent>) -> Normalized {
        let commitment = StreamCommitment::of(&events);
        Normalized {
            schema,
            events,
            commitment,
        }
    }
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
    /// The **V-time anchor lower bound** this event surfaced at — a point on the
    /// `Moment` axis, **not** the emission instant of the doorbell hypercall that
    /// produced the record. An SDK event is drained at a `run_until` anchor, which
    /// quantizes the true emission `Moment` *up* to that anchor (a skew of ~27 frames
    /// per stamp was observed on the SMB workload). This boundary carries the stamp
    /// through faithfully and neither tightens nor loosens it: the contract is that an
    /// `SdkEvent`'s `moment` is a **lower bound** on the emission — usable for coarse
    /// localization, never read as the exact emission instant.
    ///
    /// Consequences a consumer must honor:
    /// - **Order, and any included-count cut, are by [`ordinal`](SdkEvent::ordinal)**
    ///   — the rollout-local SDK-vector position — never by `moment`. Distinct events
    ///   may share one anchor `moment`; the `ordinal` is the total order and the cut
    ///   coordinate, the `moment` is neither.
    /// - Treating `(event, moment)` as an exact address (event-index targeting,
    ///   resolution `run_to`, logtmpl correlation) inherits the anchor skew and must
    ///   either account for it (a calibration pass, as the film gate does) or work in
    ///   ordinal space.
    ///
    /// Tightening the stamp to the exact emission `Moment` is a spine concern (at the
    /// hypercall-handling exit), above this data boundary; it would be an observable,
    /// versioned change to the persisted stamp (see the decoder-pinning invariant in
    /// the crate root), so it is never done implicitly here.
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
    commitment: StreamCommitment,
}

/// Reconstruct the ingress stream a candidate artifact was decoded from — its
/// schema's `original_declaration` (the catalog, first) followed by every event's
/// preserved `raw` record, in the artifact's own vector order — and re-run the live
/// decoder over it.
///
/// This is the whole of load validation: `redecode(candidate)` is *what a live decode
/// of the artifact's own bytes produces*, so requiring it to equal `candidate`
/// (below) makes "loadable" definitionally "decoder-minted". A binary event whose
/// `raw` carries no `event_id` cannot be placed back on the wire, so it can be no
/// live decode's output — a divergence, not a panic.
fn redecode(candidate: &Normalized) -> Result<Normalized, SdkError> {
    let diverged = |detail: String| SdkError::ArtifactDivergedFromDecode { detail };
    match candidate.schema.source {
        SourceFormat::BinaryV1 | SourceFormat::BinaryV2 => {
            let mut records: Vec<(Moment, u32, Vec<u8>)> = Vec::new();
            // The catalog governs the batch and must precede every firing, so it is
            // reconstructed first. Its own `Moment` is not part of the schema (decode
            // ignores it), so any value round-trips; `CATALOG_EVENT_ID` is what marks
            // it as the catalog, and the comparison re-checks the stored `event_id`.
            if let Some(decl) = &candidate.schema.original_declaration {
                records.push((Moment(0), wire::CATALOG_EVENT_ID, decl.bytes.clone()));
            }
            for ev in &candidate.events {
                let event_id = ev.raw.event_id.ok_or_else(|| {
                    diverged(format!(
                        "binary event at ordinal {} has no raw event_id to reconstruct",
                        ev.ordinal
                    ))
                })?;
                records.push((ev.moment, event_id, ev.raw.bytes.clone()));
            }
            crate::binary::decode_binary(&records)
        }
        SourceFormat::AntithesisJson => {
            // Antithesis declares implicitly through its records; there is no catalog.
            let records: Vec<(Moment, Vec<u8>)> = candidate
                .events
                .iter()
                .map(|ev| (ev.moment, ev.raw.bytes.clone()))
                .collect();
            crate::antithesis::decode_antithesis(&records)
        }
    }
}

impl TryFrom<NormalizedRepr> for Normalized {
    type Error = SdkError;

    /// Validate a persisted artifact by **re-decoding and comparing**, not by
    /// enumerating coherence rules. The candidate is reconstructed from the repr,
    /// its own preserved bytes are replayed through the live decoder ([`redecode`]),
    /// and the result must be *structurally equal* to the candidate — so a
    /// `Normalized` is loadable exactly when it is what a live decode produces.
    ///
    /// This closes the whole family by construction: a payload from a source that
    /// cannot mint it, a `min`/`accumulate` firing "upgraded" from raw at an
    /// undeclared coordinate, a shifted or non-contiguous ordinal, a `raw` record
    /// contradicting the evidence it vouches for, altered token content, an unsorted
    /// or fabricated schema entry — none survive, with nothing left to enumerate. A
    /// reconstructed stream the decoder itself rejects (e.g. a `set` at a
    /// `max`-declared coordinate) surfaces that decoder's own typed error
    /// ([`MixedOperations`](SdkError::MixedOperations)); everything else that differs
    /// is a typed [`ArtifactDivergedFromDecode`](SdkError::ArtifactDivergedFromDecode),
    /// kept only for diagnosability.
    ///
    /// **Completeness** is the one thing content re-decode cannot check on its own — a
    /// truncated event vector re-decodes *to itself*, since the reconstructed stream is
    /// truncated with it. The persisted [`StreamCommitment`] closes that: recomputed
    /// from the re-decoded stream, its count and digest must match the stored value, so
    /// a truncated, extended, or raw-tampered artifact fails with a typed
    /// [`StreamCommitmentMismatch`](SdkError::StreamCommitmentMismatch). Checked first,
    /// so raw-byte tampering is reported as the commitment violation it is.
    ///
    /// The load contract this enforces is **decoder pinning** (see the crate root): a
    /// persisted artifact is pinned to the semantics of the decoders that produced it.
    fn try_from(repr: NormalizedRepr) -> Result<Normalized, SdkError> {
        let candidate = Normalized {
            schema: SdkSchema::from(repr.schema),
            events: repr.events.into_iter().map(SdkEvent::from).collect(),
            commitment: repr.commitment,
        };
        let redecoded = redecode(&candidate)?;
        // Completeness: the re-decoded stream must match the persisted commitment. The
        // stored commitment was minted over the *whole* original stream, so a truncated
        // (or extended, or raw-tampered) candidate re-decodes to a different count/digest
        // than it carries — the subset a content re-decode would otherwise accept.
        if redecoded.commitment != candidate.commitment {
            return Err(SdkError::StreamCommitmentMismatch {
                detail: format!(
                    "committed {} event(s), re-decoded {}{}",
                    candidate.commitment.events,
                    redecoded.commitment.events,
                    if redecoded.commitment.events == candidate.commitment.events {
                        " (event digest differs)"
                    } else {
                        ""
                    }
                ),
            });
        }
        if redecoded != candidate {
            return Err(SdkError::ArtifactDivergedFromDecode {
                detail: "re-decoding the artifact's own bytes yields a different artifact"
                    .to_string(),
            });
        }
        // `redecoded == candidate`; return the decoder-minted one as the canonical form.
        Ok(redecoded)
    }
}
