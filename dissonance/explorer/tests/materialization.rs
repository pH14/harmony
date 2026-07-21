// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task-68 gate 2 — the **lazy-materialization proptests** (≥256 cases each),
//! against the toy `Machine` wrapped in a replay-cost meter (synthetic replay
//! costs: the meter observes every issued branch→run→seal and its depth):
//!
//! (a) **Hot-path property** — the issued replay depth always equals the
//!     distance to the nearest **retained** ancestor (checked three ways: the
//!     engine's report, its `modeled_cost`, and the meter's observation of
//!     what the machine actually replayed), and genesis is replayed only when
//!     no ancestor is retained — driven on the [`Materializer`] directly over
//!     deep (3–7 hop) hand-built chains that force the compose-fold path.
//! (b) **RESTRICTED arm** — under a synthetic `sealable` predicate
//!     materialization refuses loudly with `NotSealable`, and divergence /
//!     cut-divergence are loud, never a wrong seal.
//!
//! (The engine-loop mirrors of these properties retired with the legacy
//! `Explorer` in task 132 M3; the `DifferentialCampaign` path has its own
//! materialization coverage in `src/campaign.rs` tests.)

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use common::{ToyCodec, ToyMachine, config};
use explorer::{
    Answer, EnvCodec, EvidenceCut, ExemplarRef, Frontier, FrontierEntry, Machine, MachineError,
    Materializer, Moment, Reproducer, Reward, SnapId, StopConditions, StopMask, StopReason,
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

    fn snapshot(&mut self) -> Result<(SnapId, EvidenceCut), MachineError> {
        let (id, cut) = self.inner.snapshot()?;
        let mut m = self.meter.borrow_mut();
        let (branch_at, at) = (m.branch_at, m.last_stop);
        m.snap_at.insert(id.0, at);
        m.seals.push((branch_at, at));
        Ok((id, cut))
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
        let (seal, cut) = machine.snapshot().expect("seal");
        let suffix = machine.recorded_env().expect("recorded_env");
        let env = match &entry_env {
            None => suffix.clone(),
            Some(base) => codec
                .compose(base, &suffix)
                .expect("toy codec is infallible"),
        };
        assert_eq!(cut.at, Moment(at), "the toy stamps the seal moment");
        let r = frontier.insert(FrontierEntry {
            exemplar: VirtualExemplar {
                parent: cur,
                seed,
                suffix: suffix.clone(),
                cut,
            },
            env: env.clone(),
            reward: Reward { new_cells: 1 },
        });
        frontier.claim((i as u64).to_le_bytes().to_vec(), r);
        assert_eq!(mat.register(r, seal, cur, suffix, cut), None);
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
        let (genesis, _) = machine.snapshot().expect("genesis");
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
#[test]
fn materialize_refuses_an_inadmissible_exemplar_loudly() {
    let (mut machine, meter) = MeterMachine::new();
    let (genesis, _) = machine.snapshot().expect("genesis");
    let mut mat = Materializer::new(genesis, Moment(0));
    mat.set_sealable(Box::new(|at| at.0 != 30));

    let mut frontier = Frontier::new();
    let env = ToyCodec.seeded(7);
    let r = frontier.insert(FrontierEntry {
        exemplar: VirtualExemplar {
            parent: genesis,
            seed: 7,
            suffix: env.clone(),
            cut: EvidenceCut {
                at: Moment(30),
                sdk_events: 0,
            },
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
    let (genesis, _) = machine.snapshot().expect("genesis");
    let mut mat = Materializer::new(genesis, Moment(0));

    let mut frontier = Frontier::new();
    let env = ToyCodec.seeded(7);
    let r = frontier.insert(FrontierEntry {
        exemplar: VirtualExemplar {
            parent: genesis,
            seed: 7,
            suffix: env.clone(),
            // off the toy's 10-grid: the replay lands at 40
            cut: EvidenceCut {
                at: Moment(35),
                sdk_events: 0,
            },
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

/// A re-materialized seal whose **stamped cut** differs from the entry's
/// recorded cut is a loud `CutDivergence` (task 127), never a silently
/// overwritten stamp — and the fresh handle is released, not cached or
/// leaked. The entry here claims an SDK prefix (99) the replayed state cannot
/// re-stamp (the toy stamps its answer-log length, 4, at moment 40), standing
/// in for a restore that lost or grew the captured SDK prefix on the real
/// substrate.
#[test]
fn cut_divergence_is_loud_and_releases_the_fresh_seal() {
    let mut machine = ToyMachine::new();
    let (genesis, _) = machine.snapshot().expect("genesis");
    let mut mat = Materializer::new(genesis, Moment(0));

    let mut frontier = Frontier::new();
    let env = ToyCodec.seeded(7);
    let r = frontier.insert(FrontierEntry {
        exemplar: VirtualExemplar {
            parent: genesis,
            seed: 7,
            suffix: env.clone(),
            // The moment is on-grid (the replay lands exactly), but the
            // recorded SDK prefix length is not what the replayed state
            // re-stamps.
            cut: EvidenceCut {
                at: Moment(40),
                sdk_events: 99,
            },
        },
        env,
        reward: Reward { new_cells: 1 },
    });
    frontier.claim(vec![0], r);

    let live_before = machine.live_snaps();
    assert!(matches!(
        mat.materialize(&mut machine, &ToyCodec, &frontier, r),
        Err(MachineError::CutDivergence {
            exemplar: 0,
            at: 40,
            sdk_events: 99,
            got_at: 40,
            got_sdk_events: 4,
        })
    ));
    assert_eq!(mat.seal_of(r), None, "the divergent seal was not cached");
    assert_eq!(
        machine.live_snaps(),
        live_before,
        "the fresh handle was released, not leaked"
    );
}
