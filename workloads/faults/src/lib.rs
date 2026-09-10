// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic fault-search package for distributed workloads.
//!
//! A workload image declares its nodes, hooks, readiness and setup commands in
//! `/etc/harmony/bundle`. The package boots that image under the in-guest
//! fault agent, and the search draws an action list the agent enforces: kill a
//! node, pause it, restart it, run a hook, park a thread at an execution
//! place, inject an interrupt, or wait. Each action runs for a fixed horizon
//! of guest time, so an input names an exact schedule the whole-VM snapshots
//! reproduce.

#[cfg(feature = "consonance")]
pub mod action_execution;
pub mod archive;
pub mod bundle;
pub mod checkpoint;
#[cfg(feature = "consonance")]
pub mod continuation;
pub mod declarations;
pub mod execution;
pub mod package;
pub mod prepare;
pub mod report;
#[cfg(feature = "consonance")]
pub mod retained;
pub mod target;
pub mod workspace;

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
pub mod campaign;
#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
pub mod consonance;

pub use bundle::FaultVocabulary;
pub use package::{Artifacts, Options, RecordedActions, Report, parse_recorded_input};
pub use target::{DEFAULT_HORIZON_NANOS, FaultAction, MAX_FAULT_ACTIONS};
