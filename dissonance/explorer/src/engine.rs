// SPDX-License-Identifier: AGPL-3.0-or-later
//! The engine: [`Explorer`] and the two loops, composed over the spine.
//!
//! [`Explorer::modulation`] drives one run to a terminal stop, answering each
//! surfaced decision through the open-loop [`Tactic`] and capturing every
//! sealable point as parent-rooted exemplar material.
//! [`Explorer::progression_step`] asks the [`Selector`] for the next branch
//! base, materializes it, mints the next [`Environment`] through the
//! [`EnvCodec`], runs one Modulation, folds the run into the [`Archive`]
//! (timeline admission), rewards the selector, and judges the run with the
//! [`Oracle`]. [`Explorer::explore`] runs the Progression for a bounded number
//! of steps.
//!
//! ## Seals: the engine-side materialization cache (task 68)
//!
//! A frontier entry is a *virtual* exemplar — kilobytes, never a resource. The
//! engine keeps the expensive half separately, in its embedded
//! [`Materializer`]: a **seal** (a live [`SnapId`]) per materialized exemplar,
//! minted eagerly at each admitted fork and re-minted on demand, plus the
//! **lineage table** and the **spanning-ancestor retention pool** (task 68).
//! Materialization is **parent-rooted**: `branch(parent, suffix)` + replay
//! only the suffix + seal; when the parent's seal was evicted, the suffix
//! chain from the nearest *retained* ancestor is folded via
//! [`EnvCodec::compose`] — one branch + one run. Genesis
//! (`branch(genesis, entry.env)` under [`StopMask::NONE`], a pinned replay) is
//! reached only when no ancestor is retained — the graceful worst case, and
//! why dropping seals ([`Explorer::evict_seals`], the budgeted
//! [`Explorer::enforce_seal_budget`]) is **reproducibility-safe** (spine
//! invariant 4): determinism makes any re-materialized state identical.
//! Retention is a pure performance knob.

use std::collections::BTreeSet;

use crate::error::MachineError;
use crate::materialize::{Materialization, Materializer, SealBudget};
use crate::prng::Prng;
use crate::seam::{EnvCodec, Machine};
use crate::spine::{
    Archive, Bug, CellFn, CoverageView, DecisionPoint, ExemplarRef, Fork, Frontier, Moment, Oracle,
    ProbeOracle, ProbePlan, RunTrace, Selector, Sensor, Tactic, VirtualExemplar,
};
use crate::{Answer, Environment, SnapId, StopConditions, StopMask, StopReason};

/// The result of one Modulation: where it stopped and the **branch-local**
/// reproducer [`Environment`] accumulated over it
/// ([`Machine::recorded_env`], keyed since the Modulation's branch origin). The
/// enclosing Progression step rebases it to genesis-complete (via
/// [`EnvCodec::compose`]) before it reaches a [`RunTrace`] or a [`Bug`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RunOutcome {
    /// The terminal [`StopReason`] the Modulation ended at.
    pub stop: StopReason,
    /// The reproducer accumulated over the run, keyed since the branch origin.
    pub env: Environment,
}

/// A sealable point captured mid-run, awaiting admission by the enclosing
/// Progression step: the eagerly-minted seal, the fork moment, the **prefix**
/// reproducer as of that point (not the whole run — a later branch's overrides
/// would mis-key against the fork's decision-index origin), and the coverage
/// view as of that point.
struct PendingFork {
    seal: SnapId,
    at: Moment,
    suffix: Environment,
    coverage: Vec<u8>,
}

/// The spine composition an [`Explorer`] drives: one implementation per seam.
/// [`Composition::defaults`] wires the behavior-equivalence defaults; a
/// campaign swaps parts individually as later tasks ship them.
pub struct Composition {
    /// The inner loop's open-loop answering policy.
    pub tactic: Box<dyn Tactic>,
    /// The outer loop's branch-base policy.
    pub selector: Box<dyn Selector>,
    /// The frontier fold (timeline admission, best-per-cell, eviction).
    pub archive: Box<dyn Archive>,
    /// The trace oracle judging each finished run.
    pub oracle: Box<dyn Oracle>,
    /// The campaign-defined cell function.
    pub cells: Box<dyn CellFn>,
    /// The sensors deriving timeline features from each run's trace.
    pub sensors: Vec<Box<dyn Sensor>>,
}

impl Composition {
    /// The behavior-equivalence default composition: decline every decision
    /// ([`DeclineTactic`](crate::DeclineTactic)), explore/exploit the frontier
    /// ([`ExploreExploitSelector`](crate::ExploreExploitSelector)), admit on
    /// coverage novelty ([`CoverageArchive`](crate::CoverageArchive) over
    /// [`IdentityCells`](crate::IdentityCells)), and judge terminal stops
    /// ([`TerminalOracle`](crate::TerminalOracle)). Composed as the old
    /// `Strategy` was, this reproduces the pre-refactor campaign
    /// (`tests/behavior_equiv.rs`).
    pub fn defaults() -> Self {
        Self {
            tactic: Box::new(crate::defaults::DeclineTactic::new()),
            selector: Box::new(crate::defaults::ExploreExploitSelector::new()),
            archive: Box::new(crate::defaults::CoverageArchive::new()),
            oracle: Box::new(crate::defaults::TerminalOracle::new()),
            cells: Box::new(crate::defaults::IdentityCells::new()),
            sensors: Vec::new(),
        }
    }
}

/// The exploration engine: a [`Machine`] driven by a spine [`Composition`],
/// minting environments through an [`EnvCodec`] from a single caller-seeded
/// campaign stream. Owns the genesis snapshot every first-generation Modulation
/// branches from, and the seal cache materialized exemplars branch from.
pub struct Explorer<M: Machine> {
    machine: M,
    codec: Box<dyn EnvCodec>,
    tactic: Box<dyn Tactic>,
    selector: Box<dyn Selector>,
    archive: Box<dyn Archive>,
    oracle: Box<dyn Oracle>,
    cells: Box<dyn CellFn>,
    sensors: Vec<Box<dyn Sensor>>,
    /// The campaign stream: every seed/salt draw and every tactic/selector draw
    /// comes from here, so a campaign is a pure function of `(seed, machine)`.
    rng: Prng,
    genesis: SnapId,
    until: StopConditions,
    /// Sealable points captured this Modulation, awaiting admission by the
    /// enclosing [`progression_step`](Explorer::progression_step). A `Vec` (not a
    /// single slot) so a Modulation that forks more than once admits/drops
    /// *every* fork and never leaks a backend handle.
    pending_forks: Vec<PendingFork>,
    /// The task-68 materialization engine: the seal cache (**stable** frontier
    /// id → live seal, so an [`Archive::evict`] that compacts the frontier can
    /// never re-point a seal at a different exemplar), the lineage table, and
    /// the retention pool. Never a correctness surface — see
    /// [`evict_seals`](Explorer::evict_seals).
    mat: Materializer,
}

impl<M: Machine> Explorer<M> {
    /// Snapshot the freshly-spawned machine at its quiescent boot point → the
    /// **genesis [`SnapId`]**, the base every first-generation Modulation branches
    /// from (the frontier starts empty, so step 1 has no admitted exemplar to
    /// branch from). Returns [`Err`] if that initial snapshot fails (e.g. not
    /// quiescent) — never panics or fabricates a base. `seed` starts the
    /// campaign stream.
    ///
    /// The default [`StopConditions`] surface every decision class and the
    /// snapshot point ([`StopMask::ALL`], no deadline) — the coverage-guided
    /// default. A pure seed-driven campaign sets [`StopMask::NONE`] via
    /// [`set_stop_conditions`](Explorer::set_stop_conditions).
    pub fn new(
        machine: M,
        codec: Box<dyn EnvCodec>,
        parts: Composition,
        seed: u64,
    ) -> Result<Self, MachineError> {
        let mut machine = machine;
        let genesis = machine.snapshot()?;
        Ok(Self {
            machine,
            codec,
            tactic: parts.tactic,
            selector: parts.selector,
            archive: parts.archive,
            oracle: parts.oracle,
            cells: parts.cells,
            sensors: parts.sensors,
            rng: Prng::new(seed),
            genesis,
            until: StopConditions {
                deadline: None,
                on: StopMask::ALL,
            },
            pending_forks: Vec::new(),
            // Genesis moment defaults to 0 (exact for the toy; a live driver
            // that probed the true origin records it via
            // `set_genesis_moment`). Policy-only — see `Materializer`.
            mat: Materializer::new(genesis, Moment(0)),
        })
    }

    /// The genesis snapshot every first-generation Modulation branches from.
    pub fn genesis(&self) -> SnapId {
        self.genesis
    }

    /// The archive's current frontier, for inspection.
    pub fn frontier(&self) -> &Frontier {
        self.archive.frontier()
    }

    /// The archive itself (e.g. to consult its `admissible` predicate).
    pub fn archive(&self) -> &dyn Archive {
        self.archive.as_ref()
    }

    /// The live seal materializing frontier entry `r`, if any — `None` after
    /// eviction (the entry re-materializes on its next exploit).
    pub fn seal_of(&self, r: ExemplarRef) -> Option<SnapId> {
        self.mat.seal_of(r)
    }

    /// How many frontier entries currently hold a live seal.
    pub fn sealed_count(&self) -> usize {
        self.mat.sealed_count()
    }

    /// The embedded task-68 materialization engine (lineage table + retention
    /// pool), read-only — for inspection and diagnostics.
    pub fn materializer(&self) -> &Materializer {
        &self.mat
    }

    /// Record the genesis snapshot's true moment (e.g. a live origin probed
    /// before [`Explorer::new`]). Default `Moment(0)`. Policy-only: it scales
    /// the retention model's genesis bound, never correctness.
    pub fn set_genesis_moment(&mut self, at: Moment) {
        self.mat.set_genesis_at(at);
    }

    /// Inject the task-63 `sealable(Moment)` predicate (default always-true —
    /// the GO arm). Gates every seal the engine takes: the eager fork seals in
    /// [`modulation`](Explorer::modulation) (an inadmissible [`StopReason::SnapshotPoint`]
    /// is stepped past, never sealed) and every
    /// [`materialize`](Explorer::materialize) target (refused loudly with
    /// [`MachineError::NotSealable`]). Compose with
    /// [`CoverageArchive::with_sealable`](crate::CoverageArchive::with_sealable)
    /// so admission agrees with the engine.
    pub fn set_sealable(&mut self, sealable: Box<dyn Fn(Moment) -> bool>) {
        self.mat.set_sealable(sealable);
    }

    /// Set the retention pool's budget (default [`SealBudget::Unbounded`],
    /// preserving the eager seal-per-admission behavior).
    /// [`progression_step`](Explorer::progression_step) enforces it after every
    /// admission; [`enforce_seal_budget`](Explorer::enforce_seal_budget) runs it
    /// on demand.
    pub fn set_seal_budget(&mut self, budget: SealBudget) {
        self.mat.set_budget(budget);
    }

    /// The modeled materialization cost of entry `r` in `Moment` units (`0`
    /// when its own seal is live, else the replay depth from the nearest
    /// retained ancestor, up to the genesis bound); `None` for a dead ref.
    pub fn modeled_cost(&self, r: ExemplarRef) -> Option<u64> {
        self.mat.modeled_cost(self.archive.frontier(), r)
    }

    /// Drop entry `r`'s seal, if it holds one (the selective retention knob;
    /// always reproducibility-safe). Returns the dropped handle.
    pub fn evict_seal(&mut self, r: ExemplarRef) -> Result<Option<SnapId>, MachineError> {
        self.mat.evict_seal(&mut self.machine, r)
    }

    /// Enforce the retention budget now: evict minimum-benefit seals until the
    /// pool fits [`SealBudget::of`] the live frontier (deterministic tie-break
    /// by [`SnapId`]). Returns the evicted handles. Called automatically at
    /// the end of every [`progression_step`](Explorer::progression_step).
    pub fn enforce_seal_budget(&mut self) -> Result<Vec<SnapId>, MachineError> {
        self.mat
            .enforce_budget(&mut self.machine, self.archive.frontier())
    }

    /// The [`StopConditions`] used by [`progression_step`](Explorer::progression_step)
    /// and [`explore`](Explorer::explore).
    pub fn stop_conditions(&self) -> &StopConditions {
        &self.until
    }

    /// Set the [`StopConditions`] the Progression drives each Modulation with — e.g.
    /// [`StopMask::NONE`] for a pure seed-driven campaign, or a deadline.
    pub fn set_stop_conditions(&mut self, until: StopConditions) {
        self.until = until;
    }

    /// Direct access to the driven machine, for tests that branch/replay/hash it
    /// outside the loop (e.g. the Modulation-replay gate).
    pub fn machine_mut(&mut self) -> &mut M {
        &mut self.machine
    }

    /// Drop **every** live seal (the aggressive end of the retention knob).
    /// Reproducibility-safe by construction: frontier entries are untouched,
    /// and a later exploit re-materializes the state from genesis, identically
    /// — the eviction-safety gate (`tests/spine_invariants.rs`) proves a
    /// campaign under this-after-every-step finds byte-identical bugs and
    /// admissions. A production archive trims seals by expected re-execution
    /// cost instead (Agamotto economics); the safety property is the same.
    ///
    /// Error-safe: each mapping is removed only **after** its `drop_snap`
    /// succeeds, so a mid-way backend failure forgets nothing — every
    /// undropped handle (the failed one included) stays cached and the call is
    /// retryable.
    pub fn evict_seals(&mut self) -> Result<(), MachineError> {
        self.mat.evict_all(&mut self.machine)
    }

    /// **Inner loop.** Drive one run from `base` to a terminal stop, answering
    /// each surfaced [`StopReason::Decision`] via the open-loop [`Tactic`] and
    /// sealing at any [`StopReason::SnapshotPoint`] (stored, with the prefix
    /// env/coverage as of the fork, for the enclosing Progression step to
    /// admit). Returns the terminal stop and the accumulated branch-local
    /// reproducer.
    pub fn modulation(
        &mut self,
        base: SnapId,
        env: &Environment,
        until: &StopConditions,
    ) -> Result<RunOutcome, MachineError> {
        // Drop any forks left pending by a prior *direct* `modulation` call or an
        // aborted Progression step (only `progression_step` admits/drains them)
        // so no backend handle is ever leaked — and error-safely: a pending is
        // removed only AFTER its `drop_snap` succeeds, so a mid-way backend
        // failure forgets nothing and the next call retries the leftovers.
        while let Some(pending) = self.pending_forks.last() {
            self.machine.drop_snap(pending.seal)?;
            self.pending_forks.pop();
        }
        self.machine.branch(base, env)?;
        let mut resolve: Option<Answer> = None;
        loop {
            let stop = self.machine.run(until, resolve.as_ref())?;
            match stop {
                StopReason::Decision { vtime, id, ref ctx } => {
                    // Answer the surfaced decision and feed it back on the next
                    // `run`. The DecisionPoint is the tactic's WHOLE live input
                    // surface (open-loop, spine invariant 1): no coverage, no
                    // archive state.
                    let pt = DecisionPoint {
                        at: Moment(vtime.0),
                        id,
                        ctx: ctx.clone(),
                    };
                    let answer = self.tactic.decide(&pt, &mut self.rng);
                    resolve = Some(answer);
                }
                StopReason::SnapshotPoint { vtime } => {
                    // The task-63 sealable seam: a point the injected
                    // predicate rejects is stepped past, never sealed — the
                    // engine must not even *attempt* a seal at an inadmissible
                    // moment (the archive's own predicate would refuse the
                    // admission anyway; compose the two so they agree).
                    if !self.mat.sealable_at(Moment(vtime.0)) {
                        resolve = None;
                        continue;
                    }
                    // Sealable point: seal eagerly (the materialization the
                    // admitted exemplar will branch from) and continue the run
                    // past it. The env/coverage are captured *now* (the prefix
                    // as of this fork), not at the terminal stop — admitting
                    // the whole-run env would mis-key a later branch's
                    // overrides against the fork's decision-index origin.
                    let seal = self.machine.snapshot()?;
                    // If capturing the prefix env fails after the seal already
                    // succeeded, the handle would leak — release it (best
                    // effort, preserving the original error) before propagating.
                    let suffix = match self.machine.recorded_env() {
                        Ok(env) => env,
                        Err(e) => {
                            let _ = self.machine.drop_snap(seal);
                            return Err(e);
                        }
                    };
                    let coverage = self.machine.coverage().to_vec();
                    self.pending_forks.push(PendingFork {
                        seal,
                        at: Moment(vtime.0),
                        suffix,
                        coverage,
                    });
                    resolve = None;
                }
                terminal => {
                    let env = self.machine.recorded_env()?;
                    return Ok(RunOutcome {
                        stop: terminal,
                        env,
                    });
                }
            }
        }
    }

    /// **Outer loop.** One Progression step: the [`Selector`] picks the branch
    /// base (a frontier exemplar, or genesis to explore), the engine
    /// materializes it and mints the next environment through the codec, runs
    /// one Modulation, admits the run's sealable forks into the [`Archive`]
    /// (dropping the seals of everything not admitted), rewards the selector,
    /// and returns the [`Oracle`]'s verdict. A [`MachineError`] aborts the step
    /// loudly and is never reported as a bug.
    pub fn progression_step(&mut self) -> Result<Option<Bug>, MachineError> {
        let until = self.until.clone();

        // 1. Pick the branch base and mint the environment. Draw order (seed on
        //    explore; pick inside `choose`, then salt, on exploit) mirrors the
        //    pre-refactor god-object stream exactly — the equivalence gate pins
        //    it.
        let choice = self.selector.choose(self.archive.frontier(), &mut self.rng);
        let (base_snap, base_env, minted, branch_env) = match choice {
            None => {
                let seed = self.rng.next_u64();
                (self.genesis, None, seed, self.codec.seeded(seed))
            }
            Some(r) => {
                let entry_env = match self.archive.frontier().get(r) {
                    Some(entry) => entry.env.clone(),
                    // A dead/foreign reference is policy misuse by the
                    // selector — surfaced loudly, never papered over.
                    None => return Err(MachineError::UnknownExemplar(r.0)),
                };
                let salt = self.rng.next_u64();
                let env = self.codec.mutate(&entry_env, salt);
                let snap = self.materialize(r)?;
                (snap, Some(entry_env), salt, env)
            }
        };

        // 2. One Modulation.
        let outcome = self.modulation(base_snap, &branch_env, &until)?;

        // 3. Rebase the run to genesis-complete (the task-93 compose ruling):
        //    a genesis-rooted run's reproducer already is; a run branched below
        //    an exemplar composes through the entry's genesis-complete env.
        let genesis_env = match &base_env {
            None => outcome.env.clone(),
            Some(base) => self.codec.compose(base, &outcome.env),
        };
        let trace = RunTrace {
            terminal: outcome.stop.clone(),
            env: genesis_env,
            coverage: Some(CoverageView {
                map: self.machine.coverage().to_vec(),
            }),
            events: Vec::new(),
            records: Vec::new(),
        };

        // 4. Build the fork candidates: parent-rooted exemplars plus their
        //    genesis-complete envs (the suffix-chain fold, memoized here so the
        //    schema-blind archive never composes). The pendings — and their
        //    seals — stay owned by `self.pending_forks` until step 5 transfers
        //    or drops each one, so an error can never orphan a handle.
        let mut forks: Vec<Fork> = Vec::with_capacity(self.pending_forks.len());
        for p in &self.pending_forks {
            let env = match &base_env {
                None => p.suffix.clone(),
                Some(base) => self.codec.compose(base, &p.suffix),
            };
            forks.push(Fork {
                exemplar: VirtualExemplar {
                    parent: base_snap,
                    seed: minted,
                    suffix: p.suffix.clone(),
                    at: p.at,
                },
                env,
                coverage: Some(CoverageView {
                    map: p.coverage.clone(),
                }),
            });
        }

        // 5. Timeline admission. The archive appends the forks it admits to the
        //    frontier in fork order (a subsequence); walk the forks against the
        //    new entries in lockstep — a fork matching the next unclaimed entry
        //    moves its seal into the cache under that entry's **stable id**,
        //    any other fork's seal is dropped. An archive that appended
        //    something that is *not* the next fork (a foreign entry) simply
        //    gets no seal and re-materializes on first exploit — robust, never
        //    a mis-assignment. Error-safe: a pending leaves the queue only once
        //    its seal is cached or dropped, so a mid-way `drop_snap` failure
        //    forgets nothing (the next `modulation` retries the leftovers).
        let before = self.archive.frontier().len();
        let reward = self
            .archive
            .admit(&trace, &forks, self.cells.as_ref(), &self.sensors);
        let new_entries: Vec<(ExemplarRef, VirtualExemplar)> = self
            .archive
            .frontier()
            .iter()
            .skip(before)
            .map(|(r, e)| (r, e.exemplar.clone()))
            .collect();
        let mut ni = 0usize;
        for fork in &forks {
            let admitted = new_entries
                .get(ni)
                .is_some_and(|(_, exemplar)| *exemplar == fork.exemplar);
            if admitted {
                let p = self.pending_forks.remove(0);
                // Cache the eager seal under the entry's stable id AND record
                // its lineage (task 68): parent = this Modulation's branch
                // base, suffix = the branch-local prefix env as of the fork.
                // The chain is what a later materialization walks once seals
                // start being evicted. A displaced seal is impossible under
                // stable ids (fresh entries never carry one), but a handle
                // must never leak — release it defensively if it exists.
                if let Some(old) =
                    self.mat
                        .register(new_entries[ni].0, p.seal, base_snap, p.suffix, p.at)
                {
                    self.machine.drop_snap(old)?;
                }
                ni += 1;
            } else {
                self.machine.drop_snap(self.pending_forks[0].seal)?;
                self.pending_forks.remove(0);
            }
        }

        // 6. Retention policy: the archive's own trim (reproducibility-safe; a
        //    no-op for the default archive), then sweep the seals of anything
        //    it evicted — a stable id can never be re-minted, so a dead ref's
        //    seal is provably orphaned and its handle is released here rather
        //    than leaked — then the task-68 pool budget (min-benefit seal
        //    eviction; a no-op under the default Unbounded budget). The
        //    selector's reward hook runs last.
        self.archive.evict();
        self.sweep_dead_seals()?;
        self.enforce_seal_budget()?;
        if let Some(r) = choice {
            self.selector.reward(r, reward);
        }

        // 7. The oracle's verdict over the finished, genesis-complete trace.
        Ok(self.oracle.judge(&trace))
    }

    /// Run the Progression for `steps` steps; return the distinct bugs found
    /// (deduplicated by fingerprint). Any [`MachineError`] aborts the whole
    /// campaign loudly (propagated), exactly as the two-result-categories rule
    /// requires.
    pub fn explore(&mut self, steps: u64) -> Result<Vec<Bug>, MachineError> {
        let mut bugs = Vec::new();
        let mut seen: BTreeSet<[u8; 32]> = BTreeSet::new();
        for _ in 0..steps {
            if let Some(bug) = self.progression_step()?
                && seen.insert(bug.fingerprint)
            {
                bugs.push(bug);
            }
        }
        Ok(bugs)
    }

    /// **The probe mechanism (task 75).** Run a directed liveness probe forward
    /// from a live terminal state on a **throwaway branch**, judge it purely over
    /// the probe's recorded trace, and discard the branch — the liveness is in
    /// *producing* the probe trace, not judging it (spine [`ProbeOracle`]).
    ///
    /// This is engine plumbing between the Progression and the [`Machine`] (like
    /// materialization — a function, not a loop change), NOT a `judge(&RunTrace)`
    /// call. Given a live `terminal` snapshot of the state to probe (the caller's
    /// chosen quiescent terminal) and its recorded `original` run:
    ///
    /// 1. Ask the oracle whether this state warrants a probe
    ///    ([`ProbeOracle::plan`]); `None` skips it (the common case).
    /// 2. [`branch`](Machine::branch) the throwaway forward exploration off
    ///    `terminal` with a **quiesced** env ([`EnvCodec::quiesce`] of the
    ///    original: same seed + policy, fault schedule stripped), so what it
    ///    observes is pure
    ///    convergence behaviour.
    /// 3. Run to `plan.horizon`'s deadline under a `StopMask::NONE` mask: every
    ///    decision (and the snapshot point) is masked out, so the seed answers
    ///    each decision locally, none surfaces, and the engine never stages a
    ///    resolve — the probe mints no snapshot and admits no exemplar.
    /// 4. Record the probe's [`RunTrace`], its `env` folded onto `original.env`
    ///    via [`EnvCodec::compose`] — genesis-complete, replaying the original
    ///    run *and* the failed convergence window.
    /// 5. Hand it to [`ProbeOracle::judge_probe`] (pure) and return the verdict.
    ///
    /// The probe run is **never** admitted to the [`Archive`], and this method
    /// touches neither the archive, the frontier, nor the seal cache: the
    /// archive/trunk digest (frontier + admitted exemplars + retained snapshot
    /// set) is byte-identical before and after (the uncontamination property the
    /// box gate proves). The `terminal` snapshot is the caller's — this method
    /// neither mints nor drops it; a campaign snapshots the terminal, probes, and
    /// drops it, leaving the snapshot pool identical.
    pub fn probe(
        &mut self,
        oracle: &dyn ProbeOracle,
        original: &RunTrace,
        terminal: SnapId,
    ) -> Result<Option<Bug>, MachineError> {
        let Some(plan) = oracle.plan(original) else {
            return Ok(None);
        };
        // Branch the throwaway forward exploration: restore the terminal state,
        // reseed with a **quiesced** view of the original env — same seed + fault
        // policy (so the probe's delta stays compose-compatible with
        // `original.env`; a fresh `seeded(0)` would panic the production codec's
        // seed-mismatch guard for a non-zero-seeded campaign), fault schedule
        // stripped (nominal, no faults). `branch` mints no snapshot.
        let quiesced = self.codec.quiesce(&original.env);
        self.machine.branch(terminal, &quiesced)?;
        let stop = self.run_probe_to_horizon(&plan)?;
        // The forward-window reproducer, keyed since the terminal branch, folded
        // onto the original's genesis-complete env (the task-93 compose ruling).
        let probe_delta = self.machine.recorded_env()?;
        let probe_trace = RunTrace {
            terminal: stop,
            env: self.codec.compose(&original.env, &probe_delta),
            coverage: Some(CoverageView {
                map: self.machine.coverage().to_vec(),
            }),
            events: Vec::new(),
            records: Vec::new(),
        };
        Ok(oracle.judge_probe(original, &probe_trace))
    }

    /// Drive the throwaway probe forward to its horizon under a **`StopMask::NONE`**
    /// mask: every decision class (and the snapshot point) is masked *out* of the
    /// probe horizon, so the quiesced env's seed answers each decision **locally**
    /// and none ever surfaces. The engine therefore never stages a `resolve` — in
    /// particular never an empty [`Answer`], which the production socket adapter
    /// decodes as an `environment::Answer` where **empty bytes are malformed** and
    /// would abort the probe. Masking the snapshot bit also keeps the probe
    /// snapshot-neutral for free. The caller's `plan.horizon` supplies only the
    /// convergence **deadline**; its decision/snapshot bits are deliberately
    /// dropped.
    fn run_probe_to_horizon(&mut self, plan: &ProbePlan) -> Result<StopReason, MachineError> {
        let horizon = StopConditions {
            deadline: plan.horizon.deadline,
            on: StopMask::NONE,
        };
        loop {
            match self.machine.run(&horizon, None)? {
                // Unreachable for a mask-honoring backend (NONE surfaces neither).
                // A backend that surfaces anyway is stepped past with a nominal
                // (seed) `None` answer — never a staged empty one.
                StopReason::Decision { .. } | StopReason::SnapshotPoint { .. } => continue,
                terminal => return Ok(terminal),
            }
        }
    }

    /// The seal materializing frontier entry `r`, minting one if needed. `r`
    /// must be **live** — a dead (evicted) or foreign ref fails loudly with
    /// [`MachineError::UnknownExemplar`], never a wrong snapshot (stable ids
    /// make aliasing impossible; see [`ExemplarRef`]). A live seal is returned
    /// as-is (the cheap path — the seal minted eagerly at the fork, or by a
    /// previous re-materialization). Otherwise the state is re-materialized
    /// **parent-rooted** (task 68): `branch` from the nearest **retained**
    /// ancestor with the suffix chain folded via [`EnvCodec::compose`], replay
    /// only that suffix to `exemplar.at` under [`StopMask::NONE`] (a pinned
    /// replay — recorded overrides pin what the run answered; the seed answers
    /// the rest; nothing surfaces), then seal. Genesis
    /// (`branch(genesis, entry.env)`) is reached only when no ancestor on the
    /// chain is retained — the graceful worst case. Determinism makes the
    /// result identical to the evicted seal, which is why eviction is never a
    /// correctness concern.
    pub fn materialize(&mut self, r: ExemplarRef) -> Result<SnapId, MachineError> {
        self.materialize_report(r).map(|(seal, _)| seal)
    }

    /// [`materialize`](Explorer::materialize), also returning the replay's
    /// depth accounting — `None` on a seal-cache hit (nothing was replayed).
    /// The report is what the box gates and the hot-path property measure:
    /// [`Materialization::depth`] is the issued replay depth, and
    /// [`Materialization::from_genesis`] marks the graceful worst case.
    pub fn materialize_report(
        &mut self,
        r: ExemplarRef,
    ) -> Result<(SnapId, Option<Materialization>), MachineError> {
        self.mat.materialize(
            &mut self.machine,
            self.codec.as_ref(),
            self.archive.frontier(),
            r,
        )
    }

    /// Release the seal of every frontier id that is no longer live (its entry
    /// was evicted by the archive). Ids are never reused, so an unresolvable
    /// id's seal is provably orphaned; dropping it is pure GC. Called after
    /// every [`Archive::evict`]; public so a custom driver interleaving its
    /// own eviction can GC at the same point. Error-safe: a mapping is removed
    /// only **after** its `drop_snap` succeeds, so a mid-way failure forgets
    /// nothing and the next sweep retries the leftovers.
    pub fn sweep_dead_seals(&mut self) -> Result<(), MachineError> {
        self.mat
            .sweep_dead(&mut self.machine, self.archive.frontier())
    }
}
