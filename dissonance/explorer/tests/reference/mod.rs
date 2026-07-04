// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **frozen pre-refactor engine** (task 12), vendored verbatim for the
//! task-64 behavior-equivalence gate.
//!
//! This module is the `Explorer`/`Strategy`/`Corpus` implementation exactly as
//! it stood before the spine refactor (commit `d7c230a`), re-rooted onto the
//! crate's public seam types (`Machine`, `EnvCodec`, `Environment`, …), which
//! the refactor leaves untouched. The equivalence gate
//! (`tests/behavior_equiv.rs`) drives this reference and the refactored engine
//! over the same toy machines and asserts **byte-identical bug fingerprints and
//! admission decisions** — so "behavior-preserving" is checked against the real
//! pre-refactor code, not a hand-maintained golden file.
//!
//! Do not "improve" this module: it is a historical artifact. The only edits
//! versus the original sources are `crate::` → `explorer::` paths, module
//! flattening, and the removal of the in-module unit tests (their pins live on
//! in the refactored crate).
#![allow(dead_code)]

use std::collections::BTreeSet;

use explorer::{
    Answer, EnvCodec, Environment, FaultCoord, Machine, MachineError, Moment, SnapId,
    StopConditions, StopMask, StopReason, TerminalSig, VTimeCoord, mint_fingerprint,
};

// ---- prng.rs (verbatim) ----

/// xorshift64\* multiplier (the `hypercall-proto` constant).
const MUL: u64 = 0x2545_F491_4F6C_DD1D;
/// Seed substituted for a zero seed, so the nonzero-state invariant holds.
const FALLBACK: u64 = 0x9E37_79B9_7F4A_7C15;

/// A deterministic xorshift64\* stream (the pre-refactor private `Prng`).
#[derive(Clone, Debug)]
pub struct RefPrng {
    state: u64,
}

impl RefPrng {
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { FALLBACK } else { seed },
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        self.state.wrapping_mul(MUL)
    }
}

// ---- corpus.rs (verbatim) ----

/// A run's coverage-novelty magnitude (pre-refactor `CovScore`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct CovScore(pub u64);

#[derive(Clone, Debug)]
struct Entry {
    snap: SnapId,
    env: Environment,
    score: CovScore,
}

const DEFAULT_CAPACITY: usize = 64;

/// The deterministic pre-refactor corpus: kept entries plus the accumulated
/// `(edge, bucket)` novelty index.
#[derive(Clone, Debug)]
pub struct Corpus {
    entries: Vec<Entry>,
    seen: BTreeSet<(usize, u8)>,
    capacity: usize,
    evicted: Vec<SnapId>,
}

impl Default for Corpus {
    fn default() -> Self {
        Self::new()
    }
}

impl Corpus {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Vec::new(),
            seen: BTreeSet::new(),
            capacity: capacity.max(1),
            evicted: Vec::new(),
        }
    }

    pub fn admit(&mut self, snap: SnapId, env: Environment, coverage: &[u8]) -> bool {
        let mut fresh: Vec<(usize, u8)> = Vec::new();
        for (i, &count) in coverage.iter().enumerate() {
            let b = bucket(count);
            if b != 0 && !self.seen.contains(&(i, b)) {
                fresh.push((i, b));
            }
        }
        if fresh.is_empty() {
            return false;
        }
        let score = CovScore(fresh.len() as u64);
        for pair in fresh {
            self.seen.insert(pair);
        }
        self.entries.push(Entry { snap, env, score });
        self.evict_over_capacity();
        true
    }

    pub fn novelty(&self, coverage: &[u8]) -> CovScore {
        let mut n = 0u64;
        for (i, &count) in coverage.iter().enumerate() {
            let b = bucket(count);
            if b != 0 && !self.seen.contains(&(i, b)) {
                n += 1;
            }
        }
        CovScore(n)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn select(&self, salt: u64) -> Option<(SnapId, &Environment)> {
        if self.entries.is_empty() {
            return None;
        }
        let idx = (salt % self.entries.len() as u64) as usize;
        let e = &self.entries[idx];
        Some((e.snap, &e.env))
    }

    pub fn entry(&self, i: usize) -> Option<(SnapId, &Environment, CovScore)> {
        self.entries.get(i).map(|e| (e.snap, &e.env, e.score))
    }

    pub fn base_env(&self, snap: SnapId) -> Option<&Environment> {
        self.entries.iter().find(|e| e.snap == snap).map(|e| &e.env)
    }

    pub fn drain_evicted(&mut self) -> Vec<SnapId> {
        std::mem::take(&mut self.evicted)
    }

    fn evict_over_capacity(&mut self) {
        while self.entries.len() > self.capacity {
            let upto = self.entries.len() - 1;
            let mut victim = 0usize;
            for i in 1..upto {
                if self.entries[i].score < self.entries[victim].score {
                    victim = i;
                }
            }
            let e = self.entries.remove(victim);
            self.evicted.push(e.snap);
        }
    }
}

/// The AFL count-bucket classifier (pre-refactor `corpus::bucket`).
fn bucket(count: u8) -> u8 {
    match count {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 3,
        4..=7 => 4,
        8..=15 => 5,
        16..=31 => 6,
        32..=127 => 7,
        _ => 8,
    }
}

// ---- strategy.rs (verbatim) ----

/// The pre-refactor god-object policy seam: inner-loop answering and outer-loop
/// environment selection conflated on one trait.
pub trait Strategy {
    fn choose(&mut self, ctx: &[u8], coverage: &[u8]) -> Answer;

    fn next_env(
        &mut self,
        corpus: &Corpus,
        genesis: SnapId,
        env: &dyn EnvCodec,
    ) -> (SnapId, Environment);
}

/// Pure seed-driven exploration (pre-refactor `SeedStrategy`).
#[derive(Clone, Debug)]
pub struct SeedStrategy {
    seeds: RefPrng,
}

impl SeedStrategy {
    pub fn new(seed: u64) -> Self {
        Self {
            seeds: RefPrng::new(seed),
        }
    }
}

impl Strategy for SeedStrategy {
    fn choose(&mut self, _ctx: &[u8], _coverage: &[u8]) -> Answer {
        Answer(Vec::new())
    }

    fn next_env(
        &mut self,
        _corpus: &Corpus,
        genesis: SnapId,
        env: &dyn EnvCodec,
    ) -> (SnapId, Environment) {
        (genesis, env.seeded(self.seeds.next_u64()))
    }
}

const DEFAULT_EXPLORE_PERIOD: u64 = 3;

/// Coverage-guided exploration (pre-refactor `CoverageStrategy`). Note that
/// `choose` folds the **live** coverage map into the answer — the closed-loop
/// feedback the task-64 open-loop `Tactic` invariant outlaws; the equivalence
/// gate therefore drives this strategy only in configurations where `choose` is
/// never called (see `tests/behavior_equiv.rs`).
#[derive(Clone, Debug)]
pub struct CoverageStrategy {
    rng: RefPrng,
    step: u64,
    explore_period: u64,
}

impl CoverageStrategy {
    pub fn new(seed: u64) -> Self {
        Self {
            rng: RefPrng::new(seed),
            step: 0,
            explore_period: DEFAULT_EXPLORE_PERIOD,
        }
    }

    pub fn with_explore_period(mut self, period: u64) -> Self {
        self.explore_period = period.max(1);
        self
    }
}

impl Strategy for CoverageStrategy {
    fn choose(&mut self, ctx: &[u8], coverage: &[u8]) -> Answer {
        let mix = checksum(coverage) ^ checksum(ctx);
        let r = self.rng.next_u64() ^ mix;
        Answer(vec![(r & 0xff) as u8])
    }

    fn next_env(
        &mut self,
        corpus: &Corpus,
        genesis: SnapId,
        env: &dyn EnvCodec,
    ) -> (SnapId, Environment) {
        self.step = self.step.wrapping_add(1);
        let explore = corpus.is_empty() || self.step.is_multiple_of(self.explore_period);
        if explore {
            return (genesis, env.seeded(self.rng.next_u64()));
        }
        let pick = self.rng.next_u64();
        let salt = self.rng.next_u64();
        match corpus.select(pick) {
            Some((snap, base)) => (snap, env.mutate(base, salt)),
            None => (genesis, env.seeded(salt)),
        }
    }
}

/// FNV-1a (pre-refactor `strategy::checksum`).
fn checksum(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ---- engine.rs (verbatim) ----

/// The result of one pre-refactor Timeline.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RunOutcome {
    pub stop: StopReason,
    pub env: Environment,
    pub coverage_novelty: CovScore,
}

struct PendingSnapshot {
    snap: SnapId,
    env: Environment,
    coverage: Vec<u8>,
}

/// A pre-refactor bug report (genesis-complete reproducer + fingerprint).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Bug {
    pub fingerprint: [u8; 32],
    pub env: Environment,
    pub stop: StopReason,
}

/// The pre-refactor exploration engine.
pub struct Explorer<M: Machine, S: Strategy> {
    machine: M,
    strategy: S,
    env: Box<dyn EnvCodec>,
    corpus: Corpus,
    genesis: SnapId,
    until: StopConditions,
    pending_snapshots: Vec<PendingSnapshot>,
}

impl<M: Machine, S: Strategy> Explorer<M, S> {
    pub fn new(machine: M, strategy: S, env: Box<dyn EnvCodec>) -> Result<Self, MachineError> {
        let mut machine = machine;
        let genesis = machine.snapshot()?;
        Ok(Self {
            machine,
            strategy,
            env,
            corpus: Corpus::new(),
            genesis,
            until: StopConditions {
                deadline: None,
                on: StopMask::ALL,
            },
            pending_snapshots: Vec::new(),
        })
    }

    pub fn genesis(&self) -> SnapId {
        self.genesis
    }

    pub fn corpus(&self) -> &Corpus {
        &self.corpus
    }

    pub fn set_corpus_capacity(&mut self, capacity: usize) -> Result<(), MachineError> {
        let snaps: Vec<SnapId> = (0..self.corpus.len())
            .filter_map(|i| self.corpus.entry(i).map(|(snap, _, _)| snap))
            .collect();
        for snap in snaps {
            self.machine.drop_snap(snap)?;
        }
        self.corpus = Corpus::with_capacity(capacity);
        Ok(())
    }

    pub fn stop_conditions(&self) -> &StopConditions {
        &self.until
    }

    pub fn set_stop_conditions(&mut self, until: StopConditions) {
        self.until = until;
    }

    pub fn machine_mut(&mut self) -> &mut M {
        &mut self.machine
    }

    pub fn timeline(
        &mut self,
        base: SnapId,
        env: &Environment,
        until: &StopConditions,
    ) -> Result<RunOutcome, MachineError> {
        for pending in std::mem::take(&mut self.pending_snapshots) {
            self.machine.drop_snap(pending.snap)?;
        }
        self.machine.branch(base, env)?;
        let mut resolve: Option<Answer> = None;
        loop {
            let stop = self.machine.run(until, resolve.as_ref())?;
            match stop {
                StopReason::Decision { ref ctx, .. } => {
                    let answer = self.strategy.choose(ctx, self.machine.coverage());
                    resolve = Some(answer);
                }
                StopReason::SnapshotPoint { .. } => {
                    let snap = self.machine.snapshot()?;
                    let prefix_env = match self.machine.recorded_env() {
                        Ok(env) => env,
                        Err(e) => {
                            let _ = self.machine.drop_snap(snap);
                            return Err(e);
                        }
                    };
                    let prefix_coverage = self.machine.coverage().to_vec();
                    self.pending_snapshots.push(PendingSnapshot {
                        snap,
                        env: prefix_env,
                        coverage: prefix_coverage,
                    });
                    resolve = None;
                }
                terminal => {
                    let env = self.machine.recorded_env()?;
                    let coverage_novelty = self.corpus.novelty(self.machine.coverage());
                    return Ok(RunOutcome {
                        stop: terminal,
                        env,
                        coverage_novelty,
                    });
                }
            }
        }
    }

    pub fn multiverse_step(&mut self) -> Result<Option<Bug>, MachineError> {
        let until = self.until.clone();
        let (base_snap, branch_env) =
            self.strategy
                .next_env(&self.corpus, self.genesis, self.env.as_ref());

        let outcome = self.timeline(base_snap, &branch_env, &until)?;

        let bug = if outcome.stop.is_bug() {
            Some(self.report(base_snap, &outcome))
        } else {
            None
        };

        let base_genesis: Option<Environment> = if base_snap == self.genesis {
            None
        } else {
            self.corpus.base_env(base_snap).cloned()
        };

        for pending in std::mem::take(&mut self.pending_snapshots) {
            let env = if base_snap == self.genesis {
                pending.env
            } else if let Some(base) = &base_genesis {
                self.env.compose(base, &pending.env)
            } else {
                self.machine.drop_snap(pending.snap)?;
                continue;
            };
            let novel = self.corpus.admit(pending.snap, env, &pending.coverage);
            if !novel {
                self.machine.drop_snap(pending.snap)?;
            }
        }
        for evicted in self.corpus.drain_evicted() {
            self.machine.drop_snap(evicted)?;
        }

        Ok(bug)
    }

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

    fn report(&self, base_snap: SnapId, outcome: &RunOutcome) -> Bug {
        let env = if base_snap == self.genesis {
            outcome.env.clone()
        } else if let Some(base) = self.corpus.base_env(base_snap) {
            self.env.compose(base, &outcome.env)
        } else {
            outcome.env.clone()
        };
        Bug {
            fingerprint: fingerprint(&outcome.stop),
            env,
            stop: outcome.stop.clone(),
        }
    }
}

/// The bug fingerprint the reference reports, tracking the pinned scheme the
/// refactored engine uses. Task 12 minted a stop-reason-only
/// `dissonance.explorer.bug.v1` digest; task 75 supersedes it with the shared
/// three-coordinate [`mint_fingerprint`](explorer::mint_fingerprint) schema.
/// The behavior-equivalence gate proves the engine and this reference agree on
/// **which bugs, which reproducers, which admissions** — the fingerprint scheme
/// moved forward on *both* sides in lockstep, so it stays a pure function of the
/// (stop, env) both sides already agree on. Its byte-for-byte correctness is
/// independently pinned by the crate's own golden (`defaults::tests`).
pub fn fingerprint(stop: &StopReason) -> [u8; 32] {
    // Coordinate-1 detail is the *class* of the bug site, not its raw per-run
    // payload (round 9): a crash's leading kind byte, an assertion's id — kept in
    // lockstep with `defaults::terminal_detail`.
    let (class, detail) = match stop {
        StopReason::Crash { info, .. } => (0u32, info.iter().take(1).copied().collect()),
        StopReason::Assertion { id, .. } => (1u32, id.to_le_bytes().to_vec()),
        StopReason::Deadline { .. } => (2, Vec::new()),
        StopReason::Quiescent { .. } => (3, Vec::new()),
        StopReason::Decision { .. } => (4, Vec::new()),
        StopReason::SnapshotPoint { .. } => (5, Vec::new()),
    };
    let sig = TerminalSig::new("terminal", class, stop.discriminant()).with_detail(detail);
    mint_fingerprint(
        &sig,
        &FaultCoord::none(),
        VTimeCoord::quantize(Moment(stop.vtime().0)),
    )
}
