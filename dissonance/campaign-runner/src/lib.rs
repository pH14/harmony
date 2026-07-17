// SPDX-License-Identifier: AGPL-3.0-or-later
//! # campaign-runner — close the loop (task 58)
//!
//! `campaign-runner` is the composition root where dissonance first drives consonance
//! for real: the explorer's socket-backed [`SocketMachine`] (the R2 adapter)
//! against vmm-core's [`ControlServer`], over a unix socketpair. The library
//! holds the pieces the demo binary and the loopback gates share:
//!
//! - [`run_session`] — one server session: the server loop on the calling
//!   thread (a [`Vmm`](vmm_core::vmm::Vmm) is not `Send` — its work source is a
//!   thread-affine counter on the box), the client on a spawned thread.
//! - [`run_sweep`] — the outer-loop demo protocol: probe V-time, snapshot once
//!   (retrying past non-snapshottable boundaries), then per seed
//!   `branch → run → hash` (repeated, for the bit-identical check), then a
//!   `replay` of the base and a final `drop` — producing a [`SweepReport`].
//! - [`render_table`] / [`verify`] — the printed run table and the task-58 box
//!   gates (per-seed reproducibility, cross-seed divergence, replay-equals-
//!   capture) as a pure check over the report.
//! - [`mock`] — the scripted `MockBackend` server composition (with
//!   [`TickingWork`](mock::TickingWork), a monotone portable work source) the
//!   portable gates and the demo's `mock` mode drive.
//!
//! Everything here is workload-blind: the same sweep runs against the mock
//! guest and the box's Postgres image; only the composition root (the binary)
//! knows which.
//!
//! ## The first campaign (task 60)
//!
//! [`campaign`] extends this bin into the milestone: [`run_campaign`](campaign::run_campaign)
//! seals a base, searches seed-driven **host-fault schedules** ([`mint_fault_env`](campaign::mint_fault_env),
//! riding the branch env task-59's server enforces), judges each terminal with the workload-aware
//! [`CrashOracle`](campaign::CrashOracle), and on the first crash emits the [`Bug`](explorer::Bug)
//! and replays it N/N ([`verify_campaign`](campaign::verify_campaign)). The identical loop drives the
//! box's real Postgres-campaign guest and, portably, the [`planted`] module's
//! [`ToyPlantedMachine`](planted::ToyPlantedMachine) — a controllable guest with a planted,
//! fault-triggerable bug.

use std::os::unix::net::UnixStream;

use environment::EnvSpec;
use explorer::adapter::SocketMachine;
use explorer::{Machine, MachineError, Moment, StopConditions, StopMask, StopReason};
use vmm_backend::Backend;
use vmm_core::control::{ControlServer, ServeError};

pub mod benchcampaign;
pub mod campaign;
pub mod gamecampaign;
pub mod materialize;
pub mod mock;
pub mod planted;
pub mod record;
pub mod stopwatch;

/// Run one control session: `client` on a spawned thread against `server` on
/// the calling thread, over a fresh socketpair. Returns the server-loop result
/// and the client's return value. A client panic (a failed test assertion) is
/// resumed on this thread after the server loop ends, so it surfaces intact.
pub fn run_session<B, T>(
    server: &mut ControlServer<B>,
    client: impl FnOnce(UnixStream) -> T + Send + 'static,
) -> (Result<(), ServeError>, T)
where
    B: Backend<A = vmm_backend::X86>,
    T: Send + 'static,
{
    // A socketpair failure means the harness itself cannot run — loud is right.
    let (client_end, server_end) = UnixStream::pair().expect("socketpair for the control session");
    let client_thread = std::thread::spawn(move || client(client_end));
    let served = server.serve(server_end);
    match client_thread.join() {
        Ok(value) => (served, value),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// Configuration for one [`run_sweep`]: which seeds to branch, how many
/// repeated runs per seed (≥ 2 proves bit-identical reproduction), how far
/// past the snapshot each branch runs, and the snapshot-retry policy.
#[derive(Clone, Debug)]
pub struct SweepConfig {
    /// The entropy seeds to branch from the base snapshot.
    pub seeds: Vec<u64>,
    /// Runs per seed (each `branch → run → hash` from the same base).
    pub runs_per_seed: usize,
    /// `Some(d)`: each run stops at `snapshot V-time + d` (the box mode — the
    /// workload's natural terminal is far away). `None`: run to the guest's
    /// terminal stop (the mock mode).
    pub deadline_delta: Option<u64>,
    /// Snapshot retry: on a `NotQuiescent` refusal, advance the guest by this
    /// much V-time and try again …
    pub snapshot_retry_step: u64,
    /// … at most this many times before giving up loudly.
    pub snapshot_max_attempts: usize,
}

impl Default for SweepConfig {
    fn default() -> Self {
        SweepConfig {
            seeds: Vec::new(),
            runs_per_seed: 2,
            deadline_delta: None,
            snapshot_retry_step: 10_000,
            snapshot_max_attempts: 10_000,
        }
    }
}

/// One `branch → run → hash` observation.
#[derive(Clone, Debug)]
pub struct RunRow {
    /// The terminal [`StopReason`] — the **real value**, not a rendered string:
    /// [`verify`] compares it directly (`StopReason` is `Eq`), so two runs of a
    /// seed that reproduce the hash but stop with different *detail* (e.g. two
    /// crashes with different info bytes) are still caught. Rendered for display
    /// only, via [`fmt_stop`].
    pub stop: StopReason,
    /// The terminal `state_hash`.
    pub hash: [u8; 32],
}

/// All runs of one seed.
#[derive(Clone, Debug)]
pub struct SeedRow {
    /// The branch seed.
    pub seed: u64,
    /// One entry per run (all must be identical for the gate to pass).
    pub runs: Vec<RunRow>,
}

/// What one [`run_sweep`] observed.
#[derive(Clone, Debug)]
pub struct SweepReport {
    /// The V-time at which the base snapshot was sealed.
    pub snapshot_vtime: u64,
    /// How many snapshot attempts the seal took (1 = first try).
    pub snapshot_attempts: usize,
    /// `state_hash` right after sealing the base (the capture hash).
    pub base_hash: [u8; 32],
    /// Per-seed observations.
    pub rows: Vec<SeedRow>,
    /// `state_hash` after `replay(base)` — must equal `base_hash`.
    pub replay_hash: [u8; 32],
}

/// Compact, stable rendering of a stop for the run table.
pub fn fmt_stop(stop: &StopReason) -> String {
    match stop {
        StopReason::Deadline { vtime } => format!("Deadline@{}", vtime.0),
        StopReason::Quiescent { vtime } => format!("Quiescent@{}", vtime.0),
        StopReason::Crash { vtime, info } => format!("Crash@{}[{}B]", vtime.0, info.len()),
        StopReason::Decision { vtime, id, .. } => format!("Decision#{id}@{}", vtime.0),
        StopReason::SnapshotPoint { vtime } => format!("SnapshotPoint@{}", vtime.0),
        StopReason::Assertion { vtime, id, .. } => format!("Assertion#{id}@{}", vtime.0),
    }
}

/// Probe the machine's current V-time without advancing it: a `run` whose
/// deadline is already met (`0`) stops immediately — the server checks the
/// deadline before entering the guest — and stamps the effective V-time.
pub fn probe_vtime<M: Machine>(machine: &mut M) -> Result<u64, MachineError> {
    let stop = machine.run(
        &StopConditions {
            deadline: Some(Moment(0)),
            on: StopMask::NONE,
        },
        None,
    )?;
    Ok(stop.vtime().0)
}

/// The outer-loop demo protocol (see the module doc). Drives any [`Machine`];
/// in this crate that is always the socket adapter, so every verb crosses the
/// wire. Fails loudly on any [`MachineError`] other than the snapshot-retry
/// `NotQuiescent`.
pub fn run_sweep<M: Machine>(
    machine: &mut M,
    codec: &dyn explorer::EnvCodec,
    cfg: &SweepConfig,
) -> Result<SweepReport, MachineError> {
    // 1. Where are we? (Also the anchor for every deadline below.)
    let mut vt = probe_vtime(machine)?;

    // 2. Seal the base snapshot, nudging past non-snapshottable boundaries:
    //    an RNG mid-exit completion or a non-V-time-synchronized point answers
    //    NotQuiescent; running a little further lands on the next intercept
    //    boundary, which is (usually) sealable.
    let mut attempts = 0usize;
    let base = loop {
        attempts += 1;
        match machine.snapshot() {
            Ok((snap, _cut)) => break snap,
            Err(MachineError::NotQuiescent) => {
                if attempts >= cfg.snapshot_max_attempts {
                    return Err(MachineError::NotQuiescent);
                }
                let stop = machine.run(
                    &StopConditions {
                        // Saturating: a deadline that would overflow the axis
                        // just means "run to the end", never a wrapped-small
                        // deadline that would stop immediately.
                        deadline: Some(Moment(vt.saturating_add(cfg.snapshot_retry_step))),
                        on: StopMask::NONE,
                    },
                    None,
                )?;
                if !matches!(stop, StopReason::Deadline { .. }) {
                    // The guest ended before a sealable boundary was found.
                    return Err(MachineError::NotQuiescent);
                }
                vt = stop.vtime().0;
            }
            Err(e) => return Err(e),
        }
    };
    let snapshot_vtime = vt;
    let base_hash = machine.hash()?;

    // 3. The search loop: per seed, branch → run → hash, repeated.
    let until = StopConditions {
        // Saturating (as above): an overflowing deadline means "to the end".
        deadline: cfg
            .deadline_delta
            .map(|d| Moment(snapshot_vtime.saturating_add(d))),
        on: StopMask::NONE,
    };
    let mut rows = Vec::with_capacity(cfg.seeds.len());
    for &seed in &cfg.seeds {
        let mut runs = Vec::with_capacity(cfg.runs_per_seed);
        for _ in 0..cfg.runs_per_seed {
            machine.branch(base, &codec.seeded(seed))?;
            let stop = machine.run(&until, None)?;
            let hash = machine.hash()?;
            runs.push(RunRow { stop, hash });
        }
        rows.push(SeedRow { seed, runs });
    }

    // 4. Replay the base verbatim: the restored state must hash exactly like
    //    the capture. Then release the handle (pool GC — exercises `drop`).
    machine.replay(base)?;
    let replay_hash = machine.hash()?;
    machine.drop_snap(base)?;

    Ok(SweepReport {
        snapshot_vtime,
        snapshot_attempts: attempts,
        base_hash,
        rows,
        replay_hash,
    })
}

/// Lowercase hex of a digest.
pub fn hex(digest: &[u8; 32]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Render the run table (the artifact the box gate records in
/// IMPLEMENTATION.md).
pub fn render_table(report: &SweepReport) -> String {
    let mut out = String::new();
    let push = |out: &mut String, line: String| {
        out.push_str(&line);
        out.push('\n');
    };
    push(
        &mut out,
        format!(
            "base snapshot: sealed at V-time {} ({} attempt{}), capture state_hash {}",
            report.snapshot_vtime,
            report.snapshot_attempts,
            if report.snapshot_attempts == 1 {
                ""
            } else {
                "s"
            },
            hex(&report.base_hash)
        ),
    );
    push(
        &mut out,
        format!(
            "{:<20} {:>4}  {:<24} {}",
            "seed", "run", "stop", "state_hash"
        ),
    );
    for row in &report.rows {
        for (i, run) in row.runs.iter().enumerate() {
            push(
                &mut out,
                format!(
                    "{:#018x}   {:>4}  {:<24} {}",
                    row.seed,
                    i,
                    fmt_stop(&run.stop),
                    hex(&run.hash)
                ),
            );
        }
    }
    push(
        &mut out,
        format!(
            "replay(base): state_hash {} ({} capture)",
            hex(&report.replay_hash),
            if report.replay_hash == report.base_hash {
                "=="
            } else {
                "!="
            }
        ),
    );
    out
}

/// The task-58 gates over a [`SweepReport`]:
///
/// 1. **Reproducibility** — every seed's repeated runs are bit-identical, in
///    **both** the terminal `state_hash` and the [`StopReason`] (a run that
///    reproduces the hash but stops for a different reason is still a
///    determinism failure — e.g. `Deadline` vs `Quiescent` at the same point).
///    A seed with **fewer than two runs cannot demonstrate reproducibility at
///    all**, so it is a failure — `verify` is a sound oracle regardless of how
///    the sweep was configured (the milestone runs each seed twice).
/// 2. **Divergence** — at least `min_distinct` distinct terminal hashes across
///    seeds (the box gate asks ≥ 2).
/// 3. **Replay** — `replay(base)` hashes identically to the original capture.
///
/// Returns every violated gate (empty = all pass), so a caller can assert or
/// report them all at once.
pub fn verify(report: &SweepReport, min_distinct: usize) -> Vec<String> {
    let mut failures = Vec::new();
    for row in &report.rows {
        let Some(first) = row.runs.first() else {
            failures.push(format!("seed {:#018x}: no runs recorded", row.seed));
            continue;
        };
        if row.runs.len() < 2 {
            failures.push(format!(
                "seed {:#018x}: only {} run — reproducibility needs at least 2 runs to compare",
                row.seed,
                row.runs.len()
            ));
        }
        for (i, run) in row.runs.iter().enumerate() {
            if run.hash != first.hash {
                failures.push(format!(
                    "seed {:#018x}: run {i} hash {} != run 0 hash {} — NOT reproducible",
                    row.seed,
                    hex(&run.hash),
                    hex(&first.hash)
                ));
            }
            if run.stop != first.stop {
                failures.push(format!(
                    "seed {:#018x}: run {i} stop {:?} != run 0 stop {:?} — NOT reproducible",
                    row.seed, run.stop, first.stop
                ));
            }
        }
    }
    let mut distinct: Vec<[u8; 32]> = report
        .rows
        .iter()
        .filter_map(|r| r.runs.first().map(|run| run.hash))
        .collect();
    distinct.sort_unstable();
    distinct.dedup();
    if distinct.len() < min_distinct {
        failures.push(format!(
            "only {} distinct hash(es) across {} seeds (need >= {min_distinct}) — futures did \
             not diverge",
            distinct.len(),
            report.rows.len()
        ));
    }
    if report.replay_hash != report.base_hash {
        failures.push(format!(
            "replay(base) hash {} != capture hash {} — replay is not verbatim",
            hex(&report.replay_hash),
            hex(&report.base_hash)
        ));
    }
    failures
}

/// Connect the socket adapter over `stream`, running under `initial` (the env
/// the server's live VM was booted with — its seed/policy), and hand it to the
/// sweep. The standard client body for [`run_session`].
pub fn sweep_client(
    stream: UnixStream,
    initial: EnvSpec,
    cfg: SweepConfig,
) -> Result<SweepReport, MachineError> {
    let mut machine = SocketMachine::connect(stream, initial)?;
    run_sweep(&mut machine, &explorer::SpecEnvCodec, &cfg)
}

/// Connect the socket adapter over `stream` and run the task-68 chain
/// protocol ([`materialize::run_materialize`]) against the production codec.
/// The standard client body for the materialization gates.
pub fn materialize_client(
    stream: UnixStream,
    initial: EnvSpec,
    cfg: materialize::MaterializeConfig,
) -> Result<materialize::MaterializeReport, MachineError> {
    let mut machine = SocketMachine::connect(stream, initial)?;
    materialize::run_materialize(&mut machine, &explorer::SpecEnvCodec, &cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use explorer::EnvCodec;

    /// Build a report with the given per-seed run counts, all runs of a seed
    /// identical (so only the run-count check can fail). Distinct seeds get
    /// distinct hashes (divergence satisfied).
    fn report(run_counts: &[usize]) -> SweepReport {
        let rows = run_counts
            .iter()
            .enumerate()
            .map(|(s, &n)| SeedRow {
                seed: s as u64,
                runs: (0..n)
                    .map(|_| RunRow {
                        stop: StopReason::Quiescent { vtime: Moment(100) },
                        hash: [s as u8; 32],
                    })
                    .collect(),
            })
            .collect();
        SweepReport {
            snapshot_vtime: 10,
            snapshot_attempts: 1,
            base_hash: [0xAA; 32],
            rows,
            replay_hash: [0xAA; 32],
        }
    }

    #[test]
    fn verify_rejects_a_single_run_row_as_not_reproducible() {
        // Two seeds, but the first has only ONE run — it cannot demonstrate
        // reproducibility, so verify must flag it even though every run "matches
        // itself" and the two seeds diverge.
        let failures = verify(&report(&[1, 2]), 2);
        assert!(
            failures.iter().any(|f| f.contains("only 1 run")),
            "a single-run row is a reproducibility failure, got {failures:?}"
        );
    }

    #[test]
    fn verify_passes_a_well_formed_two_run_report() {
        assert_eq!(
            verify(&report(&[2, 2]), 2),
            Vec::<String>::new(),
            "two seeds, two identical runs each, two distinct futures, replay == capture"
        );
    }

    #[test]
    fn verify_flags_an_empty_row_and_a_replay_mismatch() {
        let mut r = report(&[2, 0]);
        r.replay_hash = [0xBB; 32]; // != base_hash
        let failures = verify(&r, 2);
        assert!(failures.iter().any(|f| f.contains("no runs recorded")));
        assert!(failures.iter().any(|f| f.contains("replay")));
    }

    /// [`fmt_stop`] renders every [`StopReason`] variant in the compact form
    /// the run tables print — pin the exact shape for each, since a rendering
    /// regression here would silently corrupt every box-gate table.
    #[test]
    fn fmt_stop_renders_every_stop_reason_variant() {
        assert_eq!(
            fmt_stop(&StopReason::Deadline { vtime: Moment(7) }),
            "Deadline@7"
        );
        assert_eq!(
            fmt_stop(&StopReason::Quiescent { vtime: Moment(7) }),
            "Quiescent@7"
        );
        assert_eq!(
            fmt_stop(&StopReason::Crash {
                vtime: Moment(7),
                info: vec![1, 2, 3]
            }),
            "Crash@7[3B]"
        );
        assert_eq!(
            fmt_stop(&StopReason::Decision {
                vtime: Moment(7),
                id: 9,
                ctx: vec![]
            }),
            "Decision#9@7"
        );
        assert_eq!(
            fmt_stop(&StopReason::SnapshotPoint { vtime: Moment(7) }),
            "SnapshotPoint@7"
        );
        assert_eq!(
            fmt_stop(&StopReason::Assertion {
                vtime: Moment(7),
                id: 4,
                data: vec![]
            }),
            "Assertion#4@7"
        );
    }

    /// [`hex`] lowercases every byte independently — pin against a
    /// hand-constructed expected string (not built by calling `hex` itself).
    #[test]
    fn hex_lowercases_every_byte_independently() {
        let mut digest = [0u8; 32];
        digest[0] = 0xAB;
        digest[31] = 0xCD;
        let expected = format!("ab{}cd", "00".repeat(30));
        assert_eq!(hex(&digest), expected);
    }

    /// [`render_table`] pins the exact run-table shape — the artifact the
    /// box gate records verbatim in IMPLEMENTATION.md.
    #[test]
    fn render_table_pins_the_run_table_format() {
        let rep = SweepReport {
            snapshot_vtime: 10,
            snapshot_attempts: 1,
            base_hash: [0xAA; 32],
            rows: vec![SeedRow {
                seed: 0x1111,
                runs: vec![
                    RunRow {
                        stop: StopReason::Quiescent { vtime: Moment(50) },
                        hash: [0x22; 32],
                    },
                    RunRow {
                        stop: StopReason::Quiescent { vtime: Moment(50) },
                        hash: [0x22; 32],
                    },
                ],
            }],
            replay_hash: [0xAA; 32],
        };
        let expected = "base snapshot: sealed at V-time 10 (1 attempt), capture state_hash aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
seed                  run  stop                     state_hash\n\
0x0000000000001111      0  Quiescent@50             2222222222222222222222222222222222222222222222222222222222222222\n\
0x0000000000001111      1  Quiescent@50             2222222222222222222222222222222222222222222222222222222222222222\n\
replay(base): state_hash aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa (== capture)\n";
        assert_eq!(render_table(&rep), expected);
    }

    /// A `Machine` whose `snapshot()` refuses `NotQuiescent` a fixed number
    /// of times before succeeding, and whose first `run()` may fail outright
    /// — an abstract double for [`run_sweep`]'s base-seal retry loop and
    /// [`probe_vtime`]'s error propagation (the real `NotQuiescent` semantics
    /// live at the vmm-core layer this crate does not own).
    struct RetryingMachine {
        first_run_fails: bool,
        refusals_left: usize,
        halts_before_boundary: bool,
        vtime: u64,
    }

    impl Machine for RetryingMachine {
        fn branch(
            &mut self,
            _snap: explorer::SnapId,
            _env: &explorer::Reproducer,
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
            if self.first_run_fails {
                self.first_run_fails = false;
                return Err(MachineError::Transport(
                    "simulated transport failure".into(),
                ));
            }
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
        fn snapshot(&mut self) -> Result<(explorer::SnapId, explorer::EvidenceCut), MachineError> {
            if self.refusals_left > 0 {
                self.refusals_left -= 1;
                Err(MachineError::NotQuiescent)
            } else {
                // The cut is not load-bearing for this double (task 127): a
                // zero stamp on its constant axis.
                Ok((explorer::SnapId(1), explorer::EvidenceCut::default()))
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
        fn recorded_env(&self) -> Result<explorer::Reproducer, MachineError> {
            Ok(explorer::SpecEnvCodec.seeded(0))
        }
    }

    fn retry_cfg(snapshot_retry_step: u64, snapshot_max_attempts: usize) -> SweepConfig {
        SweepConfig {
            seeds: Vec::new(), // no per-seed sweep — this test is about the base seal only
            snapshot_retry_step,
            snapshot_max_attempts,
            ..SweepConfig::default()
        }
    }

    /// [`probe_vtime`]'s error propagation: a transport failure on the very
    /// first `run` call must surface, not be swallowed.
    #[test]
    fn probe_vtime_propagates_a_machine_error() {
        let mut m = RetryingMachine {
            first_run_fails: true,
            refusals_left: 0,
            halts_before_boundary: false,
            vtime: 0,
        };
        let err = probe_vtime(&mut m).expect_err("a transport failure must propagate");
        assert!(matches!(err, MachineError::Transport(_)));
    }

    /// A base seal that refuses `NotQuiescent` a few times, then succeeds:
    /// `run_sweep` retries at `snapshot_retry_step` increments and seals at
    /// the V-time the last successful retry reached.
    #[test]
    fn run_sweep_retries_past_non_quiescent_points_then_seals() {
        let mut m = RetryingMachine {
            first_run_fails: false,
            refusals_left: 2,
            halts_before_boundary: false,
            vtime: 0,
        };
        let cfg = retry_cfg(100, 5);
        let report = run_sweep(&mut m, &explorer::SpecEnvCodec, &cfg).expect("seals after retries");
        assert_eq!(
            report.snapshot_attempts, 3,
            "2 refusals + 1 success = 3 attempts"
        );
        assert_eq!(
            report.snapshot_vtime, 200,
            "sealed at 2 retries x 100 V-time each"
        );
        assert_eq!(
            report.replay_hash, report.base_hash,
            "replay(base) reproduces the capture hash"
        );
    }

    /// Exceeding `snapshot_max_attempts` without ever sealing is a loud
    /// `NotQuiescent` error, never a silent partial result.
    #[test]
    fn run_sweep_gives_up_after_exceeding_max_snapshot_attempts() {
        let mut m = RetryingMachine {
            first_run_fails: false,
            refusals_left: 10,
            halts_before_boundary: false,
            vtime: 0,
        };
        let cfg = retry_cfg(100, 3);
        let err = run_sweep(&mut m, &explorer::SpecEnvCodec, &cfg)
            .expect_err("must give up loudly, not loop forever or seal on garbage");
        assert!(matches!(err, MachineError::NotQuiescent));
    }

    /// If the guest halts before a sealable boundary is found mid-retry (a
    /// non-`Deadline` stop), the seal fails loudly rather than proceeding on
    /// a point it never actually verified was quiescent.
    #[test]
    fn run_sweep_fails_if_the_guest_halts_before_a_sealable_boundary() {
        let mut m = RetryingMachine {
            first_run_fails: false,
            refusals_left: 1,
            halts_before_boundary: true,
            vtime: 0,
        };
        let cfg = retry_cfg(100, 5);
        let err = run_sweep(&mut m, &explorer::SpecEnvCodec, &cfg)
            .expect_err("a non-Deadline stop mid-retry must not be treated as sealable");
        assert!(matches!(err, MachineError::NotQuiescent));
    }
}
