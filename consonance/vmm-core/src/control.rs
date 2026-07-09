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
//! - **`hello(caps)`** → the server's [`Caps`]: protocol 1, `Environment` blob
//!   version exactly [`EnvSpec::BLOB_VERSION`], **empty/zero-width coverage
//!   geometry** (no coverage producer exists yet) and — task 73 —
//!   `GUEST_HAS_SDK` (the doorbell is serviced). Any other verb before `hello`
//!   answers [`ControlError::Unsupported`].
//! - **`snapshot`** → seal the current point (memory + `vm_state`) into the
//!   engine and mint a pool-wide [`SnapId`]. Task 41's non-quiescent capture is
//!   merged, so mid-workload points are sealable; the remaining fail-closed
//!   boundaries (an RNG mid-exit completion, a non-V-time-synchronized point)
//!   answer [`ControlError::NotQuiescent`] — the caller runs a little further
//!   and retries.
//! - **`drop(snap)`** → release + GC via the store (corpus GC).
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
//!   env carrying a **guest** override, a **standing** fault, or a **non-`none`
//!   fault policy** still answers [`ControlError::Unsupported`] (they need the
//!   task-61 guest-plane / decide-seam enforcement loops), rather than silently
//!   running without them.
//! - **`replay(snap)`** → restore verbatim into a fresh VM, **no reseed** — the
//!   repro / determinism-gate path.
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
//! - **`perturb(fault, at)`** → **stage a [`HostFault`](environment::HostFault)
//!   at a [`Moment`](environment::Moment)** (task 59): the fault blob is decoded
//!   and validated (an out-of-range [`CorruptMemory`](environment::HostFault::CorruptMemory)
//!   gpa is a loud [`ControlError::PerturbOutOfRange`], a malformed blob a
//!   [`ControlError::MalformedEnvironment`], the out-of-scope `SkewTime`/
//!   `SetClockRate` a [`ControlError::Unsupported`]), then queued. [`run`](ControlServer::run)
//!   applies it *between instructions* at its `Moment` — a guest-RAM XOR for
//!   `CorruptMemory`, an IRR raise through the LAPIC arbitration for
//!   `InjectInterrupt` — and stamps it into the recorded env
//!   ([`recorded_env`](ControlServer::recorded_env)), so the emitted reproducer
//!   replays to the identical `state_hash`.
//!
//! ## The fresh-VM restore discipline
//!
//! `branch`/`replay` never restore in place: a VM that just serviced an exit
//! usually has a **staged completion** in its backend (`kvm_run`), which
//! [`Vmm::restore_vm_state`] correctly refuses to restore across, and the box
//! substrate allows only one open `perf_event` work counter at a time. So every
//! restore drops the live VM first, then boots a fresh one via the
//! [`VmmFactory`] and restores into that — exactly the pattern the task-40/41
//! box demos proved (`tests/live_branching_demo.rs`), and within budget here
//! because task 58 declares snapshot performance a non-goal (D5: one full-image
//! branch per seed is acceptable).
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

use control_proto::{
    Caps, ControlError, CoverageGeometry, CrashInfo, CrashKind, Environment, EventRef, HashScope,
    Moment, READ_CAP, RegsView, Reply, Request, SnapId, StopReason, VTime, decode_request,
    encode_reply,
};
use environment::{EnvError, EnvSpec, FaultPolicy};
use snapshot_store::SnapshotId;
use vmm_backend::Backend;

use crate::exec::ExecSession;
use crate::snapshot::{SnapshotEngine, SnapshotError};
use crate::vmm::{NetSnapshot, SdkSnapshot, SdkStop, Step, TerminalReason, Vmm, VmmError};

/// Boots a fresh, equivalently-composed VM — the restore target for every
/// `branch`/`replay` (see the module doc's fresh-VM discipline). On the box
/// this re-runs the composition root (`boot_linux_selected`): same RAM size,
/// same wiring (V-time + xAPIC + legacy), same contract — the boot-loaded guest
/// image is immediately overwritten by the restore, so the factory's seed is
/// irrelevant. In the portable gates it builds a fresh scripted
/// `Vmm<MockBackend>`. **Must be called only after the previous VM is dropped**
/// (the box allows one open `perf_event` work counter at a time); the server
/// guarantees that ordering.
pub type VmmFactory<B> = Box<dyn FnMut() -> Result<Vmm<B>, VmmError>>;

/// Boots a fresh restore target **around a materialized snapshot mapping** —
/// the task-95 M2.2 remap-restore factory (see
/// [`crate::bringup::compose_restore_target`]): the mapping's buffer becomes the
/// guest RAM the memslots register, so the restore performs **no** full-image
/// memcpy and untouched pages fault lazily. Must compose its VMs exactly like
/// the session's [`VmmFactory`] (same RAM size, wiring, contract) minus the
/// boot-image load; the server then restores only the non-memory half
/// ([`Vmm::restore_vm_state`]). Same drop-the-old-VM-first discipline as
/// [`VmmFactory`].
pub type RemapVmmFactory<B> = Box<dyn FnMut(snapshot_store::Mapping) -> Result<Vmm<B>, VmmError>>;

/// How `branch`/`replay` restore guest memory (task 95 M2.2) — the A/B knob of
/// the restore determinism gate, and the fallback if a box gate fails.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RestoreMode {
    /// The mapping becomes the memslot backing (no memcpy; lazy faults). The
    /// default — used whenever a [`RemapVmmFactory`] is installed; without one
    /// the server can only memcpy and does so regardless of this setting.
    Remap,
    /// Materialize, boot a fresh owned-RAM VM, memcpy the image in — the
    /// pre-task-95 path, byte-for-byte.
    Memcpy,
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
    #[error("substrate failure")]
    Vmm(#[from] VmmError),
    /// The snapshot store / codec hit an invariant failure (not a caller error
    /// — those answer `ControlError` replies).
    #[error("snapshot store failure")]
    Snapshot(#[from] SnapshotError),
    /// A verb arrived after a previous fatal error already tore the VM down
    /// (the server is poisoned; a prior [`ServeError`] was returned).
    #[error("server poisoned by a prior fatal error")]
    Poisoned,
}

/// The [`Caps`] this server negotiates: the current negotiated application
/// protocol ([`control_proto::APP_PROTOCOL_VERSION`]), `Environment` blobs exactly
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
        // `setup_complete` surfaces `StopReason::SnapshotPoint`, and buggify
        // decisions are answered — so `GUEST_HAS_SDK` is advertised.
        flags: control_proto::CapFlags::GUEST_HAS_SDK,
    }
}

/// The control-transport server: one live [`Vmm`], a [`SnapshotEngine`] holding
/// the snapshot pool, the [`VmmFactory`] that boots restore targets, and the
/// wire-handle table. One server = one session = one VM; see the module doc.
pub struct ControlServer<B: Backend> {
    /// The live VM. `None` only after a fatal error already tore it down (or
    /// transiently inside a `branch`/`replay`, where the old VM must be dropped
    /// before the factory boots its replacement).
    vmm: Option<Vmm<B>>,
    factory: VmmFactory<B>,
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
    /// captures via `snapshot_derive` over the harvested dirty set; on **any**
    /// doubt it falls back to `snapshot_base` (the safety rule: the dirty set is
    /// a cost hint, never a correctness input — a seal never fails because the
    /// optimization was unavailable).
    derive_parent: Option<SnapshotId>,
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
    schedule: BTreeMap<environment::Moment, environment::HostFault>,
    /// The **staged reseed schedule** (task 78): the branch env's reseed markers
    /// strictly beyond the restore floor, ordered. A marker-carrying env's
    /// collapsed-hop reseeds are re-executed at their recorded `Moment`s by
    /// [`run`](ControlServer::run) (the exact-arrival discipline of the task-59
    /// plane) — the ruled fix for the sequential-entropy splice: a compose-folded
    /// env replays each hop's reseed at its position instead of reseeding once at
    /// the fold's root. A reseed staged beyond the trajectory is the same loud
    /// [`ControlError::ScheduleUnsatisfiable`] class as a crossed fault. At a
    /// `Moment` shared with a staged host fault the reseed applies **first**
    /// (fixed order, so the apply sequence is deterministic; the recorded
    /// tables are disjoint, so replay preserves it).
    reseed_schedule: BTreeMap<environment::Moment, u64>,
    /// The **active recorded reproducer** (task 59 requirement 3): every applied
    /// host fault is stamped here via task-45's [`EnvSpec::perturb`], so the env
    /// [`recorded_env`](ControlServer::recorded_env) returns replays to the
    /// identical `state_hash` (the record → replay closure). With one fault per
    /// `Moment` (above) the stamping is exact — no fault is ever lost. Its seed is
    /// set by the most recent [`Request::Branch`] (default `0` before any branch);
    /// reset on each restore so a new future records fresh.
    recorded: EnvSpec,
    /// **Poison latch** for an unsatisfiable schedule (PR #51 round-3). Set to the
    /// `(Moment, vtime)` of a fault a [`run`](ControlServer::run) executed *past*
    /// without applying (a crossed `Moment`). While latched, [`run`](ControlServer::run),
    /// [`perturb`](ControlServer::perturb), and [`snapshot`](ControlServer::snapshot)
    /// keep failing loud with [`ControlError::ScheduleUnsatisfiable`] — the crossed
    /// fault can never be applied at its recorded count, so the session must
    /// **rewind** (`branch`/`replay`, which clears the latch via
    /// [`reset_schedule_to_fresh_vm`](ControlServer::reset_schedule_to_fresh_vm))
    /// before it can continue. Without the latch a client that ignored the error and
    /// re-sent `run` would get the crossed fault applied from the past — the exact
    /// non-reproducing case the error exists to prevent.
    schedule_poisoned: Option<(environment::Moment, u64)>,
    /// The task-73 **SDK channel snapshots**, keyed by wire [`SnapId`]: the
    /// replay-relevant SDK state (seeded stream position + emitted event log)
    /// captured when a snapshot is sealed, so a `branch`/`replay` from a mid-run
    /// SDK snapshot reproduces (its seeded streams continue from the right
    /// position) and keeps the declared catalog the never-fired report needs.
    /// Removed with its snapshot on `drop`; ephemeral pool state, like the
    /// snapshot handles themselves.
    sdk_snaps: BTreeMap<u64, SdkSnap>,
    /// Per-snapshot `Net` channel state (task 61), same discipline as
    /// [`sdk_snaps`](Self::sdk_snaps): a `branch`/`replay` from a mid-run snapshot
    /// restores the flow-policy stream position (and, for a replay, the decision
    /// prefix) so a fork's `net_decide` answers do not diverge from the sequential
    /// run. Ephemeral, removed with its snapshot on `drop`.
    net_snaps: BTreeMap<u64, NetSnap>,
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
    /// A monotonically-increasing counter salting each [`Request::Exec`]'s
    /// completion-sentinel marker ([`ExecSession`]), so two `exec`s on one session
    /// cannot alias their sentinels. Not wall-clock / RNG (conventions rule 4);
    /// `exec` is off the record, so this never needs to be reproducible — only
    /// unique-enough within a session.
    exec_nonce: u64,
}

/// The per-snapshot `Net` state the control server retains (task 61): the
/// VM-level channel snapshot (seeded flow-policy stream position + decision log)
/// **and** the [`FaultPolicy`] active when sealed — captured for the same reason
/// as [`SdkSnap`]: a **replay** resets the recorded reproducer to `none()`, so the
/// seal-time policy must be restored before materializing the Net env, else the
/// restored stream would draw all-`Nominal`.
#[derive(Clone)]
struct NetSnap {
    channel: NetSnapshot,
    policy: FaultPolicy,
}

/// The per-snapshot SDK state the control server retains (task 73): the VM-level
/// channel snapshot (seeded stream position + event log) **and** the
/// [`FaultPolicy`] active when the snapshot was sealed. The policy is captured
/// because [`reset_schedule_to_fresh_vm`](ControlServer::reset_schedule_to_fresh_vm)
/// resets the recorded reproducer to `none()` on every restore — so a **replay**
/// must restore this policy before materializing the SDK env, else the restored
/// stream position would draw all-`Nominal` (the buggify biasing lost).
#[derive(Clone)]
struct SdkSnap {
    channel: SdkSnapshot,
    policy: FaultPolicy,
}

impl<B: Backend> ControlServer<B> {
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
        let recorded = EnvSpec::Seeded {
            seed,
            policy: FaultPolicy::none(),
        };
        // Task 73: wire the SDK channel on the **live VM too** (not only restore
        // targets), so an SDK guest that rings the hypercall doorbell BEFORE its
        // first `branch`/`replay` is serviced — we advertise `GUEST_HAS_SDK`
        // unconditionally, so the capability must be honored from construction.
        // Inert (and unhashed) for a non-SDK guest.
        vmm.enable_sdk(recorded.materialize(), recorded.policy());
        // Task 61: wire the Net channel on the live VM too, so a guest flow agent
        // that rings `net_decide` before its first branch/replay is serviced. Inert
        // (and unhashed) for a guest that never asks about a flow.
        vmm.enable_net();
        ControlServer {
            vmm: Some(vmm),
            factory,
            remap_factory: None,
            restore_mode: RestoreMode::Remap,
            engine,
            // A fresh boot is no snapshot's continuation: the first seal is a base.
            derive_parent: None,
            snaps: BTreeMap::new(),
            next_snap: 1,
            hello_done: false,
            schedule: BTreeMap::new(),
            reseed_schedule: BTreeMap::new(),
            recorded: EnvSpec::Seeded {
                seed,
                policy: FaultPolicy::none(),
            },
            schedule_poisoned: None,
            sdk_snaps: BTreeMap::new(),
            net_snaps: BTreeMap::new(),
            // A fresh session's live timeline is untainted; no snapshots exist yet.
            timeline_tainted: false,
            tainted_snaps: BTreeSet::new(),
            exec_nonce: 0,
        }
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

    /// Install the remap-restore factory (task 95 M2.2). With one installed —
    /// and [`RestoreMode::Remap`] active, which is the default — every
    /// `branch`/`replay` builds the fresh VM **around** the materialized
    /// mapping instead of memcpying the image into a fresh allocation. The
    /// factory must mirror the session's [`VmmFactory`] composition (RAM size,
    /// wiring, contract) minus the boot-image load.
    pub fn set_remap_factory(&mut self, factory: RemapVmmFactory<B>) {
        self.remap_factory = Some(factory);
    }

    /// Flip the restore-mode A/B knob (task 95 M2.2's determinism gate arm; no
    /// effect unless a [`RemapVmmFactory`] is installed).
    pub fn set_restore_mode(&mut self, mode: RestoreMode) {
        self.restore_mode = mode;
    }

    /// The active restore mode (informational; see [`RestoreMode`]).
    pub fn restore_mode(&self) -> RestoreMode {
        self.restore_mode
    }

    /// Tune the engine's derive-chain bound (task 95 M2.1; see
    /// [`SnapshotEngine::set_max_chain_len`]).
    pub fn set_max_chain_len(&mut self, max_chain_len: u32) {
        self.engine.set_max_chain_len(max_chain_len);
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
            Request::Hello(_client_caps) => {
                // Version compatibility is detectable from Caps alone (task 25
                // gate 4): the server answers its own capabilities and the
                // client compares — no error reply is needed here.
                self.hello_done = true;
                Ok(Ok(Reply::Hello(server_caps())))
            }
            Request::Snapshot => self.snapshot(),
            Request::Drop(snap) => Ok(self.drop_snap(*snap)),
            Request::Branch { snap, env } => self.restore(*snap, Some(env)),
            Request::Replay(snap) => self.restore(*snap, None),
            Request::Run { until, resolve } => {
                if resolve.is_some() {
                    // No decision is ever outstanding on the seed-driven
                    // substrate; absorbing a resolve would desync the client's
                    // DecisionId bookkeeping (task 25: never silently dropped).
                    return Ok(Err(ControlError::ResolveWithoutDecision));
                }
                self.run(until)
            }
            Request::Hash { scope } => match scope {
                HashScope::Whole => {
                    let vmm = self.vmm.as_ref().ok_or(ServeError::Poisoned)?;
                    Ok(Ok(Reply::Hash(vmm.state_hash())))
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
            // Observation verbs (task 80): `read`/`regs` look at guest state without
            // moving it. Both borrow `self.vmm` immutably and touch nothing else —
            // not the schedule, not `recorded`, not V-time — so `hash(Whole)` before
            // and after any sequence of them is bit-identical, and neither is ever
            // recorded into an `Environment` (the RESOLUTION.md search-surface
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
        // Fetch the guest image; a `None` VM is a torn session, session-fatal like
        // every sibling verb — never an empty-RAM fallback that fakes a range error.
        let ram = self
            .vmm
            .as_ref()
            .ok_or(ServeError::Poisoned)?
            .guest_memory();
        let ram_len = ram.len() as u64;
        // `gpa + len` in u128 so a near-u64::MAX gpa cannot wrap into a "valid" range.
        let end = u128::from(gpa) + u128::from(len);
        if end > u128::from(ram_len) {
            return Ok(Err(ControlError::ReadOutOfRange { gpa, len, ram_len }));
        }
        // In range: gpa and end both fit usize (end <= ram_len <= isize::MAX).
        let start = gpa as usize;
        Ok(Ok(Reply::Bytes(ram[start..start + len as usize].to_vec())))
    }

    /// The session's **V-time synchronization** predicate (PR #51 round-7): `true`
    /// iff the live VM's [`effective_vns`](Vmm::effective_vns) is **exact** (at a
    /// V-time intercept — [`Vmm::is_synchronized`]) rather than a stale lower bound.
    /// It is `true` after a deadline stop that landed on an arrival, a `restore` /
    /// `branch` (both anchor at the snapshot's intercept), a successful seal, or a
    /// fresh boot — and `false` after a terminal stop or any non-intercept exit,
    /// where the guest may have retired branches past the last-intercept anchor.
    ///
    /// The control plane trusts `effective_vns` as an exact position in exactly two
    /// places, both gated on this: the [`perturb`](ControlServer::perturb) floor and
    /// the `m == vns` exact-arrival drain in [`run`](ControlServer::run). Every other
    /// `effective_vns` consumer uses it only as a monotone lower bound (the deadline
    /// check, the informational `Deadline`/terminal `vtime`) or does not use it at
    /// all (the `branch` floor is the snapshot's `vm_state.vtime.snapshot_vns`, and
    /// `recorded` is stamped at the staged `Moment`, never at `effective_vns`).
    fn synchronized(&self) -> bool {
        self.vmm.as_ref().is_some_and(|v| v.is_synchronized())
    }

    /// `perturb(fault, at)`: decode the opaque host-fault blob and **stage** it at
    /// `Moment` `at` for [`ControlServer::run`] to apply — going through the same
    /// [`validate_host_fault`](ControlServer::validate_host_fault) gate a
    /// [`Request::Branch`] env host fault does, so the two paths reject identically
    /// (nothing that would mint a reproducer that does not reproduce is ever
    /// staged). A malformed blob is [`ControlError::MalformedEnvironment`]; the
    /// remaining rejections (unsynchronized point, past `Moment`, out-of-range gpa,
    /// out-of-scope clock fault, and the same-`Moment` conflict) are below / the
    /// shared gate's.
    fn perturb(
        &mut self,
        fault: &control_proto::HostFault,
        at: control_proto::Moment,
    ) -> Result<Reply, ControlError> {
        // A poisoned schedule must be rewound (branch/replay) before it accepts any
        // new fault — staging onto an unsatisfiable schedule is itself unsatisfiable.
        if let Some((moment, vtime)) = self.schedule_poisoned {
            return Err(ControlError::ScheduleUnsatisfiable { moment, vtime });
        }
        let decoded = environment::HostFault::decode(&fault.0)
            .map_err(|_| ControlError::MalformedEnvironment)?;
        // Capability first (an unarmable backend can't enforce host faults at all —
        // Unsupported, not the run-a-little-further NotSynchronized).
        if !self.vmm.as_ref().is_some_and(|v| v.can_arm_arrival()) {
            return Err(ControlError::Unsupported);
        }
        // **Synchronization gate (PR #51 round-7, family root cause).** The floor
        // below is `effective_vns`; at a non-intercept stop (a terminal HLT / debug /
        // shutdown, or a non-intercept exit) that value is only a lower bound, so a
        // fault staged at `at == floor` could be recorded at a `Moment` the guest has
        // already executed past — a reproducer that does not reproduce. Reject; the
        // client rewinds (branch/replay lands on an intercept) first.
        if !self.synchronized() {
            return Err(ControlError::NotSynchronized);
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

    /// The **single validate-and-stage gate** for a host fault, shared by
    /// [`perturb`](ControlServer::perturb) and by a [`Request::Branch`] env's host
    /// overrides so both reject identically (PR #51 review, blocking item 1). It
    /// checks, in order, and never mutates on failure:
    ///
    /// - **Past `Moment`** (`at < floor`) → [`ControlError::PerturbPastMoment`].
    ///   `floor` is the current effective V-time (perturb) or the restored
    ///   snapshot's V-time (branch); `at == floor` is fine (applies immediately and
    ///   truthfully).
    /// - **Same-`Moment` conflict** (`at` already staged) →
    ///   [`ControlError::PerturbMomentTaken`] (one fault per `Moment` — see the
    ///   [`schedule`](ControlServer::schedule) doc).
    /// - **Out-of-range [`CorruptMemory`](environment::HostFault::CorruptMemory)**
    ///   (`gpa + 8 > guest RAM`) → [`ControlError::PerturbOutOfRange`] (never
    ///   clipped/wrapped).
    /// - **Out-of-scope [`SkewTime`](environment::HostFault::SkewTime) /
    ///   [`SetClockRate`](environment::HostFault::SetClockRate)** →
    ///   [`ControlError::Unsupported`] (a follow-on lights these up).
    ///
    /// An [`InjectInterrupt`](environment::HostFault::InjectInterrupt) is rejected
    /// here (not at apply time) when its vector is architecturally reserved
    /// (`< 16`) → [`ControlError::PerturbReservedVector`], or the VM has no
    /// userspace LAPIC to raise it into → [`ControlError::Unsupported`] (PR #51
    /// round-8) — both stage-time-decidable, so a recoverable reply instead of a
    /// session-fatal apply-time failure.
    fn validate_host_fault(
        &self,
        fault: &environment::HostFault,
        at: environment::Moment,
        floor: environment::Moment,
    ) -> Result<(), ControlError> {
        // One fault per `Moment` — reject a duplicate against **both** the still-
        // staged schedule **and** the already-applied faults recorded in the
        // reproducer (PR #51 round-2 finding): once a fault at `at` has applied it is
        // gone from the schedule but present in `recorded`, and re-staging it would
        // overwrite it in `recorded_env()` (an applied fault silently vanishing). The
        // remaining, occupancy-free checks are shared with the branch-env path.
        if self.schedule.contains_key(&at) || self.recorded.overrides().contains_key(&at) {
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
        fault: &environment::HostFault,
        at: environment::Moment,
        floor: environment::Moment,
    ) -> Result<(), ControlError> {
        let vmm = self.vmm.as_ref().ok_or(ControlError::Unsupported)?;
        // **Capability check, up front (PR #51 round-2 finding).** Host-plane
        // enforcement needs the exact-count arrival seam ([`Vmm::arm_arrival`]); on a
        // backend that cannot arm it (stock KVM / M1 / M2 — no deterministic
        // retired-branch counter) a staged fault could only be applied at a natural
        // exit *past* its `Moment`, recording a count the run never truly stopped at.
        // Reject rather than silently apply late.
        if !vmm.can_arm_arrival() {
            return Err(ControlError::Unsupported);
        }
        if at < floor {
            return Err(ControlError::PerturbPastMoment { at, floor });
        }
        match fault {
            environment::HostFault::CorruptMemory { gpa, .. } => {
                let ram_len = vmm.guest_memory().len() as u64;
                if gpa.checked_add(8).is_none_or(|end| end > ram_len) {
                    return Err(ControlError::PerturbOutOfRange { gpa: *gpa, ram_len });
                }
            }
            environment::HostFault::SkewTime(_) | environment::HostFault::SetClockRate(_) => {
                return Err(ControlError::Unsupported);
            }
            environment::HostFault::InjectInterrupt { vector } => {
                // Both `InjectInterrupt` failure modes are stage-time-decidable
                // properties of the request/backend (PR #51 round-8) — reject them
                // here as recoverable replies, mirroring the `CorruptMemory` bounds
                // check, rather than letting them explode as a session-fatal
                // `ServeError::Vmm` at `apply_host_fault`:
                // - no userspace LAPIC to raise the vector into → `Unsupported`
                //   (a permanent backend limitation — unlike `CorruptMemory`, which a
                //   no-LAPIC guest can still take via the idle arrival-wake);
                if !vmm.lapic_wired() {
                    return Err(ControlError::Unsupported);
                }
                // - an architecturally reserved vector (`< 16`) the LAPIC cannot raise
                //   → `PerturbReservedVector` (a request error the client can fix).
                if *vector < 16 {
                    return Err(ControlError::PerturbReservedVector { vector: *vector });
                }
            }
        }
        Ok(())
    }

    /// The capture-path chooser for one seal (task 95 M2.1). Derive over the
    /// harvested dirty set **only** when everything is provably right: a tracked
    /// parent exists, it is still live in the store, its chain is under the
    /// bound, and the harvest vouches for completeness ([`Vmm::harvest_dirty_gfns`]
    /// returns `Some` — backend log readable, no untracked host write). On any
    /// doubt — including a failed derive itself — fall back to `snapshot_base`
    /// (correct-by-construction; content-dedup keeps a flatten cheap in storage).
    /// The seal RPC never fails because the optimization was unavailable.
    fn seal_into_store(
        engine: &mut SnapshotEngine,
        vmm: &mut Vmm<B>,
        parent: Option<SnapshotId>,
        blob: &[u8],
    ) -> Result<SnapshotId, SnapshotError> {
        if let Some(parent) = parent {
            // Still live (a `drop` verb may have released it since) and bounded:
            // at the chain cap the seal flattens via a fresh base instead.
            let chain_ok = engine
                .stats(parent)
                .is_ok_and(|s| s.chain_len < engine.max_chain_len());
            if chain_ok
                && let Some(gfns) = vmm.harvest_dirty_gfns()
                // A gfn past the image would be a slot-decode bug; don't feed it
                // to the engine (whose loud error is for callers) — full-scan.
                && gfns.iter().all(|&g| g < engine.mem_pages())
                && let Ok(id) =
                    engine.snapshot_derive(parent, vmm.guest_memory(), Some(&gfns), blob)
            {
                return Ok(id);
            }
        }
        engine.snapshot_base(vmm.guest_memory(), blob)
    }

    /// `snapshot`: seal the current point into the engine (memory image +
    /// canonical `vm_state` blob) and mint a wire handle. Since task 95 M2.1 the
    /// memory half derives from the tracked parent over the harvested dirty set
    /// when it safely can ([`Self::seal_into_store`]); the reply and semantics
    /// are identical either way.
    ///
    /// **Rejects loudly while a host-fault schedule is pending** (PR #51 round-3):
    /// a snapshot seals only VM state, and every restore of it clears the schedule
    /// — so the sealed state's *future* (the staged fault) would be unreproducible
    /// from the snapshot. A staged fault is "armed" in exactly the sense
    /// [`ControlError::SnapshotWhileArmed`] names, so the seal is refused rather
    /// than silently dropping the future (persisting the schedule inside the
    /// snapshot is a semantics change that would need its own ruling).
    fn snapshot(&mut self) -> Result<Result<Reply, ControlError>, ServeError> {
        if let Some((moment, vtime)) = self.schedule_poisoned {
            return Ok(Err(ControlError::ScheduleUnsatisfiable { moment, vtime }));
        }
        if !self.schedule.is_empty() || !self.reseed_schedule.is_empty() {
            return Ok(Err(ControlError::SnapshotWhileArmed));
        }
        let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
        let vm_state = match vmm.save_vm_state() {
            Ok(s) => s,
            // The remaining fail-closed boundaries (RNG mid-exit completion,
            // non-V-time-synchronized point, unrepresentable in-flight state)
            // all surface as ContractViolation: "not a snapshottable point" —
            // the caller runs a little further and retries.
            Err(VmmError::ContractViolation(_)) => return Ok(Err(ControlError::NotQuiescent)),
            // A backend save failure is substrate breakage, not a caller error.
            Err(e) => return Err(e.into()),
        };
        let blob = vm_state.encode().map_err(SnapshotError::from)?;
        // Task 95 M2.1: capture O(dirty) when the tracked window allows it,
        // full-scan otherwise — then re-arm the window with the new snapshot as
        // the next seal's parent (the arm fails ⇒ the next seal full-scans too).
        let store_id = Self::seal_into_store(&mut self.engine, vmm, self.derive_parent, &blob)?;
        self.derive_parent = vmm.reset_dirty_tracking().then_some(store_id);
        // Task 73: capture the SDK channel's replay-relevant state (seeded stream
        // position + event log) alongside the guest snapshot — owned, so `vmm`'s
        // borrow ends before we touch `self.sdk_snaps`.
        let sdk_channel = vmm.sdk_snapshot();
        // Task 61: capture the Net channel's flow-policy stream position + decision
        // log the same way (owned, so the borrow ends before touching self).
        let net_channel = vmm.net_snapshot();
        let id = self.next_snap;
        self.next_snap += 1;
        self.snaps.insert(id, store_id);
        if let Some(channel) = sdk_channel {
            // Capture the active policy too, so a replay restores the buggify
            // biasing (the restore path resets `recorded` to `none()`).
            let policy = self.recorded.policy().clone();
            self.sdk_snaps.insert(id, SdkSnap { channel, policy });
        }
        if let Some(channel) = net_channel {
            // Same reason as SdkSnap: a replay resets `recorded` to `none()`, so
            // the seal-time policy must be restored before the Net env materializes.
            let policy = self.recorded.policy().clone();
            self.net_snaps.insert(id, NetSnap { channel, policy });
        }
        // Task 81: a snapshot inherits the live timeline's taint. If the timeline
        // was tainted by an `exec` improvisation, record the handle (so any
        // `branch`/`replay` of it yields a tainted timeline) and surface the
        // taint-carrying reply; otherwise the pre-81 taint-free `SnapId` reply,
        // byte-identical to every existing capture (gate 4).
        if self.timeline_tainted {
            self.tainted_snaps.insert(id);
            Ok(Ok(Reply::Snapshot {
                id: SnapId(id),
                tainted: true,
            }))
        } else {
            Ok(Ok(Reply::SnapId(SnapId(id))))
        }
    }

    /// `drop`: release the store layer behind a wire handle and GC.
    fn drop_snap(&mut self, snap: SnapId) -> Result<Reply, ControlError> {
        let Some(store_id) = self.snaps.remove(&snap.0) else {
            return Err(ControlError::UnknownSnapshot(snap));
        };
        // Task 73: drop the SDK channel snapshot with its handle (ephemeral pool
        // state, released alongside the guest snapshot).
        self.sdk_snaps.remove(&snap.0);
        // Task 61: drop the Net channel snapshot with its handle too.
        self.net_snaps.remove(&snap.0);
        // Task 81: drop its taint record with the handle. (The `SnapId` is
        // monotonic — [`next_snap`] never reuses a number — so a later snapshot can
        // never inherit this one's taint by handle reuse.)
        self.tainted_snaps.remove(&snap.0);
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

    /// `branch` (with an env) / `replay` (without): restore `snap` into a
    /// fresh VM from the factory, then reseed from the env's seed iff branching.
    fn restore(
        &mut self,
        snap: SnapId,
        env: Option<&control_proto::Environment>,
    ) -> Result<Result<Reply, ControlError>, ServeError> {
        // 1. Validate everything non-destructively first: the env blob …
        //    `seed` is the branch seed (`None` for a verbatim replay); `host` is
        //    the env's host-plane schedule to stage after a successful restore.
        let mut host: Vec<(environment::Moment, environment::HostFault)> = Vec::new();
        let mut reseeds: BTreeMap<environment::Moment, u64> = BTreeMap::new();
        // Task 73: the branch env's (buggify-only) fault policy, preserved into the
        // recorded reproducer below so `recorded_env()` re-emits it and a replay
        // reproduces the buggify decisions. `None` for a verbatim replay.
        let mut env_policy: Option<FaultPolicy> = None;
        let seed = match env {
            None => None,
            Some(env) => {
                if env.blob_version != EnvSpec::BLOB_VERSION {
                    return Ok(Err(ControlError::BadEnvVersion(env.blob_version)));
                }
                let spec = match EnvSpec::decode(&env.bytes) {
                    Ok(spec) => spec,
                    Err(EnvError::BadVersion(v)) => return Ok(Err(ControlError::BadEnvVersion(v))),
                    Err(_) => return Ok(Err(ControlError::MalformedEnvironment)),
                };
                // Task 59 lights up the **host** plane: an env's host overrides are
                // now enforced — staged for `run` to apply at their `Moment`s (the
                // record → replay closure). The still-unenforceable halves must be
                // REJECTED rather than silently run without them (a silent no-op
                // mints reproducers that do not reproduce): a **guest** override
                // needs the task-61 `decide`-seam loop; a **standing** fault needs
                // the guest-utility enforcement; a fault policy that faults a
                // **service** class (net/block/process) makes the seeded stream
                // answer decisions with faults no service enforces.
                //
                // **Task 73 relaxation:** a **buggify-only** policy IS enforceable
                // now — the SDK decide-seam (the doorbell's `Sdk` service →
                // `Environment::decide`) answers `DecisionClass::Buggify`, so a
                // reproducer whose only faults are buggify biasing is accepted
                // (its buggify decisions replay from the seeded fault stream).
                //
                // **Task 61 widening:** the in-guest **flow agent** is the
                // `NetFlow` decide-seam (the doorbell's `Net` service →
                // `Environment::decide` → the guest enforces the per-flow policy on
                // the CNI), so a policy that faults the **net** class is now
                // enforceable too. Accept a policy that faults **only** the
                // enforceable classes (buggify and/or net) — its per-flow decisions
                // replay from the seeded fault stream exactly like buggify (no guest
                // overrides, so the reproducer still round-trips a later
                // branch/replay). A block/process fault (no decide-seam yet) is
                // still rejected. A **guest override** (a pinned per-Moment answer)
                // and a **standing** fault remain unsupported — the net path is
                // driven by the seeded policy, not by pinned overrides.
                let has_standing = matches!(
                    &spec,
                    EnvSpec::Recorded { standing, .. } if !standing.is_empty()
                );
                let has_guest = spec
                    .overrides()
                    .values()
                    .any(|a| a.guest_answer().is_some());
                if has_guest || has_standing || !spec.policy().is_enforceable_only() {
                    return Ok(Err(ControlError::Unsupported));
                }
                host = spec.host_faults().collect();
                reseeds = spec.reseeds().clone();
                env_policy = Some(spec.policy().clone());
                Some(spec.seed())
            }
        };
        // … then the handle and the sealed snapshot pieces.
        let Some(&store_id) = self.snaps.get(&snap.0) else {
            return Ok(Err(ControlError::UnknownSnapshot(snap)));
        };
        let Ok(mapping) = self.engine.materialize(store_id) else {
            return Ok(Err(ControlError::RestoreFailed));
        };
        let Ok(vm_state) = self.engine.vm_state(store_id) else {
            return Ok(Err(ControlError::RestoreFailed));
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
        let restored_floor = vm_state.vtime.snapshot_vns;
        let mut seen: std::collections::BTreeSet<environment::Moment> =
            std::collections::BTreeSet::new();
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
            if m > restored_floor && !self.vmm.as_ref().is_some_and(|v| v.can_arm_arrival()) {
                return Ok(Err(ControlError::Unsupported));
            }
        }
        // 2. Drop the live VM (frees its work counter — the box allows one
        //    open at a time), then boot the fresh restore target. A factory
        //    failure is fatal: the session has no VM anymore.
        //
        //    Task 95 M2.2: with a remap factory installed (and the Remap mode
        //    active, the default), the fresh VM is composed **around** the
        //    materialized mapping — its buffer is the guest RAM the memslots
        //    register — and only the non-memory half is restored
        //    (`restore_vm_state`): no full-image memcpy, untouched pages fault
        //    lazily, guest writes stay CoW-private. Otherwise the pre-task-95
        //    memcpy path runs byte-for-byte (`restore_snapshot`).
        let use_remap = self.restore_mode == RestoreMode::Remap && self.remap_factory.is_some();
        self.vmm = None;
        let (mut fresh, restore_result) = if use_remap {
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
        };
        // 3. Split the two result categories (mirrors `snapshot`).
        //    `restore_vm_state` validates the untrusted blob **before** mutating
        //    any live state, so a *validation-class* rejection leaves the fresh
        //    VM intact at its boot point — keep it (the session stays usable) and
        //    answer the recoverable `RestoreFailed`. A failure *after* validation
        //    (a `Backend::restore` fault, a work-counter reset failure) is
        //    substrate breakage: the fresh VM's state can no longer be vouched
        //    for, so the VM is dropped (stays `None` → poisoned) and the session
        //    is torn down (`ServeError`) rather than let a client run from
        //    unvouched state.
        match restore_result {
            Ok(()) => {}
            // Pre-commit rejection (a bad/foreign blob, mismatched wiring, or an
            // invalid clock config) — the fresh VM never mutated, so keep it.
            Err(VmmError::ContractViolation(_) | VmmError::Snapshot(_) | VmmError::Vtime(_)) => {
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
                self.vmm = Some(fresh);
                // Task 95 M2.1: the kept VM is a fresh boot — no snapshot's
                // tracked continuation, so the next seal full-scans.
                self.derive_parent = None;
                // The VM was still REPLACED (a fresh boot), so the old timeline's
                // staged faults + recorded reproducer must not survive attached to
                // it (PR #51 round-2 finding): reset on every path that swaps the VM,
                // not just success. **This is the one branch/replay path that mutates
                // on a recoverable error** (PR #51 round-5): a genuine restore failure
                // cannot be pre-validated (it is only discovered by attempting the
                // restore into the fresh VM), so — unlike the host-fault rejection
                // above, which is now side-effect-free — a `RestoreFailed` leaves the
                // session on the reset fresh boot. Callers treat `RestoreFailed` as
                // "the session VM was replaced; re-establish your point."
                self.reset_schedule_to_fresh_vm();
                // Task 81: the kept VM is a **fresh boot** (a clean genesis-like
                // timeline), not the tainted snapshot's state — so it is untainted,
                // whatever the failed-to-restore handle's taint was. (An untainted
                // state reachable only from an untainted ancestor: this boot is one.)
                self.timeline_tainted = false;
                // Task 73 (round-4 P3): the kept fresh VM must carry an SDK channel
                // too — `GUEST_HAS_SDK` is advertised unconditionally, so a doorbell
                // on it after a recoverable `RestoreFailed` would otherwise be a
                // contract violation (`sdk: None`). Wire it from the reset (bare
                // `Seeded`) reproducer, mirroring `new` + the success path.
                let sdk_env = self.recorded.materialize();
                let sdk_policy = self.recorded.policy().clone();
                if let Some(vmm) = self.vmm.as_mut() {
                    vmm.enable_sdk(sdk_env, &sdk_policy);
                }
                // Task 61: the kept fresh VM must carry a Net channel too (same
                // reason — the doorbell is serviced when net is wired), mirroring
                // `new` + the success path. No env: a net decision draws from the
                // shared SDK stream wired just above (the single-stream ruling).
                if let Some(vmm) = self.vmm.as_mut() {
                    vmm.enable_net();
                }
                return Ok(Err(ControlError::RestoreFailed));
            }
            // Post-validation substrate breakage — the VM is unvouched; tear down.
            Err(e) => return Err(e.into()),
        }
        // 4. Branch ⇒ fork the entropy stream. On this substrate `reseed_entropy`
        //    fails only if V-time is unwired — a composition bug (the factory must
        //    mirror the live VM), fatal.
        //
        //    **No markers** (the pre-task-78 shape): reseed from the env's seed —
        //    byte-for-byte the task-58/59 behavior. **Markers present**: the table
        //    is authoritative (task 78) — a marker at the restore floor is the
        //    (collapsed) branch reseed and applies now; markers beyond the floor
        //    are staged for `run`'s exact-arrival drain; and with no marker at the
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
        // Task 73: preserve the branch env's (buggify-only) policy in the recorded
        // reproducer (the reset above reset it to `none`), then wire the SDK
        // channel from the now-final reproducer so a seeded run draws buggify from
        // the seeded fault stream and a replay from the recorded overrides. Wired
        // on every restore; inert (and unhashed) for a guest that never rings the
        // doorbell, so non-SDK paths are unchanged.
        // Task 73: choose the policy the SDK env materializes with. A **branch**
        // uses the branch env's (buggify-only) policy; a **replay** restores the
        // policy active when the snapshot was sealed — else the reset-to-`none`
        // above would make the restored stream draw all-`Nominal` (P1). `env_policy`
        // is `Some` iff branching, so `or_else` picks the snapshot's policy only on
        // a replay.
        let sdk_snap = self.sdk_snaps.get(&snap.0).cloned();
        let net_snap = self.net_snaps.get(&snap.0).cloned();
        // On a replay, restore the seal-time policy from whichever channel snapshot
        // carries it (SDK or Net capture the same reproducer policy); a branch uses
        // its own `env_policy`. Without this the reset-to-`none` above makes a
        // restored stream draw all-`Nominal` — including a Net-only run (P1).
        let restore_policy = env_policy
            .or_else(|| sdk_snap.as_ref().map(|s| s.policy.clone()))
            .or_else(|| net_snap.as_ref().map(|s| s.policy.clone()));
        if let Some(policy) = restore_policy {
            self.set_recorded_policy(policy);
        }
        let sdk_env = self.recorded.materialize();
        let sdk_policy = self.recorded.policy().clone();
        self.vmm
            .as_mut()
            .ok_or(ServeError::Poisoned)?
            .enable_sdk(sdk_env, &sdk_policy);
        // Task 61: wire the Net channel (capture only, no env). A net decision
        // draws from the shared SDK stream wired just above (the single-stream
        // ruling), so a seeded run draws the flow policy from that one seeded fault
        // stream and a replay continues it from the restored position.
        self.vmm.as_mut().ok_or(ServeError::Poisoned)?.enable_net();
        // Restore the SDK channel snapshot for this handle, if any. A verbatim
        // **replay** (`seed` is `None`) continues the seeded streams from the
        // snapshot's position AND keeps the event prefix — so a fork from a mid-run
        // SDK snapshot reproduces. A **branch** reseeds (`enable_sdk` just set fresh
        // streams from the new seed), so it takes only the shared prefix events —
        // the declared catalog the never-fired report needs — and lets the new seed
        // drive the fork's future.
        if let Some(s) = sdk_snap {
            let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
            if seed.is_some() {
                vmm.sdk_restore_events(&s.channel);
            } else {
                vmm.sdk_restore(&s.channel);
            }
        }
        // Task 61: restore the Net channel's decision prefix. The flow-policy
        // stream position rides the shared SDK stream, restored by
        // sdk_restore/sdk_restore_events above (so a replay's net_decide answers are
        // bit-identical and a branch reseeds), so both paths restore the same thing
        // here — just the decision log carried forward for the fork's evidence.
        if let Some(n) = net_snap {
            let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
            vmm.net_restore(&n.channel);
        }
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
        // Task 95 M2.1: the restored VM's memory IS `store_id`'s image (memcpy
        // wrote exactly it; remap maps exactly it), so arm the dirty window with
        // the branch/replay source as the next seal's derive parent. The
        // harvest-and-discard resets the backend log (dropping any stale
        // pre-restore guest-write bits) and clears the memcpy path's wholesale
        // latch; if it cannot arm, the next seal simply full-scans.
        {
            let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
            self.derive_parent = vmm.reset_dirty_tracking().then_some(store_id);
        }
        Ok(Ok(Reply::Unit))
    }

    /// Overwrite the recorded reproducer's fault policy in place, keeping its
    /// variant (task 73): the branch env's buggify-only policy must survive into
    /// [`recorded_env`](ControlServer::recorded_env) so a replay reproduces the
    /// buggify decisions.
    fn set_recorded_policy(&mut self, policy: FaultPolicy) {
        match &mut self.recorded {
            EnvSpec::Seeded { policy: p, .. } | EnvSpec::Recorded { policy: p, .. } => *p = policy,
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
        self.schedule.clear();
        self.reseed_schedule.clear();
        // A rewind is the recovery from a poisoned schedule (round-3): clear the
        // latch so `run`/`perturb`/`snapshot` work again on the fresh timeline.
        self.schedule_poisoned = None;
        let seed = self
            .vmm
            .as_ref()
            .and_then(|v| v.entropy_state())
            .unwrap_or(0);
        self.recorded = EnvSpec::Seeded {
            seed,
            policy: FaultPolicy::none(),
        };
    }

    /// `run(until)`: step the event loop to a terminal stop or the V-time
    /// deadline. The deadline is checked against [`Vmm::effective_vns`]
    /// **before** each step, so a run already at-or-past its deadline stops
    /// immediately (without entering the guest), and the stop point is the first
    /// V-time-intercept boundary at-or-after the deadline — deterministic across
    /// same-seed runs, because effective V-time is.
    ///
    /// **Deadline enforcement is opportunistic, not a hard force-exit.** The
    /// deadline is observed at each step's V-time boundary: a guest that keeps
    /// taking VM-exits (any real workload — and a compute-bound one is preempted
    /// by task-47's LAPIC-timer force-exit, which advances the anchor) is bounded
    /// within one exit/preemption of the deadline. A *hard* force-exit at an
    /// arbitrary deadline (round 4's `step_until`) was **reverted**: on the box it
    /// armed `run_until` at the far sweep deadline on every step, and because
    /// every run terminates *before* that deadline (the workload reboots first),
    /// each left an un-hit PMU/planner arm behind — stale state that accumulated
    /// across restore boundaries and finally diverged a `state_hash` on the 16th
    /// run (PR #44 pass 5; the `#34`/`#55` stale-arm class). Making the deadline a
    /// hard bound needs the backend to reset the `run_until` arm across runs — a
    /// `patched_kvm`/`pmu_sys` change **outside task-58's surface**, deferred.
    ///
    /// `run(until)` becomes **"run to `min(next staged Moment, until)`"** (task
    /// 59): before each step, apply the host fault the run has reached (arrival is
    /// exact — [`Vmm::arm_arrival`] makes `step`'s `run_until` stop *between
    /// instructions* at the next staged `Moment`), stamping it into the recorded
    /// env ([`ControlServer::recorded_env`]). With no faults staged this is
    /// byte-for-byte the task-58 loop (arrival is never armed).
    ///
    /// **Exact vs. late vs. future (PR #51 round-2/5).** At each V-time `vns` the
    /// drain classifies a staged `Moment m`:
    /// - `m == vns` → **exact arrival**: apply now (the arrival landed here, or the
    ///   guest is exactly at the current point). This holds **regardless of the
    ///   deadline** — applying at `m == vns` is never "late" (round-5 item 3).
    /// - `m < vns` → **late/crossed**: the guest executed *past* `m` (only possible
    ///   on an overshoot), so it can never be applied at its recorded count — the
    ///   schedule is **poisoned** (round-3) and every later `run`/`perturb`/`snapshot`
    ///   rejects until a `branch`/`replay` rewinds.
    /// - `m > vns` → **future**: not yet reached; left staged (or dropped at a
    ///   terminal, round-5 item 2).
    ///
    /// The deadline only gates **arming** (arm only `m ≤ deadline`, matching task-58's
    /// no-hard-force-exit posture) and the **stop** (`vns ≥ deadline` → `Deadline`).
    fn run(
        &mut self,
        until: &control_proto::StopConditions,
    ) -> Result<Result<Reply, ControlError>, ServeError> {
        // A poisoned schedule keeps rejecting `run` (with the crossed fault's
        // coordinates) until a `branch`/`replay` rewinds it — never applying the
        // crossed fault from the past on a re-sent `run` (PR #51 round-3).
        if let Some((moment, vtime)) = self.schedule_poisoned {
            return Ok(Err(ControlError::ScheduleUnsatisfiable { moment, vtime }));
        }
        loop {
            let (vns, synchronized) = {
                let vmm = self.vmm.as_ref().ok_or(ServeError::Poisoned)?;
                (vmm.effective_vns().unwrap_or(0), vmm.is_synchronized())
            };

            // 1. Drain: apply an **exact arrival** — `m == vns` at a **synchronized**
            //    point (`effective_vns` is exact there). A `Moment` at-or-below a vns
            //    that is *not* provably exact must NOT be applied as an exact arrival:
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
                if m < vns || !synchronized {
                    self.schedule_poisoned = Some((m, vns));
                    return Ok(Err(ControlError::ScheduleUnsatisfiable {
                        moment: m,
                        vtime: vns,
                    }));
                }
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
                    vmm.apply_host_fault(&fault).map_err(ServeError::Vmm)?;
                    self.recorded.perturb(fault, m);
                }
            }

            // 1b. Surface a DEFERRED `setup_complete` snapshot point (task 73 P1) —
            //     only HERE, **after the drain** (round-5 P1), and only when the
            //     schedule is **EMPTY** (round-6 P2): `snapshot` rejects *any*
            //     non-empty schedule (`SnapshotWhileArmed`), so a still-staged
            //     FUTURE fault (`m > vns`) would make the advertised seal fail. The
            //     drain applied every fault at-or-below this synchronized vns; if a
            //     future fault remains, keep deferring — the run applies each fault
            //     at its `Moment`, shrinking the schedule, and the point surfaces at
            //     the first synchronized boundary where the schedule has drained to
            //     empty (`clear_arrival` then disarms the next arrival, so the seal
            //     is clean). (`take_synchronized_snapshot_point` is a no-op unless
            //     the VM is at a synchronized, sealable boundary.)
            //     Gated on the client `StopMask` (round-7): only surface when the
            //     `SNAPSHOT_POINT` class is armed. The whole block is gated (not
            //     just the return), so an unarmed run does NOT consume the pending
            //     point — it stays deferred and the run continues to the terminal
            //     (`StopMask::NONE` runs straight through setup_complete).
            //     Gate on BOTH staged structures being empty (task 78): `snapshot`
            //     (control.rs:604) rejects a seal while EITHER `schedule` OR
            //     `reseed_schedule` is non-empty, so surfacing the point with a
            //     future reseed still staged would advertise a seal the explorer's
            //     eager `snapshot()` then fails on — or seal a point missing its
            //     pending reseeds (replay diverges). Keep deferring until both drain.
            if until.on.armed(control_proto::class_bit::SNAPSHOT_POINT)
                && self.schedule.is_empty()
                && self.reseed_schedule.is_empty()
            {
                let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
                if vmm.take_synchronized_snapshot_point() {
                    let vns = vmm.effective_vns().unwrap_or(0);
                    vmm.clear_arrival();
                    return Ok(Ok(Reply::Stop(StopReason::SnapshotPoint {
                        vtime: VTime(vns),
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
                vmm.clear_arrival();
                return Ok(Ok(Reply::Stop(StopReason::Deadline { vtime: VTime(vns) })));
            }

            // 3. Arm exact-count arrival at the next staged `Moment` that is at-or-
            //    before the deadline (a fault beyond the deadline is never reached —
            //    the opportunistic stop above catches the deadline first, matching
            //    the task-58 no-hard-force-exit posture). No such `Moment` ⇒ no
            //    arrival armed (plain open-ended step / task-47 timer preemption).
            let next = [
                self.schedule.keys().next().copied(),
                self.reseed_schedule.keys().next().copied(),
            ]
            .into_iter()
            .flatten()
            .min()
            .filter(|&m| until.deadline.is_none_or(|d| m <= d.0));
            match next {
                Some(m) => {
                    vmm.arm_arrival(m);
                }
                None => vmm.clear_arrival(),
            }

            // 4. Step. A terminal stop ends the run; a cooperating-SDK stop
            //    (task 73) surfaces the assertion / snapshot point.
            match vmm.step()? {
                // A deferred `setup_complete` snapshot point is surfaced at the top
                // of the NEXT iteration, after the drain (round-5 P1) — not here.
                Step::Continued => {}
                Step::SdkStop => {
                    let vns = vmm.effective_vns().unwrap_or(0);
                    vmm.clear_arrival();
                    let stop = vmm.take_sdk_stop();
                    // **Poison loud on ANY staged fault (P2, task 59's crossed-
                    // fault rule).** An SDK stop surfaces at a hypercall-doorbell
                    // `OUT` — NOT a V-time intercept — so `effective_vns` here is
                    // only a lower bound; a still-staged fault at-or-just-above it
                    // may already be crossed (the guest ran past `m` within the
                    // skid window). Exactly like the terminal arm below, poison
                    // rather than silently returning a stop past a crossed fault;
                    // the client rewinds via `branch`/`replay`. (Buggify decisions
                    // reproduce from the reproducer's seed + policy, so nothing is
                    // recorded here.)
                    // Any staged host fault OR staged reseed (task 78) poisons — a
                    // crossed reseed marker (guest ran past its `Moment` in the skid
                    // window) is the same non-reproducing class as a crossed fault:
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
                        self.schedule_poisoned = Some((m, vns));
                        return Ok(Err(ControlError::ScheduleUnsatisfiable {
                            moment: m,
                            vtime: vns,
                        }));
                    }
                    // Gate the assertion on the client `StopMask` (round-7): if the
                    // `ASSERTION` class is not armed, the stop is already consumed
                    // (`take_sdk_stop` above) and the guest is past the assert
                    // doorbell `OUT`, so continue the loop — the run proceeds to the
                    // terminal (`StopMask::NONE` runs an assertion straight through).
                    if !until.on.armed(control_proto::class_bit::ASSERTION) {
                        continue;
                    }
                    let reason = match stop {
                        Some(sdk_stop) => sdk_stop_to_reason(sdk_stop, vns),
                        // `SdkStop` is armed (an assertion) before the step returns
                        // `SdkStop`, so this is statically unreachable; be total
                        // anyway with a benign quiescent stop.
                        None => StopReason::Quiescent { vtime: VTime(vns) },
                    };
                    return Ok(Ok(Reply::Stop(reason)));
                }
                Step::Terminal(reason) => {
                    let vns = vmm.effective_vns().unwrap_or(0);
                    vmm.clear_arrival();
                    // **Poison loud on ANY staged fault at a terminal (PR #51
                    // round-6, supersedes round-5 item 2).** A natural terminal exit
                    // (HLT / debug) is *not* a V-time intercept, so `effective_vns`
                    // here is only the last-intercept **lower bound** — nothing still
                    // staged is provably uncrossed (with `deadline < m` the arrival is
                    // never armed and the guest can run past `m` to the terminal). The
                    // round-5 silent `clear()` could therefore drop an accepted perturb
                    // that *was* crossed — breaking exact-arrival-or-loud. The safe
                    // semantic is LOUD: poison whenever any fault remains staged, and
                    // let the client rewind (`branch`/`replay` clears the schedule —
                    // which campaign flows do anyway, so the task-60 crash path stays
                    // viable and the round-5 `SnapshotWhileArmed` trap stays fixed: a
                    // named, rewindable error instead of a silent stuck state).
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
                        self.schedule_poisoned = Some((m, vns));
                        return Ok(Err(ControlError::ScheduleUnsatisfiable {
                            moment: m,
                            vtime: vns,
                        }));
                    }
                    return Ok(Ok(Reply::Stop(map_terminal(reason, vns))));
                }
            }
        }
    }

    /// `exec(cmd, deadline)`: the **improvisation** (task 81). Inject `cmd` on the
    /// guest's serial input (as if typed at the serial shell), step the VM until the
    /// completion sentinel or the V-time `deadline`, and capture the serial output.
    ///
    /// **Taints the timeline first — before any fallible work** (the conservative
    /// taint invariant). Even if injection, a step, or a terminal aborts the run
    /// below, the timeline is already (correctly) tainted, so the reproducer guard
    /// ([`recorded_env_reply`](Self::recorded_env_reply)) can never mint a clean
    /// reproducer after an attempted `exec`. The server **refuses nothing** — a
    /// caller may deliberately sacrifice a timeline; fork-first is a usage
    /// discipline, not a server rule.
    ///
    /// **Off the record by ruling** (`docs/RESOLUTION.md` §Improvisations): the
    /// serial channel is deliberately crude, there is **no determinism guarantee**
    /// on this path, and nothing here is recorded into the reproducer
    /// ([`recorded`](Self::recorded) is untouched) or the fault schedule. See the
    /// sentinel scheme + failure modes in [`crate::exec`] and `IMPLEMENTATION.md`.
    fn exec(
        &mut self,
        cmd: &str,
        deadline: VTime,
    ) -> Result<Result<Reply, ControlError>, ServeError> {
        // 1. Conservative taint: set BEFORE touching the guest, covering every
        //    failure point below.
        self.timeline_tainted = true;
        let nonce = self.exec_nonce;
        self.exec_nonce = self.exec_nonce.wrapping_add(1);

        let mut session = ExecSession::new(cmd, nonce);
        let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
        // 2. Inject the command line on the serial RX and note the current capture
        //    length, so only NEW output is fed to the completion scanner.
        vmm.inject_serial_input(session.input());
        let mut cursor = vmm.serial_output().len();

        // 3. Step toward the sentinel / deadline / terminal. The deadline is
        //    observed opportunistically at each V-time boundary (like `run`): a hung
        //    or shell-less guest ends here `ok = false`. No fault-schedule
        //    interaction — `exec` is off the record.
        loop {
            let out = vmm.serial_output();
            if out.len() > cursor {
                session.feed(&out[cursor..]);
                cursor = out.len();
            }
            if session.is_done() {
                break;
            }
            let vns = vmm.effective_vns().unwrap_or(0);
            if vns >= deadline.0 {
                session.finish_timeout();
                break;
            }
            match vmm.step()? {
                Step::Continued => {}
                // A cooperating-SDK doorbell during an improvisation is consumed and
                // ignored (exec is not an SDK-driven run); keep stepping.
                Step::SdkStop => {
                    let _ = vmm.take_sdk_stop();
                }
                // The guest halted / crashed / rebooted before the sentinel: drain
                // any final output and close as an (unsuccessful) timeout.
                Step::Terminal(_) => {
                    let out = vmm.serial_output();
                    if out.len() > cursor {
                        session.feed(&out[cursor..]);
                    }
                    session.finish_timeout();
                    break;
                }
            }
        }
        let outcome = session.into_outcome();
        Ok(Ok(Reply::ExecResult {
            output: outcome.output,
            ok: outcome.ok,
        }))
    }

    /// The reply to [`Request::RecordedEnv`] (task 81) — the taint guard's
    /// **fail-loud site**. Mint the recorded reproducer (the [`recorded`](Self::recorded)
    /// [`EnvSpec`] as a wire [`Environment`]) **only** on an untainted timeline; a
    /// timeline an `exec` improvisation has tainted returns [`ControlError::Tainted`]
    /// instead — an improvised timeline is off the record and has no honest
    /// reproducer, so the server refuses rather than hand back an `Environment` that
    /// does not reproduce. Pure (no VM mutation), so it is answerable at any point.
    fn recorded_env_reply(&self) -> Result<Reply, ControlError> {
        if self.timeline_tainted {
            return Err(ControlError::Tainted);
        }
        Ok(Reply::Recorded(Environment {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: self.recorded.encode(),
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

/// Assemble the wire [`RegsView`] for the `regs` observation verb (task 80) from
/// the VM's best-effort vCPU read ([`Vmm::inspect_vcpu`]) and its effective V-time
/// ([`Vmm::effective_vns`]). Pure and non-mutating.
///
/// The GPRs and segment selectors are placed in the view's canonical order
/// (`rax rbx rcx rdx rsi rdi rbp rsp r8..r15` — note **rbp before rsp** — and
/// `cs ss ds es fs gs`). `Moment` and `vtime` are the two names of the single
/// deterministic axis: the effective V-time is a retired-branch count in whole
/// nanoseconds (ratio 1), which is exactly the [`Moment`] the perturb/run plane
/// addresses, so both fields carry it (a fresh / V-time-unwired VM reads `0`).
fn regs_view<B: Backend>(vmm: &Vmm<B>) -> RegsView {
    let s = vmm.inspect_vcpu();
    let r = &s.regs;
    let vns = vmm.effective_vns().unwrap_or(0);
    RegsView {
        version: RegsView::VERSION,
        gpr: [
            r.rax, r.rbx, r.rcx, r.rdx, r.rsi, r.rdi, r.rbp, r.rsp, r.r8, r.r9, r.r10, r.r11,
            r.r12, r.r13, r.r14, r.r15,
        ],
        rip: r.rip,
        rflags: r.rflags,
        seg: [
            s.sregs.cs.selector,
            s.sregs.ss.selector,
            s.sregs.ds.selector,
            s.sregs.es.selector,
            s.sregs.fs.selector,
            s.sregs.gs.selector,
        ],
        cr0: s.sregs.cr0,
        cr3: s.sregs.cr3,
        cr4: s.sregs.cr4,
        moment: Moment(vns),
        vtime: vns,
    }
}

fn sdk_stop_to_reason(stop: SdkStop, vns: u64) -> StopReason {
    let vtime = VTime(vns);
    match stop {
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
    let vtime = VTime(vns);
    match reason {
        TerminalReason::Hlt | TerminalReason::DebugExit { code: 0 } => {
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
    //! The socket loopback + adapter integration lives in
    //! `dissonance/conductor` (which composes this server with the explorer's
    //! socket `Machine`).

    use control_proto::{
        Answer, CapFlags, ControlError, CrashKind, Environment, HashScope, HostFault, Moment,
        READ_CAP, Reply, Request, SnapId, StopConditions, StopMask, StopReason, VTime,
    };
    use environment::{BitMask, EnvSpec, FaultPolicy, HostFault as EnvHostFault};
    use vmm_backend::{Backend, Exit, MockBackend, Vtime};

    use proptest::prelude::*;

    use super::{ControlServer, ServeError, page_sdk_events, server_caps};
    use crate::vmm::{GuestRam, Vmm, VmmError, VtimeWiring, contract_vclock_config};
    use crate::work::ScriptedWork;

    const RAM: usize = 0x4000; // 16 KiB = 4 pages

    /// A configured, V-time-wired `Vmm<MockBackend>` with a distinctive memory
    /// image loaded and the canonical-blob hash wired (as the box composition
    /// does), advanced to a synchronized (post-RDTSC) boundary.
    fn vmm_at_sync(exits: Vec<Exit>, work: u64, seed: u64) -> Vmm<MockBackend> {
        vmm_at_sync_from(MockBackend::new(), exits, work, seed)
    }

    /// [`vmm_at_sync`] over a caller-prepared mock (e.g. one with dirty tracking
    /// enabled, task 95 M2.1) — the mock's exit script is overwritten.
    fn vmm_at_sync_from(
        mut m: MockBackend,
        exits: Vec<Exit>,
        work: u64,
        seed: u64,
    ) -> Vmm<MockBackend> {
        let mut exits_with_sync = vec![Exit::Rdtsc];
        exits_with_sync.extend(exits);
        m.extend_exits(exits_with_sync);
        m.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
        m.set_msr_filter(&vmm_backend::MsrFilter::default())
            .unwrap();
        let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
        v.wire_vtime(
            VtimeWiring::new(
                contract_vclock_config(),
                Box::new(ScriptedWork::at(work)),
                seed,
            )
            .unwrap(),
        );
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
    fn server(fork_exits: Vec<Exit>) -> ControlServer<MockBackend> {
        let live = vmm_at_sync(vec![Exit::Hlt], 500, 0xBA5E);
        let factory = Box::new(move || {
            let mut m = MockBackend::with_exits(fork_exits.clone());
            m.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
            m.set_msr_filter(&vmm_backend::MsrFilter::default())
                .unwrap();
            let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
            v.wire_vtime(
                VtimeWiring::new(
                    contract_vclock_config(),
                    Box::new(ScriptedWork::at(9_999)),
                    0,
                )
                .unwrap(),
            );
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
        ControlServer::new(live, factory)
    }

    /// [`server`] whose live VM's mock has **dirty tracking armed** (task 95
    /// M2.1), so a second seal can derive from the first.
    fn server_tracked() -> ControlServer<MockBackend> {
        let mut m = MockBackend::new();
        m.enable_dirty_tracking();
        let live = vmm_at_sync_from(m, vec![Exit::Hlt], 500, 0xBA5E);
        // Mirror `server`'s factory (unused by the seal-path tests, present so
        // the composition invariant holds if one branches).
        let factory = Box::new(move || {
            let mut m = MockBackend::with_exits(vec![Exit::Hlt]);
            m.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
            m.set_msr_filter(&vmm_backend::MsrFilter::default())
                .unwrap();
            let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
            v.wire_vtime(
                VtimeWiring::new(
                    contract_vclock_config(),
                    Box::new(ScriptedWork::at(9_999)),
                    0,
                )
                .unwrap(),
            );
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
        ControlServer::new(live, factory)
    }

    /// [`server`] plus a remap-restore factory (task 95 M2.2) mirroring the
    /// memcpy factory's composition — built through the production
    /// `compose_restore_target`, so the portable A/B drives the real seam.
    /// `wire_lapic: false` mis-composes the target on purpose when
    /// `sabotage_lapic` (the RestoreFailed-recovery test's arm).
    fn server_with_remap(
        fork_exits: Vec<Exit>,
        sabotage_lapic: bool,
    ) -> ControlServer<MockBackend> {
        let mut s = server(fork_exits.clone());
        let remap: super::RemapVmmFactory<MockBackend> = Box::new(move |mapping| {
            let m = MockBackend::with_exits(fork_exits.clone());
            let mut v = crate::bringup::compose_restore_target(m, mapping, !sabotage_lapic)?;
            v.wire_vtime(
                VtimeWiring::new(
                    contract_vclock_config(),
                    Box::new(ScriptedWork::at(9_999)),
                    0,
                )
                .unwrap(),
            );
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
            Ok(Reply::SnapId(id)) => id,
            other => panic!("snapshot reply: {other:?}"),
        }
    }

    fn seeded_env(seed: u64) -> Environment {
        let spec = EnvSpec::Seeded {
            seed,
            policy: FaultPolicy::none(),
        };
        Environment {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: spec.encode(),
        }
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

    // ---- task 95 M2: O(dirty) capture + remap restore -------------------------

    /// The store-side chain length of a wire handle (1 = a base layer; >1 = a
    /// derived capture) — how these tests observe which capture path a seal took.
    fn chain_len(s: &ControlServer<MockBackend>, id: SnapId) -> u32 {
        s.engine.stats(s.snaps[&id.0]).unwrap().chain_len
    }

    /// M2.1 wiring: with tracking armed, the second seal derives from the first
    /// (chain 2), the harvested set covers a host-side write, and the derived
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
            .apply_host_fault(&EnvHostFault::CorruptMemory {
                gpa: 0x40,
                mask: BitMask(0xDEAD_BEEF),
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

    /// The safety default: without backend dirty tracking every seal full-scans
    /// (base layers throughout) — nothing ever derives on an unvouched window.
    #[test]
    fn untracked_seals_always_full_scan() {
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        let first = snap(&mut s);
        let second = snap(&mut s);
        assert_eq!(chain_len(&s, first), 1);
        assert_eq!(chain_len(&s, second), 1, "no tracking ⇒ base, never derive");
    }

    /// The chain bound (M2.1): at `max_chain_len` the seal flattens via a fresh
    /// base instead of deriving deeper.
    #[test]
    fn seal_flattens_at_the_chain_bound() {
        let mut s = server_tracked();
        s.set_max_chain_len(2);
        hello(&mut s);
        let a = snap(&mut s);
        let b = snap(&mut s);
        let c = snap(&mut s);
        assert_eq!(chain_len(&s, a), 1);
        assert_eq!(chain_len(&s, b), 2, "under the bound: derive");
        assert_eq!(chain_len(&s, c), 1, "at the bound: flatten to a base");
    }

    /// A released parent (the client dropped the handle) makes the next seal
    /// fall back to a base — the parent-liveness check of the safety rule.
    #[test]
    fn seal_falls_back_to_base_when_the_parent_was_dropped() {
        let mut s = server_tracked();
        hello(&mut s);
        let first = snap(&mut s);
        assert_eq!(s.handle(&Request::Drop(first)).unwrap(), Ok(Reply::Unit));
        let second = snap(&mut s);
        assert_eq!(chain_len(&s, second), 1, "dead parent ⇒ full scan");
    }

    /// An untrackable full-image host write between seals forces the fallback
    /// (the wholesale poison), and the state still captures correctly.
    #[test]
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
        let mut s = server_with_remap(vec![Exit::Rdtsc, Exit::Hlt], false);
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
        let mut s = server_with_remap(vec![Exit::Hlt], true); // sabotaged target
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
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        let caps = server_caps();
        assert_eq!(caps.protocol_version, control_proto::APP_PROTOCOL_VERSION);
        assert_eq!(
            caps.protocol_version, 6,
            "task 81 bumped for the exec/recorded_env improvisation verbs"
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

    /// The `SdkEvents` verb (task 73) is routed to the live VM's capture — a
    /// mock guest that never rings the doorbell yields an empty `SdkEvents` reply
    /// (not `Unsupported`), so a remote client always gets the capture over the
    /// wire.
    #[test]
    fn sdk_events_verb_is_routed_to_the_capture() {
        let mut s = server(vec![Exit::Hlt]);
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
        let mut s = server(vec![Exit::Hlt]);
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
        let mut s = server(vec![Exit::Hlt]);
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
        let mut s = server(vec![Exit::Hlt]);
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
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        let v = regs(&mut s);
        assert_eq!(v.version, control_proto::RegsView::VERSION);
        let vns = s.vmm().unwrap().effective_vns().unwrap();
        assert_eq!(v.moment.0, vns, "moment is the current V-time");
        assert_eq!(v.vtime, vns, "vtime and moment coincide on the single axis");
        assert_eq!(v.moment.0, 500, "the fixture is wired at work-count 500");
    }

    /// Both observation verbs are subject to the `hello`-first gate — before a
    /// session is negotiated nothing is supported.
    #[test]
    fn observations_before_hello_are_unsupported() {
        let mut s = server(vec![Exit::Hlt]);
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
    fn read_and_regs_on_a_poisoned_server_are_session_fatal() {
        // A factory that cannot boot poisons the session on the first branch: the
        // live VM is dropped, the factory fails, and `vmm` stays `None`.
        let live = vmm_at_sync(vec![Exit::Hlt], 500, 0xBA5E);
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
    fn observations_do_not_perturb_hash_or_recorded_env() {
        let mut s = server(vec![Exit::Hlt]);
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
        let mut s = server(vec![Exit::Hlt]);
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
        /// stripped — the RESOLUTION.md search-surface criterion: observation, not a
        /// move. Reads that are out of range / over-cap (loud errors) are included,
        /// so even a *rejected* observation is proven inert.
        #[test]
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
        let mut s = server(vec![Exit::Hlt]);
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
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        let a = snap(&mut s);
        let b = snap(&mut s);
        assert_ne!(a, b, "handles are pool-wide and never reused");
        assert_eq!(s.handle(&Request::Drop(a)).unwrap(), Ok(Reply::Unit));
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
        assert_eq!(s.handle(&Request::Drop(b)).unwrap(), Ok(Reply::Unit));
    }

    /// A **replay** restores the buggify policy captured with the SDK snapshot
    /// (P1). The restore path resets the recorded reproducer to `none()`, so
    /// without capturing the policy a replay would materialize an SDK env whose
    /// buggify draws all-`Nominal` — the restored stream position would then not
    /// reproduce. Branch with a firing buggify policy → snapshot → replay → the
    /// reproducer's policy must survive (not be wiped to `none`).
    #[test]
    fn replay_restores_the_buggify_policy() {
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        let base = snap(&mut s);

        // Branch with a buggify-only policy (point 1 always fires).
        let mut policy = FaultPolicy::none();
        policy.set_buggify_point(1, 1, 1).unwrap();
        let env = Environment {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: EnvSpec::Seeded {
                seed: 5,
                policy: policy.clone(),
            }
            .encode(),
        };
        assert_eq!(
            s.handle(&Request::Branch { snap: base, env }).unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(
            s.recorded_env().policy(),
            &policy,
            "the branch carries the buggify policy"
        );

        // Snapshot the SDK-policy state, then replay it.
        let mid = snap(&mut s);
        assert_eq!(s.handle(&Request::Replay(mid)).unwrap(), Ok(Reply::Unit));

        // Without the P1 fix, the replay's reproducer policy would be `none()`;
        // with it, the snapshot's buggify policy is restored so the run reproduces.
        assert_eq!(
            s.recorded_env().policy(),
            &policy,
            "replay restored the buggify policy, not none()"
        );
        assert_ne!(s.recorded_env().policy(), &FaultPolicy::none());
    }

    #[test]
    fn branch_validates_the_env_before_touching_the_vm() {
        let mut s = server(vec![Exit::Hlt]);
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
        let env = Environment {
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
    fn branch_accepts_host_overrides_but_rejects_guest_or_standing_or_policy() {
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        let base = snap(&mut s);
        // A HOST override-carrying env is now ENFORCED (task 59): branch accepts it
        // and stages the fault for `run` to apply — no longer Unsupported.
        let mut spec = EnvSpec::Seeded {
            seed: 7,
            policy: FaultPolicy::none(),
        };
        spec.record(
            1234,
            environment::Action::Host(environment::HostFault::InjectInterrupt { vector: 32 }),
        );
        let env = Environment {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: spec.encode(),
        };
        assert_eq!(
            s.handle(&Request::Branch { snap: base, env }).unwrap(),
            Ok(Reply::Unit),
            "a host override is enforced (task 59), so branch accepts it"
        );

        // A GUEST override still needs the task-61 decide-seam loop → Unsupported.
        let mut guest_spec = EnvSpec::Seeded {
            seed: 7,
            policy: FaultPolicy::none(),
        };
        guest_spec.record(99, environment::Action::Guest(environment::Answer::Nominal));
        let guest_env = Environment {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: guest_spec.encode(),
        };
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: guest_env
            })
            .unwrap(),
            Err(ControlError::Unsupported),
            "a guest override needs the task-61 enforcement loop"
        );

        // A non-nominal FaultPolicy (same class): the seeded stream would answer
        // some decisions with faults, which no service is wired to enforce yet.
        let mut policy = FaultPolicy::none();
        policy
            .set_class(
                environment::DecisionClass::BlockIo,
                1,
                2,
                &[environment::Fault::BlockEio],
            )
            .unwrap();
        let faulting = Environment {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: EnvSpec::Seeded { seed: 7, policy }.encode(),
        };
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: faulting
            })
            .unwrap(),
            Err(ControlError::Unsupported),
            "a non-none fault policy is unenforceable until task 59/61"
        );
    }

    #[test]
    fn run_resolve_is_always_resolve_without_decision() {
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        let req = Request::Run {
            until: StopConditions {
                deadline: None,
                on: StopMask::NONE,
            },
            resolve: Some(Answer(vec![1, 2, 3])),
        };
        assert_eq!(
            s.handle(&req).unwrap(),
            Err(ControlError::ResolveWithoutDecision)
        );
    }

    #[test]
    fn hash_whole_matches_the_vmm_and_other_scopes_are_unsupported() {
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        let h = hash(&mut s);
        assert_eq!(Some(h), s.vmm().map(|v| v.state_hash()));
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
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        let perturb = |fault: environment::HostFault, at: u64| Request::Perturb {
            fault: HostFault(fault.encode()),
            at: Moment(at),
        };
        // An InjectInterrupt at a future Moment stages cleanly (Reply::Unit).
        assert_eq!(
            s.handle(&perturb(
                environment::HostFault::InjectInterrupt { vector: 32 },
                1000
            ))
            .unwrap(),
            Ok(Reply::Unit)
        );
        // A second fault at the SAME Moment is a loud rejection (task-45
        // one-action-per-Moment; the recorded env never silently drops a fault).
        assert_eq!(
            s.handle(&perturb(
                environment::HostFault::InjectInterrupt { vector: 33 },
                1000
            ))
            .unwrap(),
            Err(ControlError::PerturbMomentTaken { at: 1000 })
        );
        // A Moment BEHIND the current V-time (500) is rejected loud — it could only
        // apply later than recorded.
        assert_eq!(
            s.handle(&perturb(
                environment::HostFault::InjectInterrupt { vector: 34 },
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
                environment::HostFault::InjectInterrupt { vector: 35 },
                500
            ))
            .unwrap(),
            Ok(Reply::Unit)
        );
        // A CorruptMemory whose word falls outside the 16 KiB guest RAM is rejected
        // loudly at stage time (never clipped/wrapped).
        assert_eq!(
            s.handle(&perturb(
                environment::HostFault::CorruptMemory {
                    gpa: RAM as u64 - 4, // gpa + 8 > ram
                    mask: environment::BitMask(0xFF),
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
                environment::HostFault::CorruptMemory {
                    gpa: 0,
                    mask: environment::BitMask(0xFF),
                },
                3000
            ))
            .unwrap(),
            Ok(Reply::Unit)
        );
        // The out-of-scope clock faults are Unsupported (a follow-on lights them up).
        assert_eq!(
            s.handle(&perturb(
                environment::HostFault::SkewTime(environment::VTime(5)),
                4000
            ))
            .unwrap(),
            Err(ControlError::Unsupported)
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

    #[test]
    fn run_maps_terminals_workload_blind() {
        // Hlt → Quiescent.
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        assert!(matches!(run_all(&mut s), StopReason::Quiescent { .. }));

        // Shutdown → Crash{Shutdown}.
        let mut s = server(vec![Exit::Shutdown]);
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
        let dbg = |code: u32| Exit::Io {
            port: 0xF4,
            size: 1,
            write: Some(code),
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
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        let req = Request::Run {
            until: StopConditions {
                deadline: Some(VTime(500)),
                on: StopMask::NONE,
            },
            resolve: None,
        };
        match s.handle(&req).unwrap() {
            Ok(Reply::Stop(StopReason::Deadline { vtime })) => assert_eq!(vtime, VTime(500)),
            other => panic!("expected Deadline{{500}}, got {other:?}"),
        }
        // And the pending scripted exit was NOT consumed: a deadline-free run
        // still reaches its Hlt terminal.
        assert!(matches!(run_all(&mut s), StopReason::Quiescent { .. }));
    }

    #[test]
    fn branch_reseeds_and_replay_does_not() {
        // Fork VMs take one RDTSC (to a synchronized point) then halt.
        let mut s = server(vec![Exit::Rdtsc, Exit::Hlt]);
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
    fn branch_run_hash_is_deterministic_per_seed_end_to_end() {
        // The portable determinism shape of the box gate: branch(s, seed) →
        // run → hash, twice per seed, over fork VMs that draw entropy (RDRAND)
        // so the seed actually reaches the run.
        let fork_script = vec![
            Exit::Rdtsc,
            Exit::Rdrand { width: 8 },
            Exit::Rdrand { width: 8 },
            Exit::Hlt,
        ];
        let mut s = server(fork_script);
        hello(&mut s);
        let base = snap(&mut s);
        let mut run_hash = |seed: u64| -> (StopReason, [u8; 32]) {
            s.handle(&Request::Branch {
                snap: base,
                env: seeded_env(seed),
            })
            .unwrap()
            .unwrap();
            let stop = run_all(&mut s);
            (stop, hash(&mut s))
        };
        let (stop_a1, h_a1) = run_hash(0xAAAA);
        let (stop_b1, h_b1) = run_hash(0xBBBB);
        let (stop_a2, h_a2) = run_hash(0xAAAA);
        assert_eq!(stop_a1, stop_a2, "same seed ⇒ same stop");
        assert_eq!(h_a1, h_a2, "same seed ⇒ bit-identical terminal hash");
        assert_eq!(stop_a1, stop_b1, "stop kind is seed-independent here");
        assert_ne!(h_a1, h_b1, "distinct seeds diverge");
    }

    #[test]
    fn snapshot_at_an_unsynchronized_point_is_not_quiescent() {
        // Drive the live VM past a NON-vtime exit (a serial write): the point
        // is not V-time-synchronized, so save_vm_state fails closed and the
        // verb answers NotQuiescent (the caller may run further and retry).
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        // Consume the live VM's scripted Hlt → terminal, which is fine — but
        // first push a serial OUT through a fresh branch? Simpler: build a
        // dedicated server whose live VM sits at a serial-write exit.
        let mut m = MockBackend::with_exits(vec![
            Exit::Rdtsc,
            Exit::Io {
                port: 0x3F8,
                size: 1,
                write: Some(b'x' as u32),
            },
            Exit::Hlt,
        ]);
        m.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
        m.set_msr_filter(&vmm_backend::MsrFilter::default())
            .unwrap();
        let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
        v.wire_vtime(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(1)), 3).unwrap(),
        );
        v.step().unwrap(); // RDTSC → synchronized
        v.step().unwrap(); // serial OUT → NOT synchronized
        let mut s = ControlServer::new(
            v,
            Box::new(|| {
                Err(VmmError::ContractViolation(
                    "factory unused in this test".into(),
                ))
            }),
        );
        hello(&mut s);
        assert_eq!(
            s.handle(&Request::Snapshot).unwrap(),
            Err(ControlError::NotQuiescent)
        );
    }

    #[test]
    fn a_failed_factory_is_session_fatal_and_poisons_the_server() {
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        let base = snap(&mut s);
        // Swap in a failing factory by rebuilding the server around the same
        // live VM shape: simplest is a dedicated server.
        let live = vmm_at_sync(vec![Exit::Hlt], 500, 0xBA5E);
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
    fn restore_validation_rejection_is_recoverable_and_keeps_the_fresh_vm() {
        // The recoverable half of restore's error split (round 4): a
        // **validation-class** rejection — here a V-time wiring mismatch (the
        // live VM is V-time-wired, so the snapshot carries a V-time block, but
        // the factory boots forks WITHOUT V-time) — is caught *before* the fresh
        // VM mutates, so it answers the recoverable `RestoreFailed` and KEEPS the
        // intact fresh VM (the session stays usable).
        let live = vmm_at_sync(vec![Exit::Hlt], 500, 0xBA5E); // V-time wired
        let factory = Box::new(|| {
            // A fork with NO V-time wired → restoring a V-time-bearing blob into
            // it is a ContractViolation (wiring must match the snapshot source).
            let mut m = MockBackend::with_exits(vec![Exit::Hlt]);
            m.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
            m.set_msr_filter(&vmm_backend::MsrFilter::default())
                .unwrap();
            Ok(Vmm::new(m, GuestRam::new(RAM).unwrap()))
        });
        let mut s = ControlServer::new(live, factory);
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
        fn set_cpuid(&mut self, m: &vmm_backend::CpuidModel) -> vmm_backend::Result<()> {
            self.0.set_cpuid(m)
        }
        fn set_msr_filter(&mut self, f: &vmm_backend::MsrFilter) -> vmm_backend::Result<()> {
            self.0.set_msr_filter(f)
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
        fn run(&mut self) -> vmm_backend::Result<Exit> {
            self.0.run()
        }
        fn run_until(&mut self, d: vmm_backend::Vtime) -> vmm_backend::Result<Exit> {
            self.0.run_until(d)
        }
        fn inject(&mut self, e: vmm_backend::Event) -> vmm_backend::Result<()> {
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
        fn complete_cpuid(&mut self, a: u32, b: u32, c: u32, d: u32) -> vmm_backend::Result<()> {
            self.0.complete_cpuid(a, b, c, d)
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
        fn capabilities(&self) -> vmm_backend::Capabilities {
            self.0.capabilities()
        }
    }

    #[test]
    fn restore_substrate_failure_is_session_fatal_and_poisons_the_server() {
        // The fatal half of restore's error split (round 4): a failure AFTER
        // validation — here `Backend::restore` itself faults — is substrate
        // breakage (the fresh VM's state can no longer be vouched for), so it
        // tears the session down (ServeError) rather than answering the
        // recoverable RestoreFailed and letting a client run from unvouched state.
        let build = || -> Vmm<RestoreFailBackend> {
            let mut m = MockBackend::with_exits(vec![Exit::Rdtsc, Exit::Hlt]);
            m.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
            m.set_msr_filter(&vmm_backend::MsrFilter::default())
                .unwrap();
            let mut v = Vmm::new(RestoreFailBackend(m), GuestRam::new(RAM).unwrap());
            v.wire_vtime(
                VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(500)), 1)
                    .unwrap(),
            );
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
            Ok(Reply::SnapId(id)) => id,
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
    fn serve_speaks_frames_over_an_in_memory_stream() {
        // A tiny wire-level session over a socketpair: hello → snapshot →
        // hash → EOF. The server stays on this thread (a `Vmm` is not `Send` —
        // its work source is a thread-affine counter on the box); the client
        // runs on a spawned thread, exactly the composition the demo bin uses.
        // The full loopback (socket Machine end-to-end) lives in
        // dissonance/conductor.
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
            assert!(matches!(reply, Ok(Reply::SnapId(_))));
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
        let mut s = server(vec![Exit::Hlt]);
        s.serve(server_end).unwrap();
        client_thread.join().unwrap();
    }

    // -----------------------------------------------------------------------
    // Task 59 — host-plane enforcement (apply a HostFault at a Moment).
    //
    // Portable proof over the scripted `MockBackend`: each staged `Moment`
    // arrives via `run_until` (the mock rewrites a scripted `Exit::Deadline`'s
    // `reached` to the work count the VMM armed — `arm_arrival`'s
    // `work_for_vns(moment)`, which under the 1 ns/branch contract clock is the
    // Moment itself). One scripted `Deadline` per *distinct* Moment, then a
    // terminal `Hlt`. The end-to-end record→replay-on-real-KVM closure is the
    // box gate; here we prove the apply-at-Moment seam, its determinism, and
    // that the emitted recorded env re-applies to the identical hash.
    // -----------------------------------------------------------------------

    /// A live VM for enforcement: V-time + userspace-LAPIC + snapshot-hashing
    /// wired, a distinctive RAM image loaded, and a script of `deadlines`
    /// arrival placeholders followed by a terminal `Hlt`. `ScriptedWork::at(0)`
    /// so every armed arrival (`work_for_vns(moment) = moment ≥ 1`) is a real
    /// forward entry.
    fn enforce_vmm(deadlines: usize, image: [u8; RAM], seed: u64) -> Vmm<MockBackend> {
        let mut exits = vec![Exit::Deadline { reached: Vtime(0) }; deadlines];
        exits.push(Exit::Hlt);
        let mut m = MockBackend::with_exits(exits);
        m.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
        m.set_msr_filter(&vmm_backend::MsrFilter::default())
            .unwrap();
        let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
        v.wire_vtime(
            VtimeWiring::new(
                contract_vclock_config(),
                Box::new(ScriptedWork::at(0)),
                seed,
            )
            .unwrap(),
        );
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

    /// The number of distinct `Moment`s in a schedule (one arrival — one scripted
    /// `Deadline` — per distinct `Moment`; faults sharing a `Moment` share one).
    fn distinct_moments(schedule: &[(u64, EnvHostFault)]) -> usize {
        let mut ms: Vec<u64> = schedule.iter().map(|(m, _)| *m).collect();
        ms.sort_unstable();
        ms.dedup();
        ms.len()
    }

    /// Build a server, stage `schedule` via `perturb`, run to terminal, and
    /// return `(state_hash, recorded_env)`. The factory is unused (no
    /// branch/replay in these direct tests), so it errors loudly if ever called.
    fn enforce_run(schedule: &[(u64, EnvHostFault)], seed: u64) -> ([u8; 32], EnvSpec) {
        let live = enforce_vmm(distinct_moments(schedule), enforce_image(), seed);
        let factory = Box::new(|| {
            Err(VmmError::ContractViolation(
                "factory unused in a direct enforcement run".into(),
            ))
        });
        let mut s = ControlServer::new(live, factory);
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

    fn enforce_hash(schedule: &[(u64, EnvHostFault)], seed: u64) -> [u8; 32] {
        enforce_run(schedule, seed).0
    }

    #[test]
    fn same_schedule_run_twice_is_bit_identical_and_control_differs() {
        // Gate 1: an arbitrary schedule applied twice is bit-identical; and an
        // ABSENT schedule (the control) differs — the faults are actually landing.
        let schedule = vec![
            (
                100,
                EnvHostFault::CorruptMemory {
                    gpa: 0x40,
                    mask: BitMask(0xDEAD_BEEF_0000_0001),
                },
            ),
            (250, EnvHostFault::InjectInterrupt { vector: 0x40 }),
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
        let fault = EnvHostFault::CorruptMemory {
            gpa: gpa as u64,
            mask: BitMask(mask),
        };
        let live = enforce_vmm(1, enforce_image(), 7);
        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = ControlServer::new(live, factory);
        hello(&mut s);
        s.handle(&Request::Perturb {
            fault: HostFault(fault.encode()),
            at: Moment(42),
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
        // Round-5 P1: a host fault scheduled EXACTLY at the deferred `setup_complete`
        // boundary must be drained (applied + removed from the schedule) BEFORE the
        // snapshot point surfaces — else the explorer's eager seal there hits
        // `SnapshotWhileArmed` (a non-empty schedule). A guest rings `setup_complete`
        // (unsealable OUT) then lands an arrival at Moment M; a `CorruptMemory` is
        // staged at M; the run surfaces `SnapshotPoint` with the schedule drained, so
        // a `snapshot()` there SUCCEEDS.
        const REQ_GPA: usize = 0xE000;
        const BIG_RAM: usize = 0x2_0000; // holds the doorbell pages at 0xE000/0xF000
        let m: u64 = 4_000;

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
            Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(n as u32),
            },
            Exit::Deadline { reached: Vtime(0) },
            Exit::Hlt,
        ]);
        mb.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
        mb.set_msr_filter(&vmm_backend::MsrFilter::default())
            .unwrap();
        let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
        live.wire_vtime(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(0)), 9).unwrap(),
        );
        live.wire_snapshot_hashing();
        let mut ram = vec![0u8; BIG_RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        live.restore_guest_memory(&ram).unwrap();

        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = ControlServer::new(live, factory);
        hello(&mut s);

        // Stage a CorruptMemory fault EXACTLY at the deferred boundary Moment M.
        let fault = EnvHostFault::CorruptMemory {
            gpa: 0x1000,
            mask: BitMask(0xDEAD_BEEF),
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

        // The schedule was drained, so the eager seal SUCCEEDS (not SnapshotWhileArmed).
        match s.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::SnapId(_)) => {}
            other => {
                panic!("seal at the deferred boundary failed (SnapshotWhileArmed?): {other:?}")
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
    fn a_future_fault_keeps_deferring_the_snapshot_point_until_the_schedule_drains() {
        // Round-6 P2(1): the deferred `setup_complete` point must NOT surface while a
        // FUTURE fault (m > vns) is still staged — `snapshot()` rejects any non-empty
        // schedule (`SnapshotWhileArmed`), so the advertised seal would fail. Ring
        // `setup_complete`, then hit a synchronized RDTSC boundary (vns 0, the fault
        // still future) where round-5 would surface early, then land the fault's
        // arrival at M. The point surfaces ONLY at M, once the schedule has drained to
        // empty — and a `snapshot()` there SUCCEEDS.
        const REQ_GPA: usize = 0xE000;
        const BIG_RAM: usize = 0x2_0000;
        let m: u64 = 4_000;

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
            Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(n as u32),
            },
            Exit::Rdtsc,
            Exit::Deadline { reached: Vtime(0) },
            Exit::Hlt,
        ]);
        mb.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
        mb.set_msr_filter(&vmm_backend::MsrFilter::default())
            .unwrap();
        let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
        live.wire_vtime(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(0)), 9).unwrap(),
        );
        live.wire_snapshot_hashing();
        let mut ram = vec![0u8; BIG_RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        live.restore_guest_memory(&ram).unwrap();

        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = ControlServer::new(live, factory);
        hello(&mut s);

        // A FUTURE fault at M (> the RDTSC boundary's vns of 0).
        let fault = EnvHostFault::CorruptMemory {
            gpa: 0x1000,
            mask: BitMask(0x00C0_FFEE),
        };
        s.handle(&Request::Perturb {
            fault: HostFault(fault.encode()),
            at: Moment(m),
        })
        .unwrap()
        .unwrap();

        // The point surfaces only after the schedule drains (at M), NOT at the RDTSC
        // (arm the SNAPSHOT_POINT class — round-7 gates it on the mask).
        match run_seeking_snapshot(&mut s) {
            StopReason::SnapshotPoint { vtime } => assert_eq!(
                vtime.0, m,
                "surfaced after the future fault drained, not at the earlier RDTSC"
            ),
            other => panic!("expected a deferred SnapshotPoint, got {other:?}"),
        }
        // The schedule is empty, so the eager seal SUCCEEDS (not SnapshotWhileArmed).
        match s.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::SnapId(_)) => {}
            other => panic!("seal failed with a future fault mishandled: {other:?}"),
        }
    }

    #[test]
    fn a_future_reseed_keeps_deferring_the_snapshot_point_until_it_drains() {
        // Round-9 P1 (task 78 seam): the deferred `setup_complete` point must NOT
        // surface while a FUTURE reseed (m > vns) is still staged — `snapshot()`
        // rejects a non-empty `reseed_schedule` too (`SnapshotWhileArmed`, mirrored
        // at control.rs:604), so surfacing early would advertise a seal that fails,
        // or seal a point missing its pending reseeds (replay diverges). The reseed
        // is the exact analogue of the future-fault case above. Ring
        // `setup_complete`, hit a synchronized RDTSC boundary (vns 0, the reseed
        // still future) where the old gate would surface early, then land the
        // reseed's arrival at M — the point surfaces ONLY at M, and a seal SUCCEEDS.
        const REQ_GPA: usize = 0xE000;
        const BIG_RAM: usize = 0x2_0000;
        let m: u64 = 4_000;

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
            Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(n as u32),
            },
            Exit::Rdtsc,
            Exit::Deadline { reached: Vtime(0) },
            Exit::Hlt,
        ]);
        mb.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
        mb.set_msr_filter(&vmm_backend::MsrFilter::default())
            .unwrap();
        let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
        live.wire_vtime(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(0)), 9).unwrap(),
        );
        live.wire_snapshot_hashing();
        let mut ram = vec![0u8; BIG_RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        live.restore_guest_memory(&ram).unwrap();

        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = ControlServer::new(live, factory);
        hello(&mut s);

        // Stage a FUTURE reseed at M. No public verb stages a reseed without a
        // branch env, so insert directly — the test module shares the crate.
        s.reseed_schedule.insert(m, 9);

        // The point surfaces only after the reseed drains (at M), NOT at the RDTSC
        // (vns 0), proving the gate honors `reseed_schedule` (arm SNAPSHOT_POINT).
        match run_seeking_snapshot(&mut s) {
            StopReason::SnapshotPoint { vtime } => assert_eq!(
                vtime.0, m,
                "surfaced after the future reseed drained, not at the earlier RDTSC"
            ),
            other => panic!("expected a deferred SnapshotPoint, got {other:?}"),
        }
        // The reseed drained, so the eager seal SUCCEEDS (not SnapshotWhileArmed).
        match s.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::SnapId(_)) => {}
            other => panic!("seal failed with a staged reseed mishandled: {other:?}"),
        }
    }

    #[test]
    fn snapshot_point_defers_past_an_rng_boundary_to_the_next_clean_seal() {
        // Round-4 P1: the first V-time-synchronized exit after `setup_complete` is
        // an RDRAND — synchronized, but with a STAGED RNG completion, so
        // `save_vm_state` fails closed there. The deferred snapshot point must NOT
        // surface at that RNG boundary (gating on `is_synchronized()` alone did, and
        // cleared `pending_snapshot` before the failed seal → the point was LOST);
        // it must defer to the next CLEAN synchronized boundary (the RDTSC), where a
        // seal succeeds. Exits: doorbell(setup_complete) → RDRAND (unsealable) →
        // RDTSC (clean) → HLT.
        const REQ_GPA: usize = 0xE000;
        const BIG_RAM: usize = 0x2_0000;

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

        let mut mb = MockBackend::with_exits(vec![
            Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(n as u32),
            },
            Exit::Rdrand { width: 8 }, // synchronized BUT rng_completion_staged
            Exit::Rdtsc,               // the next clean, sealable boundary
            Exit::Hlt,
        ]);
        mb.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
        mb.set_msr_filter(&vmm_backend::MsrFilter::default())
            .unwrap();
        let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
        live.wire_vtime(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(0)), 9).unwrap(),
        );
        live.wire_snapshot_hashing();
        let mut ram = vec![0u8; BIG_RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        live.restore_guest_memory(&ram).unwrap();

        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = ControlServer::new(live, factory);
        hello(&mut s);

        // The point surfaces (only at the clean RDTSC — never at the RDRAND, which
        // `save_vm_state` would reject), so the eager seal SUCCEEDS.
        assert!(
            matches!(
                run_seeking_snapshot(&mut s),
                StopReason::SnapshotPoint { .. }
            ),
            "the deferred point surfaced at a sealable boundary"
        );
        match s.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::SnapId(_)) => {}
            other => panic!(
                "seal failed — the point surfaced at an unsealable (RNG) boundary: {other:?}"
            ),
        }
    }

    #[test]
    fn an_sdk_stop_with_a_staged_reseed_poisons_loud() {
        // Round-9 P1 (task 78 seam): an SDK stop (an assert violation at a doorbell
        // OUT) is NOT a V-time intercept, so `effective_vns` there is only a lower
        // bound — a staged reseed at-or-above it may already be crossed (the guest
        // ran past its Moment in the skid window). Poison loud — the same
        // ScheduleUnsatisfiable class as a crossed fault, mirroring the terminal
        // arm — else a later replay from the reseed re-derives a different stream.
        const REQ_GPA: usize = 0xE000;
        const BIG_RAM: usize = 0x2_0000;

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
            Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(n as u32),
            },
            Exit::Hlt,
        ]);
        mb.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
        mb.set_msr_filter(&vmm_backend::MsrFilter::default())
            .unwrap();
        let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
        live.wire_vtime(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(0)), 9).unwrap(),
        );
        live.wire_snapshot_hashing();
        let mut ram = vec![0u8; BIG_RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        live.restore_guest_memory(&ram).unwrap();

        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = ControlServer::new(live, factory);
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
        const BIG_RAM: usize = 0x2_0000;

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
        let run_with = |rest: Vec<Exit>, frame: &[u8], on: StopMask| -> StopReason {
            let mut script = vec![Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(frame.len() as u32),
            }];
            script.extend(rest);
            let mut mb = MockBackend::with_exits(script);
            mb.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
            mb.set_msr_filter(&vmm_backend::MsrFilter::default())
                .unwrap();
            let mut live = Vmm::new(mb, GuestRam::new(BIG_RAM).unwrap());
            live.wire_vtime(
                VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(0)), 3)
                    .unwrap(),
            );
            live.wire_snapshot_hashing();
            let mut ram = vec![0u8; BIG_RAM];
            ram[REQ_GPA..REQ_GPA + frame.len()].copy_from_slice(frame);
            live.restore_guest_memory(&ram).unwrap();
            let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
            let mut s = ControlServer::new(live, factory);
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
                run_with(vec![Exit::Rdtsc, Exit::Hlt], &setup, StopMask::NONE),
                StopReason::Quiescent { .. }
            ),
            "StopMask::NONE runs through setup_complete straight to the terminal"
        );
        assert!(
            matches!(
                run_with(
                    vec![Exit::Rdtsc, Exit::Hlt],
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
                run_with(vec![Exit::Hlt], &viol, StopMask::NONE),
                StopReason::Quiescent { .. }
            ),
            "StopMask::NONE runs through an assertion straight to the terminal"
        );
        assert!(
            matches!(
                run_with(
                    vec![Exit::Hlt],
                    &viol,
                    StopMask::NONE.arm(control_proto::class_bit::ASSERTION)
                ),
                StopReason::Assertion { .. }
            ),
            "arming ASSERTION surfaces the assertion"
        );
    }

    #[test]
    fn arrival_lands_at_the_exact_moment_and_in_order() {
        // The run arrives at each Moment via run_until: effective V-time equals the
        // Moment when its fault is applied, and later Moments apply strictly after
        // earlier ones. Prove it by staging CorruptMemory at ascending Moments and
        // reading the terminal effective V-time (= the last Moment reached).
        let schedule = vec![
            (
                1_000,
                EnvHostFault::CorruptMemory {
                    gpa: 0x40,
                    mask: BitMask(1),
                },
            ),
            (
                5_000,
                EnvHostFault::CorruptMemory {
                    gpa: 0x48,
                    mask: BitMask(2),
                },
            ),
        ];
        let live = enforce_vmm(distinct_moments(&schedule), enforce_image(), 1);
        let factory = Box::new(|| Err(VmmError::ContractViolation("unused".into())));
        let mut s = ControlServer::new(live, factory);
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
        // The last arrival advanced effective V-time to the final Moment (5_000).
        assert_eq!(
            s.vmm().unwrap().effective_vns(),
            Some(5_000),
            "the run arrived at the last staged Moment"
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
        let mut s = ControlServer::new(live, factory);
        hello(&mut s);
        let stage = |f: EnvHostFault, m: u64| Request::Perturb {
            fault: HostFault(f.encode()),
            at: Moment(m),
        };
        assert_eq!(
            s.handle(&stage(
                EnvHostFault::CorruptMemory {
                    gpa: 0x40,
                    mask: BitMask(0x0F0F_0F0F),
                },
                300,
            ))
            .unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(
            s.handle(&stage(EnvHostFault::InjectInterrupt { vector: 0x50 }, 300))
                .unwrap(),
            Err(ControlError::PerturbMomentTaken { at: 300 }),
            "a second fault at Moment 300 is rejected (not silently dropped)"
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
            s.recorded_env().host_faults().count(),
            1,
            "exactly the accepted fault is recorded"
        );
    }

    #[test]
    fn recorded_env_replays_to_the_same_hash() {
        // Task 59 requirement 3 (portable form of the box gate's record→replay
        // closure): every applied fault is stamped into the recorded env, and
        // re-applying that env's host faults reproduces the run's state_hash.
        let schedule = vec![
            (
                150,
                EnvHostFault::CorruptMemory {
                    gpa: 0x20,
                    mask: BitMask(0x1234_5678_9ABC_DEF0),
                },
            ),
            (400, EnvHostFault::InjectInterrupt { vector: 0x60 }),
        ];
        let seed = 0xC105u64;
        let (h1, recorded) = enforce_run(&schedule, seed);

        // The recorded env carries both host faults on the Moment axis, and it
        // round-trips through its own byte codec (a real reproducer blob).
        let host: Vec<_> = recorded.host_faults().collect();
        assert_eq!(host.len(), 2, "both applied faults were stamped");
        let reencoded = EnvSpec::decode(&recorded.encode()).expect("recorded env round-trips");
        let replay_schedule: Vec<(u64, EnvHostFault)> = reencoded.host_faults().collect();

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
        let mut m = MockBackend::with_exits(vec![Exit::Rdtsc, Exit::Hlt]);
        m.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
        m.set_msr_filter(&vmm_backend::MsrFilter::default())
            .unwrap();
        let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
        v.wire_vtime(
            VtimeWiring::new(
                contract_vclock_config(),
                Box::new(ScriptedWork::at(rdtsc_work)),
                1,
            )
            .unwrap(),
        );
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
                EnvHostFault::CorruptMemory {
                    gpa: 0x40,
                    mask: BitMask(0xFFFF_FFFF),
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
                deadline: Some(VTime(deadline)),
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
            Ok(Reply::Stop(StopReason::Deadline { vtime })) => assert_eq!(vtime, VTime(1000)),
            other => panic!("expected Deadline, got {other:?}"),
        }
        assert_eq!(
            &s.vmm().unwrap().guest_memory()[0x40..0x48],
            &[0u8; 8],
            "the beyond-deadline fault did not apply"
        );
        assert_eq!(
            s.recorded_env().host_faults().count(),
            0,
            "nothing recorded — the fault is still staged for a later run"
        );
    }

    #[test]
    fn overshooting_a_staged_moment_fails_loud() {
        // PR #51 round-2 finding 1: a fault beyond the deadline that the run
        // EXECUTES PAST (RDTSC lands at 2000, deadline 1000, fault at 1500) can never
        // be applied at its recorded count — the run fails loud with
        // `ScheduleUnsatisfiable` rather than carry the crossed Moment forward for a
        // later run to apply from the past.
        let mut s = rdtsc_then_hlt_server(2000);
        hello(&mut s);
        stage_corrupt(&mut s, 1500);
        assert_eq!(
            run_with_deadline(&mut s, 1000),
            Err(ControlError::ScheduleUnsatisfiable {
                moment: 1500,
                vtime: 2000,
            }),
        );
        // The fault did NOT apply (recorded-apply-point integrity: never applied at
        // the wrong count).
        assert_eq!(
            &s.vmm().unwrap().guest_memory()[0x40..0x48],
            &[0u8; 8],
            "the crossed fault must not have applied"
        );
    }

    #[test]
    fn schedule_poison_persists_until_a_rewind() {
        // PR #51 round-3 item 1: after a crossed Moment poisons the schedule, a
        // re-sent `run` / `perturb` / `snapshot` must KEEP failing loud (never apply
        // the crossed fault from the past) until a `branch`/`replay` rewinds. Then
        // the session works again.
        let mut s = rdtsc_then_hlt_server(2000);
        hello(&mut s);
        let base = snap(&mut s);
        stage_corrupt(&mut s, 1500);
        let poisoned = Err(ControlError::ScheduleUnsatisfiable {
            moment: 1500,
            vtime: 2000,
        });
        // The overshooting run poisons.
        assert_eq!(run_with_deadline(&mut s, 1000), poisoned);
        // A re-sent run stays poisoned (does NOT apply 1500 from the past).
        assert_eq!(run_with_deadline(&mut s, 5000), poisoned);
        assert_eq!(run_all_res(&mut s), poisoned);
        // Perturb stays poisoned.
        assert_eq!(
            s.handle(&Request::Perturb {
                fault: HostFault(EnvHostFault::InjectInterrupt { vector: 0x40 }.encode()),
                at: Moment(9000),
            })
            .unwrap(),
            poisoned
        );
        // Snapshot stays poisoned.
        assert_eq!(s.handle(&Request::Snapshot).unwrap(), poisoned);
        // The crossed fault never applied.
        assert_eq!(&s.vmm().unwrap().guest_memory()[0x40..0x48], &[0u8; 8]);
        // A rewind (replay of the pristine base) clears the poison — the session runs
        // cleanly again.
        assert_eq!(s.handle(&Request::Replay(base)).unwrap(), Ok(Reply::Unit));
        assert!(matches!(run_all_res(&mut s), Ok(Reply::Stop(_))));
    }

    #[test]
    fn snapshot_while_a_fault_is_staged_is_rejected() {
        // PR #51 round-3 item 2: a snapshot seals only VM state; a staged future
        // fault would be silently dropped by any restore of it — reject loudly with
        // `SnapshotWhileArmed` while the schedule is non-empty.
        let mut s = rdtsc_then_hlt_server(2000);
        hello(&mut s);
        // A snapshot at a clean (empty-schedule) point is fine.
        let base = snap(&mut s);
        // Stage a fault, then a snapshot is refused loudly.
        stage_corrupt(&mut s, 3000);
        assert_eq!(
            s.handle(&Request::Snapshot).unwrap(),
            Err(ControlError::SnapshotWhileArmed)
        );
        // A rewind clears the schedule; snapshot works again.
        assert_eq!(s.handle(&Request::Replay(base)).unwrap(), Ok(Reply::Unit));
        assert!(matches!(
            s.handle(&Request::Snapshot).unwrap(),
            Ok(Reply::SnapId(_))
        ));
    }

    #[test]
    fn branch_env_host_faults_go_through_the_same_validation_as_perturb() {
        // Blocking item 1c: an env host fault that is out-of-range / out-of-scope /
        // behind the snapshot is a recoverable ControlError at branch time (the same
        // reply `perturb` gives), NOT a later session-fatal ServeError at apply time.
        // The `server()` live VM is at effective V-time 500, so the snapshot — and
        // the restored floor — is 500.
        let host_env = |m: u64, fault: EnvHostFault| {
            let mut spec = EnvSpec::Seeded {
                seed: 7,
                policy: FaultPolicy::none(),
            };
            spec.record(m, environment::Action::Host(fault));
            Environment {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: spec.encode(),
            }
        };

        // (i) out-of-range gpa → recoverable PerturbOutOfRange (was a fatal
        // ServeError::Vmm before this fix).
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        let base = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: host_env(
                    1000,
                    EnvHostFault::CorruptMemory {
                        gpa: RAM as u64 - 4,
                        mask: BitMask(0xFF),
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
                env: host_env(100, EnvHostFault::InjectInterrupt { vector: 40 }),
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
                env: host_env(1000, EnvHostFault::SkewTime(environment::VTime(5))),
            })
            .unwrap(),
            Err(ControlError::Unsupported)
        );

        // (iv) a valid future host fault is accepted and staged (Reply::Unit).
        let base = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: host_env(1000, EnvHostFault::InjectInterrupt { vector: 40 }),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
    }

    /// Build a branch env carrying a single host fault at `m`.
    fn host_env(m: u64, fault: EnvHostFault) -> Environment {
        let mut spec = EnvSpec::Seeded {
            seed: 7,
            policy: FaultPolicy::none(),
        };
        spec.record(m, environment::Action::Host(fault));
        Environment {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: spec.encode(),
        }
    }

    #[test]
    fn a_rejected_branch_env_fault_is_side_effect_free() {
        // PR #51 round-5 item 1: a branch whose env carries an inadmissible host
        // fault must reject WITHOUT swapping the VM — the old timeline is untouched,
        // so the client's state is exactly what it was before the (failed) branch.
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        let base = snap(&mut s);
        let before = s.vmm().unwrap().state_hash();

        // An out-of-range gpa is rejected — validated against the LIVE VM before any
        // drop/restore/reseed.
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: host_env(
                    1000,
                    EnvHostFault::CorruptMemory {
                        gpa: RAM as u64 - 4,
                        mask: BitMask(0xFF),
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
            s.vmm().unwrap().state_hash(),
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
    fn a_terminal_stop_with_a_staged_fault_poisons_loud() {
        // PR #51 round-6 item 1 (supersedes round-5 item 2): a natural terminal exit
        // is NOT a V-time intercept, so `effective_vns` is only a lower bound —
        // nothing staged is provably uncrossed. So a terminal stop with ANY fault
        // still staged **poisons loud** (a named, rewindable error) rather than
        // silently dropping a possibly-crossed accepted perturb. The round-5
        // `SnapshotWhileArmed` trap stays fixed: poison is rewindable, not a stuck
        // state. Task-60 crash campaigns rewind (`branch`/`replay`) anyway.
        let mut s = server(vec![Exit::Hlt]);
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
        assert_eq!(s.recorded_env().host_faults().count(), 0);
        // Poisoned: re-run + snapshot both reject with the named, rewindable error
        // (not a silent stuck `SnapshotWhileArmed`).
        assert_eq!(run_all_res(&mut s), poisoned);
        assert_eq!(s.handle(&Request::Snapshot).unwrap(), poisoned);
        // A rewind (replay of the pristine base) clears the poison — the session runs
        // and snapshots cleanly again.
        assert_eq!(s.handle(&Request::Replay(base)).unwrap(), Ok(Reply::Unit));
        assert!(matches!(
            run_all_res(&mut s),
            Ok(Reply::Stop(StopReason::Quiescent { .. }))
        ));
    }

    #[test]
    fn perturb_after_a_terminal_stop_is_rejected_not_synchronized() {
        // PR #51 round-7 (family root cause): after a natural terminal stop the VM is
        // NOT at a V-time intercept, so `effective_vns` is only a lower bound —
        // staging a fault against it could record it at a `Moment` the guest already
        // executed past. `perturb` must reject with `NotSynchronized`; the client
        // rewinds (branch/replay lands on an intercept) and can then stage.
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        let base = snap(&mut s); // synchronized (post-RDTSC, V-time 500)
        // Run to the terminal HLT → unsynchronized.
        assert!(matches!(run_all(&mut s), StopReason::Quiescent { .. }));
        let inject = |vector: u8| Request::Perturb {
            fault: HostFault(EnvHostFault::InjectInterrupt { vector }.encode()),
            at: Moment(1000),
        };
        assert_eq!(
            s.handle(&inject(40)).unwrap(),
            Err(ControlError::NotSynchronized),
            "a perturb at an unsynchronized (terminal) point is rejected"
        );
        // A rewind restores onto a V-time intercept → synchronized → perturb accepted.
        assert_eq!(s.handle(&Request::Replay(base)).unwrap(), Ok(Reply::Unit));
        assert_eq!(s.handle(&inject(40)).unwrap(), Ok(Reply::Unit));
    }

    #[test]
    fn perturb_after_a_synchronized_deadline_stop_reproduces() {
        // The positive half of the sync gate + a recorded-env replay-equivalence
        // check: a `perturb` after a **deadline stop that landed on an arrival**
        // (synchronized) is accepted, and the recorded env — branched from base and
        // re-run — reproduces the live `state_hash` across the multi-run session.
        let run_to = |s: &mut ControlServer<ArrivalBackend>, d: u64| {
            s.handle(&Request::Run {
                until: StopConditions {
                    deadline: Some(VTime(d)),
                    on: StopMask::NONE,
                },
                resolve: None,
            })
            .unwrap()
        };
        let perturb = |s: &mut ControlServer<ArrivalBackend>, gpa: u64, at: u64| {
            s.handle(&Request::Perturb {
                fault: HostFault(
                    EnvHostFault::CorruptMemory {
                        gpa,
                        mask: BitMask(0xDEAD_0000_BEEF),
                    }
                    .encode(),
                ),
                at: Moment(at),
            })
            .unwrap()
        };

        let mut s = arrival_server();
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
        // accepted (NOT NotSynchronized).
        assert_eq!(perturb(&mut s, 0x80, 300), Ok(Reply::Unit));
        assert!(matches!(arr_run(&mut s), Ok(Reply::Stop(_))));
        let h_live = arr_hash(&s);
        let recorded = s.recorded_env().clone();
        assert_eq!(recorded.host_faults().count(), 2, "both faults recorded");

        // Replay-equivalence: branch the recorded env from base and re-run → the same
        // live hash.
        let mut r = arrival_server();
        arr_hello(&mut r);
        let base_r = arr_snap(&mut r);
        r.handle(&Request::Branch {
            snap: base_r,
            env: Environment {
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
        // EXACT arrival, not late — even when the run's deadline is already expired
        // (d < vns). It must apply and stop with `Deadline`, never poison. Live VM
        // RDTSCs to V-time 500; the fault is staged at 500; the deadline is 300.
        let mut s = rdtsc_then_hlt_server(500);
        hello(&mut s);
        stage_corrupt(&mut s, 500);
        match run_with_deadline(&mut s, 300) {
            Ok(Reply::Stop(StopReason::Deadline { vtime })) => assert_eq!(vtime, VTime(500)),
            other => panic!("expected Deadline{{500}}, got {other:?}"),
        }
        // The exact-arrival fault APPLIED (recorded), and the schedule is NOT poisoned
        // — a later run works.
        assert_eq!(
            s.recorded_env().host_faults().count(),
            1,
            "the m==vns fault applied"
        );
        assert_ne!(
            &s.vmm().unwrap().guest_memory()[0x40..0x48],
            &[0u8; 8],
            "the exact-arrival upset landed"
        );
        assert!(
            matches!(run_all_res(&mut s), Ok(Reply::Stop(_))),
            "not poisoned"
        );
    }

    #[test]
    fn a_branch_env_moment_occupies_the_schedule_for_ruling_b() {
        // Ruling B across the branch→perturb boundary (PR #51 round-5 suggestion): a
        // branch env stages a fault at Moment M; a later `perturb` at M is then the
        // loud `PerturbMomentTaken` (one fault per Moment holds after a branch, not
        // just within a fresh session). Pins the invariant so a future batch-validate
        // refactor can't silently regress it.
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        let base = snap(&mut s);
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: host_env(1000, EnvHostFault::InjectInterrupt { vector: 40 }),
            })
            .unwrap(),
            Ok(Reply::Unit)
        );
        assert_eq!(
            s.handle(&Request::Perturb {
                fault: HostFault(EnvHostFault::InjectInterrupt { vector: 41 }.encode()),
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
        /// schedule under test never carries them) in `1..=100_000` (each arms
        /// a forward arrival); faults are in-range CorruptMemory (any gpa whose word
        /// fits the 16 KiB RAM, any mask) or InjectInterrupt (any non-reserved vector).
        #[test]
        fn arbitrary_schedule_applied_twice_is_identical(
            schedule in proptest::collection::btree_map(
                1u64..=100_000u64,
                prop_oneof![
                    (0u64..(RAM as u64 - 8), any::<u64>())
                        .prop_map(|(gpa, m)| EnvHostFault::CorruptMemory { gpa, mask: BitMask(m) }),
                    (16u8..=255u8)
                        .prop_map(|vector| EnvHostFault::InjectInterrupt { vector }),
                ],
                0..8usize,
            ),
        ) {
            let sched: Vec<(u64, EnvHostFault)> = schedule.into_iter().collect();
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

    /// A mock-wrapping backend that makes arrival **exact without a scripted
    /// `Deadline`**: `run_until(d)` always lands at `d` (a `Deadline`), and `run()`
    /// (no arrival armed) is always a terminal `Hlt`. This lets a *random* verb
    /// sequence drive any number of arrivals + runs without pre-scripting exits.
    /// `deterministic_tsc` (forwarded from the inner mock) is `true`, so the server
    /// treats it as an armable host-plane backend.
    struct ArrivalBackend(MockBackend);
    impl Backend for ArrivalBackend {
        fn set_cpuid(&mut self, m: &vmm_backend::CpuidModel) -> vmm_backend::Result<()> {
            self.0.set_cpuid(m)
        }
        fn set_msr_filter(&mut self, f: &vmm_backend::MsrFilter) -> vmm_backend::Result<()> {
            self.0.set_msr_filter(f)
        }
        unsafe fn map_memory(
            &mut self,
            gpa: vmm_backend::Gpa,
            host: &mut [u8],
        ) -> vmm_backend::Result<()> {
            // SAFETY: forwards to the inner mock, which only records the region.
            unsafe { self.0.map_memory(gpa, host) }
        }
        fn run(&mut self) -> vmm_backend::Result<Exit> {
            Ok(Exit::Hlt)
        }
        fn run_until(&mut self, d: vmm_backend::Vtime) -> vmm_backend::Result<Exit> {
            Ok(Exit::Deadline { reached: d })
        }
        fn inject(&mut self, e: vmm_backend::Event) -> vmm_backend::Result<()> {
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
        fn complete_cpuid(&mut self, a: u32, b: u32, c: u32, d: u32) -> vmm_backend::Result<()> {
            self.0.complete_cpuid(a, b, c, d)
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
        fn capabilities(&self) -> vmm_backend::Capabilities {
            self.0.capabilities()
        }
    }

    fn arrival_vmm(seed: u64) -> Vmm<ArrivalBackend> {
        let mut m = MockBackend::with_exits(vec![]);
        m.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
        m.set_msr_filter(&vmm_backend::MsrFilter::default())
            .unwrap();
        let mut v = Vmm::new(ArrivalBackend(m), GuestRam::new(RAM).unwrap());
        v.wire_vtime(
            VtimeWiring::new(
                contract_vclock_config(),
                Box::new(ScriptedWork::at(0)),
                seed,
            )
            .unwrap(),
        );
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

    /// A server over the exact-arrival mock, whose factory boots identically-composed
    /// restore targets (so `branch`/`replay` succeed). Seed `0x59` for the live VM
    /// and every fork.
    fn arrival_server() -> ControlServer<ArrivalBackend> {
        let live = arrival_vmm(0x59);
        ControlServer::new(live, Box::new(|| Ok(arrival_vmm(0x59))))
    }

    fn arr_hello<B: Backend>(s: &mut ControlServer<B>) {
        assert!(s.handle(&Request::Hello(server_caps())).unwrap().is_ok());
    }
    fn arr_snap<B: Backend>(s: &mut ControlServer<B>) -> SnapId {
        match s.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::SnapId(id)) => id,
            other => panic!("snapshot: {other:?}"),
        }
    }
    fn arr_run<B: Backend>(s: &mut ControlServer<B>) -> Result<Reply, ControlError> {
        s.handle(&Request::Run {
            until: StopConditions {
                deadline: None,
                on: StopMask::NONE,
            },
            resolve: None,
        })
        .unwrap()
    }
    fn arr_hash<B: Backend>(s: &ControlServer<B>) -> [u8; 32] {
        s.vmm().unwrap().state_hash()
    }

    /// A **no-op** host fault (`CorruptMemory` with a zero XOR mask) — it changes no
    /// guest byte, but staging it at a `Moment` arms the exact-count arrival so a
    /// `run` lands there. On the portable mock this is the stand-in for the box's
    /// timer-driven force-exit (tasks 47/55): the mock's bare `run()` HLTs
    /// immediately, so a plain deadline never advances V-time, but an armed arrival
    /// drives `run_until` to the exact `Moment`. On the real backend the deadline
    /// itself force-exits, so no marker is needed (see the `live_moment_address` box
    /// gate).
    fn arrival_marker(at: u64) -> Request {
        Request::Perturb {
            fault: HostFault(
                EnvHostFault::CorruptMemory {
                    gpa: 0,
                    mask: BitMask(0), // XOR 0 — the state is untouched
                }
                .encode(),
            ),
            at: Moment(at),
        }
    }

    /// The **moment-address materialization procedure** (task 80), exercised
    /// portably on the exact-arrival mock: given `(env, moment)` with a
    /// genesis-complete `env`, `branch(genesis, env)` then advance to the exact
    /// `Moment` (here via a no-op arrival marker — see [`arrival_marker`]; on the
    /// box the deadline force-exits) and read that materialized point with the
    /// observation verbs. Materializing the same address **twice from genesis**
    /// yields byte-identical `regs` (including `rip` and `moment`), `read`, and
    /// `hash(Whole)` — the address is a stable coordinate. (The box gate proves the
    /// same against the live Postgres workload, where the state actually differs
    /// Moment-to-Moment; the mock's static image makes this a determinism/mechanism
    /// proof, not a state-evolution one.)
    #[test]
    fn moment_address_materializes_identically_twice() {
        let mut s = arrival_server();
        arr_hello(&mut s);
        let genesis = arr_snap(&mut s); // the genesis snapshot (V-time 0)
        let env = seeded_env_arr(0x0080_0080); // a genesis-complete Seeded env

        let materialize = |s: &mut ControlServer<ArrivalBackend>,
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
            s.handle(&arrival_marker(moment)).unwrap().unwrap();
            let stop = match s
                .handle(&Request::Run {
                    until: StopConditions {
                        deadline: Some(VTime(moment)),
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
                    vtime: VTime(moment)
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
                "regs reports the retired count == the addressed Moment"
            );
            assert_eq!(view.vtime, moment, "vtime coincides with moment");
            let bytes = match s.handle(&Request::Read { gpa: 0, len: 128 }).unwrap() {
                Ok(Reply::Bytes(b)) => b,
                other => panic!("read answered {other:?}"),
            };
            (view, bytes, arr_hash(s))
        };

        for moment in [1_000u64, 5_000, 50_000, 250_000] {
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
    fn inspection_mid_materialization_does_not_perturb_the_continuation() {
        let env = seeded_env_arr(0x0B5E_0BED);
        let (mid, late) = (10_000u64, 90_000u64);

        // One materialization to `mid`→`late` through two arrival markers, with an
        // optional inspection pass at `mid`. `arrival_server`s are freshly and
        // identically composed, so the two runs differ only in the inspection.
        let run_to_late = |inspect: bool| -> [u8; 32] {
            let mut s = arrival_server();
            arr_hello(&mut s);
            let genesis = arr_snap(&mut s);
            s.handle(&Request::Branch {
                snap: genesis,
                env: env.clone(),
            })
            .unwrap()
            .unwrap();
            // Advance to `mid`.
            s.handle(&arrival_marker(mid)).unwrap().unwrap();
            assert!(matches!(
                s.handle(&Request::Run {
                    until: StopConditions {
                        deadline: Some(VTime(mid)),
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
            s.handle(&arrival_marker(late)).unwrap().unwrap();
            assert!(matches!(
                s.handle(&Request::Run {
                    until: StopConditions {
                        deadline: Some(VTime(late)),
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
        let mut s = arrival_server();
        arr_hello(&mut s);
        // Stage + apply a fault at Moment 100 by running to deadline 100.
        s.handle(&Request::Perturb {
            fault: HostFault(EnvHostFault::InjectInterrupt { vector: 0x40 }.encode()),
            at: Moment(100),
        })
        .unwrap()
        .unwrap();
        // Run with deadline 100: arrival lands at 100, applies, then the deadline
        // stops the run at V-time 100.
        let stop = s
            .handle(&Request::Run {
                until: StopConditions {
                    deadline: Some(VTime(100)),
                    on: StopMask::NONE,
                },
                resolve: None,
            })
            .unwrap();
        assert!(matches!(stop, Ok(Reply::Stop(StopReason::Deadline { .. }))));
        assert_eq!(
            s.recorded_env().host_faults().count(),
            1,
            "the fault applied"
        );
        assert_eq!(s.vmm().unwrap().effective_vns(), Some(100));
        // A second perturb at the already-APPLIED Moment 100 (== floor, so it clears
        // the past-Moment check) is rejected — it does not overwrite the recorded fault.
        assert_eq!(
            s.handle(&Request::Perturb {
                fault: HostFault(EnvHostFault::InjectInterrupt { vector: 0x41 }.encode()),
                at: Moment(100),
            })
            .unwrap(),
            Err(ControlError::PerturbMomentTaken { at: 100 })
        );
        assert_eq!(
            s.recorded_env().host_faults().count(),
            1,
            "the applied fault is still recorded (not overwritten)"
        );
    }

    #[test]
    fn perturb_on_an_unarmable_backend_is_unsupported() {
        // Finding 3: a backend that cannot arm the exact-arrival seam (V-time
        // unwired, or deterministic_tsc=false) would apply a staged fault late.
        // Reject perturb up front with Unsupported. Here: a mock with NO V-time wired.
        let mut m = MockBackend::with_exits(vec![Exit::Hlt]);
        m.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
        m.set_msr_filter(&vmm_backend::MsrFilter::default())
            .unwrap();
        let v = Vmm::new(m, GuestRam::new(RAM).unwrap()); // V-time NOT wired
        let mut s = ControlServer::new(
            v,
            Box::new(|| Err(VmmError::ContractViolation("unused".into()))),
        );
        assert!(s.handle(&Request::Hello(server_caps())).unwrap().is_ok());
        assert_eq!(
            s.handle(&Request::Perturb {
                fault: HostFault(EnvHostFault::InjectInterrupt { vector: 0x40 }.encode()),
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
        let mut s = arrival_server(); // LAPIC wired, synchronized
        arr_hello(&mut s);
        for vector in [0u8, 1, 15] {
            assert_eq!(
                s.handle(&Request::Perturb {
                    fault: HostFault(EnvHostFault::InjectInterrupt { vector }.encode()),
                    at: Moment(100),
                })
                .unwrap(),
                Err(ControlError::PerturbReservedVector { vector })
            );
        }
        // A non-reserved vector still stages cleanly.
        assert_eq!(
            s.handle(&Request::Perturb {
                fault: HostFault(EnvHostFault::InjectInterrupt { vector: 16 }.encode()),
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
                fault: HostFault(EnvHostFault::InjectInterrupt { vector: 0x40 }.encode()),
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
                    EnvHostFault::CorruptMemory {
                        gpa: 0x40,
                        mask: BitMask(0xFF),
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
    fn recoverable_restore_failure_clears_the_stale_schedule() {
        // Finding 4a: a recoverable RestoreFailed still REPLACES the VM with a fresh
        // boot, so the old timeline's staged faults must not survive attached to it.
        // The live VM is V-time-wired; the factory boots forks WITHOUT V-time, so the
        // restore is a validation-class rejection (RestoreFailed, fresh VM kept).
        let live = arrival_vmm(0x59);
        let factory = Box::new(|| {
            // A fork with NO V-time → restoring a V-time blob is a ContractViolation.
            let mut m = MockBackend::with_exits(vec![]);
            m.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
            m.set_msr_filter(&vmm_backend::MsrFilter::default())
                .unwrap();
            Ok(Vmm::new(ArrivalBackend(m), GuestRam::new(RAM).unwrap()))
        });
        let mut s = ControlServer::new(live, factory);
        arr_hello(&mut s);
        let base = arr_snap(&mut s);
        // Stage a fault on the current (soon-to-be-replaced) timeline.
        s.handle(&Request::Perturb {
            fault: HostFault(EnvHostFault::InjectInterrupt { vector: 0x40 }.encode()),
            at: Moment(50),
        })
        .unwrap()
        .unwrap();
        // Branch fails recoverably (RestoreFailed), keeping the fresh (unwired) VM.
        assert_eq!(
            s.handle(&Request::Branch {
                snap: base,
                env: Environment {
                    blob_version: EnvSpec::BLOB_VERSION,
                    bytes: EnvSpec::Seeded {
                        seed: 7,
                        policy: FaultPolicy::none()
                    }
                    .encode(),
                },
            })
            .unwrap(),
            Err(ControlError::RestoreFailed)
        );
        // The stale fault is gone: recorded reset, schedule cleared. A run applies
        // nothing (and the unwired fork can't even enforce — but the point is the
        // schedule did not carry forward).
        assert_eq!(
            s.recorded_env().host_faults().count(),
            0,
            "the stale schedule/recorded was cleared on the recoverable failure"
        );
    }

    #[test]
    fn replay_derives_recorded_seed_from_the_restored_stream() {
        // Finding 4b: `replay` must derive the recorded seed from the RESTORED
        // stream, not the prior session. Drive the recorded seed to a wrong value
        // via a branch, then replay the pristine base and confirm the recorded env —
        // re-applied from base — reproduces the live post-replay+perturb+run hash.
        let mut s = arrival_server();
        arr_hello(&mut s);
        let base = arr_snap(&mut s);
        // Poison the session seed: branch to a distinct seed 0xDEAD (recorded.seed
        // becomes that stream). If replay carried this forward, reproduction breaks.
        s.handle(&Request::Branch {
            snap: base,
            env: Environment {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: EnvSpec::Seeded {
                    seed: 0xDEAD,
                    policy: FaultPolicy::none(),
                }
                .encode(),
            },
        })
        .unwrap()
        .unwrap();
        // Now replay the pristine base (its stream is seed 0x59, not 0xDEAD).
        assert_eq!(s.handle(&Request::Replay(base)).unwrap(), Ok(Reply::Unit));
        // Perturb + run on the replayed timeline.
        s.handle(&Request::Perturb {
            fault: HostFault(
                EnvHostFault::CorruptMemory {
                    gpa: 0x80,
                    mask: BitMask(0x1234_5678),
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
        let mut r = arrival_server();
        arr_hello(&mut r);
        let base_r = arr_snap(&mut r);
        r.handle(&Request::Branch {
            snap: base_r,
            env: Environment {
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
        Perturb(EnvHostFault, u64),
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
                        EnvHostFault::CorruptMemory {
                            gpa,
                            mask: BitMask(m),
                        }
                    }),
                    (16u8..=255u8).prop_map(|vector| EnvHostFault::InjectInterrupt { vector }),
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
        /// of `perturb`/`run`/`branch`/`replay` on the exact-arrival mock, the
        /// `recorded_env()` — re-applied by branching it from the starting snapshot
        /// on a fresh server and running — reproduces the live `state_hash` after
        /// every completed run. This is the net that covers the whole verb space:
        /// the recorded apply point must equal the actual apply point, or the op
        /// must have failed loudly (rejected ops are simply skipped by the model).
        #[test]
        fn verb_sequence_recorded_env_reproduces_live_hash(ops in prop::collection::vec(arb_verb_op(), 1..12)) {
            let mut s = arrival_server();
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
                                let mut r = arrival_server();
                                arr_hello(&mut r);
                                let base_r = arr_snap(&mut r);
                                r.handle(&Request::Branch {
                                    snap: base_r,
                                    env: Environment { blob_version: EnvSpec::BLOB_VERSION, bytes: e.encode() },
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
                        // Clean → Ok(SnapId); with a fault staged → SnapshotWhileArmed
                        // (loud). Either way the session stays consistent; the minted
                        // handle is unused (branches/replays use `base` only).
                        let _ = s.handle(&Request::Snapshot).unwrap();
                    }
                }
            }
        }
    }

    fn seeded_env_arr(seed: u64) -> Environment {
        Environment {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: EnvSpec::Seeded {
                seed,
                policy: FaultPolicy::none(),
            }
            .encode(),
        }
    }

    // -----------------------------------------------------------------------
    // PR #51 round-4 — the idle-HLT-before-fault path: a staged arrival must wake
    // the idle jump (jump to min(timer, arrival)) rather than sail past the Moment.
    // -----------------------------------------------------------------------

    /// A mock-wrapping backend whose guest is a **resumable idle**: `run`/`run_until`
    /// always return a natural `Hlt` and the vCPU has `RFLAGS.IF` set, so every
    /// arrival is reached through the idle-jump path (`on_hlt` → `idle_action` →
    /// jump to `min(timer, arrival)`) instead of a `run_until` `Deadline`. No timer
    /// is armed, so the staged host-fault arrival is the sole wake event; with
    /// `CorruptMemory`-only faults (no IRR raise) the run idles Moment-to-Moment and
    /// terminates cleanly once the schedule drains. `deterministic_tsc` is `true`.
    struct IdleBackend(MockBackend);
    impl Backend for IdleBackend {
        fn set_cpuid(&mut self, m: &vmm_backend::CpuidModel) -> vmm_backend::Result<()> {
            self.0.set_cpuid(m)
        }
        fn set_msr_filter(&mut self, f: &vmm_backend::MsrFilter) -> vmm_backend::Result<()> {
            self.0.set_msr_filter(f)
        }
        unsafe fn map_memory(
            &mut self,
            gpa: vmm_backend::Gpa,
            host: &mut [u8],
        ) -> vmm_backend::Result<()> {
            // SAFETY: forwards to the inner mock, which only records the region.
            unsafe { self.0.map_memory(gpa, host) }
        }
        fn run(&mut self) -> vmm_backend::Result<Exit> {
            Ok(Exit::Hlt)
        }
        fn run_until(&mut self, _d: vmm_backend::Vtime) -> vmm_backend::Result<Exit> {
            // The guest idles (a natural HLT) before any deadline — the arrival is
            // reached through the idle jump, not a run_until Deadline.
            Ok(Exit::Hlt)
        }
        fn inject(&mut self, e: vmm_backend::Event) -> vmm_backend::Result<()> {
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
        fn complete_cpuid(&mut self, a: u32, b: u32, c: u32, d: u32) -> vmm_backend::Result<()> {
            self.0.complete_cpuid(a, b, c, d)
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
        fn capabilities(&self) -> vmm_backend::Capabilities {
            self.0.capabilities()
        }
    }

    fn idle_vmm(seed: u64) -> Vmm<IdleBackend> {
        let mut m = MockBackend::with_exits(vec![]);
        m.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
        m.set_msr_filter(&vmm_backend::MsrFilter::default())
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
        v.wire_vtime(
            VtimeWiring::new(
                contract_vclock_config(),
                Box::new(ScriptedWork::at(0)),
                seed,
            )
            .unwrap(),
        );
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

    fn idle_seeded_env(seed: u64) -> Environment {
        Environment {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: EnvSpec::Seeded {
                seed,
                policy: FaultPolicy::none(),
            }
            .encode(),
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
        /// recorded-env-reproduces invariant as the exact-arrival proptest, but every
        /// arrival is reached through the **idle jump** (the guest HLTs before each
        /// staged `Moment`; `idle_action` wakes at the arrival). `CorruptMemory`-only
        /// (an idle guest with no timer terminates cleanly once the schedule drains),
        /// verbs `perturb`/`run`/`branch`/`replay`, all from a fixed base snapshot.
        #[test]
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
                            fault: HostFault(EnvHostFault::CorruptMemory { gpa, mask: BitMask(0xA5A5_5A5A) }.encode()),
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
                                    env: Environment { blob_version: EnvSpec::BLOB_VERSION, bytes: e.encode() },
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
    // collapsed hop's entropy reseed at its recorded Moment (exact-arrival
    // discipline), instead of reseeding once at the fold's root. The fold ==
    // hop-by-hop equality (the flipped task-68 pin) is the conductor's socket
    // gate; here we pin the server-side mechanics.
    // -----------------------------------------------------------------------

    /// A branch env carrying only reseed markers (no overrides/standing).
    fn marker_env(seed: u64, markers: &[(u64, u64)]) -> Environment {
        let mut spec = EnvSpec::Seeded {
            seed,
            policy: FaultPolicy::none(),
        };
        for &(m, s) in markers {
            spec.record_reseed(m, s);
        }
        Environment {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: spec.encode(),
        }
    }

    #[test]
    fn branch_with_a_floor_marker_reseeds_from_the_marker_not_the_env_seed() {
        // Markers are authoritative: a marker at the restore floor IS the branch
        // reseed, and the env's own seed is not consulted for reseeding.
        let mut s = arrival_server();
        arr_hello(&mut s);
        let base = arr_snap(&mut s);
        let mut branch_hash = |env: Environment| -> [u8; 32] {
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
    fn mid_run_reseed_marker_applies_at_its_moment_and_recorded_env_reproduces() {
        // A mid-trajectory marker (a collapsed hop's branch reseed) is applied at
        // its exact Moment, the run is deterministic, the marker value reaches the
        // state (control differs), and the emitted recorded env replays to the
        // identical hash (the record → replay closure).
        let run_leg = |mid_seed: u64| -> ([u8; 32], EnvSpec) {
            let mut s = arrival_server();
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
        let mut r = arrival_server();
        arr_hello(&mut r);
        let base_r = arr_snap(&mut r);
        r.handle(&Request::Branch {
            snap: base_r,
            env: Environment {
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
    fn a_buggify_decision_after_a_mid_run_reseed_folds_to_the_sequential_branch() {
        // Round-5 P1 — the DECISIVE fold-vs-sequential test. A run that takes a
        // mid-run reseed marker AND then resolves a buggify decision past it records
        // a reproducer that must replay bit-identically. A task-78 marker reseeds
        // ONLY entropy (`reseed_entropy` → `vt.entropy`), never the buggify PRNG
        // (`SdkChannel.env`); BOTH the sequential branch and the fold do exactly
        // that, so buggify-after-reseed is coherent and the record→replay closure
        // holds. (The buggify⊥entropy independence itself is pinned in vmm.rs's
        // `buggify_decisions_are_independent_of_an_entropy_reseed`.)
        const REQ_GPA: usize = 0xE000;
        const BIG_RAM: usize = 0x2_0000;
        let m: u64 = 4_000; // the mid-run reseed marker Moment
        let point: u32 = 1;

        // A buggify request frame (point 1), carried in the genesis snapshot so
        // every restored fork's doorbell resolves a buggify decision.
        let mut bug = [0u8; 4096];
        let bn = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Sdk,
            1,
            1,
            &point.to_le_bytes(),
            &mut bug,
        )
        .unwrap();
        let bug_frame = bug[..bn].to_vec();

        // A wired SDK VM with the buggify frame in RAM. Forks restore genesis's
        // memory (which carries the frame), so their doorbell resolves buggify.
        fn wire(script: Vec<Exit>, frame: &[u8], req_gpa: usize, ram: usize) -> Vmm<MockBackend> {
            let mut mb = MockBackend::with_exits(script);
            mb.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
            mb.set_msr_filter(&vmm_backend::MsrFilter::default())
                .unwrap();
            let mut v = Vmm::new(mb, GuestRam::new(ram).unwrap());
            v.wire_vtime(
                VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(0)), 9)
                    .unwrap(),
            );
            v.wire_snapshot_hashing();
            let mut mem = vec![0u8; ram];
            mem[req_gpa..req_gpa + frame.len()].copy_from_slice(frame);
            v.restore_guest_memory(&mem).unwrap();
            v
        }

        // Forks run: Rdtsc (synchronize) → arrival at m (the reseed drains) →
        // buggify doorbell (decides AFTER the reseed) → HLT.
        let fork_exits = vec![
            Exit::Rdtsc,
            Exit::Deadline { reached: Vtime(0) },
            Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(bn as u32),
            },
            Exit::Hlt,
        ];

        // The env: seed + an always-firing buggify policy + a mid-run reseed marker.
        let env_with = |mid_seed: u64| -> Environment {
            let mut policy = FaultPolicy::none();
            policy.set_buggify_point(point, 1, 1).unwrap();
            let mut spec = EnvSpec::Seeded {
                seed: 0x51EED,
                policy,
            };
            spec.record_reseed(m, mid_seed);
            Environment {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: spec.encode(),
            }
        };

        let build_server = {
            let bug_frame = bug_frame.clone();
            move || -> ControlServer<MockBackend> {
                let frame = bug_frame.clone();
                let exits = fork_exits.clone();
                let mut live = wire(vec![Exit::Rdtsc, Exit::Hlt], &frame, REQ_GPA, BIG_RAM);
                live.step().unwrap(); // Rdtsc → synchronized, so genesis seals
                let factory = Box::new(move || Ok(wire(exits.clone(), &frame, REQ_GPA, BIG_RAM)));
                ControlServer::new(live, factory)
            }
        };

        // Sequential branch: run through the reseed to the buggify decision.
        let mut s = build_server();
        hello(&mut s);
        let base = snap(&mut s);
        s.handle(&Request::Branch {
            snap: base,
            env: env_with(0xABCD),
        })
        .unwrap()
        .unwrap();
        assert!(matches!(
            run_all(&mut s),
            StopReason::Crash { .. } | StopReason::Quiescent { .. }
        ));
        let h_seq = hash(&mut s);
        let rec = s.recorded_env().clone();
        // The reproducer carries BOTH the mid-run reseed marker and the buggify
        // policy, and a buggify decision was actually resolved after the reseed.
        assert!(
            !rec.reseeds().is_empty(),
            "the mid-run reseed marker is recorded"
        );
        assert!(
            rec.policy().is_buggify_only(),
            "the buggify policy is recorded"
        );
        assert!(
            !s.vmm().unwrap().sdk_buggify().is_empty(),
            "a buggify decision was resolved during the run"
        );

        // Fold: replay the recorded reproducer — bit-identical to the sequential run.
        let mut r = build_server();
        hello(&mut r);
        let base_r = snap(&mut r);
        r.handle(&Request::Branch {
            snap: base_r,
            env: Environment {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: rec.encode(),
            },
        })
        .unwrap()
        .unwrap();
        assert!(matches!(
            run_all(&mut r),
            StopReason::Crash { .. } | StopReason::Quiescent { .. }
        ));
        assert_eq!(
            hash(&mut r),
            h_seq,
            "the folded reproducer replays bit-identically to the sequential branch \
             (buggify-after-reseed is coherent)"
        );
    }

    /// Task 61 (review R2): the control plane can now **decide a non-nominal
    /// per-flow policy**. A branch env that faults the `NetFlow` class is ACCEPTED
    /// (the task-73 buggify-only gate widened to the enforceable net decide-seam —
    /// `is_enforceable_only`), the guest's `net_decide` returns the fault from the
    /// seeded stream at a **stable Moment**, and the recorded reproducer replays
    /// bit-identically. This is the record→replay closure for a host-decided net
    /// fault — the mechanism the deferred live gate B drives.
    #[test]
    fn a_net_flow_fault_branch_is_accepted_and_replays_at_a_stable_moment() {
        const REQ_GPA: usize = 0xE000;
        const BIG_RAM: usize = 0x2_0000;
        let conn: u64 = 42;

        // A `net_decide` request frame (flow 1->2, conn 42, event Open), carried in
        // the genesis snapshot so every restored fork's doorbell resolves a flow
        // decision from the branch env's seeded net policy.
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes()); // src
        payload.extend_from_slice(&2u32.to_le_bytes()); // dst
        payload.extend_from_slice(&conn.to_le_bytes()); // conn
        payload.extend_from_slice(&0u16.to_le_bytes()); // event Open
        let mut buf = [0u8; 4096];
        let n = hypercall_proto::encode_request(
            hypercall_proto::ServiceId::Net,
            1,
            1,
            &payload,
            &mut buf,
        )
        .unwrap();
        let net_frame = buf[..n].to_vec();

        fn wire(script: Vec<Exit>, frame: &[u8], req_gpa: usize, ram: usize) -> Vmm<MockBackend> {
            let mut mb = MockBackend::with_exits(script);
            mb.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
            mb.set_msr_filter(&vmm_backend::MsrFilter::default())
                .unwrap();
            let mut v = Vmm::new(mb, GuestRam::new(ram).unwrap());
            v.wire_vtime(
                VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(0)), 9)
                    .unwrap(),
            );
            v.wire_snapshot_hashing();
            let mut mem = vec![0u8; ram];
            mem[req_gpa..req_gpa + frame.len()].copy_from_slice(frame);
            v.restore_guest_memory(&mem).unwrap();
            v
        }

        // Forks run: Rdtsc (synchronize) → net doorbell (decides the flow) → HLT.
        let fork_exits = vec![
            Exit::Rdtsc,
            Exit::Io {
                port: 0x0CA1,
                size: 4,
                write: Some(n as u32),
            },
            Exit::Hlt,
        ];

        // The branch env: a seed + a policy that faults every NetFlow with a
        // `NetReset` (1/1). Non-buggify — the OLD gate would reject this `Unsupported`.
        let net_env = || -> Environment {
            let mut policy = FaultPolicy::none();
            policy
                .set_class(
                    environment::DecisionClass::NetFlow,
                    1,
                    1,
                    &[environment::Fault::NetReset],
                )
                .unwrap();
            let spec = EnvSpec::Seeded {
                seed: 0x51EED,
                policy,
            };
            Environment {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: spec.encode(),
            }
        };

        let build_server = {
            let net_frame = net_frame.clone();
            move || -> ControlServer<MockBackend> {
                let frame = net_frame.clone();
                let exits = fork_exits.clone();
                let mut live = wire(vec![Exit::Rdtsc, Exit::Hlt], &frame, REQ_GPA, BIG_RAM);
                live.step().unwrap(); // Rdtsc → synchronized, so genesis seals
                let factory = Box::new(move || Ok(wire(exits.clone(), &frame, REQ_GPA, BIG_RAM)));
                ControlServer::new(live, factory)
            }
        };

        // Sequential branch with the net-fault env — must be ACCEPTED (the widened
        // gate), not rejected `Unsupported`.
        let mut s = build_server();
        hello(&mut s);
        let base = snap(&mut s);
        let branched = s
            .handle(&Request::Branch {
                snap: base,
                env: net_env(),
            })
            .unwrap();
        assert!(
            branched.is_ok(),
            "a NetFlow-faulting branch env is now accepted (was Unsupported): {branched:?}"
        );
        assert!(matches!(
            run_all(&mut s),
            StopReason::Crash { .. } | StopReason::Quiescent { .. }
        ));
        let h_seq = hash(&mut s);

        // The reproducer carries the net policy (enforceable, not buggify-only), and
        // a non-nominal flow decision was resolved at a stable Moment.
        let rec = s.recorded_env().clone();
        assert!(
            rec.policy().is_enforceable_only() && !rec.policy().is_buggify_only(),
            "the net policy is recorded (enforceable, non-buggify)"
        );
        let decisions = s.vmm().unwrap().net_decisions().to_vec();
        assert_eq!(decisions.len(), 1, "one flow decision resolved");
        let (moment0, conn0, ans0) = decisions[0].clone();
        assert_eq!(conn0, conn);
        assert_eq!(
            ans0,
            environment::Answer::Fault(environment::Fault::NetReset),
            "the host decided a non-nominal per-flow policy"
        );

        // Replay the reproducer — bit-identical hash AND the same non-nominal
        // decision at the SAME Moment (stable across the round-trip).
        let mut r = build_server();
        hello(&mut r);
        let base_r = snap(&mut r);
        r.handle(&Request::Branch {
            snap: base_r,
            env: Environment {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: rec.encode(),
            },
        })
        .unwrap()
        .unwrap();
        assert!(matches!(
            run_all(&mut r),
            StopReason::Crash { .. } | StopReason::Quiescent { .. }
        ));
        assert_eq!(
            hash(&mut r),
            h_seq,
            "the net-fault reproducer replays bit-identically"
        );
        assert_eq!(
            r.vmm().unwrap().net_decisions(),
            &[(moment0, conn0, ans0)],
            "the same non-nominal flow decision surfaces at the same stable Moment on replay"
        );
    }

    #[test]
    fn reseed_marker_behind_the_restore_floor_is_rejected() {
        // Advance the live V-time to 100 (a perturb-armed deadline run lands
        // exactly there), seal, then try to branch with a marker behind it.
        let mut s = arrival_server();
        arr_hello(&mut s);
        s.handle(&Request::Perturb {
            fault: HostFault(
                EnvHostFault::CorruptMemory {
                    gpa: 0x40,
                    mask: BitMask(0xFF),
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
                    deadline: Some(VTime(100)),
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
    fn snapshot_with_a_staged_reseed_is_snapshot_while_armed() {
        let mut s = arrival_server();
        arr_hello(&mut s);
        let base = arr_snap(&mut s);
        s.handle(&Request::Branch {
            snap: base,
            env: marker_env(7, &[(0, 7), (300, 9)]),
        })
        .unwrap()
        .unwrap();
        assert_eq!(
            s.handle(&Request::Snapshot).unwrap(),
            Err(ControlError::SnapshotWhileArmed),
            "a staged reseed is armed future state a snapshot cannot carry"
        );
    }

    #[test]
    fn terminal_with_a_staged_reseed_poisons_and_rewind_recovers() {
        // A reseed staged beyond the trajectory is the same loud
        // ScheduleUnsatisfiable class as a crossed fault (the task spec's gate).
        let mut s = server(vec![Exit::Rdtsc, Exit::Hlt]);
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

    // ===================== task 81: the taint guard =======================

    /// Take a snapshot, returning its handle and the taint bit its reply carries
    /// (`Reply::SnapId` ⇒ untainted; `Reply::Snapshot{tainted}` ⇒ the flag).
    fn snap_tainted(server: &mut ControlServer<MockBackend>) -> (SnapId, bool) {
        match server.handle(&Request::Snapshot).unwrap() {
            Ok(Reply::SnapId(id)) => (id, false),
            Ok(Reply::Snapshot { id, tainted }) => (id, tainted),
            other => panic!("snapshot reply: {other:?}"),
        }
    }

    /// Improvise: `exec` with an already-expired deadline (`VTime(0)`), so it taints
    /// the timeline and returns immediately without stepping the mock guest.
    fn exec(server: &mut ControlServer<MockBackend>, cmd: &str) -> (Vec<u8>, bool) {
        match server
            .handle(&Request::Exec {
                cmd: cmd.to_string(),
                deadline: VTime(0),
            })
            .unwrap()
        {
            Ok(Reply::ExecResult { output, ok }) => (output, ok),
            other => panic!("exec reply: {other:?}"),
        }
    }

    /// The reproducer mint result: `Ok(Environment)` or the loud `Tainted`.
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
        let mut s = server(vec![Exit::Hlt]);
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

    /// A snapshot taken from a tainted timeline reports `tainted: true`; one taken
    /// before any `exec` reports untainted (and via the pre-81 `SnapId` reply).
    #[test]
    fn snapshot_reply_carries_the_taint() {
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        let (_clean, clean_taint) = snap_tainted(&mut s);
        assert!(!clean_taint, "a pre-exec snapshot is untainted");
        exec(&mut s, "ls /");
        let (_dirty, dirty_taint) = snap_tainted(&mut s);
        assert!(dirty_taint, "a post-exec snapshot is tainted");
    }

    /// A `branch` **and** a `replay` from a tainted snapshot both yield a tainted
    /// timeline (taint follows ancestry through either restore verb), and the mint
    /// stays refused on the restored fork.
    #[test]
    fn branch_and_replay_from_a_tainted_snapshot_stay_tainted() {
        let mut s = server(vec![Exit::Hlt]);
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
    fn rewind_to_an_untainted_ancestor_clears_the_live_taint() {
        let mut s = server(vec![Exit::Hlt]);
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
        fn taint_propagates_exactly_along_ancestry(ops in arb_taint_ops()) {
            let mut s = server(vec![Exit::Hlt]);
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
                        exec(&mut s, "echo hi");
                        current = true; // conservative taint: set unconditionally
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
}
