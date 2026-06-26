// SPDX-License-Identifier: AGPL-3.0-or-later
//! # telemetry — out-of-band observation tap + std-only web console
//!
//! A **read-only telemetry lane** for the deterministic VMM. The guest→host data
//! lanes that already exist — the serial console (hashed into M2), the `Event`
//! hypercall service (id 4), and task 28's report channel (`0x0CA2`, folded into
//! `observable_digest`) — are all **in-band and deterministic**. This crate adds
//! **none** of them. It adds a host-side tap that *watches* the exit stream
//! `vmm-core` already services and copies it out for a human:
//!
//! - The [`Observer`] trait — the one new seam. `vmm-core` calls
//!   [`Observer::emit`] after each serviced exit with an already-built [`Event`].
//!   The contract is **read-only** (`&Event` in, `()` out), so attaching an
//!   observer cannot draw entropy, advance `work`, or mutate any state feeding
//!   `state_hash` — determinism is preserved *by construction*. The default is
//!   [`NullObserver`] (a no-op), so M1/M2/corpus/Linux goldens stay
//!   byte-identical unless an operator opts a real sink in.
//! - The sinks: [`NdjsonRecorder`] (lossless, the replay source of truth) and
//!   [`LiveSink`] (lossy, never blocks — drops and counts under load).
//! - The [`Event`]/[`EventKind`] schema and its NDJSON wire ([`to_ndjson`] /
//!   [`from_ndjson`]), round-trip proptested.
//! - The std-only web [`server`]: SSE `/events`, `/recording` replay, the
//!   embedded vanilla-JS UI. No async runtime, no framework, no npm, no build
//!   step — just [`std::net::TcpListener`] and `serde_json`.
//!
//! Nothing here is ever hashed, folded into `observable_digest`/`state_hash`, or
//! fed back to the guest. Telemetry is for the operator; the hashes remain the
//! source of truth. The per-exit wiring inside `vmm-core` is **frontier**
//! (integrator-owned) and is documented, not built, here — see
//! `docs/INTEGRATION.md` §8. This crate is driven in tests by a scripted
//! `Vec<Event>` with no KVM.
//!
//! ## Record → replay (the integrator's use case)
//!
//! Postgres/Docker workloads are box-only (patched KVM). The path is built so a
//! **box** run attaches an [`NdjsonRecorder`] (captured to a file) and/or a
//! [`LiveSink`] (live view where the VMM runs), and the captured file replays in
//! the **Mac** console identically: the console keys every render on `vns`, a
//! pure function of the run, so live and replay use the same renderer.

mod event;
mod observer;
mod sink;

pub mod server;

pub use event::{Event, EventKind, ExitCounts, WireError, from_ndjson, to_ndjson};
pub use observer::{NdjsonRecorder, NullObserver, Observer};
pub use server::{Mode, RunningServer, ServerOptions, serve};
pub use sink::{DEFAULT_CAPACITY, LiveSink};
