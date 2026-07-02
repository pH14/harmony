// SPDX-License-Identifier: AGPL-3.0-or-later
//! The engine: [`Explorer`] and the two loops, composed over the spine.
//!
//! [`Explorer::timeline`] drives one run to a terminal stop, answering each
//! surfaced decision through the open-loop [`Tactic`] and capturing every
//! sealable point as parent-rooted exemplar material.
//! [`Explorer::multiverse_step`] asks the [`Selector`] for the next branch
//! base, materializes it, mints the next [`Environment`] through the
//! [`EnvCodec`], runs one Timeline, folds the run into the [`Archive`]
//! (timeline admission), rewards the selector, and judges the run with the
//! [`Oracle`]. [`Explorer::explore`] runs the Multiverse for a bounded number
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

/// The result of one Timeline: where it stopped and the **branch-local**
/// reproducer [`Environment`] accumulated over it
/// ([`Machine::recorded_env`], keyed since the Timeline's branch origin). The
/// enclosing Multiverse step rebases it to genesis-complete (via
/// [`EnvCodec::compose`]) before it reaches a [`RunTrace`] or a [`Bug`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RunOutcome {
    /// The terminal [`StopReason`] the Timeline ended at.
    pub stop: StopReason,
    /// The reproducer accumulated over the run, keyed since the branch origin.
    pub env: Environment,
}

/// A sealable point captured mid-run, awaiting admission by the enclosing
/// Multiverse step: the eagerly-minted seal, the fork moment, the **prefix**
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
/// campaign stream. Owns the genesis snapshot every first-generation Timeline
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
    /// Sealable points captured this Timeline, awaiting admission by the
    /// enclosing [`multiverse_step`](Explorer::multiverse_step). A `Vec` (not a
    /// single slot) so a Timeline that forks more than once admits/drops
    /// *every* fork and never leaks a backend handle.
    pending_forks: Vec<PendingFork>,
    /// The materialization cache: frontier entry index → live seal. Never a
    /// correctness surface — see [`evict_seals`](Explorer::evict_seals).
    seals: BTreeMap<usize, SnapId>,
}

impl<M: Machine> Explorer<M> {
    /// Snapshot the freshly-spawned machine at its quiescent boot point → the
    /// **genesis [`SnapId`]**, the base every first-generation Timeline branches
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

    /// The genesis snapshot every first-generation Timeline branches from.
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

    /// The [`StopConditions`] used by [`multiverse_step`](Explorer::multiverse_step)
    /// and [`explore`](Explorer::explore).
    pub fn stop_conditions(&self) -> &StopConditions {
        &self.until
    }

    /// Set the [`StopConditions`] the Multiverse drives each Timeline with — e.g.
    /// [`StopMask::NONE`] for a pure seed-driven campaign, or a deadline.
    pub fn set_stop_conditions(&mut self, until: StopConditions) {
        self.until = until;
    }

    /// Direct access to the driven machine, for tests that branch/replay/hash it
    /// outside the loop (e.g. the Timeline-replay gate).
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
    pub fn evict_seals(&mut self) -> Result<(), MachineError> {
        for (_, seal) in std::mem::take(&mut self.seals) {
            self.machine.drop_snap(seal)?;
        }
        Ok(())
    }

    /// **Inner loop.** Drive one run from `base` to a terminal stop, answering
    /// each surfaced [`StopReason::Decision`] via the open-loop [`Tactic`] and
    /// sealing at any [`StopReason::SnapshotPoint`] (stored, with the prefix
    /// env/coverage as of the fork, for the enclosing Multiverse step to
    /// admit). Returns the terminal stop and the accumulated branch-local
    /// reproducer.
    pub fn timeline(
        &mut self,
        base: SnapId,
        env: &Environment,
        until: &StopConditions,
    ) -> Result<RunOutcome, MachineError> {
        // Drop any forks left pending by a prior *direct* `timeline` call
        // (only `multiverse_step` admits/drains them) so a repeated or aborted
        // direct run never leaks a backend handle — rather than a bare `clear()`
        // that would forget the seal without `drop_snap`.
        for pending in std::mem::take(&mut self.pending_forks) {
            self.machine.drop_snap(pending.seal)?;
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

    /// **Outer loop.** One Multiverse step: the [`Selector`] picks the branch
    /// base (a frontier exemplar, or genesis to explore), the engine
    /// materializes it and mints the next environment through the codec, runs
    /// one Timeline, admits the run's sealable forks into the [`Archive`]
    /// (dropping the seals of everything not admitted), rewards the selector,
    /// and returns the [`Oracle`]'s verdict. A [`MachineError`] aborts the step
    /// loudly and is never reported as a bug.
    pub fn multiverse_step(&mut self) -> Result<Option<Bug>, MachineError> {
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
                    // A stale/foreign reference is engine misuse by the
                    // selector — surfaced loudly, never papered over.
                    None => return Err(MachineError::UnknownExemplar(r.0 as u64)),
                };
                let salt = self.rng.next_u64();
                let env = self.codec.mutate(&entry_env, salt);
                let snap = self.materialize(r)?;
                (snap, Some(entry_env), salt, env)
            }
        };

        // 2. One Timeline.
        let outcome = self.timeline(base_snap, &branch_env, &until)?;

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
        //    schema-blind archive never composes).
        let pending = std::mem::take(&mut self.pending_forks);
        let mut forks: Vec<Fork> = Vec::with_capacity(pending.len());
        let mut fork_seals: Vec<SnapId> = Vec::with_capacity(pending.len());
        for p in pending {
            let env = match &base_env {
                None => p.suffix.clone(),
                Some(base) => self.codec.compose(base, &p.suffix),
            };
            forks.push(Fork {
                exemplar: VirtualExemplar {
                    parent: base_snap,
                    seed: minted,
                    suffix: p.suffix,
                    at: p.at,
                },
                env,
                coverage: Some(CoverageView { map: p.coverage }),
            });
            fork_seals.push(p.seal);
        }

        // 5. Timeline admission. The archive appends admitted exemplars to the
        //    frontier; pair each new entry back to its fork (admission preserves
        //    fork order) to keep its seal, and drop every seal not admitted.
        let before = self.archive.frontier().len();
        let reward = self
            .archive
            .admit(&trace, &forks, self.cells.as_ref(), &self.sensors);
        let mut keep: BTreeMap<usize, SnapId> = BTreeMap::new();
        {
            let frontier = self.archive.frontier();
            let mut fi = 0usize;
            for idx in before..frontier.len() {
                let Some(entry) = frontier.get(ExemplarRef(idx)) else {
                    break;
                };
                while fi < forks.len() && forks[fi].exemplar != entry.exemplar {
                    fi += 1;
                }
                if fi < forks.len() {
                    keep.insert(fi, fork_seals[fi]);
                    self.seals.insert(idx, fork_seals[fi]);
                    fi += 1;
                }
            }
        }
        for (i, seal) in fork_seals.iter().enumerate() {
            if !keep.contains_key(&i) {
                self.machine.drop_snap(*seal)?;
            }
        }

        // 6. Retention policy (reproducibility-safe; a no-op for the default
        //    archive) and the selector's reward hook.
        self.archive.evict();
        if let Some(r) = choice {
            self.selector.reward(r, reward);
        }

        // 7. The oracle's verdict over the finished, genesis-complete trace.
        Ok(self.oracle.judge(&trace))
    }

    /// Run the Multiverse for `steps` steps; return the distinct bugs found
    /// (deduplicated by fingerprint). Any [`MachineError`] aborts the whole
    /// campaign loudly (propagated), exactly as the two-result-categories rule
    /// requires.
    pub fn explore(&mut self, steps: u64) -> Result<Vec<Bug>, MachineError> {
        let mut bugs = Vec::new();
        let mut seen: BTreeSet<[u8; 32]> = BTreeSet::new();
        for _ in 0..steps {
            if let Some(bug) = self.multiverse_step()?
                && seen.insert(bug.fingerprint)
            {
                bugs.push(bug);
            }
        }
        Ok(bugs)
    }

    /// The seal materializing frontier entry `r`, minting one if needed. A live
    /// seal is returned as-is (the cheap path — the seal minted eagerly at the
    /// fork, or by a previous re-materialization). Otherwise the state is
    /// **re-materialized from genesis**: `branch(genesis, entry.env)` replayed
    /// to `exemplar.at` under [`StopMask::NONE`] — a pinned replay (recorded
    /// overrides pin what the run answered; the seed answers the rest; nothing
    /// surfaces) — then sealed. Determinism makes the result identical to the
    /// evicted seal, which is why eviction is never a correctness concern.
    pub fn materialize(&mut self, r: ExemplarRef) -> Result<SnapId, MachineError> {
        if let Some(&seal) = self.seals.get(&r.0) {
            return Ok(seal);
        }
        let (env, at) = match self.archive.frontier().get(r) {
            Some(entry) => (entry.env.clone(), entry.exemplar.at),
            None => return Err(MachineError::UnknownExemplar(r.0 as u64)),
        };
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
}
