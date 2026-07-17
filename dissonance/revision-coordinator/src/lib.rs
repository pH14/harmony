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

mod coordinator;
mod file_ledger;
mod host;
mod ids;
mod ledger;

pub use coordinator::{
    Completion, CoordError, Coordinator, DrainedView, PendingProposal, StateProjection,
};
pub use file_ledger::FileLedger;
pub use ids::{CampaignConfigId, CohortId, EvidenceBatchId, ProposalId, Revision, TerminalRecord};
pub use ledger::{Ledger, LedgerError, LedgerRecord, MemFault, MemLedger};
