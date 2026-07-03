// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task-75 gate — the **probe mechanism**, portable half.
//!
//! The frontier box gate (gate 5) proves a real planted convergence failure is
//! caught by a probe on a discarded branch, with a byte-identical archive/trunk
//! digest before and after. That is a property of the *engine's* probe plumbing,
//! not of KVM — so it is proven here over an in-crate deterministic **convergence
//! machine** that models "does the cluster converge once faults stop?": a run
//! that experiences a fault leaves the state *poisoned*, and a forward probe
//! (faults now removed) never re-quiesces. The same [`Explorer::probe`] that will
//! drive a live `Machine` drives this one unchanged.
//!
//! What the gate pins:
//! - a probe on a poisoned terminal catches the non-convergence as a `Bug`,
//!   built on a **throwaway branch** (the machine minted no snapshot, admitted
//!   no exemplar);
//! - the timeline is **uncontaminated**: the frontier, the live snapshot set,
//!   and the seal cache are byte-identical before and after the probe;
//! - a healthy terminal converges within budget and judges **clean**;
//! - `plan` returning `None` skips the forward run entirely (no VM time).

use std::collections::BTreeMap;

use explorer::{
    Answer, Bug, Composition, CoverageArchive, DeclineTactic, EnvCodec, Environment,
    ExploreExploitSelector, Explorer, FaultCoord, IdentityCells, Machine, MachineError, Moment,
    ProbeOracle, ProbePlan, RunTrace, SnapId, StopConditions, StopMask, StopReason, TerminalOracle,
    TerminalSig, VTime, VTimeCoord, mint_fingerprint,
};

// ---------------------------------------------------------------------------
// The convergence machine + codec
// ---------------------------------------------------------------------------

/// V-time a converging run needs to re-quiesce after its branch point.
const CONVERGE_WINDOW: u64 = 100;

/// The env the convergence machine ferries: a genesis-complete `base_offset`, a
/// seed, a one-byte fault marker (`0` = no fault), and a branch-local trail of
/// phase marks (so distinct runs produce distinct reproducers/hashes).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct ConvEnv {
    base_offset: u64,
    seed: u64,
    fault: u8,
    marks: Vec<u8>,
}

fn enc(e: &ConvEnv) -> Environment {
    Environment {
        blob_version: 1,
        bytes: serde_json::to_vec(e).expect("encode conv env"),
    }
}

fn dec(env: &Environment) -> Result<ConvEnv, MachineError> {
    serde_json::from_slice(&env.bytes).map_err(|_| MachineError::BadEnvironment(env.blob_version))
}

/// An env carrying a fault (the "original run" the probe interrogates).
fn faulted_env(fault: u8) -> Environment {
    enc(&ConvEnv {
        base_offset: 0,
        seed: 7,
        fault,
        marks: Vec::new(),
    })
}

/// The trivial [`EnvCodec`] over [`ConvEnv`]: a seed carries no fault (the
/// quiesced env), a mutation tweaks the fault byte, and compose folds a
/// branch-local delta onto a genesis-complete base by keeping the base's origin
/// and appending the delta's marks — genesis-complete iff the base is.
#[derive(Clone, Debug, Default)]
struct ConvCodec;

impl EnvCodec for ConvCodec {
    fn seeded(&self, seed: u64) -> Environment {
        enc(&ConvEnv {
            base_offset: 0,
            seed,
            fault: 0,
            marks: Vec::new(),
        })
    }

    fn mutate(&self, base: &Environment, salt: u64) -> Environment {
        let mut b = dec(base).unwrap_or(ConvEnv {
            base_offset: 0,
            seed: salt,
            fault: 0,
            marks: Vec::new(),
        });
        b.fault = (salt & 0xff) as u8;
        enc(&b)
    }

    fn compose(&self, base: &Environment, branch_local: &Environment) -> Environment {
        let b = dec(base).expect("compose base");
        let d = dec(branch_local).expect("compose delta");
        // Mirror the production `SpecEnvCodec` seed-mismatch guard, so this test
        // codec actually exercises the compose contract a forward probe must
        // honor (a `seeded(0)` probe delta on a non-zero-seeded base would panic
        // here — the round-3 P1 the `quiesce` fix prevents).
        assert_eq!(
            b.seed, d.seed,
            "ConvCodec::compose: seed mismatch (base {} vs delta {}) — a probe delta must be \
             quiesced from the original, not freshly seeded",
            b.seed, d.seed
        );
        let mut marks = b.marks.clone();
        marks.extend_from_slice(&d.marks);
        enc(&ConvEnv {
            base_offset: b.base_offset,
            seed: b.seed,
            fault: b.fault,
            marks,
        })
    }

    fn quiesce(&self, base: &Environment) -> Environment {
        // Same seed (compose-compatible), fault cleared (nominal forward run).
        let b = dec(base).expect("quiesce base");
        enc(&ConvEnv {
            base_offset: 0,
            seed: b.seed,
            fault: 0,
            marks: Vec::new(),
        })
    }
}

/// A captured convergence-machine state: the V-time it froze at, whether the run
/// that reached it was poisoned by a fault, and its phase-mark log.
#[derive(Clone, Debug)]
struct ConvSnap {
    vtime: u64,
    poisoned: bool,
    marks: Vec<u8>,
}

/// A deterministic machine modeling convergence-after-faults. A run "converges"
/// (re-quiesces) iff it currently carries an active fault **or** its state is not
/// poisoned; a poisoned state with faults removed never converges — exactly the
/// liveness bug a probe exists to find.
#[derive(Debug)]
struct ConvMachine {
    snaps: BTreeMap<u64, ConvSnap>,
    next: u64,
    branch_vtime: u64,
    vtime: u64,
    poisoned: bool,
    active_fault: bool,
    /// The seed of the env this branch was reseeded with — carried into the
    /// recorded delta so `compose` can enforce seed consistency (a probe that
    /// reseeds with a fresh `seeded(0)` instead of `quiesce`ing the original
    /// would surface as a compose seed mismatch — the round-3 P1).
    seed: u64,
    marks: Vec<u8>,
    coverage: Vec<u8>,
}

impl ConvMachine {
    fn new() -> Self {
        Self {
            snaps: BTreeMap::new(),
            next: 1,
            branch_vtime: 0,
            vtime: 0,
            poisoned: false,
            active_fault: false,
            seed: 0,
            marks: Vec::new(),
            coverage: vec![0u8; 8],
        }
    }

    /// Live (minted, not dropped) snapshot handles — the uncontamination witness.
    fn live_snaps(&self) -> usize {
        self.snaps.len()
    }
}

impl Machine for ConvMachine {
    fn branch(&mut self, snap: SnapId, env: &Environment) -> Result<(), MachineError> {
        let s = self
            .snaps
            .get(&snap.0)
            .ok_or(MachineError::UnknownSnapshot(snap.0))?
            .clone();
        let e = dec(env)?;
        self.branch_vtime = s.vtime;
        self.vtime = s.vtime;
        self.seed = e.seed;
        self.active_fault = e.fault != 0;
        // A fault poisons the state; a quiesced branch (fault 0) carries the
        // parent's poison forward unchanged.
        self.poisoned = s.poisoned || self.active_fault;
        self.marks = s.marks.clone();
        self.marks.push(b'B');
        self.coverage.iter_mut().for_each(|c| *c = 0);
        Ok(())
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
        let s = self
            .snaps
            .get(&snap.0)
            .ok_or(MachineError::UnknownSnapshot(snap.0))?
            .clone();
        self.branch_vtime = s.vtime;
        self.vtime = s.vtime;
        self.poisoned = s.poisoned;
        self.active_fault = false;
        self.marks = s.marks;
        Ok(())
    }

    fn run(
        &mut self,
        until: &StopConditions,
        _resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError> {
        let converge_at = self.branch_vtime + CONVERGE_WINDOW;
        // Converges iff a fault is masking the poison, or the state is clean.
        let converges = self.active_fault || !self.poisoned;
        if converges {
            if let Some(d) = until.deadline
                && d.0 < converge_at
            {
                self.vtime = d.0;
                self.marks.push(b'D');
                self.coverage[0] = self.coverage[0].saturating_add(1);
                return Ok(StopReason::Deadline { vtime: VTime(d.0) });
            }
            self.vtime = converge_at;
            self.marks.push(b'Q');
            self.coverage[1] = self.coverage[1].saturating_add(1);
            Ok(StopReason::Quiescent {
                vtime: VTime(converge_at),
            })
        } else {
            // Never converges: run to the deadline the probe always sets.
            let d = until.deadline.map(|d| d.0).unwrap_or(converge_at + 10_000);
            self.vtime = d;
            self.marks.push(b'D');
            self.coverage[2] = self.coverage[2].saturating_add(1);
            Ok(StopReason::Deadline { vtime: VTime(d) })
        }
    }

    fn snapshot(&mut self) -> Result<SnapId, MachineError> {
        let id = self.next;
        self.next += 1;
        self.snaps.insert(
            id,
            ConvSnap {
                vtime: self.vtime,
                poisoned: self.poisoned,
                marks: self.marks.clone(),
            },
        );
        Ok(SnapId(id))
    }

    fn drop_snap(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.snaps
            .remove(&snap.0)
            .ok_or(MachineError::UnknownSnapshot(snap.0))?;
        Ok(())
    }

    fn hash(&mut self) -> Result<[u8; 32], MachineError> {
        let mut h = [0u8; 32];
        for (i, m) in self.marks.iter().enumerate() {
            h[i % 32] ^= *m;
        }
        Ok(h)
    }

    fn coverage(&self) -> &[u8] {
        &self.coverage
    }

    fn recorded_env(&self) -> Result<Environment, MachineError> {
        // Branch-local: the marks since this branch, keyed from branch_vtime,
        // carrying the branch's seed so compose can check consistency.
        Ok(enc(&ConvEnv {
            base_offset: self.branch_vtime,
            seed: self.seed,
            fault: self.active_fault as u8,
            marks: self.marks.clone(),
        }))
    }
}

// ---------------------------------------------------------------------------
// The liveness probe oracle
// ---------------------------------------------------------------------------

/// A liveness probe: probe every quiesced terminal, and flag a `Bug` iff the
/// forward-convergence window did not re-quiesce within the budget (it hit its
/// deadline instead). `active` toggles whether `plan` fires at all — the "skip"
/// path.
struct LivenessProbe {
    budget: u64,
    active: bool,
}

impl ProbeOracle for LivenessProbe {
    fn plan(&self, t: &RunTrace) -> Option<ProbePlan> {
        if !self.active {
            return None;
        }
        // Only probe quiesced terminals; the deadline is an absolute V-time
        // (terminal V-time + the convergence budget).
        match t.terminal {
            StopReason::Quiescent { vtime } => Some(ProbePlan {
                horizon: StopConditions {
                    deadline: Some(VTime(vtime.0 + self.budget)),
                    on: StopMask::NONE,
                },
            }),
            _ => None,
        }
    }

    fn judge_probe(&self, _original: &RunTrace, probe: &RunTrace) -> Option<Bug> {
        // The probe converged iff it re-quiesced; a deadline means it never did.
        match probe.terminal {
            StopReason::Quiescent { .. } => None,
            _ => {
                let sig = TerminalSig::new("liveness", 0, probe.terminal.discriminant())
                    .with_detail(b"no-converge".to_vec());
                Some(Bug {
                    env: probe.env.clone(),
                    stop: probe.terminal.clone(),
                    fingerprint: mint_fingerprint(
                        &sig,
                        &FaultCoord::none(),
                        VTimeCoord::quantize(Moment(probe.terminal.vtime().0)),
                    ),
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Build an explorer over a fresh convergence machine, drive one "original" run
/// from genesis with the given fault to a quiescent terminal, snapshot it, and
/// return `(explorer, original trace, terminal snap)`.
fn setup(fault: u8) -> (Explorer<ConvMachine>, RunTrace, SnapId) {
    let parts = Composition {
        tactic: Box::new(DeclineTactic::new()),
        selector: Box::new(ExploreExploitSelector::new()),
        archive: Box::new(CoverageArchive::new()),
        oracle: Box::new(TerminalOracle::new()),
        cells: Box::new(IdentityCells::new()),
        sensors: Vec::new(),
    };
    let mut ex = Explorer::new(ConvMachine::new(), Box::new(ConvCodec), parts, 1).expect("new");
    let genesis = ex.genesis();

    // Drive the original run from genesis with the fault, to its quiescent
    // terminal, and snapshot that terminal state.
    let m = ex.machine_mut();
    let orig_env = faulted_env(fault);
    m.branch(genesis, &orig_env).expect("branch genesis");
    let until = StopConditions {
        deadline: None,
        on: StopMask::NONE,
    };
    let stop = m.run(&until, None).expect("run to terminal");
    assert!(
        matches!(stop, StopReason::Quiescent { .. }),
        "the original run must reach a quiescent terminal, got {stop:?}"
    );
    let terminal = m.snapshot().expect("snapshot terminal");
    let env = m.recorded_env().expect("recorded env");
    let original = RunTrace {
        terminal: stop,
        env,
        coverage: None,
        events: Vec::new(),
        records: Vec::new(),
    };
    (ex, original, terminal)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A probe on a **poisoned** terminal catches the non-convergence as a `Bug`,
/// and the timeline is demonstrably uncontaminated: the frontier, the live
/// snapshot set, and the seal cache are byte-identical before and after, and the
/// probe minted no snapshot (it ran on a throwaway branch).
#[test]
fn probe_catches_nonconvergence_uncontaminated() {
    let (mut ex, original, terminal) = setup(0xAB);
    let oracle = LivenessProbe {
        budget: 1000,
        active: true,
    };

    let before_frontier = serde_json::to_vec(ex.frontier()).expect("ser");
    let before_snaps = ex.machine_mut().live_snaps();
    let before_sealed = ex.sealed_count();

    let verdict = ex.probe(&oracle, &original, terminal).expect("probe");

    let bug = verdict.expect("a poisoned terminal never converges → a liveness bug");
    assert!(
        matches!(bug.stop, StopReason::Deadline { .. }),
        "the probe's terminal is the missed convergence deadline"
    );
    // The bug's reproducer is genesis-complete (base_offset 0) and folds the
    // original run plus the failed convergence window.
    let repro: ConvEnv = dec(&bug.env).expect("decode repro");
    assert_eq!(repro.base_offset, 0, "the reproducer is genesis-complete");

    // Uncontamination: nothing the campaign keeps changed.
    let after_frontier = serde_json::to_vec(ex.frontier()).expect("ser");
    assert_eq!(before_frontier, after_frontier, "frontier untouched");
    assert_eq!(
        before_snaps,
        ex.machine_mut().live_snaps(),
        "no snapshot minted or dropped by the probe"
    );
    assert_eq!(before_sealed, ex.sealed_count(), "seal cache untouched");
    // The probe never admitted to the archive.
    assert!(ex.frontier().is_empty(), "the probe admits no exemplar");
}

/// A **healthy** terminal converges within the budget and judges clean — the
/// nominal control.
#[test]
fn probe_passes_a_converging_terminal_clean() {
    let (mut ex, original, terminal) = setup(0x00);
    let oracle = LivenessProbe {
        budget: 1000,
        active: true,
    };
    let verdict = ex.probe(&oracle, &original, terminal).expect("probe");
    assert!(
        verdict.is_none(),
        "a fault-free terminal re-quiesces within budget → clean"
    );
    assert_eq!(ex.machine_mut().live_snaps(), 2, "genesis + terminal only");
}

/// A `plan` that returns `None` skips the forward run entirely: no verdict, and
/// the machine never left the terminal (no probe branch was taken).
#[test]
fn probe_skips_when_plan_declines() {
    let (mut ex, original, terminal) = setup(0xAB);
    let oracle = LivenessProbe {
        budget: 1000,
        active: false,
    };
    let before = ex.machine_mut().live_snaps();
    let verdict = ex.probe(&oracle, &original, terminal).expect("probe");
    assert!(verdict.is_none(), "a declined plan yields no verdict");
    assert_eq!(
        before,
        ex.machine_mut().live_snaps(),
        "no branch, no snapshot"
    );
}

/// The probe is deterministic: the same terminal probed twice yields byte-equal
/// verdicts (the offline-reproducibility discipline, applied to the live plane).
#[test]
fn probe_is_deterministic() {
    let oracle = LivenessProbe {
        budget: 1000,
        active: true,
    };
    let (mut ex1, o1, t1) = setup(0xAB);
    let (mut ex2, o2, t2) = setup(0xAB);
    let b1 = ex1.probe(&oracle, &o1, t1).expect("probe1").expect("bug1");
    let b2 = ex2.probe(&oracle, &o2, t2).expect("probe2").expect("bug2");
    assert_eq!(b1.fingerprint, b2.fingerprint);
    assert_eq!(b1.env.bytes, b2.env.bytes);
    assert_eq!(b1.stop, b2.stop);
}
