// SPDX-License-Identifier: AGPL-3.0-or-later
//! The coordinator: persist-then-dispatch assignment, out-of-order
//! completion buffering, cohort freeze, probe-frontier drive, and crash
//! recovery. See the crate docs for the contract summary.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::host::ProbeHost;
use crate::ids::{
    CampaignConfigId, CohortId, EvidenceBatchId, ProposalId, Revision, TerminalRecord,
};
use crate::ledger::{Ledger, LedgerError, LedgerRecord};

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
            aborted: None,
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
        self.advance_visible();
    }

    fn close_unchecked(&mut self, cohort: CohortId) {
        if let Some(c) = self.cohorts.get_mut(&cohort) {
            c.closed = true;
        }
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
            host: ProbeHost::new(),
            submitted: 0,
            poisoned: false,
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
            host: ProbeHost::new(),
            submitted: 0,
            poisoned: false,
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
    /// search-visible frontier. The frozen view can never include a partial
    /// cohort: visibility only advances over closed, fully-committed
    /// cohorts.
    pub fn open_cohort(&mut self) -> Result<CohortId, CoordError> {
        self.ensure_live()?;
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
    /// when this returns — the persist-then-dispatch handshake.
    pub fn assign(&mut self, cohort: CohortId) -> Result<PendingProposal, CoordError> {
        self.ensure_live()?;
        match self.core.cohorts.get(&cohort) {
            None => return Err(CoordError::UnknownCohort(cohort)),
            Some(c) if c.closed => return Err(CoordError::CohortClosed(cohort)),
            Some(_) => {}
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

    /// Drain contiguous Revision-ordered completions up to the first unmet
    /// slot, advancing the probe frontier: each drained pair is submitted to
    /// the live dataflow at its revision. Returns the newly drained pairs
    /// (empty after an abort or poisoning — the frontier never advances
    /// again).
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
            out.push((rev, info.batch));
            self.submitted += 1;
        }
        self.host.advance(self.submitted + 1);
        out
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
    pub fn abort(&mut self, reason: &str) -> Result<(), CoordError> {
        if self.poisoned {
            return Err(CoordError::Poisoned);
        }
        if self.core.aborted.is_some() {
            return Ok(());
        }
        self.persist(&LedgerRecord::Abort {
            reason: reason.to_owned(),
        })?;
        self.core.aborted = Some(reason.to_owned());
        Ok(())
    }
}
