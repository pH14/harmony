// SPDX-License-Identifier: AGPL-3.0-or-later
//! # runtrace — the RunTrace journal, store, and scrape decoder (task 65)
//!
//! `runtrace` is the **replay-plane infrastructure** that makes a finished run
//! *recordable*. After task 58's control loop stops, the campaign runner assembles a
//! [`RunTrace`](explorer::RunTrace) — the versioned, serialized bundle the whole
//! replay plane (Sensors, Oracles, re-derivation) works over offline — and this
//! crate is where that bundle is turned into bytes and back:
//!
//! - **The versioned journal** ([`encode`]/[`decode`]): a canonical, magic +
//!   version-tagged binary format modeled on `control-proto`'s codec discipline.
//!   Equal traces encode to equal bytes; an unknown format version fails loudly
//!   with [`TraceError::Version`] (never a silent reinterpretation). A run is
//!   content-addressed by [`TraceId`] = `blake3` of its canonical env bytes, so
//!   determinism makes byte-stability id-stability for free.
//! - **The scrape-tier decoder** ([`decode_chunks`]/[`ChunkDecoder`]): the
//!   concrete [`Record`](explorer::Record) decode — console bytes → timestamped
//!   records, total and lossless over torn/non-UTF-8 input. Runs offline over
//!   any recorded chunk stream, including a telemetry NDJSON `Console` recording
//!   via [`ingest_ndjson`].
//! - **The store** ([`TraceStore`]): directory-backed, always persisting the
//!   tiny env sidecar and — under the [`Retain`]/[`RetentionPolicy`] knob — the
//!   full journal for a retained subset (`docs/EXPLORATION.md`'s "not a data
//!   lake" ruling).
//!
//! ## Its place in the plane
//!
//! runtrace depends on `explorer` and consumes the task-64 spine vocabulary
//! ([`RunTrace`](explorer::RunTrace), [`Record`](explorer::Record),
//! [`StreamId`](explorer::StreamId), [`Moment`](explorer::Moment)); it is a
//! **pure sink** — nothing here reads back into the live plane, and no search
//! policy learns the store exists (search-loop blindness). It is the Wave-5
//! plugin direction later signal/matcher/search tasks (66/67/70+) reuse. The
//! concrete `Record` shape (and [`StreamId`](explorer::StreamId)) it produces
//! was pinned by this task, additively, next to `RunTrace` in the spine.
//!
//! ## Determinism discipline
//!
//! Canonical encodings only (`BTreeMap`s walked sorted, no floats, no
//! wall-clock); stamps come only from deterministic counters mapped one-for-one
//! onto the [`Moment`](explorer::Moment) axis; content addressing is a pure
//! `blake3` of canonical bytes. Library code never panics on untrusted input —
//! a malformed journal is a typed [`TraceError`], not a crash.

mod codec;
mod error;
mod ingest;
mod scrape;
mod store;

/// The on-disk journal format version. Bump this — and re-freeze the golden
/// fixture — whenever the [`encode`]/[`decode`] byte layout changes; a journal
/// written at any other version fails [`decode`] with [`TraceError::Version`]
/// (the bump procedure is documented in `IMPLEMENTATION.md`, mirroring
/// `control-proto`'s `PROTO_VERSION`).
pub const TRACE_FORMAT_VERSION: u16 = 1;

pub use codec::{decode, encode, encode_env};
pub use error::{TraceError, TraceId};
pub use ingest::ingest_ndjson;
pub use scrape::{ChunkDecoder, decode_chunks};
pub use store::{Retain, RetentionPolicy, TraceStore, retain_for};
