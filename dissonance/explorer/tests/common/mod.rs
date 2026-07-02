// SPDX-License-Identifier: AGPL-3.0-or-later
//! The in-crate deterministic **toy machine** and **toy codec** the gate tests
//! drive the engine with.
//!
//! Together they stand in for the production R2-socket adapter + `environment`
//! codec: a tiny state machine whose `run` answers decisions and whose
//! `coverage`/`hash` are pure functions of the answer sequence, and a trivial
//! `EnvCodec` over a `{base_offset, seed, overrides}` blob. Because the engine
//! codes against the [`Machine`]/[`EnvCodec`] seams (conventions rule 2), the same
//! engine and the same determinism gate run here exactly as they will over a real
//! socket — so every property proven here is a property of the engine, not of the
//! toy.
//!
//! ## The model
//!
//! A run is a sequence of `TOTAL_DECISIONS` decisions at absolute indices
//! `0..TOTAL_DECISIONS`. Decision `i` has class `i % NUM_CLASSES`. At each
//! decision the machine either (a) answers from a **carried override** (env, no
//! surface), (b) **surfaces** a `Decision` if the run's [`StopMask`] selects its
//! class, or (c) **seed-answers** locally `f(seed, i)`. Crucially the seed answer
//! depends on the *absolute* index, so a decision answered the same way whether it
//! is reached from genesis or resumed from a mid-run branch — which is what makes
//! a branch-local reproducer recompose to a genesis-complete one. `coverage` is an
//! AFL-style edge map and `hash` is `sha256` over the full answer log; both are
//! pure functions of the answers. A genesis branch forks a `SnapshotPoint` at
//! `SNAP_AT`, giving the corpus a non-genesis base to exercise `compose`/rebasing.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::ProptestConfig;
use sha2::{Digest, Sha256};

use explorer::{
    Answer, Composition, CoverageArchive, DecisionPoint, DeclineTactic, EnvCodec, Environment,
    ExploreExploitSelector, GenesisSelector, IdentityCells, Machine, MachineError, MachineFactory,
    Prng, SnapId, StopConditions, StopReason, Tactic, TerminalOracle, VTime,
};

// ---- the toy's fixed shape ----

/// Decisions per genesis run (absolute indices `0..TOTAL_DECISIONS`).
pub const TOTAL_DECISIONS: u64 = 8;
/// The absolute index a genesis run forks its first `SnapshotPoint` at (freezing
/// the `0..SNAP_AT` prefix); the corpus's first-generation non-genesis bases sit
/// here.
pub const SNAP_AT: u64 = 4;
/// The deeper fork point. A run already past `SNAP_AT` (i.e. branched off a
/// `SNAP_AT` corpus snapshot) forks its `SnapshotPoint` here instead, producing a
/// **nested**, non-genesis-rooted corpus base — the case round-2 review exercised.
pub const SNAP_AT2: u64 = 6;
/// The fork points, ascending. A run forks at the smallest of these strictly
/// greater than its branch origin (so a genesis run forks at `SNAP_AT`, a run off
/// a `SNAP_AT` snapshot forks at `SNAP_AT2`, and a run off a `SNAP_AT2` snapshot
/// forks nowhere).
pub const SNAP_POINTS: [u64; 2] = [SNAP_AT, SNAP_AT2];
/// Number of decision classes; class of decision `i` is `i % NUM_CLASSES`.
pub const NUM_CLASSES: u64 = 4;
/// Answer alphabet size (answers are `0..K`).
pub const K: u8 = 4;
/// AFL edge-map width.
pub const COVERAGE_LEN: usize = 32;
/// V-time per decision.
pub const VTIME_STEP: u64 = 10;
/// The `StopMask` bit that enables the `SnapshotPoint` fork.
pub const SNAP_BIT: u32 = 1 << 31;

/// The toy blob's container magic, `"TOY1"` little-endian.
const MAGIC: u32 = u32::from_le_bytes(*b"TOY1");
/// The toy blob format version (mirrored into [`Environment::blob_version`]).
pub const TOY_BLOB_VERSION: u16 = 1;

// ---- the env blob codec ----

/// A decoded toy environment: the absolute decision index its overrides are keyed
/// from (`0` = genesis-complete), the position the env was captured at (the
/// snapshot's frozen prefix length for a corpus base, `TOTAL_DECISIONS` for a
/// terminal env), the base seed, and the per-decision overrides keyed by index
/// *since `base_offset`*. `pos` is what lets [`ToyCodec::mutate`] slice a corpus
/// base at the *right* offset whether the snapshot is genesis-rooted or nested.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToyEnv {
    pub base_offset: u64,
    pub pos: u64,
    pub seed: u64,
    pub overrides: BTreeMap<u64, u8>,
}

/// Encode a toy env to a canonical, byte-deterministic blob (overrides in sorted
/// id order — no map iteration order reaches a byte).
pub fn encode(e: &ToyEnv) -> Environment {
    let mut w = Vec::new();
    w.extend_from_slice(&MAGIC.to_le_bytes());
    w.extend_from_slice(&TOY_BLOB_VERSION.to_le_bytes());
    w.extend_from_slice(&e.base_offset.to_le_bytes());
    w.extend_from_slice(&e.pos.to_le_bytes());
    w.extend_from_slice(&e.seed.to_le_bytes());
    w.extend_from_slice(&(e.overrides.len() as u32).to_le_bytes());
    for (id, b) in &e.overrides {
        w.extend_from_slice(&id.to_le_bytes());
        w.push(*b);
    }
    Environment {
        blob_version: TOY_BLOB_VERSION,
        bytes: w,
    }
}

/// Decode a toy blob, bounds-checked and total (no panic on arbitrary bytes).
pub fn decode(env: &Environment) -> Result<ToyEnv, MachineError> {
    let b = &env.bytes;
    let bad = || MachineError::BadEnvironment(env.blob_version);
    let u32at = |o: usize, b: &[u8]| -> Result<u32, MachineError> {
        b.get(o..o + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
            .ok_or_else(bad)
    };
    let u64at = |o: usize, b: &[u8]| -> Result<u64, MachineError> {
        b.get(o..o + 8)
            .map(|s| u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
            .ok_or_else(bad)
    };
    if u32at(0, b)? != MAGIC {
        return Err(bad());
    }
    let ver = b
        .get(4..6)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(bad)?;
    if ver != TOY_BLOB_VERSION {
        return Err(bad());
    }
    let base_offset = u64at(6, b)?;
    let pos = u64at(14, b)?;
    let seed = u64at(22, b)?;
    let n = u32at(30, b)? as usize;
    let mut overrides = BTreeMap::new();
    let mut cur = 34;
    for _ in 0..n {
        let id = u64at(cur, b)?;
        let val = *b.get(cur + 8).ok_or_else(bad)?;
        overrides.insert(id, val % K);
        cur += 9;
    }
    if cur != b.len() {
        return Err(bad());
    }
    Ok(ToyEnv {
        base_offset,
        pos,
        seed,
        overrides,
    })
}

/// The toy [`EnvCodec`]: mints genesis-complete seeds, slices a corpus base into a
/// branch-local mutation at the base's snapshot offset (`pos`), and recomposes a
/// branch-local delta back to genesis-complete. It is the only place the toy's
/// schema is interpreted on the engine's behalf — the engine itself stays
/// schema-blind.
#[derive(Clone, Debug, Default)]
pub struct ToyCodec;

impl EnvCodec for ToyCodec {
    fn seeded(&self, seed: u64) -> Environment {
        encode(&ToyEnv {
            base_offset: 0,
            pos: 0,
            seed,
            overrides: BTreeMap::new(),
        })
    }

    fn mutate(&self, base: &Environment, salt: u64) -> Environment {
        // A corpus base is genesis-complete; produce a branch-local delta keyed
        // from the base snapshot's offset (`pos` — `SNAP_AT` for a genesis-rooted
        // base, `SNAP_AT2` for a nested one), the suffix the snapshot will branch
        // into. Keep the base seed so a later genesis recompose is PRNG-consistent.
        let b = decode(base).unwrap_or(ToyEnv {
            base_offset: 0,
            pos: SNAP_AT,
            seed: salt,
            overrides: BTreeMap::new(),
        });
        let offset = b.pos.min(TOTAL_DECISIONS);
        let local_len = (TOTAL_DECISIONS - offset).max(1);
        let mut overrides: BTreeMap<u64, u8> = b
            .overrides
            .iter()
            .filter(|(abs, _)| **abs >= offset)
            .map(|(abs, v)| (abs - offset, *v))
            .collect();
        // Tweak exactly one suffix override, deterministically by salt.
        let pick = salt % local_len;
        let val = ((salt >> 8) % K as u64) as u8;
        overrides.insert(pick, val);
        encode(&ToyEnv {
            base_offset: offset,
            pos: TOTAL_DECISIONS,
            seed: b.seed,
            overrides,
        })
    }

    fn compose(&self, base: &Environment, branch_local: &Environment) -> Environment {
        let b = decode(base).unwrap_or(ToyEnv {
            base_offset: 0,
            pos: 0,
            seed: 0,
            overrides: BTreeMap::new(),
        });
        let d = decode(branch_local).unwrap_or(ToyEnv {
            base_offset: SNAP_AT,
            pos: TOTAL_DECISIONS,
            seed: b.seed,
            overrides: BTreeMap::new(),
        });
        let k = d.base_offset;
        // Genesis prefix from base (abs < k) + the delta re-keyed onto the end. The
        // result's `pos` is the delta's capture point, so a composed nested base
        // still records *its* snapshot offset for a later `mutate`.
        let mut overrides: BTreeMap<u64, u8> = b
            .overrides
            .iter()
            .filter(|(abs, _)| **abs < k)
            .map(|(a, v)| (*a, *v))
            .collect();
        for (lid, v) in &d.overrides {
            overrides.insert(lid + k, *v);
        }
        encode(&ToyEnv {
            base_offset: 0,
            pos: d.pos,
            seed: b.seed,
            overrides,
        })
    }
}

// ---- the toy machine ----

/// A captured snapshot: the frozen prefix plus the env active when it was taken
/// (so `replay` reproduces the snapshotted continuation verbatim).
#[derive(Clone, Debug)]
struct Snap {
    frozen_pos: u64,
    answers: Vec<u8>,
    coverage: Vec<u8>,
    seed: u64,
    overrides_abs: BTreeMap<u64, u8>,
}

/// The deterministic toy machine.
#[derive(Clone, Debug)]
pub struct ToyMachine {
    // Snapshot pool.
    snaps: BTreeMap<u64, Snap>,
    next_snap: u64,
    dropped: BTreeSet<u64>,
    // Current Timeline state.
    branch_start: u64,
    seed: u64,
    overrides_abs: BTreeMap<u64, u8>,
    answers: Vec<u8>,
    coverage: Vec<u8>,
    awaiting: Option<u64>,
    // Fork points already snapshotted this run (a run forks at most once per point,
    // and only at points strictly ahead of its branch origin).
    snapped_at: BTreeSet<u64>,
    // Error injection (gate 5): fail the Nth `run` (1-based) with a transport error.
    fail_after: Option<u64>,
    run_calls: u64,
    // Error injection (gate 5): fail the next `snapshot` with NotQuiescent.
    fail_snapshot: bool,
    // Error injection: fail every `recorded_env` (drives the "snapshot succeeded but
    // the prefix-env capture failed" handle-leak path).
    fail_recorded_env: bool,
}

impl Default for ToyMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl ToyMachine {
    /// A fresh machine, quiescent at boot.
    pub fn new() -> Self {
        Self {
            snaps: BTreeMap::new(),
            next_snap: 1,
            dropped: BTreeSet::new(),
            branch_start: 0,
            seed: 0,
            overrides_abs: BTreeMap::new(),
            answers: Vec::new(),
            coverage: vec![0; COVERAGE_LEN],
            awaiting: None,
            snapped_at: BTreeSet::new(),
            fail_after: None,
            run_calls: 0,
            fail_snapshot: false,
            fail_recorded_env: false,
        }
    }

    /// Make this machine raise a [`MachineError::Transport`] on the `n`-th `run`
    /// (1-based) — the backend-fault injection the two-error-categories gate
    /// drives.
    pub fn fail_after(mut self, n: u64) -> Self {
        self.fail_after = Some(n);
        self
    }

    /// Make the next `snapshot` fail with [`MachineError::NotQuiescent`] — drives
    /// the `Explorer::new` "initial snapshot failed" error path.
    pub fn fail_snapshot(mut self) -> Self {
        self.fail_snapshot = true;
        self
    }

    /// Make every `recorded_env` fail with a transport error — drives the
    /// "snapshot succeeded but the prefix-env capture failed" handle-leak path at a
    /// `SnapshotPoint`.
    pub fn fail_recorded_env(mut self) -> Self {
        self.fail_recorded_env = true;
        self
    }

    /// Live (minted, not dropped) snapshot handles — the genesis snapshot plus
    /// the kept corpus bases. Used by the corpus-GC gate to prove evicted handles
    /// were really released, not leaked.
    pub fn live_snaps(&self) -> usize {
        self.snaps.len()
    }

    /// How many snapshot handles have been dropped.
    pub fn dropped_count(&self) -> usize {
        self.dropped.len()
    }

    /// Push `ans` for absolute index `i`, updating the AFL edge map. `prev` is the
    /// previous answer (a `0xFF` sentinel at the start of the log).
    fn record(&mut self, i: u64, prev: u8, ans: u8) {
        let edge = ((i.wrapping_mul(K as u64).wrapping_add(ans as u64))
            ^ (prev as u64).wrapping_mul(0x100)) as usize
            % COVERAGE_LEN;
        self.coverage[edge] = self.coverage[edge].saturating_add(1);
        self.answers.push(ans);
    }

    /// The terminal stop, a pure function of the full answer log. Crashes and
    /// assertions are reachable by the search so the bug-reporting gates have real
    /// bugs to find — including one in the *deep suffix* (`a[7]`) so a bug can be
    /// found below a nested (`SNAP_AT2`) snapshot, whose prefix is already frozen.
    fn terminal_stop(&self, vtime: VTime) -> StopReason {
        let a = &self.answers;
        if a.get(2) == Some(&2) && a.get(4) == Some(&2) {
            return StopReason::Crash {
                vtime,
                info: vec![0x02, 0x04],
            };
        }
        if a.get(5) == Some(&3) {
            return StopReason::Assertion {
                vtime,
                id: 5,
                data: vec![3],
            };
        }
        if a.get(7) == Some(&2) {
            return StopReason::Assertion {
                vtime,
                id: 7,
                data: vec![2],
            };
        }
        StopReason::Quiescent { vtime }
    }

    /// Load a (decoded) env as the active branch env, re-keying its branch-local
    /// overrides to absolute indices from `branch_start`.
    fn load_env(&mut self, e: &ToyEnv) {
        self.seed = e.seed;
        self.overrides_abs = e
            .overrides
            .iter()
            .map(|(lid, v)| (lid + self.branch_start, *v))
            .collect();
    }

    /// Whether `snap` is a live (minted, not dropped) handle.
    fn live(&self, snap: SnapId) -> Result<&Snap, MachineError> {
        if self.dropped.contains(&snap.0) {
            return Err(MachineError::UnknownSnapshot(snap.0));
        }
        self.snaps
            .get(&snap.0)
            .ok_or(MachineError::UnknownSnapshot(snap.0))
    }
}

impl Machine for ToyMachine {
    fn branch(&mut self, snap: SnapId, env: &Environment) -> Result<(), MachineError> {
        let s = self.live(snap)?.clone();
        let decoded = decode(env)?;
        self.branch_start = s.frozen_pos;
        self.answers = s.answers;
        self.coverage = s.coverage;
        self.awaiting = None;
        self.snapped_at.clear();
        self.run_calls = 0;
        self.load_env(&decoded);
        Ok(())
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
        let s = self.live(snap)?.clone();
        self.branch_start = s.frozen_pos;
        self.answers = s.answers;
        self.coverage = s.coverage;
        self.awaiting = None;
        self.snapped_at.clear();
        self.run_calls = 0;
        self.seed = s.seed;
        self.overrides_abs = s.overrides_abs;
        Ok(())
    }

    fn run(
        &mut self,
        until: &StopConditions,
        resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError> {
        self.run_calls += 1;
        if let Some(n) = self.fail_after
            && self.run_calls >= n
        {
            return Err(MachineError::Transport("injected backend fault".into()));
        }

        // Answer a previously-surfaced decision with the staged resolve.
        if let Some(local_id) = self.awaiting.take() {
            let abs = self.branch_start + local_id;
            let prev = self.answers.last().copied().unwrap_or(0xFF);
            let ans = match resolve {
                Some(a) if !a.0.is_empty() => {
                    let b = a.0[0] % K;
                    self.overrides_abs.insert(abs, b); // a real, recorded override
                    b
                }
                // An empty/absent answer falls through to the seed (no override),
                // so a declining strategy keeps the artifact a pure seed.
                _ => seed_answer(self.seed, abs),
            };
            self.record(abs, prev, ans);
        }

        loop {
            let abs = self.answers.len() as u64;
            let vt = VTime(abs.saturating_mul(VTIME_STEP));

            if let Some(d) = until.deadline
                && vt.0 >= d.0
            {
                return Ok(StopReason::Deadline { vtime: vt });
            }
            if abs >= TOTAL_DECISIONS {
                return Ok(self.terminal_stop(vt));
            }
            // Fork a SnapshotPoint at any configured point strictly ahead of this
            // run's branch origin (so a run off a SNAP_AT snapshot forks at the
            // deeper SNAP_AT2 — a nested, non-genesis-rooted base — and a run off a
            // SNAP_AT2 snapshot forks nowhere). The prefix `0..abs` is frozen here.
            if (until.on.0 & SNAP_BIT) != 0
                && SNAP_POINTS.contains(&abs)
                && abs > self.branch_start
                && !self.snapped_at.contains(&abs)
            {
                self.snapped_at.insert(abs);
                return Ok(StopReason::SnapshotPoint { vtime: vt });
            }
            // A carried override pins the decision (no surface), exactly as a
            // recorded reproducer replays.
            if let Some(&b) = self.overrides_abs.get(&abs) {
                let prev = self.answers.last().copied().unwrap_or(0xFF);
                self.record(abs, prev, b);
                continue;
            }
            let class = (abs % NUM_CLASSES) as u32;
            if (until.on.0 & (1 << class)) != 0 {
                self.awaiting = Some(abs - self.branch_start);
                return Ok(StopReason::Decision {
                    vtime: vt,
                    id: abs,
                    ctx: vec![(abs & 0xff) as u8, class as u8],
                });
            }
            let prev = self.answers.last().copied().unwrap_or(0xFF);
            let ans = seed_answer(self.seed, abs);
            self.record(abs, prev, ans);
        }
    }

    fn snapshot(&mut self) -> Result<SnapId, MachineError> {
        if self.fail_snapshot {
            return Err(MachineError::NotQuiescent);
        }
        // Snapshots are quiescent-only: never while a decision is armed.
        if self.awaiting.is_some() {
            return Err(MachineError::NotQuiescent);
        }
        let id = self.next_snap;
        self.next_snap += 1;
        self.snaps.insert(
            id,
            Snap {
                frozen_pos: self.answers.len() as u64,
                answers: self.answers.clone(),
                coverage: self.coverage.clone(),
                seed: self.seed,
                overrides_abs: self.overrides_abs.clone(),
            },
        );
        Ok(SnapId(id))
    }

    fn drop_snap(&mut self, snap: SnapId) -> Result<(), MachineError> {
        if self.snaps.remove(&snap.0).is_none() {
            return Err(MachineError::UnknownSnapshot(snap.0));
        }
        self.dropped.insert(snap.0);
        Ok(())
    }

    fn hash(&mut self) -> Result<[u8; 32], MachineError> {
        let mut h = Sha256::new();
        h.update(b"toy.machine.state.v1");
        h.update(&self.answers);
        Ok(h.finalize().into())
    }

    fn coverage(&self) -> &[u8] {
        &self.coverage
    }

    fn recorded_env(&self) -> Result<Environment, MachineError> {
        if self.fail_recorded_env {
            return Err(MachineError::Transport(
                "injected recorded_env fault".into(),
            ));
        }
        // Branch-local: overrides re-keyed to indices since this branch.
        let overrides = self
            .overrides_abs
            .iter()
            .filter(|(abs, _)| **abs >= self.branch_start)
            .map(|(abs, v)| (abs - self.branch_start, *v))
            .collect();
        Ok(encode(&ToyEnv {
            base_offset: self.branch_start,
            // The current position: the snapshot's frozen-prefix length when
            // captured at a SnapshotPoint, `TOTAL_DECISIONS` at a terminal stop.
            pos: self.answers.len() as u64,
            seed: self.seed,
            overrides,
        }))
    }
}

/// Spawns fresh toy machines (the in-crate stand-in for the R2 adapter's
/// `MachineFactory`).
#[derive(Clone, Debug, Default)]
pub struct ToyFactory;

impl MachineFactory for ToyFactory {
    type M = ToyMachine;
    fn spawn(&self) -> ToyMachine {
        ToyMachine::new()
    }
}

/// The toy's local seed answer: a SplitMix64 of `(seed, absolute index)`, so a
/// decision is answered the same way whether reached from genesis or resumed from
/// a mid-run branch (the property that lets a branch-local reproducer recompose to
/// a genesis-complete one).
pub fn seed_answer(seed: u64, idx: u64) -> u8 {
    let mut z = seed.wrapping_add(idx.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z % K as u64) as u8
}

/// A driver that runs one machine to a terminal stop with no strategy — every
/// surfaced decision is answered by the staged `answer` (or the seed if `None`),
/// and any `SnapshotPoint` is snapshotted and stepped past. Used by the replay
/// gates to re-run a recorded env and compare hashes.
pub fn drive_to_terminal(
    m: &mut ToyMachine,
    until: &StopConditions,
    answer: Option<&Answer>,
) -> Result<StopReason, MachineError> {
    let mut resolve: Option<Answer> = None;
    loop {
        let stop = m.run(until, resolve.as_ref())?;
        match stop {
            StopReason::Decision { .. } => resolve = answer.cloned(),
            StopReason::SnapshotPoint { .. } => {
                m.snapshot()?;
                resolve = None;
            }
            terminal => return Ok(terminal),
        }
    }
}

/// Drive a machine forward (seed-answering every surfaced decision) until its next
/// `SnapshotPoint`, snapshot there, and return `(snap, recorded_env at the fork)` —
/// the snapshot handle plus its branch-local prefix env. Used by the nested-snapshot
/// replay gate to fork a snapshot *below* a non-genesis base. Panics if the run
/// reaches a terminal stop before forking.
pub fn drive_to_snapshot(m: &mut ToyMachine, until: &StopConditions) -> (SnapId, Environment) {
    loop {
        match m.run(until, None).expect("toy run") {
            StopReason::Decision { .. } => continue, // seed-answered on the next run(None)
            StopReason::SnapshotPoint { .. } => {
                let snap = m.snapshot().expect("snapshot");
                let env = m.recorded_env().expect("recorded_env");
                return (snap, env);
            }
            other => panic!("expected a SnapshotPoint before terminating, got {other:?}"),
        }
    }
}

/// A tiny order-independent FNV-1a checksum, used by [`PinTactic`] to fold the
/// decision ctx into its draw (the open-loop analogue of the pre-refactor
/// coverage strategy's ctx term — the live-coverage term is gone by ruling).
pub fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A test tactic that **pins** every surfaced decision: one campaign-stream
/// draw folded with the ctx checksum, low byte answered. Open-loop by
/// construction (state-free; inputs are exactly `(pt, rng)`), and it records
/// real overrides into the reproducer — the pinning half the replay gates need.
#[derive(Clone, Debug, Default)]
pub struct PinTactic;

impl Tactic for PinTactic {
    fn decide(&mut self, pt: &DecisionPoint, rng: &mut Prng) -> Answer {
        let r = rng.next_u64() ^ fnv(&pt.ctx);
        Answer(vec![(r & 0xff) as u8])
    }
}

/// The default composition with the answering half swapped for [`PinTactic`]
/// — the coverage-guided campaign shape the replay/GC/smoke gates drive.
pub fn pin_composition() -> Composition {
    Composition {
        tactic: Box::new(PinTactic),
        selector: Box::new(ExploreExploitSelector::new()),
        archive: Box::new(CoverageArchive::new()),
        oracle: Box::new(TerminalOracle::new()),
        cells: Box::new(IdentityCells::new()),
        sensors: Vec::new(),
    }
}

/// The pure-DST composition: decline every decision, always explore from
/// genesis (the pre-refactor `SeedStrategy` decomposed).
pub fn seed_composition() -> Composition {
    Composition {
        tactic: Box::new(DeclineTactic::new()),
        selector: Box::new(GenesisSelector::new()),
        archive: Box::new(CoverageArchive::new()),
        oracle: Box::new(TerminalOracle::new()),
        cells: Box::new(IdentityCells::new()),
        sensors: Vec::new(),
    }
}

/// Proptest config: full case count natively, cut hard under Miri (kept for
/// portability even though this crate has no `unsafe`).
pub fn config(cases: u32) -> ProptestConfig {
    let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 16 } else { cases });
    if cfg!(miri) {
        cfg.failure_persistence = None;
    }
    cfg
}
