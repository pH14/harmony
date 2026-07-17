// SPDX-License-Identifier: AGPL-3.0-or-later
//! The normalized, persisted **`SdkSchema`** — the host-side data contract both
//! ingress formats decode into.
//!
//! Per `docs/DISSONANCE-STRATEGY.md`, `SdkSchema` is *ordinary versioned data
//! persisted with the trace*, not a new application API object: stable event
//! identities, value shapes, whether an identity reports occurrences or state,
//! and — when state-bearing — the base update operation needed to reconstruct
//! values. It deliberately does **not** decide the final cell representation, run
//! any temporal reduction, or judge anything; those live above this boundary
//! (Differential / `CampaignConfig` / the Explorer oracles).
//!
//! A declaration whose legacy source cannot supply a base operation stays
//! *reportable* ([`UpdateOp`] `None`) but is not eligible for state reduction
//! until a versioned source declaration resolves it — the binary-v1 never-fired
//! contract. The original source declaration and raw bytes remain recoverable
//! ([`SdkSchema::original_declaration`]) so a later decoder can audit or migrate.

use serde::{Deserialize, Serialize};

use crate::error::SdkError;
use crate::wire;

/// Which source-specific ingress format produced a schema or event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SourceFormat {
    /// The app-facing Antithesis SDK JSON, decoded from `/dev/harmony` device
    /// traffic (`docs/LAYERS.md` §R-L3 items 1–4).
    AntithesisJson,
    /// The internal binary Event wire, version 1 (`guest/sdk`). Its catalog does
    /// not declare value shape or a fixed base operation, so state points are
    /// unresolved until a v2 declaration resolves them.
    BinaryV1,
    /// The internal binary Event wire, version 2 — the cooperative production
    /// declaration carrying occurrence/state classification, value shape, and base
    /// update operation.
    BinaryV2,
}

/// The ordering scope of an ingested source's evidence — how far its persisted
/// order can be trusted. Per the strategy, "persisted vector position is the
/// rollout-local source ordinal and must be contractual."
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum OrderingScope {
    /// Persisted vector position is the rollout-local source ordinal: events are
    /// ordered within this source, but cross-source sequencing needs a shared
    /// machine-event ordinal this boundary does not have.
    RolloutLocalSourceOrdinal,
    /// A batched source with only source-local order; it declares that limitation
    /// and cannot participate in cross-source sequence predicates.
    SourceLocalBatched,
}

/// A stable **observation identity** — the particular register, property, or point
/// an event concerns. Deliberately distinct from *source provenance* and from
/// *cell projection* (the strategy's separation of the four roles): the identity
/// names what is tracked, not where it came from or how a campaign discretizes it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ObservationId {
    /// A binary-wire point: an 8-bit namespace and a 24-bit local id.
    Point {
        /// The event-id namespace (assert / state / buggify / …).
        namespace: u8,
        /// The 24-bit local id within the namespace.
        local: u32,
    },
    /// An Antithesis **property** identity — the aggregated property the assertion
    /// message names. Multiple sites may contribute to one property.
    Property(String),
    /// A **lifecycle** point (e.g. `setup`). A disjoint variant so a lifecycle
    /// identity can never be forged by a user-controlled property message that
    /// happens to equal the lifecycle's name.
    Lifecycle(String),
}

/// Whether an identity reports one-shot **occurrences** or persistent **state**.
/// A one-shot occurrence (an assertion hit, a lifecycle point) is *not*
/// automatically persistent state; it says something happened at one `Moment`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Classification {
    /// A one-shot occurrence.
    Occurrence,
    /// A state-bearing register whose value persists until updated.
    State,
}

/// The base temporal update operation for a state-bearing observation — the
/// reduction needed to reconstruct the register's current value at a queried
/// `Moment`. These are the *base* operations only; historical derivations
/// (`ever`, `count`, `latest`, ordered patterns) are a campaign concern, not
/// declared here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum UpdateOp {
    /// Replace: the latest value at or before the `Moment` is current.
    Set,
    /// Keep the greatest value observed.
    Max,
    /// Keep the least value observed.
    Min,
    /// Retain the set of distinct values/species observed so far.
    Accumulate,
}

impl UpdateOp {
    /// The wire byte for this operation in a binary v2 declaration.
    pub(crate) fn to_byte(self) -> u8 {
        match self {
            UpdateOp::Set => 0,
            UpdateOp::Max => 1,
            UpdateOp::Min => 2,
            UpdateOp::Accumulate => 3,
        }
    }

    /// Decode an operation wire byte, or `None` for an unrecognized value.
    pub(crate) fn from_byte(b: u8) -> Option<UpdateOp> {
        match b {
            0 => Some(UpdateOp::Set),
            1 => Some(UpdateOp::Max),
            2 => Some(UpdateOp::Min),
            3 => Some(UpdateOp::Accumulate),
            _ => None,
        }
    }
}

/// The declared shape of an observation's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ValueShape {
    /// A bounded unsigned 64-bit integer (the initial cooperative-vertical
    /// representation).
    U64,
    /// A boolean.
    Bool,
    /// Opaque bytes (an assertion detail, an unclassified payload).
    Bytes,
    /// A numeric value carried as its original token — report-only until it
    /// normalizes into a bounded exact representation (never a host `f64`).
    Numeric,
}

impl ValueShape {
    /// The wire byte for this shape in a binary v2 declaration.
    pub(crate) fn to_byte(self) -> u8 {
        match self {
            ValueShape::U64 => 0,
            ValueShape::Bool => 1,
            ValueShape::Bytes => 2,
            ValueShape::Numeric => 3,
        }
    }

    /// Decode a shape wire byte, or `None` for an unrecognized value.
    pub(crate) fn from_byte(b: u8) -> Option<ValueShape> {
        match b {
            0 => Some(ValueShape::U64),
            1 => Some(ValueShape::Bool),
            2 => Some(ValueShape::Bytes),
            3 => Some(ValueShape::Numeric),
            _ => None,
        }
    }
}

/// A property/point **expectation** carried by a declaration — the absence-based
/// obligation reporting later evaluates. `sdk-events` only *preserves* it; the
/// derived never-fired / never-satisfied claim is a separate finalized view (the
/// strategy's "reporting owns the derived absence claim"), not computed here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Expectation {
    /// Must be hit / satisfied at least once (`sometimes` / `reachable`, and any
    /// `must_hit` property); a never-firing is a coverage gap or a claim to check.
    MustHit,
    /// Must never be hit (`unreachable`); a firing is the counterexample.
    MustNotHit,
}

/// Raw source bytes preserved verbatim so a later decoder can audit or migrate the
/// normalization. For a binary record `event_id` carries the original id; for a
/// JSON record it is `None` and `bytes` holds the original object text.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Raw {
    /// The ingress format the bytes are in.
    pub source: SourceFormat,
    /// The original binary event id, or `None` for a JSON record.
    pub event_id: Option<u32>,
    /// The original bytes, exactly as ingested.
    pub bytes: Vec<u8>,
}

/// One declared identity's normalized semantics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaEntry {
    /// The stable observation identity this entry declares.
    pub id: ObservationId,
    /// Whether the identity reports occurrences or state.
    pub classification: Classification,
    /// The value shape, or `None` for a pure occurrence with no reducible value.
    pub value_shape: Option<ValueShape>,
    /// The base update operation for a state identity, or `None` when unresolved
    /// (a binary-v1 state point whose reducer the source cannot declare). An
    /// occurrence always has `None` here.
    pub base_op: Option<UpdateOp>,
    /// The absence-based expectation, if the source declared one.
    pub expectation: Option<Expectation>,
    /// A human-readable name from the source declaration, if any.
    pub name: Option<String>,
}

impl SchemaEntry {
    /// Whether this entry is eligible for temporal **state reduction**: a state
    /// identity with a resolved base operation *and* the one currently-supported
    /// concrete value shape, the bounded integer [`ValueShape::U64`] (exact by
    /// construction).
    ///
    /// Everything else is reportable coverage, not reducible:
    /// - an unresolved v1 state point (no base op);
    /// - a **numeric-guidance** state point ([`ValueShape::Numeric`]): its value is
    ///   an unvalidated [`NumericToken`](crate::NumericToken), and the bounded exact
    ///   representation + total order needed to reduce it (the `NumericLimits`
    ///   selection) is not yet versioned in the persisted schema, so reducing it
    ///   under a consumer-chosen bound could replay differently;
    /// - a **shape-less** or non-`u64` resolved state (`value_shape` `None`/`Bool`/
    ///   `Bytes`): a resolved reducer with no reducible representation. The decoders
    ///   never produce this, but a public or *deserialized* [`SchemaEntry`] could,
    ///   so the check refuses it here (and [`SdkSchema`] deserialization rejects the
    ///   combination outright).
    pub fn is_reducible_state(&self) -> bool {
        self.classification == Classification::State
            && self.base_op.is_some()
            && self.value_shape == Some(ValueShape::U64)
    }

    /// The **single validation choke point** for the normalized schema model:
    /// enforce every source-specific invariant of a `SchemaEntry`. Every path that
    /// admits an entry from persisted input routes through here —
    /// [`SdkSchema::merge_entry`] (decode) and `SdkSchema`'s deserialization
    /// (`try_from`) — so the invariant set lives in one place and the decoders and
    /// persisted input are held to the same contract.
    ///
    /// The invariant family (a value the decoders never mint is refused on load):
    /// - **id ↔ source**: binary sources address [`ObservationId::Point`]s;
    ///   Antithesis addresses [`ObservationId::Property`]/[`ObservationId::Lifecycle`];
    /// - **point coordinate** (binary): the namespace matches the classification
    ///   (`NS_STATE`⇔state; `NS_ASSERT`/`NS_BUGGIFY`/`NS_LIFECYCLE`⇔occurrence), the
    ///   local id is an addressable 24-bit coordinate, and a lifecycle point sits at
    ///   the sole `setup_complete` local;
    /// - **occurrence is inert**: no base operation, no value shape (any source);
    /// - **state shape/op is source-specific**: binary-v1 leaves both unresolved;
    ///   binary-v2 is a resolved op over the bounded-integer `u64` shape;
    ///   Antithesis is numeric guidance — a resolved `max`/`min` over the
    ///   report-only `Numeric` shape.
    pub(crate) fn validate(&self, source: SourceFormat) -> Result<(), String> {
        let err = |msg: &str| Err(format!("entry {:?} (source {source:?}): {msg}", self.id));

        // id variant ↔ source, and — for binary points — the coordinate rules.
        match (source, &self.id) {
            (
                SourceFormat::BinaryV1 | SourceFormat::BinaryV2,
                ObservationId::Point { namespace, local },
            ) => {
                if *local > wire::LOCAL_MASK {
                    return err("point local id exceeds the 24-bit limit");
                }
                let expected = match *namespace {
                    wire::NS_STATE => Classification::State,
                    wire::NS_ASSERT | wire::NS_BUGGIFY | wire::NS_LIFECYCLE => {
                        Classification::Occurrence
                    }
                    _ => return err("point namespace has no reportable firing"),
                };
                if expected != self.classification {
                    return err("classification disagrees with the point namespace");
                }
                if *namespace == wire::NS_LIFECYCLE && *local != wire::LIFECYCLE_SETUP_COMPLETE {
                    return err("only the setup_complete lifecycle point (local 0) is reportable");
                }
            }
            (
                SourceFormat::AntithesisJson,
                ObservationId::Property(_) | ObservationId::Lifecycle(_),
            ) => {}
            _ => return err("id variant does not match the source"),
        }

        // An occurrence is inert — no reducer, no value shape — for every source.
        if self.classification == Classification::Occurrence {
            if self.base_op.is_some() {
                return err("occurrence carries a base operation");
            }
            if self.value_shape.is_some() {
                return err("occurrence carries a value shape");
            }
            return Ok(());
        }

        // State: the shape/op contract is source-specific.
        match source {
            SourceFormat::BinaryV1 => {
                if self.base_op.is_some() || self.value_shape.is_some() {
                    return err(
                        "binary v1 state must leave the reducer and value shape unresolved",
                    );
                }
            }
            SourceFormat::BinaryV2 => {
                if self.base_op.is_none() {
                    return err("binary v2 state must declare a base operation");
                }
                if self.value_shape != Some(ValueShape::U64) {
                    return err("binary v2 state must carry the u64 value shape");
                }
            }
            SourceFormat::AntithesisJson => {
                if !matches!(self.base_op, Some(UpdateOp::Max) | Some(UpdateOp::Min)) {
                    return err("antithesis state (guidance) must declare a max/min extremum");
                }
                if self.value_shape != Some(ValueShape::Numeric) {
                    return err("antithesis state (guidance) must carry the numeric value shape");
                }
            }
        }
        Ok(())
    }
}

/// The normalized, persisted schema: every declared identity's semantics, the
/// producing source, the ordering scope, and the recoverable original
/// declaration. Entries are kept sorted by [`ObservationId`] and unique, so the
/// serde form is **canonical** and identical across platforms (no `HashMap`
/// iteration order, no float).
///
/// Deserialization is guarded: [`SdkSchema::entry`] binary-searches `entries`, so
/// a persisted schema with unsorted or duplicate entries would make declared
/// identities unfindable. `#[serde(try_from)]` re-verifies the sorted-and-unique
/// invariant on the way in and rejects a non-canonical schema rather than
/// accepting silently corrupt evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SdkSchemaRepr")]
pub struct SdkSchema {
    /// The ingress format this schema was decoded from.
    pub source: SourceFormat,
    /// The trust scope of the source's persisted ordering.
    pub ordering: OrderingScope,
    /// Declared identities, sorted by id and unique.
    entries: Vec<SchemaEntry>,
    /// The original source declaration bytes, recoverable for audit/migration.
    /// `None` when the source carries no separate declaration blob (Antithesis
    /// JSON declares implicitly through its records).
    pub original_declaration: Option<Raw>,
}

/// The on-the-wire shape of an [`SdkSchema`], deserialized before the
/// sorted-and-unique invariant is re-checked (see [`SdkSchema`]'s `try_from`).
#[derive(Deserialize)]
struct SdkSchemaRepr {
    source: SourceFormat,
    ordering: OrderingScope,
    entries: Vec<SchemaEntry>,
    original_declaration: Option<Raw>,
}

impl TryFrom<SdkSchemaRepr> for SdkSchema {
    type Error = String;

    fn try_from(repr: SdkSchemaRepr) -> Result<SdkSchema, String> {
        // The invariant `entry()` depends on: strictly ascending, no duplicates.
        for pair in repr.entries.windows(2) {
            match pair[0].id.cmp(&pair[1].id) {
                std::cmp::Ordering::Less => {}
                std::cmp::Ordering::Equal => {
                    return Err(format!("duplicate schema entry id {:?}", pair[0].id));
                }
                std::cmp::Ordering::Greater => {
                    return Err("schema entries are not sorted by id".to_string());
                }
            }
        }
        // Every entry admitted from persisted input passes the one validation
        // choke point — the full source-specific invariant family.
        for entry in &repr.entries {
            entry.validate(repr.source)?;
        }
        Ok(SdkSchema {
            source: repr.source,
            ordering: repr.ordering,
            entries: repr.entries,
            original_declaration: repr.original_declaration,
        })
    }
}

impl SdkSchema {
    /// An empty schema for `source`/`ordering` with no original declaration.
    pub fn new(source: SourceFormat, ordering: OrderingScope) -> SdkSchema {
        SdkSchema {
            source,
            ordering,
            entries: Vec::new(),
            original_declaration: None,
        }
    }

    /// The declared entries, sorted by id.
    pub fn entries(&self) -> &[SchemaEntry] {
        &self.entries
    }

    /// The entry for `id`, if declared.
    pub fn entry(&self, id: &ObservationId) -> Option<&SchemaEntry> {
        self.entries
            .binary_search_by(|e| e.id.cmp(id))
            .ok()
            .map(|i| &self.entries[i])
    }

    /// The number of declared identities.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is declared.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Insert or reconcile a declared/observed identity, enforcing per-identity
    /// coherence. A second sighting of an identity must agree with the first:
    ///
    /// - a differing base operation is [`SdkError::MixedOperations`];
    /// - a differing value shape is [`SdkError::IncompatibleShapes`];
    /// - a flip between occurrence and state is [`SdkError::ClassificationConflict`].
    ///
    /// Reconciliation is monotone-refining: an unresolved (`None`) base op or value
    /// shape is filled in by a later resolved sighting, and a `None`→`Some`
    /// expectation is adopted; a resolved value is never silently overwritten by a
    /// conflicting one.
    pub(crate) fn merge_entry(&mut self, incoming: SchemaEntry) -> Result<(), SdkError> {
        // The one validation choke point: the incoming entry must satisfy every
        // source-specific invariant before it is admitted.
        incoming
            .validate(self.source)
            .map_err(|detail| SdkError::MalformedSchemaEntry { detail })?;
        match self.entries.binary_search_by(|e| e.id.cmp(&incoming.id)) {
            Ok(i) => {
                let existing = &mut self.entries[i];
                if existing.classification != incoming.classification {
                    return Err(SdkError::ClassificationConflict {
                        id: incoming.id,
                        first: existing.classification,
                        second: incoming.classification,
                    });
                }
                reconcile_option(&mut existing.base_op, incoming.base_op, |first, second| {
                    SdkError::MixedOperations {
                        id: incoming.id.clone(),
                        first,
                        second,
                    }
                })?;
                reconcile_option(
                    &mut existing.value_shape,
                    incoming.value_shape,
                    |first, second| SdkError::IncompatibleShapes {
                        id: incoming.id.clone(),
                        first,
                        second,
                    },
                )?;
                // Expectations refine `None` → `Some`; a genuine disagreement keeps
                // the first (a source should not declare a point both must-hit and
                // must-not-hit, but if it does the earlier declaration wins rather
                // than erroring — the expectation is advisory reporting data).
                if existing.expectation.is_none() {
                    existing.expectation = incoming.expectation;
                }
                if existing.name.is_none() {
                    existing.name = incoming.name;
                }
                Ok(())
            }
            Err(i) => {
                self.entries.insert(i, incoming);
                Ok(())
            }
        }
    }

    /// Attach the recoverable original declaration bytes.
    pub(crate) fn set_original_declaration(&mut self, raw: Raw) {
        self.original_declaration = Some(raw);
    }
}

/// Fill an unresolved slot or verify agreement; a resolved conflict is an error.
fn reconcile_option<T: PartialEq + Copy>(
    slot: &mut Option<T>,
    incoming: Option<T>,
    conflict: impl FnOnce(T, T) -> SdkError,
) -> Result<(), SdkError> {
    match (*slot, incoming) {
        (Some(a), Some(b)) if a != b => Err(conflict(a, b)),
        (None, Some(_)) => {
            *slot = incoming;
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The wire-byte codecs are exercised here directly: `to_byte` is only ever
    // *encoded* with `U64` on the valid v2 path (other state shapes are rejected
    // upstream), so its non-`U64` arms are unreachable from the integration
    // decoders and are pinned here instead.
    #[test]
    fn value_shape_byte_round_trips_every_variant() {
        for (shape, byte) in [
            (ValueShape::U64, 0u8),
            (ValueShape::Bool, 1),
            (ValueShape::Bytes, 2),
            (ValueShape::Numeric, 3),
        ] {
            assert_eq!(shape.to_byte(), byte, "{shape:?} encodes to {byte}");
            assert_eq!(ValueShape::from_byte(byte), Some(shape));
        }
        assert_eq!(ValueShape::from_byte(4), None);
    }

    #[test]
    fn update_op_byte_round_trips_every_variant() {
        for (op, byte) in [
            (UpdateOp::Set, 0u8),
            (UpdateOp::Max, 1),
            (UpdateOp::Min, 2),
            (UpdateOp::Accumulate, 3),
        ] {
            assert_eq!(op.to_byte(), byte);
            assert_eq!(UpdateOp::from_byte(byte), Some(op));
        }
        assert_eq!(UpdateOp::from_byte(4), None);
    }

    fn conflict(a: u8, b: u8) -> SdkError {
        SdkError::MixedOperations {
            id: ObservationId::Property(String::new()),
            first: UpdateOp::from_byte(a).unwrap_or(UpdateOp::Set),
            second: UpdateOp::from_byte(b).unwrap_or(UpdateOp::Set),
        }
    }

    // `reconcile_option`'s fill and conflict arms are not reachable through the
    // decoders (binary coordinates are unique, and same-identity Antithesis records
    // share a classification hence op/shape), so they are pinned directly.
    #[test]
    fn reconcile_fills_an_unresolved_slot() {
        let mut slot = None;
        assert!(reconcile_option(&mut slot, Some(1u8), conflict).is_ok());
        assert_eq!(
            slot,
            Some(1),
            "an unresolved slot is filled by a later value"
        );
    }

    #[test]
    fn reconcile_accepts_an_agreeing_value_and_rejects_a_conflicting_one() {
        let mut slot = Some(1u8);
        assert!(reconcile_option(&mut slot, Some(1), conflict).is_ok());
        assert_eq!(slot, Some(1));
        assert!(
            reconcile_option(&mut slot, Some(2), conflict).is_err(),
            "a conflicting value is a typed error"
        );
    }

    #[test]
    fn reconcile_keeps_an_existing_value_when_nothing_new_arrives() {
        let mut slot = Some(7u8);
        assert!(reconcile_option(&mut slot, None, conflict).is_ok());
        assert_eq!(slot, Some(7));
    }

    // `merge_entry` routes every decoded entry through the `validate` choke point,
    // so an invariant-violating entry is refused on the decode path too (not only
    // at deserialization). `merge_entry` is `pub(crate)`, so this is in-crate.
    #[test]
    fn merge_entry_rejects_an_invalid_entry_via_the_choke_point() {
        let mut schema = SdkSchema::new(
            SourceFormat::BinaryV1,
            OrderingScope::RolloutLocalSourceOrdinal,
        );
        // A binary-v1 state that resolves a reducer + shape violates INV-2.
        let bad = SchemaEntry {
            id: ObservationId::Point {
                namespace: wire::NS_STATE,
                local: 1,
            },
            classification: Classification::State,
            value_shape: Some(ValueShape::U64),
            base_op: Some(UpdateOp::Set),
            expectation: None,
            name: None,
        };
        assert!(matches!(
            schema.merge_entry(bad),
            Err(SdkError::MalformedSchemaEntry { .. })
        ));

        // A valid v1 state (unresolved) is admitted.
        let good = SchemaEntry {
            id: ObservationId::Point {
                namespace: wire::NS_STATE,
                local: 1,
            },
            classification: Classification::State,
            value_shape: None,
            base_op: None,
            expectation: None,
            name: None,
        };
        assert!(schema.merge_entry(good).is_ok());
    }
}
