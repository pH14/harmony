// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **lazy-materialization engine** (task 68): the mechanism between
//! `Selector::choose` and `Machine::branch` — an engine mechanism, not a trait
//! (the `docs/EXPLORATION.md` ruling).
//!
//! A frontier entry is a *virtual* exemplar — kilobytes. [`Materializer`] owns
//! the expensive half: the **seal pool** (live [`SnapId`]s per materialized
//! exemplar), the **lineage table** (`SnapId → {parent, suffix, at}`,
//! genesis-rooted chains in a `BTreeMap`), and the **spanning-ancestor
//! retention policy** (Agamotto cost/benefit over integer replay depth).
//!
//! ## The hot path
//!
//! Materialize = `branch(parent, suffix)` → replay **only the suffix** to
//! `at` → seal. When the direct parent's seal has been evicted, the lineage
//! table — which deliberately **outlives eviction** — locates the nearest
//! *retained* ancestor and the engine folds
//! [`EnvCodec::compose`] over the suffix chain from that ancestor down to the
//! target: **one branch + one run, never a re-seal per hop**. Genesis is
//! reached only when no ancestor on the chain is retained (the graceful worst
//! case, replayed via the entry's memoized genesis-complete env) — never as
//! the routine path.
//!
//! ## The task-63 ruling (GO, grid-restricted)
//!
//! `SEAL-RATE-REPORT.md` §10 ruled **GO (grid-restricted)**: exemplars key to
//! the **nearest synchronized boundary**, which `run(deadline) → seal` lands
//! on by construction — so any admitted `at` is a boundary of its own recorded
//! trajectory and an identical replay stops exactly there (checked loudly:
//! [`MachineError::MaterializeDivergence`]). The seam for the RESTRICTED arm
//! is kept: an injected `sealable(Moment)` predicate (default always-true)
//! gates **every** seal the engine takes — the eager fork seals and the
//! materialization target — and a non-`sealable` exemplar is refused loudly
//! ([`MachineError::NotSealable`]), never silently sealed. A seal failure at
//! an admissible point is a task-41/63 regression: it propagates loudly for
//! escalation, it is never patched here.
//!
//! ## The retention pool
//!
//! Retain by **expected re-execution time saved** (Agamotto): the benefit of
//! keeping a seal is the frontier-weighted replay depth it saves descendants,
//! where an exemplar's materialization cost is the replay depth from its
//! nearest retained ancestor. Over budget, the minimum-benefit seal is
//! evicted, deterministic tie-break by [`SnapId`]. The budget is a function of
//! the **active frontier** ([`SealBudget`]), never of archive size. Cost unit
//! is **`Moment` deltas** (retired work) — never wall-clock — and everything
//! is integer arithmetic over `BTreeMap` order (determinism discipline).
//! Degradation is graceful: eviction only lengthens suffixes, up to the
//! genesis bound; it can never make an exemplar unmaterializable.

use std::collections::BTreeMap;

use crate::error::MachineError;
use crate::seam::{EnvCodec, Machine};
use crate::spine::{ExemplarRef, Frontier, Moment, VirtualExemplar};
use crate::{Environment, SnapId, StopConditions, StopMask, VTime};

/// One lineage record: how a sealed snapshot was produced — its parent seal
/// (or genesis), the branch-local suffix that took the parent to `at`, and the
/// moment it was sealed at. Records are **never removed**: the chain must
/// outlive the eviction of any seal on it, or an evicted parent would strand
/// its descendants on the genesis worst case forever.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Lineage {
    /// The snapshot this seal was branched from (a prior seal, or genesis).
    pub parent: SnapId,
    /// The branch-local delta replayed from `parent` to reach `at`
    /// (tail-complete, the task-93 compose contract).
    pub suffix: Environment,
    /// The moment the seal was taken at.
    pub at: Moment,
}

/// The depth accounting of one materialization that actually ran (a seal-cache
/// hit reports nothing — it replayed nothing).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Materialization {
    /// The snapshot branched from: the nearest **retained** ancestor.
    pub base: SnapId,
    /// The moment of `base`.
    pub base_at: Moment,
    /// The exemplar's target moment (where the new seal was taken).
    pub at: Moment,
    /// How many [`EnvCodec::compose`] folds built the replayed env (`0` on the
    /// direct-parent hot path and on the genesis worst case, which replays the
    /// entry's memoized genesis-complete env).
    pub folded: u64,
    /// Whether no ancestor on the chain was retained, so genesis was replayed
    /// (the graceful worst case — never the routine path).
    pub from_genesis: bool,
}

impl Materialization {
    /// The issued replay depth in `Moment` units — the retention policy's cost
    /// unit (retired work, never wall-clock).
    pub fn depth(&self) -> u64 {
        self.at.0.saturating_sub(self.base_at.0)
    }
}

/// The retention pool's budget: how many seals may be retained, as a function
/// of the **live frontier** (never the archive — spine invariant: snapshot
/// count is bounded by the active frontier).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SealBudget {
    /// Never evict by policy — the engine default, preserving the eager
    /// seal-per-admission behavior (retention then moves only by explicit
    /// [`Materializer::evict_seal`]/[`Materializer::evict_all`] calls).
    Unbounded,
    /// `budget(n) = base + n·num/den` over the live frontier count `n`
    /// (integer, saturating; a zero `den` is treated as `1`).
    Frontier {
        /// The unconditional floor of the pool.
        base: u64,
        /// Numerator of the per-entry fraction.
        num: u64,
        /// Denominator of the per-entry fraction (`0` is treated as `1`).
        den: u64,
    },
}

impl SealBudget {
    /// The retained-seal cap for a frontier of `live` entries.
    pub fn of(&self, live: usize) -> usize {
        match *self {
            SealBudget::Unbounded => usize::MAX,
            SealBudget::Frontier { base, num, den } => {
                let n = live as u64;
                let cap = base.saturating_add(n.saturating_mul(num) / den.max(1));
                usize::try_from(cap).unwrap_or(usize::MAX)
            }
        }
    }
}

/// Where a chain walk found its branch base.
struct Walk {
    /// The live handle to branch from (a retained seal, or genesis).
    base: SnapId,
    /// The moment of `base`.
    base_at: Moment,
    /// The **dead** intermediate seals between `base` and the exemplar's
    /// parent, ordered top-down (nearest `base` first) — the suffixes to fold.
    fold: Vec<u64>,
    /// `base` is genesis: no ancestor on the chain is retained.
    from_genesis: bool,
}

/// The materialization engine + spanning-ancestor retention pool (module doc).
///
/// Deliberately **Progression-blind**: it sees opaque [`SnapId`]s, [`Moment`]s,
/// opaque [`Environment`] blobs, and integer costs — no fault types, no signal
/// channels, no cell meaning. [`Explorer`](crate::Explorer) embeds one; a
/// driver that builds its own chains (the conductor's live harness) drives one
/// directly over any [`Machine`] + [`EnvCodec`].
pub struct Materializer {
    genesis: SnapId,
    genesis_at: Moment,
    /// `SnapId → Lineage` for every seal ever registered; genesis-rooted
    /// chains. Never pruned (kilobytes; the chain outlives eviction).
    lineage: BTreeMap<u64, Lineage>,
    /// `SnapId → ExemplarRef`: which frontier entry a seal materializes. A
    /// chain names the **original** `SnapId`s; when one was evicted and its
    /// entry later re-materialized under a fresh handle, this indirection is
    /// what lets the walk find the fresh seal — stable ids make it exact.
    owner: BTreeMap<u64, ExemplarRef>,
    /// The pool: stable frontier id → live seal.
    seals: BTreeMap<u64, SnapId>,
    /// The task-63 `sealable(Moment)` seam (default always-true — the GO
    /// arm; the ruling's grid restriction is structural: exemplars key to
    /// observed synchronized boundaries).
    sealable: Box<dyn Fn(Moment) -> bool>,
    /// The retention budget (default [`SealBudget::Unbounded`]).
    budget: SealBudget,
}

impl Materializer {
    /// A pool rooted at `genesis` (always retained, never evictable), whose
    /// moment `genesis_at` is the genesis bound of the cost model.
    pub fn new(genesis: SnapId, genesis_at: Moment) -> Self {
        Materializer {
            genesis,
            genesis_at,
            lineage: BTreeMap::new(),
            owner: BTreeMap::new(),
            seals: BTreeMap::new(),
            sealable: Box::new(|_| true),
            budget: SealBudget::Unbounded,
        }
    }

    /// The genesis handle the pool is rooted at.
    pub fn genesis(&self) -> SnapId {
        self.genesis
    }

    /// The genesis moment (the cost model's worst-case bound origin).
    pub fn genesis_at(&self) -> Moment {
        self.genesis_at
    }

    /// Set the genesis moment (a driver that probed the live origin records it
    /// here; the default is `Moment(0)`). Policy-only: it scales the genesis
    /// bound of the cost model, never correctness (eviction is always safe).
    pub fn set_genesis_at(&mut self, at: Moment) {
        self.genesis_at = at;
    }

    /// Inject the task-63 `sealable(Moment)` predicate (the RESTRICTED-arm
    /// seam; default always-true per the GO ruling). Gates **every** seal this
    /// engine takes; compose with
    /// [`CoverageArchive::with_sealable`](crate::CoverageArchive::with_sealable)
    /// so admission agrees.
    pub fn set_sealable(&mut self, sealable: Box<dyn Fn(Moment) -> bool>) {
        self.sealable = sealable;
    }

    /// Whether the injected predicate admits a seal at `at`.
    pub fn sealable_at(&self, at: Moment) -> bool {
        (self.sealable)(at)
    }

    /// Set the retention budget (default [`SealBudget::Unbounded`]).
    pub fn set_budget(&mut self, budget: SealBudget) {
        self.budget = budget;
    }

    /// The current retention budget.
    pub fn budget(&self) -> SealBudget {
        self.budget
    }

    /// The live seal materializing frontier entry `r`, if any.
    pub fn seal_of(&self, r: ExemplarRef) -> Option<SnapId> {
        self.seals.get(&r.0).copied()
    }

    /// How many frontier entries currently hold a live seal.
    pub fn sealed_count(&self) -> usize {
        self.seals.len()
    }

    /// The lineage record of a seal (also for evicted ones — records outlive
    /// eviction), for inspection/diagnostics.
    pub fn lineage_of(&self, snap: SnapId) -> Option<&Lineage> {
        self.lineage.get(&snap.0)
    }

    /// Record a freshly-minted seal for entry `r`: `seal` was taken at `at` on
    /// a branch of `parent` replaying `suffix`. Returns the seal `r` previously
    /// held, if any — the caller owns dropping that displaced handle (under
    /// stable ids the engine never displaces; a driver re-registering an entry
    /// must release it or leak).
    pub fn register(
        &mut self,
        r: ExemplarRef,
        seal: SnapId,
        parent: SnapId,
        suffix: Environment,
        at: Moment,
    ) -> Option<SnapId> {
        let displaced = self.seals.insert(r.0, seal);
        self.owner.insert(seal.0, r);
        self.lineage.insert(seal.0, Lineage { parent, suffix, at });
        displaced
    }

    /// Walk `ex`'s ancestor chain to the nearest **retained** ancestor
    /// (optionally pretending `excluding` were evicted — the benefit model's
    /// counterfactual), collecting the dead intermediates whose suffixes a
    /// materialization must fold. Chains are genesis-rooted by construction;
    /// an unknown link or an (impossible) cycle degrades gracefully to the
    /// genesis worst case — never an error, never unmaterializable.
    fn nearest_retained(&self, ex: &VirtualExemplar, excluding: Option<SnapId>) -> Walk {
        let genesis_walk = |fold: Vec<u64>| Walk {
            base: self.genesis,
            base_at: self.genesis_at,
            fold,
            from_genesis: true,
        };
        let mut fold_up: Vec<u64> = Vec::new();
        let mut cur = ex.parent;
        // Defensive cap: lineage chains strictly shorten toward genesis, so
        // any walk longer than the table is a corrupted chain — degrade.
        let mut hops = self.lineage.len() + 1;
        loop {
            if cur == self.genesis {
                fold_up.reverse();
                return genesis_walk(fold_up);
            }
            if let Some(r) = self.owner.get(&cur.0)
                && let Some(&live) = self.seals.get(&r.0)
                && Some(live) != excluding
            {
                // Retained: possibly under a re-minted handle — the owner
                // indirection resolves the original chain id to the live seal
                // of the same entry (same state, same moment).
                let base_at = self
                    .lineage
                    .get(&cur.0)
                    .map(|l| l.at)
                    .unwrap_or(self.genesis_at);
                fold_up.reverse();
                return Walk {
                    base: live,
                    base_at,
                    fold: fold_up,
                    from_genesis: false,
                };
            }
            match self.lineage.get(&cur.0) {
                Some(l) if hops > 0 => {
                    hops -= 1;
                    fold_up.push(cur.0);
                    cur = l.parent;
                }
                // Unknown link / exhausted cap: the graceful worst case (the
                // fold list is irrelevant — genesis replays the entry's
                // memoized genesis-complete env).
                _ => return genesis_walk(Vec::new()),
            }
        }
    }

    /// The modeled materialization cost of entry `r` in `Moment` units: `0`
    /// when its own seal is retained, else the replay depth from the nearest
    /// retained ancestor (the genesis bound at worst). `None` if `r` is not
    /// live in `frontier`.
    pub fn modeled_cost(&self, frontier: &Frontier, r: ExemplarRef) -> Option<u64> {
        let entry = frontier.get(r)?;
        Some(self.cost_excluding(&entry.exemplar, r, None))
    }

    /// [`modeled_cost`](Self::modeled_cost) under the counterfactual that
    /// `excluding` were evicted.
    fn cost_excluding(
        &self,
        ex: &VirtualExemplar,
        r: ExemplarRef,
        excluding: Option<SnapId>,
    ) -> u64 {
        if let Some(&own) = self.seals.get(&r.0)
            && Some(own) != excluding
        {
            return 0;
        }
        let walk = self.nearest_retained(ex, excluding);
        ex.at.0.saturating_sub(walk.base_at.0)
    }

    /// The seal for entry `r`, materializing it if needed. Returns the live
    /// handle plus the depth accounting of the replay that ran (`None` on a
    /// seal-cache hit).
    ///
    /// The replay is **parent-rooted** (module doc): `branch` from the nearest
    /// retained ancestor with the (fold-composed) suffix, `run` to `at` under
    /// [`StopMask::NONE`] (a pinned replay — recorded overrides pin what the
    /// run answered, the seed answers the rest, nothing surfaces), then seal
    /// and record lineage. Genesis is replayed only when no ancestor is
    /// retained.
    ///
    /// # Errors
    /// [`MachineError::UnknownExemplar`] on a dead/foreign ref (loud, never a
    /// wrong snapshot); [`MachineError::NotSealable`] when the injected
    /// predicate rejects `at`; [`MachineError::MaterializeDivergence`] when
    /// the replay lands at a different moment than `at` (a determinism/keying
    /// violation — escalation material); any backend failure propagates.
    pub fn materialize<M: Machine>(
        &mut self,
        machine: &mut M,
        codec: &dyn EnvCodec,
        frontier: &Frontier,
        r: ExemplarRef,
    ) -> Result<(SnapId, Option<Materialization>), MachineError> {
        // Resolve the entry FIRST: a dead ref must error even if a stale seal
        // lingered (it cannot, past the sweep, but the order keeps the
        // guarantee locally evident).
        let Some(entry) = frontier.get(r) else {
            return Err(MachineError::UnknownExemplar(r.0));
        };
        if let Some(&seal) = self.seals.get(&r.0) {
            return Ok((seal, None));
        }
        let at = entry.exemplar.at;
        if !(self.sealable)(at) {
            // The RESTRICTED-arm refusal: such an exemplar should never have
            // been admitted (the Archive keys on the predicate, task 64).
            return Err(MachineError::NotSealable(at.0));
        }

        let walk = self.nearest_retained(&entry.exemplar, None);
        let (env, folded) = if walk.from_genesis {
            // The graceful worst case: the entry's memoized genesis-complete
            // env replays the whole prefix — determinism makes the result
            // identical to any evicted seal, which is why retention is a pure
            // performance knob.
            (entry.env.clone(), 0u64)
        } else if walk.fold.is_empty() {
            // The hot path: the direct parent is retained; replay only the
            // exemplar's own suffix.
            (entry.exemplar.suffix.clone(), 0u64)
        } else {
            // Dead intermediates: fold compose over their suffixes from the
            // nearest retained ancestor down to the target — one branch + one
            // run, not a re-seal per hop. Lineage entries for walk.fold ids
            // exist by construction (the walk traversed them).
            let mut env = self.lineage[&walk.fold[0]].suffix.clone();
            let mut folded = 0u64;
            for mid in &walk.fold[1..] {
                // A malformed lineage suffix aborts the materialization as a
                // loud control error (`MachineError::EnvCodec`), never a bug
                // (task 99).
                env = codec.compose(&env, &self.lineage[mid].suffix)?;
                folded += 1;
            }
            env = codec.compose(&env, &entry.exemplar.suffix)?;
            folded += 1;
            (env, folded)
        };

        machine.branch(walk.base, &env)?;
        let until = StopConditions {
            deadline: Some(VTime(at.0)),
            on: StopMask::NONE,
        };
        // Nothing can surface under StopMask::NONE; loop defensively until the
        // terminal (Deadline) stop all the same.
        let landed = loop {
            let stop = machine.run(&until, None)?;
            if stop.is_terminal() {
                break stop.vtime();
            }
        };
        // GO grid-restricted: `at` is a synchronized boundary of the
        // exemplar's own recorded trajectory, so the identical replay must
        // stop exactly there. Anything else is a determinism/keying violation
        // — loud, escalation material (a task-41/63 regression class).
        if landed.0 != at.0 {
            return Err(MachineError::MaterializeDivergence {
                exemplar: r.0,
                at: at.0,
                landed: landed.0,
            });
        }
        let seal = machine.snapshot()?;
        let displaced = self.register(r, seal, walk.base, env, at);
        debug_assert!(displaced.is_none(), "the seal cache was checked above");
        Ok((
            seal,
            Some(Materialization {
                base: walk.base,
                base_at: walk.base_at,
                at,
                folded,
                from_genesis: walk.from_genesis,
            }),
        ))
    }

    /// Drop entry `r`'s seal, if it holds one; returns the dropped handle.
    /// Always reproducibility-safe: the entry re-materializes on its next use,
    /// identically. Lineage records are kept (the chain outlives eviction).
    pub fn evict_seal<M: Machine>(
        &mut self,
        machine: &mut M,
        r: ExemplarRef,
    ) -> Result<Option<SnapId>, MachineError> {
        let Some(&seal) = self.seals.get(&r.0) else {
            return Ok(None);
        };
        machine.drop_snap(seal)?;
        self.seals.remove(&r.0);
        Ok(Some(seal))
    }

    /// Drop **every** live seal (the aggressive end of the retention knob).
    /// Error-safe: a mapping is removed only after its `drop_snap` succeeds,
    /// so a mid-way backend failure forgets nothing and the call is retryable.
    pub fn evict_all<M: Machine>(&mut self, machine: &mut M) -> Result<(), MachineError> {
        while let Some((&id, &seal)) = self.seals.first_key_value() {
            machine.drop_snap(seal)?;
            self.seals.remove(&id);
        }
        Ok(())
    }

    /// Release the seal of every frontier id no longer live in `frontier`
    /// (its entry was evicted by the archive). Ids are never reused, so an
    /// unresolvable id's seal is provably orphaned; dropping it is pure GC.
    /// Error-safe like [`evict_all`](Self::evict_all).
    pub fn sweep_dead<M: Machine>(
        &mut self,
        machine: &mut M,
        frontier: &Frontier,
    ) -> Result<(), MachineError> {
        let dead: Vec<u64> = self
            .seals
            .keys()
            .copied()
            .filter(|&id| frontier.get(ExemplarRef(id)).is_none())
            .collect();
        for id in dead {
            if let Some(&seal) = self.seals.get(&id) {
                machine.drop_snap(seal)?;
                self.seals.remove(&id);
            }
        }
        Ok(())
    }

    /// The Agamotto benefit of retaining `seal`: the summed replay depth the
    /// live frontier would additionally pay were it evicted (the expected
    /// re-execution saved, in `Moment` units). Integer, deterministic.
    fn benefit(&self, frontier: &Frontier, seal: SnapId) -> u128 {
        let mut total: u128 = 0;
        for (er, entry) in frontier.iter() {
            let with = self.cost_excluding(&entry.exemplar, er, None);
            let without = self.cost_excluding(&entry.exemplar, er, Some(seal));
            total += u128::from(without.saturating_sub(with));
        }
        total
    }

    /// Enforce the retention budget: while the pool exceeds
    /// [`SealBudget::of`] the live frontier, evict the **minimum-benefit**
    /// seal (deterministic tie-break by [`SnapId`]). Returns the evicted
    /// handles, oldest eviction first. Reproducibility-safe by construction —
    /// eviction only lengthens suffixes, up to the genesis bound.
    pub fn enforce_budget<M: Machine>(
        &mut self,
        machine: &mut M,
        frontier: &Frontier,
    ) -> Result<Vec<SnapId>, MachineError> {
        let cap = self.budget.of(frontier.len());
        let mut evicted = Vec::new();
        while self.seals.len() > cap {
            let mut victim: Option<(u128, SnapId, u64)> = None;
            for (&ref_id, &seal) in &self.seals {
                let benefit = self.benefit(frontier, seal);
                let better = match &victim {
                    None => true,
                    Some((b, s, _)) => benefit < *b || (benefit == *b && seal < *s),
                };
                if better {
                    victim = Some((benefit, seal, ref_id));
                }
            }
            let Some((_, seal, ref_id)) = victim else {
                break; // unreachable: the loop guard implies a non-empty pool
            };
            machine.drop_snap(seal)?;
            self.seals.remove(&ref_id);
            evicted.push(seal);
        }
        Ok(evicted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spine::FrontierEntry;
    use crate::spine::Reward;

    fn env(bytes: Vec<u8>) -> Environment {
        Environment {
            blob_version: 1,
            bytes,
        }
    }

    fn exemplar(parent: SnapId, at: u64) -> VirtualExemplar {
        VirtualExemplar {
            parent,
            seed: 0,
            suffix: env(vec![]),
            at: Moment(at),
        }
    }

    fn entry(parent: SnapId, at: u64) -> FrontierEntry {
        FrontierEntry {
            exemplar: exemplar(parent, at),
            env: env(vec![]),
            reward: Reward { new_cells: 1 },
        }
    }

    /// The budget formula, pinned: unbounded never caps; the frontier form is
    /// `base + n·num/den` with integer division and a zero-den guard.
    #[test]
    fn budget_formula_is_pinned() {
        assert_eq!(SealBudget::Unbounded.of(0), usize::MAX);
        assert_eq!(SealBudget::Unbounded.of(1_000_000), usize::MAX);
        let b = SealBudget::Frontier {
            base: 2,
            num: 1,
            den: 4,
        };
        assert_eq!(b.of(0), 2);
        assert_eq!(b.of(3), 2, "3/4 floors to 0");
        assert_eq!(b.of(4), 3);
        assert_eq!(b.of(9), 4);
        let zero_den = SealBudget::Frontier {
            base: 0,
            num: 1,
            den: 0,
        };
        assert_eq!(zero_den.of(5), 5, "den 0 is treated as 1, never a panic");
        let sat = SealBudget::Frontier {
            base: u64::MAX,
            num: u64::MAX,
            den: 1,
        };
        assert_eq!(sat.of(usize::MAX), usize::MAX, "saturates, never wraps");
    }

    /// The chain walk: own seal ⇒ cost 0; nearest retained ancestor prices the
    /// depth; the counterfactual exclusion falls through to the next ancestor
    /// (and to the genesis bound when nothing else is retained); a re-minted
    /// handle is found through the owner indirection.
    #[test]
    fn cost_model_walks_to_the_nearest_retained_ancestor() {
        let genesis = SnapId(1);
        let mut m = Materializer::new(genesis, Moment(100));
        let mut f = Frontier::new();

        // Chain: genesis(100) → E0 @ 200 (seal 10) → E1 @ 350 (seal 11).
        let r0 = f.insert(entry(genesis, 200));
        assert_eq!(
            m.register(r0, SnapId(10), genesis, env(vec![]), Moment(200)),
            None
        );
        let r1 = f.insert(entry(SnapId(10), 350));
        assert_eq!(
            m.register(r1, SnapId(11), SnapId(10), env(vec![]), Moment(350)),
            None
        );

        // Own seals live: both cost 0.
        assert_eq!(m.modeled_cost(&f, r0), Some(0));
        assert_eq!(m.modeled_cost(&f, r1), Some(0));
        assert_eq!(m.modeled_cost(&f, ExemplarRef(99)), None, "dead ref");

        // Excluding r1's own seal: nearest retained is its parent (seal 10).
        assert_eq!(
            m.cost_excluding(&f.get(r1).unwrap().exemplar, r1, Some(SnapId(11))),
            350 - 200
        );
        // Excluding the parent instead: r1's own seal still zeroes it.
        assert_eq!(
            m.cost_excluding(&f.get(r1).unwrap().exemplar, r1, Some(SnapId(10))),
            0
        );

        // Evict the parent's seal: r1 still costs 0 (own seal), r0's next
        // ancestor is genesis (the bound).
        assert_eq!(
            m.cost_excluding(&f.get(r0).unwrap().exemplar, r0, Some(SnapId(10))),
            200 - 100
        );

        // Benefit accounting: seal 10 saves r0 its genesis depth (100) and r1
        // nothing (own seal); seal 11 saves r1 its parent depth (150).
        assert_eq!(m.benefit(&f, SnapId(10)), 100);
        assert_eq!(m.benefit(&f, SnapId(11)), 150);
    }

    /// A dead intermediate is folded through, and the owner indirection finds
    /// a re-minted seal for an original chain id.
    #[test]
    fn walk_resolves_re_minted_seals_through_the_owner_map() {
        let genesis = SnapId(1);
        let mut m = Materializer::new(genesis, Moment(0));
        let mut f = Frontier::new();

        let r0 = f.insert(entry(genesis, 10));
        m.register(r0, SnapId(10), genesis, env(vec![0]), Moment(10));
        let r1 = f.insert(entry(SnapId(10), 30));
        m.register(r1, SnapId(11), SnapId(10), env(vec![1]), Moment(30));

        // Simulate: seal 10 evicted, then r0 re-materialized under seal 20.
        m.seals.remove(&r0.0);
        m.register(r0, SnapId(20), genesis, env(vec![0]), Moment(10));

        // r1's exemplar names the ORIGINAL SnapId(10); the walk must resolve
        // it to the live re-minted handle via owner, at the same moment.
        m.seals.remove(&r1.0); // force a real walk
        let walk = m.nearest_retained(&f.get(r1).unwrap().exemplar, None);
        assert!(!walk.from_genesis);
        assert_eq!(
            walk.base,
            SnapId(20),
            "the re-minted handle, not the dead id"
        );
        assert_eq!(walk.base_at, Moment(10));
        assert!(walk.fold.is_empty(), "direct parent — nothing to fold");

        // With r0's seal also gone, the walk degrades to genesis and lists the
        // dead intermediate for folding.
        m.seals.remove(&r0.0);
        let walk = m.nearest_retained(&f.get(r1).unwrap().exemplar, None);
        assert!(walk.from_genesis);
        assert_eq!(walk.base, genesis);

        // An exemplar whose parent chain has an unknown link degrades to the
        // genesis worst case, never an error.
        let stray = exemplar(SnapId(777), 50);
        let walk = m.nearest_retained(&stray, None);
        assert!(walk.from_genesis);
        assert_eq!(walk.base, genesis);
    }
}
