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
    Answer, Composition, CoverageArchive, DecisionPoint, ExemplarRef, ExploreExploitSelector,
    Explorer, IdentityCells, Machine, Prng, Tactic, TerminalOracle,
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
            noise.multiverse_step().unwrap();
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
/// fresh coverage, so a single Multiverse step admits two entries.
#[test]
fn one_run_seeds_many_exemplars() {
    let mut ex =
        Explorer::new(ToyMachine::new(), Box::new(ToyCodec), pin_composition(), 7).unwrap();
    ex.multiverse_step().unwrap();
    assert_eq!(
        ex.frontier().len(),
        2,
        "one genesis run admitted an exemplar at each of its two fork moments"
    );
    let ats: Vec<u64> = ex.frontier().iter().map(|(_, e)| e.exemplar.at.0).collect();
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
        if let Some(bug) = ex.multiverse_step().unwrap()
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
    ex.multiverse_step().unwrap();
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
