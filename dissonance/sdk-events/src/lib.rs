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
//! Content equality alone leaves one gap — **completeness**: a truncated event vector
//! re-decodes *to itself*, because the stream it reconstructs from is truncated with
//! it. A [`StreamCommitment`] (event count + a blake3 digest over the ingress records)
//! is minted once at decode over the whole stream and persisted, so the load recomputes
//! it from the re-decoded stream and rejects any artifact whose extent or raw bytes
//! disagree with the stored value. Content is pinned by re-decode; completeness by the
//! commitment.
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
//! ## No judgment, no cells, no dependency on the Explorer
//!
//! This boundary decodes and normalizes; it defines its own [`Moment`] (rather
//! than importing the Explorer's) so it depends on **no** other dissonance crate
//! — the Explorer consumes [`Normalized`] evidence, so a dependency the other way
//! would be a cycle. The pre-normalization link-tier compatibility surface (the
//! `LinkSensor`, the packed `(register, value) → FeatureId`, the `AlwaysViolation`
//! oracle, and the `decode_events`/`Catalog` `GuestEvent` path) was **deleted
//! during the Differential integration** (`hm-bbx.4`, per
//! `docs/DISSONANCE-STRATEGY.md`): temporal reduction, cell projection, and oracle
//! judgment live in the Explorer/Differential layer over this crate's ordered
//! `SdkEvent`s, never in the decoder. A workload's own cell derivation (e.g. the
//! game campaign's packed register state) is campaign policy that lives with the
//! campaign, not a reusable sensor here.

mod antithesis;
mod binary;
mod error;
mod event;
mod moment;
mod numeric;
mod read;
mod schema;
mod wire;

// The normalized SDK ingress boundary (hm-bbx.1).
pub use antithesis::decode_antithesis;
pub use binary::{DeclaredPoint, decode_binary, encode_v2_declaration, resolve_v1_declaration};
pub use error::SdkError;
pub use event::{AssertType, Normalized, Payload, SdkEvent, SiteId, StreamCommitment};
pub use moment::Moment;
pub use numeric::{BoundedNumeric, NumericError, NumericLimits, NumericToken};
pub use schema::{
    Classification, Expectation, ObservationId, OrderingScope, Raw, SchemaEntry, SdkSchema,
    SourceFormat, UpdateOp, ValueShape,
};
pub use wire::{NS_ASSERT, NS_BUGGIFY, NS_CONTROL, NS_LIFECYCLE, NS_STATE};
