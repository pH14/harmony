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
pub mod dispositions;
pub mod guest;
pub mod pack;
pub mod payload;
pub mod perf;
pub mod report;
pub mod stage0;
pub mod stage1;

// The measurement halves: Linux-only, because everything in them is a
// `perf_event_open`, a `/proc` or `/sys` read, an MSR read, or a KVM ioctl.
// Absent everywhere else, where the stages refuse loudly rather than reporting
// an empty pass. The guest window narrows further to x86-64: its payload is
// real-mode x86 and its state components are the x86 set.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod guest_sys;
#[cfg(target_os = "linux")]
pub mod perf_sys;
#[cfg(target_os = "linux")]
pub mod stage0_sys;
#[cfg(target_os = "linux")]
pub mod stage1_sys;

pub use chips::{ChipEntry, ChipIdentity, HostConditionKind, KNOWN_CHIPS, Refusal, match_chip};
pub use pack::{Field, Pack, PackError};
pub use report::{Check, Floors, Record, Verdict, parse_records, recompute};
pub use stage0::{Reading, Row, Stage0Error, Stage0Outcome, build_rows, rows_differ};
