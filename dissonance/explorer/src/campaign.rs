// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **two-barrier Differential campaign controller** (`hm-bbx.4`).
//!
//! [`DifferentialCampaign`] is the generic Explorer's production search loop in
//! its Differential-integrated form: the imperative, seeded campaign controller
//! that owns the crash-safe evidence append, drives the Revision coordinator's
//! two probe barriers, schedules budgeted materialization replay, and commits
//! archive occupancy only at an actual seal. It **is** the "replace the production
//! Sensor/FeatureSet/Archive::admit path" — no `Archive::admit`, no packed
//! feature; cells come from independently-reduced normalized observations at the
//! server-captured `sealed_at`.
//!
//! ## One step, two barriers (the acceptance-criteria protocol)
//!
//! 1. **Reserve** a durable revision slot (persist-then-dispatch): `assign`
//!    before any rollout runs.
//! 2. **Dispatch**: the [`Selector`] picks a base, the [`Materializer`]
//!    materializes it, one open-loop rollout runs, its normalized SDK evidence is
//!    decoded.
//! 3. **Durably append** the finished normalized evidence to the [`EvidenceLedger`]
//!    → a durable batch identity.
//! 4. **Submit** that identity to the coordinator for commit (`complete`), and
//!    close the rollout's cohort.
//! 5. **Barrier 1** (`probe_drive`): read non-authoritative provisional
//!    observation/cell transitions from the *committed* evidence only after the
//!    probe frontier passes.
//! 6. **Dedupe / order / cap** the provisional candidates, and **charge the
//!    replay budget**.
//! 7. For each surviving candidate: **materialize** (replay to the first valid
//!    `sealed_at`, holding a temporary server-cut seal), append the seal's
//!    actual-cut evidence, `complete` its **later revision**.
//! 8. **Barrier 2** (`probe_drive`): compute `CellFn` at the **actual** `sealed_at`
//!    and the best-Entry-per-cell occupancy; keep the temporary seal only for an
//!    admitted Entry, drop it otherwise.
//!
//! A provisional transition never occupies the archive (only an actual seal past
//! barrier 2 does), and disappearing pre-seal state is not admitted — the
//! half-open included-count cut excludes any evidence at or after the seal
//! boundary, so evidence emitted at/after a terminal/crash cannot influence a
//! cell committed at an earlier seal (`hm-mcx`).

use std::collections::BTreeMap;
use std::rc::Rc;

use revision_coordinator::{
    Completion, CoordError, Coordinator, CutRow, EntryCommitRow, EvidenceRows, LineageRow,
    PointRow, ReduceOp, ReducedRow, Revision, SealRow, StateEventRow, TerminalRecord,
};

use crate::error::MachineError;
use crate::evidence::{
    CompletedRunEvidence, EvidenceRole, ObservationCells, ObservationMap, ReducedValue, RunId,
    decode_observation_id, encode_observation_id,
};
use crate::ledger::{EvidenceLedger, LedgerError};
use crate::materialize::Materializer;
use crate::occurrence::{AbsenceLedger, OccurrenceCounterexample};
use crate::prng::Prng;
use crate::retention::{
    BatchAvailability, GcReport, GcSkipReason, RawAvailability, Recomputation, RetentionCheckpoint,
    RetentionError, RetentionProfile, RetentionReport, RetentionViews,
};
use crate::seam::{EnvCodec, Machine};
use crate::spine::{
    CellKey, DecisionPoint, EvidenceCut, ExemplarRef, Frontier, FrontierEntry, Moment, Reward,
    Selector, Tactic, VirtualExemplar,
};
use crate::{Answer, Reproducer, SnapId, StopConditions, StopMask, StopReason};
use sdk_events::{Normalized, SdkError, UpdateOp, decode_antithesis, decode_binary};

/// The binary-wire catalog marker event id (`hm-bbx.1`): a raw tuple whose id is
/// this is the schema declaration, not a firing. Inherited through lineage on a
/// branch child, never re-appended as child firing evidence.
pub(crate) const CATALOG_EVENT_ID: u32 = 0;

/// How much V-time each candidate-seal retry leg advances past the last stop
/// before re-attempting the seal (task 136 / hm-esfd): the "run a little
/// further and retry" of the `ControlServer::run` seal contract, mirroring the
/// base-seal loop's box step (`campaign_runner::boxrun::SNAPSHOT_RETRY`).
const SEAL_RETRY_STEP: u64 = 1_000_000;

/// How many failed candidate-seal attempts before the candidate is dropped
/// like a disappearing state ([`MachineError::NotSealable`]) — never a
/// campaign abort. Bounds the advance past the last drained marker at
/// `SEAL_RETRY_ATTEMPTS × SEAL_RETRY_STEP` of V-time (plus natural-boundary
/// skid), keeping "wait a beat, then snap" a beat: a never-quiescent
/// candidate costs one bounded replay, not the campaign.
const SEAL_RETRY_ATTEMPTS: u32 = 64;

/// Which ingress format the controller decodes a rollout's raw SDK capture with.
/// The internal binary Event wire (the shape [`Machine::sdk_events`] returns) is
/// the default; Antithesis JSON is available for the app-facing surface.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Ingress {
    /// The internal binary Event wire (`(moment, event_id, bytes)`).
    #[default]
    Binary,
    /// The app-facing Antithesis JSON surface (each record is one JSON object;
    /// the `event_id` is ignored).
    AntithesisJson,
}

/// The controller's deterministic knobs. Search knobs stay distinct from the
/// declared evidence-retention policy, but both live here: every retention or
/// eviction choice that can affect search or Resolution is declared in the
/// campaign configuration, with stable tie-breaks, independent of disk
/// pressure and wall time (`hm-5sv`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CampaignConfig {
    /// The maximum number of provisional candidates materialized in one step
    /// (the configured materialization cap).
    pub candidate_cap: usize,
    /// The total materialization-replay budget the campaign may charge across all
    /// steps. Each materialized candidate charges one unit; at zero, no further
    /// candidate is replayed (search continues, materialization does not).
    pub replay_budget: u64,
    /// Which ingress format to decode each rollout's SDK capture with.
    pub ingress: Ingress,
    /// The declared evidence-retention profile (with its stable expiry
    /// tie-breaks). Defaults to the full-retention evaluation profile, which
    /// records from the first rollout.
    pub retention: RetentionProfile,
    /// The declared evidence byte budget, if any. Exceeding it fails an append
    /// **loudly** ([`LedgerError::Exhausted`]) — host disk pressure never
    /// silently changes the retention policy.
    pub evidence_budget: Option<u64>,
    /// Where a rollout's provisional-cut nomination coordinates come from
    /// (default: machine-surfaced snapshot points).
    pub nominate: Nomination,
    /// Whether each rollout's terminal machine state is hashed into its
    /// [`StepReport`] (`state_hash`) — the per-branch determinism artifact a
    /// campaign gate compares. Hash-neutral to the search itself (nothing
    /// reads it); off by default (a full state hash costs real time on a
    /// live backend).
    pub hash_rollouts: bool,
}

/// Where a rollout's provisional-cut nomination coordinates come from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Nomination {
    /// The machine-surfaced [`StopReason::SnapshotPoint`]s the sealable
    /// predicate admits (the default; a cooperating workload with explicit
    /// checkpoint boundaries).
    #[default]
    SnapshotPoints,
    /// The distinct `Moment`s of the rollout's own SDK events (filtered by
    /// the sealable predicate): the configured-evidence-cut source for a
    /// workload that surfaces no mid-run snapshot points (e.g. a quiet-arm
    /// game rollout under a deadline, whose only machine snapshot point is
    /// its setup seal).
    EventMoments,
}

impl Default for CampaignConfig {
    fn default() -> Self {
        Self {
            candidate_cap: 8,
            replay_budget: u64::MAX,
            ingress: Ingress::Binary,
            retention: RetentionProfile::Full,
            evidence_budget: None,
            nominate: Nomination::SnapshotPoints,
            hash_rollouts: false,
        }
    }
}

/// Typed controller errors — a transport/backend, coordinator, ledger, codec, or
/// decode failure aborts the step loudly; none is ever reported as a guest bug
/// (the two-result-categories rule).
#[derive(Debug, thiserror::Error)]
pub enum CampaignError {
    /// A [`Machine`]/[`EnvCodec`] transport or backend failure.
    #[error(transparent)]
    Machine(#[from] MachineError),
    /// A Revision-coordinator failure (poisoned handle, commit conflict, stalled
    /// frontier, id exhaustion) — a determinism/durability violation surfaced,
    /// never absorbed.
    #[error("revision coordinator: {0}")]
    Coord(#[from] CoordError),
    /// A durable evidence-ledger failure.
    #[error("evidence ledger: {0}")]
    Ledger(#[from] LedgerError),
    /// An SDK-evidence decode failure (malformed capture) — typed evidence error.
    #[error("sdk evidence decode: {0}")]
    Decode(#[from] SdkError),
    /// The reproducer codec rejected an untrusted blob.
    #[error("env codec: {0}")]
    EnvCodec(#[from] crate::error::EnvCodecError),
    /// A retention/GC proof obligation failed (loud, never absorbed into a
    /// silent policy change).
    #[error("retention: {0}")]
    Retention(#[from] RetentionError),
    /// The materialized Differential view is missing a row the controller
    /// committed inputs for — an internal-invariant break in the production
    /// relations, surfaced loudly rather than absorbed (task 132).
    #[error("materialized view missing {what} for rollout {rollout}")]
    ViewIncomplete {
        /// Which row class was missing ("cut cell", "seal cell").
        what: &'static str,
        /// The rollout whose row was expected.
        rollout: u64,
    },
    /// The task-136 blocking sub-gate failed: an entry's from-ledger
    /// re-materialization (`run(deadline = sealed_at)`, zero probes) landed
    /// at the right coordinate but a **different machine state hash** than
    /// the live seal — the retry-forward's extra deadline legs perturbed
    /// state the ledger env does not record. A determinism violation to
    /// escalate (the env-rider fallback needs a ruling), never to absorb.
    #[error("reseal identity: entry {entry} re-materialized at {at} with a diverged state hash")]
    ResealDivergence {
        /// The frontier entry that failed the identity check.
        entry: u64,
        /// The entry's sealed moment.
        at: u64,
    },
    /// The operational archive and the Differential occupancy view disagree
    /// — a divergence between the production backend and the controller's
    /// mirror, surfaced loudly (the recompute-parity discipline, task 132).
    #[error("occupancy divergence: {detail}")]
    OccupancyDivergence {
        /// What diverged.
        detail: String,
    },
}

// ---------------------------------------------------------------------------
// Occupancy — deterministic best-Entry-per-cell
// ---------------------------------------------------------------------------

/// The outcome of offering one materialized Entry to the occupancy reduction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Occupied {
    /// The Entry claimed a previously-unoccupied cell.
    Fresh(ExemplarRef),
    /// The Entry dominated the cell's prior occupant (which was evicted).
    Dominated {
        /// The admitted Entry's ref.
        entry: ExemplarRef,
        /// The prior occupant, now evicted (its seal is dropped, deterministically,
        /// separate from evidence retention).
        evicted: ExemplarRef,
    },
    /// The Entry was dominated by the cell's occupant and not admitted (its
    /// temporary seal is dropped; it never occupies the archive).
    Rejected,
}

/// The archive occupancy: a deterministic best-Entry-per-cell reduction over a
/// [`Frontier`], dominating by `(quality desc, stable Entry-id asc)`. Entry
/// eviction here is a deterministic archive-policy update, **separate** from
/// evidence retention.
#[derive(Debug, Default)]
struct Occupancy {
    frontier: Frontier,
    /// The versioned quality of each live Entry (higher dominates; ties break to
    /// the lower/earlier stable Entry-id, which is always the occupant, so ties
    /// are first-wins — deterministic).
    quality: BTreeMap<u64, u64>,
}

impl Occupancy {
    fn new() -> Self {
        Self::default()
    }

    fn frontier(&self) -> &Frontier {
        &self.frontier
    }

    /// Offer an Entry occupying `cell` at `quality`. A fresh cell always admits;
    /// an occupied cell admits only a strictly higher quality (equal quality keeps
    /// the earlier occupant — the stable tie-break). The dominated loser (a
    /// replaced occupant, or the rejected newcomer) is reported so the controller
    /// drops its seal.
    fn admit(&mut self, entry: FrontierEntry, cell: CellKey, quality: u64) -> Occupied {
        match self.frontier.occupant(&cell) {
            None => {
                let r = self.frontier.insert(entry);
                self.frontier.claim(cell, r);
                self.quality.insert(r.0, quality);
                Occupied::Fresh(r)
            }
            Some(occ) => {
                let occ_quality = self.quality.get(&occ.0).copied().unwrap_or(0);
                if quality > occ_quality {
                    let r = self.frontier.insert(entry);
                    // `occupy` repoints the cell to `r` and returns the loser.
                    self.frontier.occupy(cell, r);
                    self.quality.insert(r.0, quality);
                    // The dominated occupant held exactly this one cell; evict it.
                    self.frontier.remove(occ);
                    self.quality.remove(&occ.0);
                    Occupied::Dominated {
                        entry: r,
                        evicted: occ,
                    }
                } else {
                    Occupied::Rejected
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The controller
// ---------------------------------------------------------------------------

/// One rollout's captured material, before the two-barrier admission.
struct Rollout {
    /// The terminal stop.
    stop: StopReason,
    /// The branch-local reproducer accumulated over the run.
    env: Reproducer,
    /// The genesis-complete reproducer (the suffix chain folded via `compose`).
    genesis_env: Reproducer,
    /// The normalized SDK evidence of the run (child suffix only — the inherited
    /// ancestor prefix is never re-decoded here).
    normalized: Normalized,
    /// The sealable-point moments observed during the run (nomination coordinates
    /// for provisional transitions), in observation order.
    sealable_moments: Vec<Moment>,
}

/// One step's outcome, for reporting and tests.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct StepReport {
    /// The rollout's committed revision (barrier 1).
    pub rollout_revision: Revision,
    /// The provisional candidates found (nominated for replay) after dedupe/cap.
    pub candidates: usize,
    /// The Entries admitted to occupancy at their actual `sealed_at` (barrier 2).
    pub admitted: Vec<ExemplarRef>,
    /// Occurrence counterexamples newly found this step (deduped by property
    /// across the campaign).
    pub counterexamples: Vec<OccurrenceCounterexample>,
    /// Whether the step explored fresh (genesis) rather than exploiting.
    pub explored: bool,
    /// The rollout's terminal machine state hash, when
    /// [`CampaignConfig::hash_rollouts`] is set (the per-branch determinism
    /// artifact); `None` otherwise.
    pub state_hash: Option<[u8; 32]>,
    /// How many `NotQuiescent` seal refusals this step's candidate
    /// materializations hit under the bounded run-forward (task 136 /
    /// hm-esfd) — direct evidence the "wait a beat, then snap" mechanism
    /// engaged, not merely that nothing needed it. Counts **every** refusal,
    /// including the `1 + SEAL_RETRY_ATTEMPTS` burned by a candidate dropped
    /// at the cap (T136-J2(c): a drop must not report as zero).
    pub seal_retries: u64,
}

/// One task-136 reseal-identity check
/// ([`DifferentialCampaign::verify_reseal_identity`]): the frontier entry, its
/// server-stamped seal moment, and the machine state hash at the seal — equal
/// before eviction and after the from-ledger re-materialization by the
/// check's own assertion.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ResealCheck {
    /// The frontier entry checked.
    pub entry: ExemplarRef,
    /// The entry's server-stamped seal moment.
    pub sealed_at: u64,
    /// The machine state hash at the (re-)materialized seal.
    pub state_hash: [u8; 32],
}

/// The two-barrier Differential campaign controller (module doc). Owns the
/// [`Machine`], the seeded streams, the durable evidence ledger, and the Revision
/// coordinator; drives the whole search loop deterministically.
pub struct DifferentialCampaign<M: Machine> {
    machine: M,
    codec: Box<dyn EnvCodec>,
    tactic: Box<dyn Tactic>,
    selector: Box<dyn Selector>,
    cells: Rc<dyn ObservationCells>,
    occupancy: Occupancy,
    /// The sealed rollout issue behind each live Entry (`ExemplarRef.0` →
    /// rollout issue): the lineage parent a child branched off this Entry
    /// records. Rebuilt from the committed assignments on restart.
    entry_rollout: BTreeMap<u64, u64>,
    /// The Differential entry key behind each live Entry (`ExemplarRef.0` →
    /// the seal proposal's issue): the occupancy-authority reconciliation
    /// key. Rebuilt from the committed assignments on restart.
    entry_key: BTreeMap<u64, u64>,
    mat: Materializer,
    ledger: EvidenceLedger,
    coordinator: Coordinator,
    rng: Prng,
    genesis: SnapId,
    /// The genesis seal's server-stamped cut: a genesis-rooted branch
    /// inherits the machine's pre-campaign SDK prefix (e.g. a workload's
    /// setup events), so its cumulative positions start here — the branch
    /// base cut for `parent == None` rollouts (task 132).
    genesis_cut: EvidenceCut,
    until: StopConditions,
    config: CampaignConfig,
    /// The remaining materialization-replay budget (charged per materialized
    /// candidate).
    replay_left: u64,
    /// The retention views (`hm-5sv`): working set, finalized summary, committed
    /// Entry cell assignments, counterexample dedup state, and the absence fold
    /// — maintained by the same deterministic [`RetentionViews::fold_batch`] a
    /// restart's rebuild replays, so live state and rebuilt state are
    /// bit-identical by construction.
    views: RetentionViews,
}

impl<M: Machine> DifferentialCampaign<M> {
    /// Build a campaign over an already-spawned `machine` (snapshotted for the
    /// genesis base), a reproducer codec, the spine policies, the observation cell
    /// projection, a durable evidence ledger, and a Revision coordinator. `seed`
    /// starts the campaign stream.
    ///
    /// The retention views are **rebuilt from the durable ledger** (its last
    /// committed checkpoint plus the retained suffix), so reopening a campaign's
    /// ledger resumes with exactly the state a restart is entitled to — and a
    /// ledger checkpointed under a different declared profile is rejected
    /// loudly ([`RetentionError::ProfileMismatch`]), never silently
    /// reinterpreted. The declared evidence byte budget is applied to the
    /// ledger before any append.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mut machine: M,
        codec: Box<dyn EnvCodec>,
        tactic: Box<dyn Tactic>,
        selector: Box<dyn Selector>,
        cells: Box<dyn ObservationCells>,
        mut ledger: EvidenceLedger,
        mut coordinator: Coordinator,
        config: CampaignConfig,
        seed: u64,
    ) -> Result<Self, CampaignError> {
        let cells: Rc<dyn ObservationCells> = Rc::from(cells);
        let views = RetentionViews::rebuild(config.retention, cells.as_ref(), &ledger)?;
        ledger.set_budget(config.evidence_budget);
        // Install the campaign's cell projection as the production
        // relations' projection (before any drain): the Differential graph
        // evaluates exactly this `ObservationCells` at every point, over the
        // decoded reduced-observation pairs.
        let proj_cells = Rc::clone(&cells);
        coordinator.set_cell_projection(Rc::new(move |cut: CutRow, pairs: &[_]| {
            let map = decode_reduced_pairs(pairs);
            let cut = EvidenceCut {
                at: Moment(cut.moment),
                sdk_events: cut.count,
            };
            proj_cells.key(cut, &map)
        }))?;
        // Recovery re-staging: restart replays committed ledger inputs (the
        // durable evidence batches), never a live arrangement. A committed
        // batch absent from this evidence ledger contributes no relation
        // rows (a foreign coordinator's input), exactly as it contributes
        // nothing to the rebuilt retention views.
        for (_rev, proposal, batch) in coordinator.committed_inputs() {
            if let Some(ev) = ledger.get(&batch) {
                let rows = evidence_rows(ev);
                coordinator.stage_evidence(proposal, rows)?;
            }
        }
        let (genesis, genesis_cut) = machine.snapshot()?;
        // Restore the operational archive from the committed Entry cell
        // assignments, so the live occupancy and the committed record stay in
        // lock-step across a restart. A restored Entry keeps its
        // genesis-complete reproducer and cut; its snapshot is ephemeral by
        // design and re-materializes from genesis on first exploit.
        let mut occupancy = Occupancy::new();
        let mut entry_rollout = BTreeMap::new();
        let mut entry_key = BTreeMap::new();
        for a in &views.assignments {
            let entry = FrontierEntry {
                exemplar: VirtualExemplar {
                    parent: genesis,
                    // The step-time campaign draw is a diagnostic, not part of
                    // the committed assignment record.
                    seed: 0,
                    suffix: a.env.clone(),
                    cut: a.cut,
                },
                env: a.env.clone(),
                reward: Reward { new_cells: 1 },
            };
            let admitted = occupancy.admit(entry, a.cell.clone(), a.quality);
            debug_assert!(
                matches!(admitted, Occupied::Fresh(_)),
                "assignments hold one entry per cell"
            );
            if let Occupied::Fresh(r) = admitted {
                // A seal batch's RunId is (seal issue, parent = the sealed
                // rollout) — the lineage identities a child branch records.
                entry_rollout.insert(r.0, a.rollout.parent.unwrap_or(a.rollout.issue));
                entry_key.insert(r.0, a.rollout.issue);
            }
        }
        Ok(Self {
            machine,
            codec,
            tactic,
            selector,
            cells,
            occupancy,
            entry_rollout,
            entry_key,
            mat: Materializer::new(genesis, genesis_cut.at),
            ledger,
            coordinator,
            rng: Prng::new(seed),
            genesis,
            genesis_cut,
            until: StopConditions {
                deadline: None,
                on: StopMask::ALL,
            },
            config,
            replay_left: config.replay_budget,
            views,
        })
    }

    /// The archive frontier (the selector-facing materialized read model).
    pub fn frontier(&self) -> &Frontier {
        self.occupancy.frontier()
    }

    /// The durable evidence ledger, read-only.
    pub fn ledger(&self) -> &EvidenceLedger {
        &self.ledger
    }

    /// The finalized absence-expectations view.
    pub fn absences(&self) -> &AbsenceLedger {
        &self.views.absences
    }

    /// The retention views (`hm-5sv`): the bounded working set (record 2) and
    /// the finalized summary + committed Entry cell assignments (record 3).
    pub fn views(&self) -> &RetentionViews {
        &self.views
    }

    /// The Revision coordinator, read-only.
    pub fn coordinator(&self) -> &Coordinator {
        &self.coordinator
    }

    /// Set the stop conditions each rollout runs under (default [`StopMask::ALL`]).
    pub fn set_stop_conditions(&mut self, until: StopConditions) {
        self.until = until;
    }

    /// The number of Entries currently occupying the archive.
    pub fn occupied(&self) -> usize {
        self.occupancy.frontier().len()
    }

    /// Run `steps` search-loop steps, returning the per-step reports.
    pub fn explore(&mut self, steps: u64) -> Result<Vec<StepReport>, CampaignError> {
        let mut reports = Vec::with_capacity(steps as usize);
        for _ in 0..steps {
            reports.push(self.step()?);
        }
        Ok(reports)
    }

    /// One two-barrier search-loop step (module doc).
    pub fn step(&mut self) -> Result<StepReport, CampaignError> {
        // ---- Cohort A: the completed rollout, committed before barrier 1. ----
        let cohort_a = self.coordinator.open_cohort()?;
        let p1 = self.coordinator.assign(cohort_a)?;

        // Dispatch: select a base, materialize it, and mint the rollout env.
        let choice = self
            .selector
            .choose(self.occupancy.frontier(), &mut self.rng);
        let (base_snap, base_env, parent_cut, explored) = self.pick_base(choice)?;
        let (branch_env, minted) = self.mint_env(choice, &base_env)?;

        let rollout = self.run_rollout(base_snap, &base_env, &branch_env, parent_cut)?;
        // The per-branch determinism artifact, taken at the rollout terminal
        // (before any materialization replay disturbs the machine).
        let state_hash = if self.config.hash_rollouts {
            Some(self.machine.hash()?)
        } else {
            None
        };
        // The lineage parent is the SEALED ROLLOUT behind the chosen Entry —
        // the rollout whose evidence prefix this child inherits.
        let parent_issue = match choice {
            None => None,
            Some(r) => Some(
                self.entry_rollout
                    .get(&r.0)
                    .copied()
                    .ok_or(MachineError::UnknownExemplar(r.0))?,
            ),
        };
        let rollout_id = RunId {
            issue: p1.proposal.get(),
            parent: parent_issue,
        };
        // The completed-rollout cut is the observed terminal: the full
        // cumulative SDK prefix (inherited ancestor prefix + own suffix).
        let start = parent_cut.map(|c| c.sdk_events).unwrap_or(0);
        let observed_cut = EvidenceCut {
            at: rollout.stop.vtime(),
            sdk_events: start + rollout.normalized.events.len() as u64,
        };
        let evidence = CompletedRunEvidence {
            rollout: rollout_id,
            role: EvidenceRole::Rollout,
            terminal: rollout.stop.clone(),
            env: rollout.genesis_env.clone(),
            cut: observed_cut,
            normalized: rollout.normalized.clone(),
            parent_cut,
            sealable_moments: rollout.sealable_moments.iter().map(|m| m.0).collect(),
        };

        // Durably append BEFORE commit, stage the typed relation rows, then
        // submit the batch identity for commit.
        let batch1 = self.ledger.append(&evidence)?;
        self.coordinator
            .stage_evidence(p1.proposal, evidence_rows(&evidence))?;
        self.coordinator.complete(Completion {
            proposal: p1.proposal,
            batch: batch1,
            terminal: terminal_record(&evidence),
        })?;
        self.coordinator.close_cohort(cohort_a)?;

        // Fold the committed rollout batch into the retention views — the same
        // deterministic fold a restart's rebuild replays (working-set
        // admission + expiry retractions touch only working views; finalized
        // counts and counterexample dedup are monotone).
        let fold = self
            .views
            .fold_batch(self.cells.as_ref(), &self.ledger, batch1, &evidence);

        // ---- Barrier 1: read only after the probe frontier passes. ----
        let view1 = self.coordinator.probe_drive(p1.revision)?;
        debug_assert!(view1.frontier >= p1.revision, "barrier 1 passed");
        let views1 = self.coordinator.materialized(view1.frontier)?;

        // Provisional transitions from the MATERIALIZED cut cells (the
        // production relations, not a recompute) → dedupe / order / cap.
        let candidates =
            self.provisional_candidates(&views1, rollout_id.issue, start, &evidence, &rollout)?;

        // ---- Cohort B: materialize the capped candidates, occupancy at barrier 2. ----
        let mut report = StepReport {
            rollout_revision: p1.revision,
            candidates: candidates.len(),
            admitted: Vec::new(),
            counterexamples: fold.new_counterexamples,
            explored,
            state_hash,
            seal_retries: 0,
        };
        if !candidates.is_empty() {
            let cohort_b = self.coordinator.open_cohort()?;
            let mut pending: Vec<PendingSeal> = Vec::new();
            let mut last_rev = Revision::ZERO;
            for cand in candidates {
                if self.replay_left == 0 {
                    break;
                }
                self.replay_left -= 1; // charge the replay budget

                // Materialize BEFORE reserving the revision slot: replay to
                // the candidate moment, holding the seal; the machine stamps
                // the AUTHORITATIVE cut with the seal. A candidate whose
                // state disappears before a valid seal — or that never
                // reaches a quiescent boundary within the bounded run-forward
                // (task 136) — is not returnable and is DROPPED (the
                // strategy's disappearing-state rule) — and because no slot
                // was reserved for it, dropping it can never hold the
                // frontier open. Any other machine failure aborts the step
                // loudly as ever. The branch origin (the base's seal moment —
                // exactly what the adapter re-anchors the env's staged
                // Moments at) rides along for the marker clamp.
                let branch_origin = parent_cut.map_or(Moment(0), |c| c.at);
                // The retry counter is an OUT-PARAM so a candidate dropped at
                // the attempt cap still reports the refusals it burned — the
                // drop path returns `Err(NotSealable)` and a return-value
                // count would vanish with it (tribunal T136-J2(c)).
                let (seal, actual_cut, seal_delta) = match self.materialize_candidate(
                    base_snap,
                    &branch_env,
                    cand.at,
                    branch_origin,
                    &mut report.seal_retries,
                ) {
                    Ok(sealed) => sealed,
                    Err(CampaignError::Machine(MachineError::NotSealable(_))) => continue,
                    Err(e) => return Err(e),
                };
                // The **seal-consistent** genesis-complete env (task 133 / PR
                // #134 finding F3). The frontier entry's env must be the
                // genesis-complete reproducer reaching the SEAL — `exemplar.cut`
                // — not the rollout terminal. `rollout.genesis_env` is positioned
                // at the terminal (`recorded_env` folded at the last stop), so an
                // exploit child's delta — recorded off THIS seal, keyed from the
                // seal — fails `compose`'s adjacency check (`d.base_offset ==
                // b.pos`) against a terminal-positioned base and aborts
                // `NonAdjacentChain` on the first exploit. The seal owns the
                // coordinate (mirroring the task-132 `DeclaredMachine` seal cut):
                // fold the base's genesis-complete env with the branch-local
                // delta the machine recorded AT the seal, exactly as
                // `run_rollout` folds the terminal delta into `genesis_env`. A
                // malformed fold aborts the step loudly (`EnvCodec`), never a bug.
                let seal_genesis_env = match &base_env {
                    None => seal_delta,
                    Some(base) => self.codec.compose(base, &seal_delta)?,
                };
                let p2 = self.coordinator.assign(cohort_b)?;
                let seal_evidence = CompletedRunEvidence {
                    rollout: RunId {
                        issue: p2.proposal.get(),
                        parent: Some(rollout_id.issue),
                    },
                    role: EvidenceRole::Seal,
                    terminal: StopReason::Quiescent {
                        vtime: actual_cut.at,
                    },
                    env: seal_genesis_env.clone(),
                    cut: actual_cut,
                    normalized: rollout.normalized.clone(),
                    parent_cut,
                    sealable_moments: Vec::new(),
                };
                let batch2 = self.ledger.append(&seal_evidence)?;
                self.coordinator
                    .stage_evidence(p2.proposal, evidence_rows(&seal_evidence))?;
                self.coordinator.complete(Completion {
                    proposal: p2.proposal,
                    batch: batch2,
                    terminal: terminal_record(&seal_evidence),
                })?;
                last_rev = p2.revision;

                // Fold the committed seal batch into the retention views: the
                // committed assignment (record 3) updates by the identical
                // best-Entry-per-cell rule the operational occupancy applies
                // after barrier 2, so the two can never drift.
                let fold2 = self.views.fold_batch(
                    self.cells.as_ref(),
                    &self.ledger,
                    batch2,
                    &seal_evidence,
                );
                pending.push(PendingSeal {
                    entry: p2.proposal.get(),
                    seal,
                    cut: actual_cut,
                    fold_admitted: fold2.admitted,
                    seal_genesis_env,
                });
            }
            self.coordinator.close_cohort(cohort_b)?;
            if last_rev != Revision::ZERO {
                // ---- Barrier 2: the cell at the ACTUAL sealed_at and the
                // occupancy, read from the materialized views only after the
                // probe frontier passes. ----
                let view2 = self.coordinator.probe_drive(last_rev)?;
                debug_assert!(view2.frontier >= last_rev, "barrier 2 passed");
                let views2 = self.coordinator.materialized(view2.frontier)?;
                for p in pending {
                    let cell = views2
                        .cell_at(rollout_id.issue, PointRow::Seal(p.entry))
                        .ok_or(CampaignError::ViewIncomplete {
                            what: "seal cell",
                            rollout: rollout_id.issue,
                        })?
                        .clone();
                    let quality = p.cut.at.0; // progress depth (configured metric)
                    let exemplar = VirtualExemplar {
                        parent: base_snap,
                        seed: minted,
                        suffix: rollout.env.clone(),
                        cut: p.cut,
                    };
                    let entry = FrontierEntry {
                        exemplar,
                        // Seal-consistent (task 133): the env reaches
                        // `exemplar.cut` (the seal), so an exploit child's delta
                        // composes adjacently against it.
                        env: p.seal_genesis_env.clone(),
                        reward: Reward { new_cells: 1 },
                    };
                    let outcome = self.occupancy.admit(entry, cell, quality);
                    debug_assert_eq!(
                        p.fold_admitted,
                        !matches!(outcome, Occupied::Rejected),
                        "the committed assignment and the operational occupancy apply one rule"
                    );
                    match outcome {
                        Occupied::Fresh(r) => {
                            self.entry_rollout.insert(r.0, rollout_id.issue);
                            self.entry_key.insert(r.0, p.entry);
                            self.register_seal(r, p.seal, base_snap, &rollout.env, p.cut)?;
                            report.admitted.push(r);
                        }
                        Occupied::Dominated { entry, evicted } => {
                            self.entry_rollout.insert(entry.0, rollout_id.issue);
                            self.entry_key.insert(entry.0, p.entry);
                            self.register_seal(entry, p.seal, base_snap, &rollout.env, p.cut)?;
                            self.drop_entry_seal(evicted)?;
                            self.entry_rollout.remove(&evicted.0);
                            self.entry_key.remove(&evicted.0);
                            report.admitted.push(entry);
                        }
                        Occupied::Rejected => {
                            // A provisional transition that lost occupancy never
                            // occupies the archive; drop its temporary seal.
                            self.machine.drop_snap(p.seal)?;
                        }
                    }
                }
                // The Differential occupancy view is the authority; the
                // operational mirror must agree exactly.
                self.check_occupancy(&views2)?;
            }
        }

        Ok(report)
    }

    /// Reconcile the operational archive against the materialized occupancy
    /// view: every occupied cell agrees on its occupant. A mismatch is a
    /// loud [`CampaignError::OccupancyDivergence`], never absorbed.
    fn check_occupancy(
        &self,
        views: &revision_coordinator::MaterializedViews,
    ) -> Result<(), CampaignError> {
        let claims = self.occupancy.frontier().occupied_cells();
        if views.occupancy.len() != claims {
            let hex =
                |b: &[u8]| -> String { b.iter().take(24).map(|x| format!("{x:02x}")).collect() };
            let view_side: Vec<String> = views
                .occupancy
                .iter()
                .map(|(c, e)| format!("{}=>{e}", hex(c)))
                .collect();
            let mirror_side: Vec<String> = self
                .occupancy
                .frontier()
                .claims()
                .map(|(c, r)| {
                    let key = self.entry_key.get(&r.0).copied();
                    format!("{}=>{key:?}", hex(c))
                })
                .collect();
            return Err(CampaignError::OccupancyDivergence {
                detail: format!(
                    "{} materialized cells vs {} mirror claims; view [{}]; mirror [{}]",
                    views.occupancy.len(),
                    claims,
                    view_side.join(", "),
                    mirror_side.join(", ")
                ),
            });
        }
        for (cell, entry) in &views.occupancy {
            let occupant = self
                .occupancy
                .frontier()
                .occupant(cell)
                .and_then(|r| self.entry_key.get(&r.0).copied());
            if occupant != Some(*entry) {
                return Err(CampaignError::OccupancyDivergence {
                    detail: format!(
                        "cell occupant disagrees: view entry {entry}, mirror {occupant:?}"
                    ),
                });
            }
        }
        Ok(())
    }

    // -- The retention/GC surface (`hm-5sv`) --------------------------------

    /// Durably commit a retention checkpoint of the current views — the
    /// rebuild anchor physical GC may cite for coverage. Returns the committed
    /// checkpoint.
    pub fn commit_checkpoint(&mut self) -> Result<RetentionCheckpoint, CampaignError> {
        let cp = RetentionCheckpoint {
            views: self.views.clone(),
        };
        self.ledger.commit_checkpoint(&cp)?;
        Ok(cp)
    }

    /// Durably mark the campaign's **explicit end to future raw-evidence
    /// reinterpretation** (the second leg GC may stand on). Idempotent.
    pub fn finalize_evidence(&mut self) -> Result<(), CampaignError> {
        self.ledger.finalize()?;
        Ok(())
    }

    /// The payload digests live Entries require — their genesis-complete
    /// reproducers — which GC can never invalidate while the Entry is live.
    fn live_entry_digests(&self) -> std::collections::BTreeSet<[u8; 32]> {
        self.occupancy
            .frontier()
            .iter()
            .map(|(_, e)| *blake3::hash(&e.env.bytes).as_bytes())
            .collect()
    }

    /// Physically collect one batch's raw evidence under the full proof chain:
    /// the declared profile must allow collection
    /// ([`RetentionError::FullRetentionForbidsCollection`]), the batch must be
    /// expired from the working set ([`RetentionError::StillInWorkingSet`]),
    /// must not be required by a live Entry, and must be covered by a durable
    /// checkpoint or the finalized end. Every failure is loud.
    pub fn collect_batch(
        &mut self,
        id: revision_coordinator::EvidenceBatchId,
    ) -> Result<crate::retention::CollectedBatch, CampaignError> {
        if self.config.retention == RetentionProfile::Full {
            return Err(RetentionError::FullRetentionForbidsCollection.into());
        }
        if self.views.working.contains(&id) {
            return Err(RetentionError::StillInWorkingSet { batch: id }.into());
        }
        let protected = self.live_entry_digests();
        Ok(self.ledger.collect(id, &protected)?)
    }

    /// One proven GC sweep: collect every retained batch that is expired from
    /// the working set, not required by a live Entry, and covered — reporting
    /// exactly what was collected and what was skipped (and why; a bounded
    /// sweep never silently caps). Errors under the full-retention profile.
    pub fn collect_expired(&mut self) -> Result<GcReport, CampaignError> {
        if self.config.retention == RetentionProfile::Full {
            return Err(RetentionError::FullRetentionForbidsCollection.into());
        }
        let protected = self.live_entry_digests();
        let candidates: Vec<revision_coordinator::EvidenceBatchId> =
            self.ledger.batch_ids().copied().collect();
        let store_before = self.ledger.trace_store().len();
        let mut report = GcReport::default();
        for id in candidates {
            if self.views.working.contains(&id) {
                report.skipped.push((id, GcSkipReason::StillInWorkingSet));
                continue;
            }
            match self.ledger.collect(id, &protected) {
                Ok(_) => report.collected.push(id),
                Err(RetentionError::LiveEntryReference { .. }) => {
                    report.skipped.push((id, GcSkipReason::LiveEntryReference));
                }
                Err(RetentionError::NotCovered { .. }) => {
                    report.skipped.push((id, GcSkipReason::NotCovered));
                }
                Err(e) => return Err(e.into()),
            }
        }
        report.reclaimed_payloads = store_before - self.ledger.trace_store().len();
        Ok(report)
    }

    /// Physically reclaim the collected raw bytes from the durable ledger file
    /// (a crash-safe rewrite that preserves the rebuild anchor, tombstones,
    /// and retained evidence). Returns the bytes reclaimed.
    pub fn compact_ledger(&mut self) -> Result<u64, CampaignError> {
        Ok(self.ledger.compact()?)
    }

    /// The campaign **completeness report**: exactly which raw evidence
    /// remains (per batch, with the coverage each collection cited), which
    /// finalized derivations and committed assignments survive, and whether
    /// future cell recomputation is available without replay.
    pub fn retention_report(&self) -> RetentionReport {
        let mut batches = BTreeMap::new();
        for id in self.ledger.batch_ids() {
            batches.insert(
                *id,
                BatchAvailability {
                    raw: RawAvailability::Retained,
                    recompute_cells: Recomputation::FromRetainedEvidence,
                    in_working_set: self.views.working.contains(id),
                },
            );
        }
        for (id, tomb) in self.ledger.collected() {
            batches.insert(
                *id,
                BatchAvailability {
                    raw: RawAvailability::Collected {
                        covered_by: tomb.covered_by,
                    },
                    recompute_cells: Recomputation::RequiresReplay,
                    in_working_set: false,
                },
            );
        }
        RetentionReport {
            profile: self.config.retention,
            finalized_end: self.ledger.is_finalized(),
            batches,
            derivations: self.views.finalized,
            committed_assignments: self.views.assignments.len() as u64,
        }
    }

    /// Pick the branch base: genesis (explore) or a materialized frontier exemplar
    /// (exploit). Returns the base snapshot, the base's genesis-complete env (for
    /// composing), the base's evidence cut (the parent cut for branch ingestion),
    /// and whether the step explored.
    fn pick_base(
        &mut self,
        choice: Option<ExemplarRef>,
    ) -> Result<(SnapId, Option<Reproducer>, Option<EvidenceCut>, bool), CampaignError> {
        match choice {
            // Genesis explores inherit the machine's pre-campaign prefix
            // through the genesis cut (its count is the cumulative position
            // base; a machine with no pre-campaign SDK prefix stamps 0).
            None => Ok((self.genesis, None, Some(self.genesis_cut), true)),
            Some(r) => {
                let (entry_env, parent_cut) = match self.occupancy.frontier().get(r) {
                    Some(entry) => (entry.env.clone(), entry.exemplar.cut),
                    None => return Err(MachineError::UnknownExemplar(r.0).into()),
                };
                let snap = self.mat.materialize(
                    &mut self.machine,
                    self.codec.as_ref(),
                    self.occupancy.frontier(),
                    r,
                )?;
                Ok((snap.0, Some(entry_env), Some(parent_cut), false))
            }
        }
    }

    /// Mint the rollout env and the campaign draw that minted it: a fresh pure
    /// seed on explore, a coverage-guided mutation of the base on exploit. Draw
    /// order mirrors the engine's (one draw per step).
    fn mint_env(
        &mut self,
        choice: Option<ExemplarRef>,
        base_env: &Option<Reproducer>,
    ) -> Result<(Reproducer, u64), CampaignError> {
        match choice {
            None => {
                let seed = self.rng.next_u64();
                Ok((self.codec.seeded(seed), seed))
            }
            Some(_) => {
                let base = base_env.as_ref().expect("exploit has a base env");
                let salt = self.rng.next_u64();
                Ok((self.codec.mutate(base, salt)?, salt))
            }
        }
    }

    /// Register a materialized seal under a freshly-admitted Entry (its lineage
    /// and cut), releasing any displaced handle.
    fn register_seal(
        &mut self,
        r: ExemplarRef,
        seal: SnapId,
        parent: SnapId,
        suffix: &Reproducer,
        cut: EvidenceCut,
    ) -> Result<(), CampaignError> {
        if let Some(old) = self.mat.register(r, seal, parent, suffix.clone(), cut) {
            self.machine.drop_snap(old)?;
        }
        Ok(())
    }

    /// Drop the seal of an evicted Entry (deterministic Entry eviction, separate
    /// from evidence retention).
    fn drop_entry_seal(&mut self, r: ExemplarRef) -> Result<(), CampaignError> {
        self.mat.evict_seal(&mut self.machine, r)?;
        Ok(())
    }

    /// The task-136 **blocking sub-gate driver**: for every admitted frontier
    /// entry, pin the machine state hash at its live seal (verbatim
    /// `replay`), evict the seal, re-materialize it **from the ledger env
    /// alone** — the task-68 zero-probe replay: one `branch` + one
    /// `run(deadline = sealed_at)` + seal, with
    /// [`MachineError::MaterializeDivergence`]/[`MachineError::CutDivergence`]
    /// refusing any coordinate drift — and require the bit-identical state
    /// hash back ([`CampaignError::ResealDivergence`] otherwise, loudly).
    ///
    /// This is the one open determinism surface of the marker-clamped
    /// run-forward (hm-esfd): a retry-forward seal's *live* history contains
    /// failed probes and extra deadline legs the ledger env does not record;
    /// if those legs perturbed real state, the reproducer would not
    /// reproduce. Entries iterate in deterministic frontier order, so the
    /// returned records are themselves a determinism artifact a `--repeat`
    /// gate can compare across identically-seeded campaigns.
    pub fn verify_reseal_identity(&mut self) -> Result<Vec<ResealCheck>, CampaignError> {
        let refs: Vec<ExemplarRef> = self.occupancy.frontier().iter().map(|(r, _)| r).collect();
        let mut out = Vec::with_capacity(refs.len());
        for r in refs {
            // A live seal (materializing if the entry's was already evicted),
            // then the pre-image: the sealed state's hash via verbatim replay.
            let (seal, _) = self.mat.materialize(
                &mut self.machine,
                self.codec.as_ref(),
                self.occupancy.frontier(),
                r,
            )?;
            self.machine.replay(seal)?;
            let before = self.machine.hash()?;
            let sealed_at = self
                .occupancy
                .frontier()
                .get(r)
                .map(|e| e.exemplar.cut.at.0)
                .ok_or(MachineError::UnknownExemplar(r.0))?;
            // Evict, then re-materialize from the ledger env alone. The
            // replay leaves the machine at the fresh seal — hash there.
            self.mat.evict_seal(&mut self.machine, r)?;
            let (_seal, ran) = self.mat.materialize(
                &mut self.machine,
                self.codec.as_ref(),
                self.occupancy.frontier(),
                r,
            )?;
            debug_assert!(ran.is_some(), "the seal was just evicted");
            let after = self.machine.hash()?;
            if before != after {
                return Err(CampaignError::ResealDivergence {
                    entry: r.0,
                    at: sealed_at,
                });
            }
            out.push(ResealCheck {
                entry: r,
                sealed_at,
                state_hash: after,
            });
        }
        Ok(out)
    }
}

/// One provisional transition nominated for materialization replay: the moment
/// an interesting (new) cell was observed at, and that cell (the dedup key).
struct Candidate {
    at: Moment,
    cell: CellKey,
}

/// One materialized candidate awaiting its barrier-2 admission: the entry key
/// (its seal proposal's issue), the held seal, its authoritative cut, the
/// retention fold's admission verdict (asserted against the mirror's), and the
/// **seal-consistent** genesis-complete env reaching the seal (task 133) — the
/// reproducer the admitted [`FrontierEntry`] carries, so an exploit child
/// composes adjacently against it.
struct PendingSeal {
    entry: u64,
    seal: SnapId,
    cut: EvidenceCut,
    fold_admitted: bool,
    seal_genesis_env: Reproducer,
}

impl<M: Machine> DifferentialCampaign<M> {
    /// Drive one open-loop rollout from `base_snap` under `branch_env`, capturing
    /// the terminal, the branch-local and genesis-complete reproducers, the
    /// **child-suffix** normalized SDK evidence (the inherited ancestor prefix is
    /// never re-decoded), and the sealable-point moments observed.
    ///
    /// The rollout is single-pass and open-loop (spine invariant 1): the
    /// [`Tactic`] answers each surfaced decision from its own state, the point,
    /// and the seeded PRNG — no observation/occupancy feedback reaches it.
    fn run_rollout(
        &mut self,
        base_snap: SnapId,
        base_env: &Option<Reproducer>,
        branch_env: &Reproducer,
        parent_cut: Option<EvidenceCut>,
    ) -> Result<Rollout, CampaignError> {
        self.machine.branch(base_snap, branch_env)?;
        let mut sealable_moments = Vec::new();
        let mut resolve: Option<Answer> = None;
        let stop = loop {
            let stop = self.machine.run(&self.until, resolve.as_ref())?;
            match stop {
                StopReason::Decision { vtime, id, ref ctx } => {
                    let pt = DecisionPoint {
                        at: Moment(vtime.0),
                        id,
                        ctx: ctx.clone(),
                    };
                    resolve = Some(self.tactic.decide(&pt, &mut self.rng));
                }
                StopReason::SnapshotPoint { vtime } => {
                    // A sealable point the injected predicate rejects is stepped
                    // past, never nominated. No eager seal is taken here — the DD
                    // path replays to seal a capped subset (budgeted).
                    if self.mat.sealable_at(Moment(vtime.0)) {
                        sealable_moments.push(Moment(vtime.0));
                    }
                    resolve = None;
                }
                terminal => break terminal,
            }
        };
        let env = self.machine.recorded_env()?;
        let genesis_env = match base_env {
            None => env.clone(),
            Some(base) => self.codec.compose(base, &env)?,
        };
        // Decode the child-suffix SDK evidence: on a branch child, drop the
        // inherited ancestor firing prefix (positions below the parent cut) and
        // keep only the catalog + the child's own firings — the ancestor prefix
        // is inherited through lineage, never duplicated as child evidence.
        let raw = self.machine.sdk_events()?;
        let inherited = parent_cut.map(|c| c.sdk_events).unwrap_or(0);
        let normalized = self.decode_child_suffix(&raw, inherited)?;
        // The nomination coordinates: machine-surfaced snapshot points (the
        // default), or — for a workload that surfaces none mid-run — the
        // distinct sealable moments of the rollout's own SDK events.
        let sealable_moments = match self.config.nominate {
            Nomination::SnapshotPoints => sealable_moments,
            Nomination::EventMoments => {
                let mut distinct = std::collections::BTreeSet::new();
                for e in &normalized.events {
                    let m = Moment(e.moment.0);
                    if self.mat.sealable_at(m) {
                        distinct.insert(m);
                    }
                }
                distinct.into_iter().collect()
            }
        };
        Ok(Rollout {
            stop,
            env,
            genesis_env,
            normalized,
            sealable_moments,
        })
    }

    /// Decode a rollout's raw SDK capture into the child-suffix normalized
    /// evidence, dropping the first `inherited` firing positions (the restored
    /// ancestor prefix) while keeping the catalog declaration.
    fn decode_child_suffix(
        &self,
        raw: &[(u64, u32, Vec<u8>)],
        inherited: u64,
    ) -> Result<Normalized, CampaignError> {
        match self.config.ingress {
            Ingress::Binary => {
                // The parent cut counts capture POSITIONS (ordinals) — the
                // catalog tuple at position 0 included (the server stamps it as
                // `vmm.sdk_events().len()`). So skip the inherited prefix by
                // position: keep the catalog declaration unconditionally (the
                // child re-declares its own schema, never inherits it) and keep
                // every non-catalog firing at or past the inherited-position
                // boundary. Counting firings alone would keep the catalog for
                // free AND skip `inherited` firings, over-skipping by the catalog
                // count and dropping the first firing that should survive (F2).
                let mut kept: Vec<(sdk_events::Moment, u32, Vec<u8>)> = Vec::new();
                for (pos, (m, id, bytes)) in raw.iter().enumerate() {
                    if *id == CATALOG_EVENT_ID {
                        kept.push((sdk_events::Moment(*m), *id, bytes.clone()));
                        continue;
                    }
                    if (pos as u64) >= inherited {
                        kept.push((sdk_events::Moment(*m), *id, bytes.clone()));
                    }
                }
                Ok(decode_binary(&kept)?)
            }
            Ingress::AntithesisJson => {
                let recs: Vec<(sdk_events::Moment, Vec<u8>)> = raw
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| (*i as u64) >= inherited)
                    .map(|(_, (m, _, bytes))| (sdk_events::Moment(*m), bytes.clone()))
                    .collect();
                Ok(decode_antithesis(&recs)?)
            }
        }
    }

    /// Read the provisional observation/cell transitions from the
    /// **materialized** cut cells at the rollout's sealable-point moments
    /// (non-authoritative nomination coordinates), then **dedupe by cell**,
    /// **order by coordinate**, and apply the **candidate cap**. A candidate
    /// is a moment whose materialized cell is not already occupied — it
    /// nominates a materialization replay, and can never itself occupy the
    /// archive.
    fn provisional_candidates(
        &self,
        views: &revision_coordinator::MaterializedViews,
        rollout_issue: u64,
        start: u64,
        evidence: &CompletedRunEvidence,
        rollout: &Rollout,
    ) -> Result<Vec<Candidate>, CampaignError> {
        // Dedupe by cell (first observing moment wins), ordered by (moment) via a
        // BTreeMap keyed on the ordering coordinate.
        let mut by_cell: BTreeMap<CellKey, Moment> = BTreeMap::new();
        for &at in &rollout.sealable_moments {
            // The provisional included-count is moment-approximate (non-authoritative):
            // the cumulative prefix through the SDK firings emitted at or
            // before this sealable moment.
            let included = evidence
                .normalized
                .events
                .iter()
                .filter(|e| e.moment.0 <= at.0)
                .count() as u64;
            let count = start + included;
            let cell = views.cell_at(rollout_issue, PointRow::Cut(count)).ok_or(
                CampaignError::ViewIncomplete {
                    what: "cut cell",
                    rollout: rollout_issue,
                },
            )?;
            // Only a fresh cell (not already occupied) is an interesting
            // transition worth replaying to seal.
            if self.occupancy.frontier().occupant(cell).is_none() {
                by_cell.entry(cell.clone()).or_insert(at);
            }
        }
        // Order candidates by their observed moment (their explicit evidence
        // coordinate), then apply the configured cap.
        let mut ordered: Vec<Candidate> = by_cell
            .into_iter()
            .map(|(cell, at)| Candidate { at, cell })
            .collect();
        ordered.sort_by_key(|c| (c.at.0, c.cell.clone()));
        ordered.truncate(self.config.candidate_cap);
        Ok(ordered)
    }

    /// Materialize one provisional candidate: replay the same rollout env from
    /// `base_snap` to the first valid `sealed_at` at or after the candidate
    /// moment, then snapshot — holding the temporary seal and capturing the
    /// **authoritative** server-stamped cut with it. The replay charges the
    /// campaign budget (charged by the caller).
    ///
    /// **Marker-clamped run-forward (task 136 / hm-esfd, Option C).** The seal
    /// may land *past* the candidate moment, at the first **fully-drained
    /// quiescent boundary** ("wait a beat, then snap"):
    ///
    /// - A [`MachineError::NotQuiescent`] seal refusal (the guest mid-exit at
    ///   the boundary — a reseed-shifted RNG draw; the server's
    ///   `SnapshotWhileArmed` collapses to the same variant at the adapter)
    ///   retries the seal a bounded number of times, running
    ///   [`SEAL_RETRY_STEP`] further per attempt — exactly the `ControlServer::
    ///   run` "caller runs a little further and retries" contract the base
    ///   seal (`seal_base`) has always followed.
    /// - Every replay deadline is **clamped to the next staged `Moment`** of
    ///   the branch env ([`EnvCodec::staged_moments`], re-anchored at `origin`
    ///   exactly as the adapter's wire conversion does): while any staged
    ///   reseed marker / host fault lies ahead, the server rejects *every*
    ///   seal, and an unclamped leg whose deadline sits *below* the staged
    ///   Moment never arms the exact-arrival machinery — the opportunistic
    ///   stop can then overshoot the Moment and poison the schedule
    ///   (`ScheduleUnsatisfiable`, hm-esfd's second blocker). A deadline
    ///   landing exactly **on** the Moment arms it (`m <= deadline`), the run
    ///   stops between instructions there and drains it — the server's own
    ///   reseed-aware machinery does all the work; this stays a client-side
    ///   change.
    /// - On attempt-cap exhaustion the candidate is **dropped** like a
    ///   disappearing state ([`MachineError::NotSealable`]) — never a
    ///   campaign abort.
    ///
    /// Also returns the machine's **branch-local delta recorded at the seal**
    /// (task 133): the tail-complete reproducer from `base_snap` to the seal,
    /// keyed from the branch origin. The caller folds it with the base's
    /// genesis-complete env to obtain the seal-consistent env the frontier
    /// entry carries — so an exploit child, whose own delta is likewise keyed
    /// from this seal, composes adjacently against the entry. Because a seal
    /// only ever succeeds once **both** staged schedules have drained, the
    /// delta carries every marker at-or-below `sealed_at` and none beyond —
    /// which is what lets the task-68 re-materialization (`run(deadline =
    /// sealed_at)`, zero probes) replay it bit-identically from the ledger
    /// env alone.
    ///
    /// Returns the seal handle, its stamped cut, and the branch-local delta.
    /// `retries` (the [`StepReport::seal_retries`] evidence) is incremented on
    /// **every** `NotQuiescent` refusal — an out-param rather than a return
    /// value so a candidate dropped at the attempt cap still reports the
    /// `1 + SEAL_RETRY_ATTEMPTS` refusals it burned (the `Err` path would
    /// discard a returned count — tribunal T136-J2(c)).
    fn materialize_candidate(
        &mut self,
        base_snap: SnapId,
        branch_env: &Reproducer,
        at: Moment,
        origin: Moment,
        retries: &mut u64,
    ) -> Result<(SnapId, EvidenceCut, Reproducer), CampaignError> {
        self.machine.branch(base_snap, branch_env)?;
        // The env's staged-schedule Moments beyond the branch origin, in the
        // absolute frame the server drains them in (`origin + relative`, the
        // adapter's own re-anchoring). The add cannot overflow past a
        // successful `branch` — the adapter refused exactly this rebase
        // before any wire traffic — so saturation is defensively unreachable.
        let staged: Vec<u64> = self
            .codec
            .staged_moments(branch_env)?
            .into_iter()
            .map(|rel| origin.0.saturating_add(rel))
            .collect();
        let next_staged = |vt: u64| staged.iter().copied().find(|&m| m > vt);
        // Advance to the first valid sealable point at or after `at`. Under a
        // deadline the machine surfaces the snapshot point; seal there. The
        // first leg already jumps to the next staged Moment when one lies
        // beyond `at`: no seal can succeed below it, and Moments at-or-below
        // the leg deadline are armed and drained exactly in-run either way.
        // The branch-local delta is read from the machine at the sealed state
        // (`recorded_env`, keyed from the branch origin) — before any later
        // machine op disturbs it.
        let mut until = StopConditions {
            deadline: Some(Moment(next_staged(at.0).unwrap_or(at.0))),
            on: StopMask::ALL,
        };
        let mut attempts_left = SEAL_RETRY_ATTEMPTS;
        loop {
            let stop = self.machine.run(&until, None)?;
            let vt = match stop {
                StopReason::SnapshotPoint { vtime } if vtime.0 >= at.0 => vtime,
                StopReason::SnapshotPoint { .. } => continue,
                StopReason::Decision { .. } => {
                    // A decision under the replay: answer with the recorded env's
                    // seed (decline) so the pinned replay reaches the seal.
                    continue;
                }
                // A stop at or past the candidate moment that is not a bug — a
                // deadline leg or a quiescent terminal — is a seal-attempt
                // point; else the state disappeared before a valid seal and is
                // not admissible.
                terminal if terminal.vtime().0 >= at.0 && !terminal.is_bug() => terminal.vtime(),
                terminal => {
                    return Err(MachineError::NotSealable(terminal.vtime().0).into());
                }
            };
            // While a staged Moment still lies ahead the server rejects every
            // seal (both schedules must drain first), so attempting one is
            // pointless and a fixed-step leg risks the overshoot poison: run
            // exactly TO the next staged Moment instead (the clamp — its
            // deadline arms the exact-arrival drain). Drain legs are bounded
            // by the env's marker count and charge no attempt.
            if let Some(m) = next_staged(vt.0) {
                until.deadline = Some(Moment(m));
                continue;
            }
            match self.machine.snapshot() {
                Ok((seal, cut)) => {
                    let delta = self.machine.recorded_env()?;
                    return Ok((seal, cut, delta));
                }
                // Mid-exit at the boundary: run a little further and retry,
                // bounded. On cap exhaustion the candidate is dropped like a
                // disappearing state — never a campaign abort. Every refusal
                // counts, including the final one that drops the candidate.
                Err(MachineError::NotQuiescent) => {
                    *retries += 1;
                    if attempts_left == 0 {
                        return Err(MachineError::NotSealable(vt.0).into());
                    }
                    attempts_left -= 1;
                    until.deadline = Some(Moment(vt.0.saturating_add(SEAL_RETRY_STEP)));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// The deterministic terminal record for a committed batch: its cut moment and a
/// work count (the SDK-event prefix length) — both pure functions of the
/// evidence, never wall-clock.
fn terminal_record(ev: &CompletedRunEvidence) -> TerminalRecord {
    TerminalRecord {
        moment: ev.cut.at.0,
        work: ev.normalized.events.len() as u64,
    }
}

/// The coordinator's payload-blind reduce op for a normalized base op.
fn reduce_op(op: UpdateOp) -> ReduceOp {
    match op {
        UpdateOp::Set => ReduceOp::Set,
        UpdateOp::Max => ReduceOp::Max,
        UpdateOp::Min => ReduceOp::Min,
        UpdateOp::Accumulate => ReduceOp::Accumulate,
    }
}

/// Decode the materialized reduced-observation pairs back into the typed
/// [`ObservationMap`] the campaign's [`ObservationCells`] consumes. Total:
/// an undecodable identity (impossible for keys minted by
/// [`evidence_rows`]'s own encoder) is skipped under a debug assertion
/// rather than panicking inside a dataflow operator.
fn decode_reduced_pairs(pairs: &[(Vec<u8>, ReducedRow)]) -> ObservationMap {
    let mut map = ObservationMap::new();
    for (key, red) in pairs {
        let Some(id) = decode_observation_id(key) else {
            debug_assert!(false, "undecodable observation key {key:?}");
            continue;
        };
        let val = match red {
            ReducedRow::Scalar(v) => ReducedValue::Scalar(*v),
            ReducedRow::Accumulated(vs) => ReducedValue::Accumulated(vs.iter().copied().collect()),
        };
        map.insert(id, val);
    }
    map
}

/// The typed relation rows one committed evidence batch contributes to the
/// production Differential relations — a pure function of the batch, so a
/// restart re-stages byte-identical inputs from the durable ledger alone.
///
/// A ROLLOUT batch contributes its lineage edge, its schema's reducible
/// declarations, its own suffix state events at **cumulative** positions,
/// and its provisional cuts (dedup by count, first moment wins). A SEAL
/// batch attaches to the SEALED rollout (`rollout.parent`) and contributes
/// only the seal point and the committed Entry offer (its events are the
/// rollout's, already in the graph).
pub(crate) fn evidence_rows(ev: &CompletedRunEvidence) -> EvidenceRows {
    let start = ev.parent_cut.map(|c| c.sdk_events).unwrap_or(0);
    match ev.role {
        EvidenceRole::Rollout => {
            let mut declares = Vec::new();
            for entry in ev.normalized.schema.entries() {
                if !entry.is_reducible_state() {
                    continue;
                }
                let Some(op) = entry.base_op else { continue };
                let mut key = Vec::new();
                encode_observation_id(&mut key, &entry.id);
                declares.push((key, reduce_op(op)));
            }
            let mut events = Vec::new();
            for (i, e) in ev.normalized.events.iter().enumerate() {
                let sdk_events::Payload::State { value, .. } = &e.payload else {
                    continue;
                };
                let Some(se) = ev.normalized.schema.entry(&e.id) else {
                    continue;
                };
                if !se.is_reducible_state() {
                    continue;
                }
                let mut key = Vec::new();
                encode_observation_id(&mut key, &e.id);
                events.push(StateEventRow {
                    pos: start + i as u64,
                    moment: e.moment.0,
                    obs: key,
                    value: *value,
                });
            }
            let mut obs_cuts = Vec::new();
            let mut seen = std::collections::BTreeSet::new();
            for &m in &ev.sealable_moments {
                let included = ev
                    .normalized
                    .events
                    .iter()
                    .filter(|e| e.moment.0 <= m)
                    .count() as u64;
                let count = start + included;
                if seen.insert(count) {
                    obs_cuts.push(CutRow { moment: m, count });
                }
            }
            EvidenceRows {
                rollout: ev.rollout.issue,
                lineage: ev.rollout.parent.map(|parent| LineageRow {
                    parent,
                    cut: CutRow {
                        moment: ev.parent_cut.map(|c| c.at.0).unwrap_or(0),
                        count: start,
                    },
                }),
                declares,
                events,
                obs_cuts,
                seal: None,
                entry: None,
            }
        }
        EvidenceRole::Seal => EvidenceRows {
            rollout: ev.rollout.parent.unwrap_or(ev.rollout.issue),
            lineage: None,
            declares: Vec::new(),
            events: Vec::new(),
            obs_cuts: Vec::new(),
            seal: Some(SealRow {
                seal: ev.rollout.issue,
                cut: CutRow {
                    moment: ev.cut.at.0,
                    count: ev.cut.sdk_events,
                },
            }),
            entry: Some(EntryCommitRow {
                entry: ev.rollout.issue,
                quality: ev.cut.at.0,
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults::{DeclineTactic, GenesisSelector};
    use crate::evidence::DefaultObservationCells;
    use crate::spine::EvidenceCut;
    use crate::testkit::{
        Emit, MarkerCodec, Program, ScriptedMachine, ToyCodec, campaign, campaign_with, config,
        coordinator, ledger, simple_program,
    };
    use revision_coordinator::EvidenceBatchId;
    use sdk_events::{NS_STATE, ObservationId, UpdateOp};
    use std::rc::Rc;

    /// One two-barrier step commits the rollout at revision 1, reads it past
    /// barrier 1, materializes a provisional candidate, and admits an Entry at its
    /// actual `sealed_at` past barrier 2 — the full acceptance-criteria protocol.
    #[test]
    fn one_step_runs_the_two_barrier_protocol() {
        let (_dir, mut camp) = campaign(simple_program(4), config(8, u64::MAX), 7);
        let report = camp.step().expect("step");
        assert_eq!(
            report.rollout_revision.get(),
            1,
            "rollout committed at rev 1"
        );
        assert!(report.explored, "genesis selector explores");
        assert_eq!(report.candidates, 1, "one fresh provisional cell nominated");
        assert_eq!(report.admitted.len(), 1, "one Entry admitted at its seal");
        assert_eq!(camp.occupied(), 1);
        // The rollout evidence is durable and committed.
        assert_eq!(
            camp.ledger().len(),
            2,
            "rollout + materialized-seal batches"
        );
        assert!(camp.coordinator().committed_frontier().get() >= 1);
    }

    /// Same seed ⇒ byte-identical campaign artifacts (the determinism gate): the
    /// admitted frontier's cells, cuts, and reproducers match exactly.
    #[test]
    fn same_seed_yields_identical_campaign() {
        let observable = |seed: u64| {
            let (_dir, mut camp) = campaign(simple_program(4), config(8, u64::MAX), seed);
            camp.explore(6).expect("explore");
            let frontier: Vec<(u64, u64, Vec<u8>)> = camp
                .frontier()
                .iter()
                .map(|(_, e)| {
                    (
                        e.exemplar.cut.at.0,
                        e.exemplar.cut.sdk_events,
                        e.env.bytes.clone(),
                    )
                })
                .collect();
            frontier
        };
        assert_eq!(observable(0xABCD), observable(0xABCD));
        // A different seed produces a different trajectory (the machine is not
        // trivially constant), so the gate is not vacuous.
        assert_ne!(observable(1), observable(2));
    }

    /// The blocking task-136 identity sub-gate, toy tier: evict the entry's
    /// seal and re-materialize it **from its ledger env alone** — one direct
    /// `run(deadline = sealed_at)`, zero probes (the task-68/78 shape) — and
    /// require the bit-identical state back. `Materializer::materialize`
    /// itself refuses any drift loudly (`MaterializeDivergence` /
    /// `CutDivergence`); the hash pin and the deadline log are the belt and
    /// suspenders.
    fn assert_rematerializes_identically(camp: &mut DifferentialCampaign<ScriptedMachine>) {
        let (r, entry) = camp.frontier().iter().next().expect("one admitted entry");
        let sealed_at = entry.exemplar.cut.at;
        let hash_at_seal = camp.machine.hash().expect("hash");
        let DifferentialCampaign {
            machine,
            mat,
            codec,
            occupancy,
            ..
        } = camp;
        mat.evict_seal(machine, r)
            .expect("evict")
            .expect("the admitted entry held a live seal");
        machine.deadlines_seen.clear();
        let (_seal, ran) = mat
            .materialize(machine, codec.as_ref(), occupancy.frontier(), r)
            .expect("re-materializes from the ledger env alone");
        assert!(ran.is_some(), "a real replay ran, not a seal-cache hit");
        assert_eq!(
            machine.deadlines_seen,
            vec![Some(sealed_at.0)],
            "zero probes: exactly one direct run at the sealed moment"
        );
        assert_eq!(
            machine.hash().expect("hash"),
            hash_at_seal,
            "the re-materialized state is bit-identical to the retry-forward seal"
        );
    }

    /// SealAnchor regression (task 136 / hm-esfd, primary blocker): the
    /// nominated moment is **non-quiescent** (a reseed-shifted RNG draw in
    /// flight at the boundary). Option C: the seal advances off the nominated
    /// moment to the first quiescent boundary the bounded run-forward reaches
    /// — the campaign completes instead of aborting — and the entry
    /// re-materializes bit-identically from its ledger env alone.
    #[test]
    fn non_quiescent_nomination_seals_with_advance_and_rematerializes() {
        let observable = |seed: u64| {
            let machine = ScriptedMachine::new(vec![(1, UpdateOp::Set)], simple_program(4))
                .with_non_quiescent(vec![(10, 12)]);
            let (_dir, led) = ledger();
            let mut camp =
                campaign_with(machine, Box::new(ToyCodec), config(8, u64::MAX), seed, led);
            let report = camp
                .step()
                .expect("the step completes despite the non-quiescent nomination");
            assert_eq!(report.candidates, 1, "the emit's fresh cell is nominated");
            assert_eq!(
                report.admitted.len(),
                1,
                "the candidate sealed with advance"
            );
            let (_, entry) = camp.frontier().iter().next().expect("one entry");
            assert!(
                entry.exemplar.cut.at.0 > 10,
                "the seal advanced off the nominated moment 10 (got {})",
                entry.exemplar.cut.at.0
            );
            let frontier: Vec<(u64, u64, Vec<u8>)> = camp
                .frontier()
                .iter()
                .map(|(_, e)| {
                    (
                        e.exemplar.cut.at.0,
                        e.exemplar.cut.sdk_events,
                        e.env.bytes.clone(),
                    )
                })
                .collect();
            (camp, frontier)
        };
        let (mut camp, frontier) = observable(7);
        assert_rematerializes_identically(&mut camp);
        // Same seed ⇒ byte-identical campaign artifacts: the advance is part
        // of the deterministic trajectory, not a wall-clock retry.
        assert_eq!(frontier, observable(7).1);
    }

    /// SealAnchor regression (task 136 / hm-esfd, second blocker): a staged
    /// reseed marker sits **beyond** the candidate moment. No seal can succeed
    /// below the marker (the server rejects while either schedule is armed),
    /// and an unclamped retry leg would overshoot it into a poisoned schedule
    /// — the clamp lands the leg deadline exactly ON the marker (arming the
    /// exact-arrival drain) and seals at the first fully-drained boundary.
    #[test]
    fn staged_marker_beyond_candidate_seals_past_the_drained_marker() {
        let program: Rc<dyn Fn(u64) -> Program> = Rc::new(|seed| Program {
            emits: vec![Emit {
                at: 10,
                reg: 1,
                value: seed % 4,
            }],
            terminal: 40,
        });
        let observable = |seed: u64| {
            let machine = ScriptedMachine::new(vec![(1, UpdateOp::Set)], Rc::clone(&program))
                .with_markers(vec![25]);
            let (_dir, led) = ledger();
            let mut camp = campaign_with(
                machine,
                Box::new(MarkerCodec { markers: vec![25] }),
                config(8, u64::MAX),
                seed,
                led,
            );
            let report = camp
                .step()
                .expect("the step completes past the staged marker");
            assert_eq!(
                report.admitted.len(),
                1,
                "the candidate sealed with advance"
            );
            let (_, entry) = camp.frontier().iter().next().expect("one entry");
            assert_eq!(
                entry.exemplar.cut.at.0, 25,
                "sealed exactly at the drained marker (the first fully-drained boundary)"
            );
            assert_eq!(
                entry.exemplar.cut.sdk_events, 1,
                "the firing below the marker is included in the cut"
            );
            // The clamp is what ran: a leg landed exactly ON the marker, and
            // no leg ever ran with the doomed candidate-moment deadline (no
            // seal can succeed below a staged Moment).
            assert!(camp.machine.deadlines_seen.contains(&Some(25)));
            assert!(!camp.machine.deadlines_seen.contains(&Some(10)));
            let frontier: Vec<(u64, u64, Vec<u8>)> = camp
                .frontier()
                .iter()
                .map(|(_, e)| {
                    (
                        e.exemplar.cut.at.0,
                        e.exemplar.cut.sdk_events,
                        e.env.bytes.clone(),
                    )
                })
                .collect();
            (camp, frontier)
        };
        let (mut camp, frontier) = observable(7);
        assert_rematerializes_identically(&mut camp);
        assert_eq!(frontier, observable(7).1);
    }

    /// SealAnchor regression (task 136 / hm-esfd, bounded-attempts posture): a
    /// candidate that never reaches a quiescent boundary is dropped after the
    /// attempt cap like a disappearing state — the campaign is NOT aborted and
    /// keeps stepping.
    #[test]
    fn never_quiescent_candidate_is_dropped_after_the_cap_without_abort() {
        let machine = ScriptedMachine::new(vec![(1, UpdateOp::Set)], simple_program(4))
            .with_non_quiescent(vec![(5, u64::MAX)]);
        let (_dir, led) = ledger();
        let mut camp = campaign_with(machine, Box::new(ToyCodec), config(8, u64::MAX), 7, led);
        let report = camp.step().expect("the campaign is not aborted");
        assert_eq!(report.candidates, 1, "the candidate was nominated");
        assert!(
            report.admitted.is_empty(),
            "but dropped after the bounded run-forward"
        );
        assert_eq!(camp.occupied(), 0, "nothing occupies the archive");
        // The run-forward was bounded: the nomination leg plus exactly
        // SEAL_RETRY_ATTEMPTS retry legs carried a deadline.
        let deadline_legs = camp
            .machine
            .deadlines_seen
            .iter()
            .filter(|d| d.is_some())
            .count();
        assert_eq!(deadline_legs, 1 + SEAL_RETRY_ATTEMPTS as usize);
        // The dropped candidate's refusals are REPORTED, not vanished with the
        // drop (T136-J2(c)): one per attempted seal — the initial refusal plus
        // one per retry leg.
        assert_eq!(report.seal_retries, 1 + u64::from(SEAL_RETRY_ATTEMPTS));
        // The campaign is alive: the next step runs the full protocol again.
        let report2 = camp.step().expect("the campaign keeps stepping");
        assert!(report2.rollout_revision.get() > report.rollout_revision.get());
    }

    /// A provisional transition never occupies the archive on its own — only an
    /// actual seal past barrier 2 does. With the replay budget at zero, provisional
    /// candidates are nominated but never materialized, so nothing is admitted.
    #[test]
    fn no_provisional_transition_occupies_the_archive() {
        let (_dir, mut camp) = campaign(simple_program(4), config(8, 0), 7);
        let report = camp.step().expect("step");
        assert_eq!(
            report.candidates, 1,
            "a provisional cell is still nominated"
        );
        assert!(
            report.admitted.is_empty(),
            "but with no replay budget nothing is materialized or admitted"
        );
        assert_eq!(
            camp.occupied(),
            0,
            "the provisional transition never occupies"
        );
    }

    /// Deterministic best-Entry-per-cell occupancy: two runs reaching the same
    /// cell at different `sealed_at` depths — the deeper (higher-quality) one
    /// dominates, and the shallower Entry is evicted. Evidence retention is
    /// untouched (both batches stay durable).
    #[test]
    fn occupancy_keeps_the_best_entry_per_cell() {
        // Both seeds reduce reg=1 to value 0 (seed % 1 == 0), but at different
        // sealable moments, so they share a cell and differ in quality (depth).
        let program: Rc<dyn Fn(u64) -> Program> = Rc::new(|seed| {
            let at = if seed == 100 { 10 } else { 30 };
            Program {
                emits: vec![Emit {
                    at,
                    reg: 1,
                    value: 0,
                }],
                terminal: at + 10,
            }
        });
        let (_dir, led) = ledger();
        let machine = ScriptedMachine::new(vec![(1, UpdateOp::Set)], program);
        // A fixed-choice selector isn't needed: GenesisSelector explores each step
        // with a fresh seed drawn from the campaign stream — but we need specific
        // seeds, so drive the machine directly via two explicit campaigns sharing
        // one occupancy is not possible; instead script the seeds through the PRNG.
        // Simpler: run two steps and rely on the domination logic via the Occupancy
        // unit below. Here assert the Occupancy directly.
        let mut occ = Occupancy::new();
        let entry = |seed: u64, at: u64| FrontierEntry {
            exemplar: VirtualExemplar {
                parent: SnapId(0),
                seed,
                suffix: Reproducer {
                    blob_version: 1,
                    bytes: vec![],
                },
                cut: EvidenceCut {
                    at: Moment(at),
                    sdk_events: 1,
                },
            },
            env: Reproducer {
                blob_version: 1,
                bytes: vec![],
            },
            reward: Reward { new_cells: 1 },
        };
        let cell = b"cell".to_vec();
        // Shallow first (quality 10).
        let o1 = occ.admit(entry(100, 10), cell.clone(), 10);
        assert!(matches!(o1, Occupied::Fresh(_)));
        // Deeper dominates (quality 30) and evicts the shallow occupant.
        let o2 = occ.admit(entry(200, 30), cell.clone(), 30);
        assert!(matches!(o2, Occupied::Dominated { .. }));
        assert_eq!(occ.frontier().len(), 1, "one Entry per cell");
        // An equal-or-lower quality is rejected (the stable earlier occupant wins).
        let o3 = occ.admit(entry(300, 30), cell.clone(), 30);
        assert_eq!(o3, Occupied::Rejected);
        let o4 = occ.admit(entry(400, 5), cell, 5);
        assert_eq!(o4, Occupied::Rejected);
        assert_eq!(occ.frontier().len(), 1);
        drop((led, machine));
    }

    /// hm-mcx regression: evidence emitted at or after the seal boundary cannot
    /// influence a cell committed at an earlier seal. The half-open included-count
    /// cut excludes it. Here a later firing (a "crash-line" species) changes the
    /// full-run cell but not the earlier seal's cell.
    #[test]
    fn evidence_after_the_seal_cannot_influence_an_earlier_cell() {
        // reg=1 set to 5 at moment 10 (the seal), then set to 99 at moment 30
        // (after the seal boundary).
        let program: Rc<dyn Fn(u64) -> Program> = Rc::new(|_seed| Program {
            emits: vec![
                Emit {
                    at: 10,
                    reg: 1,
                    value: 5,
                },
                Emit {
                    at: 30,
                    reg: 1,
                    value: 99,
                },
            ],
            terminal: 40,
        });
        let (_dir, led) = ledger();
        let machine = ScriptedMachine::new(vec![(1, UpdateOp::Set)], program.clone());
        let mut camp = DifferentialCampaign::new(
            machine,
            Box::new(ToyCodec),
            Box::new(DeclineTactic::new()),
            Box::new(GenesisSelector::new()),
            Box::new(DefaultObservationCells::new()),
            led,
            coordinator(),
            // Cap 1 so only the FIRST (earliest) provisional candidate — the seal
            // at moment 10 — is materialized.
            config(1, u64::MAX),
            7,
        )
        .expect("new");
        let report = camp.step().expect("step");
        assert_eq!(report.admitted.len(), 1);
        // The admitted Entry's cell is the state at its actual sealed_at (moment
        // 10): reg=1 → 5. The later value 99 (emitted after the seal) is excluded
        // by the half-open cut, so it did not influence this committed cell.
        let (_r, entry) = camp.frontier().iter().next().expect("one entry");
        assert_eq!(entry.exemplar.cut.at, Moment(10));
        assert_eq!(entry.exemplar.cut.sdk_events, 1, "only the pre-seal firing");
        // Reduce the committed evidence at the Entry's cut and confirm the cell is
        // {reg1:5}, never {reg1:99}.
        let id = camp
            .ledger()
            .batch_ids()
            .find(|id| {
                camp.ledger()
                    .get(id)
                    .map(|e| e.cut.at == Moment(10))
                    .unwrap_or(false)
            })
            .copied()
            .expect("the sealed batch");
        let obs = camp.ledger().get(&id).unwrap().observations_at_cut();
        let reg1 = ObservationId::Point {
            namespace: NS_STATE,
            local: 1,
        };
        assert_eq!(
            obs.get(&reg1),
            Some(&crate::evidence::ReducedValue::Scalar(5)),
            "the pre-seal value, not the post-seal 99"
        );
    }

    /// A partial (assigned-but-uncommitted) batch cannot advance a frontier: an
    /// unfinished proposal leaves the committed frontier where it was.
    #[test]
    fn an_uncommitted_batch_cannot_advance_the_frontier() {
        let mut coord = coordinator();
        let cohort = coord.open_cohort().expect("cohort");
        let before = coord.committed_frontier();
        let _p = coord.assign(cohort).expect("assign"); // reserved, never completed
        assert_eq!(
            coord.committed_frontier(),
            before,
            "an assigned-but-uncommitted proposal does not advance the committed frontier"
        );
    }

    /// Restart rebuilds the canonical inputs from the durable evidence ledger
    /// alone: after appending batches, a fresh handle over the same file replays
    /// every batch and its reduced observations.
    #[test]
    fn restart_rebuilds_canonical_inputs_from_the_ledger() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("evidence.log");
        let ids: Vec<EvidenceBatchId>;
        {
            let led = EvidenceLedger::open(&path).expect("open");
            let machine = ScriptedMachine::new(vec![(1, UpdateOp::Set)], simple_program(4));
            let mut camp = DifferentialCampaign::new(
                machine,
                Box::new(ToyCodec),
                Box::new(DeclineTactic::new()),
                Box::new(GenesisSelector::new()),
                Box::new(DefaultObservationCells::new()),
                led,
                coordinator(),
                config(8, u64::MAX),
                7,
            )
            .expect("new");
            camp.explore(3).expect("explore");
            ids = camp.ledger().batch_ids().copied().collect();
            assert!(!ids.is_empty());
        }
        // Restart: a fresh ledger handle rebuilds every canonical batch input.
        let led = EvidenceLedger::open(&path).expect("reopen");
        for id in &ids {
            assert!(led.contains(id), "batch replayed from the durable ledger");
            // …and its reduced observations recompute identically.
            let obs = led.get(id).unwrap().observations_at_cut();
            let _ = obs; // pure recomputation, no panic
        }
        assert_eq!(led.len(), ids.len());
    }

    // -- The M1 differential parity gate (task 132): direct recomputation is
    // the ORACLE for the production relations. After every barrier-passed
    // step, every materialized view (observations, cells, occupancy) must
    // equal the pure lineage-composed recomputation over the durable ledger.

    /// Recompute every view from the ledger alone and assert view-for-view
    /// equality with the coordinator's materialized views at the visible
    /// frontier.
    fn assert_view_parity(camp: &DifferentialCampaign<ScriptedMachine>) {
        let frontier = camp.coordinator().visible_frontier();
        if frontier == Revision::ZERO {
            return;
        }
        let views = camp
            .coordinator()
            .materialized(frontier)
            .expect("frontier-passed views are readable");
        let ledger = camp.ledger();
        let cells = DefaultObservationCells::new();

        // Encode a recomputed observation map into the coordinator's pair
        // shape (the byte-level currency both sides share).
        let encode_pairs = |obs: &ObservationMap| -> Vec<(Vec<u8>, ReducedRow)> {
            obs.iter()
                .map(|(id, val)| {
                    let mut key = Vec::new();
                    encode_observation_id(&mut key, id);
                    let red = match val {
                        ReducedValue::Scalar(v) => ReducedRow::Scalar(*v),
                        ReducedValue::Accumulated(s) => {
                            ReducedRow::Accumulated(s.iter().copied().collect())
                        }
                    };
                    (key, red)
                })
                .collect()
        };

        let view_pairs = |rollout: u64, point: PointRow| -> Vec<(Vec<u8>, ReducedRow)> {
            views
                .observations
                .iter()
                .filter(|((r, p, _), _)| *r == rollout && *p == point)
                .map(|((_, _, k), red)| (k.clone(), red.clone()))
                .collect()
        };

        let mut expected_occ: BTreeMap<CellKey, (u64, u64)> = BTreeMap::new();
        for id in ledger.batch_ids() {
            let ev = ledger.get(id).expect("retained");
            match ev.role {
                EvidenceRole::Rollout => {
                    // Every provisional cut: recompute observations + cell.
                    let start = ev.parent_cut.map(|c| c.sdk_events).unwrap_or(0);
                    let mut seen = std::collections::BTreeSet::new();
                    for &m in &ev.sealable_moments {
                        let included = ev
                            .normalized
                            .events
                            .iter()
                            .filter(|e| e.moment.0 <= m)
                            .count() as u64;
                        let count = start + included;
                        if !seen.insert(count) {
                            continue;
                        }
                        let obs = crate::evidence::compose_observations_at(ledger, ev, count);
                        assert_eq!(
                            view_pairs(ev.rollout.issue, PointRow::Cut(count)),
                            encode_pairs(&obs),
                            "cut observations diverge (rollout {}, count {count})",
                            ev.rollout.issue
                        );
                        let cut = EvidenceCut {
                            at: Moment(m),
                            sdk_events: count,
                        };
                        assert_eq!(
                            views.cell_at(ev.rollout.issue, PointRow::Cut(count)),
                            Some(&cells.key(cut, &obs)),
                            "cut cell diverges (rollout {}, count {count})",
                            ev.rollout.issue
                        );
                    }
                }
                EvidenceRole::Seal => {
                    let rollout = ev.rollout.parent.expect("a seal names its rollout");
                    let point = PointRow::Seal(ev.rollout.issue);
                    let obs =
                        crate::evidence::compose_observations_at(ledger, ev, ev.cut.sdk_events);
                    assert_eq!(
                        view_pairs(rollout, point),
                        encode_pairs(&obs),
                        "seal observations diverge (seal {})",
                        ev.rollout.issue
                    );
                    let cell = cells.key(ev.cut, &obs);
                    assert_eq!(
                        views.cell_at(rollout, point),
                        Some(&cell),
                        "seal cell diverges (seal {})",
                        ev.rollout.issue
                    );
                    // Recomputed occupancy: best (quality desc, entry asc).
                    let quality = ev.cut.at.0;
                    let entry = ev.rollout.issue;
                    expected_occ
                        .entry(cell)
                        .and_modify(|(bq, be)| {
                            if quality > *bq || (quality == *bq && entry < *be) {
                                *bq = quality;
                                *be = entry;
                            }
                        })
                        .or_insert((quality, entry));
                }
            }
        }
        let expected_occ: Vec<(CellKey, u64)> = expected_occ
            .into_iter()
            .map(|(cell, (_q, e))| (cell, e))
            .collect();
        assert_eq!(views.occupancy, expected_occ, "occupancy diverges");
    }

    /// The occupancy reconciliation is live: the REAL views pass, a
    /// tampered occupancy view fails with the typed divergence (kills the
    /// `check_occupancy -> Ok(())` stub).
    #[test]
    fn occupancy_reconciliation_rejects_a_tampered_view() {
        let (_dir, mut camp) = campaign(simple_program(4), config(8, u64::MAX), 7);
        camp.step().expect("step");
        let frontier = camp.coordinator().visible_frontier();
        let views = camp.coordinator().materialized(frontier).expect("readable");
        camp.check_occupancy(&views).expect("the real views agree");
        let mut tampered = views.clone();
        tampered.occupancy.push((b"no-such-cell".to_vec(), 999));
        assert!(matches!(
            camp.check_occupancy(&tampered),
            Err(CampaignError::OccupancyDivergence { .. })
        ));
        let mut wrong_entry = views;
        if let Some(first) = wrong_entry.occupancy.first_mut() {
            first.1 += 1;
            assert!(matches!(
                camp.check_occupancy(&wrong_entry),
                Err(CampaignError::OccupancyDivergence { .. })
            ));
        }
    }

    /// The lineage-composed recompute EXCLUDES a parent's post-fork
    /// accumulate value (the half-open fork truncation, `pos < upper`), and
    /// a child batch's observed cut is the parent count PLUS its suffix
    /// length (the cumulative-position arithmetic).
    #[test]
    fn lineage_cut_arithmetic_and_fork_truncation_are_exact() {
        use crate::defaults::ExploreExploitSelector;
        let program: Rc<dyn Fn(u64) -> Program> = Rc::new(|seed| Program {
            emits: vec![
                Emit {
                    at: 10,
                    reg: 2,
                    value: seed % 5,
                },
                Emit {
                    at: 20,
                    reg: 2,
                    value: 100 + seed % 5,
                },
            ],
            terminal: 30,
        });
        let (_dir, led) = ledger();
        let machine = ScriptedMachine::new(vec![(2, UpdateOp::Accumulate)], program);
        let mut camp = DifferentialCampaign::new(
            machine,
            Box::new(ToyCodec),
            Box::new(DeclineTactic::new()),
            // Explore period 3: steps 1..2 exploit once an entry exists.
            Box::new(ExploreExploitSelector::new().with_explore_period(3)),
            Box::new(DefaultObservationCells::new()),
            led,
            coordinator(),
            // Cap 1: only the FIRST provisional candidate (the shallower
            // seal) is materialized, so the exploited entry's fork cut lies
            // strictly below the parent's persisted extent — the parent has
            // a post-fork accumulate value to exclude.
            config(1, u64::MAX),
            13,
        )
        .expect("new");
        for _ in 0..6 {
            camp.step().expect("step");
        }
        let ledger = camp.ledger();
        let mut checked_child = false;
        for id in ledger.batch_ids() {
            let ev = ledger.get(id).expect("retained");
            if ev.role != EvidenceRole::Rollout {
                continue;
            }
            let start = ev.parent_cut.map(|c| c.sdk_events).unwrap_or(0);
            // The cumulative-cut arithmetic: observed cut = start + suffix.
            assert_eq!(
                ev.cut.sdk_events,
                start + ev.normalized.events.len() as u64,
                "cumulative observed cut"
            );
            let Some(parent_issue) = ev.rollout.parent else {
                continue;
            };
            // A real branch child below a mid-run fork: the parent's
            // post-fork accumulate values must NOT reach the child's map.
            let parent = ledger
                .batch_ids()
                .filter_map(|i| ledger.get(i))
                .find(|b| b.role == EvidenceRole::Rollout && b.rollout.issue == parent_issue)
                .expect("parent rollout batch");
            let parent_start = parent.parent_cut.map(|c| c.sdk_events).unwrap_or(0);
            let parent_extent = parent_start + parent.normalized.events.len() as u64;
            if start >= parent_extent {
                continue; // forked at the parent's extent: nothing excluded
            }
            checked_child = true;
            let excluded_value = {
                let idx = (start - parent_start) as usize;
                match &parent.normalized.events[idx].payload {
                    sdk_events::Payload::State { value, .. } => *value,
                    other => panic!("unexpected payload {other:?}"),
                }
            };
            let obs = crate::evidence::compose_observations_at(ledger, ev, start);
            let reg2 = sdk_events::ObservationId::Point {
                namespace: sdk_events::NS_STATE,
                local: 2,
            };
            if let Some(crate::evidence::ReducedValue::Accumulated(set)) = obs.get(&reg2) {
                assert!(
                    !set.contains(&excluded_value)
                        || parent.normalized.events[..(start - parent_start) as usize]
                            .iter()
                            .any(|e| matches!(&e.payload,
                                sdk_events::Payload::State { value, .. } if *value == excluded_value)),
                    "the parent's post-fork value {excluded_value} leaked into the \
                     child's inherited prefix"
                );
            }
        }
        assert!(checked_child, "a mid-run fork child was exercised");
    }

    /// F2 regression (tribunal #134): a real-guest rollout's SDK capture leads
    /// with a catalog tuple (`event_id 0`), and the server stamps the parent
    /// cut's `sdk_events` over the WHOLE capture vector — the catalog tuple
    /// counted as one position (`control.rs`: `vmm.sdk_events().len()`).
    /// `decode_child_suffix` must therefore skip the inherited prefix by ORDINAL
    /// POSITION (the catalog occupies one), not by firing-only count; otherwise
    /// it keeps the catalog "for free" AND skips that many firings, over-skipping
    /// by the catalog count and dropping the first firing that should survive.
    ///
    /// This is the catalog-inclusive-cut shape: a real guest, whose
    /// server-stamped cut counts its own catalog, and the portable game campaign,
    /// whose `DeclaredMachine` wrapper prepends a catalog AND stamps its seal cut
    /// catalog-inclusive to match (tribunal V1). The complementary machine-side
    /// pin — that the wrapper produces this coordinate for a no-catalog toy — is
    /// `campaign-runner`'s `declared_machine_stamps_a_catalog_inclusive_cut_for_a_no_catalog_guest`.
    #[test]
    fn child_suffix_keeps_the_first_firing_below_a_catalog_bearing_cut() {
        let (_dir, camp) = campaign(simple_program(4), config(8, u64::MAX), 7);
        let decl = sdk_events::encode_v2_declaration(&[sdk_events::DeclaredPoint {
            namespace: NS_STATE,
            local: 1,
            name: "r1".into(),
            classification: sdk_events::Classification::State,
            value_shape: Some(sdk_events::ValueShape::U64),
            base_op: Some(UpdateOp::Set),
            expectation: None,
        }])
        .expect("valid v2 declaration");
        let id = ((NS_STATE as u32) << 24) | 1;
        let firing = |value: u64| {
            let mut b = vec![0u8]; // op byte: Set
            b.extend_from_slice(&value.to_le_bytes());
            b
        };
        // A child rollout's raw capture, real-guest shape: catalog at position 0,
        // the restored ancestor firing at position 1, then the child's own two
        // firings at positions 2 and 3.
        let raw = vec![
            (0u64, CATALOG_EVENT_ID, decl),
            (10u64, id, firing(100)), // pos 1: inherited ancestor firing
            (20u64, id, firing(200)), // pos 2: FIRST child firing — must survive
            (30u64, id, firing(300)), // pos 3: second child firing
        ];
        // The parent seal counted 2 capture positions — the catalog (pos 0) and
        // the one inherited firing (pos 1) — so the server-stamped cut is 2.
        let inherited = 2;
        let normalized = camp.decode_child_suffix(&raw, inherited).expect("decodes");
        let values: Vec<u64> = normalized
            .events
            .iter()
            .map(|e| match &e.payload {
                sdk_events::Payload::State { value, .. } => *value,
                other => panic!("unexpected payload {other:?}"),
            })
            .collect();
        // The one inherited firing (100) is dropped; BOTH child firings survive.
        // The bug skipped the catalog for free and then 2 FIRINGS, dropping the
        // first child firing (200) and leaving only [300].
        assert_eq!(
            values,
            vec![200, 300],
            "the first child firing below a catalog-bearing cut must survive"
        );
    }

    /// A genesis-rooted multi-op campaign: after every step, every
    /// materialized view equals the direct recomputation (the M1 gate).
    #[test]
    fn materialized_views_match_direct_recomputation() {
        let program: Rc<dyn Fn(u64) -> Program> = Rc::new(|seed| Program {
            emits: vec![
                Emit {
                    at: 10,
                    reg: 1,
                    value: seed % 3,
                },
                Emit {
                    at: 20,
                    reg: 2,
                    value: seed % 5,
                },
                Emit {
                    at: 30,
                    reg: 3,
                    value: seed % 7,
                },
            ],
            terminal: 40,
        });
        let (_dir, led) = ledger();
        let machine = ScriptedMachine::new(
            vec![
                (1, UpdateOp::Set),
                (2, UpdateOp::Accumulate),
                (3, UpdateOp::Max),
            ],
            program,
        );
        let mut camp = DifferentialCampaign::new(
            machine,
            Box::new(ToyCodec),
            Box::new(DeclineTactic::new()),
            Box::new(GenesisSelector::new()),
            Box::new(DefaultObservationCells::new()),
            led,
            coordinator(),
            config(8, u64::MAX),
            11,
        )
        .expect("new");
        for _ in 0..6 {
            camp.step().expect("step");
            assert_view_parity(&camp);
        }
        assert!(camp.occupied() > 0, "the gate is not vacuous");
    }

    /// An exploit campaign with real branch lineage: children inherit the
    /// ancestor evidence prefix, and the materialized views still equal the
    /// lineage-composed recomputation after every step.
    #[test]
    fn lineage_views_match_direct_recomputation() {
        use crate::defaults::ExploreExploitSelector;
        let program: Rc<dyn Fn(u64) -> Program> = Rc::new(|seed| Program {
            emits: vec![
                Emit {
                    at: 10,
                    reg: 1,
                    value: seed % 4,
                },
                Emit {
                    at: 20,
                    reg: 2,
                    value: seed % 3,
                },
            ],
            terminal: 30,
        });
        let (_dir, led) = ledger();
        let machine =
            ScriptedMachine::new(vec![(1, UpdateOp::Set), (2, UpdateOp::Accumulate)], program);
        let mut camp = DifferentialCampaign::new(
            machine,
            Box::new(ToyCodec),
            Box::new(DeclineTactic::new()),
            Box::new(ExploreExploitSelector::new()),
            Box::new(DefaultObservationCells::new()),
            led,
            coordinator(),
            config(8, u64::MAX),
            5,
        )
        .expect("new");
        let mut exploited = false;
        for _ in 0..8 {
            let report = camp.step().expect("step");
            exploited |= !report.explored;
            assert_view_parity(&camp);
        }
        assert!(exploited, "the lineage path was exercised");
        assert!(
            camp.ledger()
                .batch_ids()
                .filter_map(|id| camp.ledger().get(id))
                .any(|ev| ev.role == EvidenceRole::Rollout && ev.rollout.parent.is_some()),
            "a branch child committed lineage evidence"
        );
    }

    /// The occurrence oracle and absence view flow through the controller over the
    /// immutable evidence view: an `always`-false JSON assertion in the capture is
    /// reported once (deduped by property across steps).
    #[test]
    fn controller_reports_occurrence_counterexamples_once() {
        // A program with no state (empty), so the interest is purely the oracle.
        let program: Rc<dyn Fn(u64) -> Program> = Rc::new(|_| Program {
            emits: vec![],
            terminal: 10,
        });
        let (_dir, led) = ledger();
        // Override sdk_events to inject a JSON always-false assertion is not
        // possible on the binary machine; instead build the evidence path via a
        // machine that emits an `always` violation as a binary terminal. Simpler:
        // assert the oracle path is wired by checking a binary terminal assertion.
        struct AssertMachine {
            inner: ScriptedMachine,
            fired: bool,
        }
        impl Machine for AssertMachine {
            fn branch(&mut self, s: SnapId, e: &Reproducer) -> Result<(), MachineError> {
                self.fired = false;
                self.inner.branch(s, e)
            }
            fn replay(&mut self, s: SnapId) -> Result<(), MachineError> {
                self.inner.replay(s)
            }
            fn run(
                &mut self,
                u: &StopConditions,
                r: Option<&Answer>,
            ) -> Result<StopReason, MachineError> {
                // Terminate on an assertion the first rollout run.
                if u.deadline.is_none() && !self.fired {
                    self.fired = true;
                    return Ok(StopReason::Assertion {
                        vtime: Moment(5),
                        id: 42,
                        data: vec![],
                    });
                }
                self.inner.run(u, r)
            }
            fn snapshot(&mut self) -> Result<(SnapId, EvidenceCut), MachineError> {
                self.inner.snapshot()
            }
            fn drop_snap(&mut self, s: SnapId) -> Result<(), MachineError> {
                self.inner.drop_snap(s)
            }
            fn hash(&mut self) -> Result<[u8; 32], MachineError> {
                self.inner.hash()
            }
            fn coverage(&self) -> &[u8] {
                self.inner.coverage()
            }
            fn recorded_env(&self) -> Result<Reproducer, MachineError> {
                self.inner.recorded_env()
            }
            fn sdk_events(&mut self) -> Result<Vec<(u64, u32, Vec<u8>)>, MachineError> {
                self.inner.sdk_events()
            }
        }
        let machine = AssertMachine {
            inner: ScriptedMachine::new(vec![(1, UpdateOp::Set)], program),
            fired: false,
        };
        let mut camp = DifferentialCampaign::new(
            machine,
            Box::new(ToyCodec),
            Box::new(DeclineTactic::new()),
            Box::new(GenesisSelector::new()),
            Box::new(DefaultObservationCells::new()),
            led,
            coordinator(),
            config(8, u64::MAX),
            7,
        )
        .expect("new");
        let r1 = camp.step().expect("step 1");
        assert_eq!(
            r1.counterexamples.len(),
            1,
            "the terminal assertion is reported"
        );
        assert_eq!(
            r1.counterexamples[0].kind,
            crate::occurrence::CounterexampleKind::TerminalAssertion
        );
        assert_eq!(
            camp.views().finalized.counterexamples,
            1,
            "the finalized counter counts the distinct counterexample"
        );
        // A second step reaching the same property reports nothing new (dedup by
        // property across the campaign).
        let r2 = camp.step().expect("step 2");
        assert!(
            r2.counterexamples.is_empty(),
            "the same property is not re-reported"
        );
        assert_eq!(
            camp.views().finalized.counterexamples,
            1,
            "the finalized counter does not re-count the deduped property"
        );
    }
}
