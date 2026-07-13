// SPDX-License-Identifier: AGPL-3.0-or-later
//! # sdk-events — the link-tier plugin (task 73)
//!
//! `sdk-events` is the **host-side reader of the guest SDK**: it turns the events a
//! cooperating in-guest workload emits (via `harmony-sdk`, over the hypercall
//! Event service) into the search-plane's replay-plane vocabulary. It is Tier 2
//! of `docs/DISSONANCE.md`'s cooperation gradient — a channel for *code you own*,
//! sitting beside the scrape tier (task 65's `runtrace`), not displacing it.
//!
//! Four pieces, all pure replay-plane logic:
//!
//! - **Event decode** ([`decode_events`]): raw `(Moment, event_id, bytes)` →
//!   typed `(Moment, GuestEvent)` for [`RunTrace::events`](explorer::RunTrace).
//!   Total and panic-free on arbitrary bytes.
//! - **The catalog + never-fired report** ([`Catalog`]/[`CatalogReport`]): the
//!   declared-at-init point set folded against the fired set, in a format
//!   unified with task 66's config-declared catalog (one report across link and
//!   scrape).
//! - **The link [`Sensor`](explorer::Sensor)** ([`LinkSensor`]): a `sometimes`
//!   hit or a state-register change → a `(Moment, Feature)` in the feature
//!   stream.
//! - **The [`AlwaysViolation`] [`Oracle`](explorer::Oracle)**: a
//!   [`StopReason::Assertion`](explorer::StopReason) terminal → a
//!   [`Bug`](explorer::Bug) with a genesis-complete `env` and a stable
//!   fingerprint.
//!
//! ## Its place in the plane
//!
//! `sdk-events` depends on `explorer` and consumes the task-64 spine vocabulary
//! ([`GuestEvent`](explorer::GuestEvent), [`Feature`](explorer::Feature),
//! [`Sensor`](explorer::Sensor), [`Oracle`](explorer::Oracle),
//! [`RunTrace`](explorer::RunTrace), [`Bug`](explorer::Bug)); it is a **pure
//! plugin** — nothing here reads back into the live plane, and no search policy
//! learns it exists (search-loop blindness). It sits beside `runtrace` (task 65)
//! as the second replay-plane channel.
//!
//! ## The wire convention
//!
//! The SDK event byte format is owned by the guest SDK crate
//! (`guest/sdk/src/wire.rs`, the canonical source). `sdk-events` mirrors those constants
//! privately (`wire.rs`, conventions rule 2 — the guest/host protocol pattern)
//! and the decode goldens in `tests/decode.rs` pin byte-for-byte agreement.
//!
//! ## Determinism discipline
//!
//! Canonical encodings only (`BTreeMap`/`BTreeSet` walked sorted, no floats, no
//! wall-clock); the `Bug` fingerprint is a `sha2` digest of the stop reason;
//! decode is a pure function of its bytes. Library code never panics on untrusted
//! input — a malformed event stream decodes to `unknown` events and an empty/
//! partial catalog, never a crash.

mod catalog;
mod decode;
mod oracle;
mod read;
mod sensor;
mod wire;

pub use catalog::{Catalog, CatalogReport, PointKind};
pub use decode::{
    KIND_ASSERT_HIT, KIND_ASSERT_VIOLATION, KIND_BUGGIFY, KIND_CATALOG, KIND_SETUP_COMPLETE,
    KIND_STATE, KIND_UNKNOWN, decode_event, decode_events,
};
pub use oracle::AlwaysViolation;
pub use sensor::{LINK_ASSERT_CHANNEL, LINK_STATE_CHANNEL, LinkSensor};
