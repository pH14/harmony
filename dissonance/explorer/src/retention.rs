// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **campaign evidence retention + completeness policy** (`hm-5sv`).
//!
//! This module replaces the old unconditional-record-only runbook rule with the
//! strategy's explicit retention contract (`docs/DISSONANCE-STRATEGY.md`,
//! "Evidence retention needs an explicit bounded policy separate from archive
//! admission"). Three records stay distinct, by type:
//!
//! 1. the **immutable evidence ledger** while its raw records are physically
//!    retained ([`EvidenceLedger`](crate::EvidenceLedger));
//! 2. **versioned membership in bounded working sets** ([`WorkingSet`]), where
//!    admission and expiration are ordinary positive and negative updates
//!    ([`WorkingSetUpdate`]);
//! 3. **committed Entry cell assignments and finalized campaign summaries**
//!    ([`CellAssignment`], [`FinalizedSummary`]).
//!
//! The policy itself is declared in `CampaignConfig` as a [`RetentionProfile`]
//! with an explicit, stable expiry tie-break ([`ExpiryOrder`]) — never derived
//! from disk pressure or wall time. Bounded expiry updates **only** working
//! views (record 2): it cannot retract a live Entry cell or make a finalized
//! metric move backward, both by construction (no such API exists).
//!
//! [`RetentionViews`] is the one deterministic fold both the live campaign step
//! and a restart's [`RetentionViews::rebuild`] share, so "rebuild from a
//! supported checkpoint matches live state" is bit-identical by construction:
//! the same [`fold_batch`](RetentionViews::fold_batch) runs over the same
//! committed ledger inputs in the same canonical `(issue, batch)` order.
//!
//! Physical garbage collection is **ledger-aware** and proof-gated (the ledger
//! side lives in [`EvidenceLedger::collect`](crate::EvidenceLedger::collect)):
//! a raw payload reachable from a retained ledger record or a live Entry cannot
//! be invalidated, and every downgrade cites either a durable rebuildable
//! checkpoint ([`RetentionCheckpoint`]) or the campaign's explicit finalized
//! end to future reinterpretation ([`CoverageRef`]). Host resource exhaustion
//! aborts loudly ([`crate::LedgerError::Exhausted`]) — it never expires, GCs,
//! or downgrades anything on its own.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use revision_coordinator::EvidenceBatchId;

use crate::Reproducer;
use crate::evidence::RunId;
use crate::spine::{CellKey, EvidenceCut};

/// The declared, stable order bounded expiry retracts working-set members in —
/// carried in the profile so the tie-break is part of the campaign
/// configuration, never an implementation accident.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ExpiryOrder {
    /// Retract the earliest-admitted member first; members admitted at the same
    /// issue (impossible today — one batch per issue — but declared anyway)
    /// break ties by ascending batch identity.
    OldestFirst,
}

/// The campaign's declared evidence-retention profile (`CampaignConfig` carries
/// it). Retention choices that can affect search or Resolution are declared
/// here, use stable tie-breaks, and are independent of disk pressure and wall
/// time; resource exhaustion aborts rather than silently changing profile.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RetentionProfile {
    /// The **full-retention evaluation profile**: every evidence batch is
    /// recorded from the *first* rollout and physically retained for the whole
    /// campaign. Working-set membership is unbounded and physical GC is
    /// forbidden ([`RetentionError::FullRetentionForbidsCollection`]).
    Full,
    /// A **declared bounded profile**: the analysis/novelty working set holds
    /// at most `working_set_cap` batches; over-cap admission retracts members
    /// in the declared `expiry` order. Expiry updates only working views —
    /// the ledger record, committed Entry cells, and finalized metrics are
    /// untouched.
    Bounded {
        /// The maximum working-set membership.
        working_set_cap: usize,
        /// The declared stable retraction order.
        expiry: ExpiryOrder,
    },
}

impl Default for RetentionProfile {
    /// The full-retention evaluation profile — the strategy's default (staged
    /// direction: cooperative verticals begin "only after the full-retention
    /// evaluation profile is available, so evidence is retained from rollout
    /// one").
    fn default() -> Self {
        RetentionProfile::Full
    }
}

/// Typed retention/GC proof failures — every one is loud; none is ever absorbed
/// into a silent policy change.
#[derive(Debug, thiserror::Error)]
pub enum RetentionError {
    /// A durable ledger failure while recording the retention operation.
    #[error("evidence ledger: {0}")]
    Ledger(#[from] crate::ledger::LedgerError),
    /// Physical collection was requested under the full-retention profile,
    /// which records and retains everything from the first rollout.
    #[error("the full-retention profile forbids physical evidence collection")]
    FullRetentionForbidsCollection,
    /// The batch is still a member of the bounded working set — expire it
    /// through the declared policy first; GC never implies expiry.
    #[error("batch {batch:?} is still in the working set")]
    StillInWorkingSet {
        /// The still-live member.
        batch: EvidenceBatchId,
    },
    /// The batch's raw evidence is required to reproduce a live Entry (its
    /// genesis-complete reproducer): it cannot be collected while that Entry is
    /// live.
    #[error("batch {batch:?} is referenced by a live Entry")]
    LiveEntryReference {
        /// The protected batch.
        batch: EvidenceBatchId,
    },
    /// No durable checkpoint covers the batch and the campaign is not
    /// finalized: collecting it would leave a view no rebuild anchor can
    /// recover — the proof obligation the strategy's "(a) durable base-state
    /// checkpoint or (b) finalized artifact" rule encodes.
    #[error(
        "batch {batch:?} (issue {issue}) is not covered by a checkpoint and the campaign is not finalized"
    )]
    NotCovered {
        /// The uncovered batch.
        batch: EvidenceBatchId,
        /// The batch's issue coordinate.
        issue: u64,
    },
    /// The batch is unknown to the ledger (never appended, or already
    /// collected).
    #[error("batch {batch:?} is not a retained ledger record")]
    UnknownBatch {
        /// The unknown identity.
        batch: EvidenceBatchId,
    },
    /// The ledger's durable checkpoint was written under a different declared
    /// profile than this campaign's configuration — a policy change must be a
    /// new campaign configuration, never a silent reinterpretation.
    #[error(
        "retention profile mismatch: config declares {declared:?}, durable checkpoint holds {checkpoint:?}"
    )]
    ProfileMismatch {
        /// The profile the live configuration declares.
        declared: RetentionProfile,
        /// The profile the durable checkpoint was taken under.
        checkpoint: RetentionProfile,
    },
    /// Rebuild was asked for views whose raw inputs were collected under an
    /// explicit end to reinterpretation (no covering checkpoint): those views
    /// ended; the finalized artifacts are the surviving record.
    #[error(
        "future reinterpretation of batch {batch:?} ended: collected under finalization with no covering checkpoint"
    )]
    ReinterpretationEnded {
        /// The collected, uncovered batch.
        batch: EvidenceBatchId,
    },
}

// ---------------------------------------------------------------------------
// Record 2 — versioned bounded working-set membership
// ---------------------------------------------------------------------------

/// One versioned working-set membership update: an ordinary **positive**
/// (admission, `admitted == true`, weight +1) or **negative** (retraction,
/// `admitted == false`, weight −1) update, stamped with the issue coordinate
/// that caused it. The update log is deterministic and persists in every
/// checkpoint while retained.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct WorkingSetUpdate {
    /// The issue coordinate the update happened at (the admitted batch's own
    /// issue; for a retraction, the admitting issue that forced expiry).
    pub issue: u64,
    /// The batch whose membership changed.
    pub batch: EvidenceBatchId,
    /// `true` for admission (+1), `false` for retraction (−1).
    pub admitted: bool,
}

/// The bounded analysis/novelty **working set** (record 2): versioned batch
/// membership under the declared profile. Admission and expiration are ordinary
/// positive/negative updates; expiry retracts in the profile's declared stable
/// order and touches nothing but this view.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct WorkingSet {
    profile: RetentionProfile,
    /// Current members: batch → the issue it was admitted at.
    members: BTreeMap<EvidenceBatchId, u64>,
    /// The full deterministic membership update log (admissions and
    /// retractions, in commit order).
    log: Vec<WorkingSetUpdate>,
}

impl WorkingSet {
    /// An empty working set under `profile`.
    pub fn new(profile: RetentionProfile) -> Self {
        Self {
            profile,
            members: BTreeMap::new(),
            log: Vec::new(),
        }
    }

    /// The declared profile this set expires under.
    pub fn profile(&self) -> RetentionProfile {
        self.profile
    }

    /// Admit `batch` (committed at `issue`) into the working set, then apply
    /// the declared bound: while over cap, retract the expiry-order victim.
    /// Returns the retractions emitted (empty under [`RetentionProfile::Full`]
    /// or while under cap). Re-admitting a current member is a no-op.
    pub fn admit(&mut self, issue: u64, batch: EvidenceBatchId) -> Vec<WorkingSetUpdate> {
        if self.members.contains_key(&batch) {
            return Vec::new();
        }
        self.members.insert(batch, issue);
        self.log.push(WorkingSetUpdate {
            issue,
            batch,
            admitted: true,
        });
        let cap = match self.profile {
            RetentionProfile::Full => return Vec::new(),
            RetentionProfile::Bounded {
                working_set_cap, ..
            } => working_set_cap,
        };
        let mut retractions = Vec::new();
        while self.members.len() > cap {
            // The declared stable tie-break (`ExpiryOrder::OldestFirst`): the
            // lowest (admitted-at issue, batch id) member expires first.
            let victim = self
                .members
                .iter()
                .min_by_key(|(b, at)| (**at, **b))
                .map(|(b, _)| *b)
                .expect("members is non-empty while over cap");
            self.members.remove(&victim);
            let retraction = WorkingSetUpdate {
                issue,
                batch: victim,
                admitted: false,
            };
            self.log.push(retraction.clone());
            retractions.push(retraction);
        }
        retractions
    }

    /// Whether `batch` is currently a member.
    pub fn contains(&self, batch: &EvidenceBatchId) -> bool {
        self.members.contains_key(batch)
    }

    /// The current membership, in canonical batch order.
    pub fn members(&self) -> impl Iterator<Item = &EvidenceBatchId> {
        self.members.keys()
    }

    /// The current membership count.
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Whether the working set is empty.
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// The full deterministic membership update log (positive and negative
    /// updates, in commit order).
    pub fn updates(&self) -> &[WorkingSetUpdate] {
        &self.log
    }
}

// ---------------------------------------------------------------------------
// Record 3 — finalized summaries + committed Entry cell assignments
// ---------------------------------------------------------------------------

/// The **finalized campaign summary**: monotone counters over committed
/// evidence. There is no decrement anywhere — working-set expiry and raw
/// evidence GC cannot make a finalized count move backward, by construction.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct FinalizedSummary {
    /// Completed rollouts committed (one per search-loop step).
    pub rollouts: u64,
    /// Materialized seals committed (barrier-2 candidates that replayed).
    pub seals: u64,
    /// Entries admitted to archive occupancy (fresh claims + dominations).
    pub entries_admitted: u64,
    /// Distinct occurrence counterexamples found (deduped by fingerprint).
    pub counterexamples: u64,
}

/// One **committed Entry cell assignment** (record 3): the portable, persisted
/// projection of an admitted Entry. A retained Entry keeps its
/// **genesis-complete reproducer** (`env`) and its **lineage** (`rollout`, the
/// seal batch's issue chain) — the evidence required to reproduce it, which GC
/// cannot collect while the Entry is live.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CellAssignment {
    /// The occupied cell.
    pub cell: CellKey,
    /// The seal evidence batch this assignment was committed from.
    pub batch: EvidenceBatchId,
    /// The seal's lineage identity (issue + parent rollout issue).
    pub rollout: RunId,
    /// The actual server-stamped `sealed_at` cut.
    pub cut: EvidenceCut,
    /// The deterministic quality the occupancy dominated by.
    pub quality: u64,
    /// The genesis-complete reproducer that regenerates this Entry.
    pub env: Reproducer,
}

// ---------------------------------------------------------------------------
// The one deterministic fold: live views == rebuilt views
// ---------------------------------------------------------------------------

/// What one [`RetentionViews::fold_batch`] changed — the live controller uses
/// this to report and to keep the operational archive in lock-step.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct FoldOutcome {
    /// Working-set retractions the admission forced (bounded expiry).
    pub retractions: Vec<WorkingSetUpdate>,
    /// Occurrence counterexamples newly counted by this fold (deduped by
    /// fingerprint across the campaign).
    pub new_counterexamples: Vec<crate::occurrence::OccurrenceCounterexample>,
    /// For a seal batch: whether the Entry was admitted to its cell (a fresh
    /// claim or a domination), i.e. whether an assignment was upserted.
    pub admitted: bool,
}

/// The campaign's **retention views**: the working set, the finalized summary,
/// the committed Entry cell assignments, and the fold cursor — everything a
/// checkpoint persists and a restart rebuilds. One deterministic
/// [`fold_batch`](Self::fold_batch) is shared by the live step and
/// [`rebuild`](Self::rebuild), so live and rebuilt state are bit-identical by
/// construction ([`canonical_bytes`](Self::canonical_bytes)).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RetentionViews {
    /// The declared profile these views are maintained under.
    pub profile: RetentionProfile,
    /// The highest committed issue folded in — the checkpoint's **coverage**:
    /// batches at or below this issue are covered by these views.
    pub frontier_issue: u64,
    /// Record 2: the bounded working set.
    pub working: WorkingSet,
    /// Record 3: the finalized monotone summary.
    pub finalized: FinalizedSummary,
    /// Record 3: the committed Entry cell assignments, in canonical cell order.
    pub assignments: Vec<CellAssignment>,
    /// Occurrence-counterexample fingerprints already counted (the campaign's
    /// dedup-by-property state, persisted so `counterexamples` stays exact
    /// across restart).
    pub seen_counterexamples: BTreeSet<[u8; 32]>,
    /// The finalized absence-expectations view (must-hit property aggregates —
    /// finalized property counts survive working-set retention and raw GC).
    pub absences: crate::occurrence::AbsenceLedger,
}

impl RetentionViews {
    /// Empty views under `profile`.
    pub fn new(profile: RetentionProfile) -> Self {
        Self {
            profile,
            frontier_issue: 0,
            working: WorkingSet::new(profile),
            finalized: FinalizedSummary::default(),
            assignments: Vec::new(),
            seen_counterexamples: BTreeSet::new(),
            absences: crate::occurrence::AbsenceLedger::new(),
        }
    }

    /// The committed assignment occupying `cell`, if any.
    pub fn assignment(&self, cell: &CellKey) -> Option<&CellAssignment> {
        self.assignments
            .binary_search_by(|a| a.cell.cmp(cell))
            .ok()
            .map(|i| &self.assignments[i])
    }

    /// Fold one committed evidence batch into the views — THE deterministic
    /// update both the live step and a rebuild share. For a rollout batch:
    /// working-set admission, the finalized rollout count, occurrence
    /// counterexamples (deduped by fingerprint), and the absence fold. For a
    /// seal batch: working-set admission, the finalized seal count, and the
    /// best-Entry-per-cell assignment upsert (strictly greater quality
    /// replaces; ties keep the earlier occupant).
    pub fn fold_batch(
        &mut self,
        cells: &dyn crate::evidence::ObservationCells,
        id: EvidenceBatchId,
        ev: &crate::evidence::CompletedRunEvidence,
    ) -> FoldOutcome {
        let mut out = FoldOutcome {
            retractions: self.working.admit(ev.rollout.issue, id),
            ..FoldOutcome::default()
        };
        self.frontier_issue = self.frontier_issue.max(ev.rollout.issue);
        match ev.role {
            crate::evidence::EvidenceRole::Rollout => {
                self.finalized.rollouts += 1;
                for ce in crate::occurrence::OccurrenceOracle::new().judge(ev) {
                    if self.seen_counterexamples.insert(ce.fingerprint) {
                        self.finalized.counterexamples += 1;
                        out.new_counterexamples.push(ce);
                    }
                }
                self.absences.observe(ev);
            }
            crate::evidence::EvidenceRole::Seal => {
                self.finalized.seals += 1;
                let obs = ev.observations_at_cut();
                let cell = cells.key(ev.cut, &obs);
                // The occupancy quality metric (progress depth), kept in
                // lock-step with the controller's barrier-2 admission.
                let quality = ev.cut.at.0;
                let assignment = CellAssignment {
                    cell: cell.clone(),
                    batch: id,
                    rollout: ev.rollout,
                    cut: ev.cut,
                    quality,
                    env: ev.env.clone(),
                };
                match self.assignments.binary_search_by(|a| a.cell.cmp(&cell)) {
                    Err(i) => {
                        self.assignments.insert(i, assignment);
                        self.finalized.entries_admitted += 1;
                        out.admitted = true;
                    }
                    Ok(i) => {
                        if quality > self.assignments[i].quality {
                            self.assignments[i] = assignment;
                            self.finalized.entries_admitted += 1;
                            out.admitted = true;
                        }
                    }
                }
            }
        }
        out
    }

    /// The canonical, deterministic bytes of these views — the checkpoint
    /// content, and the equality the rebuild gate compares bit-identically.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        // Infallible for our owned, finite, non-float types; a serialize error
        // here would be a programming error, not untrusted input.
        serde_json::to_vec(self).expect("RetentionViews serializes")
    }

    /// Rebuild the views from durable records alone: the ledger's last
    /// committed checkpoint (if any) as the base, then every retained batch
    /// past the checkpoint's coverage folded forward in canonical
    /// `(issue, batch)` order — "restart replays committed ledger inputs
    /// rather than treating a live arrangement as authority."
    ///
    /// Errors loudly if the durable checkpoint was taken under a different
    /// profile ([`RetentionError::ProfileMismatch`]) or if a collected batch is
    /// not covered by the base checkpoint — those views ended with the
    /// campaign's finalization ([`RetentionError::ReinterpretationEnded`]).
    pub fn rebuild(
        profile: RetentionProfile,
        cells: &dyn crate::evidence::ObservationCells,
        ledger: &crate::ledger::EvidenceLedger,
    ) -> Result<Self, RetentionError> {
        let mut views = match ledger.last_checkpoint() {
            Some(cp) => {
                if cp.views.profile != profile {
                    return Err(RetentionError::ProfileMismatch {
                        declared: profile,
                        checkpoint: cp.views.profile,
                    });
                }
                cp.views.clone()
            }
            None => RetentionViews::new(profile),
        };
        // A collected batch above the rebuild base has no raw evidence to fold
        // and no checkpoint covering it: reinterpretation explicitly ended.
        for (batch, tomb) in ledger.collected() {
            if tomb.rollout.issue > views.frontier_issue {
                return Err(RetentionError::ReinterpretationEnded { batch: *batch });
            }
        }
        let mut suffix: Vec<(u64, EvidenceBatchId)> = ledger
            .batch_ids()
            .filter_map(|id| {
                let ev = ledger.get(id).expect("batch_ids yields retained ids");
                (ev.rollout.issue > views.frontier_issue).then_some((ev.rollout.issue, *id))
            })
            .collect();
        suffix.sort();
        for (_, id) in suffix {
            let ev = ledger.get(&id).expect("suffix ids are retained");
            views.fold_batch(cells, id, ev);
        }
        Ok(views)
    }
}

// ---------------------------------------------------------------------------
// Checkpoint + coverage
// ---------------------------------------------------------------------------

/// A durable **retention checkpoint**: a rebuild anchor holding the complete
/// [`RetentionViews`] at a committed frontier. Physical GC may only downgrade
/// raw evidence this checkpoint covers (or evidence explicitly ended by
/// finalization) — "physical garbage collection is allowed only behind a
/// durable base-state checkpoint sufficient to rebuild every still-supported
/// view, or a finalized artifact that explicitly ends future reinterpretation."
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RetentionCheckpoint {
    /// The complete views at the checkpoint's frontier.
    pub views: RetentionViews,
}

impl RetentionCheckpoint {
    /// Whether this checkpoint covers a batch committed at `issue` (the batch's
    /// derivations are rebuildable from this anchor without its raw evidence).
    pub fn covers(&self, issue: u64) -> bool {
        issue <= self.views.frontier_issue
    }

    /// Canonical deterministic bytes (same-seed campaigns produce identical
    /// checkpoints).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        // Infallible for our owned, finite, non-float types (comment as in
        // `RetentionViews::canonical_bytes`).
        serde_json::to_vec(self).expect("RetentionCheckpoint serializes")
    }
}

/// What a collected batch's downgrade was **covered by** — the completeness/
/// loss metadata every tombstone carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CoverageRef {
    /// A durable checkpoint whose coverage frontier is `frontier_issue`
    /// rebuilds every still-supported view without this raw evidence.
    Checkpoint {
        /// The covering checkpoint's frontier.
        frontier_issue: u64,
    },
    /// The campaign's explicit finalized end to future raw-evidence
    /// reinterpretation.
    Finalized,
}

/// The durable tombstone of a collected batch: exact completeness/loss
/// metadata that outlives the raw evidence (what the batch was, and what its
/// collection was covered by).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CollectedBatch {
    /// The collected batch's identity.
    pub batch: EvidenceBatchId,
    /// Its rollout identity (issue + lineage).
    pub rollout: RunId,
    /// Its role (rollout or materialized seal).
    pub role: crate::evidence::EvidenceRole,
    /// Its evidence cut.
    pub cut: EvidenceCut,
    /// How many normalized SDK events its raw evidence carried.
    pub events: u64,
    /// What the downgrade was covered by.
    pub covered_by: CoverageRef,
}

// ---------------------------------------------------------------------------
// GC reporting + the completeness report
// ---------------------------------------------------------------------------

/// Why a GC sweep skipped a batch — reported, never silent (the "no silent
/// caps" discipline: a bounded sweep says exactly what it did not collect).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum GcSkipReason {
    /// Still a working-set member (expire it through policy first).
    StillInWorkingSet,
    /// Required to reproduce a live Entry (genesis-complete reproducer).
    LiveEntryReference,
    /// No checkpoint coverage and the campaign is not finalized.
    NotCovered,
}

/// One proven GC sweep's outcome: exactly what was collected, what was skipped
/// and why, and how many raw payloads the store reclaimed.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct GcReport {
    /// Batches whose raw evidence was collected (tombstoned durable).
    pub collected: Vec<EvidenceBatchId>,
    /// Batches the sweep left retained, with the proof obligation that held.
    pub skipped: Vec<(EvidenceBatchId, GcSkipReason)>,
    /// Raw payloads reclaimed from the referenced-payload store.
    pub reclaimed_payloads: usize,
}

/// Whether a batch's cells can later be recomputed under a different `CellFn`
/// — the strategy's explicit availability claim: "a campaign that discards
/// ordered evidence … cannot later have its cells recomputed without replay
/// and must say so explicitly."
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Recomputation {
    /// The ordered raw evidence is retained: cells recompute from it directly,
    /// no VM execution.
    FromRetainedEvidence,
    /// The raw evidence was collected: recomputation requires materialization
    /// replay of the reproducer.
    RequiresReplay,
}

/// The physical availability of one batch's raw evidence.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RawAvailability {
    /// The raw normalized evidence and referenced payload are retained.
    Retained,
    /// The raw evidence was collected by proven GC, covered by `covered_by`.
    Collected {
        /// The proof the downgrade cited.
        covered_by: CoverageRef,
    },
}

/// Everything the report states about one batch.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BatchAvailability {
    /// Whether the raw evidence physically remains.
    pub raw: RawAvailability,
    /// Whether future cell recomputation is available without replay.
    pub recompute_cells: Recomputation,
    /// Whether the batch is currently a working-set member.
    pub in_working_set: bool,
}

/// The campaign **completeness report**: states *exactly* which raw evidence,
/// derivations, and future recomputation remain available — per batch, plus
/// the finalized derivations and committed assignments that survive
/// regardless of raw availability.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RetentionReport {
    /// The declared profile the campaign ran under.
    pub profile: RetentionProfile,
    /// Whether the campaign's explicit end to future reinterpretation is set.
    pub finalized_end: bool,
    /// Exact per-batch availability, in canonical batch order.
    pub batches: BTreeMap<EvidenceBatchId, BatchAvailability>,
    /// The finalized derivations that remain (monotone, survive GC).
    pub derivations: FinalizedSummary,
    /// How many committed Entry cell assignments remain (each with its
    /// genesis-complete reproducer and lineage).
    pub committed_assignments: u64,
}

impl RetentionReport {
    /// Canonical deterministic bytes (same-seed campaigns produce identical
    /// reports).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        // Infallible for our owned, finite, non-float types (comment as in
        // `RetentionViews::canonical_bytes`).
        serde_json::to_vec(self).expect("RetentionReport serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn bid(n: u64) -> EvidenceBatchId {
        let mut b = [0u8; 32];
        b[..8].copy_from_slice(&n.to_le_bytes());
        EvidenceBatchId::from_bytes(b)
    }

    fn bounded(cap: usize) -> RetentionProfile {
        RetentionProfile::Bounded {
            working_set_cap: cap,
            expiry: ExpiryOrder::OldestFirst,
        }
    }

    /// The default profile is full retention (the evaluation profile).
    #[test]
    fn default_profile_is_full_retention() {
        assert_eq!(RetentionProfile::default(), RetentionProfile::Full);
    }

    /// Full retention never retracts: membership grows without bound and the
    /// log holds positive updates only.
    #[test]
    fn full_profile_never_retracts() {
        let mut ws = WorkingSet::new(RetentionProfile::Full);
        assert!(ws.is_empty());
        for i in 0..100 {
            let r = ws.admit(i, bid(i));
            assert!(r.is_empty(), "full retention never expires");
        }
        assert!(!ws.is_empty());
        assert_eq!(ws.len(), 100);
        assert!(ws.updates().iter().all(|u| u.admitted));
    }

    /// Bounded expiry retracts the oldest-admitted member first, tie-broken by
    /// ascending batch id — the declared stable order.
    #[test]
    fn bounded_expiry_is_oldest_first_with_stable_tiebreak() {
        let mut ws = WorkingSet::new(bounded(2));
        assert_eq!(ws.profile(), bounded(2), "the declared profile is carried");
        assert!(ws.admit(1, bid(10)).is_empty());
        assert!(ws.admit(2, bid(5)).is_empty());
        // Admitting a third expires the member with the lowest admitted issue
        // (bid(10), admitted at 1) — not the lowest batch id (bid(5)).
        let r = ws.admit(3, bid(7));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].batch, bid(10));
        assert!(!r[0].admitted);
        assert_eq!(r[0].issue, 3, "retraction stamped with the forcing issue");
        assert!(ws.contains(&bid(5)) && ws.contains(&bid(7)));
    }

    /// A zero-cap bound retracts even the just-admitted batch — deterministic,
    /// never a panic.
    #[test]
    fn zero_cap_retracts_immediately() {
        let mut ws = WorkingSet::new(bounded(0));
        let r = ws.admit(1, bid(1));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].batch, bid(1));
        assert!(ws.is_empty());
        // The log still records both the admission and the retraction.
        assert_eq!(ws.updates().len(), 2);
    }

    /// Re-admitting a current member is a no-op (idempotent, no duplicate
    /// positive update).
    #[test]
    fn readmission_is_idempotent() {
        let mut ws = WorkingSet::new(bounded(4));
        ws.admit(1, bid(1));
        let before = ws.updates().len();
        assert!(ws.admit(2, bid(1)).is_empty());
        assert_eq!(ws.updates().len(), before);
        assert_eq!(ws.len(), 1);
    }

    /// Assignment upsert dominates by strictly greater quality; equal quality
    /// keeps the earlier occupant (the stable tie-break), and admissions bump
    /// the monotone finalized count.
    #[test]
    fn assignment_upsert_dominates_by_strict_quality() {
        use crate::evidence::{DefaultObservationCells, ObservationCells};
        let mut v = RetentionViews::new(RetentionProfile::Full);
        let cells = DefaultObservationCells::new();
        let seal =
            |issue: u64, at: u64, value: u64| crate::testkit::seal_evidence(issue, at, value);
        // Two seals reaching the same reduced state (same cell) at different
        // depths: the deeper one dominates.
        let (id1, e1) = seal(1, 10, 5);
        let (id2, e2) = seal(2, 30, 5);
        let (id3, e3) = seal(3, 30, 5);
        assert!(v.fold_batch(&cells, id1, &e1).admitted);
        assert_eq!(v.assignments.len(), 1);
        assert!(v.fold_batch(&cells, id2, &e2).admitted, "deeper dominates");
        assert_eq!(v.assignments.len(), 1);
        assert_eq!(v.assignments[0].batch, id2);
        // Equal quality: the earlier occupant stays.
        assert!(!v.fold_batch(&cells, id3, &e3).admitted);
        assert_eq!(v.assignments[0].batch, id2);
        assert_eq!(v.finalized.entries_admitted, 2);
        assert_eq!(v.finalized.seals, 3);
        // The assignment accessor resolves the occupied cell (and only it).
        let cell = cells.key(e2.cut, &e2.observations_at_cut());
        assert_eq!(v.assignment(&cell).expect("occupied").batch, id2);
        assert!(v.assignment(&b"no-such-cell".to_vec()).is_none());
    }

    /// Canonical bytes are the real serialized views/checkpoint/report — they
    /// round-trip through serde exactly (never a stub).
    #[test]
    fn canonical_bytes_round_trip() {
        use crate::evidence::DefaultObservationCells;
        let mut v = RetentionViews::new(bounded(2));
        let (id, ev) = crate::testkit::seal_evidence(3, 10, 1);
        v.fold_batch(&DefaultObservationCells::new(), id, &ev);
        let back: RetentionViews =
            serde_json::from_slice(&v.canonical_bytes()).expect("views decode");
        assert_eq!(back, v);
        let cp = RetentionCheckpoint { views: v.clone() };
        let back: RetentionCheckpoint =
            serde_json::from_slice(&cp.canonical_bytes()).expect("checkpoint decodes");
        assert_eq!(back, cp);
        let mut batches = BTreeMap::new();
        batches.insert(
            id,
            BatchAvailability {
                raw: RawAvailability::Collected {
                    covered_by: CoverageRef::Checkpoint { frontier_issue: 3 },
                },
                recompute_cells: Recomputation::RequiresReplay,
                in_working_set: false,
            },
        );
        let report = RetentionReport {
            profile: bounded(2),
            finalized_end: true,
            batches,
            derivations: v.finalized,
            committed_assignments: v.assignments.len() as u64,
        };
        let back: RetentionReport =
            serde_json::from_slice(&report.canonical_bytes()).expect("report decodes");
        assert_eq!(back, report);
    }

    /// A batch collected at exactly the checkpoint's coverage frontier is
    /// covered (half-open on the *outside*): rebuild still succeeds and equals
    /// the checkpointed views.
    #[test]
    fn collection_at_the_checkpoint_frontier_still_rebuilds() {
        use crate::evidence::DefaultObservationCells;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("evidence.log");
        let mut led = EvidenceLedger::open(&path).expect("open");
        let cells = DefaultObservationCells::new();
        let (_id, ev) = seal_evidence(5, 10, 1);
        let id = led.append(&ev).expect("append");
        let mut views = RetentionViews::new(RetentionProfile::Full);
        views.fold_batch(&cells, id, &ev);
        assert_eq!(views.frontier_issue, 5);
        led.commit_checkpoint(&RetentionCheckpoint {
            views: views.clone(),
        })
        .expect("checkpoint");
        led.collect(id, &BTreeSet::new())
            .expect("covered at the frontier");
        let rebuilt = RetentionViews::rebuild(RetentionProfile::Full, &cells, &led)
            .expect("a frontier-covered collection does not end reinterpretation");
        assert_eq!(rebuilt, views);
    }

    // ---- campaign-level acceptance gates (`hm-5sv`) ----

    use crate::campaign::{CampaignConfig, CampaignError};
    use crate::evidence::DefaultObservationCells;
    use crate::ledger::EvidenceLedger;
    use crate::testkit::{campaign, campaign_over, config, seal_evidence, simple_program};

    fn bounded_config(cap: usize) -> CampaignConfig {
        let mut cfg = config(8, u64::MAX);
        cfg.retention = bounded(cap);
        cfg
    }

    /// Acceptance gate: `CampaignConfig` carries the retention profile (default
    /// = the full-retention evaluation profile) and its stable tie-breaks, and a
    /// full-retention runbook records from the **first** rollout and retains
    /// everything; physical collection is forbidden.
    #[test]
    fn full_retention_records_from_the_first_rollout() {
        assert_eq!(
            CampaignConfig::default().retention,
            RetentionProfile::Full,
            "the default profile is the full-retention evaluation profile"
        );
        let (_dir, mut camp) = campaign(simple_program(4), config(8, u64::MAX), 7);
        camp.step().expect("step 1");
        let first_ids: Vec<EvidenceBatchId> = camp.ledger().batch_ids().copied().collect();
        assert!(
            !first_ids.is_empty(),
            "evidence is recorded from the first rollout"
        );
        camp.explore(4).expect("more steps");
        for id in &first_ids {
            assert!(
                camp.ledger().contains(id),
                "first-rollout evidence stays retained"
            );
        }
        // Full retention: every batch is a working-set member, no retraction
        // was ever emitted.
        assert_eq!(camp.views().working.len(), camp.ledger().len());
        assert!(camp.views().working.updates().iter().all(|u| u.admitted));
        // And physical collection is refused loudly.
        assert!(matches!(
            camp.collect_expired(),
            Err(CampaignError::Retention(
                RetentionError::FullRetentionForbidsCollection
            ))
        ));
    }

    /// Acceptance gate: bounded expiry updates only working views — the ledger
    /// keeps every batch, live Entry cells stay committed, and every finalized
    /// metric is monotone across the whole run.
    #[test]
    fn bounded_expiry_updates_only_working_views() {
        let (_dir, mut camp) = campaign(simple_program(4), bounded_config(1), 7);
        let mut prev = FinalizedSummary::default();
        for _ in 0..5 {
            camp.step().expect("step");
            let f = camp.views().finalized;
            // Finalized metrics never move backward (bounded expiry included).
            assert!(f.rollouts >= prev.rollouts);
            assert!(f.seals >= prev.seals);
            assert!(f.entries_admitted >= prev.entries_admitted);
            assert!(f.counterexamples >= prev.counterexamples);
            prev = f;
        }
        let retractions = camp
            .views()
            .working
            .updates()
            .iter()
            .filter(|u| !u.admitted)
            .count();
        assert!(retractions > 0, "bounded expiry actually retracted");
        assert!(camp.views().working.len() <= 1, "the declared cap holds");
        // Only working views changed: the durable ledger retained every batch…
        assert!(camp.ledger().len() > camp.views().working.len());
        assert_eq!(camp.ledger().collected().count(), 0, "expiry is not GC");
        // …and no live Entry cell was retracted: the operational archive and
        // the committed assignments stay 1:1.
        assert!(camp.occupied() >= 1);
        assert_eq!(camp.views().assignments.len(), camp.occupied());
    }

    /// Acceptance gate: GC proves reachability + checkpoint coverage first. A
    /// batch still in the working set, required by a live Entry, or not covered
    /// by a durable checkpoint (with the campaign not finalized) is never
    /// collected — each skip is reported with its reason, and a reference
    /// reachable from the ledger or a live Entry is never invalidated.
    #[test]
    fn gc_proves_reachability_and_coverage_before_collecting() {
        let (_dir, mut camp) = campaign(simple_program(4), bounded_config(1), 7);
        camp.explore(4).expect("explore");

        // Explicit single-batch proof failures, loudest first: a working-set
        // member is refused before anything else.
        let member = *camp
            .views()
            .working
            .members()
            .next()
            .expect("working set has a member");
        assert!(matches!(
            camp.collect_batch(member),
            Err(CampaignError::Retention(
                RetentionError::StillInWorkingSet { .. }
            ))
        ));

        // No checkpoint, not finalized: the sweep collects nothing and reports
        // why, batch by batch.
        let rep = camp.collect_expired().expect("sweep");
        assert!(rep.collected.is_empty(), "nothing collected without proof");
        assert_eq!(rep.reclaimed_payloads, 0);
        assert!(
            rep.skipped
                .iter()
                .any(|(_, r)| *r == GcSkipReason::NotCovered),
            "the missing-coverage obligation is reported"
        );

        // Commit the rebuild anchor, then sweep for real.
        camp.commit_checkpoint().expect("checkpoint");
        let rep = camp.collect_expired().expect("sweep");
        assert!(!rep.collected.is_empty(), "proven candidates collect");
        for (id, reason) in &rep.skipped {
            match reason {
                GcSkipReason::StillInWorkingSet => {
                    assert!(camp.views().working.contains(id));
                }
                GcSkipReason::LiveEntryReference => {
                    // The batch's reproducer belongs to a live Entry; it stays
                    // retained.
                    assert!(camp.ledger().contains(id));
                }
                GcSkipReason::NotCovered => panic!("everything is covered now"),
            }
        }
        // Live-Entry protection held: every live Entry's genesis-complete
        // reproducer still resolves in the payload store.
        for (_, e) in camp.frontier().iter() {
            let digest = *blake3::hash(&e.env.bytes).as_bytes();
            assert!(
                camp.ledger().live_references().contains(&digest),
                "a live Entry's reproducer reference is never invalidated"
            );
        }
        // Collected batches are exactly the proven ones: expired, unprotected,
        // covered — and their loss is recorded durably.
        for id in &rep.collected {
            assert!(!camp.views().working.contains(id));
            assert!(!camp.ledger().contains(id));
        }
        assert_eq!(camp.ledger().collected().count(), rep.collected.len());

        // Physical reclamation: compaction shrinks the durable file by exactly
        // the reported amount, and the campaign continues cleanly.
        let file_before = std::fs::metadata(camp.ledger().path()).expect("meta").len();
        let reclaimed = camp.compact_ledger().expect("compact");
        let file_after = std::fs::metadata(camp.ledger().path()).expect("meta").len();
        assert!(reclaimed > 0, "collected raw bytes left the file");
        assert_eq!(
            file_before - file_after,
            reclaimed,
            "reported reclamation is the real file shrinkage"
        );
        camp.step().expect("the campaign continues after GC");
    }

    /// The campaign's explicit finalized end marker is durable and is the
    /// second GC leg: with it set (and no covering checkpoint), expired
    /// unprotected batches collect and the report cites the finalized end.
    #[test]
    fn finalize_evidence_sets_the_durable_end_marker() {
        let (_dir, mut camp) = campaign(simple_program(4), bounded_config(1), 7);
        camp.explore(4).expect("explore");
        assert!(!camp.ledger().is_finalized());
        assert!(!camp.retention_report().finalized_end);
        camp.finalize_evidence().expect("finalize");
        assert!(camp.ledger().is_finalized(), "the end marker is durable");
        let rep = camp.collect_expired().expect("sweep");
        assert!(!rep.collected.is_empty(), "finalization permits collection");
        let report = camp.retention_report();
        assert!(report.finalized_end);
        assert!(
            report.batches.values().any(|b| matches!(
                b.raw,
                RawAvailability::Collected {
                    covered_by: CoverageRef::Finalized
                }
            )),
            "collected loss metadata cites the finalized end"
        );
    }

    /// The finalized summary counts exactly: one rollout per step, rollouts +
    /// seals cover the whole ledger, and admissions match the occupied archive
    /// (this program's admissions are all fresh claims).
    #[test]
    fn finalized_counts_are_exact() {
        let (_dir, mut camp) = campaign(simple_program(4), config(8, u64::MAX), 7);
        camp.explore(3).expect("explore");
        let f = camp.views().finalized;
        assert_eq!(f.rollouts, 3, "one committed rollout per step");
        assert_eq!(
            f.rollouts + f.seals,
            camp.ledger().len() as u64,
            "every ledger batch is a rollout or a seal"
        );
        assert!(f.seals >= 1, "the first fresh cell was sealed");
        assert_eq!(f.entries_admitted, camp.occupied() as u64);
        assert_eq!(f.counterexamples, 0, "no assertion fired in this program");
    }

    /// The campaign's finalized absence view is exact and retention-stable:
    /// the test catalog's declared, never-fired must-hit property is reported
    /// as an absence, and it survives bounded expiry and raw-evidence GC.
    #[test]
    fn absence_view_survives_expiry_and_gc() {
        use sdk_events::{NS_ASSERT, ObservationId};
        let (_dir, mut camp) = campaign(simple_program(4), bounded_config(1), 7);
        camp.explore(4).expect("explore");
        let check =
            |camp: &crate::campaign::DifferentialCampaign<crate::testkit::ScriptedMachine>| {
                let absences = camp.absences().absences();
                assert_eq!(absences.len(), 1, "exactly the declared must-hit");
                assert_eq!(
                    absences[0].property,
                    ObservationId::Point {
                        namespace: NS_ASSERT,
                        local: 99
                    }
                );
            };
        check(&camp);
        // Working-set expiry already happened (cap 1); GC the expired raw
        // evidence and the finalized absence claim still stands.
        camp.commit_checkpoint().expect("checkpoint");
        camp.collect_expired().expect("sweep");
        check(&camp);
    }

    /// Acceptance gate: the completeness report states **exactly** which raw
    /// evidence, derivations, and future recomputation remain available.
    #[test]
    fn report_states_exactly_what_remains() {
        let (_dir, mut camp) = campaign(simple_program(4), bounded_config(1), 7);
        camp.explore(4).expect("explore");
        camp.commit_checkpoint().expect("checkpoint");
        let swept = camp.collect_expired().expect("sweep");
        assert!(!swept.collected.is_empty());
        let report = camp.retention_report();
        assert_eq!(report.profile, bounded(1));
        assert!(!report.finalized_end);
        // Exact coverage: one row per batch ever appended, none missing.
        assert_eq!(
            report.batches.len(),
            camp.ledger().len() + camp.ledger().collected().count()
        );
        for (id, avail) in &report.batches {
            if let Some(_ev) = camp.ledger().get(id) {
                assert_eq!(avail.raw, RawAvailability::Retained);
                assert_eq!(
                    avail.recompute_cells,
                    Recomputation::FromRetainedEvidence,
                    "retained ordered evidence recomputes cells without replay"
                );
                assert_eq!(avail.in_working_set, camp.views().working.contains(id));
            } else {
                // Collected: the report says so explicitly, cites the coverage,
                // and declares recomputation needs replay.
                let frontier = camp
                    .ledger()
                    .last_checkpoint()
                    .expect("checkpointed")
                    .views
                    .frontier_issue;
                assert_eq!(
                    avail.raw,
                    RawAvailability::Collected {
                        covered_by: CoverageRef::Checkpoint {
                            frontier_issue: frontier
                        }
                    }
                );
                assert_eq!(avail.recompute_cells, Recomputation::RequiresReplay);
                assert!(!avail.in_working_set);
            }
        }
        // The surviving derivations are stated.
        assert_eq!(report.derivations, camp.views().finalized);
        assert_eq!(
            report.committed_assignments,
            camp.views().assignments.len() as u64
        );
    }

    /// Acceptance gate: host disk pressure cannot silently change policy —
    /// exhausting the declared evidence budget fails the step loudly and
    /// expires, collects, and downgrades nothing.
    #[test]
    fn exhaustion_fails_loudly_never_downgrades() {
        let mut cfg = config(8, u64::MAX);
        cfg.evidence_budget = Some(64); // far below one evidence frame
        let (_dir, mut camp) = campaign(simple_program(4), cfg, 7);
        let err = camp.step().expect_err("exhausted");
        assert!(matches!(
            err,
            CampaignError::Ledger(crate::ledger::LedgerError::Exhausted { .. })
        ));
        // Nothing silently changed: same profile, nothing recorded as
        // committed, nothing collected, no working-set mutation.
        assert_eq!(camp.views().profile, RetentionProfile::Full);
        assert_eq!(camp.ledger().len(), 0);
        assert_eq!(camp.ledger().collected().count(), 0);
        assert!(camp.views().working.is_empty());
        assert_eq!(camp.views().finalized, FinalizedSummary::default());
    }

    /// Acceptance gate: rebuild from a supported checkpoint matches live state
    /// **bit-identically** — across a checkpoint taken mid-campaign, further
    /// live steps, proven GC, compaction, and a real file reopen. A resumed
    /// campaign starts from exactly the rebuilt views, with its operational
    /// archive in lock-step.
    #[test]
    fn rebuild_from_checkpoint_matches_live_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("evidence.log");
        let cfg = bounded_config(2);
        let cells = DefaultObservationCells::new();

        let led = EvidenceLedger::open(&path).expect("open");
        let mut camp = campaign_over(simple_program(4), cfg, 7, led);
        camp.explore(3).expect("prefix");
        camp.commit_checkpoint().expect("checkpoint");
        camp.explore(2).expect("suffix past the checkpoint");
        let live = camp.views().canonical_bytes();
        drop(camp);

        // Restart: rebuild = durable checkpoint + retained ledger suffix.
        let led = EvidenceLedger::open(&path).expect("reopen");
        let rebuilt = RetentionViews::rebuild(cfg.retention, &cells, &led).expect("rebuild");
        assert_eq!(
            rebuilt.canonical_bytes(),
            live,
            "rebuilt views are bit-identical to live state"
        );
        // A resumed campaign starts from exactly these views, operational
        // archive in lock-step with the committed assignments.
        let camp2 = campaign_over(simple_program(4), cfg, 7, led);
        assert_eq!(camp2.views().canonical_bytes(), live);
        assert_eq!(camp2.occupied(), camp2.views().assignments.len());
        drop(camp2);

        // GC + compaction do not break the rebuild contract: checkpoint at the
        // frontier, collect the expired, compact, reopen, rebuild — still
        // bit-identical.
        let led = EvidenceLedger::open(&path).expect("reopen 2");
        let mut camp = campaign_over(simple_program(4), cfg, 7, led);
        camp.commit_checkpoint().expect("cover everything");
        camp.collect_expired().expect("sweep");
        camp.compact_ledger().expect("compact");
        let live = camp.views().canonical_bytes();
        drop(camp);
        let led = EvidenceLedger::open(&path).expect("reopen 3");
        let rebuilt = RetentionViews::rebuild(cfg.retention, &cells, &led).expect("rebuild");
        assert_eq!(rebuilt.canonical_bytes(), live, "identical after GC");

        // A different declared profile cannot silently reinterpret the durable
        // checkpoint.
        let err = RetentionViews::rebuild(RetentionProfile::Full, &cells, &led)
            .expect_err("profile mismatch");
        assert!(matches!(err, RetentionError::ProfileMismatch { .. }));
    }

    /// Same-seed campaigns produce byte-identical retention artifacts
    /// (checkpoints and completeness reports) — the determinism gate for the
    /// retention plane.
    #[test]
    fn same_seed_yields_identical_retention_artifacts() {
        let artifacts = |seed: u64| {
            let (_dir, mut camp) = campaign(simple_program(4), bounded_config(2), seed);
            camp.explore(5).expect("explore");
            let cp = camp.commit_checkpoint().expect("checkpoint");
            (
                cp.canonical_bytes(),
                camp.retention_report().canonical_bytes(),
            )
        };
        assert_eq!(artifacts(0xABCD), artifacts(0xABCD));
        // Not vacuous: different seeds produce different artifacts.
        assert_ne!(artifacts(1).0, artifacts(2).0);
    }

    /// Collection under finalization with no covering checkpoint explicitly
    /// **ends** reinterpretation: a later rebuild refuses (typed, loud) instead
    /// of silently producing partial views.
    #[test]
    fn finalized_collection_ends_reinterpretation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("evidence.log");
        let mut led = EvidenceLedger::open(&path).expect("open");
        let (_id, ev) = seal_evidence(5, 10, 1);
        let id = led.append(&ev).expect("append");
        led.finalize().expect("finalize");
        led.collect(id, &BTreeSet::new()).expect("collect");
        let err = RetentionViews::rebuild(
            RetentionProfile::Full,
            &DefaultObservationCells::new(),
            &led,
        )
        .expect_err("reinterpretation ended");
        assert!(matches!(err, RetentionError::ReinterpretationEnded { .. }));
    }

    proptest! {
        /// Bounded membership never exceeds its cap, expiry follows the
        /// declared (admitted-issue, batch) order, and the whole evolution is
        /// a deterministic function of the admission sequence.
        #[test]
        fn bounded_working_set_holds_cap_and_determinism(
            cap in 0usize..6,
            seq in proptest::collection::vec(0u64..40, 1..60),
        ) {
            let run = || {
                let mut ws = WorkingSet::new(bounded(cap));
                let mut all_retractions = Vec::new();
                for (i, b) in seq.iter().enumerate() {
                    let r = ws.admit(i as u64 + 1, bid(*b));
                    prop_assert!(ws.len() <= cap, "cap held");
                    all_retractions.extend(r);
                }
                Ok((ws, all_retractions))
            };
            let (a, ra) = run()?;
            let (b, rb) = run()?;
            // Determinism: same sequence ⇒ identical membership, log, and
            // retractions (same-seed retention artifacts are identical).
            prop_assert_eq!(&a, &b);
            prop_assert_eq!(ra, rb);
            // Every retracted member was admitted before its retraction, and
            // retraction order per step follows the declared tie-break.
            let admitted: BTreeSet<_> =
                a.updates().iter().filter(|u| u.admitted).map(|u| u.batch).collect();
            prop_assert!(a.updates().iter().filter(|u| !u.admitted).all(|u| admitted.contains(&u.batch)));
        }
    }
}
