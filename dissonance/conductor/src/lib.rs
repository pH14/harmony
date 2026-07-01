// SPDX-License-Identifier: AGPL-3.0-or-later
//! # conductor — close the loop (task 58)
//!
//! `conductor` is the composition root where dissonance first drives consonance
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

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use environment::EnvSpec;
use explorer::adapter::SocketMachine;
use explorer::{Machine, MachineError, StopConditions, StopMask, StopReason, VTime};
use vmm_backend::Backend;
use vmm_core::control::{ControlServer, ServeError};

pub mod mock;

/// Run one control session: `client` on a spawned thread against `server` on
/// the calling thread, over a fresh socketpair. Returns the server-loop result
/// and the client's return value. A client panic (a failed test assertion) is
/// resumed on this thread after the server loop ends, so it surfaces intact.
pub fn run_session<B, T>(
    server: &mut ControlServer<B>,
    client: impl FnOnce(UnixStream) -> T + Send + 'static,
) -> (Result<(), ServeError>, T)
where
    B: Backend,
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
    /// A compact stop description (`Deadline@…` / `Quiescent@…` / `Crash@…`).
    pub stop: String,
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
            deadline: Some(VTime(0)),
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
            Ok(snap) => break snap,
            Err(MachineError::NotQuiescent) => {
                if attempts >= cfg.snapshot_max_attempts {
                    return Err(MachineError::NotQuiescent);
                }
                let stop = machine.run(
                    &StopConditions {
                        deadline: Some(VTime(vt + cfg.snapshot_retry_step)),
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

    // 3. The multiverse: per seed, branch → run → hash, repeated.
    let until = StopConditions {
        deadline: cfg.deadline_delta.map(|d| VTime(snapshot_vtime + d)),
        on: StopMask::NONE,
    };
    let mut rows = Vec::with_capacity(cfg.seeds.len());
    for &seed in &cfg.seeds {
        let mut runs = Vec::with_capacity(cfg.runs_per_seed);
        for _ in 0..cfg.runs_per_seed {
            machine.branch(base, &codec.seeded(seed))?;
            let stop = machine.run(&until, None)?;
            let hash = machine.hash()?;
            runs.push(RunRow {
                stop: fmt_stop(&stop),
                hash,
            });
        }
        rows.push(SeedRow { seed, runs });
    }

    // 4. Replay the base verbatim: the restored state must hash exactly like
    //    the capture. Then release the handle (corpus GC — exercises `drop`).
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
                    run.stop,
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
/// 1. **Reproducibility** — every seed's repeated runs are bit-identical.
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
        for (i, run) in row.runs.iter().enumerate() {
            if run.hash != first.hash {
                failures.push(format!(
                    "seed {:#018x}: run {i} hash {} != run 0 hash {} — NOT reproducible",
                    row.seed,
                    hex(&run.hash),
                    hex(&first.hash)
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

/// A raw-frame control-proto call over a stream — for driving wire-level cases
/// the typed adapter deliberately cannot express (`perturb`, non-`Whole` hash
/// scopes, a verb before `hello`). Panics on transport/framing failures (test
/// harness use).
pub fn raw_call<S: Read + Write>(
    stream: &mut S,
    seq: u32,
    req: &control_proto::Request,
) -> Result<control_proto::Reply, control_proto::ControlError> {
    let mut out = Vec::new();
    control_proto::encode_request(seq, req, &mut out).expect("encode request");
    stream.write_all(&out).expect("write request");
    stream.flush().expect("flush request");
    let mut inbuf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some((got_seq, reply, consumed)) =
            control_proto::decode_reply(&inbuf).expect("reply framing")
        {
            assert_eq!(got_seq, seq, "reply echoes the request seq");
            assert_eq!(consumed, inbuf.len(), "one reply per request");
            return reply;
        }
        let n = stream.read(&mut chunk).expect("read reply");
        assert_ne!(n, 0, "server closed mid-reply");
        inbuf.extend_from_slice(&chunk[..n]);
    }
}
