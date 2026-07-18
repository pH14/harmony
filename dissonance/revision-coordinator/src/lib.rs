// SPDX-License-Identifier: AGPL-3.0-or-later
//! # revision-coordinator — Deterministic Differential revision coordination
//! (task 121, `hm-bbx.3`)
//!
//! The control-side input coordinator for the Differential observation plane
//! (`docs/DISSONANCE-STRATEGY.md`): it turns the imperative, seeded search
//! loop's completion stream into a frontier-closed, crash-recoverable
//! Differential input log. [`Coordinator::assign`] persists each proposal's
//! [`Revision`] assignment *before* dispatch (the persist-then-dispatch
//! handshake); [`Coordinator::complete`] atomically commits an
//! already-durable evidence-batch identity to its revision, buffering
//! out-of-order completions in a `BTreeMap`; [`Coordinator::drain_ready`]
//! emits contiguous revision-ordered prefixes and stops at the first hole;
//! [`Coordinator::probe_drive`] drives the one-worker Differential dataflow
//! until the search-visible frontier passes a target and returns the
//! consolidated, canonically ordered inputs; [`Coordinator::recover`]
//! rebuilds all of it from the durable [`Ledger`] with no frontier holes.
//! Cohorts ([`Coordinator::open_cohort`]) freeze the selector/archive view
//! at open and expose no partial-cohort result to another proposal.
//!
//! `Revision` is the ONLY timestamp: every state-affecting order comes from
//! the seeded issue order (the dense mint sequence), never completion
//! order, wall-clock, or worker arrival — one Timely worker alone does not
//! make input ordering deterministic; the seeded `assign` order does. The
//! coordinator neither decodes SDK payloads nor materializes VMs
//! (`hm-bbx.4` owns the evidence-ledger payloads) and never authors
//! Differential relations (the merged `spikes/differential-lineage` crate
//! defines the proven dataflow shapes it submits inputs to).
//!
//! **Scope of the in-crate dataflow** (task 132, `hm-e6q`): the dataflow
//! behind [`Coordinator::probe_drive`] carries two planes. The committed
//! `(Revision, EvidenceBatchId)` input relation — consolidated, captured,
//! probed, and read back as the [`DrainedView`] — is the coordination
//! contract, byte-for-byte unchanged from the PR #124 echo program. Beside
//! it now run the **production observation/materialization relations** (the
//! merged `spikes/differential-lineage` shapes, productionized in
//! [`relations`]): typed evidence rows staged per proposal
//! ([`Coordinator::stage_evidence`]) enter at their batch's committed
//! revision, and the graph materializes lineage-composed per-observation
//! reductions, cells under an installed projection
//! ([`Coordinator::set_cell_projection`]), and best-entry-per-cell
//! occupancy, read after the probe barrier via
//! [`Coordinator::materialized`]. The coordinator stays payload-blind: rows
//! carry opaque canonical observation identities, never decoded SDK bytes.

mod coordinator;
mod file_ledger;
mod host;
mod ids;
mod ledger;
pub mod relations;

pub use coordinator::{Completion, CoordError, Coordinator, DrainedView, PendingProposal};
pub use file_ledger::FileLedger;
pub use ids::{CampaignConfigId, CohortId, EvidenceBatchId, ProposalId, Revision, TerminalRecord};
pub use ledger::{Ledger, LedgerError, LedgerRecord, MemLedger};
pub use relations::{
    CellBytes, CellProjection, CutRow, EntryCommitRow, EntryKey, EvidenceRows, LineageRow,
    MaterializedViews, ObsKey, PointRow, ReduceOp, ReducedRow, RolloutKey, SealKey, SealRow,
    StateEventRow, canonical_cell,
};

// Test/golden apparatus (hm-fb0): the durable-state projection vehicle and
// `MemLedger`'s simulated crash + fault injection. Gated behind
// `test-support` so `hm-bbx.4` importing this crate without the feature never
// freezes them as compat surface. The `--all-features` public-api snapshot
// still freezes the full surface.
#[cfg(any(test, feature = "test-support"))]
pub use coordinator::StateProjection;
#[cfg(any(test, feature = "test-support"))]
pub use ledger::MemFault;
