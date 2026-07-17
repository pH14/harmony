// SPDX-License-Identifier: AGPL-3.0-or-later
//! # sdk-events — the SDK ingress boundary
//!
//! `sdk-events` is the host-side **data boundary** between a cooperating workload's
//! SDK output and Dissonance's observation plane. It **decodes and normalizes**;
//! it does **not** judge, reduce temporal state, assign cells, or run archive
//! policy — those live above this boundary (Differential / `CampaignConfig` / the
//! Explorer oracles). See `docs/DISSONANCE-STRATEGY.md` and `docs/LAYERS.md`
//! §R-L3.
//!
//! ## The two ingress formats, one normalized contract
//!
//! Both LAYERS R-L3 ingress formats decode into the same persisted model —
//! [`SdkSchema`] (declarations) plus ordered [`SdkEvent`]s ([`Normalized`]):
//!
//! - [`decode_antithesis`] — the app-facing **Antithesis SDK JSON** over
//!   `/dev/harmony`. Assertions become occurrence/property evidence; numeric
//!   guidance normalizes to its declared monotone extremum only (never arbitrary
//!   `set` state), preserving the original number token report-only.
//! - [`decode_binary`] — the internal **binary Event wire**. v1 identities and
//!   fired operations are preserved without guessing a never-fired reducer; the
//!   new **wire-v2** declaration ([`encode_v2_declaration`]/[`DeclaredPoint`])
//!   carries occurrence/state classification, value shape, and base update
//!   operation for the cooperative production path.
//!
//! Both preserve the original declaration and raw unknown bytes recoverably, carry
//! an explicit [`OrderingScope`], and surface [typed errors](SdkError) for
//! structural contradictions (mixed operations/shapes, malformed declaration
//! lengths) — never a panic on untrusted input.
//!
//! ## Decoder pinning (binding load invariant)
//!
//! [`Normalized`] is the persisted artifact and the **only** publicly-deserializable
//! type. Loading one is not a second, hand-written validation of its fields: it
//! **re-decodes the artifact's own preserved bytes** (each event's `raw` record plus
//! the schema's `original_declaration`, in order) through the live decoders and
//! requires the result to be *structurally equal* to the persisted artifact.
//! *Loadable* is therefore, by construction, *exactly what a live decode produces* —
//! there is no set of coherence rules to enumerate and no gap for a plausible-looking
//! but decoder-unmintable artifact to slip through.
//!
//! The consequence is a **binding contract: a persisted artifact is pinned to the
//! semantics of the decoders that produced it.** Any future change to decoder
//! semantics (a new wire version, a changed normalization) must **version and
//! migrate** existing artifacts — it must never silently reinterpret them, because a
//! stored artifact that no longer round-trips through the current decoders will fail
//! to load rather than load with quietly different meaning. This is the ingestion-side
//! face of the epic's determinism doctrine (persisted evidence has one fixed meaning).
//! Two corollaries: load cost is `O(re-decode)` per artifact — accepted, as an
//! artifact-boundary operation in an evidence-integrity crate; and a **subset or
//! filtered event vector is not a valid persisted artifact** (a partial vector already
//! violates the contiguous-ordinal contract and will not re-decode to itself).
//!
//! ## Determinism discipline
//!
//! Canonical encodings only: schema entries are sorted and unique, no `HashMap`
//! iteration reaches an output, and **no `f64` ever touches a value** — numeric
//! guidance is preserved as its original token and normalized into a bounded exact
//! decimal ([`NumericToken`]/[`BoundedNumeric`]) with a deterministic total order
//! only on demand. Normalized output is byte-identical across macOS and Linux.
//!
//! ## Legacy compatibility surface (deleted during the Differential integration)
//!
//! The pre-normalization link-tier surface — [`decode_events`]/[`Catalog`], the
//! [`LinkSensor`], and the [`AlwaysViolation`] oracle — remains for the
//! `campaign-runner` game path. Per `docs/DISSONANCE-STRATEGY.md` these are
//! compatibility machinery to **delete during the Differential integration**
//! (`hm-bbx.4`), not to rename or extend here; the normalized boundary above adds
//! no judgment of its own.

mod antithesis;
mod binary;
mod catalog;
mod decode;
mod error;
mod event;
mod numeric;
mod oracle;
mod read;
mod schema;
mod sensor;
mod wire;

// The normalized SDK ingress boundary (hm-bbx.1).
pub use antithesis::decode_antithesis;
pub use binary::{DeclaredPoint, decode_binary, encode_v2_declaration};
pub use error::SdkError;
pub use event::{AssertType, Normalized, Payload, SdkEvent, SiteId};
pub use numeric::{BoundedNumeric, NumericError, NumericLimits, NumericToken};
pub use schema::{
    Classification, Expectation, ObservationId, OrderingScope, Raw, SchemaEntry, SdkSchema,
    SourceFormat, UpdateOp, ValueShape,
};
pub use wire::{NS_ASSERT, NS_BUGGIFY, NS_CONTROL, NS_LIFECYCLE, NS_STATE};

// The legacy link-tier compatibility surface (see the crate doc).
pub use catalog::{Catalog, CatalogReport, PointKind};
pub use decode::{
    KIND_ASSERT_HIT, KIND_ASSERT_VIOLATION, KIND_BUGGIFY, KIND_CATALOG, KIND_SETUP_COMPLETE,
    KIND_STATE, KIND_UNKNOWN, decode_event, decode_events,
};
pub use oracle::AlwaysViolation;
pub use sensor::{LINK_ASSERT_CHANNEL, LINK_STATE_CHANNEL, LinkSensor};
