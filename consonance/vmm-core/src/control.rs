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
//!   geometry** (no coverage producer exists yet) and `GUEST_HAS_SDK` off. Any
//!   other verb before `hello` answers [`ControlError::Unsupported`].
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
//!   divergence mechanism, tasks 40/42). The env blob is decoded (and rejected
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
//!   The [`StopMask`](control_proto::StopMask) is carried but moot: no decision
//!   class can surface yet; crash / quiescence / deadline always stop.
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

use std::collections::BTreeMap;
use std::io::{Read, Write};

use control_proto::{
    Caps, ControlError, CoverageGeometry, CrashInfo, CrashKind, HashScope, Reply, Request, SnapId,
    StopReason, VTime, decode_request, encode_reply,
};
use environment::{EnvError, EnvSpec, FaultPolicy};
use snapshot_store::SnapshotId;
use vmm_backend::Backend;

use crate::snapshot::{SnapshotEngine, SnapshotError};
use crate::vmm::{Step, TerminalReason, Vmm, VmmError};

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

/// The [`Caps`] this server negotiates: application protocol 1, `Environment`
/// blobs exactly at [`EnvSpec::BLOB_VERSION`], **zero-width coverage geometry**
/// (no coverage producer exists — task 58 is seed-driven), and no flags
/// (`GUEST_HAS_SDK` off). Exposed so the client side can pin its compatibility
/// check against the same constant.
pub fn server_caps() -> Caps {
    Caps {
        protocol_version: 1,
        env_version_min: EnvSpec::BLOB_VERSION,
        env_version_max: EnvSpec::BLOB_VERSION,
        coverage: CoverageGeometry {
            map_bytes: 0,
            producer: 0,
        },
        flags: control_proto::CapFlags::NONE,
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
    engine: SnapshotEngine,
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
}

impl<B: Backend> ControlServer<B> {
    /// Build a server around a live VM. The [`SnapshotEngine`] is sized to the
    /// VM's guest-memory image; `factory` boots the fresh restore target for
    /// every `branch`/`replay` and must compose its VMs exactly like `vmm`
    /// (same RAM size, wiring, and contract — a mismatch is caught fail-closed
    /// by [`Vmm::restore_vm_state`] at the first restore).
    pub fn new(vmm: Vmm<B>, factory: VmmFactory<B>) -> Self {
        let engine = SnapshotEngine::new(vmm.guest_memory().len());
        // Seed the recorded reproducer from the **live VM's actual entropy stream**
        // (not a bare `0`), so `recorded_env()` reproduces even for a session that
        // runs before its first `branch`/`replay` — a reproducer branched from the
        // starting snapshot then reseeds to this same stream.
        let seed = vmm.entropy_state().unwrap_or(0);
        ControlServer {
            vmm: Some(vmm),
            factory,
            engine,
            snaps: BTreeMap::new(),
            next_snap: 1,
            hello_done: false,
            schedule: BTreeMap::new(),
            recorded: EnvSpec::Seeded {
                seed,
                policy: FaultPolicy::none(),
            },
            schedule_poisoned: None,
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
        }
    }

    /// `perturb(fault, at)`: decode the opaque host-fault blob and **stage** it at
    /// `Moment` `at` for [`ControlServer::run`] to apply — going through the same
    /// [`validate_host_fault`](ControlServer::validate_host_fault) gate a
    /// [`Request::Branch`] env host fault does, so the two paths reject identically
    /// (nothing that would mint a reproducer that does not reproduce is ever
    /// staged). A malformed blob is [`ControlError::MalformedEnvironment`]; the
    /// remaining rejections (past `Moment`, out-of-range gpa, out-of-scope clock
    /// fault, and the same-`Moment` conflict) are the shared gate's.
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
        // Floor = the live VM's current V-time: a `Moment` behind it could only
        // apply *later* than recorded (a non-reproducing reproducer). `0` when
        // V-time is unwired (nothing has advanced).
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
    /// An [`InjectInterrupt`](environment::HostFault::InjectInterrupt) passes the
    /// gate unconditionally; its LAPIC-wired / non-reserved-vector requirements are
    /// enforced at apply time (fail-loud there is session-fatal, since a run that
    /// cannot deliver a staged interrupt is unvouched).
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
            environment::HostFault::InjectInterrupt { .. } => {}
        }
        Ok(())
    }

    /// `snapshot`: seal the current point into the engine as a base layer
    /// (memory image + canonical `vm_state` blob) and mint a wire handle.
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
        if !self.schedule.is_empty() {
            return Ok(Err(ControlError::SnapshotWhileArmed));
        }
        let vmm = self.vmm.as_ref().ok_or(ServeError::Poisoned)?;
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
        let store_id = self.engine.snapshot_base(vmm.guest_memory(), &blob)?;
        let id = self.next_snap;
        self.next_snap += 1;
        self.snaps.insert(id, store_id);
        Ok(Ok(Reply::SnapId(SnapId(id))))
    }

    /// `drop`: release the store layer behind a wire handle and GC.
    fn drop_snap(&mut self, snap: SnapId) -> Result<Reply, ControlError> {
        let Some(store_id) = self.snaps.remove(&snap.0) else {
            return Err(ControlError::UnknownSnapshot(snap));
        };
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
                // the guest-utility enforcement; a **non-`none` fault policy** makes
                // the seeded stream answer decisions with faults no service enforces.
                let has_standing = matches!(
                    &spec,
                    EnvSpec::Recorded { standing, .. } if !standing.is_empty()
                );
                let has_guest = spec
                    .overrides()
                    .values()
                    .any(|a| a.guest_answer().is_some());
                if has_guest || has_standing || spec.policy() != &FaultPolicy::none() {
                    return Ok(Err(ControlError::Unsupported));
                }
                host = spec.host_faults().collect();
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
        // 2. Drop the live VM (frees its work counter — the box allows one
        //    open at a time), then boot the fresh restore target. A factory
        //    failure is fatal: the session has no VM anymore.
        self.vmm = None;
        let mut fresh = (self.factory)()?;
        // 3. Restore, splitting the two result categories (mirrors `snapshot`).
        //    `restore_vm_state` validates the untrusted blob **before** mutating
        //    any live state, so a *validation-class* rejection leaves the fresh
        //    VM intact at its boot point — keep it (the session stays usable) and
        //    answer the recoverable `RestoreFailed`. A failure *after* validation
        //    (a `Backend::restore` fault, a work-counter reset failure) is
        //    substrate breakage: the fresh VM's state can no longer be vouched
        //    for, so the VM is dropped (stays `None` → poisoned) and the session
        //    is torn down (`ServeError`) rather than let a client run from
        //    unvouched state.
        match fresh.restore_snapshot(mapping.as_slice(), &vm_state) {
            Ok(()) => {}
            // Pre-commit rejection (a bad/foreign blob, mismatched wiring, or an
            // invalid clock config) — the fresh VM never mutated, so keep it.
            Err(VmmError::ContractViolation(_) | VmmError::Snapshot(_) | VmmError::Vtime(_)) => {
                self.vmm = Some(fresh);
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
                return Ok(Err(ControlError::RestoreFailed));
            }
            // Post-validation substrate breakage — the VM is unvouched; tear down.
            Err(e) => return Err(e.into()),
        }
        // 4. Branch ⇒ fork the entropy stream from the env's seed. On this
        //    substrate `reseed_entropy` fails only if V-time is unwired — a
        //    composition bug (the factory must mirror the live VM), fatal.
        if let Some(seed) = seed {
            fresh.reseed_entropy(seed)?;
        }
        self.vmm = Some(fresh);
        // 5. A restore rewinds the VM, so **re-arm the host-plane schedule** from
        //    scratch (task 59): drop any stale staged faults and reset the recorded
        //    reproducer to a bare `Seeded` at the **restored stream's** seed
        //    ([`reset_schedule_to_fresh_vm`]), then stage the branch env's own host
        //    overrides. The overrides were already validated (admissible, in-`Moment`
        //    order, no duplicates) at step 1b against the live VM — side-effect-free —
        //    so this only stages them; `run` applies + records them.
        self.reset_schedule_to_fresh_vm();
        for (m, fault) in host {
            self.schedule.insert(m, fault);
        }
        Ok(Ok(Reply::Unit))
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
            let vns = self
                .vmm
                .as_ref()
                .ok_or(ServeError::Poisoned)?
                .effective_vns()
                .unwrap_or(0);

            // 1. Drain: apply exact-arrival faults (`m == vns`), poison a crossed one
            //    (`m < vns`). Each applied fault is stamped into the recorded env at
            //    its true apply point (`vns == m`).
            while let Some((&m, _)) = self.schedule.range(..=vns).next() {
                if m < vns {
                    self.schedule_poisoned = Some((m, vns));
                    return Ok(Err(ControlError::ScheduleUnsatisfiable {
                        moment: m,
                        vtime: vns,
                    }));
                }
                let fault = self.schedule.remove(&m).expect("range key exists");
                // An apply failure (out-of-range gpa — pre-validated at stage time;
                // a reserved vector; an unwired LAPIC) is substrate-level breakage
                // of a vouched run: fail loud (session-fatal), never a silent skip
                // that would desync the recorded env from the run.
                let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
                vmm.apply_host_fault(&fault).map_err(ServeError::Vmm)?;
                self.recorded.perturb(fault, m);
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
            let next = self
                .schedule
                .keys()
                .next()
                .copied()
                .filter(|&m| until.deadline.is_none_or(|d| m <= d.0));
            match next {
                Some(m) => {
                    vmm.arm_arrival(m);
                }
                None => vmm.clear_arrival(),
            }

            // 4. Step. A terminal stop ends the run.
            match vmm.step()? {
                Step::Continued => {}
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
                    if let Some((&m, _)) = self.schedule.iter().next() {
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
}

/// Map a substrate [`TerminalReason`] to the wire [`StopReason`], stamped with
/// the effective V-time. Workload-blind (module doc): `Hlt` and a clean
/// `DebugExit{0}` are quiescence; a non-zero debug-exit code is a
/// guest-reported failure (`Crash{Panic}`, detail = the code byte); a backend
/// `Shutdown` (triple fault / guest-initiated shutdown) is `Crash{Shutdown}` —
/// a workload whose *clean terminal is a forced reboot* (the Postgres image)
/// reads as `Crash{Shutdown}` here, and interpreting that convention is the
/// workload-aware caller's job.
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
        Reply, Request, SnapId, StopConditions, StopMask, StopReason, VTime,
    };
    use environment::{BitMask, EnvSpec, FaultPolicy, HostFault as EnvHostFault};
    use vmm_backend::{Backend, Exit, MockBackend, Vtime};

    use proptest::prelude::*;

    use super::{ControlServer, ServeError, server_caps};
    use crate::vmm::{GuestRam, Vmm, VmmError, VtimeWiring, contract_vclock_config};
    use crate::work::ScriptedWork;

    const RAM: usize = 0x4000; // 16 KiB = 4 pages

    /// A configured, V-time-wired `Vmm<MockBackend>` with a distinctive memory
    /// image loaded and the canonical-blob hash wired (as the box composition
    /// does), advanced to a synchronized (post-RDTSC) boundary.
    fn vmm_at_sync(exits: Vec<Exit>, work: u64, seed: u64) -> Vmm<MockBackend> {
        let mut exits_with_sync = vec![Exit::Rdtsc];
        exits_with_sync.extend(exits);
        let mut m = MockBackend::with_exits(exits_with_sync);
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
            Ok(v)
        });
        ControlServer::new(live, factory)
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
        assert_eq!(caps.protocol_version, 1);
        assert_eq!(caps.env_version_min, EnvSpec::BLOB_VERSION);
        assert_eq!(caps.env_version_max, EnvSpec::BLOB_VERSION);
        assert_eq!(caps.coverage.map_bytes, 0, "no coverage producer exists");
        assert_eq!(caps.coverage.producer, 0);
        assert!(
            !caps.flags.contains(CapFlags::GUEST_HAS_SDK),
            "GUEST_HAS_SDK is off"
        );
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
}
