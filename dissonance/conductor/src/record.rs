// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **task-65 recording session**: drive `ControlServer::handle` in-process,
//! drain the guest console after each run, and assemble + persist a
//! [`RunTrace`] per run.
//!
//! Unlike [`run_sweep`](crate::run_sweep) (which drives the server over a socket
//! via the explorer's `SocketMachine`, the remote path), the recording session
//! calls [`ControlServer::handle`] **directly, in-process**. That is what lets
//! it reach `server.vmm().serial()` between verbs to drain the console — a
//! socket client cannot see the server-side capture. The socket path stays for
//! remote use; this is the recording path.
//!
//! The recorder is a **pure sink** (task 65 invariant): it only ever *writes* to
//! the [`TraceStore`] — no live-plane / `Tactic` code reads sensor or store
//! output mid-run (open-loop Modulation), and the search policy never learns the
//! store exists (Progression blindness). The retention knob gates journal bytes
//! only; it never changes which verbs the loop issues or the report it produces.
//!
//! ## The one stamp axis
//!
//! Console bytes are mapped onto the spine's [`Moment`] axis in exactly one
//! place — [`stamp`] — never a second time axis. In v1 a run's whole console is
//! drained under a single stop `Moment` (stop-granular; per-exit stamps wait on
//! the `telemetry::Observer` wiring, task 65 non-goal). The `Moment`-vs-`VTime`
//! unit choice (`Moment(vtime)` one-for-one, retired-branch V-time) is the unit
//! ruling escalated to the foreman per task 65 — nothing here bakes in more than
//! the one-for-one identity the spine already documents.

use std::collections::BTreeMap;

use environment::EnvSpec;
use explorer::{AdapterEnv, EnvCodec, Moment, RunTrace, SpecEnvCodec, StopReason, StreamId, VTime};
use runtrace::{Retain, RetentionPolicy, TraceId, TraceStore, retain_for};
use vmm_backend::Backend;
use vmm_core::control::{ControlServer, ServeError, server_caps};

use control_proto::{
    ControlError, Environment as WireEnv, Reply, Request, SnapId, StopConditions, StopMask,
    VTime as WireVTime,
};

/// The **single** V-time → [`Moment`] mapping (task 65 §4: "exactly one
/// documented `stamp()` function"). One-for-one, mirroring the spine's toy
/// machine and `control-proto`'s axis; the unit ruling is escalated to the
/// foreman.
pub fn stamp(vtime: VTime) -> Moment {
    Moment(vtime.0)
}

/// Configuration for one [`run_recording`] pass.
#[derive(Clone, Debug)]
pub struct RecordConfig {
    /// Seeds / runs-per-seed / deadline / snapshot-retry policy — reused from
    /// the sweep so the recording campaign is the sweep, plus recording.
    pub sweep: crate::SweepConfig,
    /// How much of each run to persist.
    pub retain: RetentionPolicy,
    /// The [`StreamId`] stamped on every scraped console record.
    pub stream: StreamId,
}

/// One recorded run — a compact row (no per-run megabyte console is retained in
/// memory; the console lives in the store). The gate checks are precomputed here
/// so [`verify_record`] is a pure cross-row check.
#[derive(Clone, Debug)]
pub struct RecordedRun {
    /// The branch seed.
    pub seed: u64,
    /// Which repeat of this seed (0-based).
    pub run: usize,
    /// The content address of the recorded trace.
    pub trace_id: TraceId,
    /// The run's terminal stop.
    pub stop: StopReason,
    /// How many console records the run produced.
    pub records_len: usize,
    /// The serialized journal's length in bytes.
    pub journal_len: usize,
    /// Whether the full journal was retained (vs. env-only).
    pub retained: bool,
    /// Whether the record stamps are monotone non-decreasing.
    pub stamps_monotone: bool,
    /// Whether this run's journal bytes equal this seed's run-0 journal bytes
    /// (per-seed byte-identical determinism).
    pub journal_matches_first_run: bool,
    /// The first bytes of the run's console, for the run-table display.
    pub console_head: Vec<u8>,
}

/// What one [`run_recording`] observed.
#[derive(Clone, Debug)]
pub struct RecordReport {
    /// The V-time the base snapshot was sealed at.
    pub snapshot_vtime: u64,
    /// One row per (seed, run), in campaign order.
    pub rows: Vec<RecordedRun>,
}

/// A recording-session failure. Keeps the control-plane's two categories apart:
/// a [`ServeError`] tore the session down; a [`ControlError`] is a wire reply we
/// did not expect on the happy path; a `runtrace` error is store I/O.
#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    /// A session-fatal server error.
    #[error("control session error: {0}")]
    Serve(#[from] ServeError),
    /// An unexpected control-plane reply error.
    #[error("unexpected control-plane error reply: {0:?}")]
    Control(ControlError),
    /// A store / codec error.
    #[error("trace store error: {0}")]
    Trace(#[from] runtrace::TraceError),
    /// A reply of the wrong shape for the verb sent.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The base snapshot could not be sealed within the retry budget.
    #[error("could not seal the base snapshot: {0}")]
    Snapshot(String),
}

/// Dispatch a verb, collapsing the happy path to a [`Reply`] and mapping both
/// error categories.
fn call<B: Backend>(server: &mut ControlServer<B>, req: &Request) -> Result<Reply, RecordError> {
    match server.handle(req)? {
        Ok(reply) => Ok(reply),
        Err(ce) => Err(RecordError::Control(ce)),
    }
}

/// Probe the current effective V-time without advancing the guest: a `run` whose
/// deadline (`0`) is already met stops immediately and stamps the V-time.
fn probe_vtime<B: Backend>(server: &mut ControlServer<B>) -> Result<u64, RecordError> {
    let stop = run(server, Some(0))?;
    Ok(stop.vtime().0)
}

/// Issue a `run` with an optional V-time deadline, returning the stop.
fn run<B: Backend>(
    server: &mut ControlServer<B>,
    deadline: Option<u64>,
) -> Result<StopReason, RecordError> {
    let req = Request::Run {
        until: StopConditions {
            deadline: deadline.map(WireVTime),
            on: StopMask::NONE,
        },
        resolve: None,
    };
    match call(server, &req)? {
        Reply::Stop(wire) => Ok(stop_from_wire(wire)),
        other => Err(RecordError::Protocol(format!(
            "run: unexpected reply {other:?}"
        ))),
    }
}

/// Seal the base snapshot, nudging past non-snapshottable boundaries exactly as
/// [`run_sweep`](crate::run_sweep) does. Returns `(SnapId, snapshot V-time)`.
fn seal_base<B: Backend>(
    server: &mut ControlServer<B>,
    cfg: &crate::SweepConfig,
) -> Result<(SnapId, u64), RecordError> {
    let mut vt = probe_vtime(server)?;
    let mut attempts = 0usize;
    loop {
        attempts += 1;
        match server.handle(&Request::Snapshot)? {
            Ok(Reply::SnapId(id)) => return Ok((id, vt)),
            Ok(Reply::Unit | Reply::Hello(_) | Reply::Hash(_) | Reply::Stop(_)) => {
                return Err(RecordError::Protocol("snapshot: unexpected reply".into()));
            }
            Err(ControlError::NotQuiescent) => {
                if attempts >= cfg.snapshot_max_attempts {
                    return Err(RecordError::Snapshot(format!(
                        "still not quiescent after {attempts} attempts"
                    )));
                }
                let stop = run(server, Some(vt.saturating_add(cfg.snapshot_retry_step)))?;
                if !matches!(stop, StopReason::Deadline { .. }) {
                    return Err(RecordError::Snapshot(format!(
                        "guest ended at {stop:?} before a sealable boundary was found"
                    )));
                }
                vt = stop.vtime().0;
            }
            Err(ce) => return Err(RecordError::Control(ce)),
        }
    }
}

/// Drive the in-process recording campaign: `hello`, seal the base, then per
/// seed × run `branch → run → drain → assemble → store`. See the module doc for
/// the pure-sink / one-stamp-axis invariants.
pub fn run_recording<B: Backend>(
    server: &mut ControlServer<B>,
    store: &TraceStore,
    cfg: &RecordConfig,
) -> Result<RecordReport, RecordError> {
    // `hello` first (task 25): the server answers its own caps.
    match call(server, &Request::Hello(server_caps()))? {
        Reply::Hello(_) => {}
        other => return Err(RecordError::Protocol(format!("hello: {other:?}"))),
    }

    let (base, snapshot_vtime) = seal_base(server, &cfg.sweep)?;
    let deadline = cfg
        .sweep
        .deadline_delta
        .map(|d| snapshot_vtime.saturating_add(d));

    let codec = SpecEnvCodec;
    let mut rows = Vec::with_capacity(cfg.sweep.seeds.len() * cfg.sweep.runs_per_seed);

    for &seed in &cfg.sweep.seeds {
        // The seed-driven reproducer (genesis-complete, no overrides); decode it
        // once to reach the raw `EnvSpec` bytes the server's `branch` wants and
        // the spec the run's recorded env re-wraps.
        let genesis = codec.seeded(seed);
        let ae = AdapterEnv::decode(&genesis)
            .map_err(|e| RecordError::Protocol(format!("self-minted env failed to decode: {e}")))?;
        let wire_env = WireEnv {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: ae.spec.encode(),
        };

        // Per-seed byte-identity: hold run-0's journal to compare the repeats.
        let mut first_journal: Option<Vec<u8>> = None;

        for run_idx in 0..cfg.sweep.runs_per_seed {
            match call(
                server,
                &Request::Branch {
                    snap: base,
                    env: wire_env.clone(),
                },
            )? {
                Reply::Unit => {}
                other => return Err(RecordError::Protocol(format!("branch: {other:?}"))),
            }

            // Baseline the console cursor at the fork's current serial length, so
            // only bytes this run emits are drained ("new serial bytes").
            let cursor = serial_len(server)?;
            let stop = run(server, deadline)?;
            let terminal_vtime = stop.vtime().0;
            let console = drained_serial(server, cursor)?;

            // The whole console is stamped at the stop's Moment (stop-granular).
            let chunks = vec![(stamp(stop.vtime()), console.clone())];
            let records = runtrace::decode_chunks(cfg.stream, &chunks);
            let stamps_monotone = is_monotone(&records);

            // The recorded reproducer == what `SocketMachine::recorded_env` would
            // emit for this seed-driven run: the branched spec rooted at the
            // snapshot Moment, positioned at the terminal (no decisions ⇒ the
            // delta is exactly the branch env).
            //
            // This IS genesis-complete despite the ephemeral base `SnapId`: the
            // snapshot regenerates by deterministic replay — boot genesis under
            // `spec`, `run(deadline = base_offset)`, seal — reaches the identical
            // base state (substrate determinism, the premise task 63 validates),
            // so `{base_offset, pos, spec}` fully reproduces the run from genesis.
            // env-only retention rests on exactly this (the Nyx take: the artifact
            // is what the restore path cannot regenerate — here only the env is).
            let run_env = AdapterEnv {
                base_offset: snapshot_vtime,
                pos: terminal_vtime,
                spec: ae.spec.clone(),
            }
            .encode();

            let trace = RunTrace {
                terminal: stop.clone(),
                env: run_env,
                coverage: None, // zero-width negotiated geometry; a terminal signal, never a key
                events: vec![], // link tier — empty until task 73
                records,
            };
            let journal = runtrace::encode(&trace);
            let retain = retain_for(cfg.retain, &stop, false);
            // The loop's ONLY store interaction is this write — the store is
            // write-only to the recording loop (gate 5's "assert no reads":
            // nothing here reads sensor/store output mid-campaign). The reload /
            // re-derive half of gate 3 is a post-campaign check
            // ([`verify_store_reload`]), not an in-loop readback.
            let id = store.record(&trace, retain)?;

            let journal_matches_first_run = match &first_journal {
                None => {
                    first_journal = Some(journal.clone());
                    true
                }
                Some(first) => *first == journal,
            };

            rows.push(RecordedRun {
                seed,
                run: run_idx,
                trace_id: id,
                stop,
                records_len: trace.records.len(),
                journal_len: journal.len(),
                retained: retain == Retain::Full,
                stamps_monotone,
                journal_matches_first_run,
                console_head: console.iter().copied().take(96).collect(),
            });
        }
    }

    Ok(RecordReport {
        snapshot_vtime,
        rows,
    })
}

fn serial_len<B: Backend>(server: &ControlServer<B>) -> Result<usize, RecordError> {
    server
        .vmm()
        .map(|v| v.serial().len())
        .ok_or_else(|| RecordError::Protocol("no live VM to drain serial from".into()))
}

fn drained_serial<B: Backend>(
    server: &ControlServer<B>,
    cursor: usize,
) -> Result<Vec<u8>, RecordError> {
    let vmm = server
        .vmm()
        .ok_or_else(|| RecordError::Protocol("no live VM to drain serial from".into()))?;
    let serial = vmm.serial();
    // A fresh fork's serial can only grow past `cursor`; guard defensively so a
    // shorter buffer never panics on the slice.
    Ok(serial.get(cursor..).unwrap_or(&[]).to_vec())
}

fn is_monotone(records: &[(Moment, explorer::Record)]) -> bool {
    records.windows(2).all(|w| w[0].0 <= w[1].0)
}

/// Convert a wire [`control_proto::StopReason`] to the spine's
/// [`StopReason`] — mirrors the adapter's `stop_from_wire`, so a recorded
/// terminal is byte-identical to what the explorer would observe.
fn stop_from_wire(stop: control_proto::StopReason) -> StopReason {
    use control_proto::StopReason as Ws;
    match stop {
        Ws::Deadline { vtime } => StopReason::Deadline {
            vtime: VTime(vtime.0),
        },
        Ws::Quiescent { vtime } => StopReason::Quiescent {
            vtime: VTime(vtime.0),
        },
        Ws::Crash { vtime, info } => {
            let kind = match info.kind {
                control_proto::CrashKind::Panic => 0u8,
                control_proto::CrashKind::TripleFault => 1,
                control_proto::CrashKind::Shutdown => 2,
            };
            let mut bytes = Vec::with_capacity(1 + info.detail.len());
            bytes.push(kind);
            bytes.extend_from_slice(&info.detail);
            StopReason::Crash {
                vtime: VTime(vtime.0),
                info: bytes,
            }
        }
        Ws::Decision { vtime, id, ctx } => StopReason::Decision {
            vtime: VTime(vtime.0),
            id: id.0,
            ctx,
        },
        Ws::SnapshotPoint { vtime } => StopReason::SnapshotPoint {
            vtime: VTime(vtime.0),
        },
        Ws::Assertion { vtime, ev } => StopReason::Assertion {
            vtime: VTime(vtime.0),
            id: ev.id,
            data: ev.data,
        },
    }
}

/// The task-65 gate checks over a [`RecordReport`] — a **pure** check on the
/// report the recording loop produced (no store access):
///
/// 1. **Per-seed byte-identity** — every run of a seed shares one `TraceId` and
///    byte-identical journal (determinism: same env + same run ⇒ same bytes);
///    fewer than two runs cannot demonstrate it.
/// 2. **Divergence** — at least `min_distinct` distinct `TraceId`s across seeds.
/// 3. **Non-empty, monotone records.**
///
/// The lossless-reload / re-derive half of gate 3 is [`verify_store_reload`], a
/// deliberately *post-campaign* check so the loop stays write-only to the store.
///
/// Returns every violated gate (empty = all pass).
pub fn verify_record(report: &RecordReport, min_distinct: usize) -> Vec<String> {
    let mut failures = Vec::new();

    // Group rows by seed (BTreeMap: deterministic order).
    let mut by_seed: BTreeMap<u64, Vec<&RecordedRun>> = BTreeMap::new();
    for row in &report.rows {
        by_seed.entry(row.seed).or_default().push(row);
    }

    for (seed, runs) in &by_seed {
        if runs.len() < 2 {
            failures.push(format!(
                "seed {seed:#018x}: only {} run — byte-identity needs at least 2 runs to compare",
                runs.len()
            ));
        }
        let first_id = runs[0].trace_id;
        for r in runs {
            if r.trace_id != first_id {
                failures.push(format!(
                    "seed {seed:#018x}: run {} TraceId {} != run 0 {} — not content-stable",
                    r.run, r.trace_id, first_id
                ));
            }
            if !r.journal_matches_first_run {
                failures.push(format!(
                    "seed {seed:#018x}: run {} journal bytes differ from run 0 — not byte-identical",
                    r.run
                ));
            }
            if r.records_len == 0 {
                failures.push(format!("seed {seed:#018x}: run {} has no records", r.run));
            }
            if !r.stamps_monotone {
                failures.push(format!(
                    "seed {seed:#018x}: run {} stamps not monotone",
                    r.run
                ));
            }
        }
    }

    let mut distinct: Vec<TraceId> = report.rows.iter().map(|r| r.trace_id).collect();
    distinct.sort_unstable();
    distinct.dedup();
    if distinct.len() < min_distinct {
        failures.push(format!(
            "only {} distinct TraceId(s) across {} seeds (need >= {min_distinct}) — envs did not \
             diverge",
            distinct.len(),
            by_seed.len()
        ));
    }

    failures
}

/// The **post-campaign** store-reload gate (the lossless-reload / re-derive half
/// of gate 3), run *after* [`run_recording`] returns so the recording loop stays
/// write-only to the store. For each distinct recorded `TraceId`:
///
/// - the env sidecar reloads and its bytes round-trip canonically;
/// - a retained journal reloads, re-encodes to byte-identical journal bytes
///   (`encode(load(id)) == encode(decode(on-disk))`), and its recorded env
///   matches the sidecar — the on-disk artifact is exactly the recorded run.
///
/// Returns every violated gate (empty = all pass).
pub fn verify_store_reload(store: &TraceStore, report: &RecordReport) -> Vec<String> {
    let mut failures = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for row in &report.rows {
        if !seen.insert(row.trace_id) {
            continue; // one check per distinct id (repeats overwrite identically)
        }
        let id = row.trace_id;
        let env = match store.env(id) {
            Ok(e) => e,
            Err(e) => {
                failures.push(format!("trace {id}: env did not reload: {e}"));
                continue;
            }
        };
        // The env sidecar is content-addressed by exactly these bytes.
        if runtrace::TraceId::of(&env) != id {
            failures.push(format!(
                "trace {id}: reloaded env is not content-addressed to its id"
            ));
        }
        if store.has_journal(id) {
            match store.load(id) {
                // The retained journal decodes, and its env is the same
                // reproducer the sidecar holds — the on-disk artifact is a
                // consistent, reloadable copy of the recorded run.
                Ok(reloaded) if reloaded.env != env => {
                    failures.push(format!("trace {id}: journal env != sidecar env"));
                }
                Ok(_) => {}
                Err(e) => {
                    failures.push(format!("trace {id}: retained journal did not reload: {e}"))
                }
            }
        }
    }
    failures
}

/// Render the recording run table (recorded in `IMPLEMENTATION.md` for the box
/// gate). One line per (seed, run) plus the header.
pub fn render_record_table(report: &RecordReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "base snapshot sealed at V-time {}\n",
        report.snapshot_vtime
    ));
    out.push_str(&format!(
        "{:<20} {:>3}  {:<20} {:>6} {:>7}  {:<7}  {}\n",
        "seed", "run", "stop", "recs", "journal", "retain", "trace_id"
    ));
    for r in &report.rows {
        out.push_str(&format!(
            "{:#018x} {:>3}  {:<20} {:>6} {:>7}  {:<7}  {}\n",
            r.seed,
            r.run,
            crate::fmt_stop(&r.stop),
            r.records_len,
            r.journal_len,
            if r.retained { "full" } else { "env" },
            r.trace_id,
        ));
    }
    out
}
