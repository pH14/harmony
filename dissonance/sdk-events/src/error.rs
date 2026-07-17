// SPDX-License-Identifier: AGPL-3.0-or-later
//! Typed decode/normalization errors.
//!
//! The boundary is **panic-free on untrusted input** (conventions rule 4): a
//! hostile or garbled stream never crashes the decoder. Unrecognized data is
//! *preserved raw*, not an error — an unknown namespace, an unknown JSON key, or
//! an opaque payload becomes a raw-carrying `Unknown` event. A **structural
//! contradiction** the boundary cannot normalize is a typed error instead:
//! malformed length prefixes, an identity carrying two different base operations
//! or value shapes, a flip between occurrence and state, or an unrecognized
//! declaration byte. Numeric tokens that do not fit a bounded exact representation
//! are *not* errors here — they stay report-only evidence (see
//! [`crate::numeric`]).

use thiserror::Error;

use crate::schema::{Classification, ObservationId, UpdateOp, ValueShape};

/// A typed ingress error. Every decoder returns `Result<_, SdkError>` and never
/// panics on adversarial input.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SdkError {
    /// A length prefix or fixed field claimed more bytes than the frame carried.
    #[error("malformed length in {context}: need {needed} bytes, {available} available")]
    MalformedLength {
        /// Where in the frame the overrun occurred.
        context: &'static str,
        /// Bytes the field claimed.
        needed: usize,
        /// Bytes actually available.
        available: usize,
    },

    /// One state identity carried two different base update operations — the
    /// binary-v1 "mixed operations for one identity are malformed evidence" rule,
    /// and the v2/guidance "declared op must match" rule.
    #[error("identity {id:?} carried conflicting base operations: {first:?} then {second:?}")]
    MixedOperations {
        /// The conflicted identity.
        id: ObservationId,
        /// The first operation seen.
        first: UpdateOp,
        /// The conflicting later operation.
        second: UpdateOp,
    },

    /// One identity carried two incompatible value shapes.
    #[error("identity {id:?} carried incompatible value shapes: {first:?} then {second:?}")]
    IncompatibleShapes {
        /// The conflicted identity.
        id: ObservationId,
        /// The first shape seen.
        first: ValueShape,
        /// The conflicting later shape.
        second: ValueShape,
    },

    /// One identity was classified both as an occurrence and as state.
    #[error("identity {id:?} classified as both {first:?} and {second:?}")]
    ClassificationConflict {
        /// The conflicted identity.
        id: ObservationId,
        /// The first classification seen.
        first: Classification,
        /// The conflicting later classification.
        second: Classification,
    },

    /// A declaration carried a byte in a fixed enumerated field that no version of
    /// the format defines.
    #[error("unrecognized {field} byte {value} in declaration")]
    UnknownDeclarationByte {
        /// The field the byte was in (`classification`, `value_shape`, `base_op`,
        /// `expectation`).
        field: &'static str,
        /// The unrecognized byte.
        value: u8,
    },

    /// The catalog declaration names a wire version this decoder does not
    /// understand. A future version may lay out event payloads differently, so the
    /// stream is refused rather than decoded under a guessed layout.
    #[error("unsupported catalog wire version {version}")]
    UnsupportedVersion {
        /// The version byte the declaration carried.
        version: u8,
    },

    /// A declared point's local id does not fit the 24-bit local-id field a runtime
    /// `event_id` splits into, so no firing could ever match it (it would mint a
    /// permanently never-fired identity). Rejected on both encode and decode.
    #[error("declared local id {local} in namespace {namespace} exceeds the 24-bit limit")]
    LocalIdOutOfRange {
        /// The declared point's namespace.
        namespace: u8,
        /// The out-of-range local id.
        local: u32,
    },

    /// A v2 declaration describes semantics the binary emission path cannot
    /// actually report (a non-`u64` state value, a state point with no base
    /// operation, an occurrence carrying a reducible value/operation, or a
    /// classification that disagrees with the namespace its firings arrive under).
    /// Accepting it would let schema and event evidence disagree, so it is refused
    /// — the "accept a declaration only for an emission path that reports every
    /// required update" rule, enforced in the codec.
    #[error("unsupported v2 declaration for point {local} in namespace {namespace}: {reason}")]
    UnsupportedDeclaration {
        /// The declared point's namespace.
        namespace: u8,
        /// The declared point's local id.
        local: u32,
        /// Why the binary emission path cannot honor the declaration.
        reason: &'static str,
    },

    /// A declaration lists the same runtime coordinate twice. A firing cannot
    /// distinguish two entries at one `(namespace, local)`, so the second would
    /// silently shadow the first; the declaration is refused instead.
    #[error("duplicate declared coordinate: namespace {namespace}, local {local}")]
    DuplicateCoordinate {
        /// The duplicated namespace.
        namespace: u8,
        /// The duplicated local id.
        local: u32,
    },

    /// A declared point name is longer than the `u16` length prefix can encode.
    /// Truncating it would corrupt the identity label irreversibly and break the
    /// round-trip contract, so the declaration is refused.
    #[error("declared name for point {local} in namespace {namespace} is {len} bytes (max {max})")]
    NameTooLong {
        /// The point's namespace.
        namespace: u8,
        /// The point's local id.
        local: u32,
        /// The name's byte length.
        len: usize,
        /// The maximum encodable length.
        max: usize,
    },

    /// A stream carries more than one catalog declaration (`event_id == 0`). One
    /// rollout declares its schema once; multiple declarations are ambiguous
    /// (which governs the layout of the events between them?), so the stream is
    /// refused rather than silently decoded under the first.
    #[error("stream carries {count} catalog declarations; exactly one is expected")]
    MultipleDeclarations {
        /// How many catalog declarations were found.
        count: usize,
    },

    /// The catalog declaration appears *after* one or more event firings. A
    /// declaration governs the whole batch, so applying it to bytes that preceded
    /// it would retroactively reassign semantics to prior untrusted input (a
    /// `min`/`accumulate` firing would become a v2 state update). The declaration
    /// must precede every firing.
    #[error("catalog declaration follows {firings_before} firing(s); it must come first")]
    DeclarationAfterFirings {
        /// How many firings preceded the declaration.
        firings_before: usize,
    },

    /// A normalized schema entry violates a source-specific invariant of the model
    /// (the single [`SchemaEntry::validate`](crate::SchemaEntry) choke point) — e.g.
    /// a binary-v1 state entry that resolves a reducer, a v2 state entry without the
    /// `u64` shape, an occurrence carrying a reducer, or an id whose variant does
    /// not match the source. Surfaced when such an entry is admitted from persisted
    /// input.
    #[error("malformed schema entry: {detail}")]
    MalformedSchemaEntry {
        /// A description of the violated invariant.
        detail: String,
    },

    /// A persisted [`Normalized`](crate::Normalized) artifact does not equal what the
    /// live decoders produce from its own preserved bytes. Loading re-decodes the
    /// reconstructed ingress stream (each event's `raw` + the schema's
    /// `original_declaration`, in order) and requires structural equality, so
    /// *loadable* is definitionally *what a live decode produces*. Any value the
    /// decoders never mint — a payload from the wrong source, an undeclared-coordinate
    /// upgrade, a shifted ordinal, contradictory raw provenance, altered token content
    /// — diverges here. (A tampered stream that the decoder itself rejects surfaces as
    /// that decoder's own error, e.g. [`MixedOperations`](SdkError::MixedOperations),
    /// exactly as during decode.)
    #[error("persisted artifact diverges from a live decode of its own bytes: {detail}")]
    ArtifactDivergedFromDecode {
        /// What diverged (a reconstruction failure, or which field differs).
        detail: String,
    },

    /// A catalog declaration carries bytes beyond its declared record `count`. The
    /// trailing bytes are unaccounted for (a miscounted or corrupted catalog), so
    /// the declaration is refused rather than silently discarding declared
    /// identities the strict schema would then omit.
    #[error(
        "catalog declaration has {extra} trailing byte(s) after its declared records ({context})"
    )]
    TrailingDeclarationBytes {
        /// Which catalog version was being parsed.
        context: &'static str,
        /// How many bytes remained after the declared record count.
        extra: usize,
    },
}
