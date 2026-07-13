// SPDX-License-Identifier: AGPL-3.0-or-later
//! # matcher — the declarative signal DSL + role router (task 66)
//!
//! Most signals should be authored as **config, not Rust** (`docs/EXPLORATION.md`,
//! "The matcher DSL"). This crate is the authoring layer that makes that true: a
//! generic [`MatchSensor`] / [`MatchOracle`] evaluates declarative match
//! expressions over any record type implementing the spine
//! [`Matchable`](explorer::Matchable) trait, and routes every match by its one
//! declared **[`Role`]** — `sometimes` / `cell` / `state_max` to a
//! [`Feature`](explorer::Feature) stream ([`MatchSensor`], an
//! [`explorer::Sensor`]), `never` to a bug verdict ([`MatchOracle`], an
//! [`explorer::Oracle`]). The declared set is also the [`Catalog`], so a
//! `sometimes` that never matched is never-fired detection — uniform across the
//! scrape (config-declared) and link (SDK-declared, task 73) tiers.
//!
//! ## Where it sits
//!
//! This crate is **replay-plane and pure**: a `MatchSensor`/`MatchOracle` is a
//! function of a finished [`RunTrace`](explorer::RunTrace), never consulted
//! mid-run — the open-loop rollout is untouched. Its load-bearing invariant
//! is **search-loop blindness**: adding, editing, or deleting a signal is a
//! config change that never edits the explorer loop. Accordingly the only
//! sibling dependency is `explorer` (the spine traits live there, so
//! conventions rule 2 — interfaces in the consumer — holds by construction);
//! this crate imports spine items and defines nothing the engine must learn.
//!
//! ## The four gates the semantics rest on
//!
//! 1. **Router totality** — every match routes to exactly its declared role's
//!    consumer; unmatched records route nowhere; no role leaks into another
//!    (channels are per-signal and disjoint across roles, see [`router`]).
//! 2. **The declared set is the catalog** — `never_fired = declared − fired`,
//!    tier-blind (see [`Catalog`]).
//! 3. **Purity + determinism** — pure per `RunTrace`; output is a deterministic
//!    function of record order via canonical `(Moment, index)` processing and a
//!    sorted stream; no floats, no `HashMap` iteration, seedless.
//! 4. **search-loop blindness** — imports spine items only; a config change
//!    adds signals with zero explorer edits.
//!
//! ## Config
//!
//! On disk a signal set is JSON (`serde_json`, the task-66 ruling over the
//! doc's illustrative YAML). Malformed config is a typed [`MatchError`], never a
//! panic. See [`SignalSet::from_json`]. The channel/context seams
//! ([`ChannelSource`] / [`ContextSource`]) are defined here; this crate ships
//! [`stub`] implementations only — the concrete channel adapters and the
//! production fault index are later tasks.

mod catalog;
mod error;
mod glob;
mod router;
mod signal;
pub mod stub;
mod value;

pub use catalog::{Catalog, CatalogReport};
pub use error::MatchError;
pub use router::{ChannelSource, ContextSource, MatchOracle, MatchSensor};
pub use signal::{During, MatchExpr, Role, SignalDecl, SignalId, SignalSet};
