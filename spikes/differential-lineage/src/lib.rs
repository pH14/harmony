// SPDX-License-Identifier: AGPL-3.0-or-later
//! # differential-lineage — spike (tasks/120, `hm-bbx.2`)
//!
//! A bounded Differential Dataflow program over persisted fixture records,
//! proving the hardest queries of the Dissonance observation/materialization
//! plane before any production integration: lineage-complete observation
//! prefixes at candidate seals, provisional transitions at unsealed evidence
//! cuts (replay nomination, never occupancy), sibling-safe rollout identity,
//! half-open same-`Moment` cuts, canonical order reconstruction,
//! `set`/`max`/`min`/`accumulate` and history derivations, property-level
//! assertion aggregation, and the separation of immutable evidence, bounded
//! working membership, committed Entry assignments, and finalized facts —
//! with the arrangement-sharing cost story measured against direct recompute.
//! Standalone; depends on nothing in `consonance/` or `dissonance/`.

pub mod data;
pub mod dataflow;
pub mod fixtures;
pub mod generate;
pub mod referee;
