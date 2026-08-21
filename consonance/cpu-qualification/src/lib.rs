// SPDX-License-Identifier: AGPL-3.0-or-later
//! CPU qualification — the rung-1 suite of `docs/TESTING.md`, specified by
//! `docs/CPU-QUALIFICATION.md`.
//!
//! The suite runs on demand on a physical chip. It decides whether the chip can
//! host the determinism machinery, and it measures the constants the machinery
//! needs on that chip. A run produces two artifacts: the qualification report (the
//! evidence of one run, never checked in) and the measured-constants pack (the
//! per-chip data the VMM consumes, checked in at `docs/chips/<baseline>.toml`).
//!
//! The portable core — the known-chip table, the pack format, the report format,
//! and floor recomputation — compiles and is unit-tested everywhere. Measurement
//! is Linux-only behind `cfg`, mirroring the gating in `vmm-backend`.
//!
//! Nothing here reports pass or fail by omission. A stage that cannot run says
//! what is missing and fails; a capability probe never turns a missing host into a
//! pass.

#![deny(missing_docs)]

pub mod chips;
pub mod pack;
pub mod report;

pub use chips::{ChipEntry, ChipIdentity, HostConditionKind, KNOWN_CHIPS, Refusal, match_chip};
pub use pack::{Field, Pack, PackError};
pub use report::{Check, Floors, Record, Verdict, parse_records, recompute};
