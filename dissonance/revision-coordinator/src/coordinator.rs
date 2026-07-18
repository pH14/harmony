// SPDX-License-Identifier: AGPL-3.0-or-later
//! The coordinator: persist-then-dispatch assignment, out-of-order
//! completion buffering, cohort freeze, probe-frontier drive, and crash
//! recovery. See the crate docs for the contract summary.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use crate::host::ProbeHost;
use crate::ids::{
    CampaignConfigId, CohortId, EvidenceBatchId, ProposalId, Revision, TerminalRecord,
};
use crate::ledger::{Ledger, LedgerError, LedgerRecord, MAX_ABORT_REASON};
use crate::relations::{
    CellProjection, EvidenceRows, MaterializedViews, ObsKey, ReduceOp, canonical_cell,
};

/// A proposal whose `Revision` assignment is durable: the caller may
/// dispatch it. A crashed worker retries the SAME `ProposalId` (re-read via
/// [`Coordinator::pending`]); an unrecoverable host/control failure aborts
/// the campaign and never skips the slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PendingProposal {
    /// The durable proposal identity.
    pub proposal: ProposalId,
    /// Its reserved revision slot (never reused).
    pub revision: Revision,
    /// The frozen cohort it was minted under.
    pub cohort: CohortId,
}

/// A finished rollout's completion: the already-durable evidence-batch
/// identity plus the deterministic V-time/work terminal record that closed
/// it. Committing the same proposal twice is a no-op when byte-identical
/// (worker retry) and a [`CoordError::CommitConflict`] otherwise (a
/// determinism violation, surfaced rather than absorbed).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completion {
    /// The proposal being completed.
    pub proposal: ProposalId,
    /// Opaque, already-durable evidence-batch identity (`hm-bbx.4`).
    pub batch: EvidenceBatchId,
    /// The deterministic terminal record that closed the rollout.
    pub terminal: TerminalRecord,
}

/// The consolidated, canonically ordered committed-input view returned by
/// [`Coordinator::probe_drive`] after the probe barrier: every row's
/// revision is `<=` the search-visible `frontier`, every partial cohort is
/// excluded, and [`DrainedView::encode`] is the byte-stable projection the
/// determinism gate asserts against a golden.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrainedView {
    /// Search-visible frontier (inclusive; [`Revision::ZERO`] = nothing
    /// visible).
    pub frontier: Revision,
    /// Committed inputs `(revision, batch)`, canonically ordered by
    /// revision.
    pub rows: Vec<(Revision, EvidenceBatchId)>,
}

impl DrainedView {
    /// Canonical byte encoding (deterministic serde_json: fixed field
    /// order, integer ids, hex batch digests).
    pub fn encode(&self) -> Vec<u8> {
        // Statically infallible: a plain struct of integers, strings, and
        // sequences cannot fail JSON encoding.
        serde_json::to_vec(self).expect("plain struct encodes")
    }
}

/// Typed coordinator failures.
#[derive(Debug, thiserror::Error)]
pub enum CoordError {
    /// The ledger failed; the coordinator is now poisoned (an unrecoverable
    /// control failure — recover from the durable ledger).
    #[error("ledger failure (coordinator poisoned): {0}")]
    Ledger(#[from] LedgerError),
    /// The campaign was durably aborted; the frontier never advances again.
    #[error("campaign aborted: {reason}")]
    Aborted {
        /// The recorded abort reason.
        reason: String,
    },
    /// A previous ledger failure poisoned this handle; recover from the
    /// durable ledger.
    #[error("coordinator poisoned by an earlier ledger failure")]
    Poisoned,
    /// The cohort was never opened.
    #[error("unknown cohort {0:?}")]
    UnknownCohort(CohortId),
    /// The cohort is closed; no further proposals mint under it.
    #[error("cohort {0:?} is closed")]
    CohortClosed(CohortId),
    /// The proposal was never assigned.
    #[error("unknown proposal {0:?}")]
    UnknownProposal(ProposalId),
    /// A second, non-identical completion for an already-committed proposal
    /// — a determinism violation in the retried worker.
    #[error("conflicting completion for {proposal:?}")]
    CommitConflict {
        /// The already-committed proposal.
        proposal: ProposalId,
    },
    /// `probe_drive` cannot pass `target` with the completions committed so
    /// far (missing completions or an unclosed cohort hold the frontier).
    #[error("frontier stalled at {visible:?}, target {target:?}")]
    FrontierStalled {
        /// The requested watermark.
        target: Revision,
        /// The current search-visible frontier.
        visible: Revision,
    },
    /// `genesis` on a non-empty ledger (use [`Coordinator::recover`]).
    #[error("ledger already initialized")]
    AlreadyInitialized,
    /// `recover` on an empty ledger (use [`Coordinator::genesis`]).
    #[error("ledger has no genesis record")]
    MissingGenesis,
    /// The durable ledger violates the coordinator's write protocol.
    #[error("corrupt ledger: {detail}")]
    CorruptLedger {
        /// What was violated, and at which record.
        detail: String,
    },
    /// An id counter reached `u64::MAX`; abort the campaign (slots are
    /// never reused or wrapped).
    #[error("id space exhausted")]
    IdExhausted,
    /// `open_cohort`/`assign` refused: an earlier cohort is not yet both
    /// closed and fully committed. Cohort visibility is cohort-ATOMIC — one
    /// cohort runs at a time, so a frozen view is constant by construction
    /// and can never split a cohort's results across the frontier
    /// (PR #124 FAM-COHORT ruling, option (a): the full cohort barrier).
    #[error("cohort barrier: {blocking:?} is not closed and fully committed")]
    CohortBarrier {
        /// The earliest cohort still holding the barrier.
        blocking: CohortId,
    },
    /// `set_cell_projection` after inputs already entered the live dataflow —
    /// the graph cannot be rebuilt once fed (install the projection before
    /// the first drain).
    #[error("cell projection installed after {submitted} inputs were fed")]
    ProjectionTooLate {
        /// How many inputs the live dataflow had already received.
        submitted: u64,
    },
    /// A second, non-identical evidence staging for an already-staged
    /// proposal — a determinism violation in the retried worker, surfaced
    /// rather than absorbed (mirroring [`CoordError::CommitConflict`]).
    #[error("conflicting evidence staging for {proposal:?}")]
    StageConflict {
        /// The already-staged proposal.
        proposal: ProposalId,
    },
    /// Evidence staged for a proposal whose revision already drained into
    /// the live dataflow — too late to participate in its revision.
    #[error("evidence for {proposal:?} staged after its revision drained")]
    StagedTooLate {
        /// The late proposal.
        proposal: ProposalId,
    },
    /// One observation identity declared under two different base
    /// operations — a schema conflict (a determinism violation, surfaced).
    #[error("observation declared under conflicting base operations")]
    DeclarationConflict {
        /// The conflicted observation identity (opaque canonical bytes).
        obs: ObsKey,
    },
}

/// Per-cohort bookkeeping.
#[derive(Clone, Debug)]
struct CohortState {
    /// Frozen selector/archive view: the search-visible frontier at open.
    view: Revision,
    closed: bool,
    /// Member revisions in canonical mint order.
    members: Vec<Revision>,
    committed: usize,
}

impl CohortState {
    /// Every member has committed (vacuously true when empty).
    fn fully_committed(&self) -> bool {
        self.committed == self.members.len()
    }
}

/// Per-proposal bookkeeping.
#[derive(Clone, Debug)]
struct ProposalState {
    revision: Revision,
    cohort: CohortId,
    commit: Option<CommitInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CommitInfo {
    batch: EvidenceBatchId,
    terminal: TerminalRecord,
}

/// The pure, durable-derived coordinator state: everything `recover`
/// rebuilds from the ledger. The mutators assume validity — both the live
/// path (after typed pre-checks) and replay (after protocol checks) call
/// them, so live and recovered state evolve through identical code.
struct Core {
    config: CampaignConfigId,
    next_proposal: u64,
    next_revision: u64,
    next_cohort: u64,
    cohorts: BTreeMap<CohortId, CohortState>,
    proposals: BTreeMap<ProposalId, ProposalState>,
    by_revision: BTreeMap<Revision, ProposalId>,
    committed: BTreeMap<Revision, CommitInfo>,
    /// Highest revision of the contiguous committed prefix (0 = none).
    contiguous: u64,
    /// Search-visible frontier (inclusive; 0 = none): the largest `V` such
    /// that every revision `<= V` belongs to a closed, fully-committed
    /// cohort. Monotone.
    visible: u64,
    /// Barrier watermark (hm-a98): the count of the contiguous prefix of
    /// cohorts (ids `1..=done_through`) that are all closed AND fully
    /// committed. Because the cohort barrier forces cohorts to run one at a
    /// time — a new cohort opens only once every prior one is done — a cohort
    /// becomes done in id order and stays done, so this only ever advances.
    /// It makes [`Core::barrier_blocker`] O(1) instead of an O(cohorts) scan
    /// (Θ(N²) over a campaign). `done_through == next_cohort - 1` means every
    /// existing cohort is done and the barrier is clear.
    done_through: u64,
    aborted: Option<String>,
}

impl Core {
    fn new(config: CampaignConfigId) -> Self {
        Core {
            config,
            next_proposal: 1,
            next_revision: 1,
            next_cohort: 1,
            cohorts: BTreeMap::new(),
            proposals: BTreeMap::new(),
            by_revision: BTreeMap::new(),
            committed: BTreeMap::new(),
            contiguous: 0,
            visible: 0,
            done_through: 0,
            aborted: None,
        }
    }

    /// The earliest cohort not yet both closed and fully committed — the
    /// cohort barrier (PR #124 FAM-COHORT, option (a)): while it exists, no
    /// new cohort opens and no proposal mints, so cohorts run one at a
    /// time, occupy contiguous revision ranges, and become visible
    /// atomically. A frozen view is constant by construction: at open,
    /// every minted revision is already visible.
    ///
    /// O(1) via the [`done_through`](Core::done_through) watermark (hm-a98):
    /// cohorts `1..=done_through` are all done, so the earliest not-done
    /// cohort — if one exists — is exactly `done_through + 1`.
    fn barrier_blocker(&self, before: Option<CohortId>) -> Option<CohortId> {
        let first_incomplete = self.done_through + 1;
        // Every existing cohort (`1..=next_cohort - 1`) is done.
        if first_incomplete >= self.next_cohort {
            return None;
        }
        // `find` under the old scan stopped at the first cohort `< before`;
        // the earliest not-done cohort is the only candidate, so it blocks
        // only when it is strictly before `before`.
        if before.is_some_and(|b| first_incomplete >= b.get()) {
            return None;
        }
        Some(CohortId::new(first_incomplete))
    }

    /// Advance the barrier watermark over every next cohort that is now both
    /// closed and fully committed (hm-a98). Amortized O(1): a cohort crosses
    /// the watermark at most once over the whole campaign. Call after any
    /// mutation that can complete a cohort (commit, close).
    fn advance_done_through(&mut self) {
        while self.done_through + 1 < self.next_cohort {
            let id = CohortId::new(self.done_through + 1);
            match self.cohorts.get(&id) {
                Some(c) if c.closed && c.fully_committed() => self.done_through += 1,
                _ => break,
            }
        }
    }

    fn open_cohort_unchecked(&mut self, cohort: CohortId, view: Revision) {
        self.next_cohort += 1;
        self.cohorts.insert(
            cohort,
            CohortState {
                view,
                closed: false,
                members: Vec::new(),
                committed: 0,
            },
        );
    }

    fn mint_unchecked(&mut self, proposal: ProposalId, revision: Revision, cohort: CohortId) {
        self.next_proposal += 1;
        self.next_revision += 1;
        if let Some(c) = self.cohorts.get_mut(&cohort) {
            c.members.push(revision);
        }
        self.proposals.insert(
            proposal,
            ProposalState {
                revision,
                cohort,
                commit: None,
            },
        );
        self.by_revision.insert(revision, proposal);
    }

    fn commit_unchecked(&mut self, proposal: ProposalId, info: CommitInfo) {
        let Some(state) = self.proposals.get_mut(&proposal) else {
            return; // unreachable after pre-checks; harmless if not
        };
        state.commit = Some(info);
        let revision = state.revision;
        let cohort = state.cohort;
        self.committed.insert(revision, info);
        if let Some(c) = self.cohorts.get_mut(&cohort) {
            c.committed += 1;
        }
        while self
            .committed
            .contains_key(&Revision::new(self.contiguous + 1))
        {
            self.contiguous += 1;
        }
        // Committing a closed cohort's last member can complete it.
        self.advance_done_through();
        self.advance_visible();
    }

    fn close_unchecked(&mut self, cohort: CohortId) {
        if let Some(c) = self.cohorts.get_mut(&cohort) {
            c.closed = true;
        }
        // Closing an already-fully-committed cohort completes it.
        self.advance_done_through();
        self.advance_visible();
    }

    /// Advance the search-visible frontier over every next revision whose
    /// cohort is closed and fully committed. No partial-cohort result is
    /// ever below the frontier, so none is readable by another proposal.
    fn advance_visible(&mut self) {
        loop {
            let next = Revision::new(self.visible + 1);
            let Some(proposal) = self.by_revision.get(&next) else {
                break; // not minted yet
            };
            let Some(state) = self.proposals.get(proposal) else {
                break; // unreachable: by_revision and proposals move together
            };
            let Some(cohort) = self.cohorts.get(&state.cohort) else {
                break; // unreachable: proposals only mint under known cohorts
            };
            if cohort.closed && cohort.fully_committed() {
                self.visible += 1;
            } else {
                break;
            }
        }
    }

    /// Strict replay of a durable record stream (the recovery path). Every
    /// deviation from the write protocol is a [`CoordError::CorruptLedger`].
    fn replay(records: &[LedgerRecord]) -> Result<Core, CoordError> {
        let corrupt = |index: usize, detail: String| CoordError::CorruptLedger {
            detail: format!("record {index}: {detail}"),
        };
        let mut iter = records.iter().enumerate();
        let Some((_, LedgerRecord::Genesis { config })) = iter.next() else {
            return Err(CoordError::CorruptLedger {
                detail: "record 0: first record is not genesis".to_owned(),
            });
        };
        let mut core = Core::new(*config);
        for (index, record) in iter {
            if core.aborted.is_some() {
                return Err(corrupt(index, "record after abort".to_owned()));
            }
            match record {
                LedgerRecord::Genesis { .. } => {
                    return Err(corrupt(index, "duplicate genesis".to_owned()));
                }
                LedgerRecord::CohortOpen { cohort, view } => {
                    if cohort.get() != core.next_cohort {
                        return Err(corrupt(index, format!("non-dense cohort id {cohort:?}")));
                    }
                    if let Some(blocking) = core.barrier_blocker(None) {
                        return Err(corrupt(
                            index,
                            format!("cohort {cohort:?} opened across the barrier ({blocking:?})"),
                        ));
                    }
                    if view.get() != core.visible {
                        return Err(corrupt(
                            index,
                            format!(
                                "cohort {cohort:?} recorded view {view:?} but the visible \
                                 frontier was {}",
                                core.visible
                            ),
                        ));
                    }
                    core.open_cohort_unchecked(*cohort, *view);
                }
                LedgerRecord::Proposal {
                    proposal,
                    revision,
                    cohort,
                } => {
                    if proposal.get() != core.next_proposal {
                        return Err(corrupt(index, format!("non-dense proposal {proposal:?}")));
                    }
                    if revision.get() != core.next_revision {
                        return Err(corrupt(index, format!("non-dense revision {revision:?}")));
                    }
                    match core.cohorts.get(cohort) {
                        None => return Err(corrupt(index, format!("unknown cohort {cohort:?}"))),
                        Some(c) if c.closed => {
                            return Err(corrupt(index, format!("mint under closed {cohort:?}")));
                        }
                        Some(_) => {}
                    }
                    if let Some(blocking) = core.barrier_blocker(Some(*cohort)) {
                        return Err(corrupt(
                            index,
                            format!("mint under {cohort:?} across the barrier ({blocking:?})"),
                        ));
                    }
                    core.mint_unchecked(*proposal, *revision, *cohort);
                }
                LedgerRecord::Commit {
                    proposal,
                    revision,
                    batch,
                    terminal,
                } => {
                    let state = core
                        .proposals
                        .get(proposal)
                        .ok_or_else(|| corrupt(index, format!("unknown proposal {proposal:?}")))?;
                    if state.commit.is_some() {
                        return Err(corrupt(index, format!("double commit of {proposal:?}")));
                    }
                    if state.revision != *revision {
                        return Err(corrupt(
                            index,
                            format!(
                                "commit revision {revision:?} != assigned {:?}",
                                state.revision
                            ),
                        ));
                    }
                    core.commit_unchecked(
                        *proposal,
                        CommitInfo {
                            batch: *batch,
                            terminal: *terminal,
                        },
                    );
                }
                LedgerRecord::CohortClose { cohort } => {
                    match core.cohorts.get(cohort) {
                        None => return Err(corrupt(index, format!("unknown cohort {cohort:?}"))),
                        Some(c) if c.closed => {
                            return Err(corrupt(index, format!("double close of {cohort:?}")));
                        }
                        Some(_) => {}
                    }
                    core.close_unchecked(*cohort);
                }
                LedgerRecord::Abort { reason } => {
                    core.aborted = Some(reason.clone());
                }
            }
        }
        Ok(core)
    }
}

/// A byte-stable projection of the durable-derived coordinator state, for
/// the crash-recovery equality gate: a crashed-and-recovered coordinator
/// must project byte-identically to a never-crashed run of the same seed
/// and completion set. Deliberately excludes process-local state (what has
/// been fed to the live dataflow) — the live arrangement is never authority.
///
/// Test/golden apparatus (hm-fb0): gated behind `test-support` so it is not
/// part of the coordinator's production contract.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateProjection {
    /// The pinned campaign configuration identity.
    pub config: CampaignConfigId,
    /// The abort reason, if the campaign aborted.
    pub aborted: Option<String>,
    /// Next proposal ordinal to mint.
    pub next_proposal: u64,
    /// Next revision slot to reserve.
    pub next_revision: u64,
    /// Next cohort ordinal to mint.
    pub next_cohort: u64,
    /// Every cohort: `(id, frozen view, closed, member revisions)`.
    pub cohorts: Vec<(CohortId, Revision, bool, Vec<Revision>)>,
    /// Every commit: `(revision, proposal, batch, terminal)`.
    pub commits: Vec<(Revision, ProposalId, EvidenceBatchId, TerminalRecord)>,
    /// Highest revision of the contiguous committed prefix.
    pub committed_frontier: Revision,
    /// The search-visible frontier.
    pub visible_frontier: Revision,
    /// Assigned-but-uncommitted proposals, in revision order.
    pub pending: Vec<PendingProposal>,
}

#[cfg(any(test, feature = "test-support"))]
impl StateProjection {
    /// Canonical byte encoding (deterministic serde_json).
    pub fn encode(&self) -> Vec<u8> {
        // Statically infallible: a plain struct of integers, strings, and
        // sequences cannot fail JSON encoding.
        serde_json::to_vec(self).expect("plain struct encodes")
    }
}

/// The control-side input coordinator. See the crate docs for the contract;
/// construction is [`Coordinator::genesis`] (fresh ledger) or
/// [`Coordinator::recover`] (restart).
pub struct Coordinator {
    core: Core,
    ledger: Box<dyn Ledger>,
    host: ProbeHost,
    /// Highest revision submitted to the live dataflow (process-local; a
    /// recovered coordinator starts at 0 and re-feeds the durable prefix).
    submitted: u64,
    poisoned: bool,
    /// Staged evidence rows per revision, awaiting their drain (process-
    /// local; a recovered coordinator's controller re-stages from its own
    /// durable evidence ledger before the first drive).
    staged: BTreeMap<u64, EvidenceRows>,
    /// Every observation declaration seen (staged or fed), for the schema
    /// conflict check — one identity, one base operation.
    declared: BTreeMap<ObsKey, ReduceOp>,
    /// Identities already fed to the live dataflow (a declaration is fed
    /// exactly once, so declaration joins never fan out).
    fed_declares: BTreeSet<ObsKey>,
}

impl Coordinator {
    /// Initialize a fresh campaign ledger: append and sync the genesis
    /// record pinning `config`. Fails with
    /// [`CoordError::AlreadyInitialized`] if the ledger has durable records.
    pub fn genesis(ledger: Box<dyn Ledger>, config: CampaignConfigId) -> Result<Self, CoordError> {
        let mut ledger = ledger;
        if !ledger.replay()?.is_empty() {
            return Err(CoordError::AlreadyInitialized);
        }
        ledger.append(&LedgerRecord::Genesis { config })?;
        ledger.sync()?;
        Ok(Coordinator {
            core: Core::new(config),
            ledger,
            host: ProbeHost::new(Rc::new(canonical_cell)),
            submitted: 0,
            poisoned: false,
            staged: BTreeMap::new(),
            declared: BTreeMap::new(),
            fed_declares: BTreeSet::new(),
        })
    }

    /// Replay the durable ledger; recover frontier and pending proposals
    /// exactly. The returned coordinator writes through an independent
    /// handle obtained from [`Ledger::reopen`]; the live dataflow is rebuilt
    /// fresh and re-fed the committed prefix on the next
    /// [`Coordinator::drain_ready`]/[`Coordinator::probe_drive`] — restart
    /// replays committed ledger inputs, never a live arrangement.
    pub fn recover(ledger: &dyn Ledger) -> Result<Self, CoordError> {
        let owned = ledger.reopen()?;
        let records = owned.replay()?;
        if records.is_empty() {
            return Err(CoordError::MissingGenesis);
        }
        let core = Core::replay(&records)?;
        Ok(Coordinator {
            core,
            ledger: owned,
            host: ProbeHost::new(Rc::new(canonical_cell)),
            submitted: 0,
            poisoned: false,
            staged: BTreeMap::new(),
            declared: BTreeMap::new(),
            fed_declares: BTreeSet::new(),
        })
    }

    /// The pinned campaign configuration identity.
    pub fn config(&self) -> CampaignConfigId {
        self.core.config
    }

    /// The search-visible frontier (inclusive; [`Revision::ZERO`] = nothing
    /// visible).
    pub fn visible_frontier(&self) -> Revision {
        Revision::new(self.core.visible)
    }

    /// Highest revision of the contiguous committed prefix.
    pub fn committed_frontier(&self) -> Revision {
        Revision::new(self.core.contiguous)
    }

    /// The abort reason, if the campaign durably aborted.
    pub fn aborted(&self) -> Option<&str> {
        self.core.aborted.as_deref()
    }

    /// A cohort's frozen selector/archive view (the search-visible frontier
    /// at its open).
    pub fn cohort_view(&self, cohort: CohortId) -> Option<Revision> {
        self.core.cohorts.get(&cohort).map(|c| c.view)
    }

    /// Assigned-but-uncommitted proposals in revision order — what a
    /// restarted dispatcher re-dispatches (same `ProposalId`, same
    /// `Revision`, never a fresh slot).
    pub fn pending(&self) -> Vec<PendingProposal> {
        let mut out: Vec<PendingProposal> = self
            .core
            .proposals
            .iter()
            .filter(|(_, s)| s.commit.is_none())
            .map(|(p, s)| PendingProposal {
                proposal: *p,
                revision: s.revision,
                cohort: s.cohort,
            })
            .collect();
        out.sort_by_key(|p| p.revision);
        out
    }

    /// The byte-stable durable-state projection (see [`StateProjection`]).
    /// Test/golden apparatus (hm-fb0), gated behind `test-support`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn state_projection(&self) -> StateProjection {
        StateProjection {
            config: self.core.config,
            aborted: self.core.aborted.clone(),
            next_proposal: self.core.next_proposal,
            next_revision: self.core.next_revision,
            next_cohort: self.core.next_cohort,
            cohorts: self
                .core
                .cohorts
                .iter()
                .map(|(id, c)| (*id, c.view, c.closed, c.members.clone()))
                .collect(),
            commits: self
                .core
                .committed
                .iter()
                .map(|(rev, info)| {
                    let proposal = self.core.by_revision.get(rev).copied().unwrap_or_default();
                    (*rev, proposal, info.batch, info.terminal)
                })
                .collect(),
            committed_frontier: Revision::new(self.core.contiguous),
            visible_frontier: Revision::new(self.core.visible),
            pending: self.pending(),
        }
    }

    /// Refuse operations on a poisoned or aborted coordinator.
    fn ensure_live(&self) -> Result<(), CoordError> {
        if self.poisoned {
            return Err(CoordError::Poisoned);
        }
        if let Some(reason) = &self.core.aborted {
            return Err(CoordError::Aborted {
                reason: reason.clone(),
            });
        }
        Ok(())
    }

    /// Append + sync one record; a failure poisons this handle (an
    /// unrecoverable control failure — the campaign must abort or recover,
    /// never skip the slot).
    fn persist(&mut self, record: &LedgerRecord) -> Result<(), CoordError> {
        let result = self.ledger.append(record).and_then(|()| self.ledger.sync());
        if let Err(e) = result {
            self.poisoned = true;
            return Err(CoordError::Ledger(e));
        }
        Ok(())
    }

    /// Open a cohort, freezing its selector/archive view at the current
    /// search-visible frontier. Refused ([`CoordError::CohortBarrier`])
    /// while any earlier cohort is not both closed and fully committed —
    /// the full cohort barrier (PR #124 FAM-COHORT, option (a)): cohorts
    /// run one at a time over contiguous revision ranges and become visible
    /// atomically, so the frozen view is constant by construction (at open,
    /// every minted revision is already visible) and can never depend on
    /// completion arrival order or split a cohort across the frontier.
    pub fn open_cohort(&mut self) -> Result<CohortId, CoordError> {
        self.ensure_live()?;
        if let Some(blocking) = self.core.barrier_blocker(None) {
            return Err(CoordError::CohortBarrier { blocking });
        }
        if self.core.next_cohort == u64::MAX {
            return Err(CoordError::IdExhausted);
        }
        let cohort = CohortId::new(self.core.next_cohort);
        let view = Revision::new(self.core.visible);
        self.persist(&LedgerRecord::CohortOpen { cohort, view })?;
        self.core.open_cohort_unchecked(cohort, view);
        Ok(cohort)
    }

    /// Close a cohort: its mint order is final, and once every member
    /// commits, its results become search-visible atomically.
    pub fn close_cohort(&mut self, cohort: CohortId) -> Result<(), CoordError> {
        self.ensure_live()?;
        match self.core.cohorts.get(&cohort) {
            None => return Err(CoordError::UnknownCohort(cohort)),
            Some(c) if c.closed => return Err(CoordError::CohortClosed(cohort)),
            Some(_) => {}
        }
        self.persist(&LedgerRecord::CohortClose { cohort })?;
        self.core.close_unchecked(cohort);
        Ok(())
    }

    /// Persist a proposal→`Revision` assignment and the cohort view BEFORE
    /// dispatch; never reuses a `Revision`. The record is durable (synced)
    /// when this returns — the persist-then-dispatch handshake. Refused
    /// ([`CoordError::CohortBarrier`]) while any earlier cohort is not both
    /// closed and fully committed (unreachable through the public API once
    /// `open_cohort` enforces the barrier, but held independently so a
    /// hand-built or future caller cannot interleave cohorts either).
    pub fn assign(&mut self, cohort: CohortId) -> Result<PendingProposal, CoordError> {
        self.ensure_live()?;
        match self.core.cohorts.get(&cohort) {
            None => return Err(CoordError::UnknownCohort(cohort)),
            Some(c) if c.closed => return Err(CoordError::CohortClosed(cohort)),
            Some(_) => {}
        }
        if let Some(blocking) = self.core.barrier_blocker(Some(cohort)) {
            return Err(CoordError::CohortBarrier { blocking });
        }
        if self.core.next_proposal == u64::MAX || self.core.next_revision == u64::MAX {
            return Err(CoordError::IdExhausted);
        }
        let proposal = ProposalId::new(self.core.next_proposal);
        let revision = Revision::new(self.core.next_revision);
        self.persist(&LedgerRecord::Proposal {
            proposal,
            revision,
            cohort,
        })?;
        self.core.mint_unchecked(proposal, revision, cohort);
        Ok(PendingProposal {
            proposal,
            revision,
            cohort,
        })
    }

    /// Atomically commit an already-durable batch identity to its
    /// proposal's `Revision`; buffers out-of-order completions and never
    /// advances the frontier past a gap. Idempotent for a byte-identical
    /// retry (crashed worker, same `ProposalId`); a divergent retry is a
    /// [`CoordError::CommitConflict`].
    pub fn complete(&mut self, c: Completion) -> Result<(), CoordError> {
        self.ensure_live()?;
        let state = self
            .core
            .proposals
            .get(&c.proposal)
            .ok_or(CoordError::UnknownProposal(c.proposal))?;
        let info = CommitInfo {
            batch: c.batch,
            terminal: c.terminal,
        };
        if let Some(existing) = &state.commit {
            if *existing == info {
                return Ok(()); // deterministic retry: absorb
            }
            return Err(CoordError::CommitConflict {
                proposal: c.proposal,
            });
        }
        let revision = state.revision;
        self.persist(&LedgerRecord::Commit {
            proposal: c.proposal,
            revision,
            batch: c.batch,
            terminal: c.terminal,
        })?;
        self.core.commit_unchecked(c.proposal, info);
        Ok(())
    }

    /// Install the cell projection the production relations evaluate at
    /// every evaluation point (default: [`canonical_cell`]). Rebuilds the
    /// live dataflow, so it is only legal **before the first drain** —
    /// afterwards it fails with [`CoordError::ProjectionTooLate`] rather
    /// than silently reinterpreting already-fed inputs. The projection must
    /// be a pure function of its arguments (the determinism contract).
    pub fn set_cell_projection(&mut self, proj: CellProjection) -> Result<(), CoordError> {
        self.ensure_live()?;
        if self.submitted > 0 {
            return Err(CoordError::ProjectionTooLate {
                submitted: self.submitted,
            });
        }
        self.host = ProbeHost::new(proj);
        self.fed_declares.clear();
        Ok(())
    }

    /// Stage the typed evidence rows of one assigned proposal's batch: they
    /// enter the production relations at the proposal's revision when it
    /// drains. Idempotent for a byte-identical restage (crashed worker); a
    /// divergent restage is a [`CoordError::StageConflict`]. Staging after
    /// the revision drained is a [`CoordError::StagedTooLate`]; declaring
    /// one observation identity under two base operations is a
    /// [`CoordError::DeclarationConflict`].
    pub fn stage_evidence(
        &mut self,
        proposal: ProposalId,
        rows: EvidenceRows,
    ) -> Result<(), CoordError> {
        self.ensure_live()?;
        let state = self
            .core
            .proposals
            .get(&proposal)
            .ok_or(CoordError::UnknownProposal(proposal))?;
        let rev = state.revision.get();
        if rev <= self.submitted {
            return Err(CoordError::StagedTooLate { proposal });
        }
        for (obs, op) in &rows.declares {
            match self.declared.get(obs) {
                Some(existing) if existing != op => {
                    return Err(CoordError::DeclarationConflict { obs: obs.clone() });
                }
                _ => {}
            }
        }
        if let Some(existing) = self.staged.get(&rev) {
            if *existing == rows {
                return Ok(()); // deterministic retry: absorb
            }
            return Err(CoordError::StageConflict { proposal });
        }
        for (obs, op) in &rows.declares {
            self.declared.insert(obs.clone(), *op);
        }
        self.staged.insert(rev, rows);
        Ok(())
    }

    /// Drain contiguous Revision-ordered completions up to the first unmet
    /// slot, advancing the probe frontier: each drained pair is submitted to
    /// the live dataflow at its revision, together with its staged evidence
    /// rows (declarations deduplicated so the declaration join never fans
    /// out). Returns the newly drained pairs (empty after an abort or
    /// poisoning — the frontier never advances again).
    pub fn drain_ready(&mut self) -> Vec<(Revision, EvidenceBatchId)> {
        if self.ensure_live().is_err() {
            return Vec::new();
        }
        let mut out = Vec::new();
        while self.submitted < self.core.contiguous {
            let rev = Revision::new(self.submitted + 1);
            let Some(info) = self.core.committed.get(&rev) else {
                break; // unreachable: contiguous counts committed slots
            };
            self.host.insert(rev.get(), info.batch.0);
            if let Some(mut rows) = self.staged.remove(&rev.get()) {
                rows.declares
                    .retain(|(obs, _)| self.fed_declares.insert(obs.clone()));
                self.host.feed_rows(rev.get(), &rows);
            }
            out.push((rev, info.batch));
            self.submitted += 1;
        }
        self.host.advance(self.submitted + 1);
        out
    }

    /// The consolidated, canonically ordered materialized views at `at`
    /// (inclusive) — the production observations/cells/occupancy relations.
    /// Follows the probe-barrier read discipline: `at` must be at or below
    /// both the search-visible frontier and the driven probe watermark
    /// (call after [`Coordinator::probe_drive`] has passed it), else this
    /// fails with [`CoordError::FrontierStalled`].
    pub fn materialized(&self, at: Revision) -> Result<MaterializedViews, CoordError> {
        self.ensure_live()?;
        let readable = self.core.visible.min(self.host.driven().saturating_sub(1));
        if at.get() > readable {
            return Err(CoordError::FrontierStalled {
                target: at,
                visible: Revision::new(readable),
            });
        }
        Ok(self.host.materialized(at.get()))
    }

    /// Every committed input in revision order:
    /// `(revision, proposal, batch)`. The recovery hook a controller uses to
    /// re-stage evidence rows from its own durable evidence ledger before
    /// the first drive (restart replays committed ledger inputs, never a
    /// live arrangement).
    pub fn committed_inputs(&self) -> Vec<(Revision, ProposalId, EvidenceBatchId)> {
        self.core
            .committed
            .iter()
            .map(|(rev, info)| {
                let proposal = self.core.by_revision.get(rev).copied().unwrap_or_default();
                (*rev, proposal, info.batch)
            })
            .collect()
    }

    /// Drive Differential probes until the search-visible frontier passes
    /// `target`, then return the consolidated, canonically ordered committed
    /// inputs at the frontier. No partial-cohort result reaches another
    /// proposal: rows above the visible frontier — in particular every row
    /// of a cohort that is not both closed and fully committed — are not in
    /// the view. Fails with [`CoordError::FrontierStalled`] when `target` is
    /// unreachable without further completions (single-threaded: blocking
    /// would deadlock, so stalling is an error, not a wait).
    pub fn probe_drive(&mut self, target: Revision) -> Result<DrainedView, CoordError> {
        self.ensure_live()?;
        self.drain_ready();
        self.host.drive(self.submitted + 1);
        let visible = self.core.visible;
        if visible < target.get() {
            return Err(CoordError::FrontierStalled {
                target,
                visible: Revision::new(visible),
            });
        }
        let rows = self
            .host
            .view(visible)
            .into_iter()
            .map(|(rev, batch)| (Revision::new(rev), EvidenceBatchId::from_bytes(batch)))
            .collect();
        Ok(DrainedView {
            frontier: Revision::new(visible),
            rows,
        })
    }

    /// Durably abort the campaign (unrecoverable host/control failure): the
    /// frontier never advances again and no slot is ever skipped.
    /// Idempotent once aborted.
    ///
    /// The `reason` is bounded to `MAX_ABORT_REASON` (64 KiB) before it is
    /// recorded (hm-20m): a post-mortem prefix is enough, and the bound
    /// guarantees the Abort frame always fits under the ledger's frame bound,
    /// so an over-long reason can never poison the coordinator *without*
    /// durably recording the abort. The same bounded reason is stored in the
    /// ledger and in the recovered state, so recovery reproduces it verbatim.
    pub fn abort(&mut self, reason: &str) -> Result<(), CoordError> {
        if self.poisoned {
            return Err(CoordError::Poisoned);
        }
        if self.core.aborted.is_some() {
            return Ok(());
        }
        let reason = bound_reason(reason);
        self.persist(&LedgerRecord::Abort {
            reason: reason.clone(),
        })?;
        self.core.aborted = Some(reason);
        Ok(())
    }
}

/// Truncate an abort reason to [`MAX_ABORT_REASON`] bytes on a UTF-8
/// boundary (hm-20m). The reason is post-mortem-only, so a bounded prefix is
/// enough; the bound guarantees the encoded Abort frame stays under the
/// ledger frame bound, so [`Coordinator::abort`] always persists rather than
/// poisoning without recording. Truncation is deterministic (same input →
/// same prefix), preserving the determinism contract.
fn bound_reason(reason: &str) -> String {
    if reason.len() <= MAX_ABORT_REASON {
        return reason.to_owned();
    }
    // Largest char boundary <= MAX_ABORT_REASON — never split a UTF-8 scalar.
    let mut end = MAX_ABORT_REASON;
    while end > 0 && !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // -- hm-a98: the O(1) barrier watermark agrees with the O(N) full scan --

    /// The pre-hm-a98 full scan — the reference the watermark must match
    /// exactly, for every `before`.
    fn barrier_blocker_fullscan(core: &Core, before: Option<CohortId>) -> Option<CohortId> {
        core.cohorts
            .iter()
            .take_while(|(id, _)| before.is_none_or(|b| **id < b))
            .find(|(_, c)| !(c.closed && c.fully_committed()))
            .map(|(id, _)| *id)
    }

    /// The watermark implementation must agree with the full scan for
    /// `before = None` and every `before = Some(CohortId(1..=next_cohort))`.
    fn assert_barrier_agrees(core: &Core) {
        assert_eq!(
            core.barrier_blocker(None),
            barrier_blocker_fullscan(core, None),
            "watermark != full scan at before=None, done_through={}",
            core.done_through,
        );
        for b in 1..=core.next_cohort {
            let before = Some(CohortId::new(b));
            assert_eq!(
                core.barrier_blocker(before),
                barrier_blocker_fullscan(core, before),
                "watermark != full scan at before={b}, done_through={}",
                core.done_through,
            );
        }
    }

    fn commit_info(r: Revision) -> CommitInfo {
        CommitInfo {
            batch: EvidenceBatchId::digest(&r.get().to_le_bytes()),
            terminal: TerminalRecord {
                moment: r.get(),
                work: r.get(),
            },
        }
    }

    /// A hand-built multi-cohort campaign (empty / partial / multi-member
    /// cohorts, close both before and after the last commit) that checks
    /// watermark == full-scan after every single mutation, and that the
    /// watermark never regresses.
    #[test]
    fn watermark_matches_full_scan_over_a_campaign() {
        let mut core = Core::new(CampaignConfigId::digest(b"watermark-test"));
        let mut prev_done = 0u64;
        macro_rules! check {
            () => {{
                assert_barrier_agrees(&core);
                assert!(core.done_through >= prev_done, "watermark regressed");
                prev_done = core.done_through;
            }};
        }
        check!();

        let sizes = [2usize, 1, 0, 3];
        let mut next_rev = 1u64;
        let mut next_prop = 1u64;
        for (ci, &size) in sizes.iter().enumerate() {
            let cohort = CohortId::new(ci as u64 + 1);
            core.open_cohort_unchecked(cohort, Revision::new(core.visible));
            check!();

            let mut members = Vec::new();
            for _ in 0..size {
                let (p, r) = (ProposalId::new(next_prop), Revision::new(next_rev));
                core.mint_unchecked(p, r, cohort);
                members.push((p, r));
                next_prop += 1;
                next_rev += 1;
                check!();
            }

            match members.split_last() {
                Some((&(last_p, last_r), rest)) => {
                    // Commit all but the last, close (still incomplete), then
                    // commit the last — exercising the commit-completes path.
                    for &(p, r) in rest {
                        core.commit_unchecked(p, commit_info(r));
                        check!();
                    }
                    core.close_unchecked(cohort);
                    check!();
                    core.commit_unchecked(last_p, commit_info(last_r));
                    check!();
                }
                None => {
                    // Empty cohort: closing it completes it immediately (the
                    // close-completes path).
                    core.close_unchecked(cohort);
                    check!();
                }
            }
        }
        // Every cohort done: the barrier is clear.
        assert_eq!(core.barrier_blocker(None), None);
        assert_eq!(core.done_through, core.next_cohort - 1);
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

        /// Over random barrier-legal campaigns, the O(1) watermark agrees
        /// with the O(N) full scan after every mutation, for every `before`.
        #[test]
        fn watermark_matches_full_scan_proptest(
            sizes in prop::collection::vec(0usize..=3, 1..=5),
            close_seed in any::<u64>(),
        ) {
            let mut core = Core::new(CampaignConfigId::digest(b"wm-proptest"));
            assert_barrier_agrees(&core);
            let (mut next_rev, mut next_prop, mut s) = (1u64, 1u64, close_seed);
            for (ci, &size) in sizes.iter().enumerate() {
                let cohort = CohortId::new(ci as u64 + 1);
                core.open_cohort_unchecked(cohort, Revision::new(core.visible));
                assert_barrier_agrees(&core);

                let mut members = Vec::new();
                for _ in 0..size {
                    let (p, r) = (ProposalId::new(next_prop), Revision::new(next_rev));
                    core.mint_unchecked(p, r, cohort);
                    members.push((p, r));
                    next_rev += 1;
                    next_prop += 1;
                    assert_barrier_agrees(&core);
                }

                // A random close point in 0..=size, interleaved with commits.
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let close_at = (s >> 33) as usize % (size + 1);
                let mut closed = false;
                if close_at == 0 {
                    core.close_unchecked(cohort);
                    closed = true;
                    assert_barrier_agrees(&core);
                }
                for (t, &(p, r)) in members.iter().enumerate() {
                    core.commit_unchecked(p, commit_info(r));
                    assert_barrier_agrees(&core);
                    if !closed && close_at == t + 1 {
                        core.close_unchecked(cohort);
                        closed = true;
                        assert_barrier_agrees(&core);
                    }
                }
                // Keep the campaign barrier-legal: each cohort is done before
                // the next opens.
                if !closed {
                    core.close_unchecked(cohort);
                    assert_barrier_agrees(&core);
                }
            }
        }
    }
}
