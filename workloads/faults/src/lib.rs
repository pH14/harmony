// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic replicated-service fault-search package.

pub mod action;
pub mod oracle;
pub mod package;
pub mod prepare;
pub mod spec;

#[cfg(feature = "consonance")]
pub mod game;

pub use action::{ACTION_SCHEMA_VERSION, CatalogAction, DELAY_FORWARD_LATENCY_MS, FaultAction};
pub use package::{SearchIdentity, SearchOptions, search_consonance};
pub use spec::{CommandSpec, NetworkSpec, NodeSpec, WorkloadSpec};
