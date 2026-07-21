// SPDX-License-Identifier: AGPL-3.0-or-later
//! # The first campaign (task 60): find a planted bug, reproduce it N/N
//!
//! This is the milestone the project exists for. With task 58 (the loop) and
//! task 59 (host faults) in place, the campaign runs the whole
//! rollout/search loop mechanism against a workload carrying a **planted,
//! fault-triggerable bug**:
//!
//! 1. **Snapshot once** — seal a base mid-workload (the campaign's
//!    genesis-equivalent), retrying past non-quiescent boundaries (task 41).
//! 2. **Search** — loop a seed-driven schedule: each branch mints a *small,
//!    seeded host-fault schedule* ([`mint_fault_env`]), `branch`es it off the
//!    base, `run`s to the deadline / terminal, and judges the terminal with the
//!    workload-aware **crash oracle** ([`CrashOracle`]). The campaign is
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
//! ## The crash oracle mapping (workload-aware terminal convention)
//!
//! vmm-core maps guest terminals blind: `Hlt`/`DebugExit{0}` → `Quiescent`, a
//! non-zero `DebugExit{code}` → `Crash{Panic}`, a backend `Shutdown` (triple
//! fault / guest reboot) → `Crash{Shutdown}`. **A guest process cannot reach the
//! isa-debug-exit port** on the kata-derived container kernel (no
//! `CONFIG_X86_IOPL_IOPERM` / `CONFIG_DEVPORT` — proven on the box by the
//! supervisor's crash-channel self-test), so the planted bug cannot signal a
//! distinct `Crash{Panic}`. Instead the workload's `/init` maps the outcome to
//! **two distinct guest terminals the kernel itself produces**
//! (`harmony-linux/linux/campaign-init.sh`): the bug reboots (`reboot -f` →
//! `Crash{Shutdown}`) while a clean run halts (`halt -f` → `Quiescent`). So the
//! oracle keys on the terminal **class**: **any [`Crash`](StopReason::Crash) or
//! [`Assertion`](StopReason::Assertion) is the bug; a [`Quiescent`](StopReason::Quiescent)
//! (or `Deadline`) terminal is the clean run.** This is the standard
//! [`TerminalOracle`] rule — the workload arranges its terminals so it applies.
//! The [`Bug`]'s fingerprint is the explorer's canonical one, so a campaign bug
//! dedups identically to any other.
//!
//! Because the oracle sees only the terminal (not the serial), it cannot tell
//! the *planted-invariant* crash from an *incidental* one (a fault that corrupts
//! kernel memory and panics). The campaign relies on the **pinned ledger gpa**
//! (the bring-up reads it from `/proc/self/pagemap`) so that a targeted fault
//! only ever corrupts the supervisor's bookkeeping — making any resulting crash
//! *the* planted bug. The distinctive `CAMPAIGN_BUG:` serial marker the
//! supervisor prints is the human-visible confirmation.

use std::collections::BTreeMap;

use environment::{BitMask, EnvSpec, FaultPolicy, HostFault};
use explorer::{
    AdapterEnv, Bug, EnvCodec, GuestEvent, Machine, MachineError, Moment, Oracle, Prng, Reproducer,
    RunTrace, StopConditions, StopMask, StopReason, TerminalOracle,
};

use crate::stopwatch::{Phase, PhaseStats, Stopwatch};
use crate::{RunRow, probe_vtime};

/// How often (in search branches) `run_campaign` prints a progress line — the
/// line that makes a silent, hours-long run legible from `tail -f` (task 96).
const PROGRESS_INTERVAL: u64 = 32;

/// The crash-kind byte the R2 adapter prepends to a `Crash`'s `info`, mirroring
/// `control_proto::CrashKind`: `Panic` (a non-zero isa-debug-exit). The oracle
/// no longer keys on the kind (a guest process cannot reach isa-debug-exit here —
/// see the module doc); kept as the faithful mirror the toy machine stamps.
pub const CRASH_KIND_PANIC: u8 = 0;
/// The crash-kind byte for a triple fault (`control_proto::CrashKind::UnrecoverableFault`).
pub const CRASH_KIND_TRIPLE_FAULT: u8 = 1;
/// The crash-kind byte for a backend `Shutdown` (`control_proto::CrashKind::Shutdown`)
/// — a guest-initiated shutdown / forced reboot. On this workload the planted
/// bug's terminal (`reboot -f`), so it is the **bug** signal; the clean run halts
/// (`Quiescent`) instead.
pub const CRASH_KIND_SHUTDOWN: u8 = 2;

/// The workload-aware **crash oracle**: a finished run is the planted bug iff it
/// ended in a [`Crash`](StopReason::Crash) or [`Assertion`](StopReason::Assertion)
/// — the standard [`TerminalOracle`] rule. On this campaign workload `/init`
/// arranges the terminals so it applies: the bug reboots (`Crash{Shutdown}`)
/// while a clean run halts ([`Quiescent`](StopReason::Quiescent)), so a `Crash`
/// unambiguously means the injected upset tripped the supervisor's invariant (the
/// gpa is pinned to the ledger — see the module doc). The [`Bug`]'s fingerprint is
/// the explorer's canonical one, so a campaign bug dedups identically to any other.
#[derive(Clone, Copy, Debug, Default)]
pub struct CrashOracle;

impl CrashOracle {
    /// The campaign crash oracle (stateless).
    pub fn new() -> Self {
        Self
    }

    /// Whether `stop` is the planted bug: a [`Crash`](StopReason::Crash) or
    /// [`Assertion`](StopReason::Assertion) (the guest rebooted / asserted). A
    /// [`Quiescent`](StopReason::Quiescent) / `Deadline` / non-terminal stop is
    /// the clean run. Pure and total — the proptested classification.
    pub fn is_planted_bug(&self, stop: &StopReason) -> bool {
        stop.is_bug()
    }
}

impl Oracle for CrashOracle {
    /// `Some` exactly on a bug-bearing terminal (`Crash`/`Assertion`), with the
    /// explorer's canonical fingerprint; `None` on the clean `Quiescent` halt (or
    /// any other non-bug stop).
    fn judge(&self, t: &RunTrace) -> Option<Bug> {
        TerminalOracle::new().judge(t)
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
pub fn mint_fault_env(base_vtime: u64, seed: u64, cfg: &CampaignConfig) -> Reproducer {
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
    pub env: Reproducer,
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
    /// Per-phase host-side timing observations (task 96) — observation-only,
    /// hash-neutral: never read by the search loop or the [`verify_campaign`]
    /// gates, so it can never influence what a campaign finds. See the
    /// [`stopwatch`](crate::stopwatch) module doc for the invariant.
    pub timing: BTreeMap<Phase, PhaseStats>,
    /// Wall-clock seconds the whole campaign took, from the internal
    /// [`Stopwatch::new`] to completion. Observation-only, like `timing`.
    pub wall_secs: u64,
    /// `branches_explored / wall_secs * 3600`, as a ×10 fixed-point integer
    /// (e.g. `47` means 4.7 branches/hour). `0` when `wall_secs == 0` (no
    /// division panic, and a report from a >0-duration run never claims an
    /// infinite rate).
    pub branches_per_hour_x10: u64,
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
    let oracle = CrashOracle::new();
    // Observation-only host-side timing (task 96) — see the `stopwatch`
    // module doc. Never read to make a decision; folded into the report at
    // the end.
    let mut sw = Stopwatch::new();

    // 1. Where are we, and seal the base mid-workload (retrying past
    //    non-snapshottable boundaries — task 41). Timed as one `BaseSeal`
    //    span covering every retry, not per-attempt.
    let mut vt = probe_vtime(machine)?;
    let mut attempts = 0usize;
    let base = sw.time(Phase::BaseSeal, || {
        loop {
            attempts += 1;
            match machine.snapshot() {
                Ok((snap, _cut)) => return Ok(snap),
                Err(MachineError::NotQuiescent) => {
                    if attempts >= cfg.snapshot_max_attempts {
                        return Err(MachineError::NotQuiescent);
                    }
                    let stop = machine.run(
                        &StopConditions {
                            deadline: Some(Moment(vt.saturating_add(cfg.snapshot_retry_step))),
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
        }
    })?;
    let base_vtime = vt;
    let base_hash = machine.hash()?;

    // Arm the SDK **assertion** class (round-14): a cooperating guest's
    // `assert_always` violation surfaces as [`StopReason::Assertion`] — a [`Bug`]
    // the oracle judges. Under `StopMask::NONE` the run would sail PAST the
    // assertion to a clean terminal, and a real bug would go unjudged (a MISSED
    // detection, the worst campaign outcome). The deadline still bounds the run;
    // whichever comes first stops it.
    let until = StopConditions {
        deadline: cfg
            .deadline_delta
            .map(|d| Moment(base_vtime.saturating_add(d))),
        on: StopMask::ASSERTION,
    };

    // 2. The search: seed-driven fault schedules until the oracle calls a crash.
    let mut campaign = Prng::new(cfg.campaign_seed);
    let mut found: Option<FoundBug> = None;
    let mut explored = 0u64;
    for i in 0..cfg.max_branches {
        explored = i + 1;
        let seed = campaign.next_u64();
        let env = mint_fault_env(base_vtime, seed, cfg);
        sw.time(Phase::Branch, || machine.branch(base, &env))?;
        let stop = sw.time(Phase::Run, || machine.run(&until, None))?;
        let hash = sw.time(Phase::Hash, || machine.hash())?;
        let events = sw.time(Phase::Harvest, || machine_events(machine))?;
        let trace = trace_of(stop.clone(), env.clone(), events);
        let judged = sw.time(Phase::Judge, || oracle.judge(&trace));
        if let Some(bug) = judged {
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
        if explored.is_multiple_of(PROGRESS_INTERVAL) {
            print_progress(&sw, explored, cfg.max_branches);
        }
    }

    // 3. Emit + verify: replay the found reproducer N times, requiring the
    //    identical terminal `(stop, state_hash)` every time. `branch` + `run`
    //    + `hash` are timed together as one `Replay` span per iteration.
    let mut replays = Vec::new();
    if let Some(f) = &found {
        for _ in 0..cfg.replay_n {
            let (stop, hash) = sw.time(Phase::Replay, || -> Result<_, MachineError> {
                machine.branch(base, &f.env)?;
                let stop = machine.run(&until, None)?;
                let hash = machine.hash()?;
                Ok((stop, hash))
            })?;
            replays.push(RunRow { stop, hash });
        }
    }

    // 4. The nominal control: a fault-free branch must not crash. Timed as
    //    one `Nominal` span (branch + run + hash + the SDK event harvest).
    let nominal_env = codec.seeded(cfg.nominal_seed);
    let (nominal_stop, nominal_hash, nominal_events) =
        sw.time(Phase::Nominal, || -> Result<_, MachineError> {
            machine.branch(base, &nominal_env)?;
            let stop = machine.run(&until, None)?;
            let hash = machine.hash()?;
            let events = machine_events(machine)?;
            Ok((stop, hash, events))
        })?;
    let nominal_trace = trace_of(nominal_stop.clone(), nominal_env, nominal_events);
    let nominal = NominalRow {
        stop: nominal_stop,
        hash: nominal_hash,
        is_bug: oracle.judge(&nominal_trace).is_some(),
    };

    // 5. Release the base handle (pool GC — exercises `drop`).
    machine.drop_snap(base)?;

    let wall_secs = sw.elapsed_secs();
    // ×10 fixed-point branches/hour; `saturating_mul` guards the (unreachable
    // in practice) overflow case rather than panicking. 0 when wall_secs == 0
    // — no division panic, and a zero-duration report never claims an
    // infinite rate.
    let branches_per_hour_x10 = explored
        .saturating_mul(36_000)
        .checked_div(wall_secs)
        .unwrap_or(0);

    Ok(CampaignReport {
        base_vtime,
        snapshot_attempts: attempts,
        base_hash,
        branches_explored: explored,
        found,
        replays,
        nominal,
        timing: sw.stats(),
        wall_secs,
        branches_per_hour_x10,
    })
}

/// Print the task-96 progress line: how far the search is, wall-clock
/// elapsed, and the average per-branch cost of the three heaviest phases —
/// the line that makes a silent, hours-long run legible from `tail -f`.
fn print_progress(sw: &Stopwatch, explored: u64, max_branches: u64) {
    let stats = sw.stats();
    let avg = |p: Phase| {
        stats
            .get(&p)
            .map(|s| s.total_us / s.count.max(1))
            .unwrap_or(0)
    };
    println!(
        "[campaign-runner] progress: branch {explored}/{max_branches}, elapsed {}s, avg us — branch {} \
         run {} hash {}",
        sw.elapsed_secs(),
        avg(Phase::Branch),
        avg(Phase::Run),
        avg(Phase::Hash),
    );
}

/// Build the [`RunTrace`] the oracle judges from a run's terminal + branch env +
/// decoded SDK event stream. The env carried is the (genesis-complete) branch env
/// — a genesis-rooted run's reproducer already is genesis-complete (the task-93
/// compose ruling), so [`Bug::env`] is portable and replayable as-is.
fn trace_of(stop: StopReason, env: Reproducer, events: Vec<(Moment, GuestEvent)>) -> RunTrace {
    RunTrace {
        terminal: stop,
        env,
        coverage: None,
        events,
        records: Vec::new(),
    }
}

/// Fetch + decode the run's SDK event capture over the [`Machine`] seam (task 73):
/// the socket machine round-trips to the server-side capture; a machine with no
/// SDK (the toy machine) returns empty. The `CrashOracle`/`TerminalOracle` read
/// only the terminal, so the reconstructed `RunTrace.events` are populated for the
/// journal/film contract, not consulted by any verdict.
fn machine_events<M: Machine>(machine: &mut M) -> Result<Vec<(Moment, GuestEvent)>, MachineError> {
    let raw = machine.sdk_events()?;
    let normalized = crate::sdk_compat::decode_sdk(&raw)
        .map_err(|e| MachineError::Transport(format!("SDK capture failed to decode: {e}")))?;
    Ok(crate::sdk_compat::guest_events_of(&normalized))
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
    // The timing section (task 96): omitted entirely when `timing` is empty
    // (a synthetic report, or a code path with no stopwatch wired up) so a
    // report built without timing renders exactly as it did before this
    // field existed — additive, per the task's invariant 4.
    if !report.timing.is_empty() {
        let _ = writeln!(
            out,
            "timing: wall {}s, {}.{} branches/hour",
            report.wall_secs,
            report.branches_per_hour_x10 / 10,
            report.branches_per_hour_x10 % 10,
        );
        let _ = writeln!(
            out,
            "{:<10} {:>7} {:>9} {:>8} {:>8} {:>8}",
            "phase", "count", "total_s", "p50_ms", "p90_ms", "max_ms"
        );
        for (phase, stats) in &report.timing {
            let _ = writeln!(
                out,
                "{:<10} {:>7} {:>9} {:>8} {:>8} {:>8}",
                phase.as_str(),
                stats.count,
                stats.total_us / 1_000_000,
                stats.p50_us / 1_000,
                stats.p90_us / 1_000,
                stats.max_us / 1_000,
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use explorer::{EvidenceCut, Moment, SpecEnvCodec};

    /// [`CampaignConfig::toy`] with its **search space narrowed under Miri**
    /// (task 104, `hm-d4y`). The full toy space is 4 gpas × 4 mask bits × an
    /// 8-wide Moment window = 256 schedules, and the seeded search walks **868**
    /// branches before it hits the planted `(0x3000, bit 31, in-window)` trigger —
    /// ~6 min of interpreted work per end-to-end campaign test. Under Miri the
    /// space is narrowed to 2 × 2 × 4 = 16 schedules that still **bracket the
    /// trigger** (it stays a genuine three-dimensional search, just a coarser
    /// one), and the same campaign finds the bug at branch 9 with every
    /// [`verify_campaign`] gate still passing — found, N/N reproduced, nominal
    /// clean, and a timing sample for every phase. Native runs keep the full
    /// 256-schedule space and are byte-for-byte unchanged.
    fn toy_cfg() -> CampaignConfig {
        let base = CampaignConfig::toy();
        if cfg!(miri) {
            CampaignConfig {
                gpa_candidates: vec![0x2000, 0x3000],
                mask_bits: vec![15, 31],
                moment_window: (0, 4),
                ..base
            }
        } else {
            base
        }
    }

    /// A crash trace with the given kind byte, for oracle tests.
    fn crash_trace(kind: u8) -> RunTrace {
        RunTrace {
            terminal: StopReason::Crash {
                vtime: Moment(100),
                info: vec![kind, 0x60],
            },
            env: SpecEnvCodec.seeded(1),
            coverage: None,
            events: Vec::new(),
            records: Vec::new(),
        }
    }

    /// The oracle judges the terminal CLASS: any crash (whatever its kind byte —
    /// the bug reboots to `Crash{Shutdown}` here) or an assertion is the bug; the
    /// clean `Quiescent` halt and other non-terminal stops are not.
    #[test]
    fn oracle_judges_crash_as_bug_quiescent_as_clean() {
        let o = CrashOracle::new();
        assert!(
            o.judge(&crash_trace(CRASH_KIND_SHUTDOWN)).is_some(),
            "reboot crash is the bug"
        );
        assert!(o.judge(&crash_trace(CRASH_KIND_PANIC)).is_some());
        assert!(o.judge(&crash_trace(CRASH_KIND_TRIPLE_FAULT)).is_some());

        let mut quiescent = crash_trace(CRASH_KIND_SHUTDOWN);
        quiescent.terminal = StopReason::Quiescent { vtime: Moment(50) };
        assert!(o.judge(&quiescent).is_none(), "the clean halt is not a bug");

        let mut deadline = crash_trace(CRASH_KIND_SHUTDOWN);
        deadline.terminal = StopReason::Deadline { vtime: Moment(50) };
        assert!(o.judge(&deadline).is_none());

        let mut assertion = crash_trace(CRASH_KIND_SHUTDOWN);
        assertion.terminal = StopReason::Assertion {
            vtime: Moment(50),
            id: 3,
            data: vec![1],
        };
        assert!(o.judge(&assertion).is_some(), "an SDK assertion is a bug");
    }

    /// The emitted bug carries the branch env verbatim and the explorer's
    /// canonical fingerprint (so it dedups like any other bug).
    #[test]
    fn oracle_emits_canonical_bug() {
        let o = CrashOracle::new();
        let t = crash_trace(CRASH_KIND_SHUTDOWN);
        let bug = o.judge(&t).expect("a crash is a bug");
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
        // The bug reboots (Crash{Shutdown}); the clean control halts (Quiescent).
        let found = FoundBug {
            branch_index: 5,
            seed: 1,
            env: SpecEnvCodec.seeded(1),
            stop: StopReason::Crash {
                vtime: Moment(100),
                info: vec![CRASH_KIND_SHUTDOWN],
            },
            hash: [0xAB; 32],
            bug: TerminalOracle::new()
                .judge(&crash_trace(CRASH_KIND_SHUTDOWN))
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
                stop: StopReason::Quiescent { vtime: Moment(50) },
                hash: [1; 32],
                is_bug: false,
            },
            timing: BTreeMap::new(),
            wall_secs: 0,
            branches_per_hour_x10: 0,
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

    /// A toy [`Machine`] with a planted **SDK-assertion** bug, modeling the round-7
    /// StopMask gating exactly: an `assert_always` violation surfaces as
    /// [`StopReason::Assertion`] ONLY when the ASSERTION class is armed in
    /// `until.on`; unarmed, the run sails PAST it to a clean [`Quiescent`] (the
    /// guest continues) — a MISSED bug. The trigger is [`Trigger::toy`]'s
    /// `(gpa, mask, window)`, matched by [`CampaignConfig::toy`]'s search space.
    struct AssertMachine {
        trigger: crate::planted::Trigger,
        current: Reproducer,
        vtime: u64,
        snaps: std::collections::BTreeMap<u64, (u64, Reproducer)>,
        next: u64,
    }

    impl AssertMachine {
        fn new() -> Self {
            Self {
                trigger: crate::planted::Trigger::toy(),
                current: SpecEnvCodec.seeded(0),
                vtime: crate::planted::BASE_VTIME,
                snaps: std::collections::BTreeMap::new(),
                next: 1,
            }
        }
        /// Whether the active env carries the exact planted single-event upset.
        fn fires(&self) -> bool {
            let Ok(dec) = AdapterEnv::decode(&self.current) else {
                return false;
            };
            dec.spec.host_faults().any(|(m, f)| {
                matches!(f, HostFault::CorruptMemory { gpa, mask }
                    if gpa == self.trigger.gpa
                        && mask.0 == self.trigger.mask
                        && m >= self.trigger.window.0
                        && m < self.trigger.window.1)
            })
        }
    }

    impl Machine for AssertMachine {
        fn branch(&mut self, snap: explorer::SnapId, env: &Reproducer) -> Result<(), MachineError> {
            let (vt, _) = self
                .snaps
                .get(&snap.0)
                .ok_or(MachineError::UnknownSnapshot(snap.0))?;
            AdapterEnv::decode(env)?;
            self.vtime = *vt;
            self.current = env.clone();
            Ok(())
        }
        fn replay(&mut self, snap: explorer::SnapId) -> Result<(), MachineError> {
            let (vt, env) = self
                .snaps
                .get(&snap.0)
                .ok_or(MachineError::UnknownSnapshot(snap.0))?;
            self.vtime = *vt;
            self.current = env.clone();
            Ok(())
        }
        fn run(
            &mut self,
            until: &StopConditions,
            _resolve: Option<&explorer::Answer>,
        ) -> Result<StopReason, MachineError> {
            // Already-met deadline → the `probe_vtime` idiom (report current time).
            if let Some(d) = until.deadline
                && d.0 <= self.vtime
            {
                return Ok(StopReason::Deadline {
                    vtime: Moment(self.vtime),
                });
            }
            let vt = self.vtime.saturating_add(10);
            self.vtime = vt;
            let armed = until.on.0 & StopMask::ASSERTION.0 != 0;
            if self.fires() && armed {
                Ok(StopReason::Assertion {
                    vtime: Moment(vt),
                    id: 7,
                    data: vec![1],
                })
            } else {
                // Not triggered, OR the assertion class is unarmed (the guest runs
                // straight through the violation to its clean terminal).
                Ok(StopReason::Quiescent { vtime: Moment(vt) })
            }
        }
        fn snapshot(&mut self) -> Result<(explorer::SnapId, EvidenceCut), MachineError> {
            let id = self.next;
            self.next += 1;
            self.snaps.insert(id, (self.vtime, self.current.clone()));
            Ok((
                explorer::SnapId(id),
                EvidenceCut {
                    at: Moment(self.vtime),
                    sdk_events: 0,
                },
            ))
        }
        fn drop_snap(&mut self, snap: explorer::SnapId) -> Result<(), MachineError> {
            self.snaps
                .remove(&snap.0)
                .map(|_| ())
                .ok_or(MachineError::UnknownSnapshot(snap.0))
        }
        fn hash(&mut self) -> Result<[u8; 32], MachineError> {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(b"campaign-runner.campaign.assertmachine.v1");
            h.update(&self.current.bytes);
            Ok(h.finalize().into())
        }
        fn coverage(&self) -> &[u8] {
            &[]
        }
        fn recorded_env(&self) -> Result<Reproducer, MachineError> {
            Ok(self.current.clone())
        }
    }

    /// Round-14 P2(3): a planted SDK **assertion** IS reported by a campaign run —
    /// the campaign arms the ASSERTION class, so a cooperating guest's violation
    /// surfaces as a judged bug instead of going unnoticed. The same machine under
    /// `StopMask::NONE` runs straight past the violation (a clean `Quiescent`),
    /// demonstrating that the arming is exactly what makes the assertion
    /// discoverable.
    #[test]
    fn a_planted_sdk_assertion_is_reported_by_a_campaign_run() {
        // The gating the fix depends on: the SAME triggering env surfaces the
        // assertion ONLY when the class is armed.
        let mut g = AssertMachine::new();
        let b = g.snapshot().unwrap().0;
        // Craft the exact planted trigger env (gpa 0x3000, guard bit 31, in window).
        let mut spec = EnvSpec::Seeded {
            seed: 1,
            policy: FaultPolicy::none(),
        };
        spec.perturb(
            HostFault::CorruptMemory {
                gpa: 0x3000,
                mask: BitMask(1 << 31),
            },
            crate::planted::BASE_VTIME + 3,
        );
        let trigger_env = AdapterEnv {
            base_offset: 0,
            pos: 0,
            spec,
        }
        .encode();
        g.branch(b, &trigger_env).unwrap();
        assert!(
            matches!(
                g.run(
                    &StopConditions {
                        deadline: None,
                        on: StopMask::NONE
                    },
                    None
                )
                .unwrap(),
                StopReason::Quiescent { .. }
            ),
            "unarmed: the assertion is NOT surfaced (would be a missed bug)"
        );
        g.branch(b, &trigger_env).unwrap();
        assert!(
            matches!(
                g.run(
                    &StopConditions {
                        deadline: None,
                        on: StopMask::ASSERTION
                    },
                    None
                )
                .unwrap(),
                StopReason::Assertion { .. }
            ),
            "armed: the assertion surfaces as a judged bug"
        );

        // The full campaign (which arms ASSERTION) finds + reports the planted
        // assertion, replays it N/N, and the nominal control stays clean.
        let mut machine = AssertMachine::new();
        let cfg = CampaignConfig {
            replay_n: 3,
            ..toy_cfg()
        };
        let report = run_campaign(&mut machine, &SpecEnvCodec, &cfg).expect("campaign runs");
        let found = report
            .found
            .as_ref()
            .expect("the planted SDK assertion is reported");
        assert!(
            matches!(found.stop, StopReason::Assertion { .. }),
            "the reported bug is the SDK assertion, got {:?}",
            found.stop
        );
        assert!(
            verify_campaign(&report, cfg.replay_n).is_empty(),
            "all campaign gates pass (found + N/N reproduced + nominal clean)"
        );
    }

    /// `is_planted_bug` (the public, stateless predicate `judge` delegates to
    /// via `TerminalOracle`) agrees with the oracle's classification: `Crash`/
    /// `Assertion` are the bug, `Quiescent`/`Deadline` are not.
    #[test]
    fn is_planted_bug_matches_the_oracle_classification() {
        let o = CrashOracle::new();
        assert!(o.is_planted_bug(&StopReason::Crash {
            vtime: Moment(1),
            info: vec![CRASH_KIND_SHUTDOWN],
        }));
        assert!(o.is_planted_bug(&StopReason::Assertion {
            vtime: Moment(1),
            id: 1,
            data: vec![],
        }));
        assert!(!o.is_planted_bug(&StopReason::Quiescent { vtime: Moment(1) }));
        assert!(!o.is_planted_bug(&StopReason::Deadline { vtime: Moment(1) }));
    }

    /// `pick` returns `None` on an empty search dimension (a mis-configured
    /// empty space), rather than panicking or indexing out of bounds — the
    /// documented fallback `mint_fault_env` relies on via `unwrap_or`.
    #[test]
    fn pick_returns_none_on_an_empty_slice() {
        let mut p = Prng::new(1);
        assert_eq!(pick(&[], &mut p), None);
    }

    /// `run_campaign` (task 96) populates `timing` with every phase it
    /// actually exercises over a full toy campaign (base seal, every
    /// per-branch phase, the N/N replays, the nominal pass) and derives
    /// `branches_per_hour_x10` from `branches_explored`/`wall_secs` exactly
    /// per the documented 0-when-`wall_secs`-is-0 contract. `Boot` is absent
    /// — only `boxrun.rs`'s box path feeds that phase in.
    #[test]
    fn run_campaign_populates_timing_for_every_exercised_phase() {
        let mut m = crate::planted::ToyPlantedMachine::new(crate::planted::Trigger::toy());
        let cfg = toy_cfg();
        let report = run_campaign(&mut m, &SpecEnvCodec, &cfg).expect("campaign runs");
        for phase in [
            Phase::BaseSeal,
            Phase::Branch,
            Phase::Run,
            Phase::Hash,
            Phase::Harvest,
            Phase::Judge,
            Phase::Replay,
            Phase::Nominal,
        ] {
            assert!(
                report.timing.contains_key(&phase),
                "phase {phase:?} should have at least one recorded sample"
            );
        }
        assert!(
            !report.timing.contains_key(&Phase::Boot),
            "the portable toy path never boots a box guest"
        );
        assert_eq!(
            report.branches_per_hour_x10,
            report
                .branches_explored
                .saturating_mul(36_000)
                .checked_div(report.wall_secs)
                .unwrap_or(0)
        );
    }

    /// `verify_campaign` also flags a replay whose terminal `StopReason`
    /// differs from the finding's, even when the `state_hash` matches — a
    /// replay that reproduces the hash but stops for a different reason (e.g.
    /// `Quiescent` vs `Crash`) is still not a faithful reproduction.
    #[test]
    fn verify_flags_a_replay_with_a_mismatched_stop_reason() {
        let found = FoundBug {
            branch_index: 5,
            seed: 1,
            env: SpecEnvCodec.seeded(1),
            stop: StopReason::Crash {
                vtime: Moment(100),
                info: vec![CRASH_KIND_SHUTDOWN],
            },
            hash: [0xAB; 32],
            bug: TerminalOracle::new()
                .judge(&crash_trace(CRASH_KIND_SHUTDOWN))
                .unwrap(),
        };
        let mut replays: Vec<RunRow> = (0..3)
            .map(|_| RunRow {
                stop: found.stop.clone(),
                hash: found.hash,
            })
            .collect();
        // Same hash, but a different terminal StopReason.
        replays[1].stop = StopReason::Quiescent { vtime: Moment(100) };
        let report = CampaignReport {
            base_vtime: 10,
            snapshot_attempts: 1,
            base_hash: [0; 32],
            branches_explored: 6,
            found: Some(found),
            replays,
            nominal: NominalRow {
                stop: StopReason::Quiescent { vtime: Moment(50) },
                hash: [1; 32],
                is_bug: false,
            },
            timing: BTreeMap::new(),
            wall_secs: 0,
            branches_per_hour_x10: 0,
        };
        let failures = verify_campaign(&report, 3);
        assert!(
            failures.iter().any(|f| f.contains("replay 1")
                && f.contains("stop")
                && f.contains("NOT reproducible")),
            "a stop-reason mismatch (same hash) must fail reproduction, got {failures:?}"
        );
    }

    /// [`render_campaign_table`] pins the exact rendered shape for a found
    /// bug, its replay verification, and a clean nominal control — the
    /// artifact the box gate records verbatim in IMPLEMENTATION.md. Left with
    /// an empty `timing` (task 96): the timing section is conditioned on
    /// `!timing.is_empty()`, so this report — built before that field
    /// existed — renders byte-for-byte as it always has; the timing-section
    /// shape itself is pinned separately below.
    #[test]
    fn render_campaign_table_pins_the_found_bug_format() {
        let found = FoundBug {
            branch_index: 3,
            seed: 0xAA,
            env: SpecEnvCodec.seeded(1),
            stop: StopReason::Crash {
                vtime: Moment(200),
                info: vec![CRASH_KIND_SHUTDOWN, 0x60],
            },
            hash: [0x11; 32],
            bug: TerminalOracle::new()
                .judge(&crash_trace(CRASH_KIND_SHUTDOWN))
                .unwrap(),
        };
        let replays: Vec<RunRow> = (0..2)
            .map(|_| RunRow {
                stop: found.stop.clone(),
                hash: found.hash,
            })
            .collect();
        let fingerprint = found.bug.fingerprint;
        let report = CampaignReport {
            base_vtime: 42,
            snapshot_attempts: 1,
            base_hash: [0; 32],
            branches_explored: 4,
            found: Some(found),
            replays,
            nominal: NominalRow {
                stop: StopReason::Quiescent { vtime: Moment(50) },
                hash: [1; 32],
                is_bug: false,
            },
            timing: BTreeMap::new(),
            wall_secs: 0,
            branches_per_hour_x10: 0,
        };
        let out = render_campaign_table(&report, 2);
        let expected = format!(
            "base snapshot: sealed at V-time 42 (1 attempt), capture state_hash {}\n\
             planted bug found at branch 3 (seed 0x00000000000000aa) after exploring 4 branches\n\
             \x20 finding stop Crash@200[2B], state_hash {}\n\
             \x20 fingerprint {}\n\
             \x20 replay verification: 2/2 identical (crash reproduced bit-for-bit)\n\
             nominal control (seed only, no faults): Quiescent@50 — no bug (adversity-gated, as required)\n",
            hex32(&[0u8; 32]),
            hex32(&[0x11u8; 32]),
            hex32(&fingerprint),
        );
        assert_eq!(out, expected);
    }

    /// The no-find and nominal-crashed-as-bug branches of the render — the
    /// two cases the found-bug test above cannot exercise. Updated (task 96)
    /// to also carry non-empty `timing`, pinning the new timing section's
    /// exact shape (the found-bug test above deliberately leaves `timing`
    /// empty and stays byte-for-byte unchanged instead).
    #[test]
    fn render_campaign_table_pins_the_no_find_and_nominal_bug_format() {
        let mut timing = BTreeMap::new();
        timing.insert(
            Phase::Branch,
            PhaseStats {
                count: 10,
                total_us: 52_100_000,
                p50_us: 5_210_000,
                p90_us: 6_000_000,
                max_us: 7_000_000,
            },
        );
        timing.insert(Phase::Hash, PhaseStats::single(2_900_000));
        let report = CampaignReport {
            base_vtime: 7,
            snapshot_attempts: 2,
            base_hash: [0xCC; 32],
            branches_explored: 10,
            found: None,
            replays: Vec::new(),
            nominal: NominalRow {
                stop: StopReason::Crash {
                    vtime: Moment(9),
                    info: vec![CRASH_KIND_SHUTDOWN, 0x60],
                },
                hash: [2; 32],
                is_bug: true,
            },
            timing,
            wall_secs: 90,
            branches_per_hour_x10: 4000,
        };
        let out = render_campaign_table(&report, 5);
        let expected = format!(
            "base snapshot: sealed at V-time 7 (2 attempts), capture state_hash {}\n\
             NO BUG FOUND in 10 branches\n\
             nominal control (seed only, no faults): Crash@9[2B] — REPORTED A BUG (trigger not adversity-gated!)\n\
             timing: wall 90s, 400.0 branches/hour\n\
             phase        count   total_s   p50_ms   p90_ms   max_ms\n\
             branch          10        52     5210     6000     7000\n\
             hash             1         2     2900     2900     2900\n",
            hex32(&[0xCCu8; 32]),
        );
        assert_eq!(out, expected);
    }

    /// A `Machine` whose `snapshot()` refuses `NotQuiescent` a fixed number of
    /// times before succeeding — an abstract (backend-independent) double for
    /// [`run_campaign`]'s base-seal retry loop (mirrors
    /// `ControlServer::snapshot`'s real NotQuiescent semantics, which live at
    /// the vmm-core layer this crate does not own).
    struct RetryingMachine {
        refusals_left: usize,
        /// If true, a retry `run` reports `Quiescent` instead of `Deadline` —
        /// modeling a guest that halts before a sealable boundary is found.
        halts_before_boundary: bool,
        vtime: u64,
    }

    impl Machine for RetryingMachine {
        fn branch(
            &mut self,
            _snap: explorer::SnapId,
            _env: &Reproducer,
        ) -> Result<(), MachineError> {
            Ok(())
        }
        fn replay(&mut self, _snap: explorer::SnapId) -> Result<(), MachineError> {
            Ok(())
        }
        fn run(
            &mut self,
            until: &StopConditions,
            _resolve: Option<&explorer::Answer>,
        ) -> Result<StopReason, MachineError> {
            match until.deadline {
                Some(d) => {
                    self.vtime = d.0;
                    if self.halts_before_boundary {
                        Ok(StopReason::Quiescent {
                            vtime: Moment(self.vtime),
                        })
                    } else {
                        Ok(StopReason::Deadline {
                            vtime: Moment(self.vtime),
                        })
                    }
                }
                None => {
                    self.vtime += 1;
                    Ok(StopReason::Quiescent {
                        vtime: Moment(self.vtime),
                    })
                }
            }
        }
        fn snapshot(&mut self) -> Result<(explorer::SnapId, EvidenceCut), MachineError> {
            if self.refusals_left > 0 {
                self.refusals_left -= 1;
                Err(MachineError::NotQuiescent)
            } else {
                Ok((explorer::SnapId(1), EvidenceCut::default()))
            }
        }
        fn drop_snap(&mut self, _snap: explorer::SnapId) -> Result<(), MachineError> {
            Ok(())
        }
        fn hash(&mut self) -> Result<[u8; 32], MachineError> {
            Ok([0x42; 32])
        }
        fn coverage(&self) -> &[u8] {
            &[]
        }
        fn recorded_env(&self) -> Result<Reproducer, MachineError> {
            Ok(SpecEnvCodec.seeded(0))
        }
    }

    fn retry_cfg(snapshot_retry_step: u64, snapshot_max_attempts: usize) -> CampaignConfig {
        CampaignConfig {
            max_branches: 0, // no search — this test is about the base seal only
            snapshot_retry_step,
            snapshot_max_attempts,
            ..CampaignConfig::toy()
        }
    }

    /// A base seal that refuses `NotQuiescent` a few times, then succeeds:
    /// `run_campaign` retries at `snapshot_retry_step` increments and seals at
    /// the V-time the last successful retry reached.
    #[test]
    fn run_campaign_retries_past_non_quiescent_points_then_seals() {
        let mut machine = RetryingMachine {
            refusals_left: 2,
            halts_before_boundary: false,
            vtime: 0,
        };
        let cfg = retry_cfg(100, 5);
        let report = run_campaign(&mut machine, &SpecEnvCodec, &cfg).expect("seals after retries");
        assert_eq!(
            report.snapshot_attempts, 3,
            "2 refusals + 1 success = 3 attempts"
        );
        assert_eq!(
            report.base_vtime, 200,
            "sealed at 2 retries × 100 V-time each"
        );
    }

    /// Exceeding `snapshot_max_attempts` without ever sealing is a loud
    /// `NotQuiescent` error, never a silent partial result.
    #[test]
    fn run_campaign_gives_up_after_exceeding_max_snapshot_attempts() {
        let mut machine = RetryingMachine {
            refusals_left: 10,
            halts_before_boundary: false,
            vtime: 0,
        };
        let cfg = retry_cfg(100, 3);
        let err = run_campaign(&mut machine, &SpecEnvCodec, &cfg)
            .expect_err("must give up loudly, not loop forever or seal on garbage");
        assert!(matches!(err, MachineError::NotQuiescent));
    }

    /// If the guest halts before a sealable boundary is found mid-retry (a
    /// non-`Deadline` stop), the seal fails loudly rather than proceeding on a
    /// point it never actually verified was quiescent.
    #[test]
    fn run_campaign_fails_if_the_guest_halts_before_a_sealable_boundary() {
        let mut machine = RetryingMachine {
            refusals_left: 1,
            halts_before_boundary: true,
            vtime: 0,
        };
        let cfg = retry_cfg(100, 5);
        let err = run_campaign(&mut machine, &SpecEnvCodec, &cfg)
            .expect_err("a non-Deadline stop mid-retry must not be treated as sealable");
        assert!(matches!(err, MachineError::NotQuiescent));
    }
}
