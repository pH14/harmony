// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fault agnostic deterministic execution inputs for Consonance.
//!
//! The core records bounded service traffic, extension state, and mechanical
//! machine effects. Workload packages own fault catalogs and policy.

/// Fault agnostic question/answer and extension-state transition API.
pub mod channel;
/// Versioned deterministic inputs and service factory configuration.
pub mod input_spec;

/// Coordinate on the deterministic execution timeline.
pub type Moment = u64;
