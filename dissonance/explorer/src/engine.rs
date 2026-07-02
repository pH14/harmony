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
//! ## Seals: the engine-side materialization cache
//!
//! A frontier entry is a *virtual* exemplar — kilobytes, never a resource. The
//! engine keeps the expensive half separately: a **seal** (a live [`SnapId`])
//! per materialized exemplar, minted eagerly at each admitted fork and re-minted
//! on demand. Dropping seals ([`Explorer::evict_seals`]) is **reproducibility-
//! safe** (spine invariant 4): a later exploit of a seal-less exemplar
//! re-materializes it from genesis — `branch(genesis, entry.env)` replayed to
//! `exemplar.at` under [`StopMask::NONE`] (a pinned replay, nothing surfaces) —
//! and determinism makes the re-materialized state identical. Retention is a
//! pure performance knob. (Suffix-only materialization from a live `parent` —
//! the ≪-genesis fast path — is the frontier task's box-gated mechanism, not
//! wired here.)

use std::collections::{BTreeMap, BTreeSet};

use crate::error::MachineError;
use crate::prng::Prng;
use crate::seam::{EnvCodec, Machine};
use crate::spine::{
    Archive, Bug, CellFn, CoverageView, DecisionPoint, ExemplarRef, Fork, Frontier, Moment, Oracle,
    RunTrace, Selector, Sensor, Tactic, VirtualExemplar,
};
use crate::{Answer, Environment, SnapId, StopConditions, StopMask, StopReason, VTime};

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
    /// The materialization cache: **stable** frontier id ([`ExemplarRef`]) →
    /// live seal. Keyed by identity, not position, so an [`Archive::evict`]
    /// that compacts the frontier can never re-point a seal at a different
    /// exemplar — a dead id's seal is merely orphaned, and
    /// [`sweep_dead_seals`](Explorer::sweep_dead_seals) releases it. Never a
    /// correctness surface — see [`evict_seals`](Explorer::evict_seals).
    seals: BTreeMap<u64, SnapId>,
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
            seals: BTreeMap::new(),
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
        self.seals.get(&r.0).copied()
    }

    /// How many frontier entries currently hold a live seal.
    pub fn sealed_count(&self) -> usize {
        self.seals.len()
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
        while let Some((&id, &seal)) = self.seals.first_key_value() {
            self.machine.drop_snap(seal)?;
            self.seals.remove(&id);
        }
        Ok(())
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
                self.seals.insert(new_entries[ni].0.0, p.seal);
                ni += 1;
            } else {
                self.machine.drop_snap(self.pending_forks[0].seal)?;
                self.pending_forks.remove(0);
            }
        }

        // 6. Retention policy (reproducibility-safe; a no-op for the default
        //    archive), then sweep the seals of anything it evicted — a stable
        //    id can never be re-minted, so a dead ref's seal is provably
        //    orphaned and its handle is released here rather than leaked. The
        //    selector's reward hook runs last.
        self.archive.evict();
        self.sweep_dead_seals()?;
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

    /// The seal materializing frontier entry `r`, minting one if needed. `r`
    /// must be **live** — a dead (evicted) or foreign ref fails loudly with
    /// [`MachineError::UnknownExemplar`], never a wrong snapshot (stable ids
    /// make aliasing impossible; see [`ExemplarRef`]). A live seal is returned
    /// as-is (the cheap path — the seal minted eagerly at the fork, or by a
    /// previous re-materialization). Otherwise the state is **re-materialized
    /// from genesis**: `branch(genesis, entry.env)` replayed to `exemplar.at`
    /// under [`StopMask::NONE`] — a pinned replay (recorded overrides pin what
    /// the run answered; the seed answers the rest; nothing surfaces) — then
    /// sealed. Determinism makes the result identical to the evicted seal,
    /// which is why eviction is never a correctness concern.
    pub fn materialize(&mut self, r: ExemplarRef) -> Result<SnapId, MachineError> {
        // Resolve the entry FIRST: a dead ref must error even if a stale seal
        // lingers (it cannot linger past the post-evict sweep, but the order
        // makes the guarantee locally evident).
        let (env, at) = match self.archive.frontier().get(r) {
            Some(entry) => (entry.env.clone(), entry.exemplar.at),
            None => return Err(MachineError::UnknownExemplar(r.0)),
        };
        if let Some(&seal) = self.seals.get(&r.0) {
            return Ok(seal);
        }
        self.machine.branch(self.genesis, &env)?;
        let until = StopConditions {
            deadline: Some(VTime(at.0)),
            on: StopMask::NONE,
        };
        // Nothing can surface under StopMask::NONE; loop defensively until the
        // terminal (Deadline) stop all the same.
        loop {
            if self.machine.run(&until, None)?.is_terminal() {
                break;
            }
        }
        let seal = self.machine.snapshot()?;
        self.seals.insert(r.0, seal);
        Ok(seal)
    }

    /// Release the seal of every frontier id that is no longer live (its entry
    /// was evicted by the archive). Ids are never reused, so an unresolvable
    /// id's seal is provably orphaned; dropping it is pure GC. Called after
    /// every [`Archive::evict`]; public so a custom driver interleaving its
    /// own eviction can GC at the same point. Error-safe: a mapping is removed
    /// only **after** its `drop_snap` succeeds, so a mid-way failure forgets
    /// nothing and the next sweep retries the leftovers.
    pub fn sweep_dead_seals(&mut self) -> Result<(), MachineError> {
        let dead: Vec<u64> = self
            .seals
            .keys()
            .copied()
            .filter(|&id| self.archive.frontier().get(ExemplarRef(id)).is_none())
            .collect();
        for id in dead {
            if let Some(&seal) = self.seals.get(&id) {
                self.machine.drop_snap(seal)?;
                self.seals.remove(&id);
            }
        }
        Ok(())
    }
}
