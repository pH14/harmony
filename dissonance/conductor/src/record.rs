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
    ControlError, Environment as WireEnv, HashScope, Reply, Request, SnapId, StopConditions,
    StopMask, VTime as WireVTime,
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
    /// The whole-VM `state_hash` at the terminal — the **guest-state**
    /// determinism primitive the task-58 sweep gates on. Unlike the journal
    /// digest (console + terminal + env), this proves the guest reached a
    /// bit-identical *machine state*: per-seed equality is reproducibility,
    /// cross-seed inequality is real divergence (the seed reaching VM state via
    /// the entropy path). The `--record` path would otherwise bypass this.
    pub state_hash: [u8; 32],
    /// How many console records the run produced.
    pub records_len: usize,
    /// The serialized journal's length in bytes.
    pub journal_len: usize,
    /// `blake3` of the serialized journal bytes — the compact per-run fingerprint
    /// the divergence and reload gates compare (the journal covers `terminal` +
    /// `records` + `env` + `coverage`, so this captures the whole observed run,
    /// not just the env-derived [`TraceId`]).
    pub journal_digest: [u8; 32],
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

/// The whole-VM `state_hash` at the current point — the determinism primitive
/// the task-58 sweep gates on. Read-only (does not advance the guest), so it is
/// the terminal state hash when called right after a `run`.
fn hash<B: Backend>(server: &mut ControlServer<B>) -> Result<[u8; 32], RecordError> {
    match call(
        server,
        &Request::Hash {
            scope: HashScope::Whole,
        },
    )? {
        Reply::Hash(h) => Ok(h),
        other => Err(RecordError::Protocol(format!(
            "hash: unexpected reply {other:?}"
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
            Ok(
                Reply::Unit
                | Reply::Hello(_)
                | Reply::Hash(_)
                | Reply::Stop(_)
                | Reply::SdkEvents(_),
            ) => {
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
            // The guest-state determinism primitive at the terminal (read-only;
            // the state is stable at the stop). Gated per seed (equality =
            // reproducible) and across seeds (inequality = real divergence) —
            // the strong property `--record` would otherwise bypass. This is the
            // same hash the task-58 sweep checks, at the same point.
            let state_hash = hash(server)?;

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
                // Task 73: the link tier decodes the raw SDK event capture (the
                // catalog + assert/state/buggify emissions) into the typed,
                // Moment-stamped event stream — non-empty for an SDK guest.
                events: link::decode_events(&sdk_events_raw(server)),
                records,
            };
            // Encode once, up front — the source of the journal length, digest,
            // and (for a Full record) the persisted bytes. Fails loudly on an
            // unrepresentable (> 4 GiB field) trace rather than persisting garbage.
            let journal = runtrace::encode(&trace)?;
            let journal_digest = *blake3::hash(&journal).as_bytes();
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
                state_hash,
                records_len: trace.records.len(),
                journal_len: journal.len(),
                journal_digest,
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

/// The link-tier SDK event capture of the current run, remapped onto the
/// [`Moment`] axis for [`link::decode_events`] (task 73). In-process, straight
/// off the live VM — the SDK-channel mirror of [`drained_serial`]. No cursor is
/// needed: the per-run `branch` resets the SDK channel, so the capture already
/// holds exactly this run's events.
fn sdk_events_raw<B: Backend>(server: &ControlServer<B>) -> Vec<(Moment, u32, Vec<u8>)> {
    server
        .vmm()
        .map(|v| {
            v.sdk_events()
                .iter()
                .map(|(m, id, b)| (Moment(*m), *id, b.clone()))
                .collect()
        })
        .unwrap_or_default()
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
/// 1. **Per-seed determinism** — every run of a seed shares one whole-VM
///    `state_hash` (**guest-state** reproducibility, mirroring the task-58 sweep),
///    one `TraceId`, and byte-identical journal bytes; fewer than two runs cannot
///    demonstrate it.
/// 2. **Divergence** — two checks, both required:
///    - at least `min_distinct` distinct **`state_hash`es** across seeds — the
///      strong guest-state property: `--record`'s journal byte-identity alone
///      proves only console/terminal determinism, not that the seed reached a
///      distinct *machine state* (an RDRAND-seeding regression would leave the
///      console identical yet is caught here); the same primitive the sweep gates
///      on, captured at the same point;
///    - at least `min_distinct` distinct **`TraceId`s** across seeds — the spec's
///      letter (gate 6: "≥2 distinct TraceIds"). `TraceId = blake3(env)` diverges
///      *by construction*, so a collapse here means the envs folded across seeds
///      (a regression that stops embedding the seed, or coincident terminal
///      vtimes) — the content-addressed store would silently fold N seeds into one
///      reproducer, losing env-only replay of the rest. A construction sanity
///      check that must not be dropped.
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
                "seed {seed:#018x}: only {} run — determinism needs at least 2 runs to compare",
                runs.len()
            ));
        }
        let first_id = runs[0].trace_id;
        let first_hash = runs[0].state_hash;
        for r in runs {
            if r.state_hash != first_hash {
                failures.push(format!(
                    "seed {seed:#018x}: run {} state_hash != run 0 — guest state NOT reproducible",
                    r.run
                ));
            }
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

    // Divergence over the guest-state hash — the STRONG property (the journal
    // digest and TraceId both embed the seed and so diverge by construction).
    let mut distinct: Vec<[u8; 32]> = report.rows.iter().map(|r| r.state_hash).collect();
    distinct.sort_unstable();
    distinct.dedup();
    if distinct.len() < min_distinct {
        failures.push(format!(
            "only {} distinct state_hash(es) across {} seeds (need >= {min_distinct}) — guest \
             states did not diverge",
            distinct.len(),
            by_seed.len()
        ));
    }

    // AND the spec's letter: ≥ min_distinct distinct TraceIds. Diverges by
    // construction, so a collapse means envs folded across seeds — the store
    // would silently merge N reproducers into one, losing env-only replay.
    let mut distinct_ids: Vec<TraceId> = report.rows.iter().map(|r| r.trace_id).collect();
    distinct_ids.sort_unstable();
    distinct_ids.dedup();
    if distinct_ids.len() < min_distinct {
        failures.push(format!(
            "only {} distinct TraceId(s) across {} seeds (need >= {min_distinct}) — envs folded \
             (reproducers collapsed in the content-addressed store)",
            distinct_ids.len(),
            by_seed.len()
        ));
    }

    failures
}

/// The **post-campaign** store-reload gate (the lossless-reload / re-derive half
/// of gate 3), run *after* [`run_recording`] returns so the recording loop stays
/// write-only to the store. Checked against **each row's own retention
/// expectation** — never guarded by `has_journal`, so a `retained: true` row
/// whose journal is missing (or a `record` regression that stops writing it)
/// **fails** rather than vacuously passing. For every recorded row:
///
/// - the env sidecar reloads and is content-addressed to its `TraceId`;
/// - `retained: true` ⇒ [`load`](TraceStore::load) must succeed **and the reloaded
///   trace matches the report row** — same `terminal`, same record count, and a
///   journal that re-encodes byte-for-byte to the recorded digest. Comparing only
///   the env would let a stale/corrupted `.trace` for the same reproducer (same
///   env-derived id, different terminal/records) pass. `NotRetained`/`NotFound`
///   are failures;
/// - `retained: false` ⇒ no journal is present (a fresh store; env-only never
///   writes one, and an env-only re-record removes any prior journal).
///
/// Returns every violated gate (empty = all pass).
pub fn verify_store_reload(store: &TraceStore, report: &RecordReport) -> Vec<String> {
    let mut failures = Vec::new();
    for row in &report.rows {
        let id = row.trace_id;
        let where_ = format!("trace {id} (seed {:#018x} run {})", row.seed, row.run);
        // `env`/`load` re-verify the content address internally (an
        // `IdMismatch` surfaces here as a reload failure), so a successful read
        // already proves the artifact hashes back to its id.
        let env = match store.env(id) {
            Ok(e) => e,
            Err(e) => {
                failures.push(format!("{where_}: env did not reload: {e}"));
                continue;
            }
        };
        if row.retained {
            // The gate must actually LOAD the journal — no `has_journal` guard —
            // and the reloaded trace must match the report row, not merely share
            // an env.
            match store.load(id) {
                Ok(reloaded) => {
                    if reloaded.env != env {
                        failures.push(format!("{where_}: journal env != sidecar env"));
                    }
                    if reloaded.terminal != row.stop {
                        failures.push(format!("{where_}: reloaded terminal != recorded stop"));
                    }
                    if reloaded.records.len() != row.records_len {
                        failures.push(format!(
                            "{where_}: reloaded records {} != recorded {}",
                            reloaded.records.len(),
                            row.records_len
                        ));
                    }
                    match runtrace::encode(&reloaded) {
                        Ok(bytes) => {
                            if bytes.len() != row.journal_len {
                                failures.push(format!(
                                    "{where_}: reloaded journal {} bytes != recorded {}",
                                    bytes.len(),
                                    row.journal_len
                                ));
                            }
                            if *blake3::hash(&bytes).as_bytes() != row.journal_digest {
                                failures.push(format!(
                                    "{where_}: reloaded journal digest != recorded digest"
                                ));
                            }
                        }
                        Err(e) => failures.push(format!("{where_}: reloaded trace re-encode: {e}")),
                    }
                }
                Err(e) => {
                    failures.push(format!(
                        "{where_}: retained=true but the journal did not load: {e}"
                    ));
                }
            }
        } else if store.has_journal(id) {
            failures.push(format!("{where_}: retained=false but a journal is present"));
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
        "{:<20} {:>3}  {:<20} {:>6} {:>7}  {:<6}  {:<14}  {:<14}\n",
        "seed", "run", "stop", "recs", "journal", "retain", "state_hash", "trace_id"
    ));
    // 12-hex (6-byte) prefixes keep the table readable while still letting a
    // reader eyeball per-seed equality and cross-seed divergence.
    let short = |bytes: &[u8; 32]| crate::hex(bytes).chars().take(12).collect::<String>();
    for r in &report.rows {
        out.push_str(&format!(
            "{:#018x} {:>3}  {:<20} {:>6} {:>7}  {:<6}  {:<14}  {:<14}\n",
            r.seed,
            r.run,
            crate::fmt_stop(&r.stop),
            r.records_len,
            r.journal_len,
            if r.retained { "full" } else { "env" },
            short(&r.state_hash),
            short(&r.trace_id.0),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use control_proto::{CrashInfo, CrashKind, DecisionId, EventRef, StopReason as Ws};
    use explorer::StreamId as ExplorerStreamId;

    /// `is_monotone` is true for non-decreasing stamps (including a single or
    /// empty record) and false as soon as any adjacent pair goes backward.
    #[test]
    fn is_monotone_true_for_sorted_false_for_out_of_order() {
        let rec = |m: u64| {
            (
                Moment(m),
                explorer::Record {
                    stream: ExplorerStreamId(0),
                    line: vec![],
                },
            )
        };
        assert!(is_monotone(&[]), "no records is trivially monotone");
        assert!(
            is_monotone(&[rec(5)]),
            "a single record is trivially monotone"
        );
        assert!(
            is_monotone(&[rec(1), rec(1), rec(3)]),
            "equal stamps are non-decreasing"
        );
        assert!(
            !is_monotone(&[rec(3), rec(1)]),
            "a decrease must be flagged"
        );
        assert!(
            !is_monotone(&[rec(1), rec(5), rec(2)]),
            "a decrease anywhere in the sequence must be flagged"
        );
    }

    /// `stop_from_wire` mirrors every wire [`control_proto::StopReason`]
    /// variant onto the spine's `StopReason` byte-for-byte, including the
    /// `Crash` info's `kind`-byte-prefix encoding (mirrors the R2 adapter's
    /// `stop_from_wire`, so a recorded terminal is identical to what the
    /// explorer would observe over the wire).
    #[test]
    fn stop_from_wire_maps_every_variant() {
        assert_eq!(
            stop_from_wire(Ws::Deadline {
                vtime: WireVTime(10)
            }),
            StopReason::Deadline { vtime: VTime(10) }
        );
        assert_eq!(
            stop_from_wire(Ws::Quiescent {
                vtime: WireVTime(11)
            }),
            StopReason::Quiescent { vtime: VTime(11) }
        );
        assert_eq!(
            stop_from_wire(Ws::SnapshotPoint {
                vtime: WireVTime(12)
            }),
            StopReason::SnapshotPoint { vtime: VTime(12) }
        );
        assert_eq!(
            stop_from_wire(Ws::Decision {
                vtime: WireVTime(13),
                id: DecisionId(7),
                ctx: vec![9, 9]
            }),
            StopReason::Decision {
                vtime: VTime(13),
                id: 7,
                ctx: vec![9, 9]
            }
        );
        assert_eq!(
            stop_from_wire(Ws::Assertion {
                vtime: WireVTime(14),
                ev: EventRef {
                    id: 3,
                    data: vec![1, 2]
                }
            }),
            StopReason::Assertion {
                vtime: VTime(14),
                id: 3,
                data: vec![1, 2]
            }
        );
        for (kind, byte) in [
            (CrashKind::Panic, 0u8),
            (CrashKind::TripleFault, 1u8),
            (CrashKind::Shutdown, 2u8),
        ] {
            let got = stop_from_wire(Ws::Crash {
                vtime: WireVTime(15),
                info: CrashInfo {
                    kind,
                    detail: vec![0xAA, 0xBB],
                },
            });
            assert_eq!(
                got,
                StopReason::Crash {
                    vtime: VTime(15),
                    info: [&[byte][..], &[0xAA, 0xBB]].concat(),
                },
                "the kind byte is prepended to the detail bytes"
            );
        }
    }

    /// [`render_record_table`] pins the exact rendered shape — the artifact
    /// the box gate records verbatim in IMPLEMENTATION.md.
    #[test]
    fn render_record_table_pins_the_format() {
        let report = RecordReport {
            snapshot_vtime: 5,
            rows: vec![RecordedRun {
                seed: 0x1111,
                run: 0,
                trace_id: runtrace::TraceId([0x11; 32]),
                stop: StopReason::Quiescent { vtime: VTime(50) },
                state_hash: [0x22; 32],
                records_len: 3,
                journal_len: 40,
                journal_digest: [0; 32],
                retained: true,
                stamps_monotone: true,
                journal_matches_first_run: true,
                console_head: vec![],
            }],
        };
        let expected = "base snapshot sealed at V-time 5\n\
seed                 run  stop                   recs journal  retain  state_hash      trace_id      \n\
0x0000000000001111   0  Quiescent@50              3      40  full    222222222222    111111111111  \n";
        assert_eq!(render_record_table(&report), expected);
    }
}
