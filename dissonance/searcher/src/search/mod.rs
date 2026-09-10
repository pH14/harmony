// SPDX-License-Identifier: AGPL-3.0-or-later

//! Game-neutral search mechanisms shared by target adapters.

pub mod archive;
pub mod campaign;
mod continuation;
pub mod draw;
pub mod empirical_steps;
mod key_counts;
pub mod parallel;
pub mod physical_work;
pub mod rand;
mod resource_coverage;
pub mod rollout;
#[cfg(feature = "selector-cost-audit")]
mod selector_cost;
