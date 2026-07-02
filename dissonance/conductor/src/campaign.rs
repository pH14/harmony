// SPDX-License-Identifier: AGPL-3.0-or-later
//! # The first campaign (task 60): find a planted bug, reproduce it N/N
//!
//! This is the milestone the project exists for. With task 58 (the loop) and
//! task 59 (host faults) in place, the campaign runs the whole
//! Modulation/Progression mechanism against a workload carrying a **planted,
//! fault-triggerable bug**:
//!
//! 1. **Snapshot once** — seal a base mid-workload (the campaign's
//!    genesis-equivalent), retrying past non-quiescent boundaries (task 41).
//! 2. **Search** — loop a seed-driven schedule: each branch mints a *small,
//!    seeded host-fault schedule* ([`mint_fault_env`]), `branch`es it off the
//!    base, `run`s to the deadline / terminal, and judges the terminal with the
//!    workload-aware **crash oracle** ([`CampaignOracle`]). The campaign is
//!    started with **no knowledge of the trigger** — it just explores seeds.
//! 3. **Emit + verify** — on the first `Crash` the oracle calls a bug, emit the
//!    [`Bug`] with its genesis-complete reproducer, then **replay that env `N`
//!    times** and require the identical crash (same terminal `StopReason`, same
//!    `state_hash`) every time ([`verify_campaign`]). A nominal-seed control run
//!    (no faults) must **not** crash — proving the trigger is adversity-gated.
//!
//! It is the deliberate first step of the fuzzer-validation discipline: prove
//! the finder against a *seeded* bug before investing in search cleverness
//! (coverage/cells/strategy are the deferred SDK epoch — task-60 non-goals).
//!
//! ## Two paths, one loop
//!
//! [`run_campaign`] is generic over [`Machine`], exactly like
//! [`run_sweep`](crate::run_sweep): the **portable** gate drives it against the
//! in-crate [`ToyPlantedMachine`](crate::planted::ToyPlantedMachine) (a planted
//! bug we own the trigger of); the **box milestone** drives the identical loop
//! against the real socket `Machine` + the Postgres-campaign image on patched
//! KVM. The only workload-aware knowledge in the whole path is the oracle's
//! terminal convention (below) and the search space's rough scope — everything
//! else is workload-blind.
//!
//! ## The crash oracle mapping (workload-aware, `map_terminal` convention)
//!
//! vmm-core maps guest terminals blind: `Hlt`/`DebugExit{0}` → `Quiescent`, a
//! non-zero `DebugExit{code}` → `Crash{Panic}`, a backend `Shutdown` (triple
//! fault / guest reboot) → `Crash{Shutdown}`. The Postgres image's *clean*
//! terminal is a forced reboot, so a **nominal run already reads as a
//! `Crash{Shutdown}`**. The planted bug therefore signals through a **different**
//! crash class — an isa-debug-exit `FAIL` → `Crash{Panic}`. The oracle keys on
//! exactly the crash-kind byte the adapter prepends to `info`
//! ([`CRASH_KIND_PANIC`] vs [`CRASH_KIND_SHUTDOWN`]): a non-benign crash (or an
//! SDK assertion) is the bug; the benign reboot terminal is not. Interpreting
//! this convention is the workload-aware caller's job (task-58 IMPLEMENTATION.md),
//! which is why the mapping lives here and not in the substrate.

use environment::{BitMask, EnvSpec, FaultPolicy, HostFault};
use explorer::{
    AdapterEnv, Bug, EnvCodec, Environment, Machine, MachineError, Oracle, Prng, RunTrace,
    StopConditions, StopMask, StopReason, TerminalOracle, VTime,
};

use crate::{RunRow, probe_vtime};

/// The crash-kind byte the R2 adapter prepends to a `Crash`'s `info` for an
/// isa-debug-exit panic (`control_proto::CrashKind::Panic`). On the box the
/// planted bug's supervised process writes a non-zero code to isa-debug-exit
/// `0xF4`, which vmm-core maps to `Crash{Panic}`; this is the campaign's
/// **planted-bug** signal.
pub const CRASH_KIND_PANIC: u8 = 0;
/// The crash-kind byte for a triple fault (`control_proto::CrashKind::TripleFault`).
/// Also treated as a bug by the default oracle (it is not the benign terminal).
pub const CRASH_KIND_TRIPLE_FAULT: u8 = 1;
/// The crash-kind byte for a backend `Shutdown` (`control_proto::CrashKind::Shutdown`)
/// — a guest-initiated shutdown / forced reboot. On the Postgres workload this is
/// the **clean** terminal, so the default oracle treats it as **benign**, not a
/// bug.
pub const CRASH_KIND_SHUTDOWN: u8 = 2;

/// The workload-aware **crash oracle**: judges a finished run's terminal into an
/// optional [`Bug`], keyed on the crash-kind convention above. A
/// [`Crash`](StopReason::Crash) whose leading `info` byte is **not** the benign
/// reboot terminal — or an [`Assertion`](StopReason::Assertion) — is a bug;
/// everything else (the benign reboot, a `Quiescent`/`Deadline` stop, a
/// mid-run non-terminal) is not.
///
/// The [`Bug`]'s fingerprint is the explorer's canonical one (delegated to
/// [`TerminalOracle`]), so a campaign bug dedups identically to any other.
#[derive(Clone, Copy, Debug)]
pub struct CampaignOracle {
    /// The crash-kind byte that is the workload's **clean** terminal (not a
    /// bug). Defaults to [`CRASH_KIND_SHUTDOWN`] (the Postgres reboot terminal).
    pub benign_crash_kind: u8,
}

impl Default for CampaignOracle {
    fn default() -> Self {
        Self {
            benign_crash_kind: CRASH_KIND_SHUTDOWN,
        }
    }
}

impl CampaignOracle {
    /// An oracle whose `benign_crash_kind` is the workload's clean terminal.
    pub fn new(benign_crash_kind: u8) -> Self {
        Self { benign_crash_kind }
    }

    /// Whether `stop` is the planted bug: an [`Assertion`](StopReason::Assertion),
    /// or a [`Crash`](StopReason::Crash) whose kind byte is not the benign
    /// terminal. Pure and total — the proptested classification.
    pub fn is_planted_bug(&self, stop: &StopReason) -> bool {
        match stop {
            StopReason::Assertion { .. } => true,
            StopReason::Crash { info, .. } => info.first().copied() != Some(self.benign_crash_kind),
            _ => false,
        }
    }
}

impl Oracle for CampaignOracle {
    /// `Some` exactly on a planted-bug terminal, with the explorer's canonical
    /// fingerprint. A benign reboot terminal (or any non-bug stop) is `None`,
    /// even though it *is* a `Crash` — the workload convention gates it out.
    fn judge(&self, t: &RunTrace) -> Option<Bug> {
        if self.is_planted_bug(&t.terminal) {
            // A planted-bug stop is always a `Crash`/`Assertion`, i.e.
            // `is_bug()`, so `TerminalOracle::judge` is always `Some` here —
            // reusing the canonical (fingerprint-stable) `Bug` construction.
            TerminalOracle::new().judge(t)
        } else {
            None
        }
    }
}

/// The seeded fault-search space + campaign budget. Kept small enough that the
/// naive seed search finds the planted bug within ~10²–10³ branches (task-60
/// requirement), and every dimension is tunable so the box campaign completes
/// within its lease.
#[derive(Clone, Debug)]
pub struct CampaignConfig {
    /// Seeds the campaign stream — the whole campaign is a pure function of this
    /// `(seed, machine)`, so a rerun explores the identical branch sequence.
    pub campaign_seed: u64,
    /// The search budget: at most this many branches before giving up (a
    /// no-bug-found campaign is a loud gate failure, never a silent pass).
    pub max_branches: u64,
    /// `N` for the N/N replay verification (the milestone bar is `25`).
    pub replay_n: usize,
    /// `Some(d)`: each branch runs to `base V-time + d` (the box mode — the
    /// workload's natural terminal is far away and the fault must land before
    /// it). `None`: run to the guest's terminal (the toy mode).
    pub deadline_delta: Option<u64>,
    /// Candidate guest-physical word addresses a [`CorruptMemory`](environment::HostFault::CorruptMemory) may target
    /// (the region the supervisor's bookkeeping lives in).
    pub gpa_candidates: Vec<u64>,
    /// The half-open window of **offsets past the base V-time** a fault may be
    /// scheduled at, `[lo, hi)`. On the box this brackets the sensitive phase of
    /// the supervised process.
    pub moment_window: (u64, u64),
    /// Candidate single-bit-flip indices (`0..64`) the search may use as the
    /// upset mask.
    pub mask_bits: Vec<u64>,
    /// Snapshot retry: on a `NotQuiescent` refusal, advance the guest by this
    /// much V-time and retry the seal …
    pub snapshot_retry_step: u64,
    /// … at most this many times before failing loud.
    pub snapshot_max_attempts: usize,
    /// The nominal control run's seed (no faults) — its run must not crash.
    pub nominal_seed: u64,
}

impl CampaignConfig {
    /// The portable toy configuration: a 4×4×8 = 128-schedule search space whose
    /// single trigger is the [`ToyPlantedMachine`](crate::planted::ToyPlantedMachine)'s
    /// planted upset (gpa `0x3000`, bit `31`, offset `3`). Found well within the
    /// 4096-branch budget; `deadline_delta = None` runs each branch to the toy's
    /// terminal.
    pub fn toy() -> Self {
        Self {
            campaign_seed: 0x00C0_FFEE_0000_0060,
            max_branches: 4096,
            replay_n: 25,
            deadline_delta: None,
            gpa_candidates: vec![0x1000, 0x2000, 0x3000, 0x4000],
            moment_window: (0, 8),
            mask_bits: vec![7, 15, 31, 63],
            snapshot_retry_step: 10_000,
            snapshot_max_attempts: 10_000,
            nominal_seed: 0x0000_0000_C0DE_0060,
        }
    }
}

/// Mint one branch's environment: a pure-seeded base plus a **single seeded
/// host-fault schedule** — one [`CorruptMemory`](environment::HostFault::CorruptMemory) whose `(gpa, mask, Moment)` is
/// drawn deterministically from `seed` over the config's search space. The
/// result is a genesis-complete adapter blob (`base_offset = 0`) the `Machine`
/// `branch`es and task-59's server stages + applies at the fault's `Moment`.
///
/// Draw order (gpa, Moment, bit) is fixed, so the schedule is a pure function of
/// `seed` — which is why the emitted [`Bug`]'s `seed` alone reproduces it. Never
/// panics on an empty search dimension (it falls back to a fixed value); a
/// mis-configured empty space simply searches trivially, caught by the
/// no-bug-found gate rather than a crash.
pub fn mint_fault_env(base_vtime: u64, seed: u64, cfg: &CampaignConfig) -> Environment {
    let mut p = Prng::new(seed);
    let gpa = pick(&cfg.gpa_candidates, &mut p).unwrap_or(0);
    let (lo, hi) = cfg.moment_window;
    let span = hi.saturating_sub(lo).max(1);
    let at = base_vtime
        .saturating_add(lo)
        .saturating_add(p.next_u64() % span);
    let bit = pick(&cfg.mask_bits, &mut p).unwrap_or(0) % 64;
    let mut spec = EnvSpec::Seeded {
        seed,
        policy: FaultPolicy::none(),
    };
    spec.perturb(
        HostFault::CorruptMemory {
            gpa,
            mask: BitMask(1u64 << bit),
        },
        at,
    );
    AdapterEnv {
        base_offset: 0,
        pos: 0,
        spec,
    }
    .encode()
}

/// Deterministically pick an element of `xs` from the stream, or `None` if empty.
fn pick(xs: &[u64], p: &mut Prng) -> Option<u64> {
    if xs.is_empty() {
        None
    } else {
        Some(xs[(p.next_u64() % xs.len() as u64) as usize])
    }
}

/// The planted bug the campaign found: which branch surfaced it, the seed that
/// mints its schedule, its genesis-complete reproducer env, and the terminal
/// `(stop, state_hash)` the emit-and-verify step must reproduce N/N.
#[derive(Clone, Debug)]
pub struct FoundBug {
    /// The zero-based branch index the crash first surfaced at (the naive
    /// time-to-find, in branches).
    pub branch_index: u64,
    /// The per-branch seed whose schedule triggers the bug.
    pub seed: u64,
    /// The genesis-complete reproducer: `branch(base, env)` + run reproduces the
    /// crash. This is the [`Bug::env`].
    pub env: Environment,
    /// The terminal stop the crash produced.
    pub stop: StopReason,
    /// The terminal `state_hash` of the finding run — every replay must match it.
    pub hash: [u8; 32],
    /// The emitted [`Bug`] artifact (env + stop + canonical fingerprint).
    pub bug: Bug,
}

/// The nominal-seed control run: a branch with **no faults** must not crash.
#[derive(Clone, Debug)]
pub struct NominalRow {
    /// The control run's terminal stop.
    pub stop: StopReason,
    /// The control run's terminal `state_hash`.
    pub hash: [u8; 32],
    /// Whether the oracle judged it a bug (the gate requires `false`).
    pub is_bug: bool,
}

/// Everything one [`run_campaign`] observed — the artifact the box gate records.
#[derive(Clone, Debug)]
pub struct CampaignReport {
    /// The V-time the base snapshot was sealed at.
    pub base_vtime: u64,
    /// How many snapshot attempts the base seal took (1 = first try).
    pub snapshot_attempts: usize,
    /// `state_hash` right after sealing the base.
    pub base_hash: [u8; 32],
    /// How many branches the search explored before finding the bug (or the
    /// whole budget, if none).
    pub branches_explored: u64,
    /// The planted bug the search found, or `None` if the budget was exhausted.
    pub found: Option<FoundBug>,
    /// The N replay observations of the found bug's env (empty if none found).
    pub replays: Vec<RunRow>,
    /// The nominal-seed control run.
    pub nominal: NominalRow,
}

/// Drive the whole campaign against `machine` (see the module doc): seal a base,
/// search seed-driven fault schedules until the oracle calls a crash, emit the
/// [`Bug`], replay it `cfg.replay_n` times, and run a nominal control. Fails
/// loudly on any [`MachineError`] other than the snapshot-retry `NotQuiescent`
/// (a transport/backend failure is never a bug).
pub fn run_campaign<M: Machine>(
    machine: &mut M,
    codec: &dyn EnvCodec,
    cfg: &CampaignConfig,
) -> Result<CampaignReport, MachineError> {
    let oracle = CampaignOracle::default();

    // 1. Where are we, and seal the base mid-workload (retrying past
    //    non-snapshottable boundaries — task 41).
    let mut vt = probe_vtime(machine)?;
    let mut attempts = 0usize;
    let base = loop {
        attempts += 1;
        match machine.snapshot() {
            Ok(snap) => break snap,
            Err(MachineError::NotQuiescent) => {
                if attempts >= cfg.snapshot_max_attempts {
                    return Err(MachineError::NotQuiescent);
                }
                let stop = machine.run(
                    &StopConditions {
                        deadline: Some(VTime(vt.saturating_add(cfg.snapshot_retry_step))),
                        on: StopMask::NONE,
                    },
                    None,
                )?;
                if !matches!(stop, StopReason::Deadline { .. }) {
                    return Err(MachineError::NotQuiescent);
                }
                vt = stop.vtime().0;
            }
            Err(e) => return Err(e),
        }
    };
    let base_vtime = vt;
    let base_hash = machine.hash()?;

    let until = StopConditions {
        deadline: cfg
            .deadline_delta
            .map(|d| VTime(base_vtime.saturating_add(d))),
        on: StopMask::NONE,
    };

    // 2. The search: seed-driven fault schedules until the oracle calls a crash.
    let mut campaign = Prng::new(cfg.campaign_seed);
    let mut found: Option<FoundBug> = None;
    let mut explored = 0u64;
    for i in 0..cfg.max_branches {
        explored = i + 1;
        let seed = campaign.next_u64();
        let env = mint_fault_env(base_vtime, seed, cfg);
        machine.branch(base, &env)?;
        let stop = machine.run(&until, None)?;
        let hash = machine.hash()?;
        let trace = trace_of(stop.clone(), env.clone());
        if let Some(bug) = oracle.judge(&trace) {
            found = Some(FoundBug {
                branch_index: i,
                seed,
                env,
                stop,
                hash,
                bug,
            });
            break;
        }
    }

    // 3. Emit + verify: replay the found reproducer N times, requiring the
    //    identical terminal `(stop, state_hash)` every time.
    let mut replays = Vec::new();
    if let Some(f) = &found {
        for _ in 0..cfg.replay_n {
            machine.branch(base, &f.env)?;
            let stop = machine.run(&until, None)?;
            let hash = machine.hash()?;
            replays.push(RunRow { stop, hash });
        }
    }

    // 4. The nominal control: a fault-free branch must not crash.
    let nominal_env = codec.seeded(cfg.nominal_seed);
    machine.branch(base, &nominal_env)?;
    let nominal_stop = machine.run(&until, None)?;
    let nominal_hash = machine.hash()?;
    let nominal_trace = trace_of(nominal_stop.clone(), nominal_env);
    let nominal = NominalRow {
        stop: nominal_stop,
        hash: nominal_hash,
        is_bug: oracle.judge(&nominal_trace).is_some(),
    };

    // 5. Release the base handle (corpus GC — exercises `drop`).
    machine.drop_snap(base)?;

    Ok(CampaignReport {
        base_vtime,
        snapshot_attempts: attempts,
        base_hash,
        branches_explored: explored,
        found,
        replays,
        nominal,
    })
}

/// Build the [`RunTrace`] the oracle judges from a run's terminal + branch env.
/// The env carried is the (genesis-complete) branch env — a genesis-rooted run's
/// reproducer already is genesis-complete (the task-93 compose ruling), so
/// [`Bug::env`] is portable and replayable as-is.
fn trace_of(stop: StopReason, env: Environment) -> RunTrace {
    RunTrace {
        terminal: stop,
        env,
        coverage: None,
        events: Vec::new(),
        records: Vec::new(),
    }
}

/// The task-60 acceptance gates over a [`CampaignReport`]:
///
/// 1. **Found** — the search surfaced a planted bug within budget.
/// 2. **N/N reproduction** — the emitted reproducer replays the identical crash
///    (same terminal [`StopReason`] **and** same `state_hash`) `n` times out of
///    `n`. A short replay vector (fewer than `n` observations) is itself a
///    failure — a campaign that could not run the full verification did not
///    prove reproduction.
/// 3. **Adversity-gated** — the nominal control run did not crash (the trigger
///    fires only under injected adversity, never nominally).
///
/// Returns every violated gate (empty = all pass), so a caller can report them
/// all at once.
pub fn verify_campaign(report: &CampaignReport, n: usize) -> Vec<String> {
    let mut failures = Vec::new();

    let Some(found) = &report.found else {
        failures.push(format!(
            "no planted bug found in {} branches — the campaign did not reproduce the milestone",
            report.branches_explored
        ));
        // Still report the nominal gate below so a mis-planted trigger that
        // crashes nominally is flagged too.
        check_nominal(report, &mut failures);
        return failures;
    };

    if report.replays.len() < n {
        failures.push(format!(
            "only {} replay(s) recorded — the N/N verification needs {n} to prove reproduction",
            report.replays.len()
        ));
    }
    for (i, r) in report.replays.iter().enumerate() {
        if r.hash != found.hash {
            failures.push(format!(
                "replay {i}: state_hash {} != finding hash {} — NOT reproducible",
                hex32(&r.hash),
                hex32(&found.hash)
            ));
        }
        if r.stop != found.stop {
            failures.push(format!(
                "replay {i}: stop {:?} != finding stop {:?} — NOT reproducible",
                r.stop, found.stop
            ));
        }
    }

    check_nominal(report, &mut failures);
    failures
}

/// The adversity-gated gate: the nominal control must not have crashed as a bug.
fn check_nominal(report: &CampaignReport, failures: &mut Vec<String>) {
    if report.nominal.is_bug {
        failures.push(format!(
            "the nominal control run reported a bug ({:?}) — the trigger is not adversity-gated",
            report.nominal.stop
        ));
    }
}

/// Lowercase hex of a 32-byte digest.
pub fn hex32(digest: &[u8; 32]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Render the campaign run table — the artifact the box gate records in
/// IMPLEMENTATION.md (branches explored, the found reproducer, the N/N replay
/// result, the nominal control).
pub fn render_campaign_table(report: &CampaignReport, n: usize) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "base snapshot: sealed at V-time {} ({} attempt{}), capture state_hash {}",
        report.base_vtime,
        report.snapshot_attempts,
        if report.snapshot_attempts == 1 {
            ""
        } else {
            "s"
        },
        hex32(&report.base_hash),
    );
    match &report.found {
        None => {
            let _ = writeln!(out, "NO BUG FOUND in {} branches", report.branches_explored);
        }
        Some(f) => {
            let _ = writeln!(
                out,
                "planted bug found at branch {} (seed {:#018x}) after exploring {} branches",
                f.branch_index, f.seed, report.branches_explored
            );
            let _ = writeln!(
                out,
                "  finding stop {}, state_hash {}",
                crate::fmt_stop(&f.stop),
                hex32(&f.hash),
            );
            let _ = writeln!(out, "  fingerprint {}", hex32(&f.bug.fingerprint),);
            let identical = report
                .replays
                .iter()
                .all(|r| r.hash == f.hash && r.stop == f.stop);
            let _ = writeln!(
                out,
                "  replay verification: {}/{} {}",
                report.replays.len(),
                n,
                if identical && report.replays.len() >= n {
                    "identical (crash reproduced bit-for-bit)"
                } else {
                    "MISMATCH"
                },
            );
        }
    }
    let _ = writeln!(
        out,
        "nominal control (seed only, no faults): {} — {}",
        crate::fmt_stop(&report.nominal.stop),
        if report.nominal.is_bug {
            "REPORTED A BUG (trigger not adversity-gated!)"
        } else {
            "no bug (adversity-gated, as required)"
        },
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use explorer::{SpecEnvCodec, VTime};

    /// A crash trace with the given kind byte, for oracle tests.
    fn crash_trace(kind: u8) -> RunTrace {
        RunTrace {
            terminal: StopReason::Crash {
                vtime: VTime(100),
                info: vec![kind, 0x60],
            },
            env: SpecEnvCodec.seeded(1),
            coverage: None,
            events: Vec::new(),
            records: Vec::new(),
        }
    }

    /// The oracle keys on the crash-kind byte: Panic/TripleFault are bugs, the
    /// benign Shutdown reboot terminal is not, and non-crash stops are not.
    #[test]
    fn oracle_keys_on_crash_kind() {
        let o = CampaignOracle::default();
        assert!(o.judge(&crash_trace(CRASH_KIND_PANIC)).is_some());
        assert!(o.judge(&crash_trace(CRASH_KIND_TRIPLE_FAULT)).is_some());
        assert!(o.judge(&crash_trace(CRASH_KIND_SHUTDOWN)).is_none());

        let mut quiescent = crash_trace(CRASH_KIND_PANIC);
        quiescent.terminal = StopReason::Quiescent { vtime: VTime(50) };
        assert!(o.judge(&quiescent).is_none());

        let mut assertion = crash_trace(CRASH_KIND_PANIC);
        assertion.terminal = StopReason::Assertion {
            vtime: VTime(50),
            id: 3,
            data: vec![1],
        };
        assert!(o.judge(&assertion).is_some(), "an SDK assertion is a bug");
    }

    /// The emitted bug carries the branch env verbatim and the explorer's
    /// canonical fingerprint (so it dedups like any other bug).
    #[test]
    fn oracle_emits_canonical_bug() {
        let o = CampaignOracle::default();
        let t = crash_trace(CRASH_KIND_PANIC);
        let bug = o.judge(&t).expect("a panic crash is a bug");
        assert_eq!(bug.env, t.env);
        assert_eq!(bug.stop, t.terminal);
        // Same fingerprint as the explorer's canonical terminal oracle, so a
        // campaign bug dedups identically to any other.
        let canonical = TerminalOracle::new().judge(&t).expect("terminal oracle");
        assert_eq!(bug.fingerprint, canonical.fingerprint);
    }

    /// `mint_fault_env` is a pure function of the seed and lands the fault in the
    /// configured window with a single-bit mask; distinct seeds vary the schedule.
    #[test]
    fn mint_is_deterministic_and_in_space() {
        let cfg = CampaignConfig::toy();
        let e1 = mint_fault_env(1000, 42, &cfg);
        let e2 = mint_fault_env(1000, 42, &cfg);
        assert_eq!(e1, e2, "same seed → same schedule");

        let spec = AdapterEnv::decode(&e1).unwrap().spec;
        let faults: Vec<_> = spec.host_faults().collect();
        assert_eq!(faults.len(), 1, "one fault per branch");
        let (at, f) = faults[0];
        assert!((1000..1008).contains(&at), "moment in the window");
        match f {
            HostFault::CorruptMemory { gpa, mask } => {
                assert!(cfg.gpa_candidates.contains(&gpa));
                assert_eq!(mask.0.count_ones(), 1, "single-bit upset");
            }
            other => panic!("expected CorruptMemory, got {other:?}"),
        }

        // Some other seed produces a different schedule (the search actually
        // explores).
        assert_ne!(
            mint_fault_env(1000, 42, &cfg),
            mint_fault_env(1000, 7, &cfg)
        );
    }

    /// `verify_campaign` flags a no-find, a short/mismatched replay vector, and a
    /// nominal-crash trigger; and passes a clean report.
    #[test]
    fn verify_flags_each_gate() {
        let found = FoundBug {
            branch_index: 5,
            seed: 1,
            env: SpecEnvCodec.seeded(1),
            stop: StopReason::Crash {
                vtime: VTime(100),
                info: vec![CRASH_KIND_PANIC, 0x60],
            },
            hash: [0xAB; 32],
            bug: TerminalOracle::new()
                .judge(&crash_trace(CRASH_KIND_PANIC))
                .unwrap(),
        };
        let ok_replays: Vec<RunRow> = (0..3)
            .map(|_| RunRow {
                stop: found.stop.clone(),
                hash: found.hash,
            })
            .collect();
        let clean = CampaignReport {
            base_vtime: 10,
            snapshot_attempts: 1,
            base_hash: [0; 32],
            branches_explored: 6,
            found: Some(found.clone()),
            replays: ok_replays.clone(),
            nominal: NominalRow {
                stop: StopReason::Crash {
                    vtime: VTime(50),
                    info: vec![CRASH_KIND_SHUTDOWN],
                },
                hash: [1; 32],
                is_bug: false,
            },
        };
        assert_eq!(verify_campaign(&clean, 3), Vec::<String>::new());

        // No find.
        let mut nofind = clean.clone();
        nofind.found = None;
        nofind.replays = Vec::new();
        assert!(
            verify_campaign(&nofind, 3)
                .iter()
                .any(|f| f.contains("no planted bug found"))
        );

        // A single mismatched replay.
        let mut mism = clean.clone();
        mism.replays[1].hash = [0xFF; 32];
        assert!(
            verify_campaign(&mism, 3)
                .iter()
                .any(|f| f.contains("NOT reproducible"))
        );

        // Too few replays.
        let mut short = clean.clone();
        short.replays.truncate(1);
        assert!(
            verify_campaign(&short, 3)
                .iter()
                .any(|f| f.contains("needs 3"))
        );

        // Nominal control crashed as a bug.
        let mut nom = clean.clone();
        nom.nominal.is_bug = true;
        assert!(
            verify_campaign(&nom, 3)
                .iter()
                .any(|f| f.contains("adversity-gated"))
        );
    }
}
