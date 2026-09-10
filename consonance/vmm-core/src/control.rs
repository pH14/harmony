// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **control-transport server** (task 58): the frontier glue that serves
//! dissonance's out-of-band R2 verbs — `hello` / `snapshot` / `drop` / `branch`
//! / `replay` / `run` / `hash` (+ `perturb`, unsupported until task 59) — over
//! `control-proto`'s length-delimited codec, against a live [`Vmm`] and a
//! [`SnapshotEngine`].
//!
//! This is the first time any of the eight verbs is actually served: the
//! explorer's socket-backed `Machine` (dissonance task 12 / 58) drives this
//! server as a black box. The server is **workload-agnostic substrate surface**
//! (task 43 F5 discipline): nothing here knows what runs inside the guest — it
//! restores snapshots, reseeds entropy, steps the event loop, and hashes state.
//!
//! ## Verb semantics (seed-driven scope, task 58)
//!
//! - **`hello(caps)`** → the server's [`Caps`]: application protocol [`control_proto::APP_PROTOCOL_VERSION`]
//!   (framing protocol 1), `Reproducer` blob version exactly
//!   [`EnvSpec::BLOB_VERSION`], **empty/zero-width coverage
//!   geometry** (no coverage producer exists yet) and — task 73 —
//!   `GUEST_HAS_SDK` (the doorbell is serviced). A mismatched application
//!   version, or any other verb before a successful `hello`, answers
//!   [`ControlError::Unsupported`].
//! - **`snapshot`** → seal the current point (memory + `vm_state`) into the
//!   engine and mint a pool-wide [`SnapId`]. State-capture representation gaps
//!   answer [`ControlError::SnapshotRefused`] with the owning layer's diagnostic.
//!   Pending host effects, reseeds, and schedule failures are captured alongside
//!   VM and SDK state. Saving never advances the stopped execution.
//! - **`drop(snap)`** → release + GC via the store (pool GC).
//! - **`branch(snap, env)`** → restore `snap` into a **fresh, equivalently
//!   composed VM** (from the [`VmmFactory`]) and **reseed the entropy stream
//!   from the env's seed** ([`Vmm::reseed_entropy`]) so the branched future
//!   diverges through the already-deterministic RDRAND path (the proven
//!   divergence mechanism, tasks 40/42). An env carrying **reseed markers**
//!   (task 78) is honored marker-wise instead: the marker at the restore floor
//!   is the branch reseed, markers beyond it are staged and re-executed at
//!   their exact `Moment`s by `run` (a collapsed hop's reseed replays at its
//!   recorded position — bit-identical compose folds under entropy draws), and
//!   a marker beyond the trajectory is the same loud
//!   [`ControlError::ScheduleUnsatisfiable`] as a crossed fault. The no-marker
//!   path is byte-for-byte the task-58/59 behavior.
//!   The env blob is decoded (and rejected
//!   loudly — [`ControlError::BadEnvVersion`] / [`ControlError::MalformedEnvironment`])
//!   but now its **host-plane overrides are enforced** (task 59): they are staged
//!   like a `perturb` and applied at their `Moment`s during the branched run. An
//!   env carrying a **guest** override or a **standing** host effect still answers
//!   [`ControlError::Unsupported`] (those require the guest-plane or scheduling
//!   enforcement loop). A service configuration is passed to the installed generic
//!   handler factory and is never silently discarded.
//! - **`replay(snap)`** → restore verbatim under the selected restore mode,
//!   **no reseed** — the repro / determinism-gate path.
//! - **`run(until)`** → advance via [`Vmm::step`] until a terminal stop or the
//!   V-time deadline. Terminal mapping is substrate-level and workload-blind:
//!   `Hlt` and `DebugExit{0}` → [`StopReason::Quiescent`]; `DebugExit{code≠0}`
//!   → [`StopReason::Crash`] (kind `Panic`, detail = the code byte);
//!   backend `Shutdown` (triple fault / guest-initiated shutdown) →
//!   [`StopReason::Crash`] (kind `Shutdown`). A workload that *terminates by
//!   convention* through a forced reboot (the Postgres image's `reboot -f`)
//!   reads as a `Crash{Shutdown}` here — interpreting that convention is the
//!   caller's (workload-aware) job, never this server's. `resolve` is accepted
//!   on the wire but there is never an outstanding decision on the seed-driven
//!   substrate, so any resolve answers [`ControlError::ResolveWithoutDecision`].
//!   The [`StopMask`](control_proto::StopMask) gates no *decision* class yet (none
//!   surface on the seed substrate), but it DOES gate the cooperating-SDK stops
//!   (task 73 round-7): [`SnapshotPoint`](control_proto::StopReason::SnapshotPoint)
//!   and [`Assertion`](control_proto::StopReason::Assertion) surface only when
//!   their class bit is armed, so `StopMask::NONE` runs an SDK guest straight
//!   through to the terminal. Crash / quiescence / deadline always stop.
//! - **`hash(scope)`** → [`Vmm::state_hash`] for `Whole`; `Disk` / `Region`
//!   answer [`ControlError::Unsupported`] (no disk device exists; region
//!   hashing has no consumer yet).
//! - **`perturb(fault, at)`** → **stage a [`HostFault`](Effect)
//!   at a [`Moment`](u64)** (task 59): the fault blob is decoded
//!   and validated (an out-of-range [`CorruptMemory`](Effect::CorruptMemory)
//!   gpa is a loud [`ControlError::PerturbOutOfRange`], a malformed blob a
//!   [`ControlError::MalformedEnvironment`], the out-of-scope `SkewTime`/
//!   `SetClockRate` a [`ControlError::Unsupported`]), then queued. [`run`](ControlServer::run)
//!   applies it *between instructions* at its `Moment` — a guest-RAM XOR for
//!   `CorruptMemory`, an IRR raise through the LAPIC arbitration for
//!   `InjectInterrupt` — and stamps it into the recorded env
//!   ([`recorded_env`](ControlServer::recorded_env)), so the emitted reproducer
//!   replays to the identical `state_hash`.
//!
//! ## Restore discipline
//!
//! `branch`/`replay` default to the proven fresh-VM path. An explicit
//! [`RestoreMode::InPlace`] keeps the live VM, retires any staged userspace
//! completion without executing another guest instruction, patches only the
//! target-different and live-dirty pages, and restores the captured machine
//! state. Any unavailable prerequisite or in-place error is counted and falls
//! back to the fresh-VM path for that operation.
//!
//! ## Two result categories, fail-loud
//!
//! A guest-observable outcome is a [`StopReason`]; a recoverable control-plane
//! failure is a [`ControlError`] **reply**; an unrecoverable substrate failure
//! (a mid-run [`VmmError`], a store invariant, a factory that cannot boot) is a
//! [`ServeError`] that **tears the session down** — the socket closes, the
//! client surfaces a transport error, and the campaign aborts loudly. Nothing
//! is ever silently absorbed or misclassified across the categories.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use control_proto::{
    Caps, ControlError, CoverageGeometry, CrashInfo, CrashKind, EventRef, HashScope, Moment,
    READ_CAP, RegsView, Reply, Reproducer, Request, SnapId, StopReason, decode_request,
    encode_reply,
};
use environment::{
    channel::{Effect, NominalHandler, RecordedEnv, ServiceHandler},
    input_spec::{InputSpec as EnvSpec, ServiceConfig, ServiceFactory, nominal_factory},
};
use snapshot_store::SnapshotId;
use vm_state::SnapshotRecords;
use vmm_backend::Backend;

use crate::vendor::{InterruptReject, Vendor};

use crate::control_state::{ControlState, ScheduleFailure};
use crate::exec::retained::RetainedExec;
use crate::portable_snapshot::{
    PortableSnapshot, PortableSnapshotError, PortableSnapshotRef, SparsePortableSidecarRef,
    SparsePortableSnapshot, SparsePortableSnapshotReceipt, decode_sparse_sidecar,
    encode_sparse_sidecar,
};
use crate::session_trace::{SessionTraceSegment, SessionTraceStart, SessionVirtualTimeTrace};
use crate::snapshot::{SnapshotEngine, SnapshotError};
use crate::vmm::{SdkSnapshot, SdkStop, Step, TerminalReason, Vmm, VmmError};

#[cfg(all(target_os = "linux", any(test, not(miri))))]
fn host_minor_faults_with(getrusage: impl FnOnce(*mut libc::rusage) -> i32) -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    if getrusage(usage.as_mut_ptr()) != 0 {
        return None;
    }
    // SAFETY: a zero return from the supplied getrusage-compatible function
    // means it initialized the caller-owned `rusage` buffer completely.
    let minor_faults = unsafe { usage.assume_init() }.ru_minflt;
    u64::try_from(minor_faults).ok()
}

/// Returns this host process's cumulative minor-page-fault count.
///
/// This is an out-of-band profiling counter. It must never affect guest state,
/// encoded protocol bytes, or a determinism hash. Miri uses the testable seam
/// rather than calling the host C library.
#[cfg(all(target_os = "linux", not(miri)))]
pub fn host_minor_faults() -> Option<u64> {
    host_minor_faults_with(|usage| {
        // SAFETY: `usage` points to the live, writable `MaybeUninit<rusage>`
        // above, and libc writes it only for the duration of this call.
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage) }
    })
}

/// Returns no host minor-fault counter where `getrusage` is unavailable.
#[cfg(any(not(target_os = "linux"), miri))]
pub fn host_minor_faults() -> Option<u64> {
    None
}

/// Boots a fresh, equivalently-composed VM — the restore target for `Memcpy`,
/// `Remap` fallback, and every failed `InPlace` attempt. On the box
/// this re-runs the composition root (the Linux virtual-time boot helper): same RAM size,
/// same wiring (V-time + xAPIC + legacy), same contract — the boot-loaded guest
/// image is immediately overwritten by the restore, so the factory's seed is
/// irrelevant. In the portable gates it builds a fresh scripted
/// `Vmm<MockBackend>`. The server drops the previous VM before calling it so no
/// backend completion or host resource can leak across the restore.
pub type VmmFactory<B> = Box<dyn FnMut() -> Result<Vmm<B>, VmmError>>;

/// Boots a fresh restore target **around a materialized snapshot mapping** —
/// the task-95 M2.2 remap-restore factory (see
/// [`crate::vendor::x86::bringup::compose_restore_target`]): the mapping's buffer becomes the
/// guest RAM the memslots register, so the restore performs **no** full-image
/// memcpy and untouched pages fault lazily. Must compose its VMs exactly like
/// the session's [`VmmFactory`] (same RAM size, wiring, contract) minus the
/// boot-image load; the server then restores only the non-memory half
/// ([`Vmm::restore_vm_state`]). Same drop-the-old-VM-first discipline as
/// [`VmmFactory`].
pub type RemapVmmFactory<B> = Box<dyn FnMut(snapshot_store::Mapping) -> Result<Vmm<B>, VmmError>>;

/// How `branch`/`replay` restore guest memory (task 95 M2.2) — the A/B knob of
/// the restore determinism gate, and the fallback if a box gate fails.
///
/// **The mode always tells the truth** (PR #95 round-1): a server starts in
/// [`RestoreMode::Memcpy`] — the only path it *can* take — and
/// [`ControlServer::set_remap_factory`] flips it to [`RestoreMode::Remap`] as
/// part of installing the capability, so `Remap` is the default exactly where
/// remapping is possible and a composition that never opts in never *claims*
/// to remap. There is no silent degrade: [`ControlServer::restore_mode`]
/// reports the path restores actually take.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RestoreMode {
    /// Patch the live VM's RAM to the target snapshot and restore its vCPU and
    /// device state without replacing the VM. Any unavailable prerequisite or
    /// restore failure falls back to the fresh-VM path for that operation.
    InPlace,
    /// The mapping becomes the memslot backing (no memcpy; lazy faults). The
    /// default once a [`RemapVmmFactory`] is installed.
    Remap,
    /// Materialize, boot a fresh owned-RAM VM, memcpy the image in — the
    /// pre-task-95 path, byte-for-byte. The default until a remap factory
    /// exists (there is nothing else a factory-less server could do).
    Memcpy,
}

/// Whether restoring in `mode` requires materializing a complete snapshot
/// mapping before the live VM is replaced. In-place restore deliberately
/// defers materialization so it can patch only changed pages.
fn restore_defers_materialization(mode: RestoreMode) -> bool {
    mode == RestoreMode::InPlace
}

/// Only errors guaranteed to occur before `restore_vm_state` mutates the
/// target may be returned as a recoverable `RestoreFailed` reply.
fn restore_error_is_precommit(error: &VmmError) -> bool {
    matches!(
        error,
        VmmError::ContractViolation(_) | VmmError::Snapshot(_) | VmmError::Vtime(_)
    )
}

/// Classify the ordering relationship between adjacent sparse-page GFNs.
/// Keeping this check separate from the store's own validation makes the
/// control-plane error contract explicit before any layer is derived.
fn sparse_page_order_error(previous: Option<u64>, current: u64) -> Option<SnapshotError> {
    let previous = previous?;
    if current < previous {
        return Some(SnapshotError::SparsePagesNotSorted { previous, current });
    }
    if current == previous {
        return Some(SnapshotError::SparsePageDuplicate { gfn: current });
    }
    None
}

/// An unrecoverable, session-fatal server failure — the loud half of the
/// two-result-categories rule. [`ControlServer::serve`] returns it after which
/// the transport is closed (the peer sees EOF and surfaces a transport error);
/// recoverable failures are answered on the wire as [`ControlError`] replies
/// instead and never reach this type.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The transport stream failed (read/write).
    #[error("control transport I/O error")]
    Io(#[from] std::io::Error),
    /// The inbound byte stream is not a decodable frame sequence (bad magic /
    /// version / over-cap length / malformed body, or EOF mid-frame). Framing
    /// cannot be resynchronized, so this is fatal.
    #[error("control transport framing error: {0}")]
    Protocol(#[from] control_proto::ProtocolError),
    /// The substrate failed mid-verb (a step error, a failed fresh-VM boot, a
    /// backend save failure). The VM's state can no longer be vouched for.
    #[error("substrate failure: {0}")]
    Vmm(#[from] VmmError),
    /// The snapshot store / codec hit an invariant failure (not a caller error
    /// — those answer `ControlError` replies).
    #[error("snapshot store failure")]
    Snapshot(#[from] SnapshotError),
    /// An installed service cannot capture or restore its execution state.
    #[error("service state failure: {0}")]
    Service(#[from] environment::channel::ChannelError),
    /// A verb arrived after a previous fatal error already tore the VM down
    /// (the server is poisoned; a prior [`ServeError`] was returned).
    #[error("server poisoned by a prior fatal error")]
    Poisoned,
    /// Retained command state disagrees with the VM serial stream.
    #[error("retained command state: {0}")]
    CommandState(&'static str),
}

/// Stable evidence returned by portable snapshot export/import.
///
/// The imported handle is session-local, while the remaining fields are the
/// source seal's immutable cut and immediate whole-state oracle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PortableSnapshotReceipt {
    /// Session-local handle for export or the newly imported base snapshot.
    pub id: SnapId,
    /// Exact synchronized V-time at the seal.
    pub at: Moment,
    /// SDK event-prefix length included in the snapshot.
    pub sdk_events: u64,
    /// Portable normalized-event prefix length at the seal.
    pub trace_events: u64,
    /// Portable deadline-schedule prefix length at the seal.
    pub trace_schedules: u64,
    /// Whether the sealed lineage was tainted by improvisation.
    pub tainted: bool,
    /// Source whole control/VM state hash at the same stopped seal boundary.
    pub state_hash: [u8; 32],
}

/// The [`Caps`] this server negotiates: the current negotiated application
/// protocol ([`control_proto::APP_PROTOCOL_VERSION`]), `Reproducer` blobs exactly
/// at [`EnvSpec::BLOB_VERSION`], **zero-width coverage geometry** (no coverage
/// producer exists — task 58 is seed-driven), and — task 73 — the
/// `GUEST_HAS_SDK` flag (the server services the hypercall doorbell for a
/// cooperating guest SDK). Exposed so the client side can pin its check against
/// the same
/// constant — a peer that negotiated an older version rejects **at `hello`** rather
/// than breaking mid-session on a reply tag it does not know (PR #51 round-8).
pub fn server_caps() -> Caps {
    Caps {
        protocol_version: control_proto::APP_PROTOCOL_VERSION,
        env_version_min: EnvSpec::BLOB_VERSION,
        env_version_max: EnvSpec::BLOB_VERSION,
        coverage: CoverageGeometry {
            map_bytes: 0,
            producer: 0,
        },
        // Task 73: the server now services the hypercall doorbell for a
        // cooperating guest SDK — assertions surface `StopReason::Assertion`,
        // `setup_complete` surfaces `StopReason::SnapshotPoint`, and generic
        // service requests are answered — so `GUEST_HAS_SDK` is advertised.
        flags: control_proto::CapFlags::GUEST_HAS_SDK,
    }
}

/// The control-transport server: one live [`Vmm`], a [`SnapshotEngine`] holding
/// the snapshot pool, the [`VmmFactory`] that boots restore targets, and the
/// wire-handle table. One server = one session = one VM; see the module doc.
pub struct ControlServer<B: Backend<A: Vendor>> {
    /// The live VM. `None` only after a fatal error already tore it down (or
    /// transiently inside a `branch`/`replay`, where the old VM must be dropped
    /// before the factory boots its replacement).
    vmm: Option<Vmm<B>>,
    factory: VmmFactory<B>,
    service_factory: ServiceFactory,
    /// The remap-restore factory (task 95 M2.2), when the composition root
    /// provides one; `None` = memcpy-only (the pre-task-95 behavior, and what
    /// every existing composition gets unchanged).
    remap_factory: Option<RemapVmmFactory<B>>,
    /// The restore-mode A/B knob. Effective only with a [`RemapVmmFactory`]
    /// installed; see [`RestoreMode`].
    restore_mode: RestoreMode,
    engine: SnapshotEngine,
    /// The **derive parent** for the next seal (task 95 M2.1): the store id of
    /// the snapshot the live VM's state is a tracked continuation of — set after
    /// a successful seal (the new snapshot) and after a successful
    /// `branch`/`replay` (the restore source), `None` for a fresh boot or
    /// whenever the dirty-tracking window could not be armed. When `Some` and
    /// the parent is still live with `chain_len < max_chain_len`, a seal
    /// captures via `snapshot_derive` over the drained dirty set; on **any**
    /// doubt it falls back to `snapshot_base` (the safety rule: the dirty set is
    /// a cost hint, never a correctness input — a seal never fails because the
    /// optimization was unavailable).
    derive_parent: Option<SnapshotId>,
    /// Store image that the live VM's tracked dirty window continues from.
    /// Cleared whenever the VM is replaced, the image handle is dropped, or
    /// dirty tracking cannot prove the window complete.
    current_image: Option<SnapshotId>,
    /// Host-only evidence: number of requested in-place restores that used the
    /// existing fresh-VM path after an in-place prerequisite or restore failed.
    in_place_fallbacks: u64,
    /// Host-only evidence: bytes patched into RAM by the most recent successful
    /// in-place restore (zero for a fresh-VM restore or an empty patch).
    last_restore_bytes_written: u64,
    /// Wire [`SnapId`] → store [`SnapshotId`]. Wire handles are minted here,
    /// monotonically; a dropped handle is removed (using it again is a loud
    /// [`ControlError::UnknownSnapshot`]).
    snaps: BTreeMap<u64, SnapshotId>,
    next_snap: u64,
    hello_done: bool,
    /// The **staged host-fault schedule** (task 59): **one fault per [`Moment`]**,
    /// ordered. Populated by [`Request::Perturb`] and by a [`Request::Branch`]
    /// whose env carries host overrides; **drained** by [`ControlServer::run`] as
    /// each `Moment` is reached (a re-run rewinds via `branch`/`replay`, which
    /// re-stages).
    ///
    /// **One fault per `Moment`.** Task 45's [`EnvSpec`] override map is
    /// `BTreeMap<Moment, Action>` — one action per `Moment` — so a second
    /// same-`Moment` fault cannot be recorded without losing the first
    /// (a non-reproducing reproducer). The frontier therefore **loudly rejects** a
    /// second same-`Moment` stage ([`ControlError::PerturbMomentTaken`]), keeping
    /// every emitted reproducer exact. (The one-fault-per-`Moment` rule is the
    /// integrator's final ruling — spec amendment PR #54.) A `BTreeMap` so no
    /// insertion order can reach the apply sequence.
    schedule: BTreeMap<u64, Effect>,
    /// The **staged reseed schedule** (task 78): the branch env's reseed markers
    /// strictly beyond the restore floor, ordered. A marker-carrying env's
    /// collapsed-hop reseeds are re-executed at their recorded `Moment`s by
    /// [`run`](ControlServer::run) (the exit-boundary discipline of the task-59
    /// plane) — the ruled fix for the sequential-entropy splice: a compose-folded
    /// env replays each hop's reseed at its position instead of reseeding once at
    /// the fold's root. A reseed staged beyond the trajectory is the same loud
    /// [`ControlError::ScheduleUnsatisfiable`] class as a crossed fault. At a
    /// `Moment` shared with a staged host fault the reseed applies **first**
    /// (fixed order, so the apply sequence is deterministic; the recorded
    /// tables are disjoint, so replay preserves it).
    reseed_schedule: BTreeMap<u64, u64>,
    /// The **active recorded reproducer** (task 59 requirement 3): every applied
    /// host fault is stamped here via task-45's [`EnvSpec::record_effect`], so the env
    /// [`recorded_env`](ControlServer::recorded_env) returns replays to the
    /// identical `state_hash` (the record → replay closure). With one fault per
    /// `Moment` (above) the stamping is exact — no fault is ever lost. Its seed is
    /// set by the most recent [`Request::Branch`] (default `0` before any branch);
    /// preserved by replay; a branch starts a new recorded future.
    recorded: EnvSpec,
    /// A latched schedule failure prevents a later run from applying a crossed
    /// input from the past. Snapshots retain the failure and its pending plan;
    /// replay preserves it, while branching selects a new plan.
    schedule_poisoned: Option<ScheduleFailure>,
    /// The task-73 **SDK channel snapshots**, keyed by wire [`SnapId`]: the
    /// replay-relevant SDK state (seeded stream position + emitted event log)
    /// captured when a snapshot is sealed, so a `branch`/`replay` from a mid-run
    /// SDK snapshot reproduces (its seeded streams continue from the right
    /// position) and keeps the declared catalog the never-fired report needs.
    /// Removed with its snapshot on `drop`; ephemeral pool state, like the
    /// snapshot handles themselves.
    sdk_snaps: BTreeMap<u64, SdkSnap>,
    /// **The lineage taint bit** for the *current live timeline* (task 81). Set the
    /// instant an [`Request::Exec`] improvisation is issued (conservatively, before
    /// it runs — so no failure mode leaves an improvised timeline looking clean),
    /// and **re-derived on every restore** from the branched/replayed snapshot's
    /// taint ([`tainted_snaps`](Self::tainted_snaps)). Taint never clears downstream:
    /// an untainted timeline is reachable only by restoring an untainted ancestor
    /// (or the fresh boot after a `RestoreFailed`). Gates the reproducer mint
    /// ([`Request::RecordedEnv`] → [`ControlError::Tainted`]) and stamps the
    /// snapshot reply.
    timeline_tainted: bool,
    /// **The set of tainted snapshots** (task 81), keyed by wire [`SnapId`]. A
    /// [`snapshot`](ControlServer::snapshot) taken from a tainted timeline records
    /// its handle here (and its reply carries `tainted: true`); a `branch`/`replay`
    /// of a handle in this set yields a tainted timeline. This is the durable half
    /// of the taint guard — it survives across timelines so the taint propagates
    /// **exactly along snapshot ancestry**, and a future Archive/donation path
    /// (task 64+) can consult a snapshot's `tainted` flag without a session.
    /// Removed with its handle on `drop`. A `BTreeSet` so membership order never
    /// reaches an output.
    tainted_snaps: BTreeSet<u64>,
    /// Immutable evidence and policy captured at each seal. Unlike the wire
    /// reply, this side table also retains the immediate state hash and policy
    /// required to export the complete replay state later in the session.
    snapshot_meta: BTreeMap<u64, SnapshotMeta>,
    /// A monotonically-increasing counter salting each [`Request::Exec`]'s
    /// completion-sentinel marker ([`crate::exec::ExecSession`]), so two `exec`s on one session
    /// cannot alias their sentinels. Not wall-clock / RNG (conventions rule 4);
    /// `exec` is off the record, so this never needs to be reproducible — only
    /// unique-enough within a session.
    exec_nonce: u64,
    /// Physical command continuation, preserved independently of host input plans.
    retained_exec: Option<RetainedExec>,
    /// Completed restore-delimited production-trace segments. The current
    /// live VMM's segment is appended only in the read-only
    /// [`session_virtual_time_trace`](Self::session_virtual_time_trace) view.
    session_trace: Vec<SessionTraceSegment>,
    /// Boundary that began the current live VMM's trace segment.
    session_trace_start: SessionTraceStart,
    /// Dirty main-RAM GFNs drained by the most recent seal, retained solely
    /// for out-of-band profiling and never included in deterministic state.
    last_seal_dirty_gfns: Option<Vec<u64>>,
}

/// The per-snapshot SDK state the control server retains (task 73): the VM-level
/// channel snapshot (seeded stream position + event log) **and** the
/// [`ServiceConfig`] active when the snapshot was sealed. The configuration is captured
/// because [`reset_schedule_to_fresh_vm`](ControlServer::reset_schedule_to_fresh_vm)
/// resets the recorded reproducer to `none()` on every restore — so a **replay**
/// must restore this configuration before materializing the SDK environment.
#[derive(Clone)]
struct SdkSnap {
    channel: SdkSnapshot,
    policy: ServiceConfig,
}

#[derive(Clone)]
struct SnapshotMeta {
    at: u64,
    sdk_events: u64,
    trace_events: u64,
    trace_schedules: u64,
    tainted: bool,
    /// Lazily computed whole-state hash. A local seal leaves this absent;
    /// imported portable artifacts carry the hash embedded by their source.
    state_hash: Option<[u8; 32]>,
    /// Canonical state-blob bytes following the large `MEM\0` chunk, captured
    /// at the same stopped boundary as this metadata.
    state_blob_suffix: Vec<u8>,
    policy: ServiceConfig,
    /// Complete captured control-plane state; empty only for legacy imports.
    control_state: Vec<u8>,
}

/// A sparse sidecar carries both replay state and its seal-time hash preimage.
/// The final control chunk must describe the same state in both representations.
fn validate_control_hash_suffix(
    mut suffix: &[u8],
    control: &[u8],
) -> Result<(), PortableSnapshotError> {
    let malformed = || PortableSnapshotError::Malformed("sparse control hash preimage");
    while !suffix.is_empty() {
        let header = suffix.get(..12).ok_or_else(malformed)?;
        let length = u64::from_le_bytes(header[4..12].try_into().map_err(|_| malformed())?);
        let length = usize::try_from(length).map_err(|_| malformed())?;
        let end = 12_usize.checked_add(length).ok_or_else(malformed)?;
        let payload = suffix.get(12..end).ok_or_else(malformed)?;
        let tail = &suffix[end..];
        if &header[..4] == b"CPLN" {
            return if !control.is_empty() && payload == control && tail.is_empty() {
                Ok(())
            } else {
                Err(malformed())
            };
        }
        suffix = tail;
    }
    if control.is_empty() {
        Ok(()) // legacy sidecars predate the control chunk
    } else {
        Err(malformed())
    }
}

/// Hash the exact canonical `MEM\0` chunk followed by its seal-time suffix.
///
/// RAM is streamed directly into SHA-256, avoiding a second full-image copy
/// while preserving the frozen whole-state hash preimage byte for byte.
fn hash_state_blob_parts(memory: &[u8], suffix: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"MEM\0");
    hasher.update((memory.len() as u64).to_le_bytes());
    hasher.update(memory);
    hasher.update(suffix);
    hasher.finalize().into()
}

fn reseed_marker_requires_arrival(marker: u64, restored_floor: u64) -> bool {
    marker > restored_floor
}

impl<B: Backend<A: Vendor>> ControlServer<B> {
    /// Build a server around a live VM. The [`SnapshotEngine`] is sized to the
    /// VM's guest-memory image; `factory` boots the fresh restore target for
    /// every `branch`/`replay` and must compose its VMs exactly like `vmm`
    /// (same RAM size, wiring, and contract — a mismatch is caught fail-closed
    /// by [`Vmm::restore_vm_state`] at the first restore).
    pub fn new(mut vmm: Vmm<B>, factory: VmmFactory<B>) -> Self {
        let engine = SnapshotEngine::new(vmm.guest_memory().len());
        // Seed the recorded reproducer from the **live VM's actual entropy stream**
        // (not a bare `0`), so `recorded_env()` reproduces even for a session that
        // runs before its first `branch`/`replay` — a reproducer branched from the
        // starting snapshot then reseeds to this same stream.
        let seed = vmm.entropy_state().unwrap_or(0);
        let recorded = EnvSpec::seeded(seed);
        // Task 73: wire the SDK channel on the **live VM too** (not only restore
        // targets), so an SDK guest that rings the hypercall doorbell BEFORE its
        // first `branch`/`replay` is serviced — we advertise `GUEST_HAS_SDK`
        // unconditionally, so the capability must be honored from construction.
        // Inert (and unhashed) for a non-SDK guest.
        vmm.enable_sdk(
            RecordedEnv::new(seed, Box::new(NominalHandler) as Box<dyn ServiceHandler>),
            recorded.config(),
        );
        ControlServer {
            vmm: Some(vmm),
            factory,
            service_factory: nominal_factory(),
            remap_factory: None,
            // Truthful default: with no remap factory, memcpy is the only
            // path this server can take; `set_remap_factory` flips to `Remap`.
            restore_mode: RestoreMode::Memcpy,
            engine,
            // A fresh boot is no snapshot's continuation: the first seal is a base.
            derive_parent: None,
            current_image: None,
            in_place_fallbacks: 0,
            last_restore_bytes_written: 0,
            snaps: BTreeMap::new(),
            next_snap: 1,
            hello_done: false,
            schedule: BTreeMap::new(),
            reseed_schedule: BTreeMap::new(),
            recorded: EnvSpec::seeded(seed),
            schedule_poisoned: None,
            sdk_snaps: BTreeMap::new(),
            // A fresh session's live timeline is untainted; no snapshots exist yet.
            timeline_tainted: false,
            tainted_snaps: BTreeSet::new(),
            snapshot_meta: BTreeMap::new(),
            exec_nonce: 0,
            retained_exec: None,
            session_trace: Vec::new(),
            session_trace_start: SessionTraceStart::InitialBoot,
            last_seal_dirty_gfns: None,
        }
    }

    /// Install the workload composition's service implementation resolver.
    /// Existing snapshots keep their recorded identity and configuration.
    pub fn set_service_factory(&mut self, factory: ServiceFactory) {
        self.service_factory = factory;
    }

    /// The **active recorded reproducer** (task 59): the [`EnvSpec`] every applied
    /// host fault has been stamped into, in `Moment` order. Replaying/branching
    /// this env re-applies the identical schedule at the identical counts, so it
    /// reproduces the run's `state_hash` bit-for-bit (the record → replay
    /// closure). Empty (a bare `Seeded`) until a fault is applied.
    pub fn recorded_env(&self) -> &EnvSpec {
        &self.recorded
    }

    /// Read-only access to the live VM (e.g. for a composition root that wants
    /// the serial capture after a session ends). `None` after a fatal error.
    pub fn vmm(&self) -> Option<&Vmm<B>> {
        self.vmm.as_ref()
    }

    /// Mutable access to the live VM for host-side evidence settings a
    /// composition root chooses before its first run, such as
    /// [`Vmm::defer_virtual_time_checkpoint_hashes`]. Restores replace the VM,
    /// so a setting applied here covers only the current one. `None` after a
    /// fatal error.
    pub fn vmm_mut(&mut self) -> Option<&mut Vmm<B>> {
        self.vmm.as_mut()
    }

    /// Complete restore-aware normalized trace for this control session.
    ///
    /// Each branch/replay replacement closes one segment. The current live
    /// VMM is captured into the returned owned view without mutating the
    /// server, so callers can write final evidence after [`Self::serve`]
    /// returns. `None` means no VMM in the session had virtual_time tracing.
    pub fn session_virtual_time_trace(&self) -> Option<SessionVirtualTimeTrace> {
        let mut segments = self.session_trace.clone();
        if let Some(trace) = self.vmm.as_ref().and_then(Vmm::virtual_time_trace) {
            segments.push(SessionTraceSegment::capture(
                self.session_trace_start,
                trace,
            ));
        }
        (!segments.is_empty()).then(|| SessionVirtualTimeTrace::from_segments(segments))
    }

    /// Move the accumulated session trace out of the server and append an owned
    /// capture of the current live segment.
    ///
    /// Composition roots use this after [`Self::serve`] returns so a large
    /// campaign is not duplicated in memory while its evidence file is written.
    /// The live VMM and all determinism-relevant state are unchanged; only the
    /// host-side completed-segment evidence buffer is drained.
    pub fn take_session_virtual_time_trace(&mut self) -> Option<SessionVirtualTimeTrace> {
        let mut segments = std::mem::take(&mut self.session_trace);
        if let Some(trace) = self.vmm.as_ref().and_then(Vmm::virtual_time_trace) {
            segments.push(SessionTraceSegment::capture(
                self.session_trace_start,
                trace,
            ));
        }
        (!segments.is_empty()).then(|| SessionVirtualTimeTrace::from_segments(segments))
    }

    /// Close the current VMM's host-only trace immediately before a restore.
    /// Taking the trace resets its live buffer, so this also preserves segment
    /// boundaries when the restore keeps the same VMM in place.
    fn finish_session_trace_segment(&mut self) {
        if let Some(trace) = self.vmm.as_mut().and_then(Vmm::take_virtual_time_trace) {
            self.session_trace.push(SessionTraceSegment::capture(
                self.session_trace_start,
                &trace,
            ));
        }
    }

    /// Install the remap-restore factory (task 95 M2.2) **and switch the
    /// restore mode to [`RestoreMode::Remap`]** — installing the capability is
    /// the opt-in, so remap becomes the default exactly where it is possible
    /// (a factory-less server stays truthfully on `Memcpy`; PR #95 round-1).
    /// Every `branch`/`replay` then builds the fresh VM **around** the
    /// materialized mapping instead of memcpying the image into a fresh
    /// allocation; [`Self::set_restore_mode`] can still flip back for A/B. The
    /// factory must mirror the session's [`VmmFactory`] composition (RAM size,
    /// wiring, contract) minus the boot-image load.
    pub fn set_remap_factory(&mut self, factory: RemapVmmFactory<B>) {
        self.remap_factory = Some(factory);
        self.restore_mode = RestoreMode::Remap;
    }

    /// Flip the restore-mode A/B knob. [`RestoreMode::Remap`] requires an
    /// installed [`RemapVmmFactory`]; [`RestoreMode::InPlace`] uses the live VM
    /// and falls back to a fresh target when a prerequisite is unavailable.
    pub fn set_restore_mode(&mut self, mode: RestoreMode) {
        self.restore_mode = mode;
    }

    /// The active restore mode (informational; see [`RestoreMode`]).
    pub fn restore_mode(&self) -> RestoreMode {
        self.restore_mode
    }

    /// Number of in-place restore attempts that fell back to a fresh VM.
    pub fn in_place_fallbacks(&self) -> u64 {
        self.in_place_fallbacks
    }

    /// Bytes patched by the most recent successful in-place restore.
    pub fn last_restore_bytes_written(&self) -> u64 {
        self.last_restore_bytes_written
    }

    /// Tune the engine's derive-chain bound (task 95 M2.1; see
    /// [`SnapshotEngine::set_max_chain_len`]).
    pub fn set_max_chain_len(&mut self, max_chain_len: u32) {
        self.engine.set_max_chain_len(max_chain_len);
    }

    /// The store-side derive-chain length behind a wire handle (`1` = a base
    /// layer, `> 1` = a dirty-set derive; task 95 M2.1) — gate evidence and
    /// diagnostics for which capture path a seal took. `None` for an unknown or
    /// dropped handle. Read-only; not a wire verb.
    pub fn snapshot_chain_len(&self, snap: SnapId) -> Option<u32> {
        let id = self.snaps.get(&snap.0)?;
        self.engine.stats(*id).ok().map(|s| s.chain_len)
    }

    /// Store-wide snapshot accounting — how many layers are live and how much
    /// the store keeps resident. Read-only; not a wire verb.
    ///
    /// The `Drop` verb's obligation is that dropping a handle **actually
    /// releases the state**, not merely forgets the handle. That is only
    /// checkable against the store's own accounting, so the protocol tests read
    /// it here (`docs/TESTING.md`).
    pub fn snapshot_store_stats(&self) -> snapshot_store::StoreStats {
        self.engine.store_stats()
    }

    /// Statistics for one live snapshot, including its non-zero owned pages.
    /// This is a read-only, out-of-band profiling accessor; it does not
    /// participate in the control protocol or deterministic state.
    pub fn snapshot_stats(&self, snap: SnapId) -> Option<snapshot_store::SnapStats> {
        self.snaps
            .get(&snap.0)
            .and_then(|id| self.engine.stats(*id).ok())
    }

    /// Dirty main-RAM page GFNs drained by the most recent successful seal.
    /// `None` means that seal used a full-image/base capture and had no
    /// drainable dirty window. GFNs are relative to the VMM's main-RAM image,
    /// matching [`Vmm::drain_dirty_pages`]. This evidence is never hashed or
    /// serialized.
    pub fn last_seal_dirty_gfns(&self) -> Option<&[u64]> {
        self.last_seal_dirty_gfns.as_deref()
    }

    /// Most recently minted live snapshot handle, if any. This is an
    /// out-of-band composition-root convenience for exporting the final
    /// midpoint selected by a one-shot portability driver; wire clients still
    /// address snapshots only by explicit handles.
    pub fn latest_snapshot(&self) -> Option<SnapId> {
        self.snaps.last_key_value().map(|(&id, _)| SnapId(id))
    }

    /// Export a target snapshot as an in-process sparse portable value.
    ///
    /// base and target must be live handles in this server. The page list
    /// is resolved against target by the snapshot store and contains only
    /// pages that may differ from base; the sidecar carries the exact
    /// non-memory replay state. This path never materializes RAM and never
    /// computes a whole-state hash.
    pub fn export_sparse_snapshot(
        &self,
        base: SnapId,
        target: SnapId,
    ) -> Result<SparsePortableSnapshot, PortableSnapshotError> {
        let base_id = *self
            .snaps
            .get(&base.0)
            .ok_or(PortableSnapshotError::UnknownSnapshot(base.0))?;
        let target_id = *self
            .snaps
            .get(&target.0)
            .ok_or(PortableSnapshotError::UnknownSnapshot(target.0))?;
        let meta = self
            .snapshot_meta
            .get(&target.0)
            .ok_or(PortableSnapshotError::UnknownSnapshot(target.0))?;
        // A complete portable import authenticates its whole-state hash but
        // does not retain the source's canonical post-MEM state-blob suffix.
        // Re-exporting it sparsely would discard that hash and leave a later
        // importer unable to reconstruct the correct whole-state preimage.
        if meta.state_blob_suffix.is_empty() {
            return Err(PortableSnapshotError::Malformed(
                "sparse export target lacks a canonical state-blob suffix",
            ));
        }
        let candidates = self.engine.diff_pages(Some(base_id), target_id)?;
        let mut pages = Vec::with_capacity(candidates.len());
        for (gfn, page) in candidates {
            // Item 2 returns the target-resolved union of potentially
            // changed keys. Compare each candidate with the live base so
            // a base-to-base export remains genuinely sparse even when
            // both layers independently recorded the same non-zero page.
            if self.engine.read_page(base_id, gfn)? != page {
                pages.push((gfn, Arc::new(page)));
            }
        }
        let vm_state = self.engine.vm_state_bytes(target_id)?;
        let sidecar = encode_sparse_sidecar(&SparsePortableSidecarRef {
            vm_state,
            sdk: self.sdk_snaps.get(&target.0).map(|s| &s.channel),
            policy: &meta.policy,
            control_state: &meta.control_state,
            at: meta.at,
            sdk_events: meta.sdk_events,
            trace_events: meta.trace_events,
            trace_schedules: meta.trace_schedules,
            tainted: meta.tainted,
            state_blob_suffix: &meta.state_blob_suffix,
        })?;
        Ok(SparsePortableSnapshot { pages, sidecar })
    }

    /// Import an in-process sparse portable value as a child of base.
    ///
    /// The sidecar, vendor VM state, and complete page list are validated
    /// before the store derives a layer or a wire handle is minted. The
    /// resulting layer therefore retains base as its ancestor and costs
    /// O(number of supplied pages), with no full-RAM materialization.
    pub fn import_sparse_snapshot(
        &mut self,
        base: SnapId,
        portable: SparsePortableSnapshot,
    ) -> Result<SparsePortableSnapshotReceipt, PortableSnapshotError> {
        self.import_sparse_snapshot_parts(base, &portable.pages, &portable.sidecar)
    }

    /// Import sparse pages and their opaque sidecar without taking ownership.
    ///
    /// This is the parts-oriented form used by callers that keep page buffers
    /// in a worker-owned cache. Validation is atomic: an invalid GFN ordering,
    /// sidecar, or VM state leaves both the store and handle tables unchanged.
    pub fn import_sparse_snapshot_parts(
        &mut self,
        base: SnapId,
        pages: &[(u64, Arc<[u8; 4096]>)],
        sidecar: &[u8],
    ) -> Result<SparsePortableSnapshotReceipt, PortableSnapshotError> {
        let base_id = *self
            .snaps
            .get(&base.0)
            .ok_or(PortableSnapshotError::UnknownSnapshot(base.0))?;
        let mut previous = None;
        for (gfn, _) in pages {
            if let Some(error) = sparse_page_order_error(previous, *gfn) {
                return Err(PortableSnapshotError::Snapshot(error));
            }
            if *gfn >= self.engine.mem_pages() {
                return Err(PortableSnapshotError::Snapshot(
                    SnapshotError::SparsePageOutOfRange {
                        gfn: *gfn,
                        pages: self.engine.mem_pages(),
                    },
                ));
            }
            previous = Some(*gfn);
        }

        let portable = decode_sparse_sidecar(sidecar)?;
        let decoded = <<B::A as Vendor>::Snapshot as SnapshotRecords>::decode(&portable.vm_state)
            .map_err(SnapshotError::from)?;
        if decoded.vtime().snapshot_vns != portable.at {
            return Err(PortableSnapshotError::Malformed(
                "sparse sidecar V-time does not match vendor VM state",
            ));
        }
        if let Some(sdk) = &portable.sdk {
            if usize::try_from(portable.sdk_events).ok() != Some(sdk.events.len()) {
                return Err(PortableSnapshotError::Malformed(
                    "sparse sidecar SDK event count",
                ));
            }
        } else if portable.sdk_events != 0 {
            return Err(PortableSnapshotError::Malformed(
                "sparse sidecar SDK event count",
            ));
        }
        ControlState::decode_for_snapshot(
            &portable.control_state,
            &portable.policy,
            portable.tainted,
        )
        .map_err(PortableSnapshotError::Malformed)?;
        validate_control_hash_suffix(&portable.state_blob_suffix, &portable.control_state)?;
        let id = self.next_snap;
        let next_snap = self
            .next_snap
            .checked_add(1)
            .ok_or(PortableSnapshotError::Malformed("snapshot handle overflow"))?;
        let mut owned_pages = Vec::new();
        owned_pages
            .try_reserve_exact(pages.len())
            .map_err(|_| PortableSnapshotError::Malformed("sparse page allocation"))?;
        for (gfn, page) in pages {
            owned_pages.push((*gfn, **page));
        }
        let store_id =
            self.engine
                .snapshot_sparse_derive(base_id, &owned_pages, &portable.vm_state)?;

        self.next_snap = next_snap;
        self.snaps.insert(id, store_id);
        if let Some(channel) = portable.sdk {
            self.sdk_snaps.insert(
                id,
                SdkSnap {
                    channel,
                    policy: portable.policy.clone(),
                },
            );
        }
        if portable.tainted {
            self.tainted_snaps.insert(id);
        }
        self.snapshot_meta.insert(
            id,
            SnapshotMeta {
                at: portable.at,
                sdk_events: portable.sdk_events,
                trace_events: portable.trace_events,
                trace_schedules: portable.trace_schedules,
                tainted: portable.tainted,
                state_hash: None,
                state_blob_suffix: portable.state_blob_suffix,
                policy: portable.policy,
                control_state: portable.control_state,
            },
        );
        Ok(SparsePortableSnapshotReceipt {
            id: SnapId(id),
            at: Moment(decoded.vtime().snapshot_vns),
            sdk_events: self
                .snapshot_meta
                .get(&id)
                .map_or(0, |meta| meta.sdk_events),
            trace_events: self
                .snapshot_meta
                .get(&id)
                .map_or(0, |meta| meta.trace_events),
            trace_schedules: self
                .snapshot_meta
                .get(&id)
                .map_or(0, |meta| meta.trace_schedules),
            tainted: self.tainted_snaps.contains(&id),
        })
    }

    /// Export one session-local snapshot as a complete host-neutral artifact.
    ///
    /// The artifact contains materialized RAM, the exact canonical vendor
    /// VM-state bytes, SDK stream and remaining payload suffix, service
    /// configuration, taint, seal cut, and source whole-state hash. It is
    /// streamed in a fixed order and closed by a SHA-256 digest. No live-VM
    /// state is read, so exporting after the source has continued cannot
    /// accidentally capture a mixed-time artifact.
    pub fn export_portable_snapshot<W: Write>(
        &self,
        snap: SnapId,
        writer: W,
    ) -> Result<PortableSnapshotReceipt, PortableSnapshotError> {
        let store_id = *self
            .snaps
            .get(&snap.0)
            .ok_or(PortableSnapshotError::UnknownSnapshot(snap.0))?;
        let meta = self
            .snapshot_meta
            .get(&snap.0)
            .ok_or(PortableSnapshotError::UnknownSnapshot(snap.0))?;
        let memory = self.engine.materialize(store_id)?;
        let vm_state = self.engine.vm_state_bytes(store_id)?;
        let state_hash = meta
            .state_hash
            .unwrap_or_else(|| hash_state_blob_parts(memory.as_slice(), &meta.state_blob_suffix));
        PortableSnapshotRef {
            memory: memory.as_slice(),
            vm_state,
            sdk: self.sdk_snaps.get(&snap.0).map(|s| &s.channel),
            policy: &meta.policy,
            control_state: &meta.control_state,
            at: meta.at,
            sdk_events: meta.sdk_events,
            trace_events: meta.trace_events,
            trace_schedules: meta.trace_schedules,
            tainted: meta.tainted,
            state_hash,
        }
        .write_to(writer)?;
        Ok(PortableSnapshotReceipt {
            id: snap,
            at: Moment(meta.at),
            sdk_events: meta.sdk_events,
            trace_events: meta.trace_events,
            trace_schedules: meta.trace_schedules,
            tainted: meta.tainted,
            state_hash,
        })
    }

    /// Import a complete host-neutral artifact as a fresh base snapshot.
    ///
    /// Import verifies the artifact digest, every bounded nested codec, the
    /// exact configured RAM size, and the destination vendor's VM-state codec
    /// before minting a handle. The VM is not replaced; a subsequent ordinary
    /// `Replay` uses the server's selected restore mode exactly like any local
    /// snapshot.
    pub fn import_portable_snapshot<R: Read>(
        &mut self,
        reader: R,
    ) -> Result<PortableSnapshotReceipt, PortableSnapshotError> {
        let expected_memory_len = usize::try_from(
            self.engine
                .mem_pages()
                .checked_mul(snapshot_store::PAGE_SIZE as u64)
                .ok_or(PortableSnapshotError::Malformed(
                    "configured memory length overflow",
                ))?,
        )
        .map_err(|_| PortableSnapshotError::Malformed("configured memory length"))?;
        let portable = PortableSnapshot::read_from(reader, expected_memory_len)?;
        let decoded = <<B::A as Vendor>::Snapshot as SnapshotRecords>::decode(&portable.vm_state)
            .map_err(SnapshotError::from)?;
        if decoded.vtime().snapshot_vns != portable.at {
            return Err(PortableSnapshotError::Malformed(
                "portable V-time does not match vendor VM state",
            ));
        }
        if portable.sdk.as_ref().map_or(0, |sdk| sdk.events.len()) as u64 != portable.sdk_events {
            return Err(PortableSnapshotError::Malformed("portable SDK event count"));
        }
        ControlState::decode_for_snapshot(
            &portable.control_state,
            &portable.policy,
            portable.tainted,
        )
        .map_err(PortableSnapshotError::Malformed)?;
        let id = self.next_snap;
        let next_snap = self
            .next_snap
            .checked_add(1)
            .ok_or(PortableSnapshotError::Malformed("snapshot handle overflow"))?;
        let store_id = self
            .engine
            .snapshot_base(&portable.memory, &portable.vm_state)?;
        self.next_snap = next_snap;
        self.snaps.insert(id, store_id);
        if let Some(channel) = portable.sdk {
            self.sdk_snaps.insert(
                id,
                SdkSnap {
                    channel,
                    policy: portable.policy.clone(),
                },
            );
        }
        if portable.tainted {
            self.tainted_snaps.insert(id);
        }
        self.snapshot_meta.insert(
            id,
            SnapshotMeta {
                at: portable.at,
                sdk_events: portable.sdk_events,
                trace_events: portable.trace_events,
                trace_schedules: portable.trace_schedules,
                tainted: portable.tainted,
                state_hash: Some(portable.state_hash),
                state_blob_suffix: Vec::new(),
                policy: portable.policy,
                control_state: portable.control_state,
            },
        );
        Ok(PortableSnapshotReceipt {
            id: SnapId(id),
            at: Moment(portable.at),
            sdk_events: portable.sdk_events,
            trace_events: portable.trace_events,
            trace_schedules: portable.trace_schedules,
            tainted: portable.tainted,
            state_hash: portable.state_hash,
        })
    }

    /// Serve one session over a byte stream (a connected unix socket, or an
    /// in-process socketpair end): decode request frames, dispatch each through
    /// [`ControlServer::handle`], and write the reply frames back, until the
    /// peer closes the stream (EOF between frames → `Ok`). Any [`ServeError`]
    /// is returned immediately — the caller drops the stream, which the peer
    /// observes as a torn session (fail-loud).
    pub fn serve<S: Read + Write>(&mut self, mut stream: S) -> Result<(), ServeError> {
        let mut inbuf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut outbuf: Vec<u8> = Vec::new();
        loop {
            // Drain every complete frame currently buffered.
            while let Some((seq, req, consumed)) = decode_request(&inbuf)? {
                inbuf.drain(..consumed);
                let reply = self.handle(&req)?;
                outbuf.clear();
                encode_reply(seq, &reply, &mut outbuf)?;
                stream.write_all(&outbuf)?;
                stream.flush()?;
            }
            let n = stream.read(&mut chunk)?;
            if n == 0 {
                // Clean end iff the peer closed between frames.
                return if inbuf.is_empty() {
                    Ok(())
                } else {
                    Err(control_proto::ProtocolError::ShortFrame.into())
                };
            }
            inbuf.extend_from_slice(&chunk[..n]);
        }
    }

    /// Dispatch one verb. The nested result keeps the two categories apart:
    /// the outer `Err` is a session-fatal [`ServeError`]; the inner
    /// `Result<Reply, ControlError>` is what goes on the wire (both arms are
    /// encoded as reply frames). Public so composition roots and tests can
    /// drive the dispatch directly, without a socket.
    #[allow(clippy::result_large_err)] // ServeError's size is irrelevant on this cold path
    pub fn handle(&mut self, req: &Request) -> Result<Result<Reply, ControlError>, ServeError> {
        // `hello` must be the first verb of a session (task 25): before it, no
        // capability has been negotiated, so nothing is supported.
        if !self.hello_done && !matches!(req, Request::Hello(_)) {
            return Ok(Err(ControlError::Unsupported));
        }
        match req {
            Request::Hello(client_caps) => {
                // Application semantics are not backward compatible across
                // protocol versions. Reject during the handshake and do not
                // open the session; discovering (for example) a READ_CAP
                // disagreement on a later Read would be too late.
                if client_caps.protocol_version != control_proto::APP_PROTOCOL_VERSION {
                    return Ok(Err(ControlError::Unsupported));
                }
                self.hello_done = true;
                Ok(Ok(Reply::Hello(server_caps())))
            }
            Request::Snapshot => self.snapshot(),
            Request::Drop(snap) => Ok(self.drop_snap(*snap)),
            Request::Branch { snap, env } => self.restore(*snap, Some(env)),
            Request::Replay(snap) => self.restore(*snap, None),
            Request::Run { until, resolve } => {
                if let Some(resolution) = resolve {
                    let Some((at, question)) = self
                        .vmm
                        .as_ref()
                        .ok_or(ServeError::Poisoned)?
                        .pending_service_question()
                        .map(|(at, question)| (at, question.clone()))
                    else {
                        return Ok(Err(ControlError::ResolveWithoutDecision));
                    };
                    if resolution.vtime.0 != at
                        || resolution.service != question.service()
                        || resolution.id.0 != question.request_id()
                    {
                        return Ok(Err(ControlError::ResolveWithoutDecision));
                    }
                    // Reject the hypercall wire limit before decoding, so an
                    // untrusted control frame cannot first allocate the larger
                    // environment-channel bound and only then be rejected.
                    if resolution.answer.0.len() > hypercall_proto::MAX_PAYLOAD {
                        return Ok(Err(ControlError::MalformedEnvironment));
                    }
                    let response = match environment::channel::Answer::decode(&resolution.answer.0)
                    {
                        Ok(response) => response,
                        Err(_) => return Ok(Err(ControlError::MalformedEnvironment)),
                    };
                    let mut recorded = self.recorded.clone();
                    recorded.record_answer(
                        at,
                        question.service(),
                        question.request_id(),
                        response.clone(),
                    )?;
                    if recorded.try_encode().is_err() {
                        return Ok(Err(ControlError::MalformedEnvironment));
                    }
                    // A response-page write is a substrate failure. Poison the
                    // live VM before returning so a direct caller cannot retry
                    // against state whose guest re-entry contract is unknown.
                    if let Err(error) = self
                        .vmm
                        .as_mut()
                        .ok_or(ServeError::Poisoned)?
                        .resolve_service_answer(response)
                    {
                        self.vmm = None;
                        return Err(error.into());
                    }
                    self.recorded = recorded;
                }
                self.run(until)
            }
            Request::Hash { scope } => match scope {
                HashScope::Whole => {
                    let vmm = self.vmm.as_ref().ok_or(ServeError::Poisoned)?;
                    let mut suffix = vmm.state_blob_suffix()?;
                    self.capture_control_state().append_hash(&mut suffix);
                    Ok(Ok(Reply::Hash(hash_state_blob_parts(
                        vmm.guest_memory(),
                        &suffix,
                    ))))
                }
                // No disk device exists and region hashing has no consumer;
                // unsupported is loud and distinct from a malformed frame.
                HashScope::Disk | HashScope::Region { .. } => Ok(Err(ControlError::Unsupported)),
            },
            // Host-plane enforcement (task 59): decode + validate the fault,
            // stage it at its `Moment`. `run` applies it there and stamps it into
            // the recorded env.
            Request::Perturb { fault, at } => Ok(self.perturb(fault, *at)),
            // Task 73: surface the link-tier SDK event capture over the wire, so a
            // remote `SocketMachine` client can fill `RunTrace.events` (a socket
            // client cannot see the server-side `Vmm::sdk_events` capture directly).
            // **Paged** (round-5 P4): a page starts at `offset` and is bounded to the
            // control frame limit, so an arbitrarily long capture never overflows a
            // single reply — the client pages until an empty reply. Empty for a guest
            // with no SDK, or once `offset` reaches the end.
            Request::SdkEvents { offset } => {
                let vmm = self.vmm.as_ref().ok_or(ServeError::Poisoned)?;
                Ok(Ok(Reply::SdkEvents(page_sdk_events(
                    vmm.sdk_events(),
                    *offset as usize,
                ))))
            }
            // Task 69: surface the guest **console** (serial) capture over the wire,
            // so a remote `SocketMachine` client can fill `RunTrace.records` — the
            // scrape channel the task-67 log-template signal reads (a socket client
            // cannot see the server-side `Vmm::serial` capture directly, exactly as
            // `SdkEvents` surfaces the SDK capture). This is a **pure read**: it
            // reads the uart capture buffer and never advances the VM or touches any
            // hashable state, so a run's `state_hash` is identical whether or not a
            // client drains the console — the determinism-neutrality the signal
            // channel requires. **Paged** like `SdkEvents`: `total` is the full
            // capture length and `chunk` is `serial[offset..]` bounded to the frame
            // limit, so the client pages `offset..total` until drained. Unlike
            // `SdkEvents` it returns an empty capture (not `Poisoned`) when there is
            // no live VM — a drained console off a torn-down VM is defined as empty.
            Request::Console { offset } => {
                let serial = self.vmm.as_ref().map(|v| v.serial()).unwrap_or(&[]);
                let (total, chunk) = page_console(serial, *offset as usize);
                Ok(Ok(Reply::Console { total, chunk }))
            }
            // Observation verbs (task 80): `read`/`regs` look at guest state without
            // moving it. Both borrow `self.vmm` immutably and touch nothing else —
            // not the schedule, not `recorded`, not V-time — so `hash(Whole)` before
            // and after any sequence of them is bit-identical, and neither is ever
            // recorded into an `Reproducer` (the docs/PROTOCOL.md search-surface
            // criterion: observation, not a move). Serviceable at any point (no
            // synchronization gate): a `regs` view is honest even at a terminal.
            Request::Read { gpa, len } => self.read(*gpa, *len),
            Request::Regs => {
                let vmm = self.vmm.as_ref().ok_or(ServeError::Poisoned)?;
                Ok(Ok(Reply::Regs(regs_view(vmm))))
            }
            // Improvisation (task 81): inject `cmd` on the serial input, run to a
            // completion sentinel or the deadline, capture output. The FIRST thing
            // it does is set the timeline's taint bit — conservatively, before any
            // fallible work — so no failure mode can leave an improvised timeline
            // looking clean (the taint guard's authoritative half).
            Request::Exec { cmd, deadline } => self.exec(cmd, *deadline),
            Request::ExecStart { cmd } => self.exec_start(cmd),
            Request::ExecStatus => self.exec_status(),
            // Reproducer mint (task 81) — the taint guard's fail-loud site: a
            // tainted timeline is a loud `Tainted`, never a lying reproducer.
            Request::RecordedEnv => Ok(self.recorded_env_reply()),
        }
    }

    /// `read(gpa, len)`: return exactly `len` bytes of guest **physical** memory at
    /// `gpa`, or a loud [`ControlError`] — **never a truncated success** (task 80).
    /// A pure observation: it borrows the guest image immutably and mutates nothing,
    /// so it cannot perturb the run or any hash.
    ///
    /// The outer `Result` keeps the two categories apart like every other verb: a
    /// **poisoned** server (`vmm == None` after a prior fatal error) is the same
    /// session-fatal [`ServeError::Poisoned`] `regs`/`hash`/`snapshot` return —
    /// **not** a recoverable reply. Guarding the RAM fetch with `ok_or(Poisoned)`
    /// (PR #83 round-1 blocking) is what makes that so: an empty-slice fallback would
    /// have masked the torn session as a bogus `ReadOutOfRange { ram_len: 0 }`, a
    /// recoverable error a client would retry against a VM that no longer exists.
    ///
    /// The recoverable range guards, both fail-loud, checked before any copy:
    /// - `len > `[`READ_CAP`] → [`ControlError::ReadTooLarge`], rejected **before**
    ///   the slice is taken (and before touching the VM — a pure request-validation
    ///   error, like `hash`'s unsupported scopes) so an untrusted `len` can never
    ///   force an over-large copy (conventions rule 4).
    /// - `[gpa, gpa+len)` past guest RAM (or a `gpa + len` overflow) →
    ///   [`ControlError::ReadOutOfRange`]. A short read would hand the caller bytes
    ///   it never asked for; the loud error makes the caller widen or re-address.
    #[allow(clippy::result_large_err)] // ServeError's size is irrelevant on this cold path
    fn read(&self, gpa: u64, len: u32) -> Result<Result<Reply, ControlError>, ServeError> {
        if len > READ_CAP {
            return Ok(Err(ControlError::ReadTooLarge { len, cap: READ_CAP }));
        }
        // A `None` VM is a torn session, session-fatal like every sibling verb —
        // never an empty-RAM fallback that fakes a range error.
        let vmm = self.vmm.as_ref().ok_or(ServeError::Poisoned)?;
        // Resolve the ABSOLUTE guest-physical `gpa` through the engine's region
        // resolver, so an arm64 address (over `RAM_BASE`) reads the right bytes
        // and a low unmapped GPA is out-of-range rather than a wrong main-RAM
        // offset (review r11). x86 (RAM at base 0) is unchanged: a low GPA is its
        // own offset, and the same range fails closed.
        match vmm.guest_slice(gpa, len as usize) {
            Some(bytes) => Ok(Ok(Reply::Bytes(bytes.to_vec()))),
            None => Ok(Err(ControlError::ReadOutOfRange {
                gpa,
                len,
                ram_len: vmm.guest_memory().len() as u64,
            })),
        }
    }

    /// Decode and schedule a mechanical effect at an exact execution moment.
    /// Branch inputs use the same validation path. Invalid requests leave the
    /// schedule unchanged; applied effects become part of the reproducer.
    fn perturb(
        &mut self,
        fault: &control_proto::HostFault,
        at: control_proto::Moment,
    ) -> Result<Reply, ControlError> {
        // A poisoned schedule must be rewound (branch/replay) before it accepts any
        // new fault — staging onto an unsatisfiable schedule is itself unsatisfiable.
        // Re-emit the latched error verbatim (its own typed variant + coordinates).
        if let Some(err) = &self.schedule_poisoned {
            return Err(err.reply());
        }
        let decoded = Effect::decode(&fault.0).map_err(|_| ControlError::MalformedEnvironment)?;
        // Capability first: a VM without virtual time cannot schedule host faults.
        if !self.vmm.as_ref().is_some_and(|v| v.vtime_wired()) {
            return Err(ControlError::Unsupported);
        }
        // Floor = the live VM's current (now exact) V-time: a `Moment` behind it could
        // only apply *later* than recorded.
        let floor = self
            .vmm
            .as_ref()
            .and_then(|v| v.effective_vns())
            .unwrap_or(0);
        self.validate_host_fault(&decoded, at.0, floor)?;
        self.schedule.insert(at.0, decoded);
        Ok(Reply::Unit)
    }

    /// Validate an effect before staging it. Moments must be reachable and
    /// unoccupied, memory ranges must be mapped in full, and interrupt identities
    /// must be supported by the selected machine. The operation is read-only.
    fn validate_host_fault(&self, fault: &Effect, at: u64, floor: u64) -> Result<(), ControlError> {
        // One fault per `Moment` — reject a duplicate against **both** the still-
        // staged schedule **and** the already-applied faults recorded in the
        // reproducer (PR #51 round-2 finding): once a fault at `at` has applied it is
        // gone from the schedule but present in `recorded`, and re-staging it would
        // overwrite it in `recorded_env()` (an applied fault silently vanishing). The
        // remaining, occupancy-free checks are shared with the branch-env path.
        if self.schedule.contains_key(&at) || self.recorded.effects().contains_key(&at) {
            return Err(ControlError::PerturbMomentTaken { at });
        }
        self.check_fault_admissible(fault, at, floor)
    }

    /// The **occupancy-free** admissibility checks for a host fault, shared by
    /// [`perturb`](ControlServer::perturb)'s [`validate_host_fault`](ControlServer::validate_host_fault)
    /// and by the branch-env pre-swap validation (PR #51 round-5): the backend can
    /// arm the exact-count seam, the `Moment` is not behind the `floor`, the gpa is
    /// in range, and the fault class is in scope. It reads only the **current**
    /// `vmm` (its capability + RAM size) and the given `floor`, so the branch path
    /// can call it against the *live* VM using the snapshot's V-time as the floor —
    /// **before** swapping in the restored VM (making a rejected branch
    /// side-effect-free).
    fn check_fault_admissible(
        &self,
        fault: &Effect,
        at: u64,
        floor: u64,
    ) -> Result<(), ControlError> {
        let vmm = self.vmm.as_ref().ok_or(ControlError::Unsupported)?;
        // **Capability check, up front (PR #51 round-2 finding).** Host-plane
        // enforcement needs the exact-count arrival seam ([`Vmm::set_idle_wake_vns`]); on a
        // backend that cannot arm it (stock KVM / M1 / M2 — no deterministic
        // VM-exit counter) a staged fault could only be applied at a natural
        // exit *past* its `Moment`, recording a count the run never truly stopped at.
        // Reject rather than silently apply late.
        if !vmm.vtime_wired() {
            return Err(ControlError::Unsupported);
        }
        if at < floor {
            return Err(ControlError::PerturbPastMoment { at, floor });
        }
        match fault {
            Effect::WriteMemory { gpa, bytes } | Effect::XorMemory { gpa, bytes } => {
                // Stage-time validation must use the SAME region resolver
                // `corrupt_memory` applies at arrival (review r13 — the last of the
                // r11 GPA family). Otherwise, on arm64 (RAM over `RAM_BASE`) a valid
                // high GPA would be rejected here while a low unmapped GPA would
                // stage cleanly and then explode session-fatally at `apply_host_fault`.
                // x86 (RAM at base 0) resolves a low GPA to its own offset, so the
                // range decision is byte-identical.
                if vmm.guest_slice(*gpa, bytes.len()).is_none() {
                    return Err(ControlError::PerturbOutOfRange {
                        gpa: *gpa,
                        ram_len: vmm.guest_memory().len() as u64,
                    });
                }
            }
            Effect::InjectInterrupt { vector } => {
                // Every `InjectInterrupt` failure mode is a stage-time-decidable
                // property of the request and the machine (PR #51 round-8) — reject
                // it here as a recoverable reply, mirroring the `CorruptMemory`
                // bounds check, rather than letting it explode as a session-fatal
                // `ServeError::Vmm` at `apply_host_fault`.
                //
                // WHICH identities are legal is the **vendor's** question, not the
                // engine's: x86's xAPIC has 8-bit vectors with `0..16` reserved,
                // while a GIC's INTIDs run past 255 and its `0..16` are deliverable
                // SGIs. So ask the vendor and map its verdict onto the wire.
                if let Err(reject) = <B::A as Vendor>::check_wire_interrupt(vmm, *vector) {
                    return Err(match reject {
                        // A perturbation this machine's fabric simply cannot deliver.
                        InterruptReject::NoFabric | InterruptReject::OutOfRange => {
                            ControlError::Unsupported
                        }
                        // A request error the client can fix by picking another id.
                        InterruptReject::Reserved { vector } => {
                            ControlError::PerturbReservedVector { vector }
                        }
                    });
                }
            }
        }
        Ok(())
    }

    /// The capture-path chooser for one seal (task 95 M2.1). Derive over the
    /// drained dirty set **only** when everything is provably right: a tracked
    /// parent exists, it is still live in the store, its chain is under the
    /// bound, and the drain vouches for completeness ([`Vmm::drain_dirty_pages`]
    /// returns `Some` — backend log readable, no untracked host write). On any
    /// doubt — including a failed derive itself — fall back to `snapshot_base`
    /// (correct-by-construction; content-dedup keeps a flatten cheap in storage).
    /// At the chain bound, a complete dirty-log drain flattens by walking only
    /// the chain page sets plus the current dirty set. The seal RPC never fails
    /// because the optimization was unavailable.
    /// Returns `(id, window_consumed, dirty_gfns)`: `window_consumed` is `true`
    /// iff the drain ran (and therefore reset the tracking window as its own
    /// retrieve-and-reset side effect), while `dirty_gfns` preserves that
    /// drain's relative page set for out-of-band profiling.
    fn seal_into_store(
        engine: &mut SnapshotEngine,
        vmm: &mut Vmm<B>,
        parent: Option<SnapshotId>,
        blob: &[u8],
    ) -> Result<(SnapshotId, bool, Option<Vec<u64>>), SnapshotError> {
        if let Some(parent) = parent {
            // Still live (a `drop` verb may have released it since) and bounded:
            // at the chain cap the seal flattens via a fresh base instead.
            let chain_ok = engine
                .stats(parent)
                .is_ok_and(|s| s.chain_len < engine.max_chain_len());
            if chain_ok && let Some(gfns) = vmm.drain_dirty_pages() {
                // From here the window is consumed either way. A derive error
                // (unreachable in practice: liveness and image size were just
                // checked, and a dropped builder releases its interned pages)
                // still falls back to the full scan rather than failing the seal.
                return match engine.snapshot_derive(parent, vmm.guest_memory(), Some(&gfns), blob) {
                    Ok(id) => Ok((id, true, Some(gfns))),
                    Err(_) => engine
                        .snapshot_base(vmm.guest_memory(), blob)
                        .map(|id| (id, true, Some(gfns))),
                };
            }
            if !chain_ok
                && engine.stats(parent).is_ok()
                && let Some(gfns) = vmm.drain_dirty_pages()
            {
                return match engine.snapshot_flatten(parent, vmm.guest_memory(), &gfns, blob) {
                    Ok(id) => Ok((id, true, Some(gfns))),
                    Err(_) => engine
                        .snapshot_base(vmm.guest_memory(), blob)
                        .map(|id| (id, true, Some(gfns))),
                };
            }
        }
        engine
            .snapshot_base(vmm.guest_memory(), blob)
            .map(|id| (id, false, None))
    }

    /// `snapshot`: seal the current point into the engine (memory image +
    /// canonical `vm_state` blob) and mint a wire handle. Since task 95 M2.1 the
    /// memory half derives from the tracked parent over the drained dirty set
    /// when it safely can ([`Self::seal_into_store`]); the reply and semantics
    /// are identical either way.
    ///
    /// **The reply binds the seal's evidence cut** (task 127, bead `hm-bbx.6`):
    /// the one [`Reply::Snapshot`] carries the handle, the synchronized seal
    /// `Moment` (the sealed `vm_state`'s own exact V-time — the same value a
    /// later restore's floor validates against), the **included SDK-event
    /// count** (the SDK capture vector's prefix length; positions below it are
    /// included, at/after excluded — a prefix-length cut, never a `Moment`
    /// comparison), and the timeline taint. All four are read from the same
    /// stopped server state between two verbs — the server stamp is the sole
    /// authority; a client never reconstructs the cut with a second read. The
    /// serial-console capture is a separate source-local stream and is
    /// structurally unable to enter the count. Every error path below returns
    /// **neither a usable handle nor a cut** — no partial cut on failure.
    ///
    /// Captures pending host effects, reseeds, the recorded input prefix, the
    /// schedule failure latch, and the command nonce at this same boundary.
    fn snapshot(&mut self) -> Result<Result<Reply, ControlError>, ServeError> {
        // Do not let a previous seal's profiling evidence leak into a failed
        // or base capture.
        self.last_seal_dirty_gfns = None;
        let control = self.capture_control_state();
        let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
        let vm_state = match vmm.save_vm_state() {
            Ok(s) => s,
            // Preserve the owning layer's diagnostic. Advancing the guest to
            // escape a representation gap would change the requested endpoint.
            Err(VmmError::ContractViolation(reason)) => {
                return Ok(Err(ControlError::SnapshotRefused { reason }));
            }
            // A backend save failure is substrate breakage, not a caller error.
            Err(e) => return Err(e.into()),
        };
        let sdk_channel = match vmm.sdk_snapshot() {
            Ok(state) => state,
            Err(error) => {
                self.vmm = None;
                return Err(ServeError::Service(error));
            }
        };
        let mut state_blob_suffix = vmm.state_blob_suffix()?;
        control.append_hash(&mut state_blob_suffix);
        let blob = vm_state.encode().map_err(SnapshotError::from)?;
        // Task 127: stamp the seal's evidence cut from the SAME stopped state
        // the seal captures — the sealed record's own synchronized V-time (not
        // a second `effective_vns` read) and the SDK capture vector's current
        // prefix length. Nothing advances the VM between here and the reply,
        // so handle, Moment, count, and taint are one atomic observation.
        let at = vm_state.vtime().snapshot_vns;
        let sdk_events = vmm.sdk_events().len() as u64;
        let (trace_events, trace_schedules) = vmm.virtual_time_trace().map_or((0, 0), |trace| {
            (
                trace.normalized_log().events.len() as u64,
                trace.schedule().len() as u64,
            )
        });
        let policy = self.recorded.config().clone();
        // Task 95 M2.1: capture O(dirty) when the tracked window allows it,
        // full-scan otherwise — then re-arm the window with the new snapshot as
        // the next seal's parent (the arm fails ⇒ the next seal full-scans too).
        //
        // `take()` the parent across the fallible seal: if the seal errors
        // AFTER the drain emptied the window, a caller that treats the error
        // as retryable (the pub `handle` API permits it) must NOT find the old
        // parent still armed — a retried seal would then derive over a window
        // missing everything dirtied before the failure. A failed seal always
        // leaves `derive_parent = None` (the retry full-scans).
        let parent = self.derive_parent.take();
        let (store_id, window_consumed, dirty_gfns) =
            Self::seal_into_store(&mut self.engine, vmm, parent, &blob)?;
        self.last_seal_dirty_gfns = dirty_gfns;
        // A consumed window was already reset by the drain itself (nothing
        // ran since — the VM is stopped between verbs), so arm directly and
        // skip the redundant per-slot drain; otherwise drain-and-discard now.
        let tracked = window_consumed || vmm.reset_dirty_tracking();
        self.derive_parent = tracked.then_some(store_id);
        self.current_image = tracked.then_some(store_id);
        // Task 73: capture the SDK channel's replay-relevant state (seeded stream
        // position + event log) alongside the guest snapshot — owned, so `vmm`'s
        // borrow ends before we touch `self.sdk_snaps`.

        // (Task 110: the pvclock channel — offer + registration — rides the
        // sealed vm_state's device blob itself (v4), so there is no side
        // table to capture here; the restore path validates and commits it
        // with the blob.)
        let id = self.next_snap;
        self.next_snap += 1;
        self.snaps.insert(id, store_id);
        if let Some(channel) = sdk_channel {
            // Capture the active service configuration too, so a replay restores
            // the same handler setup.
            let policy = self.recorded.config().clone();
            self.sdk_snaps.insert(id, SdkSnap { channel, policy });
        }
        // Task 81: a snapshot inherits the live timeline's taint. If the timeline
        // was tainted by an `exec` improvisation, record the handle (so any
        // `branch`/`replay` of it yields a tainted timeline). Either way the
        // reply is the ONE seal-bound shape (task 127): handle + cut + taint —
        // a tainted seal still binds its exact evidence cut.
        if self.timeline_tainted {
            self.tainted_snaps.insert(id);
        }
        self.snapshot_meta.insert(
            id,
            SnapshotMeta {
                at,
                sdk_events,
                trace_events,
                trace_schedules,
                tainted: self.timeline_tainted,
                state_hash: None,
                state_blob_suffix,
                policy,
                control_state: control.encode(),
            },
        );
        Ok(Ok(Reply::Snapshot {
            id: SnapId(id),
            at: Moment(at),
            sdk_events,
            tainted: self.timeline_tainted,
        }))
    }

    /// `drop`: release the store layer behind a wire handle and GC.
    fn drop_snap(&mut self, snap: SnapId) -> Result<Reply, ControlError> {
        let Some(store_id) = self.snaps.remove(&snap.0) else {
            return Err(ControlError::UnknownSnapshot(snap));
        };
        // Task 73: drop the SDK channel snapshot with its handle (ephemeral pool
        // state, released alongside the guest snapshot).
        self.sdk_snaps.remove(&snap.0);
        // Task 81: drop its taint record with the handle. (The `SnapId` is
        // monotonic — [`next_snap`] never reuses a number — so a later snapshot can
        // never inherit this one's taint by handle reuse.)
        self.tainted_snaps.remove(&snap.0);
        self.snapshot_meta.remove(&snap.0);
        if self.current_image == Some(store_id) {
            self.current_image = None;
            self.derive_parent = None;
        }
        // The handle was minted by `snapshot`, which retains exactly one ref;
        // releasing it can only fail if the store lost the layer — an
        // invariant failure we still answer on the wire (the handle is gone
        // either way) rather than tearing the session down.
        if self.engine.release(store_id).is_err() {
            return Err(ControlError::UnknownSnapshot(snap));
        }
        self.engine.gc();
        Ok(Reply::Unit)
    }

    /// Patch the existing VM to `store_id` and restore its non-memory state.
    ///
    /// Every error is deliberately collapsed for the caller: in-place restore
    /// is an optimization, so the only safe response to a missing prerequisite
    /// or a substrate rejection is the existing fresh-VM restore path.
    fn restore_in_place(
        &mut self,
        store_id: SnapshotId,
        vm_state: &<B::A as Vendor>::Snapshot,
    ) -> Result<u64, ()> {
        let from = self.current_image;
        let mut pages: BTreeMap<u64, [u8; 4096]> = self
            .engine
            .diff_pages(from, store_id)
            .map_err(|_| ())?
            .into_iter()
            .collect();

        let dirty = {
            let vmm = self.vmm.as_mut().ok_or(())?;
            vmm.retire_pending_completion().map_err(|_| ())?;
            vmm.drain_dirty_pages()
        };
        match dirty {
            Some(gfns) => {
                for gfn in gfns {
                    if let std::collections::btree_map::Entry::Vacant(entry) = pages.entry(gfn) {
                        entry.insert(self.engine.read_page(store_id, gfn).map_err(|_| ())?);
                    }
                }
            }
            // With no known source image the store diff is the complete target
            // image, so an unavailable dirty log cannot hide a live difference.
            None if from.is_none() => {}
            None => return Err(()),
        }

        let pages: Vec<(u64, [u8; 4096])> = pages.into_iter().collect();
        let bytes = u64::try_from(pages.len())
            .ok()
            .and_then(|count| count.checked_mul(4096))
            .ok_or(())?;
        let vmm = self.vmm.as_mut().ok_or(())?;
        vmm.write_guest_pages(&pages).map_err(|_| ())?;
        vmm.restore_vm_state(vm_state).map_err(|_| ())?;
        Ok(bytes)
    }

    /// Install a genuine fresh boot after a recoverable restore rejection.
    fn install_recovery_boot(&mut self, fresh: Vmm<B>) {
        self.vmm = Some(fresh);
        self.derive_parent = None;
        self.current_image = None;
        self.last_restore_bytes_written = 0;
        self.reset_schedule_to_fresh_vm();
        self.timeline_tainted = false;
        self.session_trace_start = SessionTraceStart::RecoveryBoot;
        let sdk_env = RecordedEnv::new(
            self.recorded.seed(),
            Box::new(NominalHandler) as Box<dyn ServiceHandler>,
        );
        let sdk_policy = self.recorded.config().clone();
        if let Some(vmm) = self.vmm.as_mut() {
            vmm.enable_sdk(sdk_env, &sdk_policy);
        }
    }

    /// `branch` (with an env) / `replay` (without): restore `snap` under the
    /// selected mode, then reseed from the env's seed iff branching.
    fn restore(
        &mut self,
        snap: SnapId,
        env: Option<&control_proto::Reproducer>,
    ) -> Result<Result<Reply, ControlError>, ServeError> {
        // 1. Validate everything non-destructively first: the env blob …
        //    `seed` is the branch seed (`None` for a verbatim replay); `host` is
        //    the env's host-plane schedule to stage after a successful restore.
        let mut host: Vec<(u64, Effect)> = Vec::new();
        let mut reseeds: BTreeMap<u64, u64> = BTreeMap::new();
        let mut payloads: Option<Vec<Vec<u8>>> = None;
        let mut answers = BTreeMap::new();
        // Task 73: the branch env's service configuration, preserved into the
        // recorded reproducer below so a replay rebuilds the same handler.
        // `None` for a verbatim replay.
        let mut env_policy: Option<ServiceConfig> = None;
        let seed = match env {
            None => None,
            Some(env) => {
                if env.blob_version != EnvSpec::BLOB_VERSION {
                    return Ok(Err(ControlError::BadEnvVersion(env.blob_version)));
                }
                let spec = match EnvSpec::decode(&env.bytes) {
                    Ok(spec) => spec,
                    Err(_) => return Ok(Err(ControlError::MalformedEnvironment)),
                };
                // The core validates framing and machine operations; the installed
                // service factory validates extension identity and configuration.
                host = spec
                    .effects()
                    .iter()
                    .map(|(&at, effect)| (at, effect.clone()))
                    .collect();
                reseeds = spec.reseeds().clone();
                payloads = spec.payloads().map(<[Vec<u8>]>::to_vec);
                env_policy = Some(spec.config().clone());
                answers = spec.answers().clone();
                Some(spec.seed())
            }
        };
        // Construct the extension before changing the live VM. Unknown or
        // incompatible implementations leave the active session untouched.
        let restore_config = env_policy
            .clone()
            .or_else(|| self.sdk_snaps.get(&snap.0).map(|s| s.policy.clone()))
            .unwrap_or_default();
        let prepared_handler = match (self.service_factory)(&restore_config) {
            Ok(handler)
                if handler.identity() == restore_config.identity
                    && handler.configuration() == restore_config.configuration =>
            {
                handler
            }
            _ => return Ok(Err(ControlError::Unsupported)),
        };
        let mut prepared_env = RecordedEnv::new(seed.unwrap_or(0), prepared_handler);
        if let Some(snapshot) = self.sdk_snaps.get(&snap.0).filter(|_| seed.is_none())
            && snapshot
                .channel
                .recorded
                .restore_into(&mut prepared_env)
                .is_err()
        {
            return Ok(Err(ControlError::RestoreFailed));
        }
        if seed.is_some() && prepared_env.set_payloads(payloads.clone()).is_err() {
            return Ok(Err(ControlError::MalformedEnvironment));
        }
        for (&(at, service, request), answer) in &answers {
            prepared_env.record_service_request(at, service, request, answer.clone());
        }
        // … then the handle and the sealed snapshot pieces.
        let Some(&store_id) = self.snaps.get(&snap.0) else {
            return Ok(Err(ControlError::UnknownSnapshot(snap)));
        };
        let Ok(vm_state) = self.engine.vm_state::<<B::A as Vendor>::Snapshot>(store_id) else {
            return Ok(Err(ControlError::RestoreFailed));
        };
        let source_control = match self.snapshot_meta.get(&snap.0) {
            Some(meta) => {
                match ControlState::decode_for_policy(&meta.control_state, &meta.policy) {
                    Ok(state) => state,
                    Err(_) => return Ok(Err(ControlError::RestoreFailed)),
                }
            }
            None => return Ok(Err(ControlError::RestoreFailed)),
        };
        // The fresh restore paths need the complete image before they replace
        // the live VM. In-place restore deliberately defers materialization: a
        // successful patch must perform work proportional only to changed and
        // live-dirty pages.
        let mut mapping = if restore_defers_materialization(self.restore_mode) {
            None
        } else {
            let Ok(mapping) = self.engine.materialize(store_id) else {
                return Ok(Err(ControlError::RestoreFailed));
            };
            Some(mapping)
        };
        // 1b. **Validate the branch env's host schedule BEFORE any swap (PR #51
        //     round-5, blocking item 1).** A rejected branch must be side-effect-free
        //     — so validate against the STILL-LIVE VM (its capability + RAM size,
        //     which the factory mirrors) with the `floor` derived from the snapshot's
        //     own V-time (`vm_state.vtime.snapshot_vns`) — no restore needed. If any
        //     fault is inadmissible (unarmable backend, out-of-range gpa, out-of-scope
        //     clock fault, a `Moment` behind the snapshot, or an intra-env duplicate
        //     `Moment` — ruling B), reply the recoverable `ControlError` with the old
        //     VM untouched. (The `RestoreFailed` path below still mutates — a genuine
        //     restore failure cannot be pre-validated — and is documented there.)
        // (`vtime()` is the arch-neutral `SnapshotRecords` accessor — the one
        // engine read of the decoded snapshot; the record set stays opaque.)
        let restored_floor = vm_state.vtime().snapshot_vns;
        if let Some(bytes) = source_control
            .as_ref()
            .and_then(|control| control.exec.as_deref())
        {
            let command = RetainedExec::decode(bytes).map_err(ServeError::CommandState)?;
            if !self.tainted_snaps.contains(&snap.0)
                || command
                    .validate_at(Moment(restored_floor), usize::MAX)
                    .is_err()
            {
                return Ok(Err(ControlError::RestoreFailed));
            }
        }
        let mut seen: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
        for (m, fault) in &host {
            if let Err(e) = self.check_fault_admissible(fault, *m, restored_floor) {
                return Ok(Err(e));
            }
            if !seen.insert(*m) {
                return Ok(Err(ControlError::PerturbMomentTaken { at: *m }));
            }
        }
        // 1c. **Validate the reseed markers the same way (task 78).** A marker
        //     behind the snapshot floor could only apply later than recorded —
        //     the same non-reproducing class as a past host fault; a marker
        //     strictly beyond the floor needs the exact-count arrival seam
        //     (like a staged fault), so an unarmable backend rejects up front
        //     rather than reseeding late. A marker AT the floor is the branch
        //     reseed itself (applied below, no arrival needed).
        for &m in reseeds.keys() {
            if m < restored_floor {
                return Ok(Err(ControlError::PerturbPastMoment {
                    at: m,
                    floor: restored_floor,
                }));
            }
            if reseed_marker_requires_arrival(m, restored_floor)
                && !self.vmm.as_ref().is_some_and(|v| v.vtime_wired())
            {
                return Ok(Err(ControlError::Unsupported));
            }
        }
        // 2. In-place mode first patches the existing VM. Any missing
        //    prerequisite or restore error is an optimization miss: count it and
        //    take the proven fresh-VM path for this operation. Other modes drop
        //    the live VM immediately (freeing its virtual-time clock — the box
        //    allows one open at a time) and boot a fresh restore target. A factory
        //    failure is fatal once the old VM has been mutated or dropped.
        //
        //    Task 95 M2.2: with a remap factory installed (and the Remap mode
        //    active, the default), the fresh VM is composed **around** the
        //    materialized mapping — its buffer is the guest RAM the memslots
        //    register — and only the non-memory half is restored
        //    (`restore_vm_state`): no full-image memcpy, untouched pages fault
        //    lazily, guest writes stay CoW-private. Otherwise the pre-task-95
        //    memcpy path runs byte-for-byte (`restore_snapshot`).
        self.finish_session_trace_segment();
        let in_place = self.restore_mode == RestoreMode::InPlace;
        let in_place_result = in_place.then(|| self.restore_in_place(store_id, &vm_state));
        let used_in_place = matches!(in_place_result, Some(Ok(_)));
        if let Some(Ok(bytes)) = in_place_result {
            self.last_restore_bytes_written = bytes;
        } else if in_place {
            self.in_place_fallbacks = self.in_place_fallbacks.saturating_add(1);
            self.last_restore_bytes_written = 0;
            self.current_image = None;
        }

        // In-place fallback uses the remap factory when one is installed; an
        // explicit Memcpy selection remains the only mode that forbids it.
        let use_remap = self.restore_mode != RestoreMode::Memcpy && self.remap_factory.is_some();
        let (mut fresh, restore_result) = if used_in_place {
            let fresh = self.vmm.take().ok_or(ServeError::Poisoned)?;
            (fresh, Ok(()))
        } else {
            if mapping.is_none() {
                mapping = match self.engine.materialize(store_id) {
                    Ok(mapping) => Some(mapping),
                    Err(_) => {
                        // The in-place attempt may already have consumed a
                        // completion or dirtied the live VM. Replace it with a
                        // genuine fresh boot before returning a recoverable
                        // restore error.
                        self.vmm = None;
                        let recovery = (self.factory)()?;
                        self.install_recovery_boot(recovery);
                        return Ok(Err(ControlError::RestoreFailed));
                    }
                };
            }
            self.vmm = None;
            self.current_image = None;
            self.last_restore_bytes_written = 0;
            let mapping = mapping.expect("fresh restore materialized the target image");
            if use_remap {
                let factory = self
                    .remap_factory
                    .as_mut()
                    .expect("use_remap checked is_some");
                let mut fresh = factory(mapping)?;
                let result = fresh.restore_vm_state(&vm_state);
                (fresh, result)
            } else {
                let mut fresh = (self.factory)()?;
                let result = fresh.restore_snapshot(mapping.as_slice(), &vm_state);
                (fresh, result)
            }
        };
        // 3. Split the two result categories (mirrors `snapshot`).
        //    `restore_vm_state` validates the untrusted blob **before** mutating
        //    any live state, so a *validation-class* rejection leaves the fresh
        //    VM intact at its boot point — keep it (the session stays usable) and
        //    answer the recoverable `RestoreFailed`. A failure *after* validation
        //    (a `Backend::restore` fault, a virtual-time-clock reset failure) is
        //    substrate breakage: the fresh VM's state can no longer be vouched
        //    for, so the VM is dropped (stays `None` → poisoned) and the session
        //    is torn down (`ServeError`) rather than let a client run from
        //    unvouched state.
        match restore_result {
            Ok(()) => {}
            // Pre-commit rejection (a bad/foreign blob, mismatched wiring, or an
            // invalid clock config) — the fresh VM never mutated, so keep it.
            Err(error) if restore_error_is_precommit(&error) => {
                // Task 95 M2.2: on the remap path `fresh` is a restore-target
                // shell (its RAM is the rejected snapshot's mapping; no booted
                // guest, no entry state) — not a usable session VM. Replace it
                // with a genuine fresh boot so a `RestoreFailed` leaves the
                // session on exactly what the memcpy path leaves it on. A
                // factory failure here is fatal, as in step 2.
                if use_remap {
                    drop(fresh);
                    fresh = (self.factory)()?;
                }
                self.install_recovery_boot(fresh);
                return Ok(Err(ControlError::RestoreFailed));
            }
            // Post-validation substrate breakage — the VM is unvouched; tear down.
            Err(error) => return Err(error.into()),
        }
        // 4. Branch ⇒ fork the entropy stream. On this substrate `reseed_entropy`
        //    fails only if V-time is unwired — a composition bug (the factory must
        //    mirror the live VM), fatal.
        //
        //    **No markers** (the pre-task-78 shape): reseed from the env's seed —
        //    byte-for-byte the task-58/59 behavior. **Markers present**: the table
        //    is authoritative (task 78) — a marker at the restore floor is the
        //    (collapsed) branch reseed and applies now; markers beyond the floor
        //    are staged for `run`'s exit-boundary drain; and with no marker at the
        //    floor the stream deliberately continues from the snapshot (a fold
        //    whose first hop carried no reseed).
        if let Some(seed) = seed {
            if reseeds.is_empty() {
                fresh.reseed_entropy(seed)?;
            } else if let Some(&s0) = reseeds.get(&restored_floor) {
                fresh.reseed_entropy(s0)?;
            }
        }
        self.vmm = Some(fresh);
        self.session_trace_start = if seed.is_some() {
            SessionTraceStart::Branch { snapshot: snap.0 }
        } else {
            SessionTraceStart::Replay { snapshot: snap.0 }
        };
        // Task 81: the restored timeline **inherits the snapshot's taint** — a
        // `branch`/`replay` from a tainted snapshot is tainted; from an untainted
        // one, untainted. This is the propagation rule that makes taint follow
        // snapshot ancestry exactly (and lets a rewind to an untainted ancestor
        // legitimately reach untainted state). Both `branch` and `replay` land here.
        self.timeline_tainted = self.tainted_snaps.contains(&snap.0);
        // 5. A restore rewinds the VM, so **re-arm the host-plane schedule** from
        //    scratch (task 59): drop any stale staged faults and reset the recorded
        //    reproducer to a bare `Seeded` at the **restored stream's** seed
        //    ([`reset_schedule_to_fresh_vm`]), then stage the branch env's own host
        //    overrides. The overrides were already validated (admissible, in-`Moment`
        //    order, no duplicates) at step 1b against the live VM — side-effect-free —
        //    so this only stages them; `run` applies + records them.
        self.reset_schedule_to_fresh_vm();
        // Task 73: preserve the branch env's service configuration in the recorded
        // reproducer, then wire the SDK channel from the now-final reproducer so a
        // seeded run and a replay use the same generic handler setup. Wired
        // on every restore; inert (and unhashed) for a guest that never rings the
        // doorbell, so non-SDK paths are unchanged.
        // Task 73: choose the service configuration the SDK environment materializes
        // with. A **branch** uses the branch env's configuration; a **replay**
        // restores the configuration active when the snapshot was sealed.
        // `env_policy`
        // is `Some` iff branching, so `or_else` picks the snapshot's policy only on
        // a replay.
        let sdk_snap = self.sdk_snaps.get(&snap.0).cloned();
        // On a replay, restore the seal-time configuration from the SDK channel
        // snapshot; a branch uses its own `env_policy`.
        let restore_policy = env_policy.or_else(|| sdk_snap.as_ref().map(|s| s.policy.clone()));
        if let Some(policy) = restore_policy {
            self.set_recorded_policy(policy);
        }
        // A branch stages its newly supplied ordered tape. A verbatim replay
        // restores the snapshot's canonical remaining suffix. Set this before
        // materializing the SDK environment so the payload service's
        // availability and state are correct immediately.
        let active_payloads = if seed.is_some() {
            payloads
        } else {
            sdk_snap
                .as_ref()
                .and_then(|snap| snap.channel.remaining_payloads())
        };
        if seed.is_none()
            && let Some(snapshot) = &sdk_snap
        {
            answers = snapshot
                .channel
                .recorded
                .answers()
                .map(|(key, value)| (key, value.clone()))
                .collect();
        }
        for ((at, service, request), answer) in answers {
            self.recorded
                .record_answer(at, service, request, answer)
                .map_err(ServeError::Service)?;
        }
        self.recorded.set_payloads(active_payloads);
        if seed.is_some() {
            prepared_env.reseed(self.recorded.seed());
        }
        let sdk_env = prepared_env;
        let sdk_policy = self.recorded.config().clone();
        self.vmm
            .as_mut()
            .ok_or(ServeError::Poisoned)?
            .enable_sdk(sdk_env, &sdk_policy);
        // Restore the SDK channel snapshot for this handle, if any. A verbatim
        // **replay** (`seed` is `None`) continues the seeded streams from the
        // snapshot's position AND keeps the event prefix — so a fork from a mid-run
        // SDK snapshot reproduces. A **branch** reseeds (`enable_sdk` just set fresh
        // streams from the new seed), so it preserves the event prefix and pending
        // protocol work while the new environment drives subsequent requests.
        if let Some(s) = sdk_snap {
            let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
            if seed.is_some() {
                vmm.sdk_restore_events(&s.channel);
            } else {
                if let Err(error) = vmm.sdk_restore(&s.channel) {
                    self.vmm = None;
                    return Err(ServeError::Service(error));
                }
            }
        }
        // (Task 110: the pvclock channel state rode the restored vm_state's
        // device blob — `restore_vm_state`/`restore_snapshot` validated it
        // symmetrically against the fresh VM's composition and committed it;
        // a mismatched factory surfaces as the recoverable `RestoreFailed`
        // above, exactly like a LAPIC wiring mismatch.)
        for (m, fault) in host {
            self.schedule.insert(m, fault);
        }
        // Stage the future reseeds (strictly beyond the floor), and — iff the
        // env carried markers — stamp the branch-point reseed into the recorded
        // reproducer as a marker at the floor, so `recorded_env()` replays
        // through the marker path (its mid-run reseeds, stamped by `run`, only
        // reproduce if the floor reseed re-executes too). A no-marker branch
        // records the plain `Seeded` shape, byte-for-byte the task-59 behavior.
        if !reseeds.is_empty() {
            use std::ops::Bound;
            for (&m, &s) in reseeds.range((Bound::Excluded(restored_floor), Bound::Unbounded)) {
                self.reseed_schedule.insert(m, s);
            }
            let stream = self
                .vmm
                .as_ref()
                .and_then(|v| v.entropy_state())
                .unwrap_or(0);
            self.recorded.record_reseed(restored_floor, stream);
        }
        if let Some(control) = source_control {
            self.exec_nonce = control.exec_nonce;
            self.retained_exec = control
                .exec
                .as_deref()
                .map(RetainedExec::decode)
                .transpose()
                .map_err(ServeError::CommandState)?;
            if let Some(exec) = &self.retained_exec {
                let vmm = self.vmm.as_ref().ok_or(ServeError::Poisoned)?;
                if let Err(error) = exec.validate_serial(
                    Moment(vmm.effective_vns().unwrap_or(0)),
                    vmm.serial_output(),
                ) {
                    self.vmm = None;
                    return Err(ServeError::CommandState(error));
                }
            }
            if seed.is_none() {
                self.schedule = control.pending.effects().clone();
                self.reseed_schedule = control.pending.reseeds().clone();
                self.schedule_poisoned = control.poisoned;
                self.recorded = control.recorded;
            }
        }
        // Task 95 M2.1: the restored VM's memory IS `store_id`'s image (memcpy
        // wrote exactly it; remap maps exactly it), so arm the dirty window with
        // the branch/replay source as the next seal's derive parent. The
        // drain-and-discard resets the backend log (dropping any stale
        // pre-restore guest-write bits) and clears the memcpy path's wholesale
        // latch; if it cannot arm, the next seal simply full-scans.
        {
            let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
            let tracked = vmm.reset_dirty_tracking();
            self.derive_parent = tracked.then_some(store_id);
            self.current_image = tracked.then_some(store_id);
        }
        Ok(Ok(Reply::Unit))
    }

    /// Overwrite the recorded reproducer's service configuration in place,
    /// keeping its variant (task 73), so a replay rebuilds the same handler.
    fn set_recorded_policy(&mut self, policy: ServiceConfig) {
        self.recorded.set_config(policy);
    }

    /// Capture host-owned continuation state without touching the guest.
    fn capture_control_state(&self) -> ControlState {
        let mut pending = EnvSpec::seeded(0);
        for (&at, effect) in &self.schedule {
            pending.record_effect(at, effect.clone());
        }
        for (&at, &seed) in &self.reseed_schedule {
            pending.record_reseed(at, seed);
        }
        ControlState {
            recorded: self.recorded.clone(),
            pending,
            poisoned: self.schedule_poisoned,
            exec_nonce: self.exec_nonce,
            exec: self.retained_exec.as_ref().map(RetainedExec::encode),
        }
    }

    /// Reset the host-plane schedule + recorded reproducer for the VM currently in
    /// `self.vmm` — called on **every** path that replaces the live VM (a
    /// successful `branch`/`replay`, and a recoverable `RestoreFailed` that keeps
    /// the fresh boot). Clears the schedule and reseeds the recorded reproducer
    /// from the restored VM's **actual entropy stream** ([`Vmm::entropy_state`]),
    /// not the prior session's seed (PR #51 round-2 finding): a `replay` restores a
    /// snapshot whose stream may sit mid-flight under a seed unrelated to the old
    /// session, and a `branch` has just reseeded — reading the live stream captures
    /// the right value for both, so `recorded_env()` stamps a reproducer that
    /// actually reproduces.
    fn reset_schedule_to_fresh_vm(&mut self) {
        self.exec_nonce = 0;
        self.retained_exec = None;
        self.schedule.clear();
        self.reseed_schedule.clear();
        // A rewind is the recovery from a poisoned schedule (round-3): clear the
        // latch on the fresh timeline; replay installs its captured latch below.
        self.schedule_poisoned = None;
        let seed = self
            .vmm
            .as_ref()
            .and_then(|v| v.entropy_state())
            .unwrap_or(0);
        self.recorded = EnvSpec::seeded(seed);
    }

    /// `run(until)`: step the event loop to a terminal stop or the V-time
    /// deadline. The deadline is checked against [`Vmm::effective_vns`]
    /// **before** each step, so a run already at-or-past its deadline stops
    /// immediately (without entering the guest), and the stop point is the first
    /// V-time-intercept boundary at-or-after the deadline — deterministic across
    /// same-seed runs, because effective V-time is.
    ///
    /// Deadlines and scheduled moments are observed only at deterministic VM-exit
    /// boundaries. The backend is never asked to stop between instructions.
    /// `run(until)` therefore advances to the first exit boundary that reaches the
    /// deadline or scheduled moment. A staged fault is stamped into the recorded
    /// environment when its boundary is reached.
    ///
    /// **Reached vs. crossed vs. future.** At each V-time `vns` the
    /// drain classifies a staged `Moment m`:
    /// - `m == vns` → apply now at this exit boundary. This holds regardless of
    ///   the deadline.
    /// - `m < vns` → **late/crossed**: the guest executed *past* `m` (only possible
    ///   on an overshoot), so it can never be applied at its recorded count — the
    ///   schedule failure is retained until a branch or replay selects a
    ///   snapshot without that failure. Saving preserves the failed state.
    /// - `m > vns` → **future**: not yet reached; left staged (or dropped at a
    ///   terminal, with the schedule failure retained).
    ///
    /// The deadline stop is `vns ≥ deadline`; no stronger exact-stop guarantee is made.
    fn run(
        &mut self,
        until: &control_proto::StopConditions,
    ) -> Result<Result<Reply, ControlError>, ServeError> {
        // A poisoned schedule keeps rejecting `run` (with the latched error's own
        // variant + coordinates) until a `branch`/`replay` rewinds it — never applying
        // the crossed fault from the past on a re-sent `run` (PR #51 round-3).
        if let Some(err) = &self.schedule_poisoned {
            return Ok(Err(err.reply()));
        }
        loop {
            if let Some((moment, question)) = self
                .vmm
                .as_ref()
                .ok_or(ServeError::Poisoned)?
                .pending_service_question()
            {
                let mut ctx = question.service().to_le_bytes().to_vec();
                ctx.extend(question.payload());
                return Ok(Ok(Reply::Stop(StopReason::Decision {
                    vtime: control_proto::Moment(moment),
                    id: control_proto::DecisionId(question.request_id()),
                    ctx,
                })));
            }
            let vns = {
                let vmm = self.vmm.as_ref().ok_or(ServeError::Poisoned)?;
                vmm.effective_vns().unwrap_or(0)
            };

            // 1. Drain: apply an **exit-boundary delivery** — `m == vns` at a **synchronized**
            //    point (`effective_vns` is exact there). A `Moment` at-or-below a vns
            //    that is *not* provably exact must NOT be applied as an exit-boundary delivery:
            //    `m < vns` is crossed (the guest is past `m`), and `m == vns` at an
            //    *unsynchronized* point rests on a lower-bound vns so the guest may
            //    have run past `m` too — either way poison (the recorded apply point
            //    can't be trusted). This is the drain half of the round-7 family fix;
            //    the `perturb` gate ensures nothing is *staged* at an unsynchronized
            //    point in the first place, so this is the in-run belt-and-suspenders.
            //    Reseeds drain through the identical classification (task 78) and
            //    apply BEFORE a same-`Moment` fault (the schedule-field doc's
            //    fixed order): the loop below always services the reseed map
            //    first at any given reached `Moment`.
            loop {
                let next_reseed = self.reseed_schedule.range(..=vns).next().map(|(&m, _)| m);
                let next_fault = self.schedule.range(..=vns).next().map(|(&m, _)| m);
                // Reseed-first at a shared Moment: pick the reseed on ties.
                let (m, is_reseed) = match (next_reseed, next_fault) {
                    (Some(r), Some(f)) if r <= f => (r, true),
                    (Some(_) | None, Some(f)) => (f, false),
                    (Some(r), None) => (r, true),
                    (None, None) => break,
                };
                let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
                if is_reseed {
                    // A mid-trajectory reseed marker (a collapsed hop's branch
                    // reseed): re-execute it here, between instructions, and stamp
                    // it into the recorded reproducer so `recorded_env()` replays
                    // it at the identical count. A failure is substrate breakage
                    // (V-time unwired on a marker-validated backend): fail loud.
                    let seed = self.reseed_schedule.remove(&m).expect("range key exists");
                    vmm.reseed_entropy(seed).map_err(ServeError::Vmm)?;
                    self.recorded.record_reseed(m, seed);
                } else {
                    let fault = self.schedule.remove(&m).expect("range key exists");
                    // An apply failure (out-of-range gpa — pre-validated at stage
                    // time; a reserved vector; an unwired LAPIC) is substrate-level
                    // breakage of a vouched run: fail loud (session-fatal), never a
                    // silent skip that would desync the recorded env from the run.
                    vmm.apply_effect(&fault).map_err(ServeError::Vmm)?;
                    self.recorded.record_effect(m, fault);
                }
            }

            if until.on.armed(control_proto::class_bit::EXEC_COMPLETE)
                && let Some(exec) = &self.retained_exec
                && !exec.is_pending()
            {
                let status = exec.view(Moment(vns));
                return Ok(Ok(Reply::Stop(StopReason::ExecComplete {
                    vtime: Moment(vns),
                    id: status.id,
                })));
            }

            // Surface a requested setup-complete point after applying inputs
            // due at this boundary. Future inputs remain queued in the snapshot;
            // waiting for them to drain would move the requested cut.
            if until.on.armed(control_proto::class_bit::SNAPSHOT_POINT) {
                let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
                if vmm.take_snapshot_point() {
                    let vns = vmm.effective_vns().unwrap_or(0);
                    return Ok(Ok(Reply::Stop(StopReason::SnapshotPoint {
                        vtime: Moment(vns),
                    })));
                }
            }

            // 2. Opportunistic V-time deadline (task-58 semantics, unchanged): stop
            //    at the first boundary at-or-past the deadline. The drain above has
            //    already applied every `m == vns` and poisoned any `m < vns`, so the
            //    schedule now carries only future faults (`m > vns`) — left staged
            //    for a later `run`.
            let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
            let vns = vmm.effective_vns().unwrap_or(0);
            if let Some(deadline) = until.deadline
                && vns >= deadline.0
            {
                return Ok(Ok(Reply::Stop(StopReason::Deadline { vtime: Moment(vns) })));
            }

            // 3. Publish the next host event for idle-wakeup planning only. Normal execution
            //    still runs to the next VM exit.
            let next_host_event = [
                self.schedule.keys().next().copied(),
                self.reseed_schedule.keys().next().copied(),
            ]
            .into_iter()
            .flatten()
            .min();
            vmm.set_idle_wake_vns(next_host_event);

            // 4. Step. A terminal stop ends the run; a cooperating-SDK stop
            //    (task 73) surfaces the assertion / snapshot point.
            let step = vmm.step()?;
            if let Some(exec) = &mut self.retained_exec {
                exec.feed(
                    vmm.serial_output(),
                    Moment(vmm.effective_vns().unwrap_or(0)),
                    matches!(&step, Step::Terminal(_)),
                )
                .map_err(ServeError::CommandState)?;
            }
            match step {
                // A deferred `setup_complete` snapshot point is surfaced at the top
                // of the NEXT iteration, after the drain (round-5 P1) — not here.
                Step::Continued => {}
                Step::SdkStop => {
                    let vns = vmm.effective_vns().unwrap_or(0);
                    let stop = vmm.take_sdk_stop();
                    if matches!(stop, Some(SdkStop::Decision { .. })) {
                        return Ok(Ok(Reply::Stop(sdk_stop_to_reason(
                            stop.expect("decision is present"),
                            vns,
                        ))));
                    }
                    // **Poison loud on ANY staged host effect (P2, task 59's crossed-
                    // effect rule).** An SDK stop surfaces at a hypercall-doorbell
                    // `OUT` — NOT a V-time intercept — so `effective_vns` here is
                    // only a lower bound; a still-staged effect at-or-just-above it
                    // may already be crossed (the guest ran past `m` within the
                    // exit-boundary variability window). Exactly like the terminal arm below, poison
                    // rather than silently returning a stop past a crossed fault;
                    // the client rewinds via `branch`/`replay`. Generic service
                    // answers are recorded by the channel itself. Any staged host
                    // effect OR staged reseed (task 78) poisons — a
                    // crossed reseed marker (guest ran past its `Moment` in the exit-boundary variability
                    // window) is the same non-reproducing class as a crossed effect:
                    // a later replay from the reseed re-derives a different stream.
                    // Mirror the terminal arm below (both staged structures).
                    let staged = [
                        self.schedule.keys().next().copied(),
                        self.reseed_schedule.keys().next().copied(),
                    ]
                    .into_iter()
                    .flatten()
                    .min();
                    if let Some(m) = staged {
                        let failure = ScheduleFailure {
                            moment: m,
                            vtime: vns,
                        };
                        self.schedule_poisoned = Some(failure);
                        return Ok(Err(failure.reply()));
                    }
                    let reason = match stop {
                        Some(sdk_stop) => sdk_stop_to_reason(sdk_stop, vns),
                        // `SdkStop` is armed (an assertion) before the step returns
                        // `SdkStop`, so this is statically unreachable; be total
                        // anyway with a benign quiescent stop.
                        None => StopReason::Quiescent { vtime: Moment(vns) },
                    };
                    // Only assertions are mask-gated. Payload exhaustion is a
                    // terminal Quiescent stop and always surfaces.
                    if matches!(reason, StopReason::Assertion { .. })
                        && !until.on.armed(control_proto::class_bit::ASSERTION)
                    {
                        continue;
                    }
                    return Ok(Ok(Reply::Stop(reason)));
                }
                Step::Terminal(reason) => {
                    let vns = vmm.effective_vns().unwrap_or(0);
                    // **Poison loud on ANY staged fault at a terminal (PR #51
                    // round-6, supersedes round-5 item 2).** A natural terminal exit
                    // (HLT / debug) is *not* a V-time intercept, so `effective_vns`
                    // here is only the last-intercept **lower bound** — nothing still
                    // staged is provably uncrossed (with `deadline < m` the arrival is
                    // never armed and the guest can run past `m` to the terminal). The
                    // round-5 silent `clear()` could therefore drop an accepted perturb
                    // that *was* crossed — breaking exit-boundary-or-loud. The safe
                    // semantic is LOUD: poison whenever any fault remains staged, and
                    // retain the plan and failure in snapshots. A client can select
                    // a different plan or rewind to an earlier runnable snapshot.
                    // Any staged host fault OR staged reseed (task 78) poisons:
                    // a reseed beyond the trajectory is the same non-reproducing
                    // class as a crossed fault.
                    let staged = [
                        self.schedule.keys().next().copied(),
                        self.reseed_schedule.keys().next().copied(),
                    ]
                    .into_iter()
                    .flatten()
                    .min();
                    if let Some(m) = staged {
                        let failure = ScheduleFailure {
                            moment: m,
                            vtime: vns,
                        };
                        self.schedule_poisoned = Some(failure);
                        return Ok(Err(failure.reply()));
                    }
                    return Ok(Ok(Reply::Stop(map_terminal(reason, vns))));
                }
            }
        }
    }

    /// Inject once at this stopped point. The VM owns the queued input bytes;
    /// the control plane owns only the output cursor and completion parser.
    fn exec_start(&mut self, cmd: &str) -> Result<Result<Reply, ControlError>, ServeError> {
        if let Some(exec) = &self.retained_exec
            && exec.is_pending()
        {
            let vmm = self.vmm.as_ref().ok_or(ServeError::Poisoned)?;
            return Ok(Err(ControlError::ExecPending {
                id: exec.view(Moment(vmm.effective_vns().unwrap_or(0))).id,
            }));
        }
        let Some(next_nonce) = self.exec_nonce.checked_add(1) else {
            return Ok(Err(ControlError::Unsupported));
        };
        self.timeline_tainted = true;
        let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
        let exec = RetainedExec::new(cmd, self.exec_nonce, vmm.serial_output().len());
        self.exec_nonce = next_nonce;
        vmm.inject_serial_input(exec.input());
        self.retained_exec = Some(exec);
        self.exec_status()
    }

    /// Observation must neither feed the parser nor touch guest execution.
    fn exec_status(&self) -> Result<Result<Reply, ControlError>, ServeError> {
        let vmm = self.vmm.as_ref().ok_or(ServeError::Poisoned)?;
        let at = Moment(vmm.effective_vns().unwrap_or(0));
        Ok(Ok(Reply::ExecState(
            self.retained_exec.as_ref().map(|exec| exec.view(at)),
        )))
    }

    /// Legacy combined call uses the same retained command and Run machinery.
    /// An incomplete result stays pending and can subsequently be resumed by Run.
    fn exec(
        &mut self,
        cmd: &str,
        deadline: Moment,
    ) -> Result<Result<Reply, ControlError>, ServeError> {
        if let Err(error) = self.exec_start(cmd)? {
            return Ok(Err(error));
        }
        let until = control_proto::StopConditions {
            deadline: Some(deadline),
            on: control_proto::StopMask::NONE
                .arm(control_proto::class_bit::ASSERTION)
                .arm(control_proto::class_bit::EXEC_COMPLETE),
        };
        if let Err(error) = self.run(&until)? {
            return Ok(Err(error));
        }
        let Reply::ExecState(Some(status)) =
            self.exec_status()?.map_err(|_| ServeError::Poisoned)?
        else {
            return Err(ServeError::CommandState("started command is absent"));
        };
        Ok(Ok(Reply::ExecResult {
            output: status.output,
            ok: matches!(
                status.completion,
                control_proto::ExecCompletion::Exited { .. }
            ),
        }))
    }

    /// The reply to [`Request::RecordedEnv`] (task 81) — the taint guard's
    /// **fail-loud site**. Mint the recorded reproducer (the [`recorded`](Self::recorded)
    /// [`EnvSpec`] as a wire [`Reproducer`]) **only** on an untainted timeline; a
    /// timeline an `exec` improvisation has tainted returns [`ControlError::Tainted`]
    /// instead — an improvised timeline is off the record and has no honest
    /// reproducer, so the server refuses rather than hand back an `Reproducer` that
    /// does not reproduce. Pure (no VM mutation), so it is answerable at any point.
    fn recorded_env_reply(&self) -> Result<Reply, ControlError> {
        if self.timeline_tainted {
            return Err(ControlError::Tainted);
        }
        let mut recorded = self.recorded.clone();
        // The stored spec names the tape originally staged at branch time;
        // the live VMM owns consumption. Re-emit only the canonical remaining
        // suffix so branching this reproducer from the current cut recreates
        // the same future input state.
        recorded.set_payloads(self.vmm.as_ref().and_then(Vmm::sdk_remaining_payloads));
        Ok(Reply::Recorded(Reproducer {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: recorded
                .try_encode()
                .map_err(|_| ControlError::MalformedEnvironment)?,
        }))
    }
}

/// Map a substrate [`TerminalReason`] to the wire [`StopReason`], stamped with
/// the effective V-time. Workload-blind (module doc): `Hlt` and a clean
/// `DebugExit{0}` are quiescence; a non-zero debug-exit code is a
/// guest-reported failure (`Crash{Panic}`, detail = the code byte); a backend
/// `Shutdown` (triple fault / guest-initiated shutdown) is `Crash{Shutdown}` —
/// a workload whose *clean terminal is a forced reboot* (the Postgres image)
/// reads as `Crash{Shutdown}` here, and interpreting that convention is the
/// workload-aware caller's job.
/// Map a task-73 [`SdkStop`] to the wire [`StopReason`], stamped with the
/// effective V-time. An assertion violation carries its point id + detail as the
/// [`EventRef`]; a `setup_complete` is a snapshot fork.
/// A page of the SDK event capture starting at `offset`, bounded to the control
/// frame limit (round-5 P4): the cumulative encoded reply body stays under
/// [`control_proto::MAX_FRAME_LEN`], but always includes at least one event when
/// any remain — a single event's bytes are `<= MAX_PAYLOAD`, far under the frame
/// limit — so paging strictly progresses (the client fetches until an empty page).
fn page_sdk_events(all: &[(u64, u32, Vec<u8>)], offset: usize) -> Vec<(u64, u32, Vec<u8>)> {
    // result tag + reply tag + u32 count; each event: moment(8) + id(4) + len(4) + bytes.
    const REPLY_OVERHEAD: usize = 6;
    let start = offset.min(all.len());
    let mut page = Vec::new();
    let mut body = REPLY_OVERHEAD;
    for ev in &all[start..] {
        let ev_size = 8 + 4 + 4 + ev.2.len();
        if !page.is_empty() && body + ev_size > control_proto::MAX_FRAME_LEN {
            break;
        }
        body += ev_size;
        page.push(ev.clone());
    }
    page
}

/// Page the guest serial capture for [`Request::Console`]: `total` is the full
/// capture byte length (the client's paging bound), `chunk` is `serial[offset..]`
/// bounded to the control frame limit so an arbitrarily long console never
/// overflows a single reply. Pure over `serial` — no VM state is read or touched,
/// so it cannot perturb a subsequent `state_hash`.
fn page_console(serial: &[u8], offset: usize) -> (u32, Vec<u8>) {
    // result tag + reply tag + total(u32) + chunk-len(u32) — the reply's fixed
    // overhead, kept off the frame budget the chunk may fill.
    const REPLY_OVERHEAD: usize = 2 + 4 + 4;
    // The uart capture is bounded to well under u32 in every real run; a capture
    // that somehow exceeded u32 would only under-report `total`, never mis-slice
    // (the client pages by the returned chunk lengths, not `total`).
    let total = serial.len().min(u32::MAX as usize) as u32;
    let start = offset.min(serial.len());
    let cap = control_proto::MAX_FRAME_LEN.saturating_sub(REPLY_OVERHEAD);
    let end = (start.saturating_add(cap)).min(serial.len());
    (total, serial[start..end].to_vec())
}

/// Assemble the wire [`RegsView`] for the `regs` observation verb (task 80) from
/// the VM's best-effort vCPU read ([`Vmm::inspect_vcpu`]) and its effective V-time
/// ([`Vmm::effective_vns`]). Pure and non-mutating.
///
/// The GPRs and segment selectors are placed in the view's canonical order
/// (`rax rbx rcx rdx rsi rdi rbp rsp r8..r15` — note **rbp before rsp** — and
/// `cs ss ds es fs gs`). `Moment` and `vtime` are the two names of the single
/// deterministic axis: the effective V-time is a VM-exit count in whole
/// nanoseconds (ratio 1), which is exactly the [`Moment`] the perturb/run plane
/// addresses, so both fields carry it (a fresh / V-time-unwired VM reads `0`).
fn regs_view<B: Backend<A: Vendor>>(vmm: &Vmm<B>) -> RegsView {
    // The register half is per-arch (which registers a machine *has*), so the
    // vendor fills it; the `Moment`/`vtime` half is the engine's one deterministic
    // axis — the effective V-time is a VM-exit count in whole nanoseconds
    // (ratio 1), which is exactly the `Moment` the perturb/run plane addresses, so
    // both fields carry it (a fresh / V-time-unwired VM reads `0`).
    let vns = vmm.effective_vns().unwrap_or(0);
    let mut view = <B::A as Vendor>::regs_view(&vmm.inspect_vcpu());
    view.moment = Moment(vns);
    view.vtime = vns;
    view
}

fn sdk_stop_to_reason(stop: SdkStop, vns: u64) -> StopReason {
    let vtime = Moment(vns);
    match stop {
        SdkStop::Quiescent => StopReason::Quiescent { vtime },
        SdkStop::Decision {
            moment, question, ..
        } => {
            let mut ctx = question.service().to_le_bytes().to_vec();
            ctx.extend(question.payload());
            StopReason::Decision {
                vtime: Moment(moment),
                id: control_proto::DecisionId(question.request_id()),
                ctx,
            }
        }
        // `setup_complete`'s snapshot point is deferred to a synchronized boundary
        // (surfaced directly in the run loop), so the only immediate SDK stop is an
        // assertion.
        SdkStop::Assertion { id, data } => StopReason::Assertion {
            vtime,
            ev: EventRef { id, data },
        },
    }
}

fn map_terminal(reason: TerminalReason, vns: u64) -> StopReason {
    let vtime = Moment(vns);
    match reason {
        TerminalReason::Idle | TerminalReason::DebugExit { code: 0 } => {
            StopReason::Quiescent { vtime }
        }
        TerminalReason::DebugExit { code } => StopReason::Crash {
            vtime,
            info: CrashInfo {
                kind: CrashKind::Panic,
                detail: vec![code],
            },
        },
        TerminalReason::Shutdown => StopReason::Crash {
            vtime,
            info: CrashInfo {
                kind: CrashKind::Shutdown,
                detail: b"backend shutdown exit (triple fault or guest-initiated shutdown)"
                    .to_vec(),
            },
        },
        // The control loop surfaces `Step::SdkStop` via its own arm (mapping it to
        // `StopReason::Assertion` / the deferred snapshot point), so an SDK stop is
        // never routed through `map_terminal` — which only ever sees the substrate
        // terminals from `Step::Terminal`.
        TerminalReason::SdkStop => {
            unreachable!(
                "SdkStop is surfaced by the run loop's Step::SdkStop arm, not map_terminal"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    //! Direct-dispatch unit tests over a scripted `MockBackend` — no socket.
    //! The socket loopback + adapter integration lives in the acceptance suite
    //! (which composes this server with the explorer's
    //! socket `Machine`).

    use control_proto::{
        Answer, CapFlags, ControlError, CrashKind, HashScope, HostFault, Moment, READ_CAP, Reply,
        Reproducer, Request, Resolution, SnapId, StopConditions, StopMask, StopReason,
    };
    use environment::{
        channel::Effect as EnvHostEffect,
        input_spec::{InputSpec as EnvSpec, ServiceConfig, nominal_factory},
    };

    #[derive(Clone)]
    struct TestService {
        response: Vec<u8>,
        calls: u64,
    }
    impl environment::channel::ServiceHandler for TestService {
        fn identity(&self) -> &[u8] {
            b"test-service-v1"
        }
        fn configuration(&self) -> &[u8] {
            &self.response
        }
        fn respond(
            &mut self,
            _: environment::Moment,
            _: &environment::channel::Question,
        ) -> Result<environment::channel::ServiceResponse, environment::channel::ChannelError>
        {
            self.calls += 1;
            Ok(environment::channel::ServiceResponse::Answered(
                environment::channel::Answer::Data(self.response.clone()),
            ))
        }
        fn snapshot_state(&self) -> Result<Vec<u8>, environment::channel::ChannelError> {
            Ok(self.calls.to_le_bytes().to_vec())
        }
        fn restore_state(
            &mut self,
            state: &[u8],
        ) -> Result<(), environment::channel::ChannelError> {
            self.calls = u64::from_le_bytes(
                state
                    .try_into()
                    .map_err(|_| environment::channel::ChannelError::Malformed)?,
            );
            Ok(())
        }
        fn clone_box(&self) -> Box<dyn environment::channel::ServiceHandler> {
            Box::new(self.clone())
        }
    }
    fn with_test_service<B: Backend<A: Vendor>>(mut server: ControlServer<B>) -> ControlServer<B> {
        let nominal = nominal_factory();
        server.set_service_factory(std::sync::Arc::new(move |config| {
            if config.identity == b"test-service-v1" {
                Ok(Box::new(TestService {
                    response: config.configuration.clone(),
                    calls: 0,
                }))
            } else {
                nominal(config)
            }
        }));
        server
    }
    use vmm_backend::{
        Arm64, Arm64Policy, Backend, CommonExit, Exit, MockArm64Backend, MockBackend, X86, X86Exit,
        X86Policy,
    };

    use proptest::prelude::*;

    #[cfg(all(target_os = "linux", not(miri)))]
    use super::host_minor_faults;
    #[cfg(target_os = "linux")]
    use super::host_minor_faults_with;
    use super::{
        ControlServer, ServeError, page_console, page_sdk_events, reseed_marker_requires_arrival,
        restore_defers_materialization, restore_error_is_precommit, server_caps,
        sparse_page_order_error,
    };
    use crate::portable_snapshot::PortableSnapshotError;
    use crate::vendor::Vendor;
    use crate::vendor::x86::contract_vclock_config;
    use crate::vmm::{GuestRam, Vmm, VmmError, VtimeWiring};

    #[cfg(target_os = "linux")]
    #[test]
    fn host_minor_fault_counter_initializes_through_the_miri_seam() {
        let faults = host_minor_faults_with(|usage| {
            // SAFETY: every field in Linux's integer/timeval-only `rusage`
            // representation admits zero, producing a valid initialized value.
            let mut fixture: libc::rusage = unsafe { std::mem::zeroed() };
            fixture.ru_minflt = 77;
            // SAFETY: the seam supplies one live, correctly aligned, writable
            // `rusage` pointer, and this closure initializes it exactly once.
            unsafe { usage.write(fixture) };
            0
        });
        assert_eq!(faults, Some(77));
    }

    #[cfg(all(target_os = "linux", not(miri)))]
    #[test]
    fn host_minor_fault_counter_reports_the_live_process_counter() {
        // The process has faulted in substantially more than one page by the
        // time this test runs. Exact counts are host profiling data, but the
        // public accessor must not collapse a successful getrusage call to a
        // sentinel (`None`, `Some(0)`, or `Some(1)`).
        assert!(host_minor_faults().is_some_and(|faults| faults > 1));
    }

    /// Guest RAM for the doorbell-driving tests (was a per-test `0x2_0000`):
    /// 128 KiB natively, 64 KiB under Miri — the smallest size covering the
    /// doorbell protocol pages (`REQ_GPA` `0xE000` / reply `0xF000`, production
    /// constants). The sha256 `state_hash` over the `MEM` chunk dominates these
    /// tests' interpreted cost and scales with this size (task 98 / hm-d8o).
    /// Native runs are byte-for-byte unchanged.
    const BIG_RAM: usize = if cfg!(miri) { 0x1_0000 } else { 0x2_0000 };

    const RAM: usize = 0x4000; // 16 KiB = 4 pages

    /// A configured, V-time-wired `Vmm<MockBackend>` with a distinctive memory
    /// image loaded and the canonical-blob hash wired (as the box composition
    /// does), advanced to a synchronized (post-RDTSC) boundary.
    fn vmm_at_sync(exits: Vec<Exit<X86>>, work: u64, seed: u64) -> Vmm<MockBackend> {
        vmm_at_sync_from(MockBackend::new(), exits, work, seed)
    }

    /// [`vmm_at_sync`] over a caller-prepared mock (e.g. one with dirty tracking
    /// enabled, task 95 M2.1) — the sync `Rdtsc` prelude + `exits` are
    /// **appended** to anything the mock already has scripted, so pass a mock
    /// with an empty exit script unless you mean to run yours first.
    fn vmm_at_sync_from(
        mut m: MockBackend,
        exits: Vec<Exit<X86>>,
        work: u64,
        seed: u64,
    ) -> Vmm<MockBackend> {
        let mut exits_with_sync = vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })];
        exits_with_sync.extend(exits);
        m.extend_exits(exits_with_sync);
        m.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
        let mut cfg = contract_vclock_config();
        cfg.vns_base = work.saturating_sub(1);
        v.wire_vtime(VtimeWiring::new_virtual_time(cfg, seed).unwrap());
        v.wire_snapshot_hashing();
        // Wire the userspace xAPIC so `InjectInterrupt` host faults are enforceable
        // on this generic test server (round-8 rejects a LAPIC-less InjectInterrupt).
        // IF stays 0, so a HLT is still terminal (no idle-resume) — the base hash
        // gains a LAPC chunk but every eq/ne relationship the tests assert holds.
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        let mut image = vec![0u8; RAM];
        image[..12].copy_from_slice(b"SERVER_BOOT\n");
        v.restore_guest_memory(&image).unwrap();
        assert_eq!(v.step().unwrap(), crate::vmm::Step::Continued); // RDTSC → synchronized
        v
    }

    /// A server whose live VM is at a synchronized point and whose factory
    /// boots fresh VMs scripted with `fork_exits` (each ending in `Hlt` so a
    /// deadline-free run terminates).
    fn server(fork_exits: Vec<Exit<X86>>) -> ControlServer<MockBackend> {
        let live = vmm_at_sync(vec![Exit::Common(CommonExit::Idle)], 500, 0xBA5E);
        let factory = Box::new(move || {
            let mut m = MockBackend::with_exits(fork_exits.clone());
            m.set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
            let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
            v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 0).unwrap());
            v.wire_snapshot_hashing();
            // Mirror the live VM's LAPIC wiring so a `branch`/`replay` restore
            // matches (and InjectInterrupt is enforceable on the fork too).
            v.wire_lapic(
                lapic::Lapic::new(lapic::LapicConfig {
                    apic_id: 0,
                    timer_hz: 24_000_000,
                })
                .unwrap(),
            );
            Ok(v)
        });
        with_test_service(ControlServer::new(live, factory))
    }

    #[derive(Clone)]
    struct BrokenCapture;
    impl environment::channel::ServiceHandler for BrokenCapture {
        fn identity(&self) -> &[u8] {
            b"test-broken-capture"
        }
        fn configuration(&self) -> &[u8] {
            &[]
        }
        fn respond(
            &mut self,
            _: environment::Moment,
            _: &environment::channel::Question,
        ) -> Result<environment::channel::ServiceResponse, environment::channel::ChannelError>
        {
            Ok(environment::channel::ServiceResponse::Answered(
                environment::channel::Answer::Nominal,
            ))
        }
        fn snapshot_state(&self) -> Result<Vec<u8>, environment::channel::ChannelError> {
            Err(environment::channel::ChannelError::Handler(
                "capture failed".into(),
            ))
        }
        fn restore_state(&mut self, _: &[u8]) -> Result<(), environment::channel::ChannelError> {
            Ok(())
        }
        fn clone_box(&self) -> Box<dyn environment::channel::ServiceHandler> {
            Box::new(self.clone())
        }
    }
    #[test]
    fn extension_capture_failure_cannot_create_a_snapshot_or_hash() {
        let mut s = server(vec![]);
        let config = ServiceConfig {
            identity: b"test-broken-capture".to_vec(),
            configuration: vec![],
        };
        s.vmm.as_mut().unwrap().enable_sdk(
            environment::channel::RecordedEnv::new(0, Box::new(BrokenCapture)),
            &config,
        );
        assert!(s.vmm.as_ref().unwrap().state_hash().is_err());
        assert!(matches!(s.snapshot(), Err(ServeError::Service(_))));
        assert!(s.vmm.is_none());
        assert!(s.sdk_snaps.is_empty());
    }

    /// [`server`] whose live VM's mock has **dirty tracking armed** (task 95
    /// M2.1), so a second seal can derive from the first.
    #[test]
    fn vmm_mut_reaches_the_live_vm() {
        let mut s = server(Vec::new());
        let shared = s.vmm().expect("live VM") as *const Vmm<MockBackend>;
        let exclusive = s.vmm_mut().expect("live VM") as *mut Vmm<MockBackend> as *const _;
        assert!(std::ptr::eq(shared, exclusive));
    }

    fn server_tracked() -> ControlServer<MockBackend> {
        let mut m = MockBackend::new();
        m.enable_dirty_tracking();
        let live = vmm_at_sync_from(m, vec![Exit::Common(CommonExit::Idle)], 500, 0xBA5E);
        // Mirror `server`'s factory (unused by the seal-path tests, present so
        // the composition invariant holds if one branches).
        let factory = Box::new(move || {
            let mut m = MockBackend::with_exits(vec![Exit::Common(CommonExit::Idle)]);
            m.set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
            let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
            v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 0).unwrap());
            v.wire_snapshot_hashing();
            v.wire_lapic(
                lapic::Lapic::new(lapic::LapicConfig {
                    apic_id: 0,
                    timer_hz: 24_000_000,
                })
                .unwrap(),
            );
            Ok(v)
        });
        with_test_service(ControlServer::new(live, factory))
    }

    /// [`server`] plus a remap-restore factory (task 95 M2.2) mirroring the
    /// memcpy factory's composition — built through the production
    /// `compose_restore_target`, so the portable A/B drives the real seam.
    /// `wire_lapic: false` mis-composes the target on purpose when
    /// `sabotage_lapic` (the RestoreFailed-recovery test's arm).
    fn server_with_remap(
        fork_exits: Vec<Exit<X86>>,
        sabotage_lapic: bool,
    ) -> ControlServer<MockBackend> {
        let mut s = server(fork_exits.clone());
        let remap: super::RemapVmmFactory<MockBackend> = Box::new(move |mapping| {
            let m = MockBackend::with_exits(fork_exits.clone());
            let mut v =
                crate::vendor::x86::bringup::compose_restore_target(m, mapping, !sabotage_lapic)?;
            v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 0).unwrap());
            v.wire_snapshot_hashing();
            Ok(v)
        });
        s.set_remap_factory(remap);
        s
    }

    fn hello(server: &mut ControlServer<MockBackend>) {
        let reply = server.handle(&Request::Hello(server_caps())).unwrap();
        assert_eq!(reply, Ok(Reply::Hello(server_caps())));
    }

    fn snap(server: &mut ControlServer<MockBackend>) -> SnapId {
        match server.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::Snapshot { id, .. }) => id,
            other => panic!("snapshot reply: {other:?}"),
        }
    }

    /// [`snap`] returning the full seal-bound reply fields — `(id, at,
    /// sdk_events, tainted)` — for the task-127 cut assertions.
    fn snap_cut(server: &mut ControlServer<MockBackend>) -> (SnapId, u64, u64, bool) {
        match server.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::Snapshot {
                id,
                at,
                sdk_events,
                tainted,
            }) => (id, at.0, sdk_events, tainted),
            other => panic!("snapshot reply: {other:?}"),
        }
    }

    fn seeded_env(seed: u64) -> Reproducer {
        let spec = EnvSpec::seeded(seed);
        Reproducer {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: spec.encode(),
        }
    }

    /// A branch reproducer with the M2 ordered payload source offered. The
    /// promotion to `Recorded` is deliberate: `Some([])` means offered but
    /// exhausted, unlike a bare `Seeded` spec where service 8 is unavailable.
    fn payload_env(seed: u64, payloads: Vec<Vec<u8>>) -> Reproducer {
        let mut spec = EnvSpec::seeded(seed);
        spec.set_payloads(Some(payloads));
        Reproducer {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: spec.encode(),
        }
    }

    /// A synchronized control server whose RAM covers the canonical doorbell
    /// pages. The compact generic `server` fixture intentionally has only
    /// 16 KiB, below `REQ_GPA`; this fixture mirrors its composition at 128 KiB.
    fn payload_server() -> ControlServer<MockBackend> {
        let mut backend = MockBackend::with_exits([Exit::Arch(X86Exit::Rdmsr { index: 0x10 })]);
        backend
            .set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
        let mut live = Vmm::new(backend, GuestRam::new(BIG_RAM).unwrap());
        live.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 0xBA5E).unwrap());
        live.wire_snapshot_hashing();
        live.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        live.restore_guest_memory(&vec![0_u8; BIG_RAM]).unwrap();
        assert_eq!(live.step().unwrap(), crate::vmm::Step::Continued);

        let factory = Box::new(|| {
            let mut backend = MockBackend::new();
            backend
                .set_policy(&X86Policy {
                    cpuid: vmm_backend::CpuidModel::default(),
                    msr_filter: vmm_backend::MsrFilter::default(),
                })
                .unwrap();
            let mut vmm = Vmm::new(backend, GuestRam::new(BIG_RAM).unwrap());
            vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 0).unwrap());
            vmm.wire_snapshot_hashing();
            vmm.wire_lapic(
                lapic::Lapic::new(lapic::LapicConfig {
                    apic_id: 0,
                    timer_hz: 24_000_000,
                })
                .unwrap(),
            );
            Ok(vmm)
        });
        with_test_service(ControlServer::new(live, factory))
    }

    #[derive(Clone)]
    struct ExternalService(u64);
    impl environment::channel::ServiceHandler for ExternalService {
        fn identity(&self) -> &[u8] {
            b"test-external-v1"
        }
        fn configuration(&self) -> &[u8] {
            &[]
        }
        fn respond(
            &mut self,
            _: environment::Moment,
            _: &environment::channel::Question,
        ) -> Result<environment::channel::ServiceResponse, environment::channel::ChannelError>
        {
            self.0 += 1; // External responses must roll this tentative mutation back.
            Ok(environment::channel::ServiceResponse::External)
        }
        fn snapshot_state(&self) -> Result<Vec<u8>, environment::channel::ChannelError> {
            Ok(self.0.to_le_bytes().to_vec())
        }
        fn restore_state(
            &mut self,
            bytes: &[u8],
        ) -> Result<(), environment::channel::ChannelError> {
            self.0 = u64::from_le_bytes(
                bytes
                    .try_into()
                    .map_err(|_| environment::channel::ChannelError::Malformed)?,
            );
            Ok(())
        }
        fn clone_box(&self) -> Box<dyn environment::channel::ServiceHandler> {
            Box::new(self.clone())
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "restores the complete VM through snapshot-store mmap; pure service codec and state transitions run under Miri"
    )]
    fn host_service_response_is_stopped_recorded_and_replayed() {
        use environment::channel::{Answer as ServiceAnswer, RecordedEnv};
        let mut s = payload_server();
        hello(&mut s);
        let config = ServiceConfig {
            identity: b"test-external-v1".to_vec(),
            configuration: vec![],
        };
        let expected_config = config.clone();
        s.set_service_factory(std::sync::Arc::new(move |config| {
            if config != &expected_config {
                return Err(environment::channel::ChannelError::Malformed);
            }
            Ok(Box::new(ExternalService(0)))
        }));
        s.recorded.set_config(config.clone());
        s.vmm.as_mut().unwrap().enable_sdk(
            RecordedEnv::new(s.recorded.seed(), Box::new(ExternalService(0))),
            &config,
        );
        let origin = snap(&mut s);
        let mut payload = 19_u16.to_le_bytes().to_vec();
        payload.extend(77_u64.to_le_bytes());
        payload.extend(b"choose");
        let mut frame = [0; 4096];
        let len = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Sdk,
            3,
            12,
            &payload,
            &mut frame,
        )
        .unwrap();
        let ring = |s: &mut ControlServer<MockBackend>| {
            let v = s.vmm.as_mut().unwrap();
            v.guest_slice_mut(0xe000, len)
                .unwrap()
                .copy_from_slice(&frame[..len]);
            v.service_doorbell(len as u32).unwrap()
        };
        assert_eq!(ring(&mut s), crate::vmm::Step::SdkStop);
        let at = s.vmm().unwrap().effective_vns().unwrap();
        let until = StopConditions {
            deadline: Some(Moment(at)),
            on: StopMask::NONE,
        };
        let request = Request::Run {
            until,
            resolve: None,
        };
        let stopped_hash = hash(&mut s);
        let expected = Reply::Stop(StopReason::Decision {
            vtime: Moment(at),
            id: control_proto::DecisionId(77),
            ctx: [19_u16.to_le_bytes().as_slice(), b"choose"].concat(),
        });
        assert_eq!(s.handle(&request).unwrap(), Ok(expected.clone()));
        assert_eq!(s.handle(&request).unwrap(), Ok(expected.clone()));
        assert_eq!(hash(&mut s), stopped_hash);
        let counts = s.vmm().unwrap().exit_counts();
        let pending = snap(&mut s);
        let mut artifact = Vec::new();
        let receipt = s.export_portable_snapshot(pending, &mut artifact).unwrap();
        assert_eq!(receipt.state_hash, stopped_hash);
        assert_eq!(receipt.at, Moment(at));
        assert_eq!(s.vmm().unwrap().exit_counts(), counts);
        assert_eq!(s.vmm().unwrap().effective_vns(), Some(at));
        assert_eq!(hash(&mut s), stopped_hash);
        assert_eq!(
            s.handle(&Request::Run {
                until,
                resolve: Some(Resolution {
                    vtime: Moment(at),
                    service: 19,
                    id: control_proto::DecisionId(77),
                    answer: Answer(vec![9]),
                }),
            })
            .unwrap(),
            Err(ControlError::MalformedEnvironment)
        );
        assert!(s.vmm().unwrap().pending_service_question().is_some());
        for (vtime, service, id) in [(at + 1, 19, 77), (at, 20, 77), (at, 19, 78)] {
            assert_eq!(
                s.handle(&Request::Run {
                    until,
                    resolve: Some(Resolution {
                        vtime: Moment(vtime),
                        service,
                        id: control_proto::DecisionId(id),
                        answer: Answer(ServiceAnswer::Nominal.encode()),
                    }),
                })
                .unwrap(),
                Err(ControlError::ResolveWithoutDecision)
            );
            assert_eq!(hash(&mut s), stopped_hash);
        }
        let oversized = ServiceAnswer::Data(vec![0; hypercall_proto::MAX_PAYLOAD]).encode();
        assert_eq!(
            s.handle(&Request::Run {
                until,
                resolve: Some(Resolution {
                    vtime: Moment(at),
                    service: 19,
                    id: control_proto::DecisionId(77),
                    answer: Answer(oversized),
                }),
            })
            .unwrap(),
            Err(ControlError::MalformedEnvironment)
        );
        assert_eq!(hash(&mut s), stopped_hash);
        // The full valid response frame includes one byte of answer framing.
        let answer = ServiceAnswer::Data(vec![0x5a; hypercall_proto::MAX_PAYLOAD - 1]);
        assert!(matches!(
            s.handle(&Request::Run {
                until,
                resolve: Some(Resolution {
                    vtime: Moment(at),
                    service: 19,
                    id: control_proto::DecisionId(77),
                    answer: Answer(answer.encode()),
                }),
            })
            .unwrap(),
            Ok(Reply::Stop(StopReason::Deadline { .. }))
        ));
        assert!(s.vmm().unwrap().pending_service_question().is_none());
        let expected_hash = hash(&mut s);
        let response = s.vmm().unwrap().guest_slice(0xf000, 4096).unwrap().to_vec();
        let (_, bytes) = hypercall_proto::decode(&response).unwrap();
        assert_eq!(ServiceAnswer::decode(bytes).unwrap(), answer);
        let recorded = s.recorded_env().clone();
        assert_eq!(recorded.answers().get(&(at, 19, 77)), Some(&answer));

        // A separate session restores the unanswered request from the complete
        // portable artifact. It must neither re-invoke the service nor invent
        // a response before the matching resolution arrives.
        let mut cold = payload_server();
        cold.set_service_factory(std::sync::Arc::clone(&s.service_factory));
        hello(&mut cold);
        let imported = cold.import_portable_snapshot(artifact.as_slice()).unwrap();
        cold.restore(imported.id, None).unwrap().unwrap();
        assert_eq!(hash(&mut cold), stopped_hash);
        assert_eq!(cold.handle(&request).unwrap(), Ok(expected));
        assert_eq!(cold.vmm().unwrap().effective_vns(), Some(at));
        assert!(matches!(
            cold.handle(&Request::Run {
                until,
                resolve: Some(Resolution {
                    vtime: Moment(at),
                    service: 19,
                    id: control_proto::DecisionId(77),
                    answer: Answer(answer.encode()),
                }),
            })
            .unwrap(),
            Ok(Reply::Stop(StopReason::Deadline { .. }))
        ));
        assert_eq!(
            cold.vmm().unwrap().guest_slice(0xf000, 4096).unwrap(),
            response
        );
        assert_eq!(hash(&mut cold), expected_hash);
        assert_eq!(
            cold.vmm().unwrap().sdk_events(),
            s.vmm().unwrap().sdk_events()
        );
        assert_eq!(
            cold.recorded_env().answers().get(&(at, 19, 77)),
            Some(&answer)
        );
        assert!(cold.vmm().unwrap().pending_service_question().is_none());
        s.restore(
            origin,
            Some(&Reproducer {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: recorded.encode(),
            }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(ring(&mut s), crate::vmm::Step::Continued);
        assert_eq!(
            s.vmm().unwrap().guest_slice(0xf000, 4096).unwrap(),
            response
        );
        assert_eq!(hash(&mut s), expected_hash);
    }

    #[test]
    fn restore_rejects_factory_identity_or_configuration_mismatch_without_mutation() {
        let mut s = payload_server();
        hello(&mut s);
        let origin = snap(&mut s);
        let before = s.vmm().unwrap().state_hash().unwrap();
        // A resolver must independently honor both pieces of the requested
        // contract, even when it returns an otherwise usable implementation.
        for config in [
            ServiceConfig {
                identity: b"different-service".to_vec(),
                configuration: vec![7],
            },
            ServiceConfig {
                identity: b"test-service-v1".to_vec(),
                configuration: vec![8],
            },
        ] {
            s.set_service_factory(std::sync::Arc::new(|_| {
                Ok(Box::new(TestService {
                    response: vec![7],
                    calls: 0,
                }))
            }));
            let mut spec = EnvSpec::seeded(1);
            spec.set_config(config);
            assert_eq!(
                s.handle(&Request::Branch {
                    snap: origin,
                    env: Reproducer {
                        blob_version: EnvSpec::BLOB_VERSION,
                        bytes: spec.encode()
                    },
                })
                .unwrap(),
                Err(ControlError::Unsupported)
            );
            assert_eq!(s.vmm().unwrap().state_hash().unwrap(), before);
        }
    }

    #[test]
    fn stale_service_resolution_cannot_answer_a_later_decision() {
        use environment::channel::RecordedEnv;

        let mut s = payload_server();
        hello(&mut s);
        let config = ServiceConfig {
            identity: b"test-external-v1".to_vec(),
            configuration: vec![],
        };
        let expected_config = config.clone();
        s.set_service_factory(std::sync::Arc::new(move |config| {
            if config != &expected_config {
                return Err(environment::channel::ChannelError::Malformed);
            }
            Ok(Box::new(ExternalService(0)))
        }));
        s.recorded.set_config(config.clone());
        s.vmm.as_mut().unwrap().enable_sdk(
            RecordedEnv::new(s.recorded.seed(), Box::new(ExternalService(0))),
            &config,
        );

        let ring = |s: &mut ControlServer<MockBackend>, request_id: u64, seq: u32| {
            let mut payload = 19_u16.to_le_bytes().to_vec();
            payload.extend(request_id.to_le_bytes());
            payload.extend(b"choose");
            let mut frame = [0; 4096];
            let len = hypercall_proto::encode_request(
                hypercall_proto::ServiceId::Sdk,
                3,
                seq,
                &payload,
                &mut frame,
            )
            .unwrap();
            let v = s.vmm.as_mut().unwrap();
            v.guest_slice_mut(0xe000, len)
                .unwrap()
                .copy_from_slice(&frame[..len]);
            v.service_doorbell(len as u32).unwrap()
        };

        assert_eq!(ring(&mut s, 77, 12), crate::vmm::Step::SdkStop);
        let at = s.vmm().unwrap().effective_vns().unwrap();
        let until = StopConditions {
            deadline: Some(Moment(at)),
            on: StopMask::NONE,
        };
        assert!(matches!(
            s.handle(&Request::Run {
                until,
                resolve: None,
            })
            .unwrap(),
            Ok(Reply::Stop(StopReason::Decision {
                id: control_proto::DecisionId(77),
                ..
            }))
        ));

        let first = environment::channel::Answer::Data(b"first".to_vec());
        assert!(matches!(
            s.handle(&Request::Run {
                until,
                resolve: Some(Resolution {
                    vtime: Moment(at),
                    service: 19,
                    id: control_proto::DecisionId(77),
                    answer: Answer(first.encode()),
                }),
            })
            .unwrap(),
            Ok(Reply::Stop(StopReason::Deadline { .. }))
        ));

        assert_eq!(ring(&mut s, 88, 13), crate::vmm::Step::SdkStop);
        assert!(matches!(
            s.handle(&Request::Run {
                until,
                resolve: None,
            })
            .unwrap(),
            Ok(Reply::Stop(StopReason::Decision {
                id: control_proto::DecisionId(88),
                ..
            }))
        ));

        // A retry of the first resolution must not be accepted for request 88.
        assert_eq!(
            s.handle(&Request::Run {
                until,
                resolve: Some(Resolution {
                    vtime: Moment(at),
                    service: 19,
                    id: control_proto::DecisionId(77),
                    answer: Answer(first.encode()),
                }),
            })
            .unwrap(),
            Err(ControlError::ResolveWithoutDecision)
        );
        assert_eq!(
            s.vmm()
                .unwrap()
                .pending_service_question()
                .map(|(_, q)| q.request_id()),
            Some(88)
        );

        let second = environment::channel::Answer::Data(b"second".to_vec());
        assert!(matches!(
            s.handle(&Request::Run {
                until,
                resolve: Some(Resolution {
                    vtime: Moment(at),
                    service: 19,
                    id: control_proto::DecisionId(88),
                    answer: Answer(second.encode()),
                }),
            })
            .unwrap(),
            Ok(Reply::Stop(StopReason::Deadline { .. }))
        ));
        assert_eq!(s.recorded_env().answers().get(&(at, 19, 88)), Some(&second));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "restores the complete VM through snapshot-store mmap; the service configuration assertion is a control-server replay invariant"
    )]
    fn replay_restores_generic_service_configuration_and_recorded_env() {
        let mut s = payload_server();
        hello(&mut s);
        let origin = snap(&mut s);
        let config_a = ServiceConfig {
            identity: b"test-service-v1".to_vec(),
            configuration: b"configuration-a".to_vec(),
        };
        let config_b = ServiceConfig {
            identity: b"test-service-v1".to_vec(),
            configuration: b"configuration-b".to_vec(),
        };
        let branch_env = |seed, config: ServiceConfig| {
            let mut spec = EnvSpec::seeded(seed);
            spec.set_config(config);
            Reproducer {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: spec.encode(),
            }
        };

        assert_eq!(
            s.handle(&Request::Branch {
                snap: origin,
                env: branch_env(0xA, config_a.clone()),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(s.recorded_env().config(), &config_a);
        let config_a_snapshot = snap(&mut s);

        assert_eq!(
            s.handle(&Request::Branch {
                snap: config_a_snapshot,
                env: branch_env(0xB, config_b.clone()),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(s.recorded_env().config(), &config_b);

        assert_eq!(
            s.handle(&Request::Replay(config_a_snapshot)).unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(s.recorded_env().config(), &config_a);
        assert_eq!(
            EnvSpec::decode(&s.recorded_env().encode())
                .unwrap()
                .config(),
            &config_a
        );
    }

    /// Stage one payload request in the canonical transport page.
    fn stage_payload_request(server: &mut ControlServer<MockBackend>, bytes: u32) -> u32 {
        const REQ_GPA: u64 = 0xE000;
        let mut frame = [0_u8; 4096];
        let n = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Payload,
            1,
            1,
            &bytes.to_le_bytes(),
            &mut frame,
        )
        .unwrap();
        server
            .vmm
            .as_mut()
            .unwrap()
            .guest_slice_mut(REQ_GPA, n)
            .unwrap()
            .copy_from_slice(&frame[..n]);
        u32::try_from(n).unwrap()
    }

    /// Ring a staged payload request directly and return its framed reply.
    fn ring_payload(server: &mut ControlServer<MockBackend>, bytes: u32) -> (u16, Vec<u8>) {
        const RESP_GPA: u64 = 0xF000;
        let n = stage_payload_request(server, bytes);
        let vmm = server.vmm.as_mut().unwrap();
        assert!(matches!(
            vmm.dispatch_out(0x0CA1, 4, n).unwrap(),
            crate::vmm::Step::Continued
        ));
        let page = vmm.guest_slice(RESP_GPA, 4096).unwrap();
        let (header, payload) = hypercall_proto::decode(page).unwrap();
        (header.status, payload.to_vec())
    }

    fn run_all(server: &mut ControlServer<MockBackend>) -> StopReason {
        let req = Request::Run {
            until: StopConditions {
                deadline: None,
                on: StopMask::NONE,
            },
            resolve: None,
        };
        match server.handle(&req).unwrap() {
            Ok(Reply::Stop(stop)) => stop,
            other => panic!("run reply: {other:?}"),
        }
    }

    /// Drive the production control replacement path twice so the returned
    /// evidence contains the initial VMM plus two replay-delimited segments.
    fn accumulated_session_server() -> ControlServer<MockArm64Backend> {
        let make_vmm = |exits: Vec<Exit<Arm64>>, seed: u64| {
            let mut backend = MockArm64Backend::with_exits(exits);
            backend.set_policy(&Arm64Policy::default()).unwrap();
            let mut vmm = Vmm::new(backend, GuestRam::new(RAM).unwrap());
            vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), seed).unwrap());
            vmm.wire_snapshot_hashing();
            vmm
        };
        let serial = || {
            Exit::Common(CommonExit::Mmio {
                gpa: vmm_backend::Gpa(crate::vendor::arm64::board::PL011.0),
                size: 1,
                write: Some(u64::from(b'x')),
            })
        };
        let mut live = make_vmm(vec![serial(), Exit::Common(CommonExit::Idle)], 0xBA5E);
        assert_eq!(live.step().unwrap(), crate::vmm::Step::Continued);
        let factory =
            Box::new(move || Ok(make_vmm(vec![serial(), Exit::Common(CommonExit::Idle)], 0)));
        let mut server = with_test_service(ControlServer::new(live, factory));
        assert_eq!(
            server.handle(&Request::Hello(server_caps())).unwrap(),
            Ok(Reply::Hello(server_caps()))
        );
        let base = match server.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::Snapshot { id, .. }) => id,
            other => panic!("snapshot reply: {other:?}"),
        };
        for _ in 0..2 {
            assert_eq!(
                server.handle(&Request::Replay(base)).unwrap(),
                Ok(Reply::Unit)
            );
            let reply = server
                .handle(&Request::Run {
                    until: StopConditions {
                        deadline: None,
                        on: StopMask::NONE,
                    },
                    resolve: None,
                })
                .unwrap();
            assert!(matches!(
                reply,
                Ok(Reply::Stop(StopReason::Quiescent { .. }))
            ));
        }
        server
    }

    fn accumulated_session_trace() -> crate::session_trace::SessionVirtualTimeTrace {
        accumulated_session_server()
            .session_virtual_time_trace()
            .expect("virtual_time control server produces a session trace")
    }

    /// The load-bearing positive oracle for restore-aware accumulation: two
    /// independently driven servers retain and compare every replacement
    /// segment, rather than agreeing only on the final live VMM's suffix.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot materialize through two production Replay verbs; the pure session comparator and negative control remain Miri-covered"
    )]
    fn control_session_accumulates_every_restore_delimited_trace() {
        use crate::session_trace::{
            SessionTraceStart, check_session_delivery_placement, compare_session_traces,
        };

        let left = accumulated_session_trace();
        let right = accumulated_session_trace();
        assert_eq!(left.segments().len(), 3);
        assert_eq!(left.segments()[0].start(), SessionTraceStart::InitialBoot);
        assert_eq!(
            left.segments()[1].start(),
            SessionTraceStart::Replay { snapshot: 1 }
        );
        assert_eq!(
            left.segments()[2].start(),
            SessionTraceStart::Replay { snapshot: 1 }
        );
        assert!(
            left.event_count() > left.segments()[2].normalized_log().events.len(),
            "the session oracle must cover more than the final VMM suffix: total={}, segments={:?}",
            left.event_count(),
            left.segments()
                .iter()
                .map(|segment| segment.normalized_log().events.len())
                .collect::<Vec<_>>()
        );
        assert_eq!(compare_session_traces(&left, &right), Ok(()));
        assert_eq!(check_session_delivery_placement(&left), Ok(()));
        assert_eq!(left.digest(), right.digest());
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot materialize through two production Replay verbs; the pure session comparator and negative control remain Miri-covered"
    )]
    fn taking_the_session_trace_returns_and_drains_completed_segments() {
        let mut server = accumulated_session_server();
        let before = server.vmm().unwrap().state_hash().unwrap();
        let viewed = server.session_virtual_time_trace().unwrap();
        let taken = server.take_session_virtual_time_trace().unwrap();
        assert_eq!(taken, viewed);
        assert_eq!(taken.segments().len(), 3);

        // Taking drains only the completed host-side buffer. The live final
        // segment remains available and is captured again without mutation.
        let live_only = server.take_session_virtual_time_trace().unwrap();
        assert_eq!(live_only.segments().len(), 1);
        assert_eq!(live_only.segments()[0], taken.segments()[2]);
        assert_eq!(server.vmm().unwrap().state_hash().unwrap(), before);
        for _ in 0..8 {
            assert_eq!(server.take_session_virtual_time_trace().unwrap(), live_only);
            assert_eq!(server.vmm().unwrap().state_hash().unwrap(), before);
        }
    }

    // ---- task 95 M2: O(dirty) capture + remap restore -------------------------

    /// The store-side chain length of a wire handle (1 = a base layer; >1 = a
    /// derived capture) — through the same public accessor the box gate uses,
    /// so the accessor's handle→stats mapping is exercised portably too.
    fn chain_len(s: &ControlServer<MockBackend>, id: SnapId) -> u32 {
        s.snapshot_chain_len(id).expect("handle is live")
    }

    /// M2.1 wiring: with tracking armed, the second seal derives from the first
    /// (chain 2), the drained set covers a host-side write, and the derived
    /// snapshot materializes exactly the live image — byte-identical to what a
    /// full-scan base seal of the same state stores.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "materialize uses mmap, which Miri cannot execute; the seal-path logic is covered by the non-mmap tests"
    )]
    fn seal_derives_from_tracked_parent_and_reproduces_the_image() {
        let mut s = server_tracked();
        hello(&mut s);
        let first = snap(&mut s);
        // Mutate guest state host-side between the seals (the CorruptMemory
        // apply path — a write KVM's log could never see).
        s.vmm
            .as_mut()
            .unwrap()
            .apply_effect(&EnvHostEffect::XorMemory {
                gpa: 0x40,
                bytes: (0xDEAD_BEEF_u64).to_le_bytes().to_vec(),
            })
            .unwrap();
        let second = snap(&mut s);
        assert_eq!(chain_len(&s, first), 1, "first seal is the base");
        assert_eq!(chain_len(&s, second), 2, "second seal derived (O(dirty))");
        // Full closure: the derived capture resolves to exactly the live image.
        let map = s.engine.materialize(s.snaps[&second.0]).unwrap();
        assert_eq!(map.as_slice(), s.vmm().unwrap().guest_memory());
        // And a base seal of the same state stores the same bytes.
        let base_twin = {
            let vmm = s.vmm.as_ref().unwrap();
            s.engine.snapshot_base(vmm.guest_memory(), b"twin").unwrap()
        };
        let twin = s.engine.materialize(base_twin).unwrap();
        assert_eq!(map.as_slice(), twin.as_slice());
    }

    /// PR #95 round-1: the restore mode is truthful — `Memcpy` until a remap
    /// factory exists (the only path a factory-less server can take), flipped
    /// to `Remap` by installing one. No silent degrade.
    #[test]
    fn restore_mode_is_memcpy_until_a_remap_factory_installs() {
        let s = server(vec![Exit::Common(CommonExit::Idle)]);
        assert_eq!(s.restore_mode(), super::RestoreMode::Memcpy);
        let s = server_with_remap(vec![Exit::Common(CommonExit::Idle)], false);
        assert_eq!(s.restore_mode(), super::RestoreMode::Remap);
    }

    #[test]
    fn restore_mode_defers_materialization_only_for_in_place() {
        assert!(restore_defers_materialization(super::RestoreMode::InPlace));
        assert!(!restore_defers_materialization(super::RestoreMode::Remap));
        assert!(!restore_defers_materialization(super::RestoreMode::Memcpy));
    }

    #[test]
    fn post_commit_restore_errors_are_never_recoverable_rejections() {
        assert!(restore_error_is_precommit(&VmmError::ContractViolation(
            "validation".to_owned()
        )));
        assert!(!restore_error_is_precommit(&VmmError::Backend(
            vmm_backend::BackendError::Internal("already mutated")
        )));
    }

    #[test]
    fn sparse_page_order_check_distinguishes_sorted_duplicate_and_descending() {
        assert!(sparse_page_order_error(None, 7).is_none());
        assert!(matches!(
            sparse_page_order_error(Some(3), 3),
            Some(crate::snapshot::SnapshotError::SparsePageDuplicate { gfn: 3 })
        ));
        assert!(matches!(
            sparse_page_order_error(Some(3), 1),
            Some(crate::snapshot::SnapshotError::SparsePagesNotSorted {
                previous: 3,
                current: 1,
            })
        ));
        assert!(sparse_page_order_error(Some(1), 3).is_none());
    }

    /// The safety default: without backend dirty tracking every seal full-scans
    /// (base layers throughout) — nothing ever derives on an unvouched window.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated snapshot-seal/hash logic over the mock server VM (each seal state-hashes + page-hashes the image, ~2 s/KiB under Miri); pure safe code — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the seal/hash family keeps Miri-run siblings incl. snapshot_mints_fresh_handles_and_drop_releases_them and the deferred-snapshot-boundary tests (task 98 / hm-d8o)"
    )]
    fn untracked_seals_always_full_scan() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let first = snap(&mut s);
        let second = snap(&mut s);
        assert_eq!(chain_len(&s, first), 1);
        assert_eq!(chain_len(&s, second), 1, "no tracking ⇒ base, never derive");
    }

    /// The chain bound (M2.1): at `max_chain_len` the seal flattens via a fresh
    /// base instead of deriving deeper.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated snapshot-seal/hash logic over the mock server VM (each seal state-hashes + page-hashes the image, ~2 s/KiB under Miri); pure safe code — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the seal/hash family keeps Miri-run siblings incl. snapshot_mints_fresh_handles_and_drop_releases_them and the deferred-snapshot-boundary tests (task 98 / hm-d8o)"
    )]
    fn seal_flattens_at_the_chain_bound() {
        let mut s = server_tracked();
        s.set_max_chain_len(2);
        hello(&mut s);
        let a = snap(&mut s);
        let b = snap(&mut s);
        s.vmm
            .as_mut()
            .unwrap()
            .apply_effect(&EnvHostEffect::XorMemory {
                gpa: 2 * 4096 + 0x40,
                bytes: (0xABCD_1234_u64).to_le_bytes().to_vec(),
            })
            .unwrap();
        let c = snap(&mut s);
        assert_eq!(chain_len(&s, a), 1);
        assert_eq!(chain_len(&s, b), 2, "under the bound: derive");
        assert_eq!(chain_len(&s, c), 1, "at the bound: flatten to a base");
        assert_eq!(
            s.last_seal_dirty_gfns(),
            Some(&[2][..]),
            "the bound seal drains and reports its exact dirty window"
        );
    }

    /// A released parent (the client dropped the handle) makes the next seal
    /// fall back to a base — the parent-liveness check of the safety rule.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated snapshot-seal/hash logic over the mock server VM (each seal state-hashes + page-hashes the image, ~2 s/KiB under Miri); pure safe code — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the seal/hash family keeps Miri-run siblings incl. snapshot_mints_fresh_handles_and_drop_releases_them and the deferred-snapshot-boundary tests (task 98 / hm-d8o)"
    )]
    fn seal_falls_back_to_base_when_the_parent_was_dropped() {
        let mut s = server_tracked();
        hello(&mut s);
        let first = snap(&mut s);
        assert_eq!(s.handle(&Request::Drop(first)).unwrap(), Ok(Reply::Unit));
        assert_eq!(
            s.current_image, None,
            "dropping the tracked image clears it"
        );
        assert_eq!(
            s.derive_parent, None,
            "the dropped image cannot derive again"
        );
        let second = snap(&mut s);
        assert_eq!(chain_len(&s, second), 1, "dead parent ⇒ full scan");
    }

    /// An untrackable full-image host write between seals forces the fallback
    /// (the wholesale poison), and the state still captures correctly.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated snapshot-seal/hash logic over the mock server VM (each seal state-hashes + page-hashes the image, ~2 s/KiB under Miri); pure safe code — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the seal/hash family keeps Miri-run siblings incl. snapshot_mints_fresh_handles_and_drop_releases_them and the deferred-snapshot-boundary tests (task 98 / hm-d8o)"
    )]
    fn seal_falls_back_after_a_wholesale_host_write() {
        let mut s = server_tracked();
        hello(&mut s);
        let _first = snap(&mut s);
        let image = vec![0x5Au8; RAM];
        s.vmm
            .as_mut()
            .unwrap()
            .restore_guest_memory(&image)
            .unwrap();
        let second = snap(&mut s);
        assert_eq!(chain_len(&s, second), 1, "wholesale write ⇒ full scan");
    }

    /// M2.2's determinism A/B (the portable arm of box gate b): branching the
    /// same snapshot with the same env under `Memcpy` and under `Remap` yields
    /// bit-identical guest memory, identical run outcomes, and identical
    /// `state_hash` — and the remap arm really is mapping-backed (no memcpy).
    #[test]
    #[cfg_attr(
        miri,
        ignore = "materialize uses mmap, which Miri cannot execute; the mode plumbing is covered by the non-mmap tests"
    )]
    fn branch_remap_and_memcpy_agree_bit_for_bit() {
        let mut s = server_with_remap(
            vec![
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                Exit::Common(CommonExit::Idle),
            ],
            false,
        );
        hello(&mut s);
        let sp = snap(&mut s);

        s.set_restore_mode(super::RestoreMode::Memcpy);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: sp,
                env: seeded_env(7)
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        assert!(!s.vmm().unwrap().ram_backing_is_snapshot());
        let stop_memcpy = run_all(&mut s);
        let mem_memcpy = s.vmm().unwrap().guest_memory().to_vec();
        let hash_memcpy = s.handle(&Request::Hash {
            scope: HashScope::Whole,
        });

        s.set_restore_mode(super::RestoreMode::Remap);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: sp,
                env: seeded_env(7)
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        assert!(
            s.vmm().unwrap().ram_backing_is_snapshot(),
            "the remap arm's guest RAM is the materialized mapping itself"
        );
        let stop_remap = run_all(&mut s);
        let mem_remap = s.vmm().unwrap().guest_memory().to_vec();
        let hash_remap = s.handle(&Request::Hash {
            scope: HashScope::Whole,
        });

        assert_eq!(stop_memcpy, stop_remap);
        assert_eq!(mem_memcpy, mem_remap);
        assert_eq!(hash_memcpy.unwrap(), hash_remap.unwrap());
    }

    /// A live-dirty page absent from the store-side snapshot diff is still
    /// restored from the target image, and the operation keeps the same VMM.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "snapshot materialization and page hashing use mmap-backed production paths"
    )]
    fn in_place_restore_unions_live_dirty_pages_and_reports_exact_bytes() {
        let mut s = server_tracked();
        hello(&mut s);
        let target = snap(&mut s);
        let expected_hash = s
            .handle(&Request::Hash {
                scope: HashScope::Whole,
            })
            .unwrap();

        s.vmm
            .as_mut()
            .unwrap()
            .apply_effect(&EnvHostEffect::XorMemory {
                gpa: 3 * 4096,
                bytes: (0xA5A5_5A5A_u64).to_le_bytes().to_vec(),
            })
            .unwrap();
        s.set_restore_mode(super::RestoreMode::InPlace);
        assert_eq!(s.handle(&Request::Replay(target)).unwrap(), Ok(Reply::Unit));

        assert_eq!(s.in_place_fallbacks(), 0);
        assert_eq!(s.last_restore_bytes_written(), 4096);
        assert_eq!(
            s.handle(&Request::Hash {
                scope: HashScope::Whole,
            })
            .unwrap(),
            expected_hash
        );
    }

    /// A fresh/untracked server has no source image and no readable dirty log.
    /// The target diff is nevertheless complete, so in-place restore is safe
    /// and must not fall back merely because the log is unavailable.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "the fallback mutant would materialize a snapshot-store mapping, which Miri cannot execute"
    )]
    fn in_place_restore_without_source_image_accepts_missing_dirty_log() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let target = snap(&mut s);

        assert_eq!(s.current_image, None);
        s.set_restore_mode(super::RestoreMode::InPlace);
        assert_eq!(s.handle(&Request::Replay(target)).unwrap(), Ok(Reply::Unit));
        assert_eq!(s.in_place_fallbacks(), 0);
        assert_eq!(s.last_restore_bytes_written(), RAM as u64);
    }

    /// Once a source image exists, an unavailable dirty log is not safe: a
    /// wholesale live write may have changed pages absent from the store diff.
    /// The restore must take the fresh-VM fallback rather than accepting an
    /// empty patch as an in-place success.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "the fallback path materializes a snapshot-store mapping, which Miri cannot execute"
    )]
    fn in_place_restore_with_a_source_image_rejects_missing_dirty_log() {
        let mut s = server_tracked();
        hello(&mut s);
        let target = snap(&mut s);
        let expected_hash = s
            .handle(&Request::Hash {
                scope: HashScope::Whole,
            })
            .unwrap();

        s.vmm
            .as_mut()
            .unwrap()
            .restore_guest_memory(&vec![0xA5; RAM])
            .unwrap();
        assert_eq!(s.current_image, s.snaps.get(&target.0).copied());
        s.set_restore_mode(super::RestoreMode::InPlace);
        assert_eq!(s.handle(&Request::Replay(target)).unwrap(), Ok(Reply::Unit));
        assert_eq!(s.in_place_fallbacks(), 1);
        assert_eq!(s.last_restore_bytes_written(), 0);
        assert_eq!(
            s.handle(&Request::Hash {
                scope: HashScope::Whole,
            })
            .unwrap(),
            expected_hash
        );
    }

    /// A lost source image makes the optimization unavailable, so the restore
    /// uses the established fresh-VM path and records exactly one fallback.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "snapshot materialization and page hashing use mmap-backed production paths"
    )]
    fn in_place_restore_falls_back_when_the_source_image_is_gone() {
        let mut s = server_tracked();
        hello(&mut s);
        let target = snap(&mut s);
        let expected_hash = s
            .handle(&Request::Hash {
                scope: HashScope::Whole,
            })
            .unwrap();
        s.vmm
            .as_mut()
            .unwrap()
            .apply_effect(&EnvHostEffect::XorMemory {
                gpa: 4096,
                bytes: (0x55AA_33CC_u64).to_le_bytes().to_vec(),
            })
            .unwrap();
        let lost_source = s
            .engine
            .snapshot_base(s.vmm.as_ref().unwrap().guest_memory(), b"stale-source")
            .unwrap();
        s.engine.release(lost_source).unwrap();
        s.engine.gc();
        s.current_image = Some(lost_source);

        s.set_restore_mode(super::RestoreMode::InPlace);
        assert_eq!(s.handle(&Request::Replay(target)).unwrap(), Ok(Reply::Unit));
        assert_eq!(s.in_place_fallbacks(), 1);
        assert_eq!(s.last_restore_bytes_written(), 0);
        assert_eq!(s.current_image, None, "fresh mock factory is untracked");
        assert_eq!(
            s.handle(&Request::Hash {
                scope: HashScope::Whole,
            })
            .unwrap(),
            expected_hash
        );
    }

    /// A remap-path restore that rejects (mis-composed target: the snapshot has
    /// an xAPIC, the target none) answers the recoverable `RestoreFailed` and
    /// leaves the session on a genuine fresh boot — usable, exactly like the
    /// memcpy path's recovery.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "materialize uses mmap, which Miri cannot execute; the mode plumbing is covered by the non-mmap tests"
    )]
    fn remap_restore_failure_keeps_a_usable_session() {
        let mut s = server_with_remap(vec![Exit::Common(CommonExit::Idle)], true); // sabotaged target
        hello(&mut s);
        let sp = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: sp,
                env: seeded_env(7)
            })
            .unwrap(),
            Err(ControlError::RestoreFailed)
        );
        // The kept VM is a fresh boot from the NORMAL factory (owned RAM, not a
        // half-restored shell), and the session still answers verbs.
        assert!(!s.vmm().unwrap().ram_backing_is_snapshot());
        assert!(matches!(
            s.handle(&Request::Hash {
                scope: HashScope::Whole
            })
            .unwrap(),
            Ok(Reply::Hash(_))
        ));
    }

    /// Like `run_all` but arms the SDK `SNAPSHOT_POINT` class (round-7), so a
    /// deferred `setup_complete` point surfaces (the default `StopMask::NONE` now
    /// runs straight through it).
    fn run_seeking_snapshot(server: &mut ControlServer<MockBackend>) -> StopReason {
        let req = Request::Run {
            until: StopConditions {
                deadline: None,
                on: StopMask::NONE.arm(control_proto::class_bit::SNAPSHOT_POINT),
            },
            resolve: None,
        };
        match server.handle(&req).unwrap() {
            Ok(Reply::Stop(stop)) => stop,
            other => panic!("run reply: {other:?}"),
        }
    }

    /// A deadline-free `run` returning the raw reply (for the loud-error paths).
    fn run_all_res(server: &mut ControlServer<MockBackend>) -> Result<Reply, ControlError> {
        server
            .handle(&Request::Run {
                until: StopConditions {
                    deadline: None,
                    on: StopMask::NONE,
                },
                resolve: None,
            })
            .unwrap()
    }

    fn hash(server: &mut ControlServer<MockBackend>) -> [u8; 32] {
        let req = Request::Hash {
            scope: HashScope::Whole,
        };
        match server.handle(&req).unwrap() {
            Ok(Reply::Hash(h)) => h,
            other => panic!("hash reply: {other:?}"),
        }
    }

    #[test]
    fn hello_negotiates_the_pinned_caps() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let caps = server_caps();
        assert_eq!(caps.protocol_version, control_proto::APP_PROTOCOL_VERSION);
        assert_eq!(
            caps.protocol_version, 13,
            "protocol version numbers remain monotonic after retired tags"
        );
        assert_eq!(caps.env_version_min, EnvSpec::BLOB_VERSION);
        assert_eq!(caps.env_version_max, EnvSpec::BLOB_VERSION);
        assert_eq!(caps.coverage.map_bytes, 0, "no coverage producer exists");
        assert_eq!(caps.coverage.producer, 0);
        assert!(
            caps.flags.contains(CapFlags::GUEST_HAS_SDK),
            "task 73 services the doorbell, so GUEST_HAS_SDK is advertised"
        );
    }

    #[test]
    fn hello_rejects_an_application_version_mismatch_before_opening_the_session() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        let mut old = server_caps();
        old.protocol_version = old.protocol_version.checked_sub(1).unwrap();
        assert_eq!(
            s.handle(&Request::Hello(old)).unwrap(),
            Err(ControlError::Unsupported)
        );
        assert_eq!(
            s.handle(&Request::Snapshot).unwrap(),
            Err(ControlError::Unsupported),
            "a rejected hello must not enable later verbs"
        );
        hello(&mut s);
    }

    /// The `SdkEvents` verb (task 73) is routed to the live VM's capture — a
    /// mock guest that never rings the doorbell yields an empty `SdkEvents` reply
    /// (not `Unsupported`), so a remote client always gets the capture over the
    /// wire.
    #[test]
    fn sdk_events_verb_is_routed_to_the_capture() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        match s.handle(&Request::SdkEvents { offset: 0 }).unwrap() {
            Ok(Reply::SdkEvents(events)) => {
                assert!(events.is_empty(), "the mock guest emits no doorbell events")
            }
            other => panic!("SdkEvents verb answered unexpectedly (paged): {other:?}"),
        }
    }

    // ------------------------- task 80: observation verbs ----------------------

    fn read(
        server: &mut ControlServer<MockBackend>,
        gpa: u64,
        len: u32,
    ) -> Result<Reply, ControlError> {
        server.handle(&Request::Read { gpa, len }).unwrap()
    }

    fn regs(server: &mut ControlServer<MockBackend>) -> control_proto::RegsView {
        match server.handle(&Request::Regs).unwrap() {
            Ok(Reply::Regs(v)) => v,
            other => panic!("regs reply: {other:?}"),
        }
    }

    /// `read` returns exactly the guest bytes at `[gpa, gpa+len)` — here the boot
    /// marker the fixture loads at offset 0.
    #[test]
    fn read_returns_the_guest_bytes() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        match read(&mut s, 0, 12) {
            Ok(Reply::Bytes(b)) => assert_eq!(&b, b"SERVER_BOOT\n"),
            other => panic!("read reply: {other:?}"),
        }
        // A zero-length read at the exact RAM end is a valid empty read (the
        // boundary is inclusive: `gpa + len == ram_len` is in range).
        assert_eq!(read(&mut s, RAM as u64, 0), Ok(Reply::Bytes(Vec::new())));
        assert_eq!(
            read(&mut s, RAM as u64 - 4, 4),
            Ok(Reply::Bytes(vec![0u8; 4])),
            "a read ending exactly at ram_len is in range"
        );
    }

    /// A `[gpa, gpa+len)` range past guest RAM (or an address+len that would
    /// overflow `u64`) is a loud `ReadOutOfRange` — never a truncated/zero-filled
    /// success.
    #[test]
    fn read_out_of_range_is_loud() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        assert_eq!(
            read(&mut s, RAM as u64 - 3, 4),
            Err(ControlError::ReadOutOfRange {
                gpa: RAM as u64 - 3,
                len: 4,
                ram_len: RAM as u64,
            }),
            "one byte past the end is rejected, not clipped"
        );
        assert_eq!(
            read(&mut s, u64::MAX - 2, 8),
            Err(ControlError::ReadOutOfRange {
                gpa: u64::MAX - 2,
                len: 8,
                ram_len: RAM as u64,
            }),
            "a gpa+len that would overflow u64 is rejected, never wrapped"
        );
    }

    /// A `len` over the per-call cap is `ReadTooLarge`, checked **before** the range
    /// (so even an over-cap read at a huge address is the cap error, not a slice) and
    /// before any allocation.
    #[test]
    fn read_oversized_len_is_loud() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        assert_eq!(
            read(&mut s, 0, READ_CAP + 1),
            Err(ControlError::ReadTooLarge {
                len: READ_CAP + 1,
                cap: READ_CAP,
            })
        );
        assert_eq!(
            read(&mut s, u64::MAX, u32::MAX),
            Err(ControlError::ReadTooLarge {
                len: u32::MAX,
                cap: READ_CAP,
            }),
            "the cap is checked before the range, so no slice is attempted"
        );
    }

    /// `regs` reports the current versioned view; `moment` and `vtime` are the two
    /// names of the single V-time axis, so both equal the live effective V-time.
    #[test]
    fn regs_reports_the_versioned_view_at_the_current_moment() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let v = regs(&mut s);
        assert_eq!(v.version, control_proto::RegsView::VERSION);
        let vns = s.vmm().unwrap().effective_vns().unwrap();
        assert_eq!(v.moment.0, vns, "moment is the current V-time");
        assert_eq!(v.vtime, vns, "vtime and moment coincide on the single axis");
        assert_eq!(v.moment.0, 500, "the fixture is wired at exit count 500");
    }

    /// Both observation verbs are subject to the `hello`-first gate — before a
    /// session is negotiated nothing is supported.
    #[test]
    fn observations_before_hello_are_unsupported() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        assert_eq!(read(&mut s, 0, 4), Err(ControlError::Unsupported));
        assert_eq!(
            s.handle(&Request::Regs).unwrap(),
            Err(ControlError::Unsupported)
        );
    }

    /// A `read`/`regs` against a **poisoned** server (`vmm == None` after a prior
    /// fatal error) is the same session-fatal [`ServeError::Poisoned`] every sibling
    /// verb returns — never a recoverable reply (PR #83 round-1 blocking: `read`
    /// must not fall back to an empty-RAM slice and fake a `ReadOutOfRange { ram_len:
    /// 0 }`, which a client would retry against a VM that no longer exists).
    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn read_and_regs_on_a_poisoned_server_are_session_fatal() {
        // A factory that cannot boot poisons the session on the first branch: the
        // live VM is dropped, the factory fails, and `vmm` stays `None`.
        let live = vmm_at_sync(vec![Exit::Common(CommonExit::Idle)], 500, 0xBA5E);
        let mut s = ControlServer::new(
            live,
            Box::new(|| Err(VmmError::ContractViolation("no boot".into()))),
        );
        hello(&mut s);
        let base = snap(&mut s);
        assert!(
            matches!(
                s.handle(&Request::Branch {
                    snap: base,
                    env: seeded_env(1),
                }),
                Err(ServeError::Vmm(_))
            ),
            "the failing factory tears the session down"
        );
        // Now poisoned. Both observation verbs are session-fatal, exactly like the
        // sibling verbs — not a recoverable ControlError.
        assert!(matches!(
            s.handle(&Request::Read { gpa: 0, len: 4 }),
            Err(ServeError::Poisoned)
        ));
        assert!(matches!(
            s.handle(&Request::Regs),
            Err(ServeError::Poisoned)
        ));
        // A cross-check that a sibling agrees (hash Whole is the canonical one).
        assert!(matches!(
            s.handle(&Request::Hash {
                scope: HashScope::Whole,
            }),
            Err(ServeError::Poisoned)
        ));
    }

    /// The **observation contract** (task 80): a full inspection pass (regs +
    /// several reads, including a deliberately out-of-range one) between other
    /// verbs leaves `hash(Whole)` bit-identical and is never stamped into the
    /// recorded reproducer (`recorded_env` is unchanged) — observation, not a move.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn observations_do_not_perturb_hash_or_recorded_env() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);
        s.handle(&Request::Branch {
            snap: base,
            env: seeded_env(7),
        })
        .unwrap()
        .unwrap();
        let h_before = hash(&mut s);
        let env_before = s.recorded_env().clone();
        // A whole inspection pass — reads across the image, the register view, and
        // an out-of-range read (a loud error is still a pure observation).
        let _ = read(&mut s, 0, 16);
        let _ = read(&mut s, RAM as u64 - 8, 8);
        let _ = regs(&mut s);
        let _ = read(&mut s, RAM as u64, 64); // out of range → error, no effect
        let _ = regs(&mut s);
        assert_eq!(hash(&mut s), h_before, "observation did not move the hash");
        assert_eq!(
            s.recorded_env(),
            &env_before,
            "observation was not recorded into the reproducer"
        );
    }

    #[derive(Clone, Debug)]
    enum ObsOp {
        Snapshot,
        Run,
        Hash,
        Branch(u64),
        Replay,
        Read(u64, u32),
        Regs,
    }

    fn arb_obs_op() -> impl Strategy<Value = ObsOp> {
        prop_oneof![
            Just(ObsOp::Snapshot),
            Just(ObsOp::Run),
            Just(ObsOp::Hash),
            (1u64..=8).prop_map(ObsOp::Branch),
            Just(ObsOp::Replay),
            // Reads span in-range and (deliberately) out-of-range addresses/lengths,
            // so even a rejected observation is proven inert.
            (0u64..=(RAM as u64 + 64), 0u32..=(RAM as u32 + 64))
                .prop_map(|(gpa, len)| ObsOp::Read(gpa, len)),
            Just(ObsOp::Regs),
        ]
    }

    /// The observable output of a "core" verb — the things the invariance gate
    /// pins. `Run`/`Hash` are the spec's named surfaces; the control acks are
    /// included so a stray mutation anywhere shows up.
    #[derive(Clone, Debug, PartialEq)]
    enum Rec {
        Ctl(Result<Reply, ControlError>),
        Run(Result<Reply, ControlError>),
        Hash(Result<Reply, ControlError>),
    }

    /// Run a script against a fresh (identically-seeded) server, optionally
    /// executing the `read`/`regs` observations, and return the ordered outputs of
    /// every non-observation verb.
    fn run_obs_script(ops: &[ObsOp], include_obs: bool) -> Vec<Rec> {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);
        let mut rec = Vec::new();
        for op in ops {
            match op {
                ObsOp::Snapshot => rec.push(Rec::Ctl(s.handle(&Request::Snapshot).unwrap())),
                ObsOp::Run => rec.push(Rec::Run(run_all_res(&mut s))),
                ObsOp::Hash => rec.push(Rec::Hash(
                    s.handle(&Request::Hash {
                        scope: HashScope::Whole,
                    })
                    .unwrap(),
                )),
                ObsOp::Branch(seed) => rec.push(Rec::Ctl(
                    s.handle(&Request::Branch {
                        snap: base,
                        env: seeded_env(*seed),
                    })
                    .unwrap(),
                )),
                ObsOp::Replay => rec.push(Rec::Ctl(s.handle(&Request::Replay(base)).unwrap())),
                ObsOp::Read(gpa, len) => {
                    if include_obs {
                        let _ = read(&mut s, *gpa, *len);
                    }
                }
                ObsOp::Regs => {
                    if include_obs {
                        let _ = s.handle(&Request::Regs).unwrap();
                    }
                }
            }
        }
        rec
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// **Acceptance gate 1 (observation invariance).** Any interleaving of
        /// `read`/`regs` among the other verbs yields byte-identical `hash` results
        /// and `StopReason` outcomes as the same sequence with the observations
        /// stripped — the docs/PROTOCOL.md search-surface criterion: observation, not a
        /// move. Reads that are out of range / over-cap (loud errors) are included,
        /// so even a *rejected* observation is proven inert.
        #[test]
        #[cfg_attr(miri, ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)")]
        fn observations_never_change_hash_or_stop_outcomes(
            ops in prop::collection::vec(arb_obs_op(), 1..16)
        ) {
            let with_obs = run_obs_script(&ops, true);
            let without_obs = run_obs_script(&ops, false);
            prop_assert_eq!(with_obs, without_obs,
                "interleaved read/regs changed a hash or stop outcome");
        }
    }

    #[test]
    fn sdk_events_pages_bound_to_the_frame_limit() {
        // Round-5 P4: a capture larger than one control frame is fetched by paging
        // (`page_sdk_events` at ascending offsets) — each page's encoded body stays
        // under MAX_FRAME_LEN, it splits into >1 page, and the pages reassemble to
        // the full capture with no overlap or gap.
        let per_event = 4_000usize; // bytes per event
        let count = control_proto::MAX_FRAME_LEN / (16 + per_event) + 5; // just over one frame
        let all: Vec<(u64, u32, Vec<u8>)> = (0..count)
            .map(|i| (i as u64, i as u32, vec![0xAB_u8; per_event]))
            .collect();

        let mut fetched: Vec<(u64, u32, Vec<u8>)> = Vec::new();
        let mut pages = 0;
        loop {
            let page = page_sdk_events(&all, fetched.len());
            if page.is_empty() {
                break;
            }
            pages += 1;
            let body: usize = 6 + page.iter().map(|e| 16 + e.2.len()).sum::<usize>();
            assert!(
                body <= control_proto::MAX_FRAME_LEN,
                "page {pages} body {body} exceeds the frame limit"
            );
            fetched.extend(page);
        }
        assert!(
            pages >= 2,
            "a capture over one frame splits into multiple pages, got {pages}"
        );
        assert_eq!(fetched, all, "paging reassembles the full capture exactly");
    }

    #[test]
    fn any_verb_before_hello_is_unsupported() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        for req in [
            Request::Snapshot,
            Request::Drop(SnapId(1)),
            Request::Hash {
                scope: HashScope::Whole,
            },
        ] {
            assert_eq!(s.handle(&req).unwrap(), Err(ControlError::Unsupported));
        }
        hello(&mut s);
        assert!(s.handle(&Request::Snapshot).unwrap().is_ok());
    }

    #[test]
    fn snapshot_mints_fresh_handles_and_drop_releases_them() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let a = snap(&mut s);
        let b = snap(&mut s);
        assert_ne!(a, b, "handles are pool-wide and never reused");
        assert_eq!(s.latest_snapshot(), Some(b));
        assert_eq!(s.handle(&Request::Drop(b)).unwrap(), Ok(Reply::Unit));
        assert_eq!(s.latest_snapshot(), Some(a));
        assert_eq!(s.handle(&Request::Drop(a)).unwrap(), Ok(Reply::Unit));
        assert_eq!(s.latest_snapshot(), None);
        assert_eq!(
            s.handle(&Request::Drop(a)).unwrap(),
            Err(ControlError::UnknownSnapshot(a)),
            "double drop is loud"
        );
        assert_eq!(
            s.handle(&Request::Replay(a)).unwrap(),
            Err(ControlError::UnknownSnapshot(a)),
            "a dropped handle cannot be restored"
        );
    }

    /// Payload branch, snapshot, replay, and exhaustion close the control loop:
    /// the live suffix and generic service configuration remain deterministic.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize uses tempfile+mmap); payload tape semantics are covered by the Miri-run vmm and environment unit tests"
    )]
    fn payload_branch_snapshot_replay_and_exhaustion_close_the_control_loop() {
        let mut s = payload_server();
        hello(&mut s);
        let base = snap(&mut s);

        let chord_a = vec![0x81, 4];
        let chord_b = vec![0, 2];
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: payload_env(7, vec![chord_a.clone(), chord_b.clone()]),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        let staged_hash = hash(&mut s);

        assert_eq!(
            ring_payload(&mut s, 2),
            (hypercall_proto::Status::Ok as u16, chord_a)
        );
        let remaining = match s.handle(&Request::RecordedEnv).unwrap() {
            Ok(Reply::Recorded(reproducer)) => EnvSpec::decode(&reproducer.bytes).unwrap(),
            other => panic!("recorded environment reply: {other:?}"),
        };
        assert_eq!(remaining.payloads(), Some([chord_b.clone()].as_slice()));

        let mid = snap(&mut s);
        assert_eq!(
            ring_payload(&mut s, 2),
            (hypercall_proto::Status::Ok as u16, chord_b.clone())
        );
        assert_eq!(s.handle(&Request::Replay(mid)).unwrap(), Ok(Reply::Unit));
        assert_eq!(
            ring_payload(&mut s, 2),
            (hypercall_proto::Status::Ok as u16, chord_b)
        );

        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: payload_env(7, vec![vec![0x81, 5], vec![0, 2]]),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        assert_ne!(
            hash(&mut s),
            staged_hash,
            "altering one staged chord must trip the full-state oracle"
        );

        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: payload_env(7, vec![]),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        let n = stage_payload_request(&mut s, 2);
        s.vmm
            .as_mut()
            .unwrap()
            .backend
            .push_exit(Exit::Arch(X86Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(n),
            }));
        assert!(matches!(run_all(&mut s), StopReason::Quiescent { .. }));
        let response = s.vmm.as_ref().unwrap().guest_slice(0xF000, 4096).unwrap();
        let (header, payload) = hypercall_proto::decode(response).unwrap();
        assert_eq!(header.status, hypercall_proto::Status::OutOfRange as u16);
        assert!(payload.is_empty());
    }

    #[test]
    fn branch_validates_the_env_before_touching_the_vm() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);
        // Wrong wire version.
        let mut env = seeded_env(7);
        env.blob_version = 99;
        assert_eq!(
            s.handle(&Request::Branch { snap: base, env }).unwrap(),
            Err(ControlError::BadEnvVersion(99))
        );
        // Malformed bytes.
        let env = Reproducer {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: vec![0xFF; 8],
        };
        assert_eq!(
            s.handle(&Request::Branch { snap: base, env }).unwrap(),
            Err(ControlError::MalformedEnvironment)
        );
        // Unknown snapshot (env valid).
        assert_eq!(
            s.handle(&Request::Branch {
                snap: SnapId(999),
                env: seeded_env(7)
            })
            .unwrap(),
            Err(ControlError::UnknownSnapshot(SnapId(999)))
        );
        // The live VM was never replaced by any of the rejected branches: it
        // still hashes (and can still snapshot).
        let _ = hash(&mut s);
        let _ = snap(&mut s);
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn branch_accepts_mechanical_operations_and_rejects_unknown_extensions() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);
        let mut spec = EnvSpec::seeded(7);
        spec.record_effect(1234, EnvHostEffect::InjectInterrupt { vector: 32 });
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: Reproducer {
                    blob_version: EnvSpec::BLOB_VERSION,
                    bytes: spec.encode()
                }
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        let before = hash(&mut s);
        let mut unknown = EnvSpec::seeded(7);
        unknown.set_config(ServiceConfig {
            identity: b"uninstalled-service".to_vec(),
            configuration: vec![],
        });
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: Reproducer {
                    blob_version: EnvSpec::BLOB_VERSION,
                    bytes: unknown.encode()
                }
            })
            .unwrap(),
            Err(ControlError::Unsupported)
        );
        assert_eq!(hash(&mut s), before);
    }

    #[test]
    fn run_resolve_is_always_resolve_without_decision() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let req = Request::Run {
            until: StopConditions {
                deadline: None,
                on: StopMask::NONE,
            },
            resolve: Some(Resolution {
                vtime: Moment(0),
                service: 19,
                id: control_proto::DecisionId(1),
                answer: Answer(vec![1, 2, 3]),
            }),
        };
        assert_eq!(
            s.handle(&req).unwrap(),
            Err(ControlError::ResolveWithoutDecision)
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated snapshot-seal/hash logic over the mock server VM (each seal state-hashes + page-hashes the image, ~2 s/KiB under Miri); pure safe code — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the seal/hash family keeps Miri-run siblings incl. snapshot_mints_fresh_handles_and_drop_releases_them and the deferred-snapshot-boundary tests (task 98 / hm-d8o)"
    )]
    fn hash_whole_includes_control_state_and_other_scopes_are_unsupported() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let h = hash(&mut s);
        assert_ne!(Some(h), s.vmm().map(|v| v.state_hash().unwrap()));
        s.exec_nonce = 1;
        assert_ne!(
            hash(&mut s),
            h,
            "the next command identity is part of whole state"
        );
        s.exec_nonce = 0;
        assert_eq!(hash(&mut s), h);
        let cut = snap(&mut s);
        let mut artifact = Vec::new();
        assert_eq!(
            s.export_portable_snapshot(cut, &mut artifact)
                .unwrap()
                .state_hash,
            h
        );
        for scope in [HashScope::Disk, HashScope::Region { base: 0, len: 4096 }] {
            assert_eq!(
                s.handle(&Request::Hash { scope }).unwrap(),
                Err(ControlError::Unsupported)
            );
        }
    }

    #[test]
    fn perturb_stages_faults_and_rejects_the_unenforceable() {
        // The `server()` live VM is advanced past its RDTSC to effective V-time 500
        // (`vmm_at_sync(..., 500, ...)`), so the stage-time floor is 500.
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let perturb = |fault: environment::channel::Effect, at: u64| Request::Perturb {
            fault: HostFault(fault.encode()),
            at: Moment(at),
        };
        // An InjectInterrupt at a future Moment stages cleanly (Reply::Unit).
        assert_eq!(
            s.handle(&perturb(
                environment::channel::Effect::InjectInterrupt { vector: 32 },
                1000
            ))
            .unwrap(),
            Ok(Reply::Unit)
        );
        // A second fault at the SAME Moment is a loud rejection (task-45
        // one-action-per-Moment; the recorded env never silently drops a fault).
        assert_eq!(
            s.handle(&perturb(
                environment::channel::Effect::InjectInterrupt { vector: 33 },
                1000
            ))
            .unwrap(),
            Err(ControlError::PerturbMomentTaken { at: 1000 })
        );
        // A Moment BEHIND the current V-time (500) is rejected loud — it could only
        // apply later than recorded.
        assert_eq!(
            s.handle(&perturb(
                environment::channel::Effect::InjectInterrupt { vector: 34 },
                100
            ))
            .unwrap(),
            Err(ControlError::PerturbPastMoment {
                at: 100,
                floor: 500
            })
        );
        // `at == floor` is fine (applies immediately and truthfully).
        assert_eq!(
            s.handle(&perturb(
                environment::channel::Effect::InjectInterrupt { vector: 35 },
                500
            ))
            .unwrap(),
            Ok(Reply::Unit)
        );
        // A CorruptMemory whose word falls outside the 16 KiB guest RAM is rejected
        // loudly at stage time (never clipped/wrapped).
        assert_eq!(
            s.handle(&perturb(
                environment::channel::Effect::XorMemory {
                    gpa: RAM as u64 - 4, // gpa + 8 > ram
                    bytes: (0xFF_u64).to_le_bytes().to_vec(),
                },
                2000
            ))
            .unwrap(),
            Err(ControlError::PerturbOutOfRange {
                gpa: RAM as u64 - 4,
                ram_len: RAM as u64,
            })
        );
        // An in-range CorruptMemory stages cleanly.
        assert_eq!(
            s.handle(&perturb(
                environment::channel::Effect::XorMemory {
                    gpa: 0,
                    bytes: (0xFF_u64).to_le_bytes().to_vec(),
                },
                3000
            ))
            .unwrap(),
            Ok(Reply::Unit)
        );
        // A malformed fault blob is a loud MalformedEnvironment.
        assert_eq!(
            s.handle(&Request::Perturb {
                fault: HostFault(vec![0xFF; 3]),
                at: Moment(5000),
            })
            .unwrap(),
            Err(ControlError::MalformedEnvironment)
        );
    }

    /// Review r13 (the last of the r11 GPA family): `CorruptMemory` **stage-time**
    /// admissibility must use the same region resolver `corrupt_memory` applies at
    /// arrival. On a high-RAM-base (arm64) machine a valid absolute GPA is
    /// admissible, and a low unmapped GPA is rejected **at stage time**
    /// (recoverable) rather than staged to explode session-fatally at arrival.
    /// (x86, `ram_base_gpa == 0`, is covered byte-identically by
    /// `perturb_stages_faults_and_rejects_the_unenforceable`.)
    #[test]
    fn perturb_corrupt_memory_stage_time_resolves_high_arm_gpas() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let perturb = |fault: environment::channel::Effect, at: u64| Request::Perturb {
            fault: HostFault(fault.encode()),
            at: Moment(at),
        };
        // Simulate arm64: RAM based high, so absolute GPAs are over RAM_BASE.
        s.vmm.as_mut().unwrap().ram_base_gpa = 0x4000_0000;

        // A valid HIGH arm64 GPA (inside `[RAM_BASE, RAM_BASE + RAM)`) is
        // admissible at stage time — the pre-r13 raw-GPA check would have rejected
        // it (`gpa + 8 > ram_len`).
        assert_eq!(
            s.handle(&perturb(
                environment::channel::Effect::XorMemory {
                    gpa: 0x4000_0000,
                    bytes: (0xFF_u64).to_le_bytes().to_vec(),
                },
                1000,
            ))
            .unwrap(),
            Ok(Reply::Unit),
            "a valid high arm64 GPA must be admissible at stage time"
        );

        // A low unmapped GPA (below RAM_BASE) is rejected at STAGE time — the
        // pre-r13 check would have admitted it (`0 + 8 <= ram_len`) and let it
        // explode fatally at arrival.
        assert_eq!(
            s.handle(&perturb(
                environment::channel::Effect::XorMemory {
                    gpa: 0,
                    bytes: (0xFF_u64).to_le_bytes().to_vec(),
                },
                2000,
            ))
            .unwrap(),
            Err(ControlError::PerturbOutOfRange {
                gpa: 0,
                ram_len: RAM as u64,
            }),
            "a low unmapped GPA must be rejected at stage time, not at arrival"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn run_maps_terminals_workload_blind() {
        // Hlt → Quiescent.
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        assert!(matches!(run_all(&mut s), StopReason::Quiescent { .. }));

        // Shutdown → Crash{Shutdown}.
        let mut s = server(vec![Exit::Common(CommonExit::Shutdown)]);
        hello(&mut s);
        let base = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: seeded_env(1)
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        match run_all(&mut s) {
            StopReason::Crash { info, .. } => assert_eq!(info.kind, CrashKind::Shutdown),
            other => panic!("expected Crash{{Shutdown}}, got {other:?}"),
        }

        // DebugExit{0} → Quiescent; DebugExit{1} → Crash{Panic, [1]}.
        let dbg = |code: u32| {
            Exit::Arch(X86Exit::Io {
                port: 0xF4,
                size: 1,
                write: Some(code),
            })
        };
        let mut s = server(vec![dbg(0)]);
        hello(&mut s);
        let base = snap(&mut s);
        s.handle(&Request::Branch {
            snap: base,
            env: seeded_env(1),
        })
        .unwrap()
        .unwrap();
        assert!(matches!(run_all(&mut s), StopReason::Quiescent { .. }));

        let mut s = server(vec![dbg(1)]);
        hello(&mut s);
        let base = snap(&mut s);
        s.handle(&Request::Branch {
            snap: base,
            env: seeded_env(1),
        })
        .unwrap()
        .unwrap();
        match run_all(&mut s) {
            StopReason::Crash { info, .. } => {
                assert_eq!(info.kind, CrashKind::Panic);
                assert_eq!(info.detail, vec![1]);
            }
            other => panic!("expected Crash{{Panic}}, got {other:?}"),
        }
    }

    #[test]
    fn run_stops_at_a_vtime_deadline_without_entering_when_already_past() {
        // The live VM is at work 500 ⇒ effective V-time 500 ns (1 ns/branch).
        // A deadline at-or-below that stops immediately with Deadline{500}.
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let req = Request::Run {
            until: StopConditions {
                deadline: Some(Moment(500)),
                on: StopMask::NONE,
            },
            resolve: None,
        };
        match s.handle(&req).unwrap() {
            Ok(Reply::Stop(StopReason::Deadline { vtime })) => assert_eq!(vtime, Moment(500)),
            other => panic!("expected Deadline{{500}}, got {other:?}"),
        }
        // And the pending scripted exit was NOT consumed: a deadline-free run
        // still reaches its Hlt terminal.
        assert!(matches!(run_all(&mut s), StopReason::Quiescent { .. }));
    }

    /// M5's portable artifact must carry the control side tables, not merely
    /// RAM + VM-state. Consume one ordered payload, export the midpoint, then
    /// prove a fresh server restores the same immediate whole-state hash and
    /// consumes the exact same remaining payload to the same next hash.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "portable export/import materializes snapshot-store mappings through tempfile+mmap; the strict artifact codec and SDK state logic run under Miri separately"
    )]
    fn portable_snapshot_replays_the_complete_mid_lineage_future() {
        let mut source = payload_server();
        hello(&mut source);
        let base = snap(&mut source);
        let first = vec![0x81, 4];
        let second = vec![0, 2];
        assert_eq!(
            source
                .handle(&Request::Branch {
                    snap: base,
                    env: payload_env(7, vec![first.clone(), second.clone()]),
                })
                .unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(
            ring_payload(&mut source, 2),
            (hypercall_proto::Status::Ok as u16, first)
        );
        let midpoint = snap(&mut source);
        let mut artifact = Vec::new();
        let exported = source
            .export_portable_snapshot(midpoint, &mut artifact)
            .unwrap();
        assert_eq!(hash(&mut source), exported.state_hash);
        assert_eq!(
            ring_payload(&mut source, 2),
            (hypercall_proto::Status::Ok as u16, second.clone())
        );
        let uninterrupted_next = hash(&mut source);

        let mut destination = payload_server();
        let imported = destination
            .import_portable_snapshot(artifact.as_slice())
            .unwrap();
        assert_eq!(imported.at, exported.at);
        assert_eq!(imported.sdk_events, exported.sdk_events);
        assert_eq!(imported.state_hash, exported.state_hash);
        hello(&mut destination);
        assert_eq!(
            destination.handle(&Request::Replay(imported.id)).unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(
            hash(&mut destination),
            exported.state_hash,
            "cross-server restore must equal the source at the cut"
        );
        assert_eq!(
            ring_payload(&mut destination, 2),
            (hypercall_proto::Status::Ok as u16, second)
        );
        assert_eq!(
            hash(&mut destination),
            uninterrupted_next,
            "the restored payload suffix must reproduce the uninterrupted future"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "portable export/import materializes snapshot-store mappings through tempfile+mmap"
    )]
    fn fully_imported_snapshot_cannot_be_reexported_without_its_hash_suffix() {
        let mut source = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut source);
        let sealed = snap(&mut source);
        let mut artifact = Vec::new();
        source
            .export_portable_snapshot(sealed, &mut artifact)
            .unwrap();

        let mut destination = server(vec![Exit::Common(CommonExit::Idle)]);
        let imported = destination
            .import_portable_snapshot(artifact.as_slice())
            .unwrap();
        assert!(matches!(
            destination.export_sparse_snapshot(imported.id, imported.id),
            Err(PortableSnapshotError::Malformed(
                "sparse export target lacks a canonical state-blob suffix"
            ))
        ));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "the replay half materializes a snapshot-store mapping, while sparse export/import validation is exercised by Miri-safe unit tests"
    )]
    fn sparse_portable_snapshot_replays_from_a_corresponding_base() {
        let mut source = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut source);
        let base = snap(&mut source);
        let mut image = source.vmm().unwrap().guest_memory().to_vec();
        image[4096..8192].fill(0xA5);
        image[3 * 4096..4 * 4096].fill(0x5A);
        source
            .vmm
            .as_mut()
            .unwrap()
            .restore_guest_memory(&image)
            .unwrap();
        let target = snap(&mut source);
        let exported = source.export_sparse_snapshot(base, target).unwrap();
        assert_eq!(exported.pages.len(), 2);
        assert_eq!(
            exported
                .pages
                .iter()
                .map(|(gfn, _)| *gfn)
                .collect::<Vec<_>>(),
            vec![1, 3],
            "export pages are sorted and target-resolved"
        );
        assert_eq!(exported.pages[0].1.as_ref(), &[0xA5; 4096]);
        assert_eq!(exported.pages[1].1.as_ref(), &[0x5A; 4096]);
        assert!(
            !exported.sidecar.windows(4).any(|tag| tag == b"MEM\0"),
            "the sidecar carries no full-memory section"
        );
        let source_hash = hash(&mut source);

        let mut destination = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut destination);
        let destination_base = snap(&mut destination);
        let imported = destination
            .import_sparse_snapshot(destination_base, exported)
            .unwrap();
        assert_eq!(
            destination.snapshot_stats(imported.id).unwrap().owned_pages,
            2
        );
        assert_eq!(
            destination.handle(&Request::Replay(imported.id)).unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(hash(&mut destination), source_hash);
        let mut round_trip_artifact = Vec::new();
        let round_trip = destination
            .export_portable_snapshot(imported.id, &mut round_trip_artifact)
            .unwrap();
        assert_eq!(
            round_trip.state_hash, source_hash,
            "restored state_blob_suffix keeps an on-demand whole-state hash correct"
        );
        assert_eq!(run_all(&mut source), run_all(&mut destination));
        assert_eq!(hash(&mut source), hash(&mut destination));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "the foreign-vendor arm fixture performs an ordinary snapshot restore"
    )]
    fn sparse_portable_import_rejects_bad_input_before_minting() {
        let mut source = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut source);
        let base = snap(&mut source);
        let mut image = source.vmm().unwrap().guest_memory().to_vec();
        image[4096..8192].fill(0xA5);
        image[3 * 4096..4 * 4096].fill(0x5A);
        source
            .vmm
            .as_mut()
            .unwrap()
            .restore_guest_memory(&image)
            .unwrap();
        let target = snap(&mut source);
        let exported = source.export_sparse_snapshot(base, target).unwrap();

        let mut destination = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut destination);
        let destination_base = snap(&mut destination);
        let destination_pages = destination.engine.mem_pages();
        let mut assert_rejected = |candidate| {
            let before = destination.snapshot_store_stats();
            assert!(
                destination
                    .import_sparse_snapshot(destination_base, candidate)
                    .is_err()
            );
            assert_eq!(destination.snapshot_store_stats(), before);
            assert_eq!(destination.latest_snapshot(), Some(destination_base));
        };

        let mut unsorted = exported.clone();
        unsorted.pages.swap(0, 1);
        assert_rejected(unsorted);
        let mut duplicate = exported.clone();
        duplicate.pages[1] = duplicate.pages[0].clone();
        assert_rejected(duplicate);
        let mut out_of_range = exported.clone();
        out_of_range.pages[0].0 = destination_pages;
        assert_rejected(out_of_range);
        let mut truncated = exported.clone();
        truncated.sidecar.pop();
        assert_rejected(truncated);
        let mut corrupted = exported.clone();
        corrupted.sidecar[0] ^= 1;
        assert_rejected(corrupted);

        // A sidecar with no SDK channel cannot claim an SDK-event prefix. This
        // exercises the explicit absent-channel contradiction rather than the
        // present-channel length check above.
        let decoded = super::decode_sparse_sidecar(&exported.sidecar).unwrap();
        let malformed_sidecar = super::encode_sparse_sidecar(&super::SparsePortableSidecarRef {
            vm_state: &decoded.vm_state,
            sdk: None,
            policy: &decoded.policy,
            control_state: &decoded.control_state,
            at: decoded.at,
            sdk_events: 1,
            trace_events: decoded.trace_events,
            trace_schedules: decoded.trace_schedules,
            tainted: decoded.tainted,
            state_blob_suffix: &decoded.state_blob_suffix,
        })
        .unwrap();
        let mut sdk_count_without_channel = exported.clone();
        sdk_count_without_channel.sidecar = malformed_sidecar;
        assert_rejected(sdk_count_without_channel);

        // An x86 vm_state blob must not be accepted by an arm64 destination;
        // the vendor SnapshotRecords codec rejects it before the layer/handle.
        let mut arm = accumulated_session_server();
        let arm_base = arm.latest_snapshot().unwrap();
        let before = arm.snapshot_store_stats();
        assert!(arm.import_sparse_snapshot(arm_base, exported).is_err());
        assert_eq!(arm.snapshot_store_stats(), before);
        assert_eq!(arm.latest_snapshot(), Some(arm_base));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "portable export/import materializes snapshot-store mappings through tempfile+mmap; strict corruption rejection is covered by the Miri-safe codec test"
    )]
    fn portable_import_rejects_a_planted_ram_corruption_before_minting_a_handle() {
        let mut source = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut source);
        let snap = snap(&mut source);
        let mut artifact = Vec::new();
        source
            .export_portable_snapshot(snap, &mut artifact)
            .unwrap();
        // Fixed header is 116 bytes; the first byte after it is RAM.
        artifact[116] ^= 1;

        let mut destination = server(vec![Exit::Common(CommonExit::Idle)]);
        let before = destination.snapshot_store_stats();
        assert!(matches!(
            destination.import_portable_snapshot(artifact.as_slice()),
            Err(crate::portable_snapshot::PortableSnapshotError::DigestMismatch)
        ));
        let after = destination.snapshot_store_stats();
        assert_eq!(after.snapshots, before.snapshots);
        assert_eq!(after.stored_unique_pages, before.stored_unique_pages);
    }

    #[test]
    #[cfg_attr(miri, ignore = "portable import allocates snapshot-store mappings")]
    fn accumulated_transport_inputs_survive_local_and_portable_replay() {
        let mut source = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut source);
        let floor = source.vmm().unwrap().effective_vns().unwrap();
        for at in 1..=128 {
            let effect =
                environment::channel::Effect::write_memory(0, vec![at as u8; 10_000]).unwrap();
            assert_eq!(
                source
                    .handle(&Request::Perturb {
                        fault: control_proto::HostFault(effect.encode()),
                        at: Moment(floor + at),
                    })
                    .unwrap(),
                Ok(Reply::Unit)
            );
        }
        assert!(
            source.capture_control_state().pending.encode().len()
                > environment::channel::MAX_CHANNEL_BYTES
        );
        let before = hash(&mut source);
        let cut = snap(&mut source);
        let mut artifact = Vec::new();
        source.export_portable_snapshot(cut, &mut artifact).unwrap();
        assert_eq!(
            source.handle(&Request::Replay(cut)).unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(hash(&mut source), before);
        let mut cold = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut cold);
        let imported = cold.import_portable_snapshot(artifact.as_slice()).unwrap();
        assert_eq!(
            cold.handle(&Request::Replay(imported.id)).unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(hash(&mut cold), before);
        assert_eq!(cold.schedule, source.schedule);
        assert_eq!(cold.schedule.len(), 128);
    }

    #[test]
    #[cfg_attr(miri, ignore = "sparse snapshot import uses mapped snapshot storage")]
    fn sparse_import_rejects_a_different_control_hash_preimage() {
        let mut source = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut source);
        let cut = snap(&mut source);
        let meta = source.snapshot_meta.get_mut(&cut.0).unwrap();
        let mut changed = super::ControlState::decode(&meta.control_state)
            .unwrap()
            .unwrap();
        changed.exec_nonce += 1;
        // Both control records are well formed, but replay would use a different
        // command nonce from the one covered by the retained hash preimage.
        meta.control_state = changed.encode();
        let sparse = source.export_sparse_snapshot(cut, cut).unwrap();
        let mut destination = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut destination);
        let base = snap(&mut destination);
        let before = destination.snapshot_store_stats();
        let before_hash = hash(&mut destination);
        assert!(matches!(
            destination.import_sparse_snapshot(base, sparse),
            Err(PortableSnapshotError::Malformed(
                "sparse control hash preimage"
            ))
        ));
        assert_eq!(destination.snapshot_store_stats(), before);
        assert_eq!(destination.latest_snapshot(), Some(base));
        assert_eq!(hash(&mut destination), before_hash);
    }

    #[test]
    #[cfg_attr(miri, ignore = "portable import allocates snapshot-store mappings")]
    fn imports_reject_contradictory_control_and_cut_before_minting() {
        for field in 0..4 {
            let mut source = server(vec![Exit::Common(CommonExit::Idle)]);
            hello(&mut source);
            let cut = snap(&mut source);
            let meta = source.snapshot_meta.get_mut(&cut.0).unwrap();
            match field {
                0 => meta.at += 1,
                1 => meta.sdk_events += 1,
                2 => meta.control_state[0] ^= 1,
                3 => meta.policy.configuration.push(1),
                _ => unreachable!(),
            }
            // These artifacts have valid framing and integrity hashes: the
            // contradictory endpoint metadata must still be rejected.
            let mut artifact = Vec::new();
            source.export_portable_snapshot(cut, &mut artifact).unwrap();
            let mut destination = server(vec![Exit::Common(CommonExit::Idle)]);
            hello(&mut destination);
            let base = snap(&mut destination);
            let before = destination.snapshot_store_stats();
            let before_hash = hash(&mut destination);
            assert!(matches!(
                destination.import_portable_snapshot(artifact.as_slice()),
                Err(PortableSnapshotError::Malformed(_))
            ));
            assert_eq!(destination.snapshot_store_stats(), before);
            assert_eq!(destination.latest_snapshot(), Some(base));
            assert_eq!(hash(&mut destination), before_hash);

            let sparse = source.export_sparse_snapshot(cut, cut).unwrap();
            assert!(matches!(
                destination.import_sparse_snapshot(base, sparse),
                Err(PortableSnapshotError::Malformed(_))
            ));
            assert_eq!(destination.snapshot_store_stats(), before);
            assert_eq!(destination.latest_snapshot(), Some(base));
            assert_eq!(hash(&mut destination), before_hash);
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn branch_reseeds_and_replay_does_not() {
        // Fork VMs take one RDTSC (to a synchronized point) then halt.
        let mut s = server(vec![
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Common(CommonExit::Idle),
        ]);
        hello(&mut s);
        let base = snap(&mut s);
        let h_base = hash(&mut s);

        // replay(base) reproduces the captured state bit-for-bit.
        assert_eq!(s.handle(&Request::Replay(base)).unwrap(), Ok(Reply::Unit));
        assert_eq!(hash(&mut s), h_base, "replay is verbatim (same state hash)");

        // branch(base, seed) forks the entropy stream: distinct seeds hash
        // distinctly (the entropy seed/position is in the VTIM chunk), and the
        // same seed twice hashes identically.
        let mut branch_hash = |seed: u64| -> [u8; 32] {
            s.handle(&Request::Branch {
                snap: base,
                env: seeded_env(seed),
            })
            .unwrap()
            .unwrap();
            hash(&mut s)
        };
        let h1 = branch_hash(0x1111);
        let h2 = branch_hash(0x2222);
        let h1_again = branch_hash(0x1111);
        assert_eq!(h1, h1_again, "same seed ⇒ same branched state");
        assert_ne!(h1, h2, "distinct seeds ⇒ divergent futures");
        assert_ne!(h1, h_base, "a branch is not the verbatim replay");
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn a_failed_factory_is_session_fatal_and_poisons_the_server() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);
        // Swap in a failing factory by rebuilding the server around the same
        // live VM shape: simplest is a dedicated server.
        let live = vmm_at_sync(vec![Exit::Common(CommonExit::Idle)], 500, 0xBA5E);
        let mut s = ControlServer::new(
            live,
            Box::new(|| Err(VmmError::ContractViolation("no fresh VM".into()))),
        );
        hello(&mut s);
        let base2 = snap(&mut s);
        let err = s.handle(&Request::Replay(base2)).unwrap_err();
        assert!(matches!(err, ServeError::Vmm(_)));
        // Poisoned: the VM is gone; verbs that need it are fatal, not replies.
        assert!(matches!(
            s.handle(&Request::Snapshot).unwrap_err(),
            ServeError::Poisoned
        ));
        let _ = base; // silence: the first server was only used for setup
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn restore_validation_rejection_is_recoverable_and_keeps_the_fresh_vm() {
        // The recoverable half of restore's error split (round 4): a
        // **validation-class** rejection — here a V-time wiring mismatch (the
        // live VM is V-time-wired, so the snapshot carries a V-time block, but
        // the factory boots forks WITHOUT V-time) — is caught *before* the fresh
        // VM mutates, so it answers the recoverable `RestoreFailed` and KEEPS the
        // intact fresh VM (the session stays usable).
        let live = vmm_at_sync(vec![Exit::Common(CommonExit::Idle)], 500, 0xBA5E); // V-time wired
        let factory = Box::new(|| {
            // A fork with NO V-time wired → restoring a V-time-bearing blob into
            // it is a ContractViolation (wiring must match the snapshot source).
            let mut m = MockBackend::with_exits(vec![Exit::Common(CommonExit::Idle)]);
            m.set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
            Ok(Vmm::new(m, GuestRam::new(RAM).unwrap()))
        });
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);
        let base = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: seeded_env(1)
            })
            .unwrap(),
            Err(ControlError::RestoreFailed),
            "a validation-class restore rejection is the recoverable RestoreFailed"
        );
        // The session is NOT poisoned: the intact fresh fork VM is still there.
        assert!(
            s.vmm().is_some(),
            "the fresh VM was kept after the rejection"
        );
        // Round-4 P3: the kept fresh VM must still carry an SDK channel, or a
        // doorbell on it would be a contract violation despite GUEST_HAS_SDK.
        assert!(
            s.vmm().unwrap().sdk_is_enabled(),
            "the kept fresh VM stays SDK-capable after a recoverable RestoreFailed"
        );
        let _ = hash(&mut s); // still usable
    }

    /// A backend that forwards to an inner mock but **fails `restore`** — to
    /// exercise restore's *fatal* (post-validation substrate-breakage) split.
    struct RestoreFailBackend(MockBackend);
    impl Backend for RestoreFailBackend {
        type A = vmm_backend::X86;

        fn set_policy(&mut self, policy: &vmm_backend::X86Policy) -> vmm_backend::Result<()> {
            self.0.set_policy(policy)
        }
        unsafe fn map_memory(
            &mut self,
            gpa: vmm_backend::Gpa,
            host: &mut [u8],
        ) -> vmm_backend::Result<()> {
            // SAFETY: forwards to the inner mock, which only records the region
            // (no dereference) — no obligation beyond the trait contract.
            unsafe { self.0.map_memory(gpa, host) }
        }
        fn run(&mut self) -> vmm_backend::Result<Exit<vmm_backend::X86>> {
            self.0.run()
        }
        fn inject(&mut self, e: vmm_backend::Injection) -> vmm_backend::Result<()> {
            self.0.inject(e)
        }
        fn set_pending_irq(&mut self, v: Option<u8>) -> vmm_backend::Result<()> {
            self.0.set_pending_irq(v)
        }
        fn take_accepted_interrupt(&mut self) -> Option<u8> {
            self.0.take_accepted_interrupt()
        }
        fn complete_read(&mut self, v: u64) -> vmm_backend::Result<()> {
            self.0.complete_read(v)
        }
        fn complete_fault(&mut self) -> vmm_backend::Result<()> {
            self.0.complete_fault()
        }
        fn complete_ok(&mut self) -> vmm_backend::Result<()> {
            self.0.complete_ok()
        }
        fn complete_hypercall(&mut self, rax: u64) -> vmm_backend::Result<()> {
            self.0.complete_hypercall(rax)
        }
        fn complete_arch(&mut self, c: vmm_backend::X86Completion) -> vmm_backend::Result<()> {
            self.0.complete_arch(c)
        }
        fn save(&self) -> vmm_backend::Result<vmm_backend::VcpuState> {
            self.0.save()
        }
        fn restore(&mut self, _s: &vmm_backend::VcpuState) -> vmm_backend::Result<()> {
            Err(vmm_backend::BackendError::Memory("induced restore failure"))
        }
        fn exit_counts(&self) -> vmm_backend::ExitCounts {
            self.0.exit_counts()
        }
        fn reset_exit_counts(&mut self) {
            self.0.reset_exit_counts()
        }
        fn capabilities(&self) -> vmm_backend::Capabilities<vmm_backend::X86Caps> {
            self.0.capabilities()
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn restore_substrate_failure_is_session_fatal_and_poisons_the_server() {
        // The fatal half of restore's error split (round 4): a failure AFTER
        // validation — here `Backend::restore` itself faults — is substrate
        // breakage (the fresh VM's state can no longer be vouched for), so it
        // tears the session down (ServeError) rather than answering the
        // recoverable RestoreFailed and letting a client run from unvouched state.
        let build = || -> Vmm<RestoreFailBackend> {
            let mut m = MockBackend::with_exits(vec![
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                Exit::Common(CommonExit::Idle),
            ]);
            m.set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
            let mut v = Vmm::new(RestoreFailBackend(m), GuestRam::new(RAM).unwrap());
            v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 1).unwrap());
            v.wire_snapshot_hashing();
            v.restore_guest_memory(&vec![0u8; RAM]).unwrap();
            v
        };
        let mut live = build();
        live.step().unwrap(); // Rdtsc → synchronized (snapshottable)
        let mut s = ControlServer::new(live, Box::new(move || Ok(build())));
        // hello + snapshot inline (the typed `hello`/`snap` helpers are
        // MockBackend-only; this server is over RestoreFailBackend).
        assert!(s.handle(&Request::Hello(server_caps())).unwrap().is_ok());
        let base = match s.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::Snapshot { id, .. }) => id,
            other => panic!("snapshot: {other:?}"),
        };
        // branch restores into a fresh fork whose Backend::restore faults AFTER
        // validation → fatal ServeError, not a RestoreFailed reply.
        let err = s
            .handle(&Request::Branch {
                snap: base,
                env: seeded_env(1),
            })
            .unwrap_err();
        assert!(
            matches!(err, ServeError::Vmm(_)),
            "a post-validation restore fault is session-fatal, got {err:?}"
        );
        // Poisoned: the unvouched VM was dropped, not kept.
        assert!(s.vmm().is_none(), "the unvouched VM was dropped");
        assert!(matches!(
            s.handle(&Request::Snapshot).unwrap_err(),
            ServeError::Poisoned
        ));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "drives a real UnixStream socketpair across threads; Miri can't execute the socket syscalls (task 98)"
    )]
    fn serve_speaks_frames_over_an_in_memory_stream() {
        // A tiny wire-level session over a socketpair: hello → snapshot →
        // hash → EOF. The server stays on this thread (a `Vmm` is not `Send` —
        // its work source is a thread-affine counter on the box); the client
        // runs on a spawned thread, exactly the composition the demo bin uses.
        // The full loopback (socket Machine end-to-end) lives in the acceptance
        // suite.
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        let (mut client, server_end) = UnixStream::pair().unwrap();
        let client_thread = std::thread::spawn(move || {
            let mut seq = 0u32;
            let mut send =
                |client: &mut UnixStream, req: &Request| -> Result<Reply, ControlError> {
                    seq += 1;
                    let mut buf = Vec::new();
                    control_proto::encode_request(seq, req, &mut buf).unwrap();
                    client.write_all(&buf).unwrap();
                    let mut inbuf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        if let Some((got_seq, reply, consumed)) =
                            control_proto::decode_reply(&inbuf).unwrap()
                        {
                            assert_eq!(got_seq, seq, "the reply echoes the request seq");
                            assert_eq!(consumed, inbuf.len());
                            return reply;
                        }
                        let n = client.read(&mut chunk).unwrap();
                        assert_ne!(n, 0, "server closed mid-reply");
                        inbuf.extend_from_slice(&chunk[..n]);
                    }
                };
            assert_eq!(
                send(&mut client, &Request::Hello(server_caps())),
                Ok(Reply::Hello(server_caps()))
            );
            let reply = send(&mut client, &Request::Snapshot);
            assert!(matches!(reply, Ok(Reply::Snapshot { .. })));
            assert!(matches!(
                send(
                    &mut client,
                    &Request::Hash {
                        scope: HashScope::Whole
                    }
                ),
                Ok(Reply::Hash(_))
            ));
            // Dropping the client closes the stream: EOF between frames.
        });
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        s.serve(server_end).unwrap();
        client_thread.join().unwrap();
    }

    // -----------------------------------------------------------------------
    // Task 59 — host-plane enforcement (apply a HostFault at a Moment).
    //
    // Portable proof over the scripted `MockBackend`: each staged `Moment`
    // arrives at a deterministic scripted exit boundary whose assigned V-time
    // is the Moment itself. The end-to-end record→replay-on-real-KVM closure is the
    // box gate; here we prove the apply-at-Moment seam, its determinism, and
    // that the emitted recorded env re-applies to the identical hash.
    // -----------------------------------------------------------------------

    /// A live VM for enforcement: V-time + userspace-LAPIC + snapshot-hashing
    /// wired, a distinctive RAM image loaded, and a script of `exit_count`
    /// clock-advancing exits followed by a terminal halt.
    fn enforce_vmm(exit_count: usize, image: [u8; RAM], seed: u64) -> Vmm<MockBackend> {
        let mut exits = vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 }); exit_count];
        exits.push(Exit::Common(CommonExit::Idle));
        let mut m = MockBackend::with_exits(exits);
        m.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), seed).unwrap());
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        v.wire_snapshot_hashing();
        v.restore_guest_memory(&image).unwrap();
        v
    }

    /// The pristine 16 KiB image the enforcement tests start from.
    fn enforce_image() -> [u8; RAM] {
        let mut image = [0u8; RAM];
        image[..12].copy_from_slice(b"ENFORCE_BOOT");
        image
    }

    /// Enough one-nanosecond exits to reach the schedule's final moment.
    fn exits_to_cover(schedule: &[(u64, EnvHostEffect)]) -> usize {
        schedule
            .iter()
            .map(|(m, _)| usize::try_from(*m).unwrap_or(usize::MAX))
            .max()
            .unwrap_or(0)
    }

    /// Build a server, stage `schedule` via `perturb`, run to terminal, and
    /// return `(state_hash, recorded_env)`. The factory is unused (no
    /// branch/replay in these direct tests), so it errors loudly if ever called.
    fn enforce_run(schedule: &[(u64, EnvHostEffect)], seed: u64) -> ([u8; 32], EnvSpec) {
        let live = enforce_vmm(exits_to_cover(schedule), enforce_image(), seed);
        let factory = Box::new(|| {
            Err(VmmError::ContractViolation(
                "factory unused in a direct enforcement run".into(),
            ))
        });
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);
        for (m, fault) in schedule {
            let req = Request::Perturb {
                fault: HostFault(fault.encode()),
                at: Moment(*m),
            };
            assert_eq!(
                s.handle(&req).unwrap(),
                Ok(Reply::Unit),
                "staging fault at Moment {m}"
            );
        }
        let stop = run_all(&mut s);
        assert!(
            matches!(stop, StopReason::Quiescent { .. }),
            "an enforcement run halts cleanly, got {stop:?}"
        );
        (hash(&mut s), s.recorded_env().clone())
    }

    fn enforce_hash(schedule: &[(u64, EnvHostEffect)], seed: u64) -> [u8; 32] {
        enforce_run(schedule, seed).0
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "enforce_run boots + runs + state-hashes VMs several times (~2 s/KiB sha256 under Miri); it never restores, and its boot-path map_memory seam is Miri-run via bringup::tests::compose_drives_guestram_and_unsafe_map_memory — the same grounds as its proptest sibling arbitrary_schedule_applied_twice_is_identical; covered natively (task 98 / hm-d8o)"
    )]
    fn same_schedule_run_twice_is_bit_identical_and_control_differs() {
        // Gate 1: an arbitrary schedule applied twice is bit-identical; and an
        // ABSENT schedule (the control) differs — the faults are actually landing.
        let schedule = vec![
            (
                1,
                EnvHostEffect::XorMemory {
                    gpa: 0x40,
                    bytes: (0xDEAD_BEEF_0000_0001_u64).to_le_bytes().to_vec(),
                },
            ),
            (2, EnvHostEffect::InjectInterrupt { vector: 0x40 }),
        ];
        let seed = 0x5EED59;
        let h1 = enforce_hash(&schedule, seed);
        let h2 = enforce_hash(&schedule, seed);
        assert_eq!(h1, h2, "same schedule ⇒ bit-identical state_hash");

        let control = enforce_hash(&[], seed);
        assert_ne!(
            h1, control,
            "the empty control run differs (the faults land)"
        );
    }

    #[test]
    fn corrupt_memory_lands_the_exact_xor_at_the_gpa() {
        // The upset is the pure function `word ^ mask` at the gpa — verify the
        // exact bytes land, and that a mask that flips no bit (0) is a no-op.
        let gpa = 0x80usize;
        let mask = 0xA5A5_0000_1234_5678u64;
        let fault = EnvHostEffect::XorMemory {
            gpa: gpa as u64,
            bytes: mask.to_le_bytes().to_vec(),
        };
        let live = enforce_vmm(1, enforce_image(), 7);
        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);
        s.handle(&Request::Perturb {
            fault: HostFault(fault.encode()),
            at: Moment(1),
        })
        .unwrap()
        .unwrap();
        let _ = run_all(&mut s);
        let ram = s.vmm().unwrap().guest_memory();
        let word = u64::from_le_bytes(ram[gpa..gpa + 8].try_into().unwrap());
        // The pristine image is zero from 0x80 on, so the corrupted word is
        // exactly the mask.
        assert_eq!(word, mask, "CorruptMemory XORs the mask into the gpa word");
    }

    #[test]
    fn a_fault_at_the_deferred_snapshot_boundary_drains_before_the_seal() {
        // Inputs due at the setup boundary apply before its snapshot point is
        // advertised. A later snapshot captures that already-applied prefix.
        const REQ_GPA: usize = 0xE000;
        let m: u64 = 1;

        // A `setup_complete` Event frame staged at REQ_GPA.
        let setup_id: u32 = 4 << 24;
        let mut frame = [0u8; 4096];
        let n = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Event,
            1,
            1,
            &setup_id.to_le_bytes(),
            &mut frame,
        )
        .unwrap();

        // Live guest: ring the doorbell, then land an arrival (rewritten to M).
        let mut mb = MockBackend::with_exits(vec![
            Exit::Arch(X86Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(n as u32),
            }),
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Common(CommonExit::Idle),
        ]);
        mb.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
        live.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 9).unwrap());
        live.wire_snapshot_hashing();
        let mut ram = vec![0u8; BIG_RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        live.restore_guest_memory(&ram).unwrap();

        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);

        // Stage a CorruptMemory fault EXACTLY at the deferred boundary Moment M.
        let fault = EnvHostEffect::XorMemory {
            gpa: 0x1000,
            bytes: (0xDEAD_BEEF_u64).to_le_bytes().to_vec(),
        };
        s.handle(&Request::Perturb {
            fault: HostFault(fault.encode()),
            at: Moment(m),
        })
        .unwrap()
        .unwrap();

        // The run surfaces the deferred snapshot point at M, the fault drained first
        // (arm the SNAPSHOT_POINT class — round-7 gates it on the mask).
        let stop = run_seeking_snapshot(&mut s);
        assert!(
            matches!(stop, StopReason::SnapshotPoint { .. }),
            "the deferred point surfaced, got {stop:?}"
        );

        // The snapshot retains the already-applied input.
        match s.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::Snapshot { .. }) => {}
            other => {
                panic!("seal at the deferred boundary failed: {other:?}")
            }
        }
        // And the boundary fault DID land — the drain applied it before the seal.
        let ram = s.vmm().unwrap().guest_memory();
        let word = u64::from_le_bytes(ram[0x1000..0x1008].try_into().unwrap());
        assert_eq!(
            word, 0xDEAD_BEEF,
            "the boundary fault was applied before the seal"
        );
    }

    #[test]
    fn a_future_fault_is_retained_at_the_first_snapshot_point() {
        // The setup point remains at the first synchronized boundary, even with
        // a future fault. Saving carries that pending input without applying it.
        const REQ_GPA: usize = 0xE000;
        let m: u64 = 2;

        let setup_id: u32 = 4 << 24;
        let mut frame = [0u8; 4096];
        let n = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Event,
            1,
            1,
            &setup_id.to_le_bytes(),
            &mut frame,
        )
        .unwrap();

        // setup_complete → an RDTSC (a synchronized boundary at vns 0, the fault
        // still future) → the fault's arrival at M.
        let mut mb = MockBackend::with_exits(vec![
            Exit::Arch(X86Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(n as u32),
            }),
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Common(CommonExit::Idle),
        ]);
        mb.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
        live.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 9).unwrap());
        live.wire_snapshot_hashing();
        let mut ram = vec![0u8; BIG_RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        live.restore_guest_memory(&ram).unwrap();

        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);

        // A FUTURE fault at M (> the RDTSC boundary's vns of 0).
        let fault = EnvHostEffect::XorMemory {
            gpa: 0x1000,
            bytes: (0x00C0_FFEE_u64).to_le_bytes().to_vec(),
        };
        s.handle(&Request::Perturb {
            fault: HostFault(fault.encode()),
            at: Moment(m),
        })
        .unwrap()
        .unwrap();

        match run_seeking_snapshot(&mut s) {
            StopReason::SnapshotPoint { vtime } => assert!(vtime.0 < m),
            other => panic!("expected SnapshotPoint, got {other:?}"),
        }
        assert!(s.schedule.contains_key(&m), "future input remains pending");
        match s.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::Snapshot { .. }) => {}
            other => panic!("seal failed with a future fault mishandled: {other:?}"),
        }
    }

    #[test]
    fn a_future_reseed_is_retained_at_the_first_snapshot_point() {
        // The setup point remains at the first synchronized boundary, even with
        // a future reseed. Saving carries that pending input without applying it.
        const REQ_GPA: usize = 0xE000;
        let m: u64 = 2;

        let setup_id: u32 = 4 << 24;
        let mut frame = [0u8; 4096];
        let n = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Event,
            1,
            1,
            &setup_id.to_le_bytes(),
            &mut frame,
        )
        .unwrap();

        // setup_complete → an RDTSC (synchronized boundary at vns 0, reseed still
        // future) → the reseed's arrival at M.
        let mut mb = MockBackend::with_exits(vec![
            Exit::Arch(X86Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(n as u32),
            }),
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Common(CommonExit::Idle),
        ]);
        mb.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
        live.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 9).unwrap());
        live.wire_snapshot_hashing();
        let mut ram = vec![0u8; BIG_RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        live.restore_guest_memory(&ram).unwrap();

        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);

        // Stage a FUTURE reseed at M. No public verb stages a reseed without a
        // branch env, so insert directly — the test module shares the crate.
        s.reseed_schedule.insert(m, 9);

        match run_seeking_snapshot(&mut s) {
            StopReason::SnapshotPoint { vtime } => assert!(vtime.0 < m),
            other => panic!("expected SnapshotPoint, got {other:?}"),
        }
        assert!(
            s.reseed_schedule.contains_key(&m),
            "future input remains pending"
        );
        match s.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::Snapshot { .. }) => {}
            other => panic!("seal failed with a staged reseed mishandled: {other:?}"),
        }
    }

    #[test]
    fn an_sdk_stop_with_a_staged_reseed_poisons_loud() {
        // Round-9 P1 (task 78 seam): an SDK stop (an assert violation at a doorbell
        // OUT) is NOT a V-time intercept, so `effective_vns` there is only a lower
        // bound — a staged reseed at-or-above it may already be crossed (the guest
        // ran past its Moment in the exit-boundary variability window). Poison loud — the same
        // ScheduleUnsatisfiable class as a crossed fault, mirroring the terminal
        // arm — else a later replay from the reseed re-derives a different stream.
        const REQ_GPA: usize = 0xE000;

        // An assert *violation* Event frame (point 20) → Step::SdkStop.
        let viol_id: u32 = (1 << 24) | 20;
        let mut payload = viol_id.to_le_bytes().to_vec();
        payload.extend_from_slice(&[1, 0, 0]); // [DISP_VIOLATION, detail_len = 0]
        let mut frame = [0u8; 4096];
        let n = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Event,
            1,
            1,
            &payload,
            &mut frame,
        )
        .unwrap();

        let mut mb = MockBackend::with_exits(vec![
            Exit::Arch(X86Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(n as u32),
            }),
            Exit::Common(CommonExit::Idle),
        ]);
        mb.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
        live.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 9).unwrap());
        live.wire_snapshot_hashing();
        let mut ram = vec![0u8; BIG_RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        live.restore_guest_memory(&ram).unwrap();

        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);

        // Stage a reseed beyond the trajectory (direct, as above).
        let reseed_moment: u64 = 1_000_000;
        s.reseed_schedule.insert(reseed_moment, 9);

        // The SDK stop surfaces with the reseed staged → poison loud. (ASSERTION is
        // armed so the stop is a real surfaced stop; the poison precedes that gate.)
        let req = Request::Run {
            until: StopConditions {
                deadline: None,
                on: StopMask::NONE.arm(control_proto::class_bit::ASSERTION),
            },
            resolve: None,
        };
        assert!(
            matches!(
                s.handle(&req).unwrap(),
                Err(ControlError::ScheduleUnsatisfiable { moment, .. }) if moment == reseed_moment
            ),
            "an SDK stop with a staged reseed must poison loud"
        );
        // Latched until a rewind (a subsequent run keeps failing loud).
        assert!(matches!(
            s.handle(&req).unwrap(),
            Err(ControlError::ScheduleUnsatisfiable { .. })
        ));
    }

    #[test]
    fn stop_mask_gates_the_sdk_snapshot_point_and_assertion() {
        // Round-7: the deferred SnapshotPoint AND an Assertion honor the client
        // StopMask. `StopMask::NONE` runs a cooperating-SDK guest straight through
        // to the terminal; arming the class bit surfaces the stop.
        const REQ_GPA: usize = 0xE000;

        let frame_for = |payload: &[u8]| -> Vec<u8> {
            let mut buf = [0u8; 4096];
            let n = hypercall_proto::encode_request(
                hypercall_proto::ServiceId::Event,
                1,
                1,
                payload,
                &mut buf,
            )
            .unwrap();
            buf[..n].to_vec()
        };
        // Run a guest that rings the doorbell (first exit, req_len = `frame.len()`)
        // then `rest`, under mask `on`; return the stop.
        let run_with = |rest: Vec<Exit<X86>>, frame: &[u8], on: StopMask| -> StopReason {
            let mut script = vec![Exit::Arch(X86Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(frame.len() as u32),
            })];
            script.extend(rest);
            let mut mb = MockBackend::with_exits(script);
            mb.set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
            let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
            live.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 3).unwrap());
            live.wire_snapshot_hashing();
            let mut ram = vec![0u8; BIG_RAM];
            ram[REQ_GPA..REQ_GPA + frame.len()].copy_from_slice(frame);
            live.restore_guest_memory(&ram).unwrap();
            let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
            let mut s = with_test_service(ControlServer::new(live, factory));
            hello(&mut s);
            match s
                .handle(&Request::Run {
                    until: StopConditions { deadline: None, on },
                    resolve: None,
                })
                .unwrap()
            {
                Ok(Reply::Stop(stop)) => stop,
                other => panic!("run reply: {other:?}"),
            }
        };

        // (a) setup_complete → an RDTSC (a synchronized, sealable boundary).
        let setup = frame_for(&(4u32 << 24).to_le_bytes());
        assert!(
            matches!(
                run_with(
                    vec![
                        Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                        Exit::Common(CommonExit::Idle)
                    ],
                    &setup,
                    StopMask::NONE
                ),
                StopReason::Quiescent { .. }
            ),
            "StopMask::NONE runs through setup_complete straight to the terminal"
        );
        assert!(
            matches!(
                run_with(
                    vec![
                        Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                        Exit::Common(CommonExit::Idle)
                    ],
                    &setup,
                    StopMask::NONE.arm(control_proto::class_bit::SNAPSHOT_POINT)
                ),
                StopReason::SnapshotPoint { .. }
            ),
            "arming SNAPSHOT_POINT surfaces the deferred point at the RDTSC"
        );

        // (b) an always-violation assertion.
        let mut viol = ((1u32 << 24) | 20).to_le_bytes().to_vec();
        viol.extend_from_slice(&[1, 0, 0]); // [DISP_VIOLATION, detail_len = 0]
        let viol = frame_for(&viol);
        assert!(
            matches!(
                run_with(vec![Exit::Common(CommonExit::Idle)], &viol, StopMask::NONE),
                StopReason::Quiescent { .. }
            ),
            "StopMask::NONE runs through an assertion straight to the terminal"
        );
        assert!(
            matches!(
                run_with(
                    vec![Exit::Common(CommonExit::Idle)],
                    &viol,
                    StopMask::NONE.arm(control_proto::class_bit::ASSERTION)
                ),
                StopReason::Assertion { .. }
            ),
            "arming ASSERTION surfaces the assertion"
        );
    }

    #[test]
    fn host_events_apply_at_the_first_covering_exit_boundary_in_order() {
        // With one V-ns assigned to each RDTSC exit, events at moments 1 and 2
        // apply after the corresponding exits and preserve schedule order.
        let schedule = vec![
            (
                1,
                EnvHostEffect::XorMemory {
                    gpa: 0x40,
                    bytes: (1_u64).to_le_bytes().to_vec(),
                },
            ),
            (
                2,
                EnvHostEffect::XorMemory {
                    gpa: 0x48,
                    bytes: (2_u64).to_le_bytes().to_vec(),
                },
            ),
        ];
        let live = enforce_vmm(exits_to_cover(&schedule), enforce_image(), 1);
        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);
        for (m, f) in &schedule {
            s.handle(&Request::Perturb {
                fault: HostFault(f.encode()),
                at: Moment(*m),
            })
            .unwrap()
            .unwrap();
        }
        let _ = run_all(&mut s);
        // The last exit advanced effective V-time to the final event moment.
        assert_eq!(
            s.vmm().unwrap().effective_vns(),
            Some(2),
            "the run reached the last staged Moment"
        );
        // Both upsets landed (words at both gpas are non-zero).
        let ram = s.vmm().unwrap().guest_memory();
        assert_ne!(&ram[0x40..0x48], &[0u8; 8], "first upset landed");
        assert_ne!(&ram[0x48..0x50], &[0u8; 8], "second upset landed");
    }

    #[test]
    fn second_fault_at_one_moment_is_loudly_rejected() {
        // Task 45 is one-action-per-Moment (integrator ruling, spec amendment PR
        // #54): a second stage at an occupied Moment is loudly rejected, so
        // `recorded_env()` never silently drops a fault. The first stage stands and
        // still applies.
        let live = enforce_vmm(1, enforce_image(), 0xAB);
        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);
        let stage = |f: EnvHostEffect, m: u64| Request::Perturb {
            fault: HostFault(f.encode()),
            at: Moment(m),
        };
        assert_eq!(
            s.handle(&stage(
                EnvHostEffect::XorMemory {
                    gpa: 0x40,
                    bytes: (0x0F0F_0F0F_u64).to_le_bytes().to_vec(),
                },
                1,
            ))
            .unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(
            s.handle(&stage(EnvHostEffect::InjectInterrupt { vector: 0x50 }, 1))
                .unwrap(),
            Err(ControlError::PerturbMomentTaken { at: 1 }),
            "a second fault at Moment 1 is rejected (not silently dropped)"
        );
        // The first (accepted) fault still applies, and the recorded env carries
        // exactly it.
        let _ = run_all(&mut s);
        assert_ne!(
            &s.vmm().unwrap().guest_memory()[0x40..0x48],
            &[0u8; 8],
            "the one accepted upset landed"
        );
        assert_eq!(
            s.recorded_env().effects().len(),
            1,
            "exactly the accepted fault is recorded"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated snapshot-seal/hash logic over the mock server VM (each seal state-hashes + page-hashes the image, ~2 s/KiB under Miri); pure safe code — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the seal/hash family keeps Miri-run siblings incl. snapshot_mints_fresh_handles_and_drop_releases_them and the deferred-snapshot-boundary tests (task 98 / hm-d8o)"
    )]
    fn recorded_env_replays_to_the_same_hash() {
        // Task 59 requirement 3 (portable form of the box gate's record→replay
        // closure): every applied fault is stamped into the recorded env, and
        // re-applying that env's host faults reproduces the run's state_hash.
        let schedule = vec![
            (
                1,
                EnvHostEffect::XorMemory {
                    gpa: 0x20,
                    bytes: (0x1234_5678_9ABC_DEF0_u64).to_le_bytes().to_vec(),
                },
            ),
            (2, EnvHostEffect::InjectInterrupt { vector: 0x60 }),
        ];
        let seed = 0xC105u64;
        let (h1, recorded) = enforce_run(&schedule, seed);

        // The recorded env carries both host faults on the Moment axis, and it
        // round-trips through its own byte codec (a real reproducer blob).
        let host: Vec<_> = recorded
            .effects()
            .iter()
            .map(|(&at, effect)| (at, effect.clone()))
            .collect();
        assert_eq!(host.len(), 2, "both applied faults were stamped");
        let reencoded = EnvSpec::decode(&recorded.encode()).expect("recorded env round-trips");
        let replay_schedule: Vec<(u64, EnvHostEffect)> = reencoded
            .effects()
            .iter()
            .map(|(&at, effect)| (at, effect.clone()))
            .collect();

        // Re-applying the emitted schedule reproduces the hash bit-for-bit.
        let h2 = enforce_hash(&replay_schedule, seed);
        assert_eq!(
            h1, h2,
            "the recorded env replays to the identical state_hash"
        );
    }

    /// A V-time-wired VM that takes a single RDTSC to effective V-time `rdtsc_work`,
    /// then Hlt — used to drive the beyond-deadline / overshoot cases with a chosen
    /// V-time landing (no arrival armed, so `run()` returns the scripted RDTSC).
    fn rdtsc_then_hlt_vmm(rdtsc_work: u64) -> Vmm<MockBackend> {
        let mut m = MockBackend::with_exits(vec![
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Common(CommonExit::Idle),
        ]);
        m.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
        let mut cfg = contract_vclock_config();
        cfg.vns_base = rdtsc_work.saturating_sub(1);
        v.wire_vtime(VtimeWiring::new_virtual_time(cfg, 1).unwrap());
        v.wire_snapshot_hashing();
        v.restore_guest_memory(&enforce_image()).unwrap();
        v
    }

    /// A server over [`rdtsc_then_hlt_vmm`] whose factory boots identically-composed
    /// restore targets, so `branch`/`replay` (the poison recovery) succeed.
    fn rdtsc_then_hlt_server(rdtsc_work: u64) -> ControlServer<MockBackend> {
        ControlServer::new(
            rdtsc_then_hlt_vmm(rdtsc_work),
            Box::new(move || Ok(rdtsc_then_hlt_vmm(rdtsc_work))),
        )
    }

    fn stage_corrupt(s: &mut ControlServer<MockBackend>, at: u64) {
        s.handle(&Request::Perturb {
            fault: HostFault(
                EnvHostEffect::XorMemory {
                    gpa: 0x40,
                    bytes: (0xFFFF_FFFF_u64).to_le_bytes().to_vec(),
                }
                .encode(),
            ),
            at: Moment(at),
        })
        .unwrap()
        .unwrap();
    }

    fn run_with_deadline(
        s: &mut ControlServer<MockBackend>,
        deadline: u64,
    ) -> Result<Reply, ControlError> {
        s.handle(&Request::Run {
            until: StopConditions {
                deadline: Some(Moment(deadline)),
                on: StopMask::NONE,
            },
            resolve: None,
        })
        .unwrap()
    }

    #[test]
    fn a_beyond_deadline_fault_not_yet_crossed_stays_staged() {
        // A fault beyond `until.deadline` that the run does NOT execute past stays
        // staged (satisfiable by a later run): RDTSC lands at exactly the deadline
        // 1000, the fault is at 1500 > 1000, so it is neither applied nor crossed.
        let mut s = rdtsc_then_hlt_server(1000);
        hello(&mut s);
        stage_corrupt(&mut s, 1500);
        match run_with_deadline(&mut s, 1000) {
            Ok(Reply::Stop(StopReason::Deadline { vtime })) => assert_eq!(vtime, Moment(1000)),
            other => panic!("expected Deadline, got {other:?}"),
        }
        assert_eq!(
            &s.vmm().unwrap().guest_memory()[0x40..0x48],
            &[0u8; 8],
            "the beyond-deadline fault did not apply"
        );
        assert_eq!(
            s.recorded_env().effects().len(),
            0,
            "nothing recorded — the fault is still staged for a later run"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn snapshot_with_a_staged_fault_preserves_the_plan() {
        let mut s = rdtsc_then_hlt_server(2000);
        hello(&mut s);
        let base = snap(&mut s);
        stage_corrupt(&mut s, 3000);
        let before = hash(&mut s);
        let counts = s.vmm().unwrap().exit_counts();
        let pending = snap(&mut s);
        assert_eq!(hash(&mut s), before);
        assert_eq!(s.vmm().unwrap().exit_counts(), counts);
        assert_eq!(s.handle(&Request::Replay(base)).unwrap(), Ok(Reply::Unit));
        assert!(s.schedule.is_empty());
        assert_eq!(
            s.handle(&Request::Replay(pending)).unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(hash(&mut s), before);
        assert!(s.schedule.contains_key(&3000));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn branch_env_host_faults_go_through_the_same_validation_as_perturb() {
        // Blocking item 1c: an env host fault that is out-of-range / out-of-scope /
        // behind the snapshot is a recoverable ControlError at branch time (the same
        // reply `perturb` gives), NOT a later session-fatal ServeError at apply time.
        // The `server()` live VM is at effective V-time 500, so the snapshot — and
        // the restored floor — is 500.
        let host_env = |m: u64, fault: EnvHostEffect| {
            let mut spec = EnvSpec::seeded(7);
            spec.record_effect(m, fault);
            Reproducer {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: spec.encode(),
            }
        };

        // (i) out-of-range gpa → recoverable PerturbOutOfRange (was a fatal
        // ServeError::Vmm before this fix).
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: host_env(
                    1000,
                    EnvHostEffect::XorMemory {
                        gpa: RAM as u64 - 4,
                        bytes: (0xFF_u64).to_le_bytes().to_vec(),
                    },
                ),
            })
            .unwrap(),
            Err(ControlError::PerturbOutOfRange {
                gpa: RAM as u64 - 4,
                ram_len: RAM as u64,
            })
        );
        // The session stays usable (fresh VM kept) and nothing was staged.
        assert!(s.vmm().is_some());

        // (ii) a Moment behind the restored snapshot's V-time (500) → PerturbPastMoment.
        let base = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: host_env(100, EnvHostEffect::InjectInterrupt { vector: 40 }),
            })
            .unwrap(),
            Err(ControlError::PerturbPastMoment {
                at: 100,
                floor: 500
            })
        );

        // (iii) an out-of-scope SkewTime → Unsupported.
        let base = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: {
                    let mut spec = EnvSpec::seeded(7);
                    spec.set_config(ServiceConfig {
                        identity: b"unavailable".to_vec(),
                        configuration: vec![],
                    });
                    Reproducer {
                        blob_version: EnvSpec::BLOB_VERSION,
                        bytes: spec.encode(),
                    }
                },
            })
            .unwrap(),
            Err(ControlError::Unsupported)
        );

        // (iv) a valid future host fault is accepted and staged (Reply::Unit).
        let base = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: host_env(1000, EnvHostEffect::InjectInterrupt { vector: 40 }),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
    }

    /// Build a branch env carrying a single host fault at `m`.
    fn host_env(m: u64, fault: EnvHostEffect) -> Reproducer {
        let mut spec = EnvSpec::seeded(7);
        spec.record_effect(m, fault);
        Reproducer {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: spec.encode(),
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn a_rejected_branch_env_fault_is_side_effect_free() {
        // PR #51 round-5 item 1: a branch whose env carries an inadmissible host
        // fault must reject WITHOUT swapping the VM — the old timeline is untouched,
        // so the client's state is exactly what it was before the (failed) branch.
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);
        let before = s.vmm().unwrap().state_hash().unwrap();

        // An out-of-range gpa is rejected — validated against the LIVE VM before any
        // drop/restore/reseed.
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: host_env(
                    1000,
                    EnvHostEffect::XorMemory {
                        gpa: RAM as u64 - 4,
                        bytes: (0xFF_u64).to_le_bytes().to_vec(),
                    },
                ),
            })
            .unwrap(),
            Err(ControlError::PerturbOutOfRange {
                gpa: RAM as u64 - 4,
                ram_len: RAM as u64,
            })
        );
        // The live VM is BYTE-IDENTICAL to before: not dropped, not restored, not
        // reseeded — the rejected branch had no side effect.
        assert_eq!(
            s.vmm().unwrap().state_hash().unwrap(),
            before,
            "a rejected branch env fault must leave the old VM untouched"
        );
        // And the session is fully usable: it can still snapshot + branch cleanly.
        let base2 = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base2,
                env: seeded_env(9),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn a_terminal_stop_with_a_staged_fault_poisons_loud() {
        // A terminal with a pending plan latches a schedule failure. Saving
        // and cold replay must retain both the terminal state and that failure.
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);
        // The live VM is at V-time 500 and HLTs (terminal) on the next step. Stage a
        // fault beyond that terminal point.
        stage_corrupt(&mut s, 100_000);
        // Running to the terminal HLT poisons (the fault is still staged).
        let poisoned = Err(ControlError::ScheduleUnsatisfiable {
            moment: 100_000,
            vtime: 500,
        });
        assert_eq!(run_all_res(&mut s), poisoned);
        // The fault never applied (recorded stays empty).
        assert_eq!(s.recorded_env().effects().len(), 0);
        // Re-run rejects, but snapshot captures this same failed endpoint.
        assert_eq!(run_all_res(&mut s), poisoned);
        let failed = snap(&mut s);
        let mut artifact = Vec::new();
        let receipt = s.export_portable_snapshot(failed, &mut artifact).unwrap();
        let mut cold = server(vec![Exit::Common(CommonExit::Idle)]);
        let imported = cold.import_portable_snapshot(artifact.as_slice()).unwrap();
        hello(&mut cold);
        assert_eq!(
            cold.handle(&Request::Replay(imported.id)).unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(hash(&mut cold), receipt.state_hash);
        assert_eq!(run_all_res(&mut cold), poisoned);
        assert_eq!(
            cold.handle(&Request::Replay(imported.id)).unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(run_all_res(&mut cold), poisoned);
        cold.handle(&Request::Branch {
            snap: imported.id,
            env: seeded_env(9),
        })
        .unwrap()
        .unwrap();
        assert!(cold.schedule_poisoned.is_none());
        assert!(cold.schedule.is_empty());
        // A rewind (replay of the pristine base) clears the poison — the session runs
        // and snapshots cleanly again.
        assert_eq!(s.handle(&Request::Replay(base)).unwrap(), Ok(Reply::Unit));
        assert!(matches!(
            run_all_res(&mut s),
            Ok(Reply::Stop(StopReason::Quiescent { .. }))
        ));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn perturb_after_a_synchronized_deadline_stop_reproduces() {
        // The positive half of the sync gate + a recorded-env replay-equivalence
        // check: a `perturb` after a **deadline stop that landed on an arrival**
        // (synchronized) is accepted, and the recorded env — branched from base and
        // re-run — reproduces the live `state_hash` across the multi-run session.
        let run_to = |s: &mut ControlServer<ExitBoundaryBackend>, d: u64| {
            s.handle(&Request::Run {
                until: StopConditions {
                    deadline: Some(Moment(d)),
                    on: StopMask::NONE,
                },
                resolve: None,
            })
            .unwrap()
        };
        let perturb = |s: &mut ControlServer<ExitBoundaryBackend>, gpa: u64, at: u64| {
            s.handle(&Request::Perturb {
                fault: HostFault(
                    EnvHostEffect::XorMemory {
                        gpa,
                        bytes: (0xDEAD_0000_BEEF_u64).to_le_bytes().to_vec(),
                    }
                    .encode(),
                ),
                at: Moment(at),
            })
            .unwrap()
        };

        let mut s = exit_boundary_server();
        arr_hello(&mut s);
        let base = arr_snap(&mut s);
        // Stage + apply the first fault at Moment 100, stopping AT it (a synchronized
        // arrival deadline stop).
        assert_eq!(perturb(&mut s, 0x40, 100), Ok(Reply::Unit));
        assert!(matches!(
            run_to(&mut s, 100),
            Ok(Reply::Stop(StopReason::Deadline { .. }))
        ));
        // The deadline stop landed on the arrival → synchronized → a second perturb is
        // accepted at the exact exit boundary.
        assert_eq!(perturb(&mut s, 0x80, 300), Ok(Reply::Unit));
        assert!(matches!(arr_run(&mut s), Ok(Reply::Stop(_))));
        let h_live = arr_hash(&s);
        let recorded = s.recorded_env().clone();
        assert_eq!(recorded.effects().len(), 2, "both faults recorded");

        // Replay-equivalence: branch the recorded env from base and re-run → the same
        // live hash.
        let mut r = exit_boundary_server();
        arr_hello(&mut r);
        let base_r = arr_snap(&mut r);
        r.handle(&Request::Branch {
            snap: base_r,
            env: Reproducer {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: recorded.encode(),
            },
        })
        .unwrap()
        .unwrap();
        assert!(matches!(arr_run(&mut r), Ok(Reply::Stop(_))));
        assert_eq!(
            h_live,
            arr_hash(&r),
            "recorded env reproduces the multi-run hash"
        );
        let _ = base;
    }

    #[test]
    fn a_fault_at_current_vtime_with_an_expired_deadline_is_not_poisoned() {
        // PR #51 round-5 item 3: a fault at exactly the current V-time (m == vns) is
        // EXIT-BOUNDARY delivery, not late — even when the run's deadline is already expired
        // (d < vns). It must apply and stop with `Deadline`, never poison. Live VM
        // RDTSCs to V-time 500; the fault and deadline are both 500.
        let mut s = rdtsc_then_hlt_server(500);
        hello(&mut s);
        stage_corrupt(&mut s, 500);
        match run_with_deadline(&mut s, 500) {
            Ok(Reply::Stop(StopReason::Deadline { vtime })) => assert_eq!(vtime, Moment(500)),
            other => panic!("expected Deadline{{500}}, got {other:?}"),
        }
        // The exit-boundary fault APPLIED (recorded), and the schedule is NOT poisoned
        // — a later run works.
        assert_eq!(
            s.recorded_env().effects().len(),
            1,
            "the m==vns fault applied"
        );
        assert_ne!(
            &s.vmm().unwrap().guest_memory()[0x40..0x48],
            &[0u8; 8],
            "the exit-boundary upset landed"
        );
        assert!(
            matches!(run_all_res(&mut s), Ok(Reply::Stop(_))),
            "not poisoned"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn a_branch_env_moment_occupies_the_schedule_for_ruling_b() {
        // Ruling B across the branch→perturb boundary (PR #51 round-5 suggestion): a
        // branch env stages a fault at Moment M; a later `perturb` at M is then the
        // loud `PerturbMomentTaken` (one fault per Moment holds after a branch, not
        // just within a fresh session). Pins the invariant so a future batch-validate
        // refactor can't silently regress it.
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: host_env(1000, EnvHostEffect::InjectInterrupt { vector: 40 }),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(
            s.handle(&Request::Perturb {
                fault: HostFault(EnvHostEffect::InjectInterrupt { vector: 41 }.encode()),
                at: Moment(1000),
            })
            .unwrap(),
            Err(ControlError::PerturbMomentTaken { at: 1000 }),
            "a branch-staged Moment occupies the schedule for ruling B"
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(384))]

        /// Gate 1 proptest (≥256 cases): an arbitrary staged schedule applied twice
        /// yields identical state evolution. `Moment`s are **distinct** (a
        /// `BTreeMap` key — the one-fault-per-Moment rule rejects duplicates, so the
        /// schedule under test never carries them) in `1..=32` (the fixture emits
        /// one one-nanosecond exit per moment); faults are in-range CorruptMemory (any gpa whose word
        /// fits the 16 KiB RAM, any mask) or InjectInterrupt (any non-reserved vector).
        #[test]
        // Each case boots + runs + state-hashes (sha256) a VM twice via `enforce_run`;
        // under Miri that is ~40 s per `enforce_run`, so even a handful of cases costs
        // minutes. `enforce_run` boots (it never restores), so its map_memory site is
        // the boot-path seam exercised under Miri by
        // bringup::tests::compose_drives_guestram_and_unsafe_map_memory; the property
        // itself is covered by the native 384-case run — so it is ignored under Miri
        // like the restore-driving proptests (task 98), not merely case-reduced.
        #[cfg_attr(miri, ignore = "VM-running property test, too slow under Miri; covered natively (task 98)")]
        fn arbitrary_schedule_applied_twice_is_identical(
            schedule in proptest::collection::btree_map(
                1u64..=32u64,
                prop_oneof![
                    (0u64..(RAM as u64 - 8), any::<u64>())
                        .prop_map(|(gpa, m)| EnvHostEffect::XorMemory { gpa, bytes: m.to_le_bytes().to_vec() }),
                    (16u32..=255u32)
                        .prop_map(|vector| EnvHostEffect::InjectInterrupt { vector }),
                ],
                0..8usize,
            ),
        ) {
            let sched: Vec<(u64, EnvHostEffect)> = schedule.into_iter().collect();
            let seed = 0x9159_2653;
            let h1 = enforce_hash(&sched, seed);
            let h2 = enforce_hash(&sched, seed);
            prop_assert_eq!(h1, h2, "same schedule ⇒ identical state evolution");
        }
    }

    // -----------------------------------------------------------------------
    // PR #51 round-2 — the recorded-env-reproduces invariant across the full verb
    // space, plus the four corners the fresh cross-model pass found.
    // -----------------------------------------------------------------------

    /// A mock-wrapping backend that makes each scripted exit boundary land at
    /// the next armed arrival, while an unarmed `run()` is a terminal `Hlt`.
    /// This lets a *random* verb
    /// sequence drive any number of arrivals + runs without pre-scripting exits.
    /// `deterministic_tsc` (forwarded from the inner mock) is `true`, so the server
    /// treats it as an armable host-plane backend.
    struct ExitBoundaryBackend {
        inner: MockBackend,
        exits_left: u64,
    }
    impl Backend for ExitBoundaryBackend {
        type A = vmm_backend::X86;

        fn set_policy(&mut self, policy: &vmm_backend::X86Policy) -> vmm_backend::Result<()> {
            self.inner.set_policy(policy)
        }
        unsafe fn map_memory(
            &mut self,
            gpa: vmm_backend::Gpa,
            host: &mut [u8],
        ) -> vmm_backend::Result<()> {
            // SAFETY: forwards to the inner mock, which only records the region.
            unsafe { self.inner.map_memory(gpa, host) }
        }
        fn run(&mut self) -> vmm_backend::Result<Exit<vmm_backend::X86>> {
            if self.exits_left == 0 {
                self.inner.extend_exits([Exit::Common(CommonExit::Idle)]);
            } else {
                self.exits_left -= 1;
                self.inner
                    .extend_exits([Exit::Arch(X86Exit::Rdmsr { index: 0x10 })]);
            }
            self.inner.run()
        }
        fn inject(&mut self, e: vmm_backend::Injection) -> vmm_backend::Result<()> {
            self.inner.inject(e)
        }
        fn set_pending_irq(&mut self, v: Option<u8>) -> vmm_backend::Result<()> {
            self.inner.set_pending_irq(v)
        }
        fn take_accepted_interrupt(&mut self) -> Option<u8> {
            self.inner.take_accepted_interrupt()
        }
        fn complete_read(&mut self, v: u64) -> vmm_backend::Result<()> {
            self.inner.complete_read(v)
        }
        fn complete_fault(&mut self) -> vmm_backend::Result<()> {
            self.inner.complete_fault()
        }
        fn complete_ok(&mut self) -> vmm_backend::Result<()> {
            self.inner.complete_ok()
        }
        fn complete_hypercall(&mut self, rax: u64) -> vmm_backend::Result<()> {
            self.inner.complete_hypercall(rax)
        }
        fn complete_arch(&mut self, c: vmm_backend::X86Completion) -> vmm_backend::Result<()> {
            self.inner.complete_arch(c)
        }
        fn save(&self) -> vmm_backend::Result<vmm_backend::VcpuState> {
            self.inner.save()
        }
        fn restore(&mut self, s: &vmm_backend::VcpuState) -> vmm_backend::Result<()> {
            self.inner.restore(s)?;
            self.exits_left = 512;
            Ok(())
        }
        fn exit_counts(&self) -> vmm_backend::ExitCounts {
            self.inner.exit_counts()
        }
        fn reset_exit_counts(&mut self) {
            self.inner.reset_exit_counts()
        }
        fn capabilities(&self) -> vmm_backend::Capabilities<vmm_backend::X86Caps> {
            self.inner.capabilities()
        }
    }

    fn exit_boundary_vmm(seed: u64) -> Vmm<ExitBoundaryBackend> {
        let mut m = MockBackend::with_exits(vec![]);
        m.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut v = Vmm::new(
            ExitBoundaryBackend {
                inner: m,
                exits_left: 512,
            },
            GuestRam::new(RAM).unwrap(),
        );
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), seed).unwrap());
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        v.wire_snapshot_hashing();
        v.restore_guest_memory(&enforce_image()).unwrap();
        v
    }

    /// A server over the exit-boundary mock, whose factory boots identically-composed
    /// restore targets (so `branch`/`replay` succeed). Seed `0x59` for the live VM
    /// and every fork.
    fn exit_boundary_server() -> ControlServer<ExitBoundaryBackend> {
        let live = exit_boundary_vmm(0x59);
        ControlServer::new(live, Box::new(|| Ok(exit_boundary_vmm(0x59))))
    }

    fn arr_hello<B: Backend<A: Vendor>>(s: &mut ControlServer<B>) {
        assert!(s.handle(&Request::Hello(server_caps())).unwrap().is_ok());
    }
    fn arr_snap<B: Backend<A: Vendor>>(s: &mut ControlServer<B>) -> SnapId {
        match s.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::Snapshot { id, .. }) => id,
            other => panic!("snapshot: {other:?}"),
        }
    }
    fn arr_run<B: Backend<A: Vendor>>(s: &mut ControlServer<B>) -> Result<Reply, ControlError> {
        s.handle(&Request::Run {
            until: StopConditions {
                deadline: None,
                on: StopMask::NONE,
            },
            resolve: None,
        })
        .unwrap()
    }
    fn arr_hash<B: Backend<A: Vendor>>(s: &ControlServer<B>) -> [u8; 32] {
        s.vmm().unwrap().state_hash().unwrap()
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "full and sparse import materialize snapshot-store mappings"
    )]
    fn queued_inputs_continue_identically_without_reapplying_the_consumed_prefix() {
        fn whole(s: &mut ControlServer<ExitBoundaryBackend>) -> [u8; 32] {
            match s
                .handle(&Request::Hash {
                    scope: HashScope::Whole,
                })
                .unwrap()
                .unwrap()
            {
                Reply::Hash(hash) => hash,
                other => panic!("unexpected hash reply: {other:?}"),
            }
        }
        fn run_to(s: &mut ControlServer<ExitBoundaryBackend>, at: u64) {
            assert_eq!(
                s.handle(&Request::Run {
                    until: StopConditions {
                        deadline: Some(Moment(at)),
                        on: StopMask::NONE
                    },
                    resolve: None,
                })
                .unwrap(),
                Ok(Reply::Stop(StopReason::Deadline { vtime: Moment(at) }))
            );
        }
        fn setup() -> (ControlServer<ExitBoundaryBackend>, SnapId) {
            let mut s = exit_boundary_server();
            arr_hello(&mut s);
            let base = arr_snap(&mut s);
            let mut plan = EnvSpec::seeded(7);
            plan.record_effect(
                1,
                EnvHostEffect::WriteMemory {
                    gpa: 0x40,
                    bytes: vec![0x55],
                },
            );
            plan.record_effect(
                2,
                EnvHostEffect::XorMemory {
                    gpa: 0x40,
                    bytes: vec![0x0f],
                },
            );
            plan.record_effect(3, EnvHostEffect::InjectInterrupt { vector: 0x60 });
            plan.record_reseed(0, 7);
            plan.record_reseed(2, 99);
            s.handle(&Request::Branch {
                snap: base,
                env: Reproducer {
                    blob_version: EnvSpec::BLOB_VERSION,
                    bytes: plan.encode(),
                },
            })
            .unwrap()
            .unwrap();
            run_to(&mut s, 1);
            // Command identifiers are host-owned state too. A later command must
            // not reuse an identifier already visible in the restored guest.
            s.exec_nonce = 17;
            (s, base)
        }
        let mut endpoints = Vec::new();
        for path in 0..4 {
            let (mut s, base) = setup();
            let before_hash = whole(&mut s);
            let before_counts = s.vmm().unwrap().exit_counts();
            let before_vm = s.vmm().unwrap().save_vm_state().unwrap().encode().unwrap();
            if path != 0 {
                let cut = arr_snap(&mut s);
                assert_eq!(whole(&mut s), before_hash, "save executed no input");
                assert_eq!(
                    s.vmm().unwrap().exit_counts(),
                    before_counts,
                    "save entered no guest"
                );
                assert_eq!(
                    s.vmm().unwrap().save_vm_state().unwrap().encode().unwrap(),
                    before_vm
                );
                if path >= 2 {
                    let mut cold = exit_boundary_server();
                    arr_hello(&mut cold);
                    let imported = if path == 2 {
                        let mut artifact = Vec::new();
                        let receipt = s.export_portable_snapshot(cut, &mut artifact).unwrap();
                        assert_eq!(receipt.state_hash, before_hash);
                        cold.import_portable_snapshot(artifact.as_slice())
                            .unwrap()
                            .id
                    } else {
                        let cold_base = arr_snap(&mut cold);
                        let artifact = s.export_sparse_snapshot(base, cut).unwrap();
                        cold.import_sparse_snapshot(cold_base, artifact).unwrap().id
                    };
                    drop(s);
                    cold.handle(&Request::Replay(imported)).unwrap().unwrap();
                    s = cold;
                    assert_eq!(
                        whole(&mut s),
                        before_hash,
                        "cold restore retained complete state"
                    );
                    assert_eq!(
                        s.vmm().unwrap().save_vm_state().unwrap().encode().unwrap(),
                        before_vm
                    );
                }
            }
            assert_eq!(s.exec_nonce, 17);
            assert_eq!(s.recorded.effects().len(), 1);
            assert_eq!(s.schedule.len(), 2);
            assert_eq!(s.reseed_schedule.len(), 1);
            assert_eq!(
                s.handle(&Request::Perturb {
                    fault: HostFault(
                        EnvHostEffect::WriteMemory {
                            gpa: 0x40,
                            bytes: vec![0]
                        }
                        .encode()
                    ),
                    at: Moment(1),
                })
                .unwrap(),
                Err(ControlError::PerturbMomentTaken { at: 1 }),
                "the consumed prefix survives"
            );
            run_to(&mut s, 3);
            assert!(s.schedule.is_empty());
            assert!(s.reseed_schedule.is_empty());
            assert_eq!(
                s.vmm().unwrap().guest_memory()[0x40],
                0x5a,
                "XOR executed exactly once"
            );
            assert_eq!(s.recorded.effects().len(), 3);
            assert_eq!(s.recorded.reseeds().get(&2), Some(&99));
            let endpoint = (
                whole(&mut s),
                s.vmm().unwrap().save_vm_state().unwrap().encode().unwrap(),
                s.recorded.encode(),
                s.vmm().unwrap().sdk_events().to_vec(),
                s.vmm().unwrap().effective_vns(),
            );
            run_to(&mut s, 3);
            assert_eq!(
                whole(&mut s),
                endpoint.0,
                "repeated continuation does not reapply inputs"
            );
            endpoints.push(endpoint);
        }
        for endpoint in &endpoints[1..] {
            assert_eq!(
                endpoint, &endpoints[0],
                "uninterrupted, saved, full cold, and sparse cold agree"
            );
        }
    }

    /// A **no-op** host fault (`CorruptMemory` with a zero XOR mask) — it changes no
    /// guest byte, but staging it at a `Moment` arms the exact-count arrival so a
    /// `run` lands there. On the portable mock this is the stand-in for the box's
    /// exit-boundary arrival: the mock's bare `run()` HLTs immediately, while an
    /// armed arrival assigns the next scripted exit to the exact `Moment`.
    fn schedule_marker(at: u64) -> Request {
        Request::Perturb {
            fault: HostFault(
                EnvHostEffect::XorMemory {
                    gpa: 0,
                    bytes: (0_u64).to_le_bytes().to_vec(), // XOR 0 — the state is untouched
                }
                .encode(),
            ),
            at: Moment(at),
        }
    }

    /// The **moment-address materialization procedure** (task 80), exercised
    /// portably on the exit-boundary mock: given `(env, moment)` with a
    /// genesis-complete `env`, `branch(genesis, env)` then advance to the exact
    /// `Moment` (here via a no-op arrival marker — see [`schedule_marker`]; on the
    /// box the deadline instruction-level stops) and read that materialized point with the
    /// observation verbs. Materializing the same address **twice from genesis**
    /// yields byte-identical `regs` (including `rip` and `moment`), `read`, and
    /// `hash(Whole)` — the address is a stable coordinate. (The box gate proves the
    /// same against the live Postgres workload, where the state actually differs
    /// Moment-to-Moment; the mock's static image makes this a determinism/mechanism
    /// proof, not a state-evolution one.)
    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn moment_address_materializes_identically_twice() {
        let mut s = exit_boundary_server();
        arr_hello(&mut s);
        let genesis = arr_snap(&mut s); // the genesis snapshot (V-time 0)
        let env = seeded_env_arr(0x0080_0080); // a genesis-complete Seeded env

        let materialize = |s: &mut ControlServer<ExitBoundaryBackend>,
                           moment: u64|
         -> (control_proto::RegsView, Vec<u8>, [u8; 32]) {
            // branch(genesis, env) — restore genesis + reseed from env's seed.
            assert_eq!(
                s.handle(&Request::Branch {
                    snap: genesis,
                    env: env.clone()
                })
                .unwrap(),
                Ok(Reply::Unit)
            );
            // Advance to the exact-`Moment` stop.
            s.handle(&schedule_marker(moment)).unwrap().unwrap();
            let stop = match s
                .handle(&Request::Run {
                    until: StopConditions {
                        deadline: Some(Moment(moment)),
                        on: StopMask::NONE,
                    },
                    resolve: None,
                })
                .unwrap()
            {
                Ok(Reply::Stop(st)) => st,
                other => panic!("run answered {other:?}"),
            };
            assert_eq!(
                stop,
                StopReason::Deadline {
                    vtime: Moment(moment)
                },
                "materialization lands exactly at the addressed Moment"
            );
            // Observe: regs, a probe read, and the whole-state hash.
            let view = match s.handle(&Request::Regs).unwrap() {
                Ok(Reply::Regs(v)) => v,
                other => panic!("regs answered {other:?}"),
            };
            assert_eq!(
                view.moment.0, moment,
                "regs reports the exit count == the addressed Moment"
            );
            assert_eq!(view.vtime, moment, "vtime coincides with moment");
            let bytes = match s.handle(&Request::Read { gpa: 0, len: 128 }).unwrap() {
                Ok(Reply::Bytes(b)) => b,
                other => panic!("read answered {other:?}"),
            };
            (view, bytes, arr_hash(s))
        };

        for moment in [1u64, 5, 50, 250] {
            let (r1, b1, h1) = materialize(&mut s, moment);
            let (r2, b2, h2) = materialize(&mut s, moment);
            assert_eq!(
                r1, r2,
                "regs identical across two materializations @ {moment}"
            );
            assert_eq!(
                b1, b2,
                "read identical across two materializations @ {moment}"
            );
            assert_eq!(
                h1, h2,
                "hash identical across two materializations @ {moment}"
            );
        }
    }

    /// Observation invariance **during materialization** (task 80 gate 3, portable
    /// analogue): a full inspection pass (regs + several reads) at an intermediate
    /// Moment does not perturb the run — continuing to a later Moment yields the
    /// same `hash(Whole)` as an uninspected control that reaches the later Moment
    /// through the identical arrival schedule.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn inspection_mid_materialization_does_not_perturb_the_continuation() {
        let env = seeded_env_arr(0x0B5E_0BED);
        let (mid, late) = (10u64, 90u64);

        // One materialization to `mid`→`late` through two arrival markers, with an
        // optional inspection pass at `mid`. `exit_boundary_server`s are freshly and
        // identically composed, so the two runs differ only in the inspection.
        let run_to_late = |inspect: bool| -> [u8; 32] {
            let mut s = exit_boundary_server();
            arr_hello(&mut s);
            let genesis = arr_snap(&mut s);
            s.handle(&Request::Branch {
                snap: genesis,
                env: env.clone(),
            })
            .unwrap()
            .unwrap();
            // Advance to `mid`.
            s.handle(&schedule_marker(mid)).unwrap().unwrap();
            assert!(matches!(
                s.handle(&Request::Run {
                    until: StopConditions {
                        deadline: Some(Moment(mid)),
                        on: StopMask::NONE
                    },
                    resolve: None,
                })
                .unwrap(),
                Ok(Reply::Stop(StopReason::Deadline { .. }))
            ));
            if inspect {
                // A full inspection pass at the intermediate Moment.
                let _ = s.handle(&Request::Regs).unwrap();
                let _ = s.handle(&Request::Read { gpa: 0, len: 64 }).unwrap();
                let _ = s
                    .handle(&Request::Read {
                        gpa: RAM as u64 - 16,
                        len: 16,
                    })
                    .unwrap();
                let _ = s.handle(&Request::Regs).unwrap();
            }
            // Continue to `late`.
            s.handle(&schedule_marker(late)).unwrap().unwrap();
            assert!(matches!(
                s.handle(&Request::Run {
                    until: StopConditions {
                        deadline: Some(Moment(late)),
                        on: StopMask::NONE
                    },
                    resolve: None,
                })
                .unwrap(),
                Ok(Reply::Stop(StopReason::Deadline { .. }))
            ));
            arr_hash(&s)
        };

        assert_eq!(
            run_to_late(true),
            run_to_late(false),
            "an inspection pass mid-materialization perturbed the continuation"
        );
    }

    #[test]
    fn reperturb_at_an_applied_moment_is_rejected() {
        // Finding 2: once a fault at `m` has APPLIED it is gone from the schedule
        // but present in `recorded`; a second perturb at `m` must still be rejected
        // (else `EnvSpec::perturb` overwrites the first and it vanishes from
        // `recorded_env()`). Run to a deadline of exactly `m` so the live V-time is
        // `m` (== floor), the one case that passes the past-Moment check.
        let mut s = exit_boundary_server();
        arr_hello(&mut s);
        // Stage + apply a fault at Moment 100 by running to deadline 100.
        s.handle(&Request::Perturb {
            fault: HostFault(EnvHostEffect::InjectInterrupt { vector: 0x40 }.encode()),
            at: Moment(100),
        })
        .unwrap()
        .unwrap();
        // Run with deadline 100: arrival lands at 100, applies, then the deadline
        // stops the run at V-time 100.
        let stop = s
            .handle(&Request::Run {
                until: StopConditions {
                    deadline: Some(Moment(100)),
                    on: StopMask::NONE,
                },
                resolve: None,
            })
            .unwrap();
        assert!(matches!(stop, Ok(Reply::Stop(StopReason::Deadline { .. }))));
        assert_eq!(s.recorded_env().effects().len(), 1, "the fault applied");
        assert_eq!(s.vmm().unwrap().effective_vns(), Some(100));
        // A second perturb at the already-APPLIED Moment 100 (== floor, so it clears
        // the past-Moment check) is rejected — it does not overwrite the recorded fault.
        assert_eq!(
            s.handle(&Request::Perturb {
                fault: HostFault(EnvHostEffect::InjectInterrupt { vector: 0x41 }.encode()),
                at: Moment(100),
            })
            .unwrap(),
            Err(ControlError::PerturbMomentTaken { at: 100 })
        );
        assert_eq!(
            s.recorded_env().effects().len(),
            1,
            "the applied fault is still recorded (not overwritten)"
        );
    }

    #[test]
    fn perturb_on_an_unarmable_backend_is_unsupported() {
        // Finding 3: a backend that cannot arm the exit-boundary seam (V-time
        // unwired, or deterministic_tsc=false) would apply a staged fault late.
        // Reject perturb up front with Unsupported. Here: a mock with NO V-time wired.
        let mut m = MockBackend::with_exits(vec![Exit::Common(CommonExit::Idle)]);
        m.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let v = Vmm::new(m, GuestRam::new(RAM).unwrap()); // V-time NOT wired
        let mut s = ControlServer::new(
            v,
            Box::new(|| Err(VmmError::ContractViolation("unused".into()))),
        );
        assert!(s.handle(&Request::Hello(server_caps())).unwrap().is_ok());
        assert_eq!(
            s.handle(&Request::Perturb {
                fault: HostFault(EnvHostEffect::InjectInterrupt { vector: 0x40 }.encode()),
                at: Moment(10),
            })
            .unwrap(),
            Err(ControlError::Unsupported),
            "an unarmable backend cannot enforce host faults exactly"
        );
    }

    #[test]
    fn perturb_inject_interrupt_reserved_vector_is_rejected_at_stage_time() {
        // PR #51 round-8 item 1: an InjectInterrupt with an architecturally reserved
        // vector (0..=15) is a stage-time-decidable request error — a recoverable
        // `PerturbReservedVector`, not a session-fatal apply-time `ServeError`.
        let mut s = exit_boundary_server(); // LAPIC wired, synchronized
        arr_hello(&mut s);
        for vector in [0u32, 1, 15] {
            assert_eq!(
                s.handle(&Request::Perturb {
                    fault: HostFault(EnvHostEffect::InjectInterrupt { vector }.encode()),
                    at: Moment(100),
                })
                .unwrap(),
                Err(ControlError::PerturbReservedVector {
                    vector: vector as u8
                })
            );
        }
        // A non-reserved vector still stages cleanly.
        assert_eq!(
            s.handle(&Request::Perturb {
                fault: HostFault(EnvHostEffect::InjectInterrupt { vector: 16 }.encode()),
                at: Moment(100),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
    }

    #[test]
    fn perturb_inject_interrupt_on_a_no_lapic_vm_is_unsupported() {
        // PR #51 round-8 item 1: an InjectInterrupt on a VM with no userspace LAPIC
        // has no interrupt controller to raise into — reject at stage time with the
        // recoverable `Unsupported`, not a session-fatal apply-time failure. (The VM
        // is V-time-wired + deterministic, so `CorruptMemory` would still be armable.)
        let mut s = rdtsc_then_hlt_server(500); // V-time wired, NO LAPIC
        hello(&mut s);
        assert_eq!(
            s.handle(&Request::Perturb {
                fault: HostFault(EnvHostEffect::InjectInterrupt { vector: 0x40 }.encode()),
                at: Moment(1000),
            })
            .unwrap(),
            Err(ControlError::Unsupported),
            "no LAPIC ⇒ InjectInterrupt cannot be delivered — rejected at stage time"
        );
        // CorruptMemory on the same no-LAPIC VM is still accepted (round-6 idle wake).
        assert_eq!(
            s.handle(&Request::Perturb {
                fault: HostFault(
                    EnvHostEffect::XorMemory {
                        gpa: 0x40,
                        bytes: (0xFF_u64).to_le_bytes().to_vec(),
                    }
                    .encode(),
                ),
                at: Moment(1000),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn recoverable_restore_failure_clears_the_stale_schedule() {
        // Finding 4a: a recoverable RestoreFailed still REPLACES the VM with a fresh
        // boot, so the old timeline's staged faults must not survive attached to it.
        // The live VM is V-time-wired; the factory boots forks WITHOUT V-time, so the
        // restore is a validation-class rejection (RestoreFailed, fresh VM kept).
        let live = exit_boundary_vmm(0x59);
        let factory = Box::new(|| {
            // A fork with NO V-time → restoring a V-time blob is a ContractViolation.
            let mut m = MockBackend::with_exits(vec![]);
            m.set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
            Ok(Vmm::new(
                ExitBoundaryBackend {
                    inner: m,
                    exits_left: 512,
                },
                GuestRam::new(RAM).unwrap(),
            ))
        });
        let mut s = with_test_service(ControlServer::new(live, factory));
        arr_hello(&mut s);
        let base = arr_snap(&mut s);
        // Stage a fault on the current (soon-to-be-replaced) timeline.
        s.handle(&Request::Perturb {
            fault: HostFault(EnvHostEffect::InjectInterrupt { vector: 0x40 }.encode()),
            at: Moment(50),
        })
        .unwrap()
        .unwrap();
        // Branch fails recoverably (RestoreFailed), keeping the fresh (unwired) VM.
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: Reproducer {
                    blob_version: EnvSpec::BLOB_VERSION,
                    bytes: EnvSpec::seeded(7).encode(),
                },
            })
            .unwrap(),
            Err(ControlError::RestoreFailed)
        );
        // The stale fault is gone: recorded reset, schedule cleared. A run applies
        // nothing (and the unwired fork can't even enforce — but the point is the
        // schedule did not carry forward).
        assert_eq!(
            s.recorded_env().effects().len(),
            0,
            "the stale schedule/recorded was cleared on the recoverable failure"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn replay_derives_recorded_seed_from_the_restored_stream() {
        // Finding 4b: `replay` must derive the recorded seed from the RESTORED
        // stream, not the prior session. Drive the recorded seed to a wrong value
        // via a branch, then replay the pristine base and confirm the recorded env —
        // re-applied from base — reproduces the live post-replay+perturb+run hash.
        let mut s = exit_boundary_server();
        arr_hello(&mut s);
        let base = arr_snap(&mut s);
        // Poison the session seed: branch to a distinct seed 0xDEAD (recorded.seed
        // becomes that stream). If replay carried this forward, reproduction breaks.
        s.handle(&Request::Branch {
            snap: base,
            env: Reproducer {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: EnvSpec::seeded(0xDEAD).encode(),
            },
        })
        .unwrap()
        .unwrap();
        // Now replay the pristine base (its stream is seed 0x59, not 0xDEAD).
        assert_eq!(s.handle(&Request::Replay(base)).unwrap(), Ok(Reply::Unit));
        // Perturb + run on the replayed timeline.
        s.handle(&Request::Perturb {
            fault: HostFault(
                EnvHostEffect::XorMemory {
                    gpa: 0x80,
                    bytes: (0x1234_5678_u64).to_le_bytes().to_vec(),
                }
                .encode(),
            ),
            at: Moment(42),
        })
        .unwrap()
        .unwrap();
        assert!(matches!(arr_run(&mut s), Ok(Reply::Stop(_))));
        let h_live = arr_hash(&s);
        let e = s.recorded_env().clone();

        // Re-apply the recorded env from base on a fresh server → must reproduce.
        let mut r = exit_boundary_server();
        arr_hello(&mut r);
        let base_r = arr_snap(&mut r);
        r.handle(&Request::Branch {
            snap: base_r,
            env: Reproducer {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: e.encode(),
            },
        })
        .unwrap()
        .unwrap();
        assert!(matches!(arr_run(&mut r), Ok(Reply::Stop(_))));
        assert_eq!(
            h_live,
            arr_hash(&r),
            "recorded_env() after a replay reproduces the live hash (right stream)"
        );
    }

    #[derive(Clone, Debug)]
    enum VerbOp {
        Perturb(EnvHostEffect, u64),
        Run,
        Branch(u64),
        Replay,
        Snapshot,
    }

    fn arb_verb_op() -> impl Strategy<Value = VerbOp> {
        prop_oneof![
            (
                prop_oneof![
                    (0u64..(RAM as u64 - 8), any::<u64>()).prop_map(|(gpa, m)| {
                        EnvHostEffect::XorMemory {
                            gpa,
                            bytes: m.to_le_bytes().to_vec(),
                        }
                    }),
                    (16u32..=255u32).prop_map(|vector| EnvHostEffect::InjectInterrupt { vector }),
                ],
                1u64..=400,
            )
                .prop_map(|(f, off)| VerbOp::Perturb(f, off)),
            Just(VerbOp::Run),
            (1u64..=8).prop_map(VerbOp::Branch),
            Just(VerbOp::Replay),
            Just(VerbOp::Snapshot),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// The **structural invariant** (PR #51 round 2): after any random sequence
        /// of `perturb`/`run`/`branch`/`replay` on the exit-boundary mock, the
        /// `recorded_env()` — re-applied by branching it from the starting snapshot
        /// on a fresh server and running — reproduces the live `state_hash` after
        /// every completed run. This is the net that covers the whole verb space:
        /// the recorded apply point must equal the actual apply point, or the op
        /// must have failed loudly (rejected ops are simply skipped by the model).
        #[test]
        #[cfg_attr(miri, ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)")]
        fn verb_sequence_recorded_env_reproduces_live_hash(ops in prop::collection::vec(arb_verb_op(), 1..12)) {
            let mut s = exit_boundary_server();
            arr_hello(&mut s);
            let base = arr_snap(&mut s);

            for op in ops {
                match op {
                    VerbOp::Perturb(fault, off) => {
                        let floor = s.vmm().unwrap().effective_vns().unwrap_or(0);
                        // Absolute Moment strictly in the future of the current point.
                        let at = floor.saturating_add(off);
                        // A rejection (dup / past / etc.) is a loud, expected outcome — ignore.
                        let _ = s.handle(&Request::Perturb {
                            fault: HostFault(fault.encode()),
                            at: Moment(at),
                        }).unwrap();
                    }
                    VerbOp::Run => {
                        // Continue-after-error: a loud rejection (e.g. a poisoned
                        // schedule) just skips the invariant check for this op.
                        match arr_run(&mut s) {
                            Ok(Reply::Stop(_)) => {
                                // Invariant: replay recorded_env() from base reproduces live.
                                let e = s.recorded_env().clone();
                                let h_live = arr_hash(&s);
                                let mut r = exit_boundary_server();
                                arr_hello(&mut r);
                                let base_r = arr_snap(&mut r);
                                r.handle(&Request::Branch {
                                    snap: base_r,
                                    env: Reproducer { blob_version: EnvSpec::BLOB_VERSION, bytes: e.encode() },
                                }).unwrap().unwrap();
                                prop_assert!(matches!(arr_run(&mut r), Ok(Reply::Stop(_))));
                                prop_assert_eq!(h_live, arr_hash(&r), "recorded_env() must reproduce the live hash");
                            }
                            Ok(other) => prop_assert!(false, "unexpected run reply: {other:?}"),
                            Err(_) => { /* loud rejection — the model skips this op */ }
                        }
                    }
                    VerbOp::Branch(seed) => {
                        // From `base` only (so the recorded reproducer's origin is fixed).
                        let _ = s.handle(&Request::Branch { snap: base, env: seeded_env_arr(seed) }).unwrap();
                    }
                    VerbOp::Replay => {
                        let _ = s.handle(&Request::Replay(base)).unwrap();
                    }
                    VerbOp::Snapshot => {
                        // Both empty and pending schedules can be captured.
                        // (loud). Either way the session stays consistent; the minted
                        // handle is unused (branches/replays use `base` only).
                        let _ = s.handle(&Request::Snapshot).unwrap();
                    }
                }
            }
        }
    }

    fn seeded_env_arr(seed: u64) -> Reproducer {
        Reproducer {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: EnvSpec::seeded(seed).encode(),
        }
    }

    // -----------------------------------------------------------------------
    // PR #51 round-4 — the idle-HLT-before-fault path: a staged arrival must wake
    // the idle jump (jump to min(timer, arrival)) rather than sail past the Moment.
    // -----------------------------------------------------------------------

    /// A mock-wrapping backend whose guest is a **resumable idle**: `run` always
    /// returns a natural `Hlt` and the vCPU has `RFLAGS.IF` set, so every
    /// arrival is reached through the idle-jump path (`on_hlt` → `idle_action` →
    /// jump to `min(timer, arrival)`). No timer
    /// is armed, so the staged host-fault arrival is the sole wake event; with
    /// `CorruptMemory`-only faults (no IRR raise) the run idles Moment-to-Moment and
    /// terminates cleanly once the schedule drains. `deterministic_tsc` is `true`.
    struct IdleBackend(MockBackend);
    impl Backend for IdleBackend {
        type A = vmm_backend::X86;

        fn set_policy(&mut self, policy: &vmm_backend::X86Policy) -> vmm_backend::Result<()> {
            self.0.set_policy(policy)
        }
        unsafe fn map_memory(
            &mut self,
            gpa: vmm_backend::Gpa,
            host: &mut [u8],
        ) -> vmm_backend::Result<()> {
            // SAFETY: forwards to the inner mock, which only records the region.
            unsafe { self.0.map_memory(gpa, host) }
        }
        fn run(&mut self) -> vmm_backend::Result<Exit<vmm_backend::X86>> {
            Ok(Exit::Common(CommonExit::Idle))
        }
        fn inject(&mut self, e: vmm_backend::Injection) -> vmm_backend::Result<()> {
            self.0.inject(e)
        }
        fn set_pending_irq(&mut self, v: Option<u8>) -> vmm_backend::Result<()> {
            self.0.set_pending_irq(v)
        }
        fn take_accepted_interrupt(&mut self) -> Option<u8> {
            self.0.take_accepted_interrupt()
        }
        fn complete_read(&mut self, v: u64) -> vmm_backend::Result<()> {
            self.0.complete_read(v)
        }
        fn complete_fault(&mut self) -> vmm_backend::Result<()> {
            self.0.complete_fault()
        }
        fn complete_ok(&mut self) -> vmm_backend::Result<()> {
            self.0.complete_ok()
        }
        fn complete_hypercall(&mut self, rax: u64) -> vmm_backend::Result<()> {
            self.0.complete_hypercall(rax)
        }
        fn complete_arch(&mut self, c: vmm_backend::X86Completion) -> vmm_backend::Result<()> {
            self.0.complete_arch(c)
        }
        fn save(&self) -> vmm_backend::Result<vmm_backend::VcpuState> {
            self.0.save()
        }
        fn restore(&mut self, s: &vmm_backend::VcpuState) -> vmm_backend::Result<()> {
            self.0.restore(s)
        }
        fn exit_counts(&self) -> vmm_backend::ExitCounts {
            self.0.exit_counts()
        }
        fn reset_exit_counts(&mut self) {
            self.0.reset_exit_counts()
        }
        fn capabilities(&self) -> vmm_backend::Capabilities<vmm_backend::X86Caps> {
            self.0.capabilities()
        }
    }

    fn idle_vmm(seed: u64) -> Vmm<IdleBackend> {
        let mut m = MockBackend::with_exits(vec![]);
        m.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        // RFLAGS.IF set → a resumable idle HLT (0x2 is the always-1 reserved bit).
        m.set_state(vmm_backend::VcpuState {
            regs: vmm_backend::VcpuRegs {
                rflags: (1 << 9) | 0x2,
                ..Default::default()
            },
            ..Default::default()
        });
        let mut v = Vmm::new(IdleBackend(m), GuestRam::new(RAM).unwrap());
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), seed).unwrap());
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        v.wire_snapshot_hashing();
        v.restore_guest_memory(&enforce_image()).unwrap();
        v
    }

    fn idle_server() -> ControlServer<IdleBackend> {
        ControlServer::new(idle_vmm(0x1D1E), Box::new(|| Ok(idle_vmm(0x1D1E))))
    }

    fn idle_seeded_env(seed: u64) -> Reproducer {
        Reproducer {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: EnvSpec::seeded(seed).encode(),
        }
    }

    #[derive(Clone, Debug)]
    enum IdleOp {
        Perturb(u64, u64), // gpa, moment-offset
        Run,
        Branch(u64),
        Replay,
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// HLT-before-fault reproduction net (PR #51 round-4): the same
        /// recorded-env-reproduces invariant as the exit-boundary proptest, but every
        /// arrival is reached through the **idle jump** (the guest HLTs before each
        /// staged `Moment`; `idle_action` wakes at the arrival). `CorruptMemory`-only
        /// (an idle guest with no timer terminates cleanly once the schedule drains),
        /// verbs `perturb`/`run`/`branch`/`replay`, all from a fixed base snapshot.
        #[test]
        #[cfg_attr(miri, ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)")]
        fn idle_hlt_before_fault_recorded_env_reproduces(ops in prop::collection::vec(
            prop_oneof![
                (0u64..(RAM as u64 - 8), 1u64..=400).prop_map(|(g, off)| IdleOp::Perturb(g, off)),
                Just(IdleOp::Run),
                (1u64..=8).prop_map(IdleOp::Branch),
                Just(IdleOp::Replay),
            ],
            1..12,
        )) {
            let mut s = idle_server();
            arr_hello(&mut s);
            let base = arr_snap(&mut s);
            for op in ops {
                match op {
                    IdleOp::Perturb(gpa, off) => {
                        let floor = s.vmm().unwrap().effective_vns().unwrap_or(0);
                        let at = floor.saturating_add(off);
                        let _ = s.handle(&Request::Perturb {
                            fault: HostFault(EnvHostEffect::XorMemory { gpa, bytes: (0xA5A5_5A5A_u64).to_le_bytes().to_vec() }.encode()),
                            at: Moment(at),
                        }).unwrap();
                    }
                    IdleOp::Run => {
                        match arr_run(&mut s) {
                            Ok(Reply::Stop(_)) => {
                                let e = s.recorded_env().clone();
                                let h_live = arr_hash(&s);
                                let mut r = idle_server();
                                arr_hello(&mut r);
                                let base_r = arr_snap(&mut r);
                                r.handle(&Request::Branch {
                                    snap: base_r,
                                    env: Reproducer { blob_version: EnvSpec::BLOB_VERSION, bytes: e.encode() },
                                }).unwrap().unwrap();
                                prop_assert!(matches!(arr_run(&mut r), Ok(Reply::Stop(_))));
                                prop_assert_eq!(h_live, arr_hash(&r), "idle-path recorded_env() must reproduce the live hash");
                            }
                            Ok(other) => prop_assert!(false, "unexpected run reply: {other:?}"),
                            Err(_) => { /* loud rejection — skip */ }
                        }
                    }
                    IdleOp::Branch(seed) => {
                        let _ = s.handle(&Request::Branch { snap: base, env: idle_seeded_env(seed) }).unwrap();
                    }
                    IdleOp::Replay => {
                        let _ = s.handle(&Request::Replay(base)).unwrap();
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Task 78 — reseed-aware branch: a marker-carrying env re-executes each
    // collapsed hop's entropy reseed at its recorded Moment (exit-boundary
    // discipline), instead of reseeding once at the fold's root. The fold ==
    // hop-by-hop equality (the flipped task-68 pin) is the campaign runner's socket
    // gate; here we pin the server-side mechanics.
    // -----------------------------------------------------------------------

    /// A branch env carrying only reseed markers (no overrides/standing).
    fn marker_env(seed: u64, markers: &[(u64, u64)]) -> Reproducer {
        let mut spec = EnvSpec::seeded(seed);
        for &(m, s) in markers {
            spec.record_reseed(m, s);
        }
        Reproducer {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: spec.encode(),
        }
    }

    #[test]
    fn reseed_arrival_requirement_is_strictly_beyond_the_restore_floor() {
        assert!(!reseed_marker_requires_arrival(99, 100));
        assert!(!reseed_marker_requires_arrival(100, 100));
        assert!(reseed_marker_requires_arrival(101, 100));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn branch_with_a_floor_marker_reseeds_from_the_marker_not_the_env_seed() {
        // Markers are authoritative: a marker at the restore floor IS the branch
        // reseed, and the env's own seed is not consulted for reseeding.
        let mut s = exit_boundary_server();
        arr_hello(&mut s);
        let base = arr_snap(&mut s);
        let mut branch_hash = |env: Reproducer| -> [u8; 32] {
            s.handle(&Request::Branch { snap: base, env })
                .unwrap()
                .unwrap();
            arr_hash(&s)
        };
        let h_marker = branch_hash(marker_env(7, &[(0, 0x1111)]));
        let h_seed = branch_hash(seeded_env(0x1111));
        let h_env_seed = branch_hash(seeded_env(7));
        assert_eq!(
            h_marker, h_seed,
            "a floor marker reseeds exactly like a plain branch on the marker's seed"
        );
        assert_ne!(
            h_marker, h_env_seed,
            "the env's own seed (7) is NOT the reseed value when markers are present"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn mid_run_reseed_marker_applies_at_its_moment_and_recorded_env_reproduces() {
        // A mid-trajectory marker (a collapsed hop's branch reseed) is applied at
        // its exact Moment, the run is deterministic, the marker value reaches the
        // state (control differs), and the emitted recorded env replays to the
        // identical hash (the record → replay closure).
        let run_leg = |mid_seed: u64| -> ([u8; 32], EnvSpec) {
            let mut s = exit_boundary_server();
            arr_hello(&mut s);
            let base = arr_snap(&mut s);
            s.handle(&Request::Branch {
                snap: base,
                env: marker_env(0x1111, &[(0, 0x1111), (300, mid_seed)]),
            })
            .unwrap()
            .unwrap();
            assert!(matches!(arr_run(&mut s), Ok(Reply::Stop(_))));
            (arr_hash(&s), s.recorded_env().clone())
        };
        let (h1, rec1) = run_leg(0x2222);
        let (h1_again, _) = run_leg(0x2222);
        assert_eq!(h1, h1_again, "same markers ⇒ bit-identical terminal hash");
        let (h2, _) = run_leg(0x3333);
        assert_ne!(h2, h1, "the mid-run reseed value reaches the state");

        // The recorded env carries both markers and reproduces from the base.
        assert_eq!(rec1.reseeds().len(), 2, "floor + mid-run markers recorded");
        let mut r = exit_boundary_server();
        arr_hello(&mut r);
        let base_r = arr_snap(&mut r);
        r.handle(&Request::Branch {
            snap: base_r,
            env: Reproducer {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: rec1.encode(),
            },
        })
        .unwrap()
        .unwrap();
        assert!(matches!(arr_run(&mut r), Ok(Reply::Stop(_))));
        assert_eq!(arr_hash(&r), h1, "recorded_env replays the reseed schedule");
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn reseed_marker_behind_the_restore_floor_is_rejected() {
        // Advance the live V-time to 100 (a perturb-armed deadline run lands
        // exactly there), seal, then try to branch with a marker behind it.
        let mut s = exit_boundary_server();
        arr_hello(&mut s);
        s.handle(&Request::Perturb {
            fault: HostFault(
                EnvHostEffect::XorMemory {
                    gpa: 0x40,
                    bytes: (0xFF_u64).to_le_bytes().to_vec(),
                }
                .encode(),
            ),
            at: Moment(100),
        })
        .unwrap()
        .unwrap();
        let stop = s
            .handle(&Request::Run {
                until: StopConditions {
                    deadline: Some(Moment(100)),
                    on: StopMask::NONE,
                },
                resolve: None,
            })
            .unwrap();
        assert!(matches!(stop, Ok(Reply::Stop(_))));
        let base = arr_snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: marker_env(7, &[(50, 0xAB)]),
            })
            .unwrap(),
            Err(ControlError::PerturbPastMoment { at: 50, floor: 100 }),
            "a marker behind the snapshot floor can only apply later than recorded — reject"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn snapshot_with_a_staged_reseed_preserves_the_plan() {
        let mut s = exit_boundary_server();
        arr_hello(&mut s);
        let base = arr_snap(&mut s);
        s.handle(&Request::Branch {
            snap: base,
            env: marker_env(7, &[(0, 7), (300, 9)]),
        })
        .unwrap()
        .unwrap();
        let pending = arr_snap(&mut s);
        assert_eq!(s.handle(&Request::Replay(base)).unwrap(), Ok(Reply::Unit));
        assert!(s.reseed_schedule.is_empty());
        assert_eq!(
            s.handle(&Request::Replay(pending)).unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(s.reseed_schedule.get(&300), Some(&9));
        assert_eq!(s.recorded.reseeds().get(&0), Some(&7));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn terminal_with_a_staged_reseed_poisons_and_rewind_recovers() {
        // A reseed staged beyond the trajectory is the same loud
        // ScheduleUnsatisfiable class as a crossed fault (the task spec's gate).
        let mut s = server(vec![
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Common(CommonExit::Idle),
        ]);
        hello(&mut s);
        let base = snap(&mut s);
        s.handle(&Request::Branch {
            snap: base,
            env: marker_env(7, &[(1_000_000, 9)]),
        })
        .unwrap()
        .unwrap();
        // The fork halts long before Moment 1_000_000: poison, loud.
        assert!(matches!(
            run_all_res(&mut s),
            Err(ControlError::ScheduleUnsatisfiable {
                moment: 1_000_000,
                ..
            })
        ));
        // Latched until a rewind.
        assert!(matches!(
            run_all_res(&mut s),
            Err(ControlError::ScheduleUnsatisfiable { .. })
        ));
        // A replay rewinds and clears the latch.
        assert_eq!(s.handle(&Request::Replay(base)).unwrap(), Ok(Reply::Unit));
        assert!(run_all_res(&mut s).is_ok());
    }

    /// Page `Request::Console` from offset 0 until drained (the discipline the
    /// remote `SocketMachine` uses), returning the full serial capture.
    fn drain_console(server: &mut ControlServer<MockBackend>) -> Vec<u8> {
        let mut buf = Vec::new();
        loop {
            let (total, chunk) = match server
                .handle(&Request::Console {
                    offset: buf.len() as u32,
                })
                .unwrap()
            {
                Ok(Reply::Console { total, chunk }) => (total, chunk),
                other => panic!("console reply: {other:?}"),
            };
            if chunk.is_empty() {
                break;
            }
            buf.extend(chunk);
            if buf.len() as u32 >= total {
                break;
            }
        }
        buf
    }

    /// `page_console` reports the full length as `total`, returns the requested
    /// suffix, and yields an empty chunk at/past the end (the paging terminator)
    /// and for an absent VM.
    #[test]
    fn page_console_paging_math() {
        let serial = b"ORDER_READY\nphase one\nphase two\n";
        let (total, chunk) = page_console(serial, 0);
        assert_eq!(total as usize, serial.len());
        assert_eq!(chunk, serial);
        // A mid-offset returns the suffix, same total.
        let (total2, chunk2) = page_console(serial, 12);
        assert_eq!(total2 as usize, serial.len());
        assert_eq!(chunk2, &serial[12..]);
        // At/past the end ⇒ empty chunk (the client stops paging).
        assert!(page_console(serial, serial.len()).1.is_empty());
        assert!(page_console(serial, serial.len() + 99).1.is_empty());
        // No live VM ⇒ empty capture, total 0.
        assert_eq!(page_console(&[], 0), (0u32, Vec::new()));
    }

    /// A `Console` drain is a **pure read**: an identical (branch, run) off the
    /// same base yields the identical `state_hash` whether or not the console is
    /// drained before hashing. This is the determinism-neutrality task 69 requires
    /// — `RunTrace.records` are host-side observation and never couple into the
    /// hash. (The mock guest may emit an empty console; the invariant is the hash.)
    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn console_drain_is_determinism_neutral() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let base = snap(&mut s);

        // Trajectory A: branch + run, hash without draining.
        s.handle(&Request::Branch {
            snap: base,
            env: seeded_env(7),
        })
        .unwrap()
        .unwrap();
        run_all(&mut s);
        let without_drain = hash(&mut s);

        // Trajectory B: the identical branch + run, but drain the console first.
        s.handle(&Request::Branch {
            snap: base,
            env: seeded_env(7),
        })
        .unwrap()
        .unwrap();
        run_all(&mut s);
        let _drained = drain_console(&mut s);
        let with_drain = hash(&mut s);

        assert_eq!(
            without_drain, with_drain,
            "draining the console must not perturb state_hash (determinism-neutral)"
        );
    }

    // ============== task 127: the seal-bound evidence cut ==================
    //
    // The snapshot reply binds (handle, synchronized seal Moment, included
    // SDK-event count, taint) atomically from one stopped server state (bead
    // hm-bbx.6). Each setup-complete doorbell is followed by the clean guest
    // re-entry boundary its completion protocol requires. The cuts therefore
    // bind both the exact exit-count Moment and the SDK capture-vector prefix.

    /// The fixture server emits a serial byte, rings the `setup_complete`
    /// doorbell, rings it again, emits another serial byte, then goes idle.
    /// Exit-count virtual time makes each successfully serviced exit an exact
    /// boundary, so each lifecycle doorbell can surface its own sealable point.
    /// The factory mirrors the live composition so branch/replay restores validate.
    fn seal_cut_server() -> ControlServer<MockBackend> {
        const REQ_GPA: usize = 0xE000;
        // A `setup_complete` Event frame staged at REQ_GPA; every doorbell
        // ring reads the same staged frame, so both captured events carry the
        // identical id and payload — position is the only discriminator.
        let setup_id: u32 = 4 << 24;
        let mut frame = [0u8; 4096];
        let n = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Event,
            1,
            1,
            &setup_id.to_le_bytes(),
            &mut frame,
        )
        .unwrap();
        let serial = |b: u8| {
            Exit::Arch(X86Exit::Io {
                port: 0x3F8,
                size: 1,
                write: Some(b as u32),
            })
        };
        let ring = Exit::Arch(X86Exit::Io {
            port: 0x0CA1,
            size: 4,
            write: Some(n as u32),
        });
        let mut mb = MockBackend::with_exits(vec![
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }), // pre-stepped clock boundary
            serial(b'a'),                               // console bytes — never in the SDK count
            ring.clone(),                               // SDK event, position 0 → sealable stop #1
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }), // commits ring #1 before its deferred seal
            ring,                                       // SDK event, position 1 → sealable stop #2
            serial(b'b'),                               // console bytes after both seals
            Exit::Common(CommonExit::Idle),
        ]);
        mb.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
        live.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 9).unwrap());
        live.wire_snapshot_hashing();
        let mut ram = vec![0u8; BIG_RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        live.restore_guest_memory(&ram).unwrap();
        live.step().unwrap();
        let factory = Box::new(move || {
            let mut m = MockBackend::with_exits(vec![Exit::Common(CommonExit::Idle)]);
            m.set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
            let mut v = Vmm::new(m, GuestRam::new(BIG_RAM).unwrap());
            v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 0).unwrap());
            v.wire_snapshot_hashing();
            Ok(v)
        });
        with_test_service(ControlServer::new(live, factory))
    }

    /// Page the `SdkEvents` verb from offset 0 until drained (the remote
    /// client's discipline), returning the full capture.
    fn drain_sdk(server: &mut ControlServer<MockBackend>) -> Vec<(u64, u32, Vec<u8>)> {
        let mut all = Vec::new();
        loop {
            let page = match server
                .handle(&Request::SdkEvents {
                    offset: all.len() as u32,
                })
                .unwrap()
            {
                Ok(Reply::SdkEvents(events)) => events,
                other => panic!("SdkEvents reply: {other:?}"),
            };
            if page.is_empty() {
                return all;
            }
            all.extend(page);
        }
    }

    /// The two seals bind the SDK-vector prefix length independently of console
    /// bytes and the clock values at which the events arrived.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated snapshot-seal logic over the mock server VM (task 98 / hm-d8o); pure safe code, logic covered natively"
    )]
    fn sdk_fixture_cuts_by_prefix_length() {
        let mut s = seal_cut_server();
        hello(&mut s);

        // Sealable stop #1: one SDK event emitted so far.
        let stop = run_seeking_snapshot(&mut s);
        assert!(
            matches!(stop, StopReason::SnapshotPoint { .. }),
            "expected the first deferred snapshot point, got {stop:?}"
        );
        let (snap1, at1, n1, t1) = snap_cut(&mut s);
        assert_eq!(at1, 10002, "the seal Moment is the assigned V-time");
        assert_eq!(n1, 1, "the event emitted before the seal is included");
        assert!(!t1);

        // Sealable stop #2: a second event — emitted AFTER the first seal but
        // stamped after the first seal.
        let stop = run_seeking_snapshot(&mut s);
        assert!(
            matches!(stop, StopReason::SnapshotPoint { .. }),
            "expected the second deferred snapshot point, got {stop:?}"
        );
        let (snap2, at2, n2, _) = snap_cut(&mut s);
        assert_ne!(snap1, snap2);
        assert_eq!(
            (at2, n2),
            (20002, 2),
            "later exit-count boundary, strictly larger prefix"
        );

        // The capture has identical ids and payloads; its ordered position is
        // what the prefix counts cut.
        let events = drain_sdk(&mut s);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, 10001);
        assert_eq!(events[1].0, 10002);
        assert_eq!(events[0].1, events[1].1, "identical event ids");
        assert_eq!(events[0].2, events[1].2, "identical payloads");

        // Finish the fixture so its post-seal console byte is observed.
        assert!(matches!(
            run_all_res(&mut s),
            Ok(Reply::Stop(StopReason::Quiescent { .. }))
        ));

        // Console bytes crossed the fixture (before and after the seals)
        // and are structurally outside the count: the serial capture is its
        // own stream, and neither seal's count moved for it.
        let console = drain_console(&mut s);
        assert_eq!(console, b"ab", "the serial capture saw both bytes");
        assert_eq!(
            (n1, n2),
            (1, 2),
            "console bytes never entered the SDK count"
        );
    }

    /// **Branch/replay preserves the captured SDK prefix length.** A verbatim
    /// `replay` of either seal restores the capture to exactly that seal's
    /// prefix, and a re-seal from the restored state stamps the identical cut;
    /// a reseeding `branch` keeps the ancestor's event prefix (the declared
    /// catalog) and stamps the same cut too.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn branch_and_replay_preserve_the_captured_sdk_prefix_length() {
        let mut s = seal_cut_server();
        hello(&mut s);
        assert!(matches!(
            run_seeking_snapshot(&mut s),
            StopReason::SnapshotPoint { .. }
        ));
        let (snap1, at1, n1, _) = snap_cut(&mut s);
        assert_eq!(n1, 1);
        assert!(matches!(
            run_seeking_snapshot(&mut s),
            StopReason::SnapshotPoint { .. }
        ));
        let (snap2, at2, n2, _) = snap_cut(&mut s);
        assert_eq!(n2, 2);

        // Verbatim replay of seal 1: the capture returns to its cut, and a
        // re-seal stamps the identical (Moment, prefix) pair.
        assert_eq!(replay(&mut s, snap1), Ok(Reply::Unit));
        assert_eq!(drain_sdk(&mut s).len(), 1, "replay restored the prefix");
        let (_, at, n, _) = snap_cut(&mut s);
        assert_eq!((at, n), (at1, n1), "the re-sealed cut is identical");

        // Verbatim replay of seal 2: same, at the longer prefix.
        assert_eq!(replay(&mut s, snap2), Ok(Reply::Unit));
        assert_eq!(drain_sdk(&mut s).len(), 2);
        let (_, at, n, _) = snap_cut(&mut s);
        assert_eq!((at, n), (at2, n2));

        // A reseeding branch off seal 1 carries the ancestor's event prefix
        // (the declared catalog) — the captured prefix length is preserved
        // through the fork too.
        assert_eq!(branch(&mut s, snap1, 0xD1CE), Ok(Reply::Unit));
        assert_eq!(drain_sdk(&mut s).len(), 1, "branch kept the event prefix");
        let (_, at, n, _) = snap_cut(&mut s);
        assert_eq!((at, n), (at1, n1), "the branched fork re-stamps the cut");
    }

    /// **The cut is identical across same-seed sessions** — two fresh servers
    /// driven through the identical fixture stamp bit-identical cuts, captures,
    /// and console pages (the determinism contract; this suite runs on macOS
    /// and Linux, so platform identity rides the same assertion in CI).
    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated snapshot-seal logic over the mock server VM (task 98 / hm-d8o); pure safe code, logic covered natively"
    )]
    fn the_cut_is_identical_across_same_seed_sessions() {
        #[allow(clippy::type_complexity)]
        let session = || -> (
            (SnapId, u64, u64, bool),
            (SnapId, u64, u64, bool),
            Vec<(u64, u32, Vec<u8>)>,
            Vec<u8>,
        ) {
            let mut s = seal_cut_server();
            hello(&mut s);
            assert!(matches!(
                run_seeking_snapshot(&mut s),
                StopReason::SnapshotPoint { .. }
            ));
            let c1 = snap_cut(&mut s);
            assert!(matches!(
                run_seeking_snapshot(&mut s),
                StopReason::SnapshotPoint { .. }
            ));
            let c2 = snap_cut(&mut s);
            let events = drain_sdk(&mut s);
            let console = drain_console(&mut s);
            (c1, c2, events, console)
        };
        assert_eq!(session(), session(), "same seed ⇒ bit-identical cuts");
    }

    // ===================== task 81: the taint guard =======================

    /// Take a snapshot, returning its handle and the taint bit its reply carries
    /// (task 127: the one seal-bound `Reply::Snapshot` carries the flag both ways).
    fn snap_tainted(server: &mut ControlServer<MockBackend>) -> (SnapId, bool) {
        match server.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::Snapshot { id, tainted, .. }) => (id, tainted),
            other => panic!("snapshot reply: {other:?}"),
        }
    }

    /// Improvise: `exec` with an already-expired deadline (`Moment(0)`), so it taints
    /// the timeline and returns immediately without stepping the mock guest.
    fn exec(server: &mut ControlServer<MockBackend>, cmd: &str) -> (Vec<u8>, bool) {
        match server
            .handle(&Request::Exec {
                cmd: cmd.to_string(),
                deadline: Moment(0),
            })
            .unwrap()
        {
            Ok(Reply::ExecResult { output, ok }) => (output, ok),
            other => panic!("exec reply: {other:?}"),
        }
    }

    /// The reproducer mint result: `Ok(Reproducer)` or the loud `Tainted`.
    fn recorded_env_res(server: &mut ControlServer<MockBackend>) -> Result<Reply, ControlError> {
        server.handle(&Request::RecordedEnv).unwrap()
    }

    fn branch(
        server: &mut ControlServer<MockBackend>,
        snap: SnapId,
        seed: u64,
    ) -> Result<Reply, ControlError> {
        server
            .handle(&Request::Branch {
                snap,
                env: seeded_env(seed),
            })
            .unwrap()
    }

    fn replay(
        server: &mut ControlServer<MockBackend>,
        snap: SnapId,
    ) -> Result<Reply, ControlError> {
        server.handle(&Request::Replay(snap)).unwrap()
    }

    /// `exec` taints the live timeline: the reproducer mints cleanly before, is a
    /// loud `Tainted` after, and the `ExecResult` (crude, deadline-0) is unsuccessful
    /// with empty capture — but the guard fired regardless of the run's outcome.
    #[test]
    fn exec_taints_and_recorded_env_then_fails_loud() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        // Untainted: the reproducer mints.
        assert!(matches!(recorded_env_res(&mut s), Ok(Reply::Recorded(_))));
        // Improvise.
        let (output, ok) = exec(&mut s, "ps aux");
        assert!(!ok, "deadline-0 exec on a non-shell mock does not complete");
        assert!(output.is_empty());
        // Tainted: the mint is refused, loud.
        assert_eq!(recorded_env_res(&mut s), Err(ControlError::Tainted));
    }

    #[test]
    fn legacy_exec_preserves_accepted_inputs_through_the_common_scheduler() {
        let mut server = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut server);
        let effect = environment::channel::Effect::WriteMemory {
            gpa: 64,
            bytes: vec![0x7a],
        };
        assert_eq!(
            server
                .handle(&Request::Perturb {
                    fault: HostFault(effect.encode()),
                    at: Moment(500)
                })
                .unwrap(),
            Ok(Reply::Unit)
        );
        assert!(matches!(
            server
                .handle(&Request::Exec {
                    cmd: "true".into(),
                    deadline: Moment(500)
                })
                .unwrap(),
            Ok(Reply::ExecResult { ok: false, .. })
        ));
        assert_eq!(server.vmm.as_ref().unwrap().guest_memory()[64], 0x7a);
        assert!(server.schedule.is_empty());
        assert_eq!(server.recorded.effects().get(&500), Some(&effect));
        assert!(server.retained_exec.as_ref().unwrap().is_pending());
        assert_eq!(server.vmm.as_ref().unwrap().effective_vns(), Some(500));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "portable restore uses snapshot-store mmap; pure command codec is tested separately"
    )]
    fn pending_exec_cold_restore_is_exact_and_never_reinjects() {
        let mut source = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut source);
        let at = source.vmm.as_ref().unwrap().effective_vns();
        let start = source
            .handle(&Request::ExecStart {
                cmd: "printf once".into(),
            })
            .unwrap()
            .unwrap();
        assert!(matches!(
            start,
            Reply::ExecState(Some(control_proto::ExecStatus {
                completion: control_proto::ExecCompletion::Pending,
                ..
            }))
        ));
        assert_eq!(source.vmm.as_ref().unwrap().effective_vns(), at);
        let injected = crate::exec::ExecSession::new("printf once", 0)
            .input()
            .to_vec();
        // Model a partially delivered command: only remaining RX bytes belong
        // in the snapshot, and none may be lost or injected a second time.
        for byte in &injected[..4] {
            assert_eq!(
                source.vmm.as_mut().unwrap().devices.uart.read_in(0x3f8),
                Some(*byte)
            );
        }
        let before = hash(&mut source);
        assert_eq!(source.handle(&Request::ExecStatus).unwrap().unwrap(), start);
        assert_eq!(hash(&mut source), before, "status is a pure observation");
        assert_eq!(
            source
                .handle(&Request::ExecStart {
                    cmd: "second".into()
                })
                .unwrap(),
            Err(ControlError::ExecPending { id: 0 })
        );
        assert_eq!(
            hash(&mut source),
            before,
            "rejected command cannot change input or nonce"
        );
        let (cut, _) = snap_tainted(&mut source);
        assert_eq!(
            hash(&mut source),
            before,
            "saving cannot run or alter parser state"
        );
        let mut artifact = Vec::new();
        source.export_portable_snapshot(cut, &mut artifact).unwrap();
        let mut cold = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut cold);
        let mut false_clean = artifact.clone();
        // HMSNAP01: eight-byte magic, u16 version, then the u16 flags.
        false_clean[10] &= !2;
        // Re-authenticate the edited container so this exercises semantic
        // validation rather than merely the transport checksum.
        let body_end = false_clean.len() - 32;
        let digest = <sha2::Sha256 as sha2::Digest>::digest(&false_clean[..body_end]);
        false_clean[body_end..].copy_from_slice(&digest);
        let before_store = cold.snapshot_store_stats();
        let before_handle = cold.next_snap;
        assert!(matches!(
            cold.import_portable_snapshot(false_clean.as_slice()),
            Err(PortableSnapshotError::Malformed(
                "retained command requires a tainted snapshot"
            ))
        ));
        assert_eq!(cold.snapshot_store_stats(), before_store);
        assert_eq!(cold.next_snap, before_handle);
        let imported = cold.import_portable_snapshot(artifact.as_slice()).unwrap();
        assert_eq!(replay(&mut cold, imported.id), Ok(Reply::Unit));
        assert_eq!(cold.handle(&Request::ExecStatus).unwrap().unwrap(), start);
        assert_eq!(
            hash(&mut cold),
            before,
            "restore preserves queued bytes without reinjection"
        );
        assert_eq!(cold.exec_nonce, 1);
        assert_eq!(hash(&mut source), before, "source remains unchanged");
        for byte in &injected[4..] {
            assert_eq!(
                cold.vmm.as_mut().unwrap().devices.uart.read_in(0x3f8),
                Some(*byte),
                "restore must preserve the remaining RX FIFO"
            );
        }
        assert!(!cold.vmm.as_ref().unwrap().devices.uart.rx_has_input());
    }

    /// A snapshot taken from a tainted timeline reports `tainted: true`; one taken
    /// before any `exec` reports untainted. Both ride the one seal-bound reply
    /// (task 127), so the tainted seal binds the same evidence cut fields the
    /// untainted one does — here the deadline-0 `exec` never advanced the guest,
    /// so the two cuts are identical.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated snapshot-seal/hash logic over the mock server VM (each seal state-hashes + page-hashes the image, ~2 s/KiB under Miri); pure safe code — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the seal/hash family keeps Miri-run siblings incl. snapshot_mints_fresh_handles_and_drop_releases_them and the deferred-snapshot-boundary tests (task 98 / hm-d8o)"
    )]
    fn snapshot_reply_carries_the_taint() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let (_clean, clean_at, clean_sdk, clean_taint) = snap_cut(&mut s);
        assert!(!clean_taint, "a pre-exec snapshot is untainted");
        exec(&mut s, "ls /");
        let (_dirty, dirty_at, dirty_sdk, dirty_taint) = snap_cut(&mut s);
        assert!(dirty_taint, "a post-exec snapshot is tainted");
        // Task 127: taint changes only the taint bit — the tainted seal still
        // binds its exact cut, identical here because the guest never advanced.
        assert_eq!(
            (dirty_at, dirty_sdk),
            (clean_at, clean_sdk),
            "the tainted reply carries the same server-stamped cut"
        );
    }

    /// A `branch` **and** a `replay` from a tainted snapshot both yield a tainted
    /// timeline (taint follows ancestry through either restore verb), and the mint
    /// stays refused on the restored fork.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn branch_and_replay_from_a_tainted_snapshot_stay_tainted() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        exec(&mut s, "ls /");
        let (tainted_snap, t) = snap_tainted(&mut s);
        assert!(t);
        // Branch: tainted → mint refused, and a snapshot here is tainted.
        assert_eq!(branch(&mut s, tainted_snap, 0x1111), Ok(Reply::Unit));
        assert_eq!(recorded_env_res(&mut s), Err(ControlError::Tainted));
        assert!(
            snap_tainted(&mut s).1,
            "branch-of-tainted snapshots tainted"
        );
        // Replay: same.
        assert_eq!(replay(&mut s, tainted_snap), Ok(Reply::Unit));
        assert_eq!(recorded_env_res(&mut s), Err(ControlError::Tainted));
    }

    /// **Taint never crosses, and a rewind to an untainted ancestor recovers.** A
    /// clean snapshot taken *before* any `exec` restores to an untainted timeline —
    /// even from a currently-tainted live state — so the mint works again. This is
    /// the "untainted state is only reachable from an untainted ancestor" rule: the
    /// clean ancestor is one.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)"
    )]
    fn rewind_to_an_untainted_ancestor_clears_the_live_taint() {
        let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
        hello(&mut s);
        let (clean_snap, _) = snap_tainted(&mut s); // taken before any exec
        exec(&mut s, "rm -rf /"); // sacrifice the live timeline
        assert_eq!(recorded_env_res(&mut s), Err(ControlError::Tainted));
        // Rewind to the clean ancestor via replay: untainted again.
        assert_eq!(replay(&mut s, clean_snap), Ok(Reply::Unit));
        assert!(
            matches!(recorded_env_res(&mut s), Ok(Reply::Recorded(_))),
            "an untainted ancestor restores an untainted, recordable timeline"
        );
        // And a branch off the clean snapshot is likewise untainted.
        assert_eq!(branch(&mut s, clean_snap, 0x2222), Ok(Reply::Unit));
        assert!(matches!(recorded_env_res(&mut s), Ok(Reply::Recorded(_))));
        assert!(!snap_tainted(&mut s).1);
    }

    /// One op in the arbitrary-DAG proptest.
    #[derive(Clone, Debug)]
    enum TaintOp {
        Snapshot,
        Exec,
        /// Branch/replay reference an existing snapshot by index (mod the count).
        Branch(usize),
        Replay(usize),
    }

    fn arb_taint_ops() -> impl Strategy<Value = Vec<TaintOp>> {
        let op = prop_oneof![
            Just(TaintOp::Snapshot),
            Just(TaintOp::Exec),
            any::<usize>().prop_map(TaintOp::Branch),
            any::<usize>().prop_map(TaintOp::Replay),
        ];
        prop::collection::vec(op, 0..40)
    }

    proptest! {
        // Gate 1: over an arbitrary DAG of snapshot/branch/replay/exec, taint
        // propagates EXACTLY along ancestry — never across, never cleared — a
        // `recorded_env` on a tainted timeline ALWAYS errors, and an untainted
        // lineage is NEVER blocked. The real `ControlServer<MockBackend>` is driven
        // through `handle()` and checked against an independent oracle at every step.
        #![proptest_config(ProptestConfig::with_cases(300))]
        #[test]
        #[cfg_attr(miri, ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the restore-side map_memory unsafe is exercised under Miri by bringup::tests::compose_restore_target_map_memory_over_an_anonymous_mapping (task 98)")]
        fn taint_propagates_exactly_along_ancestry(ops in arb_taint_ops()) {
            let mut s = server(vec![Exit::Common(CommonExit::Idle)]);
            hello(&mut s);
            // Oracle: taint of each snapshot (by creation order; SnapId is monotonic
            // from 1), and the live timeline's taint. Genesis is untainted.
            let mut snap_taint: Vec<bool> = Vec::new();
            let mut snap_ids: Vec<SnapId> = Vec::new();
            let mut current = false;

            for op in ops {
                match op {
                    TaintOp::Snapshot => {
                        let (id, reported) = snap_tainted(&mut s);
                        // The reply's taint bit must match the live timeline exactly.
                        prop_assert_eq!(reported, current, "snapshot taint mismatch");
                        snap_ids.push(id);
                        snap_taint.push(current);
                    }
                    TaintOp::Exec => {
                        // Every accepted command in this fixture stops at deadline 0,
                        // so a tainted ancestor also has a still-pending command.
                        if current {
                            let before = hash(&mut s);
                            prop_assert_eq!(s.handle(&Request::ExecStart { cmd: "echo hi".into() }).unwrap(), Err(ControlError::ExecPending { id: 0 }));
                            prop_assert_eq!(hash(&mut s), before);
                        } else {
                            exec(&mut s, "echo hi");
                            current = true;
                        }
                    }
                    TaintOp::Branch(i) => {
                        if snap_ids.is_empty() {
                            continue;
                        }
                        let k = i % snap_ids.len();
                        prop_assert_eq!(branch(&mut s, snap_ids[k], 0x99), Ok(Reply::Unit));
                        // A branch inherits the snapshot's taint (never across).
                        current = snap_taint[k];
                    }
                    TaintOp::Replay(i) => {
                        if snap_ids.is_empty() {
                            continue;
                        }
                        let k = i % snap_ids.len();
                        prop_assert_eq!(replay(&mut s, snap_ids[k]), Ok(Reply::Unit));
                        current = snap_taint[k];
                    }
                }
                // The load-bearing invariant, checked after EVERY op: the reproducer
                // mint errors `Tainted` iff the live timeline is tainted — untainted
                // lineage is never blocked, tainted lineage always is.
                match recorded_env_res(&mut s) {
                    Ok(Reply::Recorded(_)) => prop_assert!(!current, "clean mint on a tainted timeline"),
                    Err(ControlError::Tainted) => prop_assert!(current, "Tainted on an untainted timeline"),
                    other => prop_assert!(false, "unexpected recorded_env reply: {:?}", other),
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Task 110: the pvclock registration rides snapshot/branch like the SDK
    // channel — captured at seal, re-established on the restored fork.
    // -----------------------------------------------------------------------

    /// The registered clock-page GPA survives a `snapshot` → `branch`: the
    /// fork's VM keeps stamping the page the restored guest already published
    /// (without the carry, its first busy-wait would hang on a frozen clock).
    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the pvclock carry logic itself is pure and covered natively"
    )]
    fn branch_carries_the_pvclock_registration() {
        const REQ_GPA: usize = 0xE000;
        const PV_GPA: u64 = 0x4000;
        let compose = |exits: Vec<Exit<X86>>, _work: u64| {
            let mut mb = MockBackend::with_exits(exits);
            mb.set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
            let mut v = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
            v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 9).unwrap());
            v.wire_snapshot_hashing();
            v.enable_pvclock();
            v
        };
        // Live VM: sync at an RDTSC, register the clock page, then perform the
        // handshake (a second RDTSC) — which is what stamps the page (r8 ruling).
        let mut live = compose(
            vec![
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                Exit::Common(CommonExit::Idle),
            ],
            100,
        );
        let mut frame = [0_u8; 64];
        let n = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Pvclock,
            1,
            1,
            &PV_GPA.to_le_bytes(),
            &mut frame,
        )
        .unwrap();
        let mut ram = vec![0u8; BIG_RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        live.restore_guest_memory(&ram).unwrap();
        assert_eq!(live.step().unwrap(), crate::vmm::Step::Continued); // RDTSC → synchronized
        live.service_doorbell(n as u32).unwrap();
        assert_eq!(live.pvclock_registration(), Some(PV_GPA));
        // The handshake: the next RDTSC intercept arms the channel and lays down
        // the first (canonical) stamp, so the sealed image carries an oracle-true
        // page (r8 — registration alone no longer stamps).
        assert_eq!(live.step().unwrap(), crate::vmm::Step::Continued);

        let factory = Box::new(move || {
            Ok(compose(
                vec![
                    Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                    Exit::Common(CommonExit::Idle),
                ],
                9_999,
            ))
        });
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);
        let base = snap(&mut s);
        match s
            .handle(&Request::Branch {
                snap: base,
                env: seeded_env(7),
            })
            .unwrap()
        {
            Ok(Reply::Unit) => {}
            other => panic!("branch failed: {other:?}"),
        }
        // The fork's VM carries the registration and a canonical, oracle-true
        // page (the sealed image restored it; the carry re-armed the stamping).
        let vmm = s.vmm().unwrap();
        assert_eq!(vmm.pvclock_registration(), Some(PV_GPA));
        vmm.pvclock_check_oracle()
            .expect("restored page matches the restored clock");
    }

    /// A factory that does NOT offer the pvclock cannot restore a snapshot
    /// whose VM had a registered page: the blob's v4 pvclock record fails the
    /// validate phase before any mutation — the recoverable `RestoreFailed`,
    /// never a silent frozen clock.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "reaches snapshot restore (materialize → snapshot-store's tempfile+mmap), which Miri cannot execute; the mismatch rejection is pure and covered natively via Vmm::pvclock_restore"
    )]
    fn branch_into_an_unoffered_composition_fails_loud() {
        const REQ_GPA: usize = 0xE000;
        const PV_GPA: u64 = 0x4000;
        let mut mb = MockBackend::with_exits(vec![
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Common(CommonExit::Idle),
        ]);
        mb.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
        live.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 9).unwrap());
        live.wire_snapshot_hashing();
        live.enable_pvclock();
        let mut frame = [0_u8; 64];
        let n = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Pvclock,
            1,
            1,
            &PV_GPA.to_le_bytes(),
            &mut frame,
        )
        .unwrap();
        let mut ram = vec![0u8; BIG_RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        live.restore_guest_memory(&ram).unwrap();
        live.service_doorbell(n as u32).unwrap();
        // Complete the registration HANDSHAKE (the guest's post-doorbell RDTSC) so
        // the page is ARMED before the seal: a pending (un-armed) registration is
        // unsealable (r13 P1), and this branch test only needs an armed snapshot
        // carrying a pvclock record to drive the cross-composition validation.
        assert_eq!(live.step().unwrap(), crate::vmm::Step::Continued);

        // The factory forgets enable_pvclock — a composition mismatch.
        let factory = Box::new(|| {
            let mut mb = MockBackend::with_exits(vec![Exit::Common(CommonExit::Idle)]);
            mb.set_policy(&X86Policy {
                cpuid: vmm_backend::CpuidModel::default(),
                msr_filter: vmm_backend::MsrFilter::default(),
            })
            .unwrap();
            let mut v = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
            v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 9).unwrap());
            v.wire_snapshot_hashing();
            Ok(v)
        });
        let mut s = with_test_service(ControlServer::new(live, factory));
        hello(&mut s);
        let base = snap(&mut s);
        // The blob's v4 pvclock record fails the fresh VM's validate phase
        // BEFORE any mutation, so the branch answers the recoverable
        // `RestoreFailed` (the LAPIC-wiring-mismatch class) and the session
        // stays alive on a fresh boot — loud, never a silently frozen clock.
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: seeded_env(7),
            })
            .unwrap(),
            Err(ControlError::RestoreFailed),
            "a pvclock composition mismatch rejects the restore loudly"
        );
        // The session survived (fresh boot) — still hashable.
        let _ = hash(&mut s);
    }
}
