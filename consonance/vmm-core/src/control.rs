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
//!   but only its seed is *enforced*: an env carrying overrides or standing
//!   faults answers [`ControlError::Unsupported`] rather than silently running
//!   without them (they need the task-59 host-plane / task-61 guest-plane
//!   enforcement loops).
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
//! - **`perturb`** → [`ControlError::Unsupported`] (task 59 lights it up).
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
}

impl<B: Backend> ControlServer<B> {
    /// Build a server around a live VM. The [`SnapshotEngine`] is sized to the
    /// VM's guest-memory image; `factory` boots the fresh restore target for
    /// every `branch`/`replay` and must compose its VMs exactly like `vmm`
    /// (same RAM size, wiring, and contract — a mismatch is caught fail-closed
    /// by [`Vmm::restore_vm_state`] at the first restore).
    pub fn new(vmm: Vmm<B>, factory: VmmFactory<B>) -> Self {
        let engine = SnapshotEngine::new(vmm.guest_memory().len());
        ControlServer {
            vmm: Some(vmm),
            factory,
            engine,
            snaps: BTreeMap::new(),
            next_snap: 1,
            hello_done: false,
        }
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
            // Host-plane enforcement is task 59; staging a fault we cannot
            // apply would silently break replay.
            Request::Perturb { .. } => Ok(Err(ControlError::Unsupported)),
        }
    }

    /// `snapshot`: seal the current point into the engine as a base layer
    /// (memory image + canonical `vm_state` blob) and mint a wire handle.
    fn snapshot(&mut self) -> Result<Result<Reply, ControlError>, ServeError> {
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
                // Seed-driven scope: only the seed is enforceable. An env
                // carrying overrides, standing faults, or a **non-nominal fault
                // policy** must be REJECTED, not silently run without them — a
                // silent no-op would mint reproducers that do not reproduce. A
                // non-`none` `FaultPolicy` makes the seeded stream answer some
                // decisions with faults, which the seed-driven server has no
                // service wired to enforce (same class as the overrides
                // rejection). Tasks 59/61 light these up.
                let has_standing = matches!(
                    &spec,
                    EnvSpec::Recorded { standing, .. } if !standing.is_empty()
                );
                if !spec.overrides().is_empty()
                    || has_standing
                    || spec.policy() != &FaultPolicy::none()
                {
                    return Ok(Err(ControlError::Unsupported));
                }
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
        Ok(Ok(Reply::Unit))
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
    fn run(
        &mut self,
        until: &control_proto::StopConditions,
    ) -> Result<Result<Reply, ControlError>, ServeError> {
        let vmm = self.vmm.as_mut().ok_or(ServeError::Poisoned)?;
        loop {
            let vns = vmm.effective_vns().unwrap_or(0);
            if let Some(deadline) = until.deadline
                && vns >= deadline.0
            {
                return Ok(Ok(Reply::Stop(StopReason::Deadline { vtime: VTime(vns) })));
            }
            match vmm.step()? {
                Step::Continued => {}
                Step::Terminal(reason) => {
                    let vns = vmm.effective_vns().unwrap_or(0);
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
    use environment::{EnvSpec, FaultPolicy};
    use vmm_backend::{Backend, Exit, MockBackend};

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
    fn branch_rejects_an_env_with_overrides_or_standing_faults() {
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        let base = snap(&mut s);
        // An override-carrying env: the seed-driven server cannot enforce it,
        // and running without it would mint reproducers that do not reproduce.
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
            Err(ControlError::Unsupported),
            "overrides need the task-59/61 enforcement loops"
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
    fn perturb_is_unsupported_until_task_59() {
        let mut s = server(vec![Exit::Hlt]);
        hello(&mut s);
        let req = Request::Perturb {
            fault: HostFault(environment::HostFault::InjectInterrupt { vector: 32 }.encode()),
            at: Moment(1000),
        };
        assert_eq!(s.handle(&req).unwrap(), Err(ControlError::Unsupported));
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
}
