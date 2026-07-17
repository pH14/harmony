// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task-64 gate 2 — the **decomposition proptests** (≥256 cases each):
//!
//! 1. **Open-loop `Tactic`** (spine invariant 1): identical
//!    `(state, point, rng)` ⇒ identical answers, whatever concurrent runs do.
//!    A recording tactic logs every `(point, stream state, answer)` a live
//!    campaign fed it; replaying the log through a fresh instance — with a
//!    *different* concurrent campaign running between decisions — reproduces
//!    every answer. The engine feeds a tactic nothing else (no coverage, no
//!    archive state), so the answers cannot depend on anything else.
//! 2. **Timeline admission bounds the archive by cells** (invariant 2): however
//!    many runs a campaign makes, frontier entries never exceed occupied cells
//!    (every admission claims a fresh cell) and never exceed the machine's
//!    cell space — the archive is bounded by distinct cells, not runs.
//! 3. **Eviction is reproducibility-safe** (invariant 4): a campaign that drops
//!    every seal after every step (aggressive eviction, forcing exploits to
//!    re-materialize from genesis) finds byte-identical bugs and admissions to
//!    one that never evicts; and a re-materialized state hashes identically to
//!    the seal it replaced.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::{COVERAGE_LEN, ToyCodec, ToyMachine, config, fnv, pin_composition};
use explorer::{
    Answer, Archive, Composition, CoverageArchive, DecisionPoint, ExemplarRef,
    ExploreExploitSelector, Explorer, Frontier, IdentityCells, Machine, Prng, StopConditions,
    StopMask, Tactic, TerminalOracle,
};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Invariant 1 — the open-loop Tactic
// ---------------------------------------------------------------------------

/// A stateful, stream-drawing tactic: its answer folds its own evolving state,
/// the point, and one PRNG draw — everything a real tactic may legally use,
/// and nothing else.
#[derive(Clone, Debug)]
struct MixTactic {
    state: u64,
}

impl MixTactic {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }
}

impl Tactic for MixTactic {
    fn decide(&mut self, pt: &DecisionPoint, rng: &mut Prng) -> Answer {
        // State evolves only from the observed points (a legal stateful
        // distribution); the draw folds state, point, and stream.
        self.state = self
            .state
            .wrapping_mul(0x0000_0100_0000_01b3)
            .wrapping_add(pt.id)
            ^ fnv(&pt.ctx);
        let r = rng.next_u64() ^ self.state ^ pt.at.0;
        Answer(vec![(r & 0xff) as u8])
    }
}

/// One logged decision: the point, the stream state *before* the draw, and the
/// answer given.
type DecisionLog = Rc<RefCell<Vec<(DecisionPoint, Prng, Answer)>>>;

/// Wraps a tactic and logs every decision it makes inside a live campaign.
struct RecordingTactic {
    inner: MixTactic,
    log: DecisionLog,
}

impl Tactic for RecordingTactic {
    fn decide(&mut self, pt: &DecisionPoint, rng: &mut Prng) -> Answer {
        let stream_before = rng.clone();
        let answer = self.inner.decide(pt, rng);
        self.log
            .borrow_mut()
            .push((pt.clone(), stream_before, answer.clone()));
        answer
    }
}

proptest! {
    #![proptest_config(config(256))]

    /// Invariant 1: replaying a campaign's decision log through a fresh tactic
    /// instance — while an unrelated campaign runs between decisions —
    /// reproduces every answer byte-for-byte.
    #[test]
    fn tactic_is_open_loop(
        campaign_seed in any::<u64>(),
        tactic_seed in any::<u64>(),
        noise_seed in any::<u64>(),
        steps in 1u64..12,
    ) {
        // A live campaign drives the recording tactic; the engine hands it
        // DecisionPoints and the campaign stream, nothing else (there is no
        // other parameter).
        let log: DecisionLog = Rc::new(RefCell::new(Vec::new()));
        let parts = Composition {
            tactic: Box::new(RecordingTactic {
                inner: MixTactic::new(tactic_seed),
                log: Rc::clone(&log),
            }),
            selector: Box::new(ExploreExploitSelector::new()),
            archive: Box::new(CoverageArchive::new()),
            oracle: Box::new(TerminalOracle::new()),
            cells: Box::new(IdentityCells::new()),
            sensors: Vec::new(),
        };
        let mut ex =
            Explorer::new(ToyMachine::new(), Box::new(ToyCodec), parts, campaign_seed).unwrap();
        ex.explore(steps).unwrap();
        let log = log.borrow();

        // Replay standalone: same tactic state evolution (fresh instance, same
        // point sequence), same per-decision stream state — with a *different*
        // concurrent campaign interleaved between decisions to stand in for
        // "whatever concurrent runs do."
        let mut fresh = MixTactic::new(tactic_seed);
        let mut noise =
            Explorer::new(ToyMachine::new(), Box::new(ToyCodec), pin_composition(), noise_seed)
                .unwrap();
        for (pt, stream_before, answer) in log.iter() {
            noise.step().unwrap();
            let mut rng = stream_before.clone();
            prop_assert_eq!(
                &fresh.decide(pt, &mut rng),
                answer,
                "identical (state, point, rng) must yield the identical answer"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Invariant 2 — timeline admission bounds the archive by cells
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(config(256))]

    /// Invariant 2: frontier entries never exceed occupied cells (every
    /// admission claimed at least one fresh cell) nor the machine's finite
    /// cell space, and running the same campaign much longer cannot push
    /// entries past the cell bound — the archive is bounded by distinct
    /// cells, not runs.
    #[test]
    fn admission_is_bounded_by_cells_not_runs(
        seed in any::<u64>(),
        steps in 1u64..40,
    ) {
        // The toy's whole cell space: one cell per (edge, bucket) pair.
        let cell_space = COVERAGE_LEN * 8;

        let mut ex =
            Explorer::new(ToyMachine::new(), Box::new(ToyCodec), pin_composition(), seed).unwrap();
        ex.explore(steps).unwrap();
        let f = ex.frontier();
        prop_assert!(f.len() <= f.occupied_cells(), "every entry claims a fresh cell");
        for (_, entry) in f.iter() {
            prop_assert!(entry.reward.new_cells >= 1, "no entry admits for free");
        }
        prop_assert!(f.occupied_cells() <= cell_space);

        // Quadruple the run count: the frontier still cannot outgrow the cell
        // space (it grows only on fresh cells, which are finite).
        let mut ex4 =
            Explorer::new(ToyMachine::new(), Box::new(ToyCodec), pin_composition(), seed).unwrap();
        ex4.explore(steps * 4).unwrap();
        let f4 = ex4.frontier();
        prop_assert!(f4.len() <= f4.occupied_cells());
        prop_assert!(f4.occupied_cells() <= cell_space);
    }
}

/// One run contributes many exemplars (timeline admission): the very first
/// genesis run forks at both toy snapshot points, and both prefixes carry
/// fresh coverage, so a single search-loop step admits two entries.
#[test]
fn one_run_seeds_many_exemplars() {
    let mut ex =
        Explorer::new(ToyMachine::new(), Box::new(ToyCodec), pin_composition(), 7).unwrap();
    ex.step().unwrap();
    assert_eq!(
        ex.frontier().len(),
        2,
        "one genesis run admitted an exemplar at each of its two fork moments"
    );
    let ats: Vec<u64> = ex
        .frontier()
        .iter()
        .map(|(_, e)| e.exemplar.cut.at.0)
        .collect();
    assert_eq!(ats, vec![40, 60], "admitted along the timeline, in order");
}

// ---------------------------------------------------------------------------
// Invariant 4 — eviction is reproducibility-safe
// ---------------------------------------------------------------------------

/// The deduped bugs of a campaign, in discovery order (fingerprint + env bytes).
type Bugs = Vec<([u8; 32], Vec<u8>)>;
/// The admitted frontier (genesis-complete env bytes + reward).
type Admitted = Vec<(Vec<u8>, u64)>;

/// A campaign under the given seal-eviction aggressiveness: bugs (deduped, in
/// discovery order) and the admitted frontier.
fn campaign(seed: u64, steps: u64, evict_every_step: bool) -> (Bugs, Admitted) {
    let mut ex = Explorer::new(
        ToyMachine::new(),
        Box::new(ToyCodec),
        pin_composition(),
        seed,
    )
    .unwrap();
    let mut bugs = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..steps {
        if let Some(bug) = ex.step().unwrap()
            && seen.insert(bug.fingerprint)
        {
            bugs.push((bug.fingerprint, bug.env.bytes));
        }
        if evict_every_step {
            ex.evict_seals().unwrap();
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

    /// Invariant 4: aggressive eviction finds the same bug fingerprints (and
    /// reproducers, and admissions) as none — retention is a pure performance
    /// knob, never a correctness concern.
    #[test]
    fn aggressive_eviction_changes_no_outcome(
        seed in any::<u64>(),
        steps in 1u64..30,
    ) {
        let keep = campaign(seed, steps, false);
        let evict = campaign(seed, steps, true);
        prop_assert_eq!(keep, evict);
    }
}

/// The direct witness: an evicted exemplar re-materializes from genesis to a
/// state that hashes identically to the seal it lost.
#[test]
fn rematerialized_state_hashes_identically_to_its_seal() {
    let mut ex =
        Explorer::new(ToyMachine::new(), Box::new(ToyCodec), pin_composition(), 11).unwrap();
    ex.step().unwrap();
    assert!(!ex.frontier().is_empty(), "the first step admitted entries");

    let r = ExemplarRef(0);
    let seal = ex
        .seal_of(r)
        .expect("an admitted entry keeps its fork seal");
    ex.machine_mut().replay(seal).unwrap();
    let sealed_hash = ex.machine_mut().hash().unwrap();

    // Evict every seal, then materialize the entry again from genesis.
    ex.evict_seals().unwrap();
    assert_eq!(ex.seal_of(r), None, "the seal is gone");
    let reseal = ex.materialize(r).unwrap();
    assert_ne!(reseal, seal, "a fresh handle was minted");
    ex.machine_mut().replay(reseal).unwrap();
    let remat_hash = ex.machine_mut().hash().unwrap();

    assert_eq!(
        sealed_hash, remat_hash,
        "re-materialization from genesis reproduces the evicted state bit-for-bit"
    );

    // And the cheap path: materializing again returns the cached seal.
    assert_eq!(ex.materialize(r).unwrap(), reseal);
}

// ---------------------------------------------------------------------------
// Invariant 4b — the seal cache survives a COMPACTING Archive::evict
// (the round-1 review's blocking finding: positional keying desyncs here)
// ---------------------------------------------------------------------------

/// A compacting archive: admits every sealable fork (each into its own fresh
/// synthetic cell — one per admission, so the cell-bound invariant holds), and
/// whose `evict` trims the frontier to its **single most-recent** entry. This
/// is the frontier-renumbering shape tasks 68/70 will actually have, and the
/// exact scenario under which a positionally-keyed seal cache would hand
/// `materialize` a different exemplar's snapshot.
struct CompactingArchive {
    frontier: Frontier,
    next_cell: u64,
}

impl CompactingArchive {
    fn new() -> Self {
        Self {
            frontier: Frontier::new(),
            next_cell: 0,
        }
    }
}

impl Archive for CompactingArchive {
    fn admit(
        &mut self,
        _t: &explorer::RunTrace,
        forks: &[explorer::Fork],
        _cells: &dyn explorer::CellFn,
        _sensors: &[Box<dyn explorer::Sensor>],
    ) -> explorer::Reward {
        let mut total = 0u64;
        for fork in forks {
            let cell = self.next_cell.to_le_bytes().to_vec();
            self.next_cell += 1;
            let r = self.frontier.insert(explorer::FrontierEntry {
                exemplar: fork.exemplar.clone(),
                env: fork.env.clone(),
                reward: explorer::Reward { new_cells: 1 },
            });
            self.frontier.claim(cell, r);
            total += 1;
        }
        explorer::Reward { new_cells: total }
    }

    fn admissible(&self, _at: explorer::Moment) -> bool {
        true
    }

    /// Trim to the most-recent live entry — every older entry is evicted.
    fn evict(&mut self) {
        let refs: Vec<ExemplarRef> = self.frontier.iter().map(|(r, _)| r).collect();
        for r in refs.iter().take(refs.len().saturating_sub(1)) {
            self.frontier.remove(*r);
        }
    }

    fn frontier(&self) -> &Frontier {
        &self.frontier
    }
}

/// A selector that always exploits the frontier's first live entry.
struct FirstEntrySelector;

impl explorer::Selector for FirstEntrySelector {
    fn choose(&mut self, frontier: &Frontier, _rng: &mut Prng) -> Option<ExemplarRef> {
        frontier.nth(0)
    }
    fn reward(&mut self, _chosen: ExemplarRef, _r: explorer::Reward) {}
}

/// Under a compacting `Archive::evict`, exemplar identity is stable: the
/// survivor keeps its original ref (never renumbered onto an evicted slot),
/// dead refs fail loudly (`UnknownExemplar`, never a wrong snapshot), evicted
/// entries' seals are swept (no handle leak), and materializing the survivor
/// yields exactly its own state — proven by hash against a from-genesis
/// re-drive. A positionally-keyed seal cache fails the hash check here.
#[test]
fn compacting_eviction_never_desyncs_the_seal_cache() {
    let parts = Composition {
        tactic: Box::new(explorer::DeclineTactic::new()),
        selector: Box::new(FirstEntrySelector),
        archive: Box::new(CompactingArchive::new()),
        oracle: Box::new(TerminalOracle::new()),
        cells: Box::new(IdentityCells::new()),
        sensors: Vec::new(),
    };
    let mut ex = Explorer::new(ToyMachine::new(), Box::new(ToyCodec), parts, 42).unwrap();
    let genesis = ex.genesis();

    // Step 1: the genesis run admits two entries (ids 0, 1); the compacting
    // evict then trims to the most recent (id 1) and the engine sweeps id 0's
    // orphaned seal.
    ex.step().unwrap();
    assert_eq!(ex.frontier().len(), 1, "compaction trimmed to one entry");
    let survivor = ex.frontier().iter().next().expect("one live entry").0;
    assert_eq!(
        survivor,
        ExemplarRef(1),
        "the survivor keeps its ORIGINAL stable id — eviction never renumbers"
    );
    assert_eq!(
        ex.seal_of(ExemplarRef(0)),
        None,
        "the evicted entry's seal was swept, not left to alias"
    );
    // A dead ref fails loudly — never resolves to another entry's snapshot.
    assert!(matches!(
        ex.materialize(ExemplarRef(0)),
        Err(explorer::MachineError::UnknownExemplar(0))
    ));

    // The survivor's materialization is provably ITS state: the seal's hash
    // equals a from-genesis re-drive of the survivor's own env to its own
    // moment. (Under positional keying, `materialize(survivor-at-position-0)`
    // would have returned the evicted id-0 seal and this hash check fails.)
    let (env, at) = {
        let e = ex.frontier().get(survivor).expect("survivor entry");
        (e.env.clone(), e.exemplar.cut.at.0)
    };
    let seal = ex.materialize(survivor).unwrap();
    ex.machine_mut().replay(seal).unwrap();
    let sealed_hash = ex.machine_mut().hash().unwrap();
    let until = StopConditions {
        deadline: Some(explorer::Moment(at)),
        on: StopMask::NONE,
    };
    ex.machine_mut().branch(genesis, &env).unwrap();
    common::drive_to_terminal(ex.machine_mut(), &until, None).unwrap();
    let genesis_hash = ex.machine_mut().hash().unwrap();
    assert_eq!(
        sealed_hash, genesis_hash,
        "the survivor's seal is the survivor's state — never the evicted entry's"
    );

    // Handle accounting stays exact across the whole compacting campaign, and
    // exploits keep working (a reused dropped handle would abort explore with
    // UnknownSnapshot in the toy).
    for _ in 0..12 {
        ex.step().unwrap();
        let sealed = ex.sealed_count();
        assert_eq!(
            ex.machine_mut().live_snaps(),
            1 + sealed,
            "live snapshots = genesis + live seals (no leak, no dangle)"
        );
        // Every cached seal belongs to a LIVE entry.
        let live: Vec<ExemplarRef> = ex.frontier().iter().map(|(r, _)| r).collect();
        for r in live {
            if let Some(s) = ex.seal_of(r) {
                ex.machine_mut().replay(s).expect("live seals resolve");
            }
        }
    }
}
