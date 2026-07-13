// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task-68 gate 2 — the **lazy-materialization proptests** (≥256 cases each),
//! against the toy `Machine` wrapped in a replay-cost meter (synthetic replay
//! costs: the meter observes every issued branch→run→seal and its depth):
//!
//! (a) **Hot-path property** — the issued replay depth always equals the
//!     distance to the nearest **retained** ancestor (checked three ways: the
//!     engine's report, its `modeled_cost`, and the meter's observation of
//!     what the machine actually replayed), and genesis is replayed only when
//!     no ancestor is retained. Exercised both through `Explorer` campaigns
//!     and through deep (3–7 hop) hand-built chains that force the
//!     compose-fold path.
//! (b) **Eviction safety** — a campaign under a tight pool budget plus random
//!     per-step seal evictions yields byte-identical bug fingerprints,
//!     reproducers, and admissions to one that never evicts.
//! (c) **Pool bound** — the retained count never exceeds the
//!     frontier-derived budget after any step, and modeled cost degrades
//!     monotonically toward the genesis bound as seals are evicted.
//! (d) **RESTRICTED arm** — under a synthetic `sealable` predicate the engine
//!     never *attempts* a seal at an inadmissible `Moment` (fork seals are
//!     stepped past; materialization refuses loudly with `NotSealable`).

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use common::{ToyCodec, ToyMachine, config, pin_composition};
use explorer::{
    Answer, Composition, CoverageArchive, EnvCodec, Reproducer, ExemplarRef, Explorer,
    Frontier, FrontierEntry, IdentityCells, Machine, MachineError, Materializer, Moment,
    Prng, Reward, SealBudget, SnapId, StopConditions, StopMask, StopReason, TerminalOracle,
    VirtualExemplar,
};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// The replay-cost meter: a Machine wrapper observing every branch/run/seal.
// ---------------------------------------------------------------------------

/// What the machine was actually asked to do — the "synthetic replay cost"
/// ledger the hot-path property checks the engine's accounting against.
#[derive(Debug, Default)]
struct Meter {
    /// `SnapId` → the V-time it was sealed at.
    snap_at: BTreeMap<u64, u64>,
    /// V-time of the base of the current branch/replay.
    branch_at: u64,
    /// V-time of the most recent stop.
    last_stop: u64,
    /// One `(branch_at, seal_at)` per `snapshot()` call, in call order —
    /// `seal_at − branch_at` is the replay depth actually paid for that seal.
    seals: Vec<(u64, u64)>,
}

/// A [`ToyMachine`] wrapped in a shared [`Meter`].
struct MeterMachine {
    inner: ToyMachine,
    meter: Rc<RefCell<Meter>>,
}

impl MeterMachine {
    fn new() -> (Self, Rc<RefCell<Meter>>) {
        let meter = Rc::new(RefCell::new(Meter::default()));
        (
            MeterMachine {
                inner: ToyMachine::new(),
                meter: Rc::clone(&meter),
            },
            meter,
        )
    }
}

impl Machine for MeterMachine {
    fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), MachineError> {
        self.inner.branch(snap, env)?;
        let mut m = self.meter.borrow_mut();
        let at = m.snap_at.get(&snap.0).copied().unwrap_or(0);
        m.branch_at = at;
        m.last_stop = at;
        Ok(())
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.inner.replay(snap)?;
        let mut m = self.meter.borrow_mut();
        let at = m.snap_at.get(&snap.0).copied().unwrap_or(0);
        m.branch_at = at;
        m.last_stop = at;
        Ok(())
    }

    fn run(
        &mut self,
        until: &StopConditions,
        resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError> {
        let stop = self.inner.run(until, resolve)?;
        self.meter.borrow_mut().last_stop = stop.vtime().0;
        Ok(stop)
    }

    fn snapshot(&mut self) -> Result<SnapId, MachineError> {
        let id = self.inner.snapshot()?;
        let mut m = self.meter.borrow_mut();
        let (branch_at, at) = (m.branch_at, m.last_stop);
        m.snap_at.insert(id.0, at);
        m.seals.push((branch_at, at));
        Ok(id)
    }

    fn drop_snap(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.inner.drop_snap(snap)
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
}

// ---------------------------------------------------------------------------
// (a2) Deep chains: the compose-fold path, driven on the Materializer directly
// (the toy campaign only mints 2-deep chains; the fold needs ≥ 3).
// ---------------------------------------------------------------------------

/// Build an `n`-hop chain over the raw driver verbs — exactly the archive's
/// live pattern (`branch → run(deadline) → seal` per hop, every suffix a real
/// `recorded_env`) — registering each hop as a frontier entry + lineage
/// record. Hop `i` seals at V-time `(i+1)·10`.
fn build_chain(
    machine: &mut MeterMachine,
    mat: &mut Materializer,
    frontier: &mut Frontier,
    seed: u64,
    hops: usize,
) -> Vec<ExemplarRef> {
    let codec = ToyCodec;
    let mut refs = Vec::with_capacity(hops);
    let mut cur = mat.genesis();
    let mut entry_env: Option<Reproducer> = None;
    for i in 0..hops {
        let at = (i as u64 + 1) * 10;
        machine.branch(cur, &codec.seeded(seed)).expect("branch");
        let until = StopConditions {
            deadline: Some(Moment(at)),
            on: StopMask::NONE,
        };
        loop {
            if machine.run(&until, None).expect("run").is_terminal() {
                break;
            }
        }
        let seal = machine.snapshot().expect("seal");
        let suffix = machine.recorded_env().expect("recorded_env");
        let env = match &entry_env {
            None => suffix.clone(),
            Some(base) => codec
                .compose(base, &suffix)
                .expect("toy codec is infallible"),
        };
        let r = frontier.insert(FrontierEntry {
            exemplar: VirtualExemplar {
                parent: cur,
                seed,
                suffix: suffix.clone(),
                at: Moment(at),
            },
            env: env.clone(),
            reward: Reward { new_cells: 1 },
        });
        frontier.claim((i as u64).to_le_bytes().to_vec(), r);
        assert_eq!(mat.register(r, seal, cur, suffix, Moment(at)), None);
        refs.push(r);
        entry_env = Some(env);
        cur = seal;
    }
    refs
}

proptest! {
    #![proptest_config(config(256))]

    /// (a) on deep chains: materializing the deepest exemplar after an
    /// arbitrary ancestor-eviction pattern replays exactly the distance to
    /// the nearest retained ancestor (report == modeled cost == what the
    /// machine actually ran), folds exactly the dead intermediates, reaches
    /// genesis only when nothing is retained — and the re-materialized state
    /// hashes identically to the never-evicted seal.
    #[test]
    fn fold_replays_exactly_to_the_nearest_retained_ancestor(
        seed in any::<u64>(),
        hops in 3usize..=7,
        evict_mask in any::<u8>(),
    ) {
        let (mut machine, meter) = MeterMachine::new();
        let genesis = machine.snapshot().expect("genesis");
        let mut mat = Materializer::new(genesis, Moment(0));
        let mut frontier = Frontier::new();
        let refs = build_chain(&mut machine, &mut mat, &mut frontier, seed, hops);
        let target = refs[hops - 1];
        let target_at = (hops as u64) * 10;

        // Reference: the never-evicted seal's state.
        let original_seal = mat.seal_of(target).expect("eagerly sealed");
        machine.replay(original_seal).expect("replay");
        let reference = machine.hash().expect("hash");

        // Evict the target's own seal (forcing a real materialization) plus
        // the masked subset of its ancestors.
        mat.evict_seal(&mut machine, target).expect("evict target");
        for (i, r) in refs.iter().enumerate().take(hops - 1) {
            if evict_mask & (1 << i) != 0 {
                mat.evict_seal(&mut machine, *r).expect("evict ancestor");
            }
        }

        // The expected base: the deepest non-evicted ancestor, else genesis.
        let retained: Option<usize> = (0..hops - 1)
            .rev()
            .find(|&i| mat.seal_of(refs[i]).is_some());
        let expected_base_at = retained.map(|i| (i as u64 + 1) * 10).unwrap_or(0);
        let expected_depth = target_at - expected_base_at;
        let expected_folded = match retained {
            None => 0, // genesis worst case: the memoized entry.env, no folds
            Some(i) => (hops - 1 - i - 1) as u64,
        };

        // The engine's cost model agrees before anything runs.
        prop_assert_eq!(mat.modeled_cost(&frontier, target), Some(expected_depth));

        let (new_seal, rep) = mat
            .materialize(&mut machine, &ToyCodec, &frontier, target)
            .expect("materialize");
        let rep = rep.expect("a real replay ran (the target's seal was evicted)");
        prop_assert_eq!(rep.depth(), expected_depth, "reported depth");
        prop_assert_eq!(rep.from_genesis, retained.is_none(),
            "genesis only when no ancestor is retained");
        prop_assert_eq!(rep.folded, expected_folded, "folds = dead intermediates");
        prop_assert_eq!(rep.at, Moment(target_at));

        // The machine really replayed exactly that much: the meter's last
        // seal event is (base_at, target_at).
        let observed = *meter.borrow().seals.last().expect("a seal ran");
        prop_assert_eq!(observed, (expected_base_at, target_at),
            "issued replay depth == distance to the nearest retained ancestor");

        // And the state is bit-identical to the never-evicted seal.
        machine.replay(new_seal).expect("replay re-materialized");
        prop_assert_eq!(machine.hash().expect("hash"), reference,
            "eviction never changes what materialization reproduces");

        // Cache hit afterwards: nothing more is replayed.
        let seals_before = meter.borrow().seals.len();
        let (again, rep2) = mat
            .materialize(&mut machine, &ToyCodec, &frontier, target)
            .expect("cache hit");
        prop_assert_eq!(again, new_seal);
        prop_assert!(rep2.is_none());
        prop_assert_eq!(meter.borrow().seals.len(), seals_before);
    }
}

// ---------------------------------------------------------------------------
// (a1) The same property through the Explorer campaign loop.
// ---------------------------------------------------------------------------

/// Test-side mirror of the nearest-retained-ancestor walk, built from public
/// observables only (`seal_of`, the frontier's exemplars, and an owner map the
/// test maintains from observed seals).
fn mirror_walk(
    ex: &Explorer<MeterMachine>,
    owner: &BTreeMap<u64, ExemplarRef>,
    r: ExemplarRef,
) -> (u64, bool) {
    let entry = ex.frontier().get(r).expect("live entry");
    let at = entry.exemplar.at.0;
    let mut cur = entry.exemplar.parent;
    loop {
        if cur == ex.genesis() {
            return (at, true); // genesis moment is 0 for the toy
        }
        let holder = owner[&cur.0];
        if ex.seal_of(holder).is_some() {
            let base_at = ex
                .frontier()
                .get(holder)
                .expect("coverage archive never drops entries")
                .exemplar
                .at
                .0;
            return (at - base_at, false);
        }
        cur = ex
            .frontier()
            .get(holder)
            .expect("coverage archive never drops entries")
            .exemplar
            .parent;
    }
}

proptest! {
    #![proptest_config(config(256))]

    /// (a) through the campaign loop: whatever eviction pattern a campaign
    /// interleaves, every materialization's issued depth equals the mirror's
    /// nearest-retained-ancestor distance, matches `modeled_cost`, and
    /// genesis is replayed only when nothing on the chain is retained.
    #[test]
    fn campaign_materializations_pay_the_nearest_retained_distance(
        seed in any::<u64>(),
        steps in 1u64..16,
        knob in any::<u64>(),
    ) {
        let (machine, meter) = MeterMachine::new();
        let mut ex = Explorer::new(machine, Box::new(ToyCodec), pin_composition(), seed).unwrap();
        let mut owner: BTreeMap<u64, ExemplarRef> = BTreeMap::new();
        let mut chaos = Prng::new(knob);

        for _ in 0..steps {
            ex.step().unwrap();
            // Adopt every observable seal into the mirror (eager fork seals
            // and any re-materialization the exploit path minted).
            let live: Vec<ExemplarRef> = ex.frontier().iter().map(|(r, _)| r).collect();
            for &r in &live {
                if let Some(s) = ex.seal_of(r) {
                    owner.entry(s.0).or_insert(r);
                }
            }
            // Random mid-campaign evictions.
            for &r in &live {
                if chaos.next_u64().is_multiple_of(4) {
                    ex.evict_seal(r).unwrap();
                }
            }
            if live.is_empty() {
                continue;
            }
            let r = ex.frontier().nth(chaos.next_u64()).unwrap();
            let cache_hit = ex.seal_of(r).is_some();
            let (expected_depth, expected_from_genesis) = if cache_hit {
                (0, false)
            } else {
                mirror_walk(&ex, &owner, r)
            };
            prop_assert_eq!(ex.modeled_cost(r), Some(expected_depth));

            let (seal, rep) = ex.materialize_report(r).unwrap();
            owner.entry(seal.0).or_insert(r);
            match rep {
                None => prop_assert!(cache_hit, "no replay ⇔ the seal was live"),
                Some(rep) => {
                    prop_assert!(!cache_hit);
                    prop_assert_eq!(rep.depth(), expected_depth);
                    prop_assert_eq!(rep.from_genesis, expected_from_genesis,
                        "genesis only when no ancestor is retained");
                    let observed = *meter.borrow().seals.last().unwrap();
                    prop_assert_eq!(observed.1 - observed.0, expected_depth,
                        "the machine actually replayed the modeled depth");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// (b) Eviction safety under the pool: same bugs, same admissions.
// ---------------------------------------------------------------------------

/// The deduped bugs (fingerprint + genesis-complete reproducer bytes) and the
/// admitted frontier (env bytes + reward) of one campaign.
type Outcome = (Vec<([u8; 32], Vec<u8>)>, Vec<(Vec<u8>, u64)>);

fn campaign(seed: u64, steps: u64, aggressive: bool) -> Outcome {
    let mut ex = Explorer::new(
        ToyMachine::new(),
        Box::new(ToyCodec),
        pin_composition(),
        seed,
    )
    .unwrap();
    if aggressive {
        // A one-seal pool: every step's budget enforcement evicts down to the
        // single highest-benefit seal…
        ex.set_seal_budget(SealBudget::Frontier {
            base: 1,
            num: 0,
            den: 1,
        });
    }
    let mut chaos = Prng::new(seed ^ 0x5EA1);
    let mut bugs = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..steps {
        if let Some(bug) = ex.step().unwrap()
            && seen.insert(bug.fingerprint)
        {
            bugs.push((bug.fingerprint, bug.env.bytes));
        }
        if aggressive {
            // …plus random extra per-seal evictions, down to zero some steps.
            let live: Vec<ExemplarRef> = ex.frontier().iter().map(|(r, _)| r).collect();
            for r in live {
                if chaos.next_u64().is_multiple_of(2) {
                    ex.evict_seal(r).unwrap();
                }
            }
        }
    }
    let admitted = ex
        .frontier()
        .iter()
        .map(|(_, e)| (e.env.bytes.clone(), e.reward.new_cells))
        .collect();
    (bugs, admitted)
}

proptest! {
    #![proptest_config(config(256))]

    /// (b) A campaign under a one-seal pool budget plus random per-step
    /// evictions finds byte-identical bugs, reproducers, and admissions to
    /// one that never evicts — retention is a pure performance knob.
    #[test]
    fn pool_eviction_changes_no_campaign_outcome(
        seed in any::<u64>(),
        steps in 1u64..25,
    ) {
        prop_assert_eq!(campaign(seed, steps, false), campaign(seed, steps, true));
    }
}

// ---------------------------------------------------------------------------
// (c) The pool bound + monotone degradation toward the genesis bound.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(config(256))]

    /// (c) Retained seals never exceed the frontier-derived budget after any
    /// step, and as seals are evicted one by one every entry's modeled cost
    /// is non-decreasing and never exceeds its genesis bound.
    #[test]
    fn pool_respects_the_budget_and_degrades_monotonically(
        seed in any::<u64>(),
        steps in 1u64..20,
        base in 0u64..3,
        num in 0u64..2,
        den in 1u64..4,
    ) {
        let budget = SealBudget::Frontier { base, num, den };
        let mut ex = Explorer::new(
            ToyMachine::new(),
            Box::new(ToyCodec),
            pin_composition(),
            seed,
        )
        .unwrap();
        ex.set_seal_budget(budget);
        for _ in 0..steps {
            ex.step().unwrap();
            prop_assert!(
                ex.sealed_count() <= budget.of(ex.frontier().len()),
                "retained {} > budget {} of frontier {}",
                ex.sealed_count(),
                budget.of(ex.frontier().len()),
                ex.frontier().len()
            );
        }

        // Monotone degradation: evict the remaining seals one at a time (in
        // deterministic ref order); every entry's cost only grows, bounded by
        // its genesis depth (toy genesis moment = 0).
        let refs: Vec<ExemplarRef> = ex.frontier().iter().map(|(r, _)| r).collect();
        let mut costs: BTreeMap<u64, u64> = refs
            .iter()
            .map(|&r| (r.0, ex.modeled_cost(r).unwrap()))
            .collect();
        for &victim in &refs {
            if ex.evict_seal(victim).unwrap().is_none() {
                continue;
            }
            for &r in &refs {
                let before = costs[&r.0];
                let after = ex.modeled_cost(r).unwrap();
                let bound = ex.frontier().get(r).unwrap().exemplar.at.0;
                prop_assert!(after >= before, "cost degrades monotonically");
                prop_assert!(after <= bound, "…up to the genesis bound");
                costs.insert(r.0, after);
            }
        }
        // Everything evicted ⇒ every entry now prices at its genesis bound.
        for &r in &refs {
            let bound = ex.frontier().get(r).unwrap().exemplar.at.0;
            prop_assert_eq!(ex.modeled_cost(r).unwrap(), bound);
        }
    }
}

// ---------------------------------------------------------------------------
// (d) The RESTRICTED arm: a synthetic sealable predicate.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(config(256))]

    /// (d) With the same synthetic `sealable` predicate injected into the
    /// archive (admission) and the engine (seals), no campaign — however it
    /// forks, evicts, and re-materializes — ever takes a seal at an
    /// inadmissible `Moment`, and every admitted exemplar sits at an
    /// admissible one.
    #[test]
    fn engine_never_seals_at_an_inadmissible_moment(
        seed in any::<u64>(),
        steps in 1u64..16,
        allow40 in any::<bool>(),
        allow60 in any::<bool>(),
    ) {
        let sealable = move |at: Moment| (at.0 != 40 || allow40) && (at.0 != 60 || allow60);
        let (machine, meter) = MeterMachine::new();
        let parts = Composition {
            archive: Box::new(CoverageArchive::with_sealable(Box::new(sealable))),
            tactic: Box::new(common::PinTactic),
            selector: Box::new(explorer::ExploreExploitSelector::new()),
            oracle: Box::new(TerminalOracle::new()),
            cells: Box::new(IdentityCells::new()),
            sensors: Vec::new(),
        };
        let mut ex = Explorer::new(machine, Box::new(ToyCodec), parts, seed).unwrap();
        ex.set_sealable(Box::new(sealable));
        ex.explore(steps).unwrap();

        // Every admitted exemplar keys an admissible moment…
        for (_, e) in ex.frontier().iter() {
            prop_assert!(sealable(e.exemplar.at));
        }
        // …and re-materialization after total eviction stays admissible.
        ex.evict_seals().unwrap();
        let refs: Vec<ExemplarRef> = ex.frontier().iter().map(|(r, _)| r).collect();
        for r in refs {
            ex.materialize(r).unwrap();
        }
        // The machine never even SAW a seal attempt at an inadmissible
        // moment — the meter records every snapshot() call. (The first seal
        // is the genesis snapshot, taken before any run — not exemplar
        // material, not predicate-gated.)
        let seals = &meter.borrow().seals;
        prop_assert_eq!(seals[0], (0, 0), "the genesis seal");
        for &(_, at) in &seals[1..] {
            prop_assert!(
                sealable(Moment(at)),
                "a seal was attempted at inadmissible moment {}",
                at
            );
        }
    }
}

/// (d) The loud half: materializing an exemplar whose `at` fails the
/// predicate is refused with `NotSealable` — and no seal is attempted.
#[test]
fn materialize_refuses_an_inadmissible_exemplar_loudly() {
    let (mut machine, meter) = MeterMachine::new();
    let genesis = machine.snapshot().expect("genesis");
    let mut mat = Materializer::new(genesis, Moment(0));
    mat.set_sealable(Box::new(|at| at.0 != 30));

    let mut frontier = Frontier::new();
    let env = ToyCodec.seeded(7);
    let r = frontier.insert(FrontierEntry {
        exemplar: VirtualExemplar {
            parent: genesis,
            seed: 7,
            suffix: env.clone(),
            at: Moment(30),
        },
        env,
        reward: Reward { new_cells: 1 },
    });
    frontier.claim(vec![0], r);

    let seals_before = meter.borrow().seals.len();
    assert!(matches!(
        mat.materialize(&mut machine, &ToyCodec, &frontier, r),
        Err(MachineError::NotSealable(30))
    ));
    assert_eq!(
        meter.borrow().seals.len(),
        seals_before,
        "no seal was attempted at the inadmissible moment"
    );
}

/// A replay that lands off the exemplar's keyed moment is a loud
/// `MaterializeDivergence`, never a mis-keyed seal. (The toy's V-time grid is
/// 10 per decision, so an off-grid `at` can never be landed on exactly —
/// standing in for a determinism/keying violation on the real substrate.)
#[test]
fn materialize_divergence_is_loud_never_a_wrong_seal() {
    let (mut machine, meter) = MeterMachine::new();
    let genesis = machine.snapshot().expect("genesis");
    let mut mat = Materializer::new(genesis, Moment(0));

    let mut frontier = Frontier::new();
    let env = ToyCodec.seeded(7);
    let r = frontier.insert(FrontierEntry {
        exemplar: VirtualExemplar {
            parent: genesis,
            seed: 7,
            suffix: env.clone(),
            at: Moment(35), // off the toy's 10-grid: the replay lands at 40
        },
        env,
        reward: Reward { new_cells: 1 },
    });
    frontier.claim(vec![0], r);

    let seals_before = meter.borrow().seals.len();
    assert!(matches!(
        mat.materialize(&mut machine, &ToyCodec, &frontier, r),
        Err(MachineError::MaterializeDivergence {
            exemplar: 0,
            at: 35,
            landed: 40,
        })
    ));
    assert_eq!(mat.seal_of(r), None, "nothing was cached");
    assert_eq!(
        meter.borrow().seals.len(),
        seals_before,
        "the divergent state was never sealed"
    );
}
