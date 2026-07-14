// SPDX-License-Identifier: AGPL-3.0-or-later
//! The deterministic VMM event loop, the owned guest-RAM backing, and the
//! all-observable-state hash.
//!
//! [`Vmm`] drives the vCPU **only** through [`vmm_backend::Backend::run`] and
//! dispatches the returned [`vmm_backend::Exit`] to the device shims and the
//! contract policy (default-deny: any unmodeled exit fails closed as a
//! [`VmmError::ContractViolation`], never a silent value). It is generic over the
//! backend, so the same loop runs the scripted `MockBackend` on macOS and a live
//! `KvmBackend` on the box. [`Vmm::state_hash`] is the M2 determinism hash over
//! all observable state.

use hypercall_proto::{
    MAX_PAYLOAD, NetFlowPoint, SeededEntropy, Service, ServiceId, Status, decode, encode_error,
    encode_response,
};
use sha2::{Digest, Sha256};
use vmm_backend::{Arch, ArchCaps, Backend, CommonExit, Exit, Moment};
use vtime::{IdlePlanner, VClock, VClockConfig};

use crate::vendor::Vendor;
use crate::work::{WorkError, WorkSource};

/// The engine's alias for the vCPU record set of the vendor `B` traps — how the
/// engine names "the register file" without naming an ISA.
pub type VcpuOf<B> = <<B as Backend>::A as Arch>::VcpuState;

/// Why a run stopped. M1 requires `DebugExit { code: 0 }` specifically — **not**
/// `Hlt` (the payload's fallback) and **not** a non-zero code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalReason {
    /// isa-debug-exit (`0xF4`) wrote `code`. PASS = 0, FAIL = 1.
    DebugExit {
        /// The code byte the guest wrote to `0xF4`.
        code: u8,
    },
    /// An idle halt nothing will wake (the payload's fallback when
    /// isa-debug-exit is absent, or the kernel's final `cli; hlt`) — terminal.
    Idle,
    /// Backend `Shutdown` (triple fault / explicit shutdown).
    Shutdown,
    /// The run stopped at a cooperating-SDK stop (task 73) — an assertion — rather
    /// than swallowing it (round-6): NOT a substrate terminal (the run could
    /// resume), and never latched as [`Vmm`]'s terminal. The stop's details are in
    /// [`RunResult::sdk_stop`].
    SdkStop,
}

/// Guest-physical address of the fixed hypercall **request** page. Mirrors
/// `vmcall_transport::REQ_GPA`.
const REQ_GPA: usize = 0x0000_E000;
/// Guest-physical address of the fixed hypercall **response** page. Mirrors
/// `vmcall_transport::RESP_GPA`.
const RESP_GPA: usize = 0x0000_F000;
/// The hypercall shared-page size (one frame per page). Mirrors
/// `vmcall_transport::PAGE_SIZE` == `hypercall_proto::MAX_FRAME`.
const HC_PAGE: usize = 4096;

// The SDK event-id wire layout (task 73), mirrored from `guest/sdk/src/wire.rs`
// (the canonical source). The doorbell needs only enough to route a stop: the
// namespace (top 8 bits of `event_id`) and the assert disposition byte.
const SDK_NS_SHIFT: u32 = 24;
const SDK_LOCAL_MASK: u32 = (1 << SDK_NS_SHIFT) - 1;
const SDK_NS_ASSERT: u8 = 1;
const SDK_NS_LIFECYCLE: u8 = 4;
const SDK_DISP_VIOLATION: u8 = 1;

/// A cooperating-SDK stop surfaced by the doorbell (task 73). The detail lives
/// here rather than in [`Step`] so `Step` stays `Copy`; the control server drains
/// it with [`Vmm::take_sdk_stop`] and maps it to the wire `StopReason`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SdkStop {
    /// An `assert_always` violation (or an `assert_unreachable` reached) — a bug.
    /// `id` is the assertion's catalog point id; `data` its detail bytes.
    Assertion {
        /// The assertion's catalog point id.
        id: u32,
        /// Opaque assertion detail bytes.
        data: Vec<u8>,
    },
    // NB: `setup_complete` no longer surfaces an immediate `SnapshotPoint` stop —
    // its doorbell OUT is unsealable, so it is **deferred** (see
    // `SdkChannel::pending_snapshot`) to the next synchronized boundary, surfaced
    // by the control loop as `StopReason::SnapshotPoint` there.
}

/// The host-side action a captured SDK Event emission drives, after
/// [`Vmm::classify_sdk_event`] validates its payload (task 73 seam 3, round-14).
#[derive(Clone, Debug, Eq, PartialEq)]
enum SdkEventAction {
    /// Surface a cooperating-SDK stop (an assert violation) as a bug.
    Stop(SdkStop),
    /// `setup_complete` (empty payload): arm the deferred snapshot point.
    DeferSnapshot,
    /// A well-formed non-stop emission: capture raw, take no host action.
    Capture,
    /// A malformed payload for an inspected namespace: reject, capture nothing —
    /// never synthesize a bug or a snapshot deferral from garbage.
    Malformed,
}

/// Errors that abort a run. A `ContractViolation` is the default-deny posture made
/// loud: an exit the skeleton does not model fails closed here — never silently.
#[derive(Debug, thiserror::Error)]
pub enum VmmError {
    /// A `Backend` operation failed.
    #[error("backend error")]
    Backend(#[from] vmm_backend::BackendError),
    /// A **vendor's boot stage** rejected the image: a malformed header, an image
    /// that does not fit the guest RAM, a bad entry state (x86: the Multiboot v1
    /// loader or the direct 64-bit Linux bzImage protocol — the boot path's trust
    /// boundary over untrusted image bytes).
    ///
    /// The cause is carried **opaquely**: which loaders a machine has is per-vendor
    /// (an ARM vendor loads an `Image` + DTB, and Multiboot is deleted for it, not
    /// ported — `docs/ARCH-BOUNDARY.md` §B), so the engine's error type must not
    /// enumerate one vendor's loaders. Construct it with
    /// [`VmmError::vendor_boot`]; the typed cause is still reachable through
    /// [`std::error::Error::source`] and `downcast_ref`.
    #[error("vendor boot error: {0}")]
    VendorBoot(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
    /// An exit the skeleton does not model (unmodeled port/MMIO/hypercall, a
    /// backend-dependent RDTSC/RDRAND, or an MSR access with no V-time backing).
    #[error("contract violation: {0}")]
    ContractViolation(String),
    /// The physical host fails one or more CPU-MSR-CONTRACT §1.1 host-homogeneity
    /// assertions (family/model/stepping, microcode, MXCSR-mask, MAXPHYADDR,
    /// RTM-disabled, or a variance-instruction absence). `boot` refuses to install
    /// the frozen policy or enter the guest on such a host — same-seed runs on a
    /// CPU outside the determinism domain would diverge in native instruction/FPU
    /// behavior while claiming the frozen contract. The string lists every failed
    /// assertion (expected vs. observed).
    #[error("host-baseline assertion failed: {0}")]
    HostAssert(String),
    /// The V-time work counter (`perf_event`) failed to read/reset, or reported
    /// an untrustworthy (multiplexed / unscheduled) count. Fail closed: a
    /// guest-visible TSC derived from a bad work read would silently diverge.
    #[error("work-counter error: {0}")]
    Work(#[from] WorkError),
    /// A V-time clock config was rejected (e.g. on snapshot restore). Never a
    /// panic — the malformed config is surfaced.
    #[error("v-time error: {0}")]
    Vtime(#[from] vtime::VtimeError),
    /// A live snapshot/branch operation failed: a `snapshot-store` error, a
    /// `vm_state` codec error, a malformed device blob, a LAPIC restore rejection,
    /// or a snapshot taken under a different CPU/MSR contract. Never a panic.
    #[error("snapshot error")]
    Snapshot(#[from] crate::snapshot::SnapshotError),
}

impl VmmError {
    /// Wrap a vendor boot/loader failure into the neutral
    /// [`VendorBoot`](VmmError::VendorBoot) variant. The engine never names a
    /// vendor's loader types; a vendor's composition root
    /// (e.g. [`crate::vendor::x86::bringup`]) calls this on its own error.
    pub fn vendor_boot<E>(err: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        VmmError::VendorBoot(Box::new(err))
    }
}

/// One serviced exit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Step {
    /// The exit was serviced; the run continues.
    Continued,
    /// The run reached a terminal state.
    Terminal(TerminalReason),
    /// A cooperating-SDK stop surfaced (task 73): an `assert` violation stops the
    /// run as a bug; a `setup_complete` stops it at a snapshot-fork point. The
    /// stop detail lives in the Vmm's SDK channel — drain it with
    /// [`Vmm::take_sdk_stop`]. Only ever produced when an SDK channel is wired.
    SdkStop,
}

/// What a completed run produced (and what the M2 hash is taken over).
pub struct RunResult {
    /// Why the run stopped.
    pub reason: TerminalReason,
    /// The cooperating-SDK stop the run halted at (task 73), if `reason` is
    /// [`TerminalReason::SdkStop`] — else `None`. `run` no longer swallows it.
    pub sdk_stop: Option<SdkStop>,
    /// The serial capture buffer, in order.
    pub serial: Vec<u8>,
    /// Per-exit-reason counts read from the backend (R-Backend observability).
    pub exit_counts: vmm_backend::ExitCounts,
}

/// The guest-RAM backing a [`Vmm`] owns — either a fresh allocation
/// ([`GuestRam`]) or, on the task-95 M2.2 **remap restore** path, the private
/// copy-on-write [`snapshot_store::Mapping`] a snapshot materialized into: the
/// mapping's buffer *is* the memory the backend's memslots register, so a
/// restore never memcpys the image into a second allocation — untouched pages
/// fault lazily from the mapping and guest writes stay private to this VM
/// (`MAP_PRIVATE`), never reaching the store or its tempfile.
///
/// Both variants uphold `map_memory`'s contract identically: page-aligned,
/// pinned (the mmap pages never move when the owning struct does), and live for
/// the backend's lifetime because the `Vmm` owns them.
pub enum RamBacking {
    /// A zeroed, owned allocation — the boot path, and the memcpy-restore path.
    Owned(GuestRam),
    /// A materialized snapshot's private CoW mapping — the remap-restore path.
    Snapshot(snapshot_store::Mapping),
}

impl RamBacking {
    /// The backing length in bytes.
    pub fn len(&self) -> usize {
        match self {
            RamBacking::Owned(ram) => ram.len(),
            RamBacking::Snapshot(map) => map.len(),
        }
    }

    /// Whether the backing is empty (never, for a well-formed VM).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The guest bytes (the [`Vmm::state_blob`] `MEM\0` chunk reads this).
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            RamBacking::Owned(ram) => ram.as_bytes(),
            RamBacking::Snapshot(map) => map.as_slice(),
        }
    }

    /// Mutable view (the loader / restore / host-fault write path).
    pub fn as_mut_bytes(&mut self) -> &mut [u8] {
        match self {
            RamBacking::Owned(ram) => ram.as_mut_bytes(),
            RamBacking::Snapshot(map) => map.as_mut_slice(),
        }
    }
}

/// Owned, pinned host backing for guest RAM. The backend registers a pointer
/// **into this buffer** via the `unsafe` [`vmm_backend::Backend::map_memory`], and
/// [`Vmm`] owns it so the backing **outlives every `run`** and [`Vmm::state_blob`]
/// can re-read materialized memory for the M2 hash. Allocated once and never
/// reallocated after mapping.
///
/// Off-Miri the backing is a page-aligned `mmap` (memmap2), which
/// `KVM_SET_USER_MEMORY_REGION` requires (a plain `Vec` is not guaranteed
/// page-aligned). Under Miri — which cannot execute `mmap` — it falls back to a
/// `Vec<u8>`; the mock backend's `map_memory` only records the slice, so the same
/// pointer/lifetime/bounds logic is still exercised by the interpreter.
pub struct GuestRam {
    #[cfg(not(miri))]
    inner: memmap2::MmapMut,
    #[cfg(miri)]
    inner: Vec<u8>,
}

impl GuestRam {
    /// Allocate `len` bytes (a multiple of 4 KiB) of zeroed, pinned backing.
    pub fn new(len: usize) -> Result<Self, VmmError> {
        if len == 0 || !len.is_multiple_of(4096) {
            return Err(VmmError::Backend(vmm_backend::BackendError::Memory(
                "guest RAM length must be a non-zero multiple of 4 KiB",
            )));
        }
        #[cfg(not(miri))]
        let inner = memmap2::MmapMut::map_anon(len)
            .map_err(|e| VmmError::Backend(vmm_backend::BackendError::Io(e)))?;
        #[cfg(miri)]
        let inner = vec![0u8; len];
        Ok(Self { inner })
    }

    /// The backing length in bytes.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the backing is empty (always `false` — `new` rejects zero length).
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// The materialized guest bytes — read by [`Vmm::state_blob`] for the M2 hash.
    pub fn as_bytes(&self) -> &[u8] {
        &self.inner
    }

    /// Mutable view for the loader / `write_boot_info` / `map_memory` (before the
    /// first run).
    pub fn as_mut_bytes(&mut self) -> &mut [u8] {
        &mut self.inner
    }
}

/// The V-time + seeded-RNG wiring for the **determinism-complete** path
/// (`PatchedKvmBackend`): the work→time [`VClock`], the host [`WorkSource`]
/// (retired-branch counter, read at each exit), and the [`SeededEntropy`] stream
/// `RDRAND`/`RDSEED` draw from. A [`Vmm`] holds this as `Option`; `None` (stock
/// KVM / M1/M2 payloads) means the four instruction exits are unmodeled — which
/// is correct, since stock KVM never surfaces them.
///
/// The seeded stream is the **same** one the `Entropy` hypercall service uses
/// (`hypercall-proto`), so a guest's `RDRAND` and its hypercall RNG cannot
/// diverge (task-21 P4). All of this lives **above** the `Backend` trait
/// (R-Backend): the backend only surfaces/completes the exits; the deterministic
/// values are computed here.
pub struct VtimeWiring {
    /// Retained so the clock can be rebuilt with a new `vns_base` on restore.
    pub(crate) cfg: VClockConfig,
    pub(crate) clock: VClock,
    pub(crate) work: Box<dyn WorkSource>,
    pub(crate) entropy: SeededEntropy,
    /// The work counter value read at the **last V-time intercept** — every
    /// determinism-cap trap (`RDTSC`/`RDTSCP`/`RDRAND`/`RDSEED`) and the
    /// `IA32_TSC`/`IA32_TSC_ADJUST` MSR paths — i.e. the synchronized point the
    /// patched backend corrects skid to. **Every** such intercept advances it (not
    /// just RDTSC); otherwise a checkpoint whose last intercept is, say, an RNG exit
    /// would hash a stale prior-intercept work value. This is **deterministic** across
    /// same-seed runs (every intercept's work is — that is why the guest's TSC reads
    /// match); a *live* counter read taken later (e.g. at hash time, post-terminal)
    /// instead carries the non-deterministic post-last-intercept exit-path skid.
    /// [`encode_vtime`] hashes the effective V-time derived from **this** value, so
    /// the `VTIM` chunk is byte-identical twice; it is **never** a live hash-time
    /// read. Reset to `0` by [`Vmm::restore_vtime`] (the effective V-time moves into
    /// `vns_base`; the work counter itself re-baselines at the NEXT guest entry via
    /// `start_run`, round-12). Starts at `0`: before the first intercept the effective
    /// V-time is exactly `vns_base`.
    pub(crate) last_intercept_work: u64,
    /// The signed offset added to the base V-time guest clock to form the
    /// **guest-visible** clock (`visible = VClock::guest_ticks + offset`, wrapping
    /// mod 2⁶⁴ as the architectural 64-bit counter does). `0` at reset and for
    /// every audited payload, so the visible clock is exactly
    /// `VClock::guest_ticks(work)`. The vendor's clock-offset register writes it
    /// (x86: `IA32_TSC_ADJUST`, and a `WRMSR(IA32_TSC, X)` that sets the visible
    /// clock to `X`). Stored as `u64` (two's-complement); hashed (it governs
    /// future clock output).
    pub(crate) guest_clock_offset: u64,
}

impl VtimeWiring {
    /// Build the wiring from a clock config, a work source, and an entropy seed.
    ///
    /// **Fails closed on a fractional work→ns ratio** (`ratio_den != 1`):
    /// [`save_vtime`](Vmm::save_vtime) records V-time in whole nanoseconds
    /// (`snapshot_vns`) and [`restore_vtime`](Vmm::restore_vtime) re-baselines the work
    /// counter (anchor to 0, effective V-time → `vns_base`), so a fractional ratio's
    /// sub-ns remainder `(work · num) mod den` would be silently lost across a snapshot — a
    /// restored clock would lag an un-snapshotted run. INTEGRATION.md §4 requires
    /// `ratio_den == 1` for any snapshot-bearing config (carrying the remainder is
    /// the §6 open question, deferred); the det-cfl-v1 contract clock is exact, so
    /// this only rejects misconfiguration.
    ///
    /// # Errors
    /// [`VmmError::ContractViolation`] if `ratio_den != 1`; [`VmmError::Vtime`] if
    /// `cfg` is otherwise invalid (zero ratio, immediate saturation).
    pub fn new(
        cfg: VClockConfig,
        work: Box<dyn WorkSource>,
        seed: u64,
    ) -> Result<VtimeWiring, VmmError> {
        if cfg.ratio_den != 1 {
            return Err(VmmError::ContractViolation(format!(
                "V-time ratio_den must be 1 (exact) for snapshot continuity; got {} — a \
                 fractional ratio loses the sub-ns remainder across a snapshot (INTEGRATION §4)",
                cfg.ratio_den
            )));
        }
        Ok(VtimeWiring {
            cfg,
            clock: VClock::new(cfg)?, // VtimeError → VmmError via #[from]
            work,
            entropy: SeededEntropy::new(seed),
            last_intercept_work: 0,
            guest_clock_offset: 0,
        })
    }

    /// Draw `width` (2/4/8) bytes from the seeded stream for an `RDRAND`/`RDSEED`
    /// completion, using the **exact** byte convention of the `Entropy`
    /// hypercall service (opcode 1, a `u32` count) so the two never diverge. The
    /// value is returned with the low `width` bytes set (the backend writes only
    /// those to the destination register).
    pub(crate) fn draw_rng(&mut self, width: u8) -> Result<u64, VmmError> {
        // The exit `width` is decoded from untrusted guest instruction bytes;
        // RDRAND/RDSEED only have 16/32/64-bit forms, so accept ONLY {2,4,8} and
        // fail closed on anything else (1/3/5/6/7/…) rather than service it.
        if !matches!(width, 2 | 4 | 8) {
            return Err(VmmError::ContractViolation(format!(
                "RDRAND/RDSEED width {width} invalid (only 2/4/8 are architectural)"
            )));
        }
        let n = usize::from(width);
        let mut buf = [0u8; 8];
        let req = (n as u32).to_le_bytes();
        let (status, got) = self.entropy.handle(1, &req, &mut buf[..n]);
        // Fail-closed defence. For the in-tree `SeededEntropy` this is unreachable
        // (a validated `n ∈ 1..=8` count + an `n`-byte buffer always yields
        // `(Ok, n)`), so the `||`→`&&` mutant here is provably equivalent and is
        // excluded in `.cargo/mutants.toml`; the `!=` halves stay mutation-gated.
        if status != Status::Ok || got != n {
            return Err(VmmError::ContractViolation(format!(
                "seeded entropy draw failed (status {status:?}, got {got} of {n} bytes)"
            )));
        }
        // `buf` is zero-initialized, so the low `width` bytes carry the draw and
        // the high bytes stay 0 (the backend masks to `width`).
        Ok(u64::from_le_bytes(buf))
    }

    /// Draw the SDK `entropy_fill` bytes from the **same** `SeededEntropy` stream
    /// RDRAND uses (round-5 P2), so a guest's RDRAND and its hypercall RNG cannot
    /// diverge or duplicate words. `req` is the `Entropy`-service request payload
    /// (a `u32` LE count), forwarded verbatim — the stream validates it and fills
    /// `resp`, returning `(status, bytes written)`.
    pub(crate) fn draw_entropy(&mut self, req: &[u8], resp: &mut [u8]) -> (Status, usize) {
        self.entropy.handle(1, req, resp)
    }

    /// The **guest-visible** clock at work `work`: the base V-time guest clock
    /// `VClock::guest_ticks(work)` plus
    /// [`guest_clock_offset`](Self::guest_clock_offset), wrapping mod 2⁶⁴ as the
    /// architectural 64-bit counter does. Every guest clock read the vendor
    /// dispatches (x86: `RDTSC`, `RDTSCP`, `RDMSR(IA32_TSC)`) resolves to this, so
    /// they agree exactly; with the default zero offset it is exactly
    /// `VClock::guest_ticks(work)`.
    pub(crate) fn guest_clock(&self, work: u64) -> u64 {
        self.clock
            .guest_ticks(work)
            .wrapping_add(self.guest_clock_offset)
    }
}

/// A V-time snapshot for mid-run save/restore (INTEGRATION.md §4): the effective
/// V-time in whole nanoseconds, the `IA32_TSC_ADJUST` register, and the entropy
/// stream position. On restore the hardware work counter restarts at 0 and `vns`
/// becomes the clock's `vns_base`, so the TSC continues exactly, `tsc_adjust` is
/// re-applied, and the RNG stream resumes where it left off.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VtimeSnapshot {
    /// The **exact** effective V-time in whole nanoseconds at the snapshot point
    /// (`VClock::snapshot_vns(last_intercept_work)`). [`Vmm::save_vtime`] only
    /// produces a snapshot at a V-time-intercept boundary, where `last_intercept_work`
    /// is the current work — so this is exact (restore resumes the TSC from it), never
    /// a stale last-intercept value.
    pub vns: u64,
    /// The guest clock-offset register at snapshot time (x86: `IA32_TSC_ADJUST`;
    /// the contract places it in `vm_state`), so a guest that wrote it
    /// snapshots/restores faithfully.
    pub guest_clock_offset: u64,
    /// `SeededEntropy::save_state()` (the PRNG position).
    pub entropy: Vec<u8>,
}

/// Upper bound on the diagnostic [`Vmm::preemption_landings`] trace, so a long-running
/// guest that preempts constantly (task 48 Postgres) cannot grow it unbounded. The trace
/// is observability only (not hashed); the task-47 gate payloads land far fewer than this
/// (`irq-landing` 8, `irq-landing-rng` 4). Recording stops at the cap.
const PREEMPTION_TRACE_CAP: usize = 4096;

/// The default pvclock staleness bound **Δ** for [`Vmm::enable_pvclock`], in
/// counted work units: 10,000,000 retired conditional branches ≈ **10 ms of
/// V-time** under the contract clock (1 branch = 1 ns, `docs/cpu-msr-contract`
/// via [`crate::vendor::x86::contract_vclock_config`]) — the same order as the
/// guest's 100 Hz periodic tick, so the forced refresh adds at most ~100
/// exits per virtual second on a fully compute-bound guest and typically zero
/// (any nearer timer deadline wins the `run_until` fold). The §6 perf
/// measurement sweeps this knob; harnesses override it per run via
/// [`Vmm::enable_pvclock`].
pub const PVCLOCK_DEFAULT_DELTA_WORK: u64 = 10_000_000;

/// Upper bound on the diagnostic pvclock refresh log
/// ([`Vmm::pvclock_refreshes`]) — the same cap as the landing traces. A gate
/// asserting per-refresh properties over a window must re-arm the log at the
/// window's start ([`Vmm::pvclock_clear_refreshes`]) and treat a saturated
/// window (`len() == this`) as a measurement failure, never a pass: a full
/// log proves only that at least this many refreshes happened, not that any
/// bound held.
pub const PVCLOCK_REFRESH_TRACE_CAP: usize = PREEMPTION_TRACE_CAP;

/// Which stamp [`Vmm::pvclock_stamp`] writes: the mid-run seqlock refresh, or
/// the seal-quiescent-point canonical form (§1.1 — `seq = 0`, zeroed tail).
#[derive(Clone, Copy, PartialEq, Eq)]
enum StampKind {
    Refresh,
    Canonical,
}

/// What an `CommonExit::Idle` should do, decided by [`Vmm::idle_action`] (task 52).
enum IdleAction {
    /// Terminal halt — `IF == 0`, off the determinism path, or no deliverable wake.
    Terminal,
    /// A deliverable interrupt is already pending in the LAPIC IRR: re-enter with **no**
    /// V-time change; the next service delivers it.
    DeliverPending,
    /// No interrupt pending now, but a deliverable timer is armed for this future V-time
    /// deadline (ns): jump the clock to it and re-enter.
    JumpToDeadline(u64),
}

/// The deterministic VMM, generic over `B: Backend`. **No method here mentions a
/// concrete backend.**
/// The task-73 SDK channel: the host-side state a cooperating guest's hypercall
/// doorbell drives. Wired per run by [`Vmm::enable_sdk`]; a guest that never
/// rings the doorbell leaves it untouched, and it is **never folded into the
/// state hash** (host-side observation, like the report stream), so an SDK-less
/// run's `state_hash` is byte-for-byte unchanged.
pub(crate) struct SdkChannel {
    /// Answers buggify decisions ([`DecisionPoint::Buggify`](environment::DecisionPoint)):
    /// materialized from the run's reproducer, so a seeded run draws from the
    /// seeded fault stream and a replay draws from the recorded overrides.
    env: environment::RecordedEnv,
    /// The `Moment`-stamped raw event stream (the link-tier capture): `(moment,
    /// event_id, data)` per SDK Event emission, in arrival order.
    events: Vec<(u64, u32, Vec<u8>)>,
    /// The buggify decisions this run resolved, `(moment, answer)`, for the
    /// control server to fold into the recorded reproducer.
    buggify: Vec<(u64, environment::Answer)>,
    /// A pending SDK stop to surface at the next step boundary.
    pending_stop: Option<SdkStop>,
    /// A `setup_complete` was seen but its doorbell `OUT` is **not** a sealable
    /// point (PMU-skid-tainted V-time — `save_vm_state` would report
    /// `NotQuiescent`). Deferred: the run surfaces `StopReason::SnapshotPoint` at
    /// the next V-time-synchronized boundary, where a seal actually succeeds — so
    /// the explorer never eagerly seals an unsealable point (round-4 P1).
    pending_snapshot: bool,
    /// The active [`FaultPolicy`](environment::FaultPolicy) bytes the channel was
    /// wired with — folded into the state hash (round-8) so two same-seed forks at
    /// the same stream position but with **different** buggify policies (a
    /// different fire probability / biasing) hash differently. The `RecordedEnv`
    /// carries the policy internally but exposes no accessor, so it is captured
    /// here from the caller's spec at `enable_sdk`.
    policy: Vec<u8>,
}

/// The task-61 `Net` channel: the host-side state the guest flow agent's
/// `net_decide` doorbell drives — the **decision log only**. Wired per run by
/// [`Vmm::enable_net`].
///
/// **Single decide-stream (the integrator ruling).** A `net_decide` answer is a
/// fault-schedule **input** the guest acts on (it enforces the per-flow policy on
/// the CNI) — the same category as a buggify decision, not a passive observation.
/// So a net decision draws from the **one** shared fault-decision stream the SDK
/// channel owns (materialized once, folded into `state_hash` via the `SDK\0`
/// chunk), exactly like buggify — the task-78 single-stream contract. The Net
/// channel therefore holds **no `env` of its own**; it only records the decisions.
/// The "inert guest" property is preserved: a flow-agent-less guest makes zero
/// `net_decide` calls, so it never advances the stream and its `state_hash` is
/// byte-for-byte unchanged (there is no `NET` hash chunk).
pub(crate) struct NetChannel {
    /// The per-flow decisions this run resolved: `(moment, conn, answer)`, in
    /// arrival order. Evidence the box gate reads (a flow decision appears at a
    /// stable `Moment` across two runs) and the control server folds into the
    /// recorded reproducer. Host-side capture (not itself hashed — the *stream
    /// advance* the decision caused is what the shared SDK stream position folds).
    decisions: Vec<(u64, u64, environment::Answer)>,
}

/// The task-110 paravirt work-derived clock channel (`docs/PARAVIRT-CLOCK.md`):
/// the host side of the materialized clock page. Offered per composition by
/// [`Vmm::enable_pvclock`]; the **guest** opts in by publishing its page GPA
/// over the hypercall doorbell ([`hypercall_proto::ServiceId::Pvclock`]), after
/// which the run loop re-stamps the page at every deterministic clock-advance
/// boundary ([`Vmm::pvclock_refresh`]) and the staleness bound Δ arms a forced
/// refresh exit ([`Vmm::pvclock_refresh_deadline`]). A guest that never
/// registers gets exactly today's behavior — no stamp is ever written and no
/// deadline is ever armed — and an un-offered composition is byte-for-byte
/// unchanged (the doorbell stays default-deny for it).
///
/// **State identity**: the page *bytes* live in guest RAM (already inside
/// `MEM\0`); the channel configuration (Δ + the registration) folds into
/// [`Vmm::state_blob`] as the `PVCK` chunk when offered — it governs future
/// guest-visible time, so two states identical in RAM but differing here must
/// hash differently (the SDK fault-policy precedent). Across snapshot/branch
/// the configuration is carried and cross-validated by the control server
/// ([`Vmm::pvclock_snapshot`] / [`Vmm::pvclock_restore`]), like the SDK
/// channel; the diagnostic refresh log stays out of the hash (like the
/// landing traces).
pub(crate) struct PvclockChannel {
    /// The staleness bound **Δ, in counted work units** (§2 point 4): with the
    /// page registered, the run loop never lets the guest execute more than Δ
    /// work beyond the last clock-advance boundary without a forced
    /// refresh exit. Trades resolution for exit rate; validated non-zero at
    /// [`Vmm::enable_pvclock`].
    delta_work: u64,
    /// The registered page GPA (page-aligned, wholly inside guest RAM, clear
    /// of the doorbell frame pages — validated at registration). `None` until
    /// the guest publishes one.
    gpa: Option<u64>,
    /// Diagnostic refresh log (**not** hashed, like
    /// [`Vmm::preemption_landings`]): `(work anchor, vns, guest_clock)` for
    /// every *value-publishing* stamp, **read back from the page bytes** after
    /// the write — so a stamping bug (wrong offset, wrong endianness, torn
    /// write) surfaces as a log/oracle mismatch, not a silently-wrong page.
    /// The G2 gate's evidence. Capped at [`PREEMPTION_TRACE_CAP`].
    refreshes: Vec<(u64, u64, u64)>,
}

/// The SDK channel's **replay-relevant** state, captured with a snapshot (task
/// 73): the seeded stream position and the emitted event log. Held by the
/// control server keyed by snapshot handle; restored on branch/replay so a fork
/// from a mid-run SDK snapshot reproduces (the seeded streams continue from the
/// right position) and keeps the declared catalog. Kilobytes, not a full state.
#[derive(Clone, Debug)]
pub struct SdkSnapshot {
    /// The seeded stream position (buggify fault + entropy supply), 16 bytes.
    stream: [u8; 16],
    /// The `Moment`-stamped event log emitted up to the snapshot (incl. the
    /// declared catalog), which a fork carries forward.
    events: Vec<(u64, u32, Vec<u8>)>,
    /// The deferred `setup_complete` snapshot-point flag
    /// ([`SdkChannel::pending_snapshot`]). Round-8 folds this into `state_blob`
    /// (the hash), so a verbatim replay MUST restore it — a snapshot sealed while
    /// it is `true` (an unarmed run that ran past `setup_complete` to a later
    /// sealable boundary) would otherwise restore to a state that hashes
    /// differently (the deferred point silently lost), breaking replay's
    /// round-trip hash equality.
    pending_snapshot: bool,
}

/// The task-61 `Net` channel's **replay-relevant** state, captured with a
/// snapshot: the **decision log only**. The flow-policy stream position is NOT
/// here — a net decision draws from the one shared fault stream the SDK channel
/// owns (the single-stream ruling), so that position is captured/restored exactly
/// once by [`SdkSnapshot`] and a fork's `net_decide` answers continue from it. The
/// Net snapshot just carries the decision log forward so a fork's decision
/// evidence is complete.
#[derive(Clone, Debug)]
pub struct NetSnapshot {
    /// The `(moment, conn, answer)` decision log up to the snapshot, carried
    /// forward so a fork's decision evidence is complete.
    decisions: Vec<(u64, u64, environment::Answer)>,
}

/// The task-110 pvclock channel's **replay-relevant** state, captured with a
/// snapshot ([`Vmm::pvclock_snapshot`], `Some` iff the page is offered): the
/// staleness bound Δ and the guest's registration. Both govern future
/// execution — Δ shapes the forced-refresh schedule, the registration is
/// where the vmm keeps stamping — so a restore must carry AND cross-validate
/// them ([`Vmm::pvclock_restore`]); the page bytes themselves ride the RAM
/// image. Held by the control server keyed by snapshot handle, like
/// [`SdkSnapshot`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PvclockSnapshot {
    /// The staleness bound Δ, in counted work units.
    delta_work: u64,
    /// The registered page GPA, if the guest had published one.
    gpa: Option<u64>,
}

pub struct Vmm<B: Backend>
where
    B::A: Vendor,
{
    pub(crate) backend: B,
    pub(crate) ram: RamBacking,
    /// The vendor's device state ([`Vendor::Devices`]) — the interrupt fabric,
    /// the platform shims, and the serial device. The engine never names one; it
    /// reaches them only through [`Vendor`], which is what makes the engine
    /// compiler-provably arch-blind (`docs/ARCH-BOUNDARY.md` §B).
    pub(crate) devices: <B::A as Vendor>::Devices,
    /// Guest frame numbers written **host-side** since the last
    /// [`Vmm::reset_dirty_tracking`] / [`Vmm::harvest_dirty_gfns`] drain (task 95
    /// M2.1). The backend's dirty log sees only *guest* writes (KVM tracks sptes,
    /// not the userspace mapping), so every place vmm-core itself writes guest
    /// RAM — the doorbell response page, a `CorruptMemory` host fault — records
    /// the touched gfns here, and the harvest unions them in. A `BTreeSet` so no
    /// order can reach the (already order-insensitive) capture. **Not hashed**
    /// (host bookkeeping, like the exit counters); the writes themselves are what
    /// the hash sees.
    pub(crate) host_dirty: std::collections::BTreeSet<u64>,
    /// Latched when guest RAM was host-written **wholesale or untrackably**
    /// ([`Vmm::restore_guest_memory`]'s full-image overwrite). While set,
    /// [`Vmm::harvest_dirty_gfns`] answers `None` — the safety rule: a dirty set
    /// that cannot be proven complete is never handed out; the caller full-scans.
    /// Cleared only by [`Vmm::reset_dirty_tracking`] (the caller's explicit
    /// "this state is my new baseline" arm point).
    pub(crate) host_dirty_wholesale: bool,
    /// The ordered **report stream** (corpus box-integration): every value the
    /// guest wrote to [`REPORT_PORT`] via `OUT`, in execution order. Each
    /// `report(u64)` payload call is two dwords (low then high). This is the
    /// guest-observable conformance output — it feeds [`Vmm::observable_digest`]
    /// (the O2/O3 digest), **not** [`Vmm::state_hash`] (the O1 full-state hash),
    /// so a stock / M1/M2 run that never touches the port leaves it empty and its
    /// `state_hash` is byte-for-byte unchanged from before this channel existed.
    pub(crate) report_stream: Vec<u32>,
    /// Diagnostic trace of the MEASURED preemption landings: the retired-branch work
    /// (`CommonExit::Deadline { reached }`) at which `run_until` actually delivered each LAPIC
    /// timer — the value the backend/VMM measured, NOT the ICR the guest programmed.
    /// **Not** hashed (observability only, like [`Self::report_stream`]); the task-47
    /// gate-2 seed-dependence assertion compares THIS (the actual landing work) across
    /// seeds, since the guest's self-reported ICR differs by seed for any backend (the
    /// RDRAND inputs differ) and so cannot prove seed-dependent *preemption*. Capped at
    /// [`PREEMPTION_TRACE_CAP`] so a long-running guest (task 48 Postgres, which preempts
    /// constantly) cannot grow it unbounded.
    pub(crate) preemption_landings: Vec<u64>,
    /// Diagnostic trace of the idle-resume landings (task 52): the **V-time** (ns) the
    /// clock was warped to when the guest went idle (`CommonExit::Idle` with `RFLAGS.IF == 1`
    /// and an armed timer) and [`Self::resume_idle`] jumped to the timer deadline. The
    /// dual of [`Self::preemption_landings`] — *jumped to* the next event instead of
    /// *executed to* it. It records the **landed V-time** (the deadline), **not** a work
    /// count: a `HLT` live work read is skid-tainted (task-27 O1), so the idle path never
    /// reads it; the landing is derived skid-free from the last-intercept anchor + the
    /// timer deadline. **Not** hashed (observability only); deterministic across same-seed
    /// runs and seed-dependent for a seed-consuming guest, so it witnesses the idle path
    /// engaged. Capped at [`PREEMPTION_TRACE_CAP`].
    pub(crate) idle_landings: Vec<u64>,
    pub(crate) terminal: Option<TerminalReason>,
    /// The vCPU state captured at terminal (so `state_blob` is consistent and the
    /// fallible `save` is resolved once, where errors can propagate from `run`).
    pub(crate) saved_state: Option<VcpuOf<B>>,
    /// V-time + seeded-RNG wiring for the determinism-complete path. `None` for
    /// stock KVM / M1/M2 (RDTSC/RNG never surface there).
    pub(crate) vtime: Option<VtimeWiring>,
    /// Set when the most-recently-serviced exit staged an **RNG** completion
    /// (RDRAND/RDSEED) whose seeded draw advanced the entropy stream but whose
    /// register-write/RIP-advance is only staged for the next `KVM_RUN` (not in
    /// `Backend::save`/`VtimeSnapshot`). Snapshotting here is unsound — restore
    /// would re-execute the instruction against the already-advanced stream and
    /// draw the *next* word. [`Vmm::save_vtime`] refuses at this boundary. Cleared
    /// at the next `step` (its re-entry commits the staged completion). RDTSC/
    /// RDTSCP/IO/MSR/CPUID completions are **idempotent on replay** (positional
    /// work / re-queried device-or-contract value), so they do not set this.
    pub(crate) rng_completion_staged: bool,
    /// `true` when the **last serviced exit staged *any* backend completion** (a
    /// read-style IO/MMIO load, an `Rdmsr`/`Wrmsr`, a `Cpuid`, or a determinism
    /// `Rdtsc`/`Rdtscp`/`Rdrand`/`Rdseed`) whose register-write/RIP-advance is only
    /// committed on the **next** `KVM_RUN`. Superset of [`Self::rng_completion_staged`]
    /// (which is the *non-idempotent* RNG subset). A snapshot may be *saved* at such a
    /// boundary for non-RNG exits (restore re-executes the instruction idempotently),
    /// but a snapshot must **not be restored into a backend that has one staged**: the
    /// pending completion lives in the backend's `kvm_run`, survives `Backend::restore`,
    /// and would commit the *old* exit's reg-write/RIP-advance on the next run — so
    /// [`Vmm::restore_vm_state`] requires a fresh/committed backend. Set after each
    /// `step`'s `run` from the serviced exit; `false` initially and after a restore.
    pub(crate) completion_staged: bool,
    /// `true` when the current point is a **V-time intercept boundary** — the last
    /// serviced exit was a V-time intercept (RDTSC/RDTSCP/RDRAND/RDSEED or a TSC
    /// MSR), or the VM is fresh (work 0) — so the **exact** effective V-time is known:
    /// `last_intercept_work` is the current, skid-corrected work. At any other exit
    /// (HLT/PIO/CPUID) the work retired since the last intercept is not
    /// deterministically measurable (skid), so the exact V-time is unknown.
    /// [`Vmm::save_vtime`] requires this (a snapshot's `vns` must be exact — restore
    /// resumes the TSC from it; §4), failing closed otherwise rather than recording a
    /// stale `vns`. Set `false` **before** each `step`'s `backend.run()` (so a failed
    /// run leaves it `false`, not stale-`true`) and back to `true` only by a
    /// V-time-intercept completion. **Not** part of the hash — `state_blob` is
    /// replay-equivalence *to the last intercept* and is correct at any exit (see
    /// [`encode_vtime`]); only the *snapshot* needs exactness here.
    pub(crate) vtime_synchronized: bool,
    /// `false` until the **first guest entry** (the first `backend.run()`, whether
    /// reached via [`step`](Vmm::step) or [`run`](Vmm::run)). On that first entry the
    /// work counter is prepared ([`WorkSource::start_run`](crate::work::WorkSource::start_run))
    /// so V-time work measures only this VM's guest execution — the box `perf_event`
    /// counter is enabled at open and counts guest branches on the *shared* vCPU
    /// thread, so a VM spawned before a coexisting VM runs would otherwise inherit
    /// that VM's branches. Gating on the real first entry (not the top of `run`) keeps
    /// a `step()`-then-`run()` consumer (telemetry/diagnostics) correct — it neither
    /// skips the early `step()` entries nor restarts work mid-run. Not hashed (a
    /// transient run-control flag, like `rng_completion_staged`/`vtime_synchronized`).
    pub(crate) first_entry_done: bool,
    /// When set ([`Vmm::wire_snapshot_hashing`]), [`Vmm::state_blob`] folds the
    /// **canonical `vm_state` encoding** into the hash as a `VMST` chunk — the
    /// snapshot/branch path's "the canonical `vm_state` blob drives `state_hash`"
    /// (BRINGUP). Default **off**, so M1/M2/corpus/Linux-boot blobs are byte-for-
    /// byte unchanged (their goldens do not move); a snapshot/branch consumer opts
    /// in. The chunk is the same bytes a [`Vmm::save_vm_state`] would seal, so two
    /// states whose canonical blob differs hash differently.
    pub(crate) snapshot_hashing: bool,
    /// The **host-fault arrival deadline** (task 59): an absolute retired-branch
    /// **work count** at which a staged [`HostFault`](environment::HostFault) is
    /// to be applied, armed by [`Vmm::arm_arrival`] and folded into
    /// [`step`](Vmm::step)'s `run_until` alongside the task-47 preemption
    /// deadline ([`Vmm::run_until_deadline`]). `None` (the default, and every
    /// protected M1/M2/corpus/Linux-boot path — none stages a fault) keeps
    /// `run_until` gated exactly as before, so those goldens are byte-for-byte
    /// unchanged. Like the preemption deadline it is a pure function of the
    /// (seed-deterministic) work axis, so arrival lands at the same instruction
    /// across same-seed runs.
    pub(crate) arrival_deadline: Option<Moment>,
    /// The task-73 SDK channel, wired per run by [`Vmm::enable_sdk`]. `None` for
    /// every non-SDK path (M1/M2/corpus/Linux-boot) — the doorbell then stays the
    /// default-deny contract violation and this field never touches the hash.
    pub(crate) sdk: Option<SdkChannel>,
    /// The task-61 `Net` channel, wired per run by [`Vmm::enable_net`]. `None` for
    /// every path without a flow agent — the doorbell then behaves exactly as
    /// before and this field never touches the hash.
    pub(crate) net: Option<NetChannel>,
    /// The task-110 paravirt clock channel, offered per composition by
    /// [`Vmm::enable_pvclock`]. `None` (the default) keeps every existing path
    /// byte-for-byte unchanged — the doorbell stays default-deny for the
    /// pvclock service, no page is ever stamped, no refresh deadline is armed.
    pub(crate) pvclock: Option<PvclockChannel>,
}

impl<B: Backend> Vmm<B>
where
    B::A: Vendor,
{
    /// Construct over an already-configured backend (CPUID/MSR-filter installed,
    /// entry state restored, RAM mapped) **and the [`GuestRam`] it owns**.
    pub fn new(backend: B, guest_ram: GuestRam) -> Self {
        Self::with_backing(backend, RamBacking::Owned(guest_ram))
    }

    /// The backend, for the vendor half's own dispatch (`pub(crate)`; the engine
    /// boundary, not a public accessor).
    pub(crate) fn backend(&self) -> &B {
        &self.backend
    }

    /// The vendor's device state ([`Vendor::Devices`]).
    pub(crate) fn devices(&self) -> &<B::A as Vendor>::Devices {
        &self.devices
    }

    /// Construct over an already-configured backend and **either** RAM backing —
    /// the [`RamBacking::Snapshot`] arm is the task-95 M2.2 remap-restore target
    /// (see [`crate::bringup::compose_restore_target`]). Same contract as
    /// [`Vmm::new`]: the backend's memslots must already point into `ram`'s
    /// buffer, which this `Vmm` now owns for the backend's lifetime.
    pub fn with_backing(backend: B, ram: RamBacking) -> Self {
        Self {
            backend,
            ram,
            devices: <B::A as Vendor>::new_devices(),
            host_dirty: std::collections::BTreeSet::new(),
            host_dirty_wholesale: false,
            report_stream: Vec::new(),
            preemption_landings: Vec::new(),
            idle_landings: Vec::new(),
            terminal: None,
            saved_state: None,
            vtime: None,
            rng_completion_staged: false,
            completion_staged: false,
            // A fresh VM is at work 0: the effective V-time is exactly `vns_base`, so
            // a snapshot here is exact (synchronized).
            vtime_synchronized: true,
            first_entry_done: false,
            snapshot_hashing: false,
            arrival_deadline: None,
            sdk: None,
            net: None,
            pvclock: None,
        }
    }

    /// Wire the determinism-complete V-time + seeded-RNG path (the
    /// `PatchedKvmBackend` composition root calls this; stock KVM leaves it
    /// unwired). After this, `RDTSC`/`RDTSCP` resolve to `VClock::guest_ticks(work)` and
    /// `RDRAND`/`RDSEED` to the seeded stream, instead of failing closed.
    pub fn wire_vtime(&mut self, wiring: VtimeWiring) -> &mut Self {
        self.vtime = Some(wiring);
        self
    }

    /// `true` once the determinism V-time path is wired.
    pub fn vtime_wired(&self) -> bool {
        self.vtime.is_some()
    }

    /// Opt this VMM into folding the **canonical `vm_state` blob** into
    /// [`Vmm::state_hash`] (a `VMST` chunk). Default off, so M1/M2/corpus/Linux-boot
    /// hashes are byte-for-byte unchanged; the snapshot/branch path calls this so a
    /// snapshot's `vm_state` integrity (not just the ad-hoc register layout) drives
    /// the determinism hash (task 39 Phase 1 / BRINGUP).
    pub fn wire_snapshot_hashing(&mut self) -> &mut Self {
        self.snapshot_hashing = true;
        self
    }

    /// `true` once the canonical-`vm_state` hash chunk is wired.
    pub fn snapshot_hashing_wired(&self) -> bool {
        self.snapshot_hashing
    }

    /// `true` iff a **genuine guest interrupt is pending delivery but not yet
    /// accepted** — a real identity raised into the vendor's interrupt fabric and
    /// re-arbitrated as deliverable (e.g. the periodic V-time timer), or a legacy
    /// line asserting — held in the inject seam awaiting the next safe VM-entry.
    ///
    /// This is the **architecturally in-flight event** the determinism overlay makes
    /// observable at a *synchronized* (snapshottable) boundary: unlike a backend
    /// injected-interrupt bit — which exists only at a non-synchronized
    /// interrupt-window exit, where [`Vmm::save_vm_state`] fails closed — a pending
    /// identity sits in the captured fabric state (device blob) and is **re-derived
    /// exactly** on restore. The live gate seals on this (or on
    /// [`Vmm::has_active_event_injection`]) to prove restore of a true in-flight
    /// event. Re-arbitrates but does not perturb the snapshot; `false` when no
    /// fabric is wired and no legacy line is asserting.
    pub fn has_pending_guest_interrupt(&mut self) -> Result<bool, VmmError> {
        <B::A as Vendor>::has_pending_guest_interrupt(self)
    }

    /// The current full guest-memory image (the owned [`RamBacking`]) — the
    /// memory half a snapshot captures into [`crate::snapshot::SnapshotEngine`].
    pub fn guest_memory(&self) -> &[u8] {
        self.ram.as_bytes()
    }

    /// `true` when this VM's guest RAM is a materialized snapshot's private CoW
    /// mapping ([`RamBacking::Snapshot`], the task-95 M2.2 remap restore) rather
    /// than an owned allocation — the gate evidence that a remap restore
    /// actually engaged (no full-image memcpy happened).
    pub fn ram_backing_is_snapshot(&self) -> bool {
        matches!(self.ram, RamBacking::Snapshot(_))
    }

    /// Inject bytes on the guest's serial input (the 8250 RBR) — the crude,
    /// off-record transport of task 81's `exec` improvisation. The bytes are
    /// consumed FIFO by the guest's serial shell as it reads the RBR; while any are
    /// queued, the COM1 receive line asserts (so an interrupt-driven console picks
    /// them up). **No determinism guarantee**: `exec` taints its timeline by ruling
    /// (`docs/RESOLUTION.md`), so this input is never recorded, hashed, or
    /// snapshotted. Inert for every run that never calls it.
    pub fn inject_serial_input(&mut self, bytes: &[u8]) {
        <B::A as Vendor>::inject_serial_input(&mut self.devices, bytes);
    }

    /// The serial output captured so far (the 8250 THR transmit stream) — the same
    /// buffer the snapshot adapter reads. Task 81's `exec` loop diffs this across
    /// steps to feed the completion-sentinel scanner.
    pub fn serial_output(&self) -> &[u8] {
        <B::A as Vendor>::serial_capture(&self.devices)
    }

    /// The current guest-visible vCPU register file, read **best-effort** and
    /// **without mutating** the VM — the substrate half of the task-80 `regs`
    /// observation verb. Returns the terminal-captured state if the VM is stopped
    /// at one, else a swallowing live `Backend::save` (default on a backend that
    /// cannot save). Identical to the vCPU state the hash folds in
    /// ([`current_vcpu`](Vmm::current_vcpu)), so a `regs` observation reports
    /// exactly the register file the determinism hash is taken over — but as a
    /// *view*, never the fallible snapshot seal ([`save_vm_state`](Vmm::save_vm_state),
    /// which fails closed at a non-synchronized boundary). Pairs with
    /// [`effective_vns`](Vmm::effective_vns) for the view's `Moment`/V-time.
    pub fn inspect_vcpu(&self) -> VcpuOf<B> {
        self.current_vcpu()
    }

    /// `true` iff the live vCPU is at a **non-quiescent** point — its `kvm_vcpu_events`
    /// carries an interrupt or exception KVM has injected but not yet delivered (or the
    /// `#PF`/`#DB` payload / `SIPI` / SMM / a queued triple fault) **in flight**. This is
    /// exactly the state task 39's quiescent-only snapshot codec **fail-closed-rejected**
    /// and task 41 now captures, so such a point is snapshottable. Exposed so a control
    /// plane or a box gate can quote a run's quiescent-vs-non-quiescent split (gate 1 —
    /// the before/after snapshottable counts) without reaching below the `Backend` trait.
    ///
    /// Reads the live vCPU **best-effort** (a `Backend::save` error reports `false`,
    /// matching [`Vmm::state_blob`]'s `current_vcpu`); the fallible snapshot path
    /// ([`Vmm::save_vm_state`]) reads it strictly instead. Does not mutate the VM.
    pub fn has_inflight_event_injection(&self) -> bool {
        <B::A as Vendor>::vcpu_has_inflight_injection(&self.current_vcpu())
    }

    /// `true` iff the live vCPU carries a **genuine in-flight event** — a real
    /// injected-or-pending bit (an injected interrupt/exception/NMI, a pending
    /// exception/NMI/SMI, a queued triple fault, or a valid SIPI), the *active* subset of
    /// [`Vmm::has_inflight_event_injection`].
    ///
    /// [`Vmm::has_inflight_event_injection`] reports the full task-39-would-reject set,
    /// which **also** fires on KVM's inert modifier residuals (a stale `interrupt.nr` /
    /// `exception.has_error_code` left set with every active bit clear). This reports only
    /// a *genuine* injection — an event KVM has committed to that the guest has not yet
    /// consumed — so a gate proving a non-quiescent snapshot seals on **this**, not on a
    /// residual (which collapses to the clean quiescent record under canonicalization).
    /// Best-effort read, like [`Vmm::has_inflight_event_injection`]; does not mutate the VM.
    pub fn has_active_event_injection(&self) -> bool {
        <B::A as Vendor>::vcpu_has_active_injection(&self.current_vcpu())
    }

    /// Overwrite the full guest-memory image on restore. `image` must be exactly the
    /// guest RAM size. On the box, KVM reads the guest through this same backing, so
    /// the restored memory is live on the next `KVM_RUN` — the host-side restore the
    /// memslot-remap optimization (task 08, below the trait) supersedes for O(dirty)
    /// latency (see `IMPLEMENTATION.md`); correctness is identical either way.
    ///
    /// # Errors
    /// [`VmmError::ContractViolation`] if `image.len()` is not the guest RAM size.
    pub fn restore_guest_memory(&mut self, image: &[u8]) -> Result<(), VmmError> {
        let ram = self.ram.as_mut_bytes();
        if image.len() != ram.len() {
            return Err(VmmError::ContractViolation(format!(
                "restore_guest_memory: image is {} bytes, guest RAM is {} bytes",
                image.len(),
                ram.len()
            )));
        }
        ram.copy_from_slice(image);
        // A full-image host write: per-gfn tracking is meaningless from here, so
        // poison the harvest (fail closed to the full scan) until the caller
        // re-arms at its next baseline (`reset_dirty_tracking`). The control
        // server's branch path does exactly that right after a restore.
        self.host_dirty_wholesale = true;
        Ok(())
    }

    /// Capture the V-time + entropy state for a mid-run snapshot (INTEGRATION.md
    /// §4). `Ok(None)` if V-time is unwired (nothing to capture). Pair with
    /// [`Vmm::restore_vtime`] (and the backend's `save`/`restore` + guest memory)
    /// to resume an identical timeline after a restore.
    ///
    /// **Clean-boundary invariant (must hold).** A snapshot is only sound at a
    /// boundary where **no RNG completion is staged**. `RDRAND`/`RDSEED` draw from
    /// the seeded stream eagerly (the value is needed to stage the completion), but
    /// the register-write/RIP-advance is only applied on the next `KVM_RUN` and is
    /// **not** captured by `Backend::save` / [`VtimeSnapshot`]. Snapshotting between
    /// the draw and that commit would, on restore, re-execute the instruction
    /// against the already-advanced stream and hand the guest the *next* word —
    /// divergence. So `save_vtime` **fails closed** there (the explorer steps to a
    /// clean boundary first). Capturing/replaying the staged completion for a true
    /// mid-exit snapshot is **task-08** (`snapshot-store`'s `vm_state` blob, which
    /// owns the backend-internal `complete_userspace_io` state). RDTSC/RDTSCP/IO/
    /// MSR/CPUID completions are idempotent on replay, so they are not guarded.
    ///
    /// **V-time-exactness invariant (must hold).** Unlike the hash, a snapshot's
    /// `vns` must be the **exact** effective V-time at the snapshot point — restore
    /// resumes the TSC from it (INTEGRATION.md §4), so an off-by-post-intercept-work
    /// `vns` is a *silently-wrong* restore (the next `RDTSC` reads low by the missed
    /// work). The exact V-time is known **only at a V-time intercept** — the
    /// synchronized, skid-corrected point where `last_intercept_work` *is* the current
    /// work. At any other exit (HLT/PIO/CPUID) the work retired since the last
    /// intercept is **not deterministically measurable** (skid; the box O1 evidence
    /// shows a terminal live read diverges), so the exact V-time is unknown and
    /// `save_vtime` **fails closed** (`vtime_synchronized == false`) rather than record
    /// a stale `vns`. (Project rule: never silently wrong.) **Integrator/design note:**
    /// this constrains the control plane to snapshot at V-time-intercept boundaries —
    /// the dissonance design snapshots at quiescent `HLT`, which is *not* such a point,
    /// so it needs either a backend skid-free quiescent work read (not established
    /// on-box for the cumulative read) or an intercept-aligned snapshot point.
    /// `IA32_TSC_ADJUST` is captured in the snapshot (the contract places
    /// TSC/TSC_ADJUST in `vm_state`), so a guest that wrote the MSR restores faithfully.
    ///
    /// **Clean-boundary invariant (must hold).** A snapshot is only sound where **no
    /// RNG completion is staged**. `RDRAND`/`RDSEED` draw from the seeded stream
    /// eagerly, but the register-write/RIP-advance is only applied on the next
    /// `KVM_RUN` and is **not** captured by `Backend::save` / [`VtimeSnapshot`].
    /// Snapshotting between the draw and that commit would, on restore, re-execute the
    /// instruction against the already-advanced stream and hand the guest the *next*
    /// word — divergence. So `save_vtime` **fails closed** there too. Full mid-exit
    /// capture is **task-08** (`snapshot-store`'s `vm_state`).
    ///
    /// # Errors
    /// [`VmmError::ContractViolation`] at an RNG mid-exit boundary or a non-synchronized
    /// (non-V-time-intercept) point.
    pub fn save_vtime(&self) -> Result<Option<VtimeSnapshot>, VmmError> {
        if self.rng_completion_staged {
            return Err(VmmError::ContractViolation(
                "save_vtime at an RNG mid-exit boundary: the seeded RDRAND/RDSEED draw advanced \
                 the stream but its completion is staged, not committed — snapshot only at a clean \
                 boundary (step once more first). Full mid-exit capture is task-08."
                    .to_string(),
            ));
        }
        match &self.vtime {
            None => Ok(None),
            Some(vt) => {
                // Fail closed unless the exact V-time is known (a V-time intercept /
                // fresh / just-restored). At a non-V-time exit the post-intercept work
                // is skid — not deterministically measurable — so a snapshot here would
                // resume the TSC from the wrong point (silently-wrong restore, §4).
                if !self.vtime_synchronized {
                    return Err(VmmError::ContractViolation(
                        "save_vtime at a non-synchronized point: the exact V-time is known only at \
                         a V-time intercept (RDTSC/RDTSCP/RDRAND/RDSEED or a TSC MSR); branches \
                         retired since the last intercept are not deterministically measurable \
                         (skid), so a snapshot here would resume the TSC from the wrong point. \
                         Snapshot at a V-time-intercept boundary."
                            .to_string(),
                    ));
                }
                // Exact: at a synchronized point `last_intercept_work` is the current
                // work, so `snapshot_vns(last_intercept_work)` is the exact V-time.
                Ok(Some(VtimeSnapshot {
                    vns: vt.clock.snapshot_vns(vt.last_intercept_work),
                    guest_clock_offset: vt.guest_clock_offset,
                    entropy: vt.entropy.save_state(),
                }))
            }
        }
    }

    /// The **effective V-time** in whole nanoseconds — `snapshot_vns` of the
    /// deterministic last-intercept anchor, i.e. exactly the V-time the `VTIM`
    /// hash chunk folds in (see [`Vmm::state_blob`]) — or `None` when the
    /// determinism path is not wired. Skid-free (never a live counter read) and
    /// identical across same-seed runs at the same point, so the control
    /// transport's `run(until)` deadline check (task 58) can compare it against a
    /// V-time deadline without perturbing determinism. Unlike
    /// [`Vmm::save_vtime`] it is **total**: at a non-synchronized point it
    /// reports the last-intercept V-time (a lower bound on the true V-time) —
    /// fine for a monotone deadline check, but never a snapshot's `vns` (that
    /// exactness is `save_vtime`'s job, which fails closed instead).
    pub fn effective_vns(&self) -> Option<u64> {
        self.vtime
            .as_ref()
            .map(|vt| vt.clock.snapshot_vns(vt.last_intercept_work))
    }

    /// `true` iff [`effective_vns`](Vmm::effective_vns) is **exact** — the VM is at a
    /// V-time-intercept boundary (`RDTSC`/`RDTSCP`/`RDRAND`/`RDSEED` / a TSC MSR / an
    /// exact-count `run_until` `Deadline`, or fresh / just-restored), so
    /// `last_intercept_work` *is* the current retired count. At any other point (a
    /// terminal `HLT`, a `Shutdown`/debug exit, a serial/MMIO exit) the guest may
    /// have retired branches since the last intercept, so `effective_vns` is only a
    /// **lower bound** and this is `false`.
    ///
    /// The control plane (PR #51 round-7) requires this wherever it trusts
    /// `effective_vns` as an exact position — the `perturb` floor and the `m == vns`
    /// exact-arrival drain — so a fault is never recorded at a `Moment` the guest has
    /// already executed past (the same exactness [`save_vtime`](Vmm::save_vtime) fails
    /// closed on). `false` when V-time is unwired.
    pub fn is_synchronized(&self) -> bool {
        self.vtime.is_some() && self.vtime_synchronized
    }

    /// The **boundary** preconditions [`Vmm::save_vm_state`] requires to seal a
    /// snapshot: no staged RNG completion (`rng_completion_staged`), and — when
    /// V-time is wired — at a `vtime_synchronized` intercept. This is the SINGLE
    /// source of truth both `save_vm_state` and the deferred SDK snapshot-point
    /// gate ([`Vmm::take_synchronized_snapshot_point`]) consult, so "can I seal
    /// here?" can never drift from what `save_vm_state` actually accepts (round-4
    /// P1: the snapshot point used to gate on `is_synchronized()` alone, which
    /// does NOT exclude a staged RNG completion, so it surfaced points the seal
    /// then rejected). NOT included here: the vCPU-state representability check
    /// (`unrepresentable_state`) — that is a property of the captured state, not
    /// the boundary.
    pub(crate) fn can_snapshot(&self) -> bool {
        !self.rng_completion_staged && (self.vtime.is_none() || self.vtime_synchronized)
    }

    /// Restore the V-time + entropy state captured by [`Vmm::save_vtime`]: rebuild
    /// the clock with `vns_base = snap.vns`, **reset the hardware work counter to
    /// 0**, re-apply `IA32_TSC_ADJUST` from the snapshot, and restore the entropy
    /// stream position. `tsc(work)` then continues from exactly the snapshotted
    /// V-time (INTEGRATION.md §4).
    ///
    /// **Fails closed at an RNG mid-exit boundary** (`rng_completion_staged`),
    /// symmetric with [`Vmm::save_vtime`]: a seeded RDRAND/RDSEED completion is
    /// staged in the backend (not yet committed) and is **not** undone by a V-time
    /// restore, so rewinding the entropy stream here would let that stale completion
    /// commit against the restored stream on the next run — shifted draws. The flag
    /// is **not** cleared here (that would falsely declare the backend clean while
    /// its staged completion is still pending); it is cleared only by the next
    /// `step`'s re-entry, which actually commits the completion, or by the full
    /// backend/`vm_state` restore (task-08) that discards it. So restore only at a
    /// clean boundary (item 3: at a clean boundary a restore-then-`save_vtime`
    /// succeeds, because the flag was already clear — nothing to clear).
    ///
    /// # Errors
    /// [`VmmError::ContractViolation`] if V-time is unwired, at an RNG mid-exit
    /// boundary, or if the entropy blob is rejected; [`VmmError::Vtime`]/
    /// [`VmmError::Work`] on a clock/counter error. **Atomic:** every fallible step
    /// that can reject an untrusted `snap` (the clock-config rebuild and the
    /// entropy-blob validation) runs **before** any live state is mutated, so a bad
    /// snapshot leaves the timeline fully intact rather than half-restored.
    pub fn restore_vtime(&mut self, snap: &VtimeSnapshot) -> Result<(), VmmError> {
        // 0. Refuse at an RNG mid-exit boundary (symmetric with `save_vtime`): a
        //    staged RDRAND/RDSEED completion lives in the backend and is not undone
        //    by a V-time restore, so rewinding entropy now would shift the next
        //    draw. The flag clears on the next `step`'s commit (or the task-08
        //    backend restore); do not clear it here (that would mask the pending
        //    completion). At a clean boundary the flag is already false.
        if self.rng_completion_staged {
            return Err(VmmError::ContractViolation(
                "restore_vtime at an RNG mid-exit boundary: a seeded RDRAND/RDSEED completion is \
                 staged (not committed) — rewinding V-time here would shift the next draw. Restore \
                 only at a clean boundary (step once more first)."
                    .to_string(),
            ));
        }
        // 1. Validate, committing nothing. Rebuild the clock (validates the cfg)
        //    and validate the entropy blob into a CLONE (its `restore_state`
        //    rejects a malformed/untrusted blob without touching the live stream).
        //    Scoped read-only borrow of `self.vtime`, dropped before any mutation.
        let (clock, cfg, entropy) = {
            let vt = self.vtime.as_ref().ok_or_else(|| {
                VmmError::ContractViolation(
                    "restore_vtime called but V-time is not wired".to_string(),
                )
            })?;
            let mut cfg = vt.cfg;
            cfg.vns_base = snap.vns;
            let clock = VClock::new(cfg)?;
            let mut entropy = vt.entropy.clone();
            entropy.restore_state(&snap.entropy).map_err(|e| {
                VmmError::ContractViolation(format!("entropy snapshot rejected on restore: {e:?}"))
            })?;
            (clock, cfg, entropy)
        };
        // 2. ATOMICITY (P3 round-12, refined). The backend save+restore round-trip — which
        //    re-arms counter B — is the SOLE HARD-FALLIBLE mutation, done BEFORE any commit.
        //    Round-11 had TWO hard-fallible mutations (this round-trip for B AND
        //    `vt.work.reset()` for A): if `work.reset()` failed AFTER the round-trip, B was
        //    re-armed but A was not → the next entry reset only B → B≡A broke on a failed
        //    restore. The fix demotes A's counter reset to BEST-EFFORT (step 4 below), so the
        //    round-trip is the only thing that can fail-and-abort here: `restore`'s
        //    `reset_arm.rearm()` is its LAST step, so a failure leaves B unchanged → if the
        //    round-trip errors, NOTHING below runs and the VM is byte-for-byte untouched
        //    (true all-or-nothing). `save`->`restore` is a vCPU identity, so the hash is
        //    unchanged. (B is unreachable directly — the production backend is
        //    `Box<dyn Backend<A = X86>>` and the FROZEN trait must not grow a re-arm method.)
        let vcpu = self.backend.save()?;
        self.backend.restore(&vcpu)?;
        // 3. Commit the validated state — ALL infallible (the round-trip above was the last
        //    HARD-fallible step), so the commit is true all-or-nothing. The snapshot's
        //    effective V-time lives in `cfg.vns_base` and the last-intercept anchor resets to
        //    0 (effective V-time = `vns_base` until the next intercept advances work) —
        //    keeping a restored VM byte-identical to a fresh one at the same effective V-time
        //    (task-27 item 2). The guest clock offset is re-applied from the snapshot.
        let vt = self.vtime.as_mut().ok_or_else(|| {
            VmmError::ContractViolation("restore_vtime called but V-time is not wired".to_string())
        })?;
        vt.clock = clock;
        vt.cfg = cfg;
        vt.entropy = entropy;
        vt.last_intercept_work = 0;
        vt.guest_clock_offset = snap.guest_clock_offset;
        // 4. Re-baseline counter A. `vt.work.reset()` is BEST-EFFORT (its error is NOT a
        //    hard failure — that is what keeps the round-trip above the SOLE abort point, so
        //    A and B can never end up re-armed-XOR-not): it gives the portable `ScriptedWork`
        //    (whose `start_run` is a no-op) its immediate zero — and `ScriptedWork::reset` is
        //    infallible, so it never actually fails there. The box `PerfWorkCounter` ALSO
        //    re-zeroes at the next entry via `start_run` (== the same `IOC_RESET`) because
        //    `first_entry_done = false` below, so a failed `IOC_RESET` here is recovered (and
        //    re-surfaced) at that entry, and nothing reads live `work()` before it
        //    (save_vtime / state_hash use `last_intercept_work` = 0).
        let _ = vt.work.reset();
        // The restored VM's effective V-time is exactly `vns_base` (anchored at
        // `last_intercept_work = 0`), a synchronized point — an immediate `save_vtime` is
        // exact. (`vt`'s borrow of `self.vtime` has ended by this disjoint-field write.)
        self.vtime_synchronized = true;
        // Counter A re-baselines at the next entry: re-arm the first-entry gate so the next
        // `step` calls `WorkSource::start_run` (exactly like a full `restore_vm_state`).
        // Counter B was re-armed by the `restore` in step 2. Both re-baseline at the next
        // REAL entry, so a coexisting VM on the shared pinned thread in between contaminates
        // neither (B≡A) — and because this re-arm and the V-time commit are INFALLIBLE and
        // run only AFTER the sole hard-fallible round-trip, B and A are re-armed together or
        // not at all.
        self.first_entry_done = false;
        // A restore/rebase resets the timeline, so any host-fault arrival deadline
        // armed against the PRE-restore V-time is stale — clear it (mirror
        // `clear_arrival`), else it would bound the first post-restore `step` at a
        // now-meaningless work count (the #34/#55 stale-arm class; PR #51 round-3).
        self.arrival_deadline = None;
        Ok(())
    }

    // --- full vm_state snapshot / restore (task 39) ------------------------

    /// Capture the **non-memory** machine state as a canonical [`vm_state::VmState`]
    /// (INTEGRATION.md §4) — pair with [`Vmm::guest_memory`] +
    /// [`crate::snapshot::SnapshotEngine`] for the memory half. The vmm-core adapter
    /// that fills `vm-state`'s plain-data structs from the live machine and
    /// `VmState::encode`s them (task 39 Phase 1).
    ///
    /// **Non-quiescent capture (task 41).** A snapshot no longer requires a *quiescent*
    /// machine: the **full** `kvm_vcpu_events` — an interrupt or exception KVM has
    /// injected but not yet delivered, the `#PF`/`#DB` payload, SMM, triple-fault — is
    /// captured verbatim (device blob) and re-established on restore, so a point with
    /// an interrupt **in flight** is now snapshottable rather than fail-closed-rejected.
    /// The LAPIC IRR/ISR is captured too, and the backend's per-entry `set_pending_irq`
    /// slot is re-derived from the restored LAPIC / UART on the restored VM's first
    /// service — so there is no separate injection plan to serialize.
    ///
    /// **Two boundary guards remain** (they are about V-time/RNG *exactness*, not about
    /// the machine being idle). `save_vm_state` **fails closed** (a) at an RNG mid-exit
    /// boundary (`rng_completion_staged`: a seeded RDRAND/RDSEED draw advanced the
    /// stream but its completion is only staged, not committed — restoring there would
    /// re-draw), and (b), when V-time is wired, at a non-`vtime_synchronized` point (the
    /// exact V-time the restored TSC resumes from is known only at a V-time intercept).
    /// These are the same guards [`Vmm::save_vtime`] enforces, and are the deliberate
    /// "staged completion is defined out" choice — a non-idempotent staged completion is
    /// excluded by snapshotting only at a clean, synchronized boundary, of which an
    /// interrupt-driven guest has many (every RDTSC the workload takes).
    ///
    /// **Canonical pvclock re-stamp (task 110, §1.1).** A successful save is a
    /// seal quiescent point, so — when a clock page is registered — the page is
    /// re-stamped to canonical form (`seq = 0`, values at the exact seal work
    /// count, reserved tail zeroed) here, after **every** rejection path (the
    /// boundary guards, the fallible vCPU read, and the sealability check) has
    /// passed — a rejected seal attempt mutates nothing (reject-before-
    /// mutation), so the `NotQuiescent` retry loops and sealability probes are
    /// side-effect-free. That is why this takes `&mut self`: a *successful*
    /// seal mutates one page of guest RAM (value-preserving — only the seqlock
    /// epoch and any guest scribbles reset), and every seal path shares this
    /// chokepoint, so the RAM image any caller captures next is canonical. The
    /// page carries zero refresh-history entropy in a sealed image.
    ///
    /// # Errors
    /// [`VmmError::ContractViolation`] at an RNG mid-exit boundary, a non-synchronized
    /// point, or if the live vCPU carries the PAE-only `kvm_sregs2` flags/pdptrs or
    /// `debugregs.flags` (all zero for the 64-bit determinism guest — see
    /// [`crate::snapshot::unrepresentable_state`]); [`VmmError::Backend`] if reading the
    /// live vCPU state fails (a snapshot **fails closed** rather than sealing a zeroed or
    /// lossy vCPU).
    pub fn save_vm_state(&mut self) -> Result<vm_state::VmState, VmmError> {
        // The boundary gate is the shared `can_snapshot()` predicate (so the SDK
        // snapshot-point surface can never advertise a point this rejects); when
        // it fails, report WHICH precondition failed for a precise diagnostic.
        if !self.can_snapshot() {
            if self.rng_completion_staged {
                return Err(VmmError::ContractViolation(
                    "save_vm_state at an RNG mid-exit boundary: the seeded RDRAND/RDSEED draw \
                     advanced the stream but its completion is staged, not committed — snapshot \
                     only at a clean boundary (step once more first)."
                        .to_string(),
                ));
            }
            return Err(VmmError::ContractViolation(
                "save_vm_state at a non-synchronized point: the exact V-time (which a restored TSC \
                 resumes from) is known only at a V-time intercept (RDTSC/RDTSCP/RDRAND/RDSEED or a \
                 TSC MSR). Snapshot at a V-time-intercept boundary."
                    .to_string(),
            ));
        }
        // Read the vCPU **fallibly**: a `Backend::save` failure must abort the
        // snapshot, not seal a `VcpuState::default()` (the swallowing `current_vcpu`
        // does for the best-effort hash). Use the terminal-captured state if present.
        let vcpu = match &self.saved_state {
            Some(s) => s.clone(),
            None => self.backend.save()?,
        };
        // Fail closed on machine state the representable `vm_state` subset would
        // silently zero on restore (`kvm_sregs2` flags/pdptrs, or pending-event
        // injection/SMM/triple-fault bookkeeping) — sealing a lossy blob is worse
        // than refusing it. Zero at a real quiescent snapshot point (64-bit guest, no
        // armed injection); a non-zero value is a misuse / a non-quiescent snapshot.
        <B::A as Vendor>::check_sealable_vcpu(&vcpu)?;
        // Task 110 (§1.1): the seal is quiescent, synchronized, AND now fully
        // validated, so re-stamp the pvclock page to canonical form at the
        // exact seal work count — a no-op without a registration, value-
        // preserving with one. Deliberately AFTER every rejection path above
        // (reject-before-mutation, the PR #12 round-8 posture): a seal attempt
        // the caller treats as recoverable (`NotQuiescent` retry loops, the
        // sealability probes) must leave guest RAM and the refresh history
        // byte-for-byte untouched. `build_vm_state` below is infallible, so a
        // canonicalized page never outlives a failed seal.
        self.pvclock_stamp(StampKind::Canonical)?;
        Ok(<B::A as Vendor>::build_vm_state(self, &vcpu))
    }

    /// Restore the **non-memory** machine state from a [`vm_state::VmState`] (pair
    /// with [`Vmm::restore_guest_memory`]; or use [`Vmm::restore_snapshot`] for
    /// both). Decodes the typed records back into the vCPU via `Backend::restore`,
    /// resumes the V-time clock (`vns_base` = the snapshot's V-time, the hardware
    /// counter reset to 0) + entropy stream + `IA32_TSC_ADJUST`, and restores the
    /// xAPIC + legacy platform + UART from the device blob.
    ///
    /// **Atomic on rejection.** Every fallible step that can reject an untrusted blob
    /// — the `contract_hash` check, the device-blob decode, the LAPIC coherence
    /// check, the clock rebuild, and the entropy-blob validation — runs **before**
    /// any live state is mutated, so a bad snapshot leaves the VM fully intact rather
    /// than half-restored. Refuses at an RNG mid-exit boundary (symmetric with
    /// [`Vmm::restore_vtime`]).
    ///
    /// # Errors
    /// [`VmmError::Snapshot`] for a contract mismatch / malformed device blob /
    /// rejected LAPIC; [`VmmError::ContractViolation`] at an RNG mid-exit boundary,
    /// a V-time wiring/rate mismatch, or a rejected entropy blob;
    /// [`VmmError::Backend`]/[`VmmError::Vtime`]/[`VmmError::Work`] from the
    /// backend/clock/counter.
    pub fn restore_vm_state(&mut self, s: &vm_state::VmState) -> Result<(), VmmError> {
        // 0. Refuse if **any** backend completion is staged (not just RNG). A
        //    read-style / MSR / CPUID / determinism exit this VM serviced leaves a
        //    pending reg-write/RIP-advance in the backend's `kvm_run`; `Backend::restore`
        //    does not clear it, so the next run would commit the *old* exit's
        //    completion over the restored state. Restore only into a fresh or committed
        //    backend (step once more to commit, or restore into a freshly-booted VM).
        if self.completion_staged {
            return Err(VmmError::ContractViolation(
                "restore_vm_state into a backend with a staged completion: the VM just serviced a \
                 read/MSR/CPUID/determinism exit whose completion is pending in kvm_run and is not \
                 cleared by restore — it would commit the old exit on the next run. Restore only \
                 into a fresh or committed backend (step once more, or use a freshly-booted VM)."
                    .to_string(),
            ));
        }
        // 1. Validate, committing nothing.
        // 1a-bis. A non-empty timer queue cannot be applied: the engine has no
        //     `vtime::TimerQueue` (the only timer is the vendor fabric's, carried in
        //     the device blob), so a non-default `timers` section would be silently
        //     dropped. Fail closed (a well-formed vmm-core blob always seals it empty).
        if s.timers != vm_state::TimerQueueState::default() {
            return Err(VmmError::ContractViolation(
                "restore_vm_state: snapshot carries a non-empty timer queue, but vmm-core has no \
                 TimerQueue to apply it — restoring would silently drop it. (A vmm-core snapshot \
                 always seals an empty timer queue; the fabric timer rides the device blob.)"
                    .to_string(),
            ));
        }
        // 1b. The vendor half: the contract hash, the device blob, the event records,
        //     and the fabric/platform wiring coherence — all validated **without
        //     mutating anything**, so a bad snapshot leaves the VM fully intact. It
        //     yields the decoded vCPU record set (events already canonicalized for
        //     restore), the guest clock-offset register the engine re-applies with its
        //     V-time commit, and the prepared devices.
        let (vcpu, clock_offset, prep) = <B::A as Vendor>::validate_restore(self, s)?;
        // 1c. V-time: validate the rate matches and pre-build the clock + entropy.
        let vtime_commit = match self.vtime.as_ref() {
            Some(vt) => {
                if s.vtime.ratio_num != vt.cfg.ratio_num
                    || s.vtime.ratio_den != 1
                    || s.vtime.guest_hz != vt.cfg.guest_hz
                    || s.vtime.guest_base != vt.cfg.guest_base
                {
                    return Err(VmmError::ContractViolation(
                        "restore_vm_state: V-time clock-rate mismatch (the snapshot's ratio/guest_hz/\
                         guest_base differ from this VM's wired clock)."
                            .to_string(),
                    ));
                }
                let mut cfg = vt.cfg;
                cfg.vns_base = s.vtime.snapshot_vns;
                let clock = VClock::new(cfg)?;
                let mut entropy = vt.entropy.clone();
                entropy.restore_state(&s.hypercall).map_err(|e| {
                    VmmError::ContractViolation(format!(
                        "entropy snapshot rejected on restore: {e:?}"
                    ))
                })?;
                Some((cfg, clock, entropy))
            }
            None => {
                // Unwired VM: the snapshot must not carry a live V-time block (a
                // non-zero clock rate means it was taken on a V-time-wired VM).
                if s.vtime.guest_hz != 0 || s.vtime.snapshot_vns != 0 {
                    return Err(VmmError::ContractViolation(
                        "restore_vm_state: snapshot carries a V-time block but this VM has no V-time \
                         wired — restore into a VM composed like the snapshot source."
                            .to_string(),
                    ));
                }
                None
            }
        };
        // 3. Commit the fallible backend restore first — a failure here leaves the
        //    V-time/device state untouched (nothing below this line can reject the
        //    blob; only the hardware counter reset can fail, infrastructurally).
        self.backend.restore(&vcpu)?;
        if vtime_commit.is_some() {
            // The hardware work counter restarts at 0; the snapshot's V-time now
            // lives in `vns_base` (continuity, INTEGRATION.md §4 / restore-transparency).
            self.vtime
                .as_mut()
                .expect("vtime_commit implies wired")
                .work
                .reset()?;
        }
        // 4. Commit the validated state (all infallible from here).
        if let Some((cfg, clock, entropy)) = vtime_commit {
            let vt = self.vtime.as_mut().expect("vtime_commit implies wired");
            vt.cfg = cfg;
            vt.clock = clock;
            vt.entropy = entropy;
            vt.last_intercept_work = 0;
            vt.guest_clock_offset = clock_offset;
            self.vtime_synchronized = true;
        }
        // The vendor half of the commit (all infallible): the prepared fabric /
        // platform / serial devices, and the restored guest-observable report stream
        // (so a branch resumes the guest's `observable_digest` / O2 signal instead of
        // losing every report emitted before the snapshot).
        <B::A as Vendor>::commit_restore(self, prep);
        // A restored VM is runnable again from the snapshot point: clear the latched
        // terminal + cached vCPU so `step`/`run` resume and `state_blob` re-reads the
        // restored backend state.
        self.terminal = None;
        self.saved_state = None;
        self.rng_completion_staged = false;
        // The restored backend is fresh (the next run re-executes from the restored
        // RIP) — no completion is pending.
        self.completion_staged = false;
        // Treat the restored VM as a **fresh spawn** for the work counter: re-arm the
        // first-entry gate so the next `step` calls `WorkSource::start_run` right
        // before VM-entry. The box `perf_event` counter is shared across the vCPU
        // thread; restore reset it to 0, but if another (coexisting) VM runs before
        // this one is entered, that VM's branches would accumulate into the shared
        // counter and be miscounted into the restored V-time. `start_run` at entry
        // (which only fires while `!first_entry_done`) re-establishes the per-VM
        // baseline, excluding them. Without this re-arm a restored VM silently
        // inherits a coexisting VM's branches (a determinism bug on the explorer's
        // N-concurrent-VM path).
        self.first_entry_done = false;
        // A restore resets the timeline, so any host-fault arrival deadline armed
        // against the PRE-restore V-time is stale — clear it (mirror `clear_arrival`;
        // the #34/#55 stale-arm class, PR #51 round-3). `restore_snapshot` and every
        // in-place restore path funnel through here.
        self.arrival_deadline = None;
        // Task 110: a pvclock registration is run-control state of the OLD
        // timeline (the same stale-arm class as the arrival deadline) — a
        // pre-registration snapshot restored into a post-registration VM must
        // not keep stamping into a page the restored guest never published.
        // Clear it (the offer + Δ are composition and stay); the snapshot's
        // own channel state is re-established and cross-validated by the
        // caller via `pvclock_restore` (the control server carries it
        // alongside the SDK channel snapshot).
        self.pvclock_clear_registration();
        Ok(())
    }

    /// Restore a full snapshot — guest memory **and** the non-memory `vm_state` — in
    /// one call. The materialized image (from [`crate::snapshot::SnapshotEngine::materialize`])
    /// goes into guest RAM, then [`Vmm::restore_vm_state`] resumes the vCPU/V-time/
    /// devices. *Same state ⇒ same future* (gate 1).
    pub fn restore_snapshot(
        &mut self,
        memory: &[u8],
        vm_state: &vm_state::VmState,
    ) -> Result<(), VmmError> {
        // All-or-nothing: pre-check the image length so a wrong-sized image is
        // rejected before either half mutates. Then restore the vm_state (itself
        // atomic on a malformed blob — see [`Vmm::restore_vm_state`]) *before* the
        // memory, so a bad blob never leaves a half-overwritten guest.
        if memory.len() != self.ram.len() {
            return Err(VmmError::ContractViolation(format!(
                "restore_snapshot: image is {} bytes, guest RAM is {} bytes",
                memory.len(),
                self.ram.len()
            )));
        }
        self.restore_vm_state(vm_state)?;
        self.restore_guest_memory(memory)?; // length pre-checked above
        Ok(())
    }

    /// **Branch**: reseed the entropy stream after a restore so the continuation
    /// draws a *divergent* RDRAND/RDSEED sequence from its parent (INTEGRATION.md §4:
    /// "after a restore intended to branch, vmm-core reseeds/perturbs the entropy
    /// service explicitly"). `branch(snap, seed') = restore(snap) + reseed(seed')`;
    /// the V-time clock and memory continue from the snapshot, only the entropy
    /// forks. The seed choice is the explorer's, so it is explicit, not ambient.
    ///
    /// # Errors
    /// [`VmmError::ContractViolation`] if the seeded-entropy path is not wired (a
    /// branch only diverges where there is a seeded stream to perturb).
    pub fn reseed_entropy(&mut self, seed: u64) -> Result<(), VmmError> {
        match self.vtime.as_mut() {
            Some(vt) => {
                vt.entropy = SeededEntropy::new(seed);
                Ok(())
            }
            None => Err(VmmError::ContractViolation(
                "reseed_entropy: the seeded-entropy path is not wired, so there is no stream to fork \
                 for a branch."
                    .to_string(),
            )),
        }
    }

    /// Drive the vCPU for exactly one exit and dispatch it. Data-returning exits
    /// (port read, `Rdmsr`, `Cpuid`) are resolved back to the backend; any
    /// unmodeled exit is a loud [`VmmError::ContractViolation`].
    pub fn step(&mut self) -> Result<Step, VmmError> {
        if let Some(reason) = self.terminal {
            return Ok(Step::Terminal(reason));
        }
        // On the FIRST guest entry of this VM's life (whether reached via `step` or
        // `run`), prepare the work counter so V-time work measures only this VM's
        // guest execution. The box `perf_event` counter is enabled at open and counts
        // guest branches on the *shared* vCPU thread, so a VM spawned before a
        // coexisting VM runs would otherwise inherit that VM's branches — and two
        // same-seed runs that differ only in spawn/run ordering (exactly what
        // `unison::compare_runs` does: spawn both, then run both) would diverge in the
        // work-derived V-time. Gating on the real first entry (not the top of `run`)
        // keeps a `step()`-then-`run()` consumer correct. No-op for the portable
        // `ScriptedWork` and for a single VM (`exclude_host` ⇒ the counter is ~0).
        // First-entry baseline: PREPARE the work counter before the entry (so it measures
        // only this VM's execution), but CONSUME the gate (`first_entry_done = true`) only
        // if the guest ACTUALLY ENTERS this call — set below, gated on `entered`. This is
        // the vmm-core half of the round-13 zero-step invariant (mirroring the backend
        // `reset_arm`, round-11): a no-entry zero-step `run_until` leaves the gate ARMED so
        // the next REAL entry re-baselines, and a coexisting VM in between cannot
        // contaminate this VM's counter. `start_run` itself is idempotent (re-zeroing a
        // counter no guest advanced), so preparing it on a no-entry step is harmless.
        let is_first_entry = !self.first_entry_done;
        if is_first_entry && let Some(vt) = self.vtime.as_mut() {
            vt.work.start_run()?;
        }
        // Desync the V-time exactness flag BEFORE entering the guest: a new exit may
        // retire branches since the last V-time intercept, and — crucially — if
        // `run()` itself errors we must not stay marked synchronized (a later
        // `save_vtime` would then emit a stale anchor instead of failing closed). A
        // V-time-intercept completion below re-sets it to `true` on success.
        self.vtime_synchronized = false;
        // Advance the V-time LAPIC timer + the serial COM1 line and hand any
        // now-deliverable vector to the backend for injection at the next safe
        // VM-entry (Linux path only; a no-op when the xAPIC is unwired, so
        // M1/M2/corpus state + hash are untouched). Done **before** the entry so the
        // queued IRQ rides the upcoming `KVM_RUN`.
        <B::A as Vendor>::service_pending_irqs(self)?;
        // This `run` re-enters the guest, which COMMITS any completion the prior step
        // staged (incl. an RNG reg-write/RIP-advance) — so once it SUCCEEDS that
        // boundary is clean again. `complete_rng` re-sets the flag if the exit we
        // service below is itself an RNG draw. (Cleared after `run()`, since a failed
        // re-entry did not commit the staged completion.)
        //
        // **Preemption (task 47).** When the determinism-complete path is wired and a
        // LAPIC timer is armed, run to the timer's V-time deadline (`run_until`)
        // instead of an open-ended `run()`, so a guest that takes no natural VM-exit
        // (a busy-spin) is still preempted at exactly the seed-deterministic
        // retired-branch count and the timer can fire. A natural guest exit before
        // the deadline returns from `run_until` identically to `run()`; only a guest
        // that would otherwise spin forever is forced out (additive — see
        // `preemption_deadline`). Unwired paths (M1/M2/corpus: no LAPIC; stock KVM:
        // no deterministic counter) keep plain `run()`, byte-for-byte unchanged.
        // On the preemption path, capture the pre-call work so a `Deadline` can be told
        // apart: the `Drive` path single-steps work strictly FORWARD (reached > before),
        // while the no-entry overdue/at-deadline zero-step returns `reached == before` with
        // NO `KVM_RUN` (round-12). `run_until_deadline()` is `Some` ⇒ V-time is wired;
        // it folds the task-47 LAPIC-timer preemption deadline together with the
        // task-59 host-fault arrival deadline, taking whichever work count is nearer.
        let deadline = self.run_until_deadline();
        let work_before = match deadline {
            Some(_) => Some(
                self.vtime
                    .as_ref()
                    .expect("preemption path implies V-time wired")
                    .work
                    .work()?,
            ),
            None => None,
        };
        let exit = match deadline {
            Some(d) => self.backend.run_until(d)?,
            None => self.backend.run()?,
        };
        // Did the guest ACTUALLY enter this call? (Round-13 zero-step invariant.) `run()`
        // always enters; a `run_until` GUEST EXIT means the guest ran; a `run_until`
        // `Deadline` entered iff work advanced past `work_before` — the no-entry zero-step
        // returns `reached == work_before` with no `KVM_RUN`, while `Drive` lands strictly
        // beyond it. (`B≡A`, so the backend's `reached` and vmm-core's `work_before` share
        // an axis.)
        let entered = match &exit {
            Exit::Common(CommonExit::Deadline { reached }) => {
                work_before.is_some_and(|wb| reached.0 > wb)
            }
            _ => true,
        };
        // INVARIANT (round-13): if NO guest entry occurred this call, do NOT touch ANY
        // entry-side state — a real `KVM_RUN` is what commits a staged completion and
        // consumes the first-entry baseline. So gate EVERY entry-side mutation on `entered`:
        if entered {
            // The entry consumed the first-entry baseline (counter prepared above).
            self.first_entry_done = true;
            // This `run` committed any completion staged by the prior step; the exit we are
            // about to service stages a new one iff it is a read-style / MSR / CPUID /
            // determinism exit (its `complete_*` below). Recorded so `restore_vm_state` can
            // refuse to restore into a backend with a pending completion (which would commit
            // the old exit's reg-write/RIP-advance on the next run).
            self.rng_completion_staged = false;
            self.completion_staged = exit.stages_completion();
        }
        // else: a no-entry zero-step `Deadline`. The prior step's staged completion is
        // STILL pending (no `KVM_RUN` committed it) and commits on the next REAL entry, so
        // `completion_staged` / `rng_completion_staged` must NOT drop (a snapshot here would
        // otherwise be saved/restored across a live pending completion → corruption); and
        // `first_entry_done` stays armed. Nothing entry-side changes.
        // Complete delivery of any vector the backend just **accepted** (issued
        // KVM_INTERRUPT for) — *after* the entry, *before* dispatching the exit, so a
        // guest APIC read / EOI in this exit (and any snapshot) sees a LAPIC vector
        // in-service exactly once KVM accepted it. (The legacy serial vector takes no
        // LAPIC transition — it is EOI'd at the 8259.)
        <B::A as Vendor>::complete_irq_delivery(self);
        // The two-level dispatch (`docs/ARCH-BOUNDARY.md` §A). The engine matches
        // the **common** exits exhaustively and hands every **arch** exit to that
        // vendor's own dispatch, which matches its enum exhaustively — so an
        // unhandled arch exit can never fall through an engine-written wildcard
        // arm (default-deny stays structural).
        let step = match exit {
            Exit::Common(CommonExit::Idle) => self.on_idle(),
            Exit::Common(CommonExit::Shutdown) => Ok(self.terminate(TerminalReason::Shutdown)),
            Exit::Common(CommonExit::Mmio { gpa, size, write }) => {
                <B::A as Vendor>::dispatch_mmio(self, gpa, size, write)
            }
            Exit::Common(CommonExit::Hypercall(_)) => Err(VmmError::ContractViolation(
                "unmodeled hypercall-instruction exit (host handler is a later phase; the \
                 cooperating-guest channel rides the doorbell)"
                    .to_string(),
            )),
            Exit::Common(CommonExit::Deadline { reached }) => self.on_deadline(reached),
            Exit::Arch(e) => <B::A as Vendor>::dispatch_arch(self, e),
        }?;
        // Task 110: refresh the pvclock page at the tail of EVERY serviced
        // exit (the §2 point-1 natural-exit refresh) with the skid-free
        // anchor's clock — value-keyed, so only the deterministic
        // clock-advance boundaries (V-time intercepts, `Deadline` landings,
        // idle warps) actually move the page bytes; see `pvclock_refresh`.
        // Stamping BEFORE the next entry is what closes the §7
        // kill-condition-1 ordering: a timer whose landing advanced the
        // anchor is injected at the NEXT entry, so the ISR reads a page
        // already stamped at (or beyond) the interrupt's own V-time. A no-op
        // unless a page is registered.
        self.pvclock_refresh()?;
        Ok(step)
    }

    /// `step()` to a `Terminal`. Returns the serial capture, terminal reason, and
    /// exit counts.
    pub fn run(&mut self) -> Result<RunResult, VmmError> {
        // The work counter is prepared at the first guest entry inside `step`
        // (`first_entry_done`), so a `step()`-then-`run()` consumer is handled
        // correctly — `run` itself does not touch it.
        // Stop at a substrate terminal OR a cooperating-SDK stop (round-6): an
        // assertion must NOT be swallowed by looping on to a later terminal.
        let reason = loop {
            match self.step()? {
                Step::Terminal(r) => break r,
                Step::SdkStop => break TerminalReason::SdkStop,
                Step::Continued => {}
            }
        };
        let sdk_stop = if reason == TerminalReason::SdkStop {
            self.take_sdk_stop()
        } else {
            None
        };
        // Cache the final vCPU **only for a genuine terminal** (propagating any
        // save error here, so the infallible `state_blob` reads a consistent
        // snapshot post-terminal, where the backend may not be re-savable). A
        // `Step::SdkStop` (an assertion) is **resumable**, not terminal: caching
        // here would make `state_blob`/`save_vm_state` read a STALE vCPU after the
        // caller resumes past the stop. So invalidate the cache on an SDK stop and
        // let `current_vcpu` do a fresh live save reflecting the resumed state.
        if reason == TerminalReason::SdkStop {
            self.saved_state = None;
        } else {
            self.saved_state = Some(self.backend.save()?);
        }
        Ok(RunResult {
            reason,
            sdk_stop,
            serial: <B::A as Vendor>::serial_capture(&self.devices).to_vec(),
            exit_counts: self.backend.exit_counts(),
        })
    }

    /// Canonical, length-prefixed, domain-tagged serialization of **all observable
    /// state**: materialized guest memory ‖ `Backend::save()` ‖ serial capture ‖
    /// device + terminal state ‖ (when wired) V-time + seeded-RNG determinism
    /// state. Pure (no map iteration into bytes, no float, no wall-clock); calling
    /// it twice is identical.
    ///
    /// The `VTIM` chunk is present **only** when the determinism path is wired
    /// (`PatchedKvmBackend`). It captures the state that governs future RDTSC/RNG
    /// output — the V-time clock rate (`ratio`/`guest_hz`/`guest_base`/`tsc_adjust`),
    /// the effective V-time (`vns_base` + work folded into one canonical field), and the entropy
    /// stream position (seed + draws so far) — so two states with identical RAM/regs
    /// but different V-time/seed hash **differently** (the replay-equivalence
    /// `unison::compare_runs` relies on), while a restored VM and a fresh VM at the
    /// same effective V-time hash **identically** (see [`encode_vtime`]). Stock KVM /
    /// M1/M2 (`vtime: None`) emit **no** chunk, so their `state_hash` is byte-for-
    /// byte unchanged from before this was added.
    pub fn state_blob(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_chunk(&mut out, b"MEM\0", self.ram.as_bytes());
        put_chunk(
            &mut out,
            b"VCPU",
            &<B::A as Vendor>::encode_vcpu_chunk(&self.current_vcpu()),
        );
        put_chunk(
            &mut out,
            b"SERL",
            <B::A as Vendor>::serial_capture(&self.devices),
        );
        put_chunk(&mut out, b"DEV\0", &self.encode_device_terminal());
        if let Some(vt) = &self.vtime {
            put_chunk(&mut out, b"VTIM", &encode_vtime(vt));
        }
        // The vendor's own device chunks, at this fixed position in the blob (x86:
        // `LAPC` then `LEGY`). A vendor emits none for a device it has not wired
        // (M1/M2/corpus never wire the xAPIC or the legacy platform), so their hash
        // is byte-for-byte unchanged.
        <B::A as Vendor>::hash_device_chunks(&self.devices, &mut out);
        // The task-73 SDK channel's **replay-relevant** state — present **only**
        // when a channel is wired (`enable_sdk`), so an SDK-less run's blob
        // (M1/M2/corpus/Linux-boot) is byte-for-byte unchanged (round-7). It folds
        // the seeded stream positions (buggify + inert supply) and the pending stop
        // into the hash, so two same-seed forks that diverge in their SDK stream (a
        // different buggify draw sequence) hash differently — the SDK divergence is
        // now IN the determinism hash, not silently outside it. The event log stays
        // out (host-side observation, like the report stream).
        if let Some(sdk) = &self.sdk {
            put_chunk(&mut out, b"SDK\0", &encode_sdk_channel(sdk));
        }
        // The task-110 pvclock channel configuration — present **only** when
        // the page is offered (`enable_pvclock`), so every existing
        // composition's blob is byte-for-byte unchanged. Δ and the
        // registration govern future guest-visible time (the forced-refresh
        // schedule and where stamps land), so — like the SDK channel's
        // fault-policy fold — two states identical in RAM but differing here
        // have different futures and must hash differently. The refresh log
        // stays out (diagnostic, like the landing traces).
        if let Some(pv) = &self.pvclock {
            let mut bytes = pv.delta_work.to_le_bytes().to_vec();
            match pv.gpa {
                Some(gpa) => {
                    bytes.push(1);
                    bytes.extend_from_slice(&gpa.to_le_bytes());
                }
                None => bytes.push(0),
            }
            put_chunk(&mut out, b"PVCK", &bytes);
        }
        // The canonical `vm_state` blob, folded into the hash **only** when the
        // snapshot/branch path opts in (`wire_snapshot_hashing`). Default-off keeps
        // M1/M2/corpus/Linux-boot blobs byte-for-byte unchanged (their goldens do
        // not move — task 39 "gate the swap"); when on, two states whose canonical
        // blob differs hash differently, so a snapshot's integrity is in the hash.
        // The only `encode` failure is `FractionalRatio`, which `build_vm_state`
        // can never produce (`ratio_den` is the invariant `1`), so the fallback is
        // unreachable; it is deterministic regardless.
        if self.snapshot_hashing {
            // Best-effort like the other hash chunks: `current_vcpu` uses the
            // terminal-captured state or a swallowing live `save` (the snapshot path,
            // `save_vm_state`, reads the vCPU fallibly instead).
            let bytes = <B::A as Vendor>::build_vm_state(self, &self.current_vcpu())
                .encode()
                .unwrap_or_default();
            put_chunk(&mut out, b"VMST", &bytes);
        }
        out
    }

    /// Device + terminal state for the `DEV\0` hash chunk: the vendor's device
    /// residual registers followed by the engine's latched terminal reason /
    /// debug-exit code. Two runs that drive the devices into a different residual
    /// configuration — even with byte-identical serial output — hash differently
    /// (their future I/O behavior differs).
    fn encode_device_terminal(&self) -> Vec<u8> {
        let mut v = <B::A as Vendor>::encode_device_state(&self.devices);
        match self.terminal {
            None => v.push(0),
            Some(TerminalReason::DebugExit { code }) => {
                v.push(1);
                v.push(code);
            }
            Some(TerminalReason::Idle) => v.push(2),
            Some(TerminalReason::Shutdown) => v.push(3),
            // `SdkStop` is a `run` stop reason, never latched as the VM's terminal
            // (only substrate terminals latch via `terminate`), so it is never
            // serialized here.
            Some(TerminalReason::SdkStop) => {
                unreachable!("SdkStop never latches as the VM terminal")
            }
        }
        v
    }

    /// `sha256(state_blob())` — the M2 determinism hash and the unison
    /// `state_hash`.
    pub fn state_hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(self.state_blob());
        hasher.finalize().into()
    }

    /// **Diagnostic only** (not part of [`Vmm::state_hash`]): a labeled per-component
    /// digest breakdown of all observable state, so a determinism bisector can pin
    /// **which** component diverges between two same-seed runs — named RAM regions,
    /// GPRs, segments, descriptor tables, control regs, PDPTRs, XCR0, debug regs,
    /// pending events, MP state, MSRs, the three XSAVE sub-areas, serial, device,
    /// and V-time. Pure; labels are stable and in a fixed order. Used by the box
    /// `c1_corpus_o1_diagnostic` to localize the architectural divergence the corpus
    /// caught (PR #51); not folded into any oracle hash.
    pub fn state_components(&self) -> Vec<(&'static str, [u8; 32])> {
        fn dig(bytes: &[u8]) -> [u8; 32] {
            let mut h = Sha256::new();
            h.update(bytes);
            h.finalize().into()
        }
        let vcpu = self.current_vcpu();
        let mut out: Vec<(&'static str, [u8; 32])> = Vec::new();

        // RAM in named regions — localize non-zeroed / host-dependent scratch. The
        // C1 payloads keep boot-info + page tables + stack in low RAM and load at
        // 1 MiB; everything from 2 MiB up should stay zeroed.
        let ram = self.ram.as_bytes();
        let region = |lo: usize, hi: usize| {
            let (lo, hi) = (lo.min(ram.len()), hi.min(ram.len()));
            dig(&ram[lo..hi])
        };
        out.push(("RAM:0..64K", region(0, 0x1_0000)));
        out.push(("RAM:64K..1M", region(0x1_0000, 0x10_0000)));
        out.push(("RAM:1M..2M", region(0x10_0000, 0x20_0000)));
        out.push(("RAM:2M..16M", region(0x20_0000, 0x100_0000)));
        out.push(("RAM:16M..", region(0x100_0000, ram.len())));

        // The vendor's register-file breakdown (GPRs, segments, descriptor tables,
        // control regs, pending events, the FPU/extended-state image, …) — which
        // records exist is per-arch, so the vendor names them.
        <B::A as Vendor>::vcpu_components(&vcpu, &mut out);

        // Serial + device + V-time.
        out.push((
            "serial",
            dig(<B::A as Vendor>::serial_capture(&self.devices)),
        ));
        out.push(("dev", dig(&self.encode_device_terminal())));
        if let Some(vt) = &self.vtime {
            // V-time chunk broken out for the O1 localizer (PR #51 box-review). The
            // first three components are a **faithful cover** of the bytes
            // `encode_vtime` actually hashes — `vtim:cfg` ‖ `vtim:eff-vns` ‖
            // `vtim:entropy` is exactly its preimage — so a `VTIM` `state_hash`
            // divergence shows up as one of them and never as a "diverged but every
            // component matched" mystery. The last two are **diagnostic-only (NOT in
            // the hash)**: they explain *why* the effective V-time might move.
            //
            // (The earlier breakdown predated #53: it hashed `vns_base` + the live
            // `work()` read, but #53's `encode_vtime` folds them into the single
            // skid-free effective field `snapshot_vns(last_intercept_work)`. Mirroring
            // the live read as a hashed component would falsely indict the
            // post-intercept skid the hash deliberately excludes.)
            let mut cfg = Vec::new();
            for x in [
                vt.cfg.ratio_num,
                vt.cfg.guest_hz,
                vt.cfg.guest_base,
                vt.guest_clock_offset,
            ] {
                cfg.extend_from_slice(&x.to_le_bytes());
            }
            out.push(("vtim:cfg", dig(&cfg)));
            // The effective V-time `encode_vtime` hashes: `snapshot_vns` of the
            // **deterministic** `last_intercept_work` (NOT a live counter read).
            out.push((
                "vtim:eff-vns",
                dig(&vt.clock.snapshot_vns(vt.last_intercept_work).to_le_bytes()),
            ));
            out.push(("vtim:entropy", dig(&vt.entropy.save_state())));
            // Diagnostic-only (NOT hashed). `vtim:last-intercept` is the
            // deterministic anchor `vtim:eff-vns` is derived from (if `eff-vns`
            // diverges, this localizes it to the anchor); `vtim:work-raw` is the
            // skid-prone **live** counter read — if it diverges while the hashed
            // components match, that is the post-intercept skid #53 correctly
            // excludes, NOT an O1 failure.
            out.push((
                "vtim:last-intercept",
                dig(&vt.last_intercept_work.to_le_bytes()),
            ));
            out.push((
                "vtim:work-raw",
                dig(&vt.work.work().unwrap_or(u64::MAX).to_le_bytes()),
            ));
        }
        out
    }

    /// The ordered conformance **report stream**: every value the guest wrote to
    /// [`REPORT_PORT`] (`OUT`), in execution order. Empty for stock / M1/M2 runs
    /// that never touch the port. Feeds [`Vmm::observable_digest`].
    pub fn report_stream(&self) -> &[u32] {
        &self.report_stream
    }

    /// The MEASURED preemption landings: the retired-branch work at which `run_until`
    /// delivered each LAPIC timer (`CommonExit::Deadline { reached }`), in order. This is the
    /// VMM/backend's measurement — distinct from the ICR the guest programmed — and is
    /// what proves seed-DEPENDENT preemption (the landing work differs across seeds for a
    /// seed-consuming guest, but is identical for a pure one). Empty when no preemption
    /// occurred; capped at [`PREEMPTION_TRACE_CAP`]. Not hashed (observability only).
    pub fn preemption_landings(&self) -> &[u64] {
        &self.preemption_landings
    }

    /// The idle-resume landings (task 52): the **V-time** (ns) the clock was warped to
    /// each time the guest went idle (`HLT` with `RFLAGS.IF == 1` and an armed timer) and
    /// [`Self::resume_idle`] jumped to the timer deadline. The dual of
    /// [`Self::preemption_landings`] (jumped-to vs executed-to the next event); empty when
    /// the run never idled. Skid-free (derived from the last-intercept anchor + the timer
    /// deadline, never a live `HLT` work read). A box gate reads it to confirm the idle
    /// path engaged (e.g. real `runc` genuinely idles mid-handshake) and that the landings
    /// are seed-deterministic. Not hashed (observability only); capped at
    /// [`PREEMPTION_TRACE_CAP`].
    pub fn idle_landings(&self) -> &[u64] {
        &self.idle_landings
    }

    /// The serial (8250 THR) capture buffer so far, in order — the live console
    /// output. [`Vmm::run`] also returns it in [`RunResult::serial`] at terminal,
    /// but this lets a bounded step loop (e.g. the box Linux-boot gate) watch the
    /// console **mid-run** to detect `GUEST_READY` before the guest powers off.
    pub fn serial(&self) -> &[u8] {
        <B::A as Vendor>::serial_capture(&self.devices)
    }

    /// The backend's per-exit-reason trap counts so far (R-Backend observability).
    /// A live read for the box Linux-boot diagnostic: how many IO/MMIO/MSR/CPUID
    /// exits the boot took says where it got to. [`RunResult::exit_counts`] carries
    /// the same at terminal.
    pub fn exit_counts(&self) -> vmm_backend::ExitCounts {
        self.backend.exit_counts()
    }

    /// The latched terminal reason, or `None` if the run has not reached a
    /// terminal state yet. Lets a caller that drove the loop via [`Vmm::run`] (and
    /// discarded its [`RunResult`]) still confirm the payload ended on a clean
    /// `DebugExit { code: 0 }` — the corpus bridge uses it as the box-run gate.
    pub fn terminal_reason(&self) -> Option<TerminalReason> {
        self.terminal
    }

    /// `sha256` of the **guest-observable conformance output** — the ordered
    /// report stream ‖ the serial banner — the O2/O3 digest the corpus pins to a
    /// golden. Deliberately **distinct** from [`Vmm::state_hash`] (the unison
    /// `Subject::state_hash`, which folds in latent RAM / V-time / seeded-entropy
    /// state): the report stream is what the guest *deliberately emits*, so it is
    /// the right conformance signal — a constant payload that happens to be
    /// perfectly deterministic still produces a meaningful (and seed-sensitive,
    /// for an RNG payload) digest here. Pure, length-prefixed, domain-tagged
    /// (`OBSV`); each report dword is hashed little-endian in execution order, so
    /// two runs that emit different reported values digest differently even with
    /// byte-identical serial output.
    pub fn observable_digest(&self) -> [u8; 32] {
        crate::corpus::observable_digest_of(
            &self.report_stream,
            <B::A as Vendor>::serial_capture(&self.devices),
        )
    }

    // --- dispatch helpers --------------------------------------------------

    pub(crate) fn terminate(&mut self, reason: TerminalReason) -> Step {
        self.terminal = Some(reason);
        Step::Terminal(reason)
    }

    /// Wire the task-73 SDK channel for the upcoming run: `env` answers buggify
    /// decisions, and the hypercall doorbell is serviced. Resets the event /
    /// decision capture. A guest that never rings the doorbell is unaffected (the
    /// channel is inert and never hashed), so non-SDK runs are byte-for-byte
    /// unchanged.
    pub fn enable_sdk(
        &mut self,
        env: environment::RecordedEnv,
        policy: &environment::FaultPolicy,
    ) -> &mut Self {
        self.sdk = Some(SdkChannel {
            env,
            events: Vec::new(),
            buggify: Vec::new(),
            pending_stop: None,
            pending_snapshot: false,
            policy: policy.to_bytes(),
        });
        self
    }

    /// Whether an SDK channel is wired (a doorbell will be serviced, not a
    /// contract violation). Test-only observation — the control server asserts a
    /// kept fresh VM stays SDK-capable after a recoverable `RestoreFailed`.
    #[cfg(test)]
    pub(crate) fn sdk_is_enabled(&self) -> bool {
        self.sdk.is_some()
    }

    /// Wire the task-61 `Net` channel for the upcoming run: the hypercall doorbell
    /// is serviced and `net_decide` decisions are captured. Takes **no env** — a
    /// net decision draws from the one shared fault stream the SDK channel owns
    /// (the single-stream ruling), so [`enable_sdk`] must also be wired for a net
    /// decision to resolve a non-nominal policy (the control server always wires
    /// both). Resets the decision capture. A guest that never asks about a flow is
    /// unaffected — the channel is inert, and since a net decision only advances
    /// the shared SDK stream, a run without net decisions is byte-for-byte
    /// unchanged (there is no `NET` hash chunk).
    pub fn enable_net(&mut self) -> &mut Self {
        self.net = Some(NetChannel {
            decisions: Vec::new(),
        });
        self
    }

    /// The per-flow decisions this run resolved, `(moment, conn, answer)`, in
    /// order. Evidence that a run exercised the net vertical (the box gate reads
    /// it): every flow decision appears at a stable `Moment` across two same-seed
    /// runs. The decision log itself is host-side capture; the *stream advance*
    /// each decision caused is folded into `state_hash` via the shared SDK stream.
    pub fn net_decisions(&self) -> &[(u64, u64, environment::Answer)] {
        self.net
            .as_ref()
            .map(|n| n.decisions.as_slice())
            .unwrap_or(&[])
    }

    /// Capture the `Net` channel's **replay-relevant** state for a snapshot: the
    /// decision log only. The flow-policy stream position rides the shared SDK
    /// stream ([`sdk_snapshot`](Self::sdk_snapshot)), so it is not captured here.
    /// `None` when no Net channel is wired.
    pub fn net_snapshot(&self) -> Option<NetSnapshot> {
        self.net.as_ref().map(|n| NetSnapshot {
            decisions: n.decisions.clone(),
        })
    }

    /// Restore a captured [`NetSnapshot`]'s decision prefix. The flow-policy stream
    /// position is restored by [`sdk_restore`](Self::sdk_restore) /
    /// [`sdk_restore_events`](Self::sdk_restore_events) (the shared stream), so both
    /// the verbatim-replay and the branch paths restore the same thing here — just
    /// the decision log carried forward. A no-op when no Net channel is wired.
    pub fn net_restore(&mut self, snap: &NetSnapshot) {
        if let Some(n) = self.net.as_mut() {
            n.decisions = snap.decisions.clone();
        }
    }

    /// Offer the task-110 paravirt work-derived clock page to the guest
    /// (`docs/PARAVIRT-CLOCK.md`), with staleness bound `delta_work` (**Δ**, in
    /// counted work units — [`PVCLOCK_DEFAULT_DELTA_WORK`] unless the harness
    /// is measuring the Δ trade-off). Offering alone changes nothing: the page
    /// engages only when the guest publishes a page GPA over the doorbell
    /// ([`hypercall_proto::ServiceId::Pvclock`], op 1), and registration is
    /// accepted only on the determinism-complete path (V-time wired **and** a
    /// deterministic work counter — the stamps must derive from the skid-free
    /// anchor, and the Δ refresh needs the exact-count `run_until` seam).
    ///
    /// A zero `delta_work` is clamped to 1 (a zero Δ would arm the forced
    /// refresh *at* the anchor, an always-overdue livelock) — documented rather
    /// than fallible, matching the composition-root builder style.
    pub fn enable_pvclock(&mut self, delta_work: u64) -> &mut Self {
        self.pvclock = Some(PvclockChannel {
            delta_work: delta_work.max(1),
            gpa: None,
            refreshes: Vec::new(),
        });
        self
    }

    /// `true` once [`enable_pvclock`](Self::enable_pvclock) offered the clock
    /// page (regardless of whether the guest has registered one).
    pub fn pvclock_offered(&self) -> bool {
        self.pvclock.is_some()
    }

    /// The registered pvclock page GPA, or `None` when the guest has not
    /// published one (or the page is not offered).
    pub fn pvclock_registration(&self) -> Option<u64> {
        self.pvclock.as_ref().and_then(|pv| pv.gpa)
    }

    /// Capture the pvclock channel's **complete replay-relevant configuration**
    /// for a snapshot: `Some` iff the page is offered, carrying the staleness
    /// bound Δ and the registration (if any). The page *bytes* ride the RAM
    /// image; this is everything else that governs future execution — Δ shapes
    /// the forced-refresh schedule and the offer shapes a future registration —
    /// so the control server carries it across snapshot/branch like the SDK
    /// channel's, restoring (and cross-validating) via
    /// [`pvclock_restore`](Self::pvclock_restore).
    pub fn pvclock_snapshot(&self) -> Option<PvclockSnapshot> {
        self.pvclock.as_ref().map(|pv| PvclockSnapshot {
            delta_work: pv.delta_work,
            gpa: pv.gpa,
        })
    }

    /// Re-establish a snapshot's pvclock channel state on this (restored) VM,
    /// **validating the composition symmetrically** first: the snapshot and
    /// this VM must agree on whether the page is offered AND on Δ — either
    /// mismatch means the restored timeline's future refresh schedule (or a
    /// future registration) would diverge from the source's, so it fails loud
    /// ([`VmmError::ContractViolation`], the LAPIC wiring-mismatch posture),
    /// never a silently different clock. A carried registration additionally
    /// re-validates the GPA against **this** VM's RAM and requires the
    /// deterministic-clock backend the original registration required. The
    /// diagnostic refresh log resets (fresh evidence window, like the landing
    /// traces).
    ///
    /// # Errors
    /// [`VmmError::ContractViolation`] on any offer/Δ mismatch, a GPA that no
    /// longer validates, or a registration restored onto a backend with no
    /// deterministic work counter.
    pub fn pvclock_restore(&mut self, snap: Option<&PvclockSnapshot>) -> Result<(), VmmError> {
        match (snap, self.pvclock.as_ref()) {
            (None, None) => Ok(()),
            (None, Some(_)) => Err(VmmError::ContractViolation(
                "pvclock_restore: this VM offers the clock page but the snapshot's VM did not — \
                 a guest registering here would fork the timeline off the sealed one; restore \
                 into a VM composed like the snapshot source."
                    .to_string(),
            )),
            (Some(s), None) => Err(VmmError::ContractViolation(format!(
                "pvclock_restore: snapshot carries a pvclock channel (Δ = {}, registration \
                 {:#x?}) but this VM was composed without enable_pvclock — restore into a VM \
                 composed like the snapshot source.",
                s.delta_work, s.gpa
            ))),
            (Some(s), Some(pv)) => {
                if s.delta_work != pv.delta_work {
                    return Err(VmmError::ContractViolation(format!(
                        "pvclock_restore: staleness bound mismatch (snapshot Δ = {}, this VM's \
                         Δ = {}) — the forced-refresh schedule would diverge from the sealed \
                         timeline; restore into a VM composed like the snapshot source.",
                        s.delta_work, pv.delta_work
                    )));
                }
                if let Some(gpa) = s.gpa {
                    if !self.backend.capabilities().arch.deterministic_clock() {
                        return Err(VmmError::ContractViolation(format!(
                            "pvclock_restore: snapshot carries a registered clock page \
                             ({gpa:#x}) but this backend has no deterministic work counter to \
                             stamp from — the original registration required one."
                        )));
                    }
                    self.pvclock_validate_gpa(gpa).map_err(|reason| {
                        VmmError::ContractViolation(format!(
                            "pvclock_restore: snapshot page GPA {gpa:#x} does not validate on \
                             this VM ({reason}) — restore into a VM composed like the snapshot \
                             source."
                        ))
                    })?;
                }
                let pv = self.pvclock.as_mut().expect("matched Some above");
                pv.gpa = s.gpa;
                pv.refreshes.clear();
                Ok(())
            }
        }
    }

    /// Drop any pvclock registration and its refresh log (the run-control
    /// stale-arm reset [`restore_vm_state`](Self::restore_vm_state) performs;
    /// the offer and Δ are composition, untouched). The caller re-establishes
    /// the snapshot's own channel state via
    /// [`pvclock_restore`](Self::pvclock_restore).
    fn pvclock_clear_registration(&mut self) {
        if let Some(pv) = self.pvclock.as_mut() {
            pv.gpa = None;
            pv.refreshes.clear();
        }
    }

    /// Reset the diagnostic refresh log — re-arm the G2/G3 evidence window at
    /// a measurement boundary, so a bounded assertion (e.g. G3's ≤Δ gap check)
    /// measures its own window instead of a boot-saturated trace. Never
    /// touches the page or the registration.
    pub fn pvclock_clear_refreshes(&mut self) {
        if let Some(pv) = self.pvclock.as_mut() {
            pv.refreshes.clear();
        }
    }

    /// The diagnostic pvclock refresh log: `(work anchor, vns, guest_clock)`
    /// per value-publishing stamp, **read back from the page bytes** (never
    /// the computed values — see [`PvclockChannel`]). Empty when nothing is
    /// registered. The G2/G3 gates' evidence; capped at
    /// [`PREEMPTION_TRACE_CAP`], not hashed.
    pub fn pvclock_refreshes(&self) -> &[(u64, u64, u64)] {
        self.pvclock
            .as_ref()
            .map(|pv| pv.refreshes.as_slice())
            .unwrap_or(&[])
    }

    /// The current pvclock page bytes (the registered 4 KiB window of guest
    /// RAM), or `None` when nothing is registered. For gates and tests — reads
    /// the live RAM, so it sees exactly what the guest would.
    pub fn pvclock_page(&self) -> Option<&[u8]> {
        let gpa = self.pvclock_registration()? as usize;
        self.ram
            .as_bytes()
            .get(gpa..gpa + vtime::pvclock::PVCLOCK_PAGE_LEN)
    }

    /// G2's function-equality check, callable at any point (the box gate calls
    /// it at chosen boundaries; the deliberate-fault test proves it can fail):
    /// the page's current stable frame must publish **exactly** the values the
    /// RDTSC-trap oracle would return at the current skid-free anchor —
    /// `vns == VClock::vns(anchor)`, `guest_clock == VtimeWiring::guest_clock(anchor)`
    /// (the same function `complete_tsc` completes with), `guest_clock_hz ==`
    /// the wired config's. A no-op `Ok` when nothing is registered.
    ///
    /// # Errors
    /// [`VmmError::ContractViolation`] naming the mismatching field — a page
    /// that diverges from the oracle is a stamping bug, never tolerated.
    pub fn pvclock_check_oracle(&self) -> Result<(), VmmError> {
        let Some(page) = self.pvclock_page() else {
            return Ok(());
        };
        let vt = self.vtime.as_ref().ok_or_else(|| {
            VmmError::ContractViolation(
                "pvclock page registered but V-time is not wired — registration is gated on the \
                 determinism path, so this is unreachable state"
                    .to_string(),
            )
        })?;
        let anchor = vt.last_intercept_work;
        let want_vns = vt.clock.snapshot_vns(anchor);
        let want_gc = vt.guest_clock(anchor);
        let want_hz = vt.cfg.guest_hz;
        let Some(f) = vtime::pvclock::read(page) else {
            return Err(VmmError::ContractViolation(
                "pvclock page is not a stable ABI-v1 frame (odd seq or foreign abi_version) at a \
                 host-quiescent read — the stamp protocol never leaves the page mid-update"
                    .to_string(),
            ));
        };
        if (f.vns, f.guest_clock, f.guest_clock_hz) != (want_vns, want_gc, want_hz) {
            return Err(VmmError::ContractViolation(format!(
                "pvclock page diverges from the RDTSC-trap oracle at anchor {anchor}: page \
                 (vns {}, guest_clock {}, hz {}) vs oracle (vns {want_vns}, guest_clock \
                 {want_gc}, hz {want_hz})",
                f.vns, f.guest_clock, f.guest_clock_hz
            )));
        }
        Ok(())
    }

    /// Validate a pvclock page GPA against this VM: page-aligned, wholly
    /// inside guest RAM, and clear of the doorbell frame pages (a stamp there
    /// would clobber an in-flight hypercall exchange). Returns the failing
    /// reason for the caller's status/error mapping.
    fn pvclock_validate_gpa(&self, gpa: u64) -> Result<(), &'static str> {
        let page_len = vtime::pvclock::PVCLOCK_PAGE_LEN as u64;
        if !gpa.is_multiple_of(page_len) {
            return Err("not page-aligned");
        }
        let end = gpa.checked_add(page_len).ok_or("address overflow")?;
        if end > self.ram.as_bytes().len() as u64 {
            return Err("past the end of guest RAM");
        }
        // The doorbell frame pages are contiguous ([REQ_GPA, RESP_GPA + page]);
        // a 4 KiB-aligned page overlaps them iff it IS one of them.
        if gpa == REQ_GPA as u64 || gpa == RESP_GPA as u64 {
            return Err("overlaps a doorbell frame page");
        }
        Ok(())
    }

    /// Register the guest-published pvclock page: validate the GPA, record it,
    /// and stamp the page to **canonical form** at the current skid-free
    /// anchor (a deterministic baseline regardless of what the guest left in
    /// those bytes). Returns the doorbell `Status` + the ABI version to
    /// answer. Gated on the determinism-complete path — a stock/M1/M2
    /// composition answers `UnknownService` ("not offered"), so a probing
    /// guest cleanly keeps its trap-backstopped time paths.
    ///
    /// Re-registration is accepted and moves the stamping target (the guest
    /// owns its page placement; guest-driven, so deterministic) — the old
    /// page's bytes are left exactly as last stamped.
    fn pvclock_register(&mut self, gpa: u64) -> (Status, Option<u32>) {
        if self.pvclock.is_none()
            || self.vtime.is_none()
            || !self.backend.capabilities().arch.deterministic_clock()
        {
            return (Status::UnknownService, None);
        }
        if self.pvclock_validate_gpa(gpa).is_err() {
            return (Status::OutOfRange, None);
        }
        self.pvclock.as_mut().expect("checked above").gpa = Some(gpa);
        // Canonical initial stamp (total function of the anchor values — never
        // of the guest's prior page content). The doorbell OUT is not a V-time
        // intercept, so the anchor may lag the live work here; the page then
        // publishes a (deterministic) lower bound until the next clock-advance
        // boundary re-stamps it — same staleness contract as any other window.
        match self.pvclock_stamp(StampKind::Canonical) {
            Ok(()) => (Status::Ok, Some(vtime::pvclock::PVCLOCK_ABI_VERSION)),
            // A stamp failure here is a validated-GPA slice failing to
            // materialize — substrate breakage; answer Internal, never a
            // silent success.
            Err(_) => (Status::Internal, None),
        }
    }

    /// Re-stamp the registered pvclock page from the current clock — the §2
    /// refresh. Called by [`step`](Self::step) at every deterministic
    /// clock-advance boundary (V-time intercepts, deadline landings, idle
    /// warps — wherever `vtime_synchronized` holds at the end of a step) and,
    /// in canonical form, by [`save_vm_state`](Self::save_vm_state) at every
    /// seal quiescent point. A no-op without a registration.
    ///
    /// The stamped values derive from the **skid-free anchor**
    /// (`last_intercept_work`), never a live counter read — the page is hashed
    /// guest RAM, so a skid-noisy stamp would be a determinism bug, and the
    /// anchor is exactly what the RDTSC-trap oracle returns (G2 holds by
    /// construction; the read-back check below makes it evidence). Stamps are
    /// value-keyed no-ops when the clock has not advanced, so the page bytes
    /// are a pure function of the distinct-value stream.
    ///
    /// # Errors
    /// [`VmmError::ContractViolation`] if the registered page cannot be
    /// sliced from RAM (unreachable past registration validation) or the
    /// read-back of a fresh stamp does not decode to the stamped values (a
    /// stamping bug — fails closed, never a silently-wrong guest clock).
    fn pvclock_stamp(&mut self, kind: StampKind) -> Result<(), VmmError> {
        let Some(pv) = self.pvclock.as_ref() else {
            return Ok(());
        };
        let Some(gpa) = pv.gpa else {
            return Ok(());
        };
        let Some(vt) = self.vtime.as_ref() else {
            // Registration is gated on V-time; reaching here without it is a
            // composition bug.
            return Err(VmmError::ContractViolation(
                "pvclock page registered but V-time is not wired".to_string(),
            ));
        };
        let anchor = vt.last_intercept_work;
        let vns = vt.clock.snapshot_vns(anchor);
        let gc = vt.guest_clock(anchor);
        let hz = vt.cfg.guest_hz;
        let start = gpa as usize;
        let ram = self.ram.as_mut_bytes();
        let Some(page) = ram.get_mut(start..start + vtime::pvclock::PVCLOCK_PAGE_LEN) else {
            return Err(VmmError::ContractViolation(format!(
                "pvclock page {gpa:#x} no longer inside guest RAM — registration validated it, so \
                 the RAM backing changed underneath the channel"
            )));
        };
        let changed = match kind {
            StampKind::Refresh => vtime::pvclock::stamp(page, vns, gc, hz),
            StampKind::Canonical => vtime::pvclock::stamp_canonical(page, vns, gc, hz),
        };
        if !changed {
            return Ok(());
        }
        // Read back what actually landed in RAM: the always-on half of G2's
        // evidence bar (a wrong-offset/wrong-endian stamp fails here, loudly,
        // on the very first refresh — never a plausible-but-wrong guest clock).
        let readback = vtime::pvclock::read(page);
        if readback.map(|f| (f.vns, f.guest_clock, f.guest_clock_hz)) != Some((vns, gc, hz)) {
            return Err(VmmError::ContractViolation(format!(
                "pvclock stamp read-back mismatch at anchor {anchor}: wrote (vns {vns}, \
                 guest_clock {gc}, hz {hz}) but the page decodes to {readback:?}"
            )));
        }
        // A host-side RAM write the backend's dirty log cannot see (task 95
        // M2.1 safety rule).
        self.mark_host_dirty(gpa, vtime::pvclock::PVCLOCK_PAGE_LEN as u64);
        // Log value publishes (not canonical seq-resets, which republish the
        // same values) — the G2/G3 gates' per-refresh evidence.
        if kind == StampKind::Refresh {
            let pv = self.pvclock.as_mut().expect("checked above");
            if pv.refreshes.len() < PREEMPTION_TRACE_CAP {
                pv.refreshes.push((anchor, vns, gc));
            }
        }
        Ok(())
    }

    /// The [`step`](Self::step)-tail refresh — the §2 point-1 "every natural
    /// exit" refresh: re-stamp the page at the tail of **every** serviced
    /// exit, publishing the clock at the **skid-free anchor**
    /// (`last_intercept_work`), the exit's deterministic work count. Between
    /// two clock advances the anchor (and the offset) cannot move, so the
    /// stamp at a non-intercept exit (PIO/MMIO/doorbell/serial) republishes
    /// identical values and the value-keyed [`pvclock_stamp`](Self::pvclock_stamp)
    /// leaves the page bytes untouched — the refresh *runs* at all four §2
    /// points, and the published value stream advances exactly at the
    /// deterministic clock-advance boundaries (intercepts, `Deadline`
    /// landings, idle warps). A fresh live counter read here would be the
    /// task-27 skid hazard: nondeterministic bytes straight into hashed guest
    /// RAM and guest-visible time. The one observable effect of the
    /// non-advance stamps is self-healing: a (deterministic) guest scribble
    /// on the page is repaired at the next exit, not merely the next
    /// intercept.
    fn pvclock_refresh(&mut self) -> Result<(), VmmError> {
        self.pvclock_stamp(StampKind::Refresh)
    }

    /// The staleness-bound forced-refresh deadline (§2 point 4): with a page
    /// registered on the determinism-complete path, the next `run_until` is
    /// bounded at `anchor + Δ` counted work units, so a compute-bound guest
    /// that takes no natural exit (a busy-wait on the page clock) is forced
    /// out — and the page re-stamped — within Δ. `None` without a
    /// registration, so every existing path arms exactly as before.
    pub(crate) fn pvclock_refresh_deadline(&self) -> Option<Moment> {
        let pv = self.pvclock.as_ref()?;
        pv.gpa?;
        if !self.backend.capabilities().arch.deterministic_clock() {
            return None;
        }
        let vt = self.vtime.as_ref()?;
        Some(Moment(vt.last_intercept_work.saturating_add(pv.delta_work)))
    }

    /// Capture the SDK channel's **replay-relevant** state for a snapshot (task
    /// 73): the seeded stream position (buggify fault + entropy supply) and the
    /// emitted event log. A fork from a mid-run snapshot restores this so its
    /// seeded streams continue from the right position and it keeps the catalog
    /// the never-fired report needs. `None` when no SDK channel is wired.
    pub fn sdk_snapshot(&self) -> Option<SdkSnapshot> {
        self.sdk.as_ref().map(|s| SdkSnapshot {
            stream: s.env.stream_state(),
            events: s.events.clone(),
            pending_snapshot: s.pending_snapshot,
        })
    }

    /// Restore a captured [`SdkSnapshot`] **verbatim** (the replay path): the
    /// seeded stream position **and** the event prefix. A no-op when no SDK
    /// channel is wired (a non-SDK replay).
    pub fn sdk_restore(&mut self, snap: &SdkSnapshot) {
        if let Some(s) = self.sdk.as_mut() {
            s.env.restore_stream_state(&snap.stream);
            s.events = snap.events.clone();
            // Restore the deferred snapshot-point flag: it is hash-folded
            // (round-8), so a verbatim replay must reproduce it exactly. The
            // branch path (`sdk_restore_events`) deliberately leaves it at the
            // fresh `false` from `enable_sdk` — a reseeded fork re-runs from the
            // restored image (where `setup_complete` is already past) and must not
            // re-surface an already-sealed deferred point.
            s.pending_snapshot = snap.pending_snapshot;
        }
    }

    /// Restore only the **event prefix** of a captured [`SdkSnapshot`] (the branch
    /// path): a branch reseeds, so the seeded streams start fresh from the new
    /// seed (`enable_sdk`), but the shared prefix events — the declared catalog —
    /// carry over so the fork's never-fired report is complete.
    pub fn sdk_restore_events(&mut self, snap: &SdkSnapshot) {
        if let Some(s) = self.sdk.as_mut() {
            s.events = snap.events.clone();
        }
    }

    /// The `Moment`-stamped SDK event stream captured this run (task 73), for the
    /// link tier to decode. Empty when no SDK channel is wired or nothing was
    /// emitted.
    pub fn sdk_events(&self) -> &[(u64, u32, Vec<u8>)] {
        self.sdk
            .as_ref()
            .map(|s| s.events.as_slice())
            .unwrap_or(&[])
    }

    /// Take the pending SDK stop (an assertion violation / snapshot point) the
    /// doorbell surfaced, clearing it. `None` when no SDK stop is pending.
    pub fn take_sdk_stop(&mut self) -> Option<SdkStop> {
        self.sdk.as_mut().and_then(|s| s.pending_stop.take())
    }

    /// The buggify decisions this run resolved, `(moment, answer)`, in order.
    /// Evidence that a run exercised buggify (the box gate reads it); the
    /// reproducer itself carries buggify as the seed + the buggify-only policy,
    /// so these are **not** re-recorded as overrides (which would make a bug's
    /// env carry guest overrides the control server rejects on branch).
    pub fn sdk_buggify(&self) -> &[(u64, environment::Answer)] {
        self.sdk
            .as_ref()
            .map(|s| s.buggify.as_slice())
            .unwrap_or(&[])
    }

    /// Service one hypercall-doorbell `OUT` (task 73 seam 1): copy the request
    /// frame the guest staged at [`REQ_GPA`], route the Event / SDK service,
    /// write the response frame to [`RESP_GPA`], and — for an assertion violation
    /// or a `setup_complete` — arm a [`SdkStop`]. One exit ⇒ the whole exchange
    /// is serviced before the guest resumes (the single-`OUT` atomic doorbell).
    pub(crate) fn service_doorbell(&mut self, req_len: u32) -> Result<Step, VmmError> {
        // ABI: the request occupies exactly one page — the loopback host reads a
        // fixed `MAX_FRAME` buffer. A `req_len` past the page is a malformed request:
        // REJECT it with a clean `BadRequest` (round-4 P2) rather than silently
        // clamping the read to a page and servicing a frame the guest never framed
        // (which could mask a guest-side length bug).
        if req_len as usize > HC_PAGE {
            let mut resp = [0_u8; HC_PAGE];
            let n = encode_response(ServiceId::Event, 1, 0, Status::BadRequest, &[], &mut resp)
                .unwrap_or(0);
            self.write_doorbell_response(&resp[..n])?;
            return Ok(Step::Continued);
        }
        let req_len = req_len as usize;
        // Copy the request out of guest RAM so the immutable borrow ends before
        // we compute the response and write the response page.
        let ram = self.guest_memory();
        let Some(req) = ram.get(REQ_GPA..REQ_GPA + req_len).map(<[u8]>::to_vec) else {
            return Err(VmmError::ContractViolation(format!(
                "doorbell request page {REQ_GPA:#x}+{req_len} is out of guest RAM ({} bytes)",
                ram.len()
            )));
        };
        // A synchronized-or-lower-bound V-time — deterministic across same-seed
        // runs (the axis is seed-derived), which is all the `Moment` stamp needs.
        let moment = self.effective_vns().unwrap_or(0);
        let mut resp = [0_u8; HC_PAGE];
        let (resp_len, stop) = self.dispatch_doorbell(moment, &req, &mut resp);
        self.write_doorbell_response(&resp[..resp_len])?;
        match stop {
            Some(s) => {
                if let Some(sdk) = self.sdk.as_mut() {
                    sdk.pending_stop = Some(s);
                }
                Ok(Step::SdkStop)
            }
            None => Ok(Step::Continued),
        }
    }

    /// Write a doorbell response frame into the response page. The guest zeroed
    /// that page before ringing, so writing only the frame leaves a clean tail.
    fn write_doorbell_response(&mut self, resp: &[u8]) -> Result<(), VmmError> {
        let ram = self.ram.as_mut_bytes();
        let Some(dst) = ram.get_mut(RESP_GPA..RESP_GPA + resp.len()) else {
            return Err(VmmError::ContractViolation(format!(
                "doorbell response page {RESP_GPA:#x}+{} is out of guest RAM ({} bytes)",
                resp.len(),
                ram.len()
            )));
        };
        dst.copy_from_slice(resp);
        // A host-side RAM write the backend's dirty log cannot see — record it
        // for the harvest union (task 95 M2.1 safety rule).
        self.mark_host_dirty(RESP_GPA as u64, resp.len() as u64);
        Ok(())
    }

    /// Route one decoded doorbell request to the Event / SDK service, writing the
    /// response frame into `resp` and returning `(response length, optional
    /// stop)`. Total and panic-free on any request bytes.
    fn dispatch_doorbell(
        &mut self,
        moment: u64,
        req: &[u8],
        resp: &mut [u8],
    ) -> (usize, Option<SdkStop>) {
        let Ok((header, payload)) = decode(req) else {
            // A malformed request: a clean BadRequest (service/opcode 0).
            let n =
                encode_response(ServiceId::Event, 1, 0, Status::BadRequest, &[], resp).unwrap_or(0);
            return (n, None);
        };
        // Validate EVERY request-header invariant `decode` does not already
        // enforce, in ONE step, before any routing (`is_request`: kind == request,
        // status == 0, reserved == 0). `decode` accepts both request and response
        // frames and a request's `status` is a response-only field, so a
        // response-typed OR non-zero-status frame in the guest's request bytes must
        // NOT be serviced (it would mis-service on the raw service/opcode). Reject
        // with a clean BadRequest echoing the raw fields. (Service/opcode validity
        // is a routing outcome below — UnknownService / UnknownOpcode, not
        // BadRequest.)
        if !header.is_request() {
            let n = encode_error(
                header.service,
                header.opcode,
                header.seq,
                Status::BadRequest,
                resp,
            );
            return (n, None);
        }
        // The Pvclock service (id 7, op 1): the guest publishes its clock-page
        // GPA (task 110). Registration validates + records the page and stamps
        // it canonically; an un-offered / non-determinism-path composition
        // answers `UnknownService` so a probing guest cleanly keeps its
        // trap-backstopped time paths. No seeded stream is touched either way
        // (the inert-guest `state_hash` property needs no guard here).
        if header.service == ServiceId::Pvclock as u16 && header.opcode == 1 {
            if payload.len() != 8 {
                let n = encode_response(
                    ServiceId::Pvclock,
                    1,
                    header.seq,
                    Status::BadRequest,
                    &[],
                    resp,
                )
                .unwrap_or(0);
                return (n, None);
            }
            let mut gpa_bytes = [0_u8; 8];
            gpa_bytes.copy_from_slice(payload);
            let (status, abi) = self.pvclock_register(u64::from_le_bytes(gpa_bytes));
            let body = abi.map(u32::to_le_bytes);
            let n = encode_response(
                ServiceId::Pvclock,
                1,
                header.seq,
                status,
                body.as_ref().map(<[u8; 4]>::as_slice).unwrap_or(&[]),
                resp,
            )
            .unwrap_or(0);
            return (n, None);
        }
        // The Event service (id 4, op 1): capture the `Moment`-stamped emission
        // and, for an assert violation / `setup_complete`, arm a stop.
        if header.service == ServiceId::Event as u16 && header.opcode == 1 {
            if payload.len() < 4 {
                let n = encode_response(
                    ServiceId::Event,
                    1,
                    header.seq,
                    Status::BadRequest,
                    &[],
                    resp,
                )
                .unwrap_or(0);
                return (n, None);
            }
            let id = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
            let data = &payload[4..];
            // Validate the SDK event payload BEFORE acting on it (round-14): a
            // malformed frame for a namespace the host inspects — an assert
            // VIOLATION whose declared detail length does not fit the frame, or a
            // `setup_complete` carrying bytes — is rejected with BadRequest and NOT
            // captured/armed/surfaced, so a bug or a snapshot deferral is never
            // synthesized from garbage guest bytes.
            let (stop, defer) = match Self::classify_sdk_event(id, data) {
                SdkEventAction::Malformed => {
                    let n = encode_response(
                        ServiceId::Event,
                        1,
                        header.seq,
                        Status::BadRequest,
                        &[],
                        resp,
                    )
                    .unwrap_or(0);
                    return (n, None);
                }
                SdkEventAction::Stop(s) => (Some(s), false),
                SdkEventAction::DeferSnapshot => (None, true),
                SdkEventAction::Capture => (None, false),
            };
            if let Some(sdk) = self.sdk.as_mut() {
                sdk.events.push((moment, id, data.to_vec()));
                // Task 73 (P1): `setup_complete` is a lifecycle milestone at a
                // skid-tainted doorbell OUT — not sealable here. Defer a snapshot
                // point; the control loop surfaces it at the next synchronized
                // boundary, where a seal succeeds.
                if defer {
                    sdk.pending_snapshot = true;
                }
            }
            let n = encode_response(ServiceId::Event, 1, header.seq, Status::Ok, &[], resp)
                .unwrap_or(0);
            return (n, stop);
        }
        // The SDK service (id 6, op 1): resolve a buggify decision.
        if header.service == ServiceId::Sdk as u16 && header.opcode == 1 {
            if payload.len() != 4 {
                let n =
                    encode_response(ServiceId::Sdk, 1, header.seq, Status::BadRequest, &[], resp)
                        .unwrap_or(0);
                return (n, None);
            }
            let point = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
            let fire = self.decide_buggify(moment, point);
            let n = encode_response(
                ServiceId::Sdk,
                1,
                header.seq,
                Status::Ok,
                &[u8::from(fire)],
                resp,
            )
            .unwrap_or(0);
            return (n, None);
        }
        // The Net service (id 5, op 1): resolve one per-flow decision. Decode the
        // fixed 18-byte `NetFlow` decision point, ask the reproducer, and answer
        // the opaque encoded flow policy the guest enforces. One decision per
        // flow/connection — the host stays on the control path.
        if header.service == ServiceId::Net as u16 && header.opcode == 1 {
            // Gate on the Net channel being wired. The doorbell is serviced whenever
            // EITHER sdk or net is enabled, so a guest that rings `net_decide` on a
            // run with only the SDK channel wired reaches here with `self.net` unset.
            // Answer a clean `UnknownService` — NOT out-of-gate behavior: without
            // this guard `decide_net` would draw a NetFlow answer from the shared SDK
            // stream (advancing it, perturbing buggify) for a service the run never
            // offered. With the guard, an unwired-Net guest never touches the stream,
            // so the inert-guest `state_hash` is unchanged (there is no draw).
            if self.net.is_none() {
                let n = encode_response(
                    ServiceId::Net,
                    1,
                    header.seq,
                    Status::UnknownService,
                    &[],
                    resp,
                )
                .unwrap_or(0);
                return (n, None);
            }
            let Some(point) = NetFlowPoint::decode(payload) else {
                let n =
                    encode_response(ServiceId::Net, 1, header.seq, Status::BadRequest, &[], resp)
                        .unwrap_or(0);
                return (n, None);
            };
            let answer = self.decide_net(moment, point.src, point.dst, point.conn, point.event);
            // The encoded answer is a handful of bytes (a `Nominal` tag or a small
            // net fault), always well within a frame payload; fail closed if not.
            let n = encode_response(ServiceId::Net, 1, header.seq, Status::Ok, &answer, resp)
                .unwrap_or_else(|_| {
                    encode_response(ServiceId::Net, 1, header.seq, Status::Internal, &[], resp)
                        .unwrap_or(0)
                });
            return (n, None);
        }
        // The Entropy service (id 2, op 1): the SDK's `entropy_fill` source. Route
        // it through the VMM's `SeededEntropy` stream — the **same** one RDRAND
        // draws from (round-5 P2) — so a guest's RDRAND and its hypercall RNG never
        // duplicate words, and a fork resumes the single stream via the VM snapshot
        // (`save_vm_state`), not a second SDK-channel stream. The stream validates
        // the request (a `u32` count, `1..=MAX_PAYLOAD`) and fills the buffer; fail
        // closed with a `BadRequest` if V-time — hence the stream — is unwired.
        if header.service == ServiceId::Entropy as u16 && header.opcode == 1 {
            let mut buf = [0_u8; MAX_PAYLOAD];
            let (status, got) = match self.vtime.as_mut() {
                Some(vt) => vt.draw_entropy(payload, &mut buf),
                None => (Status::BadRequest, 0),
            };
            let m = encode_response(ServiceId::Entropy, 1, header.seq, status, &buf[..got], resp)
                .unwrap_or(0);
            return (m, None);
        }
        // Any other service/opcode: the SDK demo rings Event / Sdk / Entropy, so
        // this is unreached in practice. Answer a clean UnknownOpcode for a known
        // service, and a clean UnknownService (echoing the raw service id) for an
        // unrecognized one — never a silent drop (the guest transport reads an
        // unwritten response page as a host rejection and hangs, violating the
        // hypercall error contract). A fuller campaign wiring Console/Block
        // registers them here.
        if header.service == ServiceId::Event as u16 {
            let n = encode_response(
                ServiceId::Event,
                header.opcode,
                header.seq,
                Status::UnknownOpcode,
                &[],
                resp,
            )
            .unwrap_or(0);
            (n, None)
        } else if header.service == ServiceId::Sdk as u16 {
            let n = encode_response(
                ServiceId::Sdk,
                header.opcode,
                header.seq,
                Status::UnknownOpcode,
                &[],
                resp,
            )
            .unwrap_or(0);
            (n, None)
        } else if header.service == ServiceId::Net as u16 {
            // Net is a KNOWN service (op 1 handled above), so a bad opcode is
            // `UnknownOpcode`, not the `UnknownService` fall-through — consistent
            // with the Event/Sdk/Entropy arms (task 61).
            let n = encode_response(
                ServiceId::Net,
                header.opcode,
                header.seq,
                Status::UnknownOpcode,
                &[],
                resp,
            )
            .unwrap_or(0);
            (n, None)
        } else if header.service == ServiceId::Entropy as u16 {
            // Entropy is a KNOWN service (op 1 handled above), so a bad opcode is
            // `UnknownOpcode`, not the `UnknownService` fall-through below (round-10
            // P3 — consistent with the Event/Sdk arms).
            let n = encode_response(
                ServiceId::Entropy,
                header.opcode,
                header.seq,
                Status::UnknownOpcode,
                &[],
                resp,
            )
            .unwrap_or(0);
            (n, None)
        } else if header.service == ServiceId::Pvclock as u16 {
            // Pvclock is a KNOWN service (op 1 handled above), so a bad opcode
            // is `UnknownOpcode` — consistent with the other known services.
            let n = encode_response(
                ServiceId::Pvclock,
                header.opcode,
                header.seq,
                Status::UnknownOpcode,
                &[],
                resp,
            )
            .unwrap_or(0);
            (n, None)
        } else {
            // An unrecognized service id: no `ServiceId` variant represents it, so
            // echo the raw fields via `encode_error` (round-9 P2) — the guest gets
            // a correlatable `UnknownService` frame instead of a hang.
            let n = encode_error(
                header.service,
                header.opcode,
                header.seq,
                Status::UnknownService,
                resp,
            );
            (n, None)
        }
    }

    /// Classify a captured SDK Event emission (`id` + `data`) at the doorbell,
    /// **validating** the payload for the namespaces the host acts on (task 73 seam
    /// 3, round-14). The host inspects exactly two:
    ///
    /// - **assert VIOLATION** (`SDK_NS_ASSERT`, disposition `1`): surfaces a bug
    ///   ([`SdkStop::Assertion`]). Payload `[disposition u8][detail_len u16][detail]`;
    ///   the declared `detail_len` must match the remaining bytes EXACTLY (no
    ///   truncation, no trailing bytes) or the frame is [`Malformed`](SdkEventAction::Malformed).
    /// - **`setup_complete`** (`SDK_NS_LIFECYCLE`, local 0): arms the deferred
    ///   snapshot point ([`DeferSnapshot`](SdkEventAction::DeferSnapshot)). It carries
    ///   NO payload; a nonempty one is [`Malformed`](SdkEventAction::Malformed).
    ///
    /// A `Malformed` frame is rejected (BadRequest) and never captured/armed/
    /// surfaced, so a bug or a snapshot deferral is never synthesized from garbage.
    /// Every OTHER emission (a hit, an unknown assert disposition, a state register,
    /// a buggify result, the catalog, an unknown namespace) is
    /// [`Capture`](SdkEventAction::Capture): captured raw for the **total** link-tier
    /// decode, which owns their validation — the host takes no action on them.
    fn classify_sdk_event(id: u32, data: &[u8]) -> SdkEventAction {
        let ns = (id >> SDK_NS_SHIFT) as u8;
        let local = id & SDK_LOCAL_MASK;
        match ns {
            SDK_NS_ASSERT if data.first() == Some(&SDK_DISP_VIOLATION) => {
                // assert payload = [disposition u8][detail_len u16][detail]. The
                // declared detail length must fit the frame EXACTLY.
                let Some(len_bytes) = data.get(1..3) else {
                    return SdkEventAction::Malformed;
                };
                let dl = u16::from_le_bytes([len_bytes[0], len_bytes[1]]) as usize;
                match data.get(3..) {
                    Some(detail) if detail.len() == dl => {
                        SdkEventAction::Stop(SdkStop::Assertion {
                            id: local,
                            data: detail.to_vec(),
                        })
                    }
                    // detail_len overflows the frame, or trailing bytes remain.
                    _ => SdkEventAction::Malformed,
                }
            }
            // `setup_complete` carries no payload; a nonempty one is malformed.
            SDK_NS_LIFECYCLE if local == 0 => {
                if data.is_empty() {
                    SdkEventAction::DeferSnapshot
                } else {
                    SdkEventAction::Malformed
                }
            }
            // Everything else is captured raw; the link tier validates it.
            _ => SdkEventAction::Capture,
        }
    }

    /// Take the deferred `setup_complete` snapshot point **iff** the VM is now at a
    /// **sealable** boundary — the FULL `save_vm_state` precondition
    /// ([`Vmm::can_snapshot`]: synchronized AND no staged RNG completion), plus an
    /// exact V-time ([`Vmm::is_synchronized`]) to stamp the point. The control loop
    /// calls this after a `Continued` step; `true` means surface
    /// `StopReason::SnapshotPoint` here — a point where the explorer's eager
    /// `save_vm_state` seal succeeds, not `NotQuiescent`.
    ///
    /// **Round-4 P1:** gating on `is_synchronized()` alone surfaced a point at the
    /// first synchronized exit after `setup_complete` even when that exit was an
    /// RDRAND/RDSEED (a staged RNG completion), which `save_vm_state` rejects — and
    /// clearing `pending_snapshot` on that unsealable surface LOST the point. Now
    /// `pending_snapshot` is cleared ONLY when the point is actually surfaced (a
    /// sealable boundary), so an RNG boundary defers it to the next clean one.
    pub fn take_synchronized_snapshot_point(&mut self) -> bool {
        if self.can_snapshot()
            && self.is_synchronized()
            && let Some(sdk) = self.sdk.as_mut()
            && sdk.pending_snapshot
        {
            sdk.pending_snapshot = false;
            return true;
        }
        false
    }

    /// Resolve a buggify decision for `point` at `moment` (task 73 seam 3): ask
    /// the SDK channel's `Environment` (seeded fault stream / recorded override),
    /// capture the answer for the reproducer, and return whether to fire.
    fn decide_buggify(&mut self, moment: u64, point: u32) -> bool {
        use environment::{Answer, DecisionPoint, Environment, Fault, Outcome};
        let Some(sdk) = self.sdk.as_mut() else {
            return false;
        };
        // `environment::Moment` is the retired-instruction axis (a `u64`).
        sdk.env.set_moment(moment);
        let ans = match sdk.env.decide(&DecisionPoint::Buggify { point }) {
            Outcome::Resolved(a) => a,
            // A pure backing (RecordedEnv) never needs the host; be total anyway.
            Outcome::NeedsHost => Answer::Nominal,
        };
        let fire = matches!(ans, Answer::Fault(Fault::BuggifyFire));
        sdk.buggify.push((moment, ans));
        fire
    }

    /// Resolve one `net_decide` flow decision (task 61): stamp the surfacing
    /// `Moment`, ask the reproducer's `Environment::decide` for the flow's policy,
    /// capture `(moment, conn, answer)`, and return the **encoded** answer bytes
    /// the guest decodes and enforces. Mirrors [`decide_buggify`] exactly — one
    /// wire shape whether the flow is answered from the seeded fault stream or a
    /// recorded override — swapping the `Buggify` point for a `NetFlow` one.
    /// Returns a one-byte encoded `Nominal` if no net channel is wired.
    fn decide_net(&mut self, moment: u64, src: u32, dst: u32, conn: u64, event: u16) -> Vec<u8> {
        use environment::{Answer, ConnId, DecisionPoint, Environment, FlowEvent, NodeId, Outcome};
        // Today the flow agent only surfaces flow-open; any event id maps to
        // `Open` (the catalog's sole `FlowEvent` — deliberately extensible) rather
        // than being rejected, so a newer agent asking about a not-yet-modeled
        // transition still gets a (nominal-or-policy) answer instead of a hang.
        let _ = event;
        let point = DecisionPoint::NetFlow {
            src: NodeId(src),
            dst: NodeId(dst),
            conn: ConnId(conn),
            event: FlowEvent::Open,
        };
        // Draw from the ONE shared fault-decision stream the SDK channel owns (the
        // single-stream ruling): a net decision advances the same hash-folded
        // stream buggify draws from, so buggify answers after a net draw match the
        // canonical one-stream reproducer. Without an SDK channel there is no shared
        // stream to draw from (not a production path — the control server always
        // wires SDK), so answer a nominal policy rather than opening a second stream.
        let Some(sdk) = self.sdk.as_mut() else {
            return Answer::Nominal.encode();
        };
        // `environment::Moment` is the retired-instruction axis (a `u64`).
        sdk.env.set_moment(moment);
        let ans = match sdk.env.decide(&point) {
            Outcome::Resolved(a) => a,
            // A pure backing (RecordedEnv) never needs the host; be total anyway.
            Outcome::NeedsHost => Answer::Nominal,
        };
        let bytes = ans.encode();
        // Capture the decision in the Net channel's log (host-side evidence).
        if let Some(net) = self.net.as_mut() {
            net.decisions.push((moment, conn, ans));
        }
        bytes
    }

    /// The V-time (ns) the xAPIC sees — for the Current-Count register read and for
    /// the LAPIC timer's expiry. `0` when V-time is unwired (M1/M2 never touch the
    /// APIC page, so this is moot there).
    ///
    /// The work value it reads differs by backend capability, **not** backend
    /// identity (R-Backend allows querying [`Backend::capabilities`]):
    ///
    /// - **Determinism-complete backend** (`deterministic_tsc`, the patched KVM /
    ///   the mock): the **skid-free last-intercept anchor** — the same value the
    ///   `VTIM`/`LAPC` hash uses. The patched backend traps every `RDTSC`, so the
    ///   anchor advances densely *and* deterministically, and two same-seed boots
    ///   fire the timer at bit-identical V-times (Phase B.2 / task-30 Phase C).
    /// - **Stock backend** (no `RDTSC` trap): the anchor advances only at the rare
    ///   `RDMSR(IA32_TSC)` intercepts and would freeze post-boot, so the periodic
    ///   tick would never advance jiffies and the userspace serial-TX drain would
    ///   stall. Read the **live** work counter instead — it advances with guest
    ///   branches, so the timer keeps firing and the boot reaches `GUEST_READY`.
    ///   Stock claims no determinism (Phase B.1 only *reaches* the milestone), so a
    ///   skid-laden live read is sound here. The live read at this exit boundary is
    ///   the work retired up to the faulting instruction (no guest code runs between
    ///   the exit and this call).
    ///
    /// A failed work-counter read is **fail-closed** ([`VmmError::Work`]) — the same
    /// posture as the TSC/RNG completions — rather than silently reusing a stale
    /// `last_intercept_work` (which would freeze or shift the timer, a determinism
    /// hazard) or fabricating a clock value.
    pub(crate) fn now_vns(&self) -> Result<u64, VmmError> {
        match &self.vtime {
            Some(vt) => {
                let work = if self.backend.capabilities().arch.deterministic_clock() {
                    vt.last_intercept_work
                } else {
                    vt.work.work()?
                };
                Ok(vt.clock.snapshot_vns(work))
            }
            None => Ok(0),
        }
    }

    /// The next-timer V-time deadline as an absolute retired-branch **work count**
    /// for [`Backend::run_until`], or `None` to keep the plain open-ended `run()`.
    ///
    /// Returns `Some` only on the **determinism-complete** path (a backend with a
    /// deterministic retired-branch counter — [`Capabilities::deterministic_tsc`])
    /// with the LAPIC wired and its timer armed. That gating keeps `run_until`
    /// strictly **additive**, so the protected goldens never take it:
    ///
    /// - **M1/M2/P6/corpus/multiboot** never wire the LAPIC → always `run()`.
    /// - **Stock KVM** (no deterministic counter; [`Self::lapic_now_vns`] reads live
    ///   work and fires the timer at natural exits — Phase B.1) → always `run()`.
    /// - **Patched KVM Linux boot** → `run_until` the timer deadline, so a
    ///   busy-spinning guest is preempted on time (task 47 / ROADMAP D4).
    ///
    /// The timer deadline is V-time ns; [`VClock::work_for_vns`] converts it to the
    /// work axis the counter measures (rounding **up** to the first work count whose
    /// V-time reaches the deadline, so the post-preemption anchor does fire the timer).
    ///
    /// [`Capabilities::deterministic_tsc`]: vmm_backend::Capabilities
    pub(crate) fn preemption_deadline(&self) -> Option<Moment> {
        let deadline_vns = self.armed_timer_deadline_vns()?;
        let vt = self
            .vtime
            .as_ref()
            .expect("armed_timer_deadline_vns implies V-time wired");
        Some(Moment(vt.clock.work_for_vns(deadline_vns)))
    }

    /// The `run_until` work-count deadline for the next [`step`](Vmm::step): the
    /// **nearest** of the task-47 LAPIC-timer [`preemption_deadline`](Self::preemption_deadline),
    /// the task-59 host-fault [`arrival_deadline`](Self::arrival_deadline), and
    /// the task-110 pvclock staleness bound
    /// ([`pvclock_refresh_deadline`](Self::pvclock_refresh_deadline)).
    /// `None` (none armed) keeps the plain open-ended `run()`, so every path
    /// that stages no fault, arms no timer, and registers no clock page is
    /// byte-for-byte unchanged. Taking the min is what lets the deadlines
    /// coexist: the guest is forced out at whichever seed-deterministic work
    /// count comes first, and the losers stay armed for the following step.
    pub(crate) fn run_until_deadline(&self) -> Option<Moment> {
        [
            self.preemption_deadline(),
            self.arrival_deadline,
            self.pvclock_refresh_deadline(),
        ]
        .into_iter()
        .flatten()
        .map(|m| m.0)
        .min()
        .map(Moment)
    }

    /// Arm a **host-fault arrival deadline** at `moment` (task 59): the next
    /// [`step`](Vmm::step) runs (via `run_until`) no further than the
    /// retired-branch work count whose effective V-time is `moment`, so the
    /// frontier can stop *between instructions* at exactly that count and apply a
    /// staged fault. `moment` is on the single [`Moment`](environment::Moment)
    /// axis (a V-time / retired-count; [`effective_vns`](Vmm::effective_vns)
    /// reports the same axis).
    ///
    /// Returns `true` iff the deadline was armed. It arms **only on the
    /// determinism-complete path** (V-time wired *and* a deterministic
    /// retired-branch counter), exactly like [`preemption_deadline`](Self::preemption_deadline):
    /// arrival needs the exact-count `run_until` seam, which stock KVM / M1 / M2
    /// do not provide. When it returns `false` the caller falls back to running to
    /// a natural exit and comparing [`effective_vns`](Vmm::effective_vns).
    pub fn arm_arrival(&mut self, moment: environment::Moment) -> bool {
        if !self.can_arm_arrival() {
            self.arrival_deadline = None;
            return false;
        }
        let vt = self
            .vtime
            .as_ref()
            .expect("can_arm_arrival implies V-time wired");
        self.arrival_deadline = Some(Moment(vt.clock.work_for_vns(moment)));
        true
    }

    /// `true` iff [`arm_arrival`](Vmm::arm_arrival) can arm an **exact-count**
    /// arrival on this backend — the determinism-complete path (V-time wired *and*
    /// a deterministic retired-branch counter). The frontier capability-checks this
    /// **once, up front** before accepting a host-plane perturbation: without the
    /// exact-arrival seam a staged fault could only be applied at a natural exit
    /// *past* its `Moment` (stock KVM / M1 / M2), which host-plane enforcement
    /// forbids — so such a backend rejects `perturb` rather than silently applying
    /// late (task 59; PR #51 round-2 finding). Pure; does not touch the arm.
    pub fn can_arm_arrival(&self) -> bool {
        self.vtime.is_some() && self.backend.capabilities().arch.deterministic_clock()
    }

    /// The armed host-fault arrival as an **effective V-time** (`vns`), or `None`
    /// when nothing is armed. [`arm_arrival`](Vmm::arm_arrival) stores the arrival
    /// as a retired-branch **work count** (`work_for_vns(moment)`); this inverts it
    /// back to the `Moment`'s V-time via the same clock, so the idle planner can
    /// weigh the arrival against the LAPIC timer's V-time deadline (both on the
    /// `vns` axis) and jump to whichever comes first — see
    /// [`idle_action`](Vmm::idle_action). Round-trips exactly under the contract
    /// clock (`ratio_den == 1`).
    pub(crate) fn arrival_vns(&self) -> Option<u64> {
        let d = self.arrival_deadline?;
        let vt = self.vtime.as_ref()?;
        Some(vt.clock.snapshot_vns(d.0))
    }

    /// The current **entropy-stream state** of the seeded RNG (the raw xorshift
    /// word), or `None` when V-time / the seeded stream is unwired. Because
    /// [`reseed_entropy`](Vmm::reseed_entropy) seeds via `SeededEntropy::new(seed)`
    /// and a non-zero state is a fixed point of that seeding, re-seeding a fresh VM
    /// with **this** value reproduces the current stream exactly — which is why the
    /// control server records it as the reproducer's seed after a `replay` (whose
    /// restored snapshot may sit mid-stream, under a seed unrelated to the prior
    /// session — PR #51 round-2 finding) as well as after a `branch`.
    pub fn entropy_state(&self) -> Option<u64> {
        self.vtime.as_ref().map(|vt| {
            let bytes = vt.entropy.save_state();
            let mut buf = [0u8; 8];
            // `SeededEntropy::save_state` is always the 8-byte LE state word.
            buf.copy_from_slice(&bytes[..8]);
            u64::from_le_bytes(buf)
        })
    }

    /// Disarm any [`arm_arrival`](Vmm::arm_arrival) deadline, so the next
    /// [`step`](Vmm::step) is bounded only by the task-47 preemption deadline (or
    /// runs open-ended if none is armed). Idempotent.
    pub fn clear_arrival(&mut self) {
        self.arrival_deadline = None;
    }

    /// Apply one host-plane [`HostFault`](environment::HostFault) **imperatively,
    /// between instructions** (task 59) — the enforcement seam task 45 declared
    /// frontier. Called by the frontier when a run has arrived at the fault's
    /// [`Moment`](environment::Moment):
    ///
    /// - [`CorruptMemory`](environment::HostFault::CorruptMemory): XOR the
    ///   [`BitMask`](environment::BitMask) into the little-endian 8-byte word at
    ///   guest-physical `gpa` in the owned [`GuestRam`] (on the box KVM reads the
    ///   guest through this same backing, so the corruption is live on the next
    ///   entry). **Fails loud** ([`VmmError::ContractViolation`]) when
    ///   `gpa + 8 > guest RAM` rather than clip or wrap — a corruption at an
    ///   unrepresentable address would not replay. (The server rejects the same
    ///   condition earlier, at stage time, with a recoverable `ControlError`; this
    ///   is the defensive backstop.)
    /// - [`InjectInterrupt`](environment::HostFault::InjectInterrupt): raise the
    ///   `vector` into the userspace-LAPIC IRR so the **existing** IRQ arbitration
    ///   ([`service_pending_irqs`](Self::service_pending_irqs)) delivers it at the
    ///   next injectable entry — delivery ordering vs. the V-time timer stays
    ///   deterministic. Requires the LAPIC wired (the Linux boot path) and a
    ///   non-reserved `vector` (`≥ 16`); both fail loud otherwise.
    /// - [`SkewTime`](environment::HostFault::SkewTime) /
    ///   [`SetClockRate`](environment::HostFault::SetClockRate): **out of scope**
    ///   for task 59 (they mutate the V-time clock itself; a follow-on lights them
    ///   up). Rejected loud so a schedule carrying one never silently no-ops.
    pub fn apply_host_fault(&mut self, fault: &environment::HostFault) -> Result<(), VmmError> {
        match fault {
            environment::HostFault::CorruptMemory { gpa, mask } => {
                self.corrupt_memory(*gpa, mask.0)
            }
            environment::HostFault::InjectInterrupt { vector } => {
                <B::A as Vendor>::inject_wire_interrupt(self, *vector)
            }
            environment::HostFault::SkewTime(_) | environment::HostFault::SetClockRate(_) => {
                Err(VmmError::ContractViolation(
                    "SkewTime/SetClockRate host faults are out of scope for task 59 (they mutate \
                     the V-time clock itself) — a follow-on lights them up; refusing to silently \
                     no-op a staged clock fault"
                        .to_string(),
                ))
            }
        }
    }

    /// XOR `mask` (as a little-endian 64-bit word) into the 8 guest-physical bytes
    /// at `gpa`. The single-event-upset apply of [`CorruptMemory`]; a pure
    /// function of `(gpa, mask)` over the current RAM, so replaying the same fault
    /// at the same [`Moment`](environment::Moment) reproduces it bit-for-bit.
    /// Fails loud on `gpa + 8 > ram` (never clips/wraps).
    ///
    /// [`CorruptMemory`]: environment::HostFault::CorruptMemory
    fn corrupt_memory(&mut self, gpa: u64, mask: u64) -> Result<(), VmmError> {
        let ram = self.ram.as_mut_bytes();
        let end = gpa.checked_add(8).filter(|&e| e <= ram.len() as u64);
        let Some(end) = end else {
            return Err(VmmError::ContractViolation(format!(
                "CorruptMemory gpa {gpa:#x} + 8 is out of range (guest RAM is {} bytes) — refusing \
                 to clip or wrap the upset",
                ram.len()
            )));
        };
        let start = gpa as usize;
        let end = end as usize;
        let word = u64::from_le_bytes(
            ram[start..end]
                .try_into()
                .expect("slice is exactly 8 bytes"),
        );
        ram[start..end].copy_from_slice(&(word ^ mask).to_le_bytes());
        // A host-side RAM write the backend's dirty log cannot see — record it
        // (the 8-byte upset may straddle a page boundary; the helper covers both).
        self.mark_host_dirty(gpa, 8);
        Ok(())
    }

    /// Record `[gpa, gpa + len)` as **host-written** for the dirty harvest (task
    /// 95 M2.1): every gfn the range touches. Called by the exhaustive set of
    /// production host-write paths — [`Vmm::write_doorbell_response`] and
    /// [`Vmm::corrupt_memory`]; the third, [`Vmm::restore_guest_memory`], is a
    /// full-image write and latches [`Self::host_dirty_wholesale`] instead. Any
    /// **new** host write into guest RAM must call one of the two, or derived
    /// snapshots silently corrupt — that invariant is the review centerpiece.
    pub(crate) fn mark_host_dirty(&mut self, gpa: u64, len: u64) {
        if len == 0 {
            return;
        }
        let first = gpa / 4096;
        let last = (gpa + len - 1) / 4096;
        self.host_dirty.extend(first..=last);
    }

    /// Harvest the **complete dirty-gfn set since the last drain** — the
    /// backend's guest-write log unioned with the host-side writes this `Vmm`
    /// performed — sorted ascending, deduplicated; and re-arm both for the next
    /// window (task 95 M2.1).
    ///
    /// Returns `None` on **any doubt**: the backend cannot harvest (no dirty
    /// tracking, an ioctl error) or an untrackable full-image host write
    /// happened ([`Vmm::restore_guest_memory`]). `None` obliges the caller to
    /// full-scan — the dirty set is a cost hint, never a correctness input, so
    /// this deliberately returns an `Option`, not a `Result` whose error a
    /// caller could act on. After a `None` the tracking window is NOT re-armed; call
    /// [`Vmm::reset_dirty_tracking`] at the next baseline.
    pub fn harvest_dirty_gfns(&mut self) -> Option<Vec<u64>> {
        if self.host_dirty_wholesale {
            return None;
        }
        let mut gfns = self.backend.harvest_dirty_gfns().ok()?;
        // The backend half is already sorted+deduped; fold in the host-side gfns.
        gfns.extend(self.host_dirty.iter().copied());
        gfns.sort_unstable();
        gfns.dedup();
        self.host_dirty.clear();
        Some(gfns)
    }

    /// Harvest-and-discard: reset the dirty tracking so the **current** state is
    /// the baseline the next [`Vmm::harvest_dirty_gfns`] measures from (task 95
    /// M2.1's arm point — right after a seal or a branch restore). Clears the
    /// host-side set and the wholesale latch, and drains the backend log.
    /// Returns `true` iff the backend log was actually reset — `false` means
    /// tracking is not armed and the next capture must full-scan.
    pub fn reset_dirty_tracking(&mut self) -> bool {
        self.host_dirty.clear();
        self.host_dirty_wholesale = false;
        self.backend.harvest_dirty_gfns().is_ok()
    }

    /// The next-timer **V-time deadline (ns)** on the determinism-complete path, or
    /// `None` when V-time is unwired, the backend has no deterministic counter, the
    /// LAPIC is unwired, or no timer is armed. The shared gating behind both
    /// [`Self::preemption_deadline`] (which converts it to a work count for `run_until`,
    /// the *execution* path) and [`Self::idle_resume_target`] (which warps the clock to
    /// it, the *idle* path) — the two halves of the discrete-event clock. `Some` here is
    /// exactly the condition "a LAPIC timer is armed on the determinism path", so an
    /// idle `HLT` is resumable precisely when this is `Some` **and** the guest can take
    /// the interrupt (`RFLAGS.IF == 1`).
    pub(crate) fn armed_timer_deadline_vns(&self) -> Option<u64> {
        if self.vtime.is_none() || !self.backend.capabilities().arch.deterministic_clock() {
            return None;
        }
        <B::A as Vendor>::next_timer_deadline_vns(self)
    }

    /// Handle [`CommonExit::Deadline`]: the guest was preempted at exactly `reached`
    /// retired branches (a pure function of the seed — bit-identical across same-seed
    /// runs even mid-spin). Advance the skid-free last-intercept anchor to it — a
    /// deterministic V-time intercept, like an RDTSC trap — so the NEXT `step`'s
    /// [`Self::service_pending_irqs`] sees [`Self::lapic_now_vns`] at the timer
    /// deadline, fires the timer into the LAPIC IRR, and injects it at the first
    /// injectable entry. No completion (the backend left nothing pending).
    pub(crate) fn on_deadline(&mut self, reached: Moment) -> Result<Step, VmmError> {
        // Trace the MEASURED landing work (diagnostic, not hashed) for the seed-dependence
        // gate — capped so a constantly-preempting guest can't grow it unbounded.
        if self.preemption_landings.len() < PREEMPTION_TRACE_CAP {
            self.preemption_landings.push(reached.0);
        }
        match self.vtime.as_mut() {
            Some(vt) => {
                vt.last_intercept_work = reached.0;
                // The preemption point is an exact work boundary, so V-time is
                // synchronized (a snapshot taken right here is exact, like any
                // other intercept).
                self.vtime_synchronized = true;
                Ok(Step::Continued)
            }
            // A `Deadline` only ever answers a `run_until`, which the VMM issues
            // ONLY on the V-time-wired determinism path ([`Self::preemption_deadline`]).
            // One arriving with no V-time wired is a backend contract violation —
            // fail closed, never silently absorbed.
            None => Err(VmmError::ContractViolation(format!(
                "Exit::Common(CommonExit::Deadline (reached {})) with no V-time wired — run_until is the \
                 determinism-path preemption seam and is never issued without it",
                reached.0
            ))),
        }
    }

    /// Handle [`CommonExit::Idle`]: discriminate a **resumable idle** halt from a **terminal**
    /// one and act ([`Self::idle_action`]). The guest is either *waiting for an interrupt
    /// that will come* or *dead*. A resumable idle either delivers an already-pending
    /// interrupt (zero V-time advance) or jumps V-time to a future deliverable timer
    /// ([`Self::resume_idle`]); everything else (the kernel's final `cli; hlt` after
    /// poweroff, or any wait nothing will satisfy) terminates exactly as before — the
    /// strictly-additive change of task 52.
    pub(crate) fn on_idle(&mut self) -> Result<Step, VmmError> {
        match self.idle_action()? {
            // A deliverable interrupt is already pending in the IRR (e.g. a one-shot
            // timer that fired while `IF == 0`, then `sti; hlt`): re-enter with **no**
            // clock change — the next `service_pending_irqs` delivers it.
            IdleAction::DeliverPending => Ok(Step::Continued),
            // No interrupt pending now, but a deliverable timer is armed for the future:
            // jump V-time to it and re-enter.
            IdleAction::JumpToDeadline(deadline_vns) => self.resume_idle(deadline_vns),
            IdleAction::Terminal => Ok(self.terminate(TerminalReason::Idle)),
        }
    }

    /// Decide what an idle exit should do. **Resumable iff** the guest can take an
    /// interrupt (the vendor's interruptibility test — x86 `RFLAGS.IF`) on the
    /// determinism path **and** a *deliverable* wake event exists — either one already
    /// pending in the interrupt fabric now ([`IdleAction::DeliverPending`],
    /// zero-advance) **or** a future deliverable armed timer
    /// ([`IdleAction::JumpToDeadline`]). Otherwise [`IdleAction::Terminal`].
    ///
    /// **Pending-now takes precedence over a future deadline.** A one-shot timer may
    /// have already fired into the fabric (its deadline hit while interrupts were
    /// masked), and the guest then idles with them unmasked — now there is no future
    /// armed deadline but a deliverable interrupt is pending and must wake the halt
    /// immediately (a normal Linux pattern: a timer fires in a critical section, then
    /// the CPU idles). So the discriminator keys on a *deliverable interrupt existing*,
    /// not merely on a future armed deadline.
    ///
    /// **Deliverability, not just armed.** A timer can be *armed* yet *undeliverable*
    /// (a reserved vector, or masked by the guest's priority threshold), in which case
    /// it fires into the fabric but never injects, so a one-shot leaves no future wake.
    /// Such a timer is **terminal**, never a resumable idle — the vendor's
    /// [`deliverable_timer_deadline_vns`](Vendor::deliverable_timer_deadline_vns)
    /// filters it out.
    ///
    /// The determinism-path gate comes **first**: the common terminal paths
    /// (minimal-boot poweroff, M1/M2/corpus, stock KVM) take the early `Terminal`, so
    /// their behavior and `state_hash` are byte-for-byte unchanged (the no-regression
    /// gate). The interruptibility read is a [`Backend::save`] (a pure vCPU read
    /// running no guest code) and **fails closed** ([`VmmError::Backend`]) on error.
    fn idle_action(&mut self) -> Result<IdleAction, VmmError> {
        // Determinism path only (stock / M1/M2 keep an idle halt terminal,
        // byte-identical).
        if self.vtime.is_none() || !self.backend.capabilities().arch.deterministic_clock() {
            return Ok(IdleAction::Terminal);
        }
        // The guest must be resumable (able to take an interrupt / be woken).
        if !<B::A as Vendor>::guest_interruptible(self)? {
            return Ok(IdleAction::Terminal);
        }
        // (a) A deliverable interrupt already pending in the fabric → re-enter, no
        //     clock change. Takes precedence over a future deadline.
        if <B::A as Vendor>::pending_deliverable_interrupt(self)? {
            return Ok(IdleAction::DeliverPending);
        }
        // (b) No pending wake, but a future scheduled event → jump to the FIRST one.
        //     Two competing discrete events wake an idle guest, and V-time must land
        //     at whichever comes first (PR #51 round-4): the deliverable fabric timer
        //     **and** a staged host-fault arrival ([`arm_arrival`](Vmm::arm_arrival)).
        //     Fold them the same way `run_until_deadline` folds arrival into the run:
        //     jump to `min(timer, arrival)`, waking at the arrival to apply.
        //
        //     **The arrival wakes independent of the fabric (PR #51 round-6).** A host
        //     fault is a host-plane event, not a guest interrupt — so a V-time-wired
        //     guest with **no fabric wired** that idles before a staged `Moment` still
        //     wakes at the arrival to apply it, rather than going `Terminal` and
        //     silently never applying an accepted perturb. The timer half stays
        //     fabric-gated (there is no timer without a fabric). With neither a timer
        //     nor a staged arrival the guest is terminal — byte-identical to before.
        let timer = <B::A as Vendor>::deliverable_timer_deadline_vns(self);
        let wake = match (timer, self.arrival_vns()) {
            (Some(t), Some(a)) => Some(t.min(a)),
            (only, None) | (None, only) => only,
        };
        match wake {
            Some(vns) => Ok(IdleAction::JumpToDeadline(vns)),
            // Neither a pending, a timer, nor an arrival wake → terminal.
            None => Ok(IdleAction::Terminal),
        }
    }

    /// Resume a *resumable idle* `HLT` by **jumping** V-time to the armed timer's
    /// deadline `deadline_vns` — reaching the next scheduled event without executing a
    /// single instruction (the idle dual of the `run_until` execution path).
    ///
    /// **Skid-free + work-axis epoch rebase (task-52 review fixes).** Two intertwined
    /// determinism requirements drive this:
    ///
    /// 1. *No skid-tainted read.* A live `work()` read at a `HLT` is skid-tainted (the
    ///    task-27 box O1 evidence shows a non-V-time-intercept live read **diverges**
    ///    across same-seed runs). So the landing V-time is derived from the **skid-free**
    ///    anchor [`last_intercept_work`](VtimeWiring) + the (seed-deterministic) timer
    ///    deadline — never the live counter at the halt.
    /// 2. *Work-axis epoch consistency.* The clock is `vns(work) = vns_base + work·ratio`
    ///    over the **cumulative** counter. Simply bumping `vns_base` (against the stale
    ///    anchor) while the counter keeps counting leaves the two axes inconsistent: the
    ///    pre-idle branches (between the last intercept and the halt) would be counted a
    ///    second time at the next intercept, and the next deadline→work conversion
    ///    ([`Self::preemption_deadline`] via [`VClock::work_for_vns`]) would land *behind*
    ///    the live counter → the periodic tick fires immediately (overdue), breaking
    ///    cadence. So the jump **rebases the work epoch**: it resets the retired-branch
    ///    counter to 0 (both counter A and the backend's counter B) and folds the landing
    ///    V-time entirely into `vns_base`, anchored at 0 — exactly a snapshot-style
    ///    restore to effective V-time `landing` with entropy/`tsc_adjust` unchanged. This
    ///    is the **same proven machinery as [`Self::restore_vtime`]**, so it reuses it.
    ///    The pre-idle branches are absorbed into the jump (the guest retires **zero**
    ///    branches during the halt, so no executed branch is lost or fabricated); post-idle
    ///    work counts from 0, so the next tick lands a full period in the **future**.
    ///
    /// After the rebase `vns(0) == landing`, so the next [`Self::service_pending_irqs`]
    /// fires the timer into the LAPIC IRR and injects it, and `step` re-enters. The
    /// landing is the [`vtime::IdlePlanner`] seam (deterministic base: land exactly at the
    /// deadline; a future fault-overlay could prescribe `deadline + δ`).
    pub(crate) fn resume_idle(&mut self, deadline_vns: u64) -> Result<Step, VmmError> {
        // The landing V-time, decided by the planner from the SKID-FREE anchor clock
        // (never a live HLT read). For a future deadline (guaranteed by `idle_action`'s
        // "not already fired" gate) this is exactly `deadline_vns`.
        let (landing, snap) = {
            let vt = self
                .vtime
                .as_ref()
                .expect("JumpToDeadline implies V-time wired");
            let now_vns = vt.clock.snapshot_vns(vt.last_intercept_work);
            let landing = IdlePlanner::new().plan(now_vns, deadline_vns).landed_vns;
            // The idle jump IS a restore to effective V-time `landing` with the current
            // entropy stream and `tsc_adjust` (unchanged — the guest drew nothing while
            // idle). Reuse the proven `restore_vtime` epoch-rebase (resets both work
            // counters, folds `landing` into `vns_base`, anchors at 0).
            let snap = VtimeSnapshot {
                vns: landing,
                guest_clock_offset: vt.guest_clock_offset,
                entropy: vt.entropy.save_state(),
            };
            (landing, snap)
        };
        self.restore_vtime(&snap)?;
        // Trace the idle V-time landing (skid-free; observability only, not hashed; capped).
        if self.idle_landings.len() < PREEMPTION_TRACE_CAP {
            self.idle_landings.push(landing);
        }
        Ok(Step::Continued)
    }

    /// The current V-time work count for the loud §1 MSR log, or `None` when
    /// V-time is unwired (stock KVM / M1/M2) — logged honestly as `unwired`
    /// rather than a fake `0` that would read as a real branch count. A bad
    /// counter read also degrades to `None` (logging must never abort a run; the
    /// architectural effect is serviced afterward where its error path applies).
    pub(crate) fn current_work(&self) -> Option<u64> {
        self.vtime.as_ref().and_then(|vt| vt.work.work().ok())
    }

    /// The vCPU state for the hash: the snapshot captured at terminal if present,
    /// else a best-effort live `save` (default on a backend that cannot save —
    /// never happens for the mock or `KvmBackend` post-run).
    pub(crate) fn current_vcpu(&self) -> VcpuOf<B> {
        match &self.saved_state {
            Some(s) => s.clone(),
            None => self.backend.save().unwrap_or_default(),
        }
    }
}

/// Append a domain-tagged, length-prefixed chunk: `tag(4) ‖ len(u64 LE) ‖ bytes`.
pub(crate) fn put_chunk(out: &mut Vec<u8>, tag: &[u8; 4], bytes: &[u8]) {
    out.extend_from_slice(tag);
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// Deterministic, fixed-layout encoding of the V-time + seeded-RNG state for the
/// `VTIM` hash chunk: the clock-rate config (4 × `u64` LE — `ratio_num`, `guest_hz`,
/// `guest_base`, `tsc_adjust`), then the **single canonical effective-V-time field**
/// (`u64` LE), then the entropy stream position (`SeededEntropy::save_state`, the
/// trailing bytes — the enclosing chunk is length-prefixed by `put_chunk`). A change
/// in seed, ratio, `guest_hz`/`guest_base`, `tsc_adjust` (`IA32_TSC_ADJUST`), effective
/// V-time, or stream position changes the hash. `ratio_den` is **not** encoded:
/// [`VtimeWiring::new`] enforces it `== 1`, so it is an invariant constant (hashing
/// it would add nothing and be unkillable).
///
/// Two task-27 (item 2) properties this layout guarantees:
///
/// - **Restore-transparency.** `vns_base` and the work counter are **not** hashed
///   separately; they are folded into one effective-V-time field,
///   `clock.snapshot_vns(last_intercept_work) = vns_base + last_intercept_work·ratio`.
///   So a restored VM (`vns_base = E`, work `0`) and a fresh VM at the same effective
///   V-time (`vns_base = 0`, work `E`) hash **identically** — the equivalence
///   `unison::compare_runs` relies on.
/// - **Determinism-twice.** The effective V-time is anchored to
///   `last_intercept_work` — the **deterministic** work at the last V-time intercept
///   (every determinism-cap trap RDTSC/RDTSCP/RDRAND/RDSEED, and the
///   `IA32_TSC`/`IA32_TSC_ADJUST` MSR paths) — **never** a live
///   `work()` read at hash time. A terminal live read carries the non-deterministic
///   post-last-intercept exit-path skid, which is exactly what made the `VTIM` chunk
///   diverge intermittently across two same-seed runs (box corpus O1, PR #51). The
///   encoding is now **total and infallible** (no counter read, no poison sentinel).
///
/// **Deliberate property — `state_blob` is V-time replay-equivalence up to the last
/// synchronized intercept (integrator ruling).** The effective V-time is the V-time at
/// the **last V-time intercept** — the synchronized, skid-corrected point — **not** the
/// live counter at the hashing exit. So **two states are equal iff identical at that
/// last intercept**; post-intercept work — distinguishable only by re-synchronizing at
/// the next RDTSC/RNG — is **intentionally not captured because it is not
/// deterministically measurable** (only the determinism-cap traps + TSC MSRs are
/// skid-corrected; the raw counter at a non-V-time exit carries the non-deterministic
/// skid that was the original O1 bug). This is **not a silent bug — it is the correct
/// hash**: it is **exact for same-seed determinism (O1)** — box-proven, both runs reach
/// the same intercepts with the same skid-free work — and the under-capture for
/// *differential* comparison is resolved at the very next intercept. Hashing at
/// non-intercept exits is **required** (the corpus checkpoints at `isa-debug-exit`, a
/// non-intercept), so "refuse to hash off an intercept" would be wrong; hashing the
/// live counter would reintroduce skid. The **snapshot** path has the *opposite*
/// requirement (it needs the exact current V-time, so [`Vmm::save_vtime`] fails closed
/// off an intercept) — same skid fact, different correct resolution.
fn encode_vtime(vt: &VtimeWiring) -> Vec<u8> {
    let mut v = Vec::new();
    for x in [
        vt.cfg.ratio_num,
        vt.cfg.guest_hz,
        vt.cfg.guest_base,
        vt.guest_clock_offset,
    ] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.extend_from_slice(&vt.clock.snapshot_vns(vt.last_intercept_work).to_le_bytes());
    v.extend_from_slice(&vt.entropy.save_state());
    v
}

/// Deterministic, fixed-layout encoding of the task-73 SDK channel's
/// **replay-relevant** state for the `SDK\0` hash chunk (round-7): the seeded
/// stream positions (16 bytes — the buggify + inert supply PRNG states) and the
/// pending stop. The event log is deliberately excluded (host-side observation,
/// like the report stream). A different buggify draw sequence (a diverged fork)
/// moves the stream state, so it hashes differently.
fn encode_sdk_channel(sdk: &SdkChannel) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&sdk.env.stream_state());
    match &sdk.pending_stop {
        None => v.push(0),
        Some(SdkStop::Assertion { id, data }) => {
            v.push(1);
            v.extend_from_slice(&id.to_le_bytes());
            v.extend_from_slice(&(data.len() as u32).to_le_bytes());
            v.extend_from_slice(data);
        }
    }
    v.push(u8::from(sdk.pending_snapshot));
    // The active fault policy (round-8 P1): a stream position alone does not
    // determine the buggify fire/nominal sequence — the policy does — so two
    // same-stream forks under different policies must hash differently.
    v.extend_from_slice(&(sdk.policy.len() as u32).to_le_bytes());
    v.extend_from_slice(&sdk.policy);
    v
}

#[cfg(test)]
mod tests {
    //! Engine tests, driven over the x86 vendor (`MockBackend`'s `Arch` is `X86`) —
    //! the engine is generic, but a test needs *a* vendor to run against.

    use super::*;
    use vmm_backend::{Gpa, VcpuState, X86, X86Caps, X86Exit, X86Policy};

    use crate::vendor::x86::devices::REPORT_PORT;
    use crate::vendor::x86::dispatch::{
        APIC_MMIO_BASE, COM1_IRQ_VECTOR, DOORBELL_PORT, IA32_TSC_ADJUST, MsrDir, RFLAGS_IF,
        contract_vclock_config, lookup_cpuid,
    };
    use crate::vendor::x86::records as snapshot;

    /// Guest RAM for the snapshot/save/restore-shaped tests: 128 KiB natively,
    /// 64 KiB under Miri — the smallest size covering the doorbell protocol pages
    /// (`REQ_GPA` `0xE000` / reply `0xF000`, production constants). These tests'
    /// dominant interpreted cost is the sha256 `state_hash` over the `MEM` chunk
    /// (plus full-image copies), which scales with this size, so halving it under
    /// `cfg(miri)` halves the vmm-core Miri job's long tail (task 98 / hm-d8o).
    /// Native runs are byte-for-byte unchanged.
    const TEST_RAM: usize = if cfg!(miri) { 0x1_0000 } else { 0x2_0000 };

    #[test]
    fn msr_dir_renders_direction_and_exit_reason() {
        assert_eq!(MsrDir::Read.dir(), "RDMSR");
        assert_eq!(MsrDir::Write.dir(), "WRMSR");
        assert_eq!(MsrDir::Read.exit_reason(), "KVM_EXIT_X86_RDMSR");
        assert_eq!(MsrDir::Write.exit_reason(), "KVM_EXIT_X86_WRMSR");
    }

    #[test]
    fn lookup_cpuid_exact_leaf_only_and_default() {
        // Exact (leaf, subleaf) match returns the frozen entry (leaf-1 EAX =
        // det-cfl-v1 family/model/stepping 06_9e_0c).
        let l1 = lookup_cpuid(1, 0);
        assert_eq!(l1.leaf, 1);
        assert_eq!(l1.eax, 0x0009_06ec);
        // Significant-subleaf exact match (leaf 4 subleaf 2 EAX from the contract).
        assert_eq!(lookup_cpuid(4, 2).eax, 0x0000_0143);
        // Leaf-only fallback: leaf 1 has a single (insignificant) subleaf, so an
        // unlisted subleaf still returns that entry (kills the `!significant` and
        // `e.subleaf == subleaf` mutants).
        assert_eq!(lookup_cpuid(1, 99).eax, 0x0009_06ec);
        // No match at all → a zeroed default that carries the queried (leaf,
        // subleaf) (kills the return-Default and field-delete mutants).
        let d = lookup_cpuid(0xDEAD, 5);
        assert_eq!((d.leaf, d.subleaf, d.eax), (0xDEAD, 5, 0));
    }

    // -----------------------------------------------------------------------
    // V-time / seeded-RNG completion (task 21 P4), driven by a scripted
    // MockBackend + a portable WorkSource — no /dev/kvm, runs on every platform.
    // -----------------------------------------------------------------------
    use crate::work::ScriptedWork;
    use std::cell::Cell;
    use vmm_backend::{Completion, CpuidModel, MockBackend, MsrFilter};

    /// A configured MockBackend (so `run`/`step` pass the `NotConfigured` gate)
    /// pre-loaded with `exits`.
    fn configured_mock(exits: Vec<Exit<X86>>) -> MockBackend {
        let mut m = MockBackend::with_exits(exits);
        m.set_policy(&X86Policy {
            cpuid: CpuidModel::default(),
            msr_filter: MsrFilter::default(),
        })
        .expect("set_policy");
        m
    }

    /// A `Vmm<MockBackend>` with the determinism path wired (clock + work + seed).
    fn vtime_vmm(exits: Vec<Exit<X86>>, work: Box<dyn WorkSource>, seed: u64) -> Vmm<MockBackend> {
        let mut vmm = Vmm::new(configured_mock(exits), GuestRam::new(0x1000).unwrap());
        vmm.wire_vtime(VtimeWiring::new(contract_vclock_config(), work, seed).unwrap());
        vmm
    }

    // ---- task 95 M2.1: the dirty harvest (backend log ∪ host-side writes) ----

    /// The harvest unions the backend's guest-write log with the host-side
    /// writes the Vmm performed (here a `CorruptMemory` straddling a page
    /// boundary), sorted + deduplicated — and draining re-arms the window.
    #[test]
    fn harvest_unions_backend_log_with_host_writes_and_drains() {
        let mut m = configured_mock(vec![]);
        m.push_dirty_gfns(vec![5, 3, 5]); // scripted guest writes, unsorted + dup
        let mut vmm = Vmm::new(m, GuestRam::new(TEST_RAM).unwrap());
        // An 8-byte upset straddling the page-6/page-7 boundary: both gfns count.
        vmm.apply_host_fault(&environment::HostFault::CorruptMemory {
            gpa: 7 * 4096 - 4,
            mask: environment::BitMask(0xFFFF_FFFF_FFFF_FFFF),
        })
        .unwrap();
        assert_eq!(vmm.harvest_dirty_gfns(), Some(vec![3, 5, 6, 7]));
        // Drained: the next window starts empty (the mock's exhausted script is
        // an empty guest-write set, and the host set was cleared).
        assert_eq!(vmm.harvest_dirty_gfns(), Some(vec![]));
    }

    /// The doorbell response write — the run loop's host-side RAM write — lands
    /// in the harvest (the safety rule's production case).
    #[test]
    fn doorbell_response_write_is_harvested_as_host_dirty() {
        let mut m = configured_mock(vec![]);
        m.enable_dirty_tracking();
        let mut vmm = Vmm::new(m, GuestRam::new(TEST_RAM).unwrap());
        vmm.write_doorbell_response(&[0xAB; 16]).unwrap();
        // RESP_GPA = 0xF000 → gfn 15.
        assert_eq!(
            vmm.harvest_dirty_gfns(),
            Some(vec![(RESP_GPA as u64) / 4096])
        );
    }

    /// A full-image host write (`restore_guest_memory`) poisons the harvest —
    /// per-gfn tracking cannot vouch for it — until the explicit re-arm.
    #[test]
    fn wholesale_host_write_poisons_the_harvest_until_reset() {
        let mut m = configured_mock(vec![]);
        m.enable_dirty_tracking();
        let mut vmm = Vmm::new(m, GuestRam::new(TEST_RAM).unwrap());
        vmm.restore_guest_memory(&vec![7u8; TEST_RAM]).unwrap();
        assert_eq!(vmm.harvest_dirty_gfns(), None, "untrackable ⇒ no dirty set");
        assert!(vmm.reset_dirty_tracking(), "re-arm at the new baseline");
        assert_eq!(vmm.harvest_dirty_gfns(), Some(vec![]));
    }

    /// Without backend dirty tracking the harvest always declines (`None`) and
    /// the window never arms — the caller full-scans forever, never corrupts.
    #[test]
    fn harvest_declines_without_backend_tracking() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        vmm.write_doorbell_response(&[1]).unwrap();
        assert_eq!(vmm.harvest_dirty_gfns(), None);
        assert!(!vmm.reset_dirty_tracking());
    }

    /// A work source that yields a strictly increasing value (current, then
    /// += `step`) on each read — models one retired branch between two exits.
    struct AutoWork {
        next: Cell<u64>,
        step: u64,
    }
    impl WorkSource for AutoWork {
        fn work(&self) -> Result<u64, WorkError> {
            let v = self.next.get();
            self.next.set(v.saturating_add(self.step));
            Ok(v)
        }
        fn reset(&mut self) -> Result<(), WorkError> {
            self.next.set(0);
            Ok(())
        }
    }

    /// The seeded draw the `Entropy` hypercall service produces for `width` bytes,
    /// recomputed independently so the test pins the *value*, not just the path.
    fn expected_draw(seed: u64, width: u8) -> u64 {
        let mut e = SeededEntropy::new(seed);
        let mut buf = [0u8; 8];
        let n = usize::from(width);
        let (st, got) = e.handle(1, &(n as u32).to_le_bytes(), &mut buf[..n]);
        assert_eq!((st, got), (Status::Ok, n));
        u64::from_le_bytes(buf)
    }

    /// Task 73: the hypercall doorbell services an Event emission (captured,
    /// `Moment`-stamped) and a buggify decision (answered from the env), and an
    /// assert-violation event / `setup_complete` surface the right `SdkStop`.
    #[test]
    fn doorbell_services_events_buggify_and_surfaces_stops() {
        use environment::{Answer, EnvSpec, Fault, FaultPolicy};

        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        // Point 50 always fires; the seeded base answers everything else.
        let mut policy = FaultPolicy::none();
        policy.set_buggify_point(50, 1, 1).unwrap();
        let spec = EnvSpec::Seeded { seed: 7, policy };
        vmm.enable_sdk(spec.materialize(), spec.policy());

        // Stage `payload` as a request frame at REQ_GPA, service the doorbell,
        // and decode the response frame — returning `(step, status, payload)`.
        fn ring(
            vmm: &mut Vmm<MockBackend>,
            service: ServiceId,
            payload: &[u8],
        ) -> (Step, u16, Vec<u8>) {
            let mut buf = [0u8; HC_PAGE];
            let n = hypercall_proto::encode_request(service, 1, 1, payload, &mut buf).unwrap();
            vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&buf[..n]);
            let step = vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
            let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
            let (hdr, pl) = decode(&page).expect("a valid response frame");
            (step, hdr.status, pl.to_vec())
        }

        // Buggify point 50 fires (1/1) → response byte 1.
        let (step, status, pl) = ring(&mut vmm, ServiceId::Sdk, &50u32.to_le_bytes());
        assert_eq!((step, status), (Step::Continued, Status::Ok as u16));
        assert_eq!(pl, vec![1], "point 50 fires");

        // A sometimes-hit event (assert ns, point 1, disposition hit): captured, continue.
        let hit_id = (1u32 << 24) | 1;
        let mut hit = hit_id.to_le_bytes().to_vec();
        hit.extend_from_slice(&[0, 0, 0]); // [DISP_HIT, detail_len=0]
        let (step, status, _) = ring(&mut vmm, ServiceId::Event, &hit);
        assert_eq!((step, status), (Step::Continued, Status::Ok as u16));

        // An always-violation event (assert ns, point 20, disposition violation): SdkStop.
        let viol_id = (1u32 << 24) | 20;
        let mut viol = viol_id.to_le_bytes().to_vec();
        viol.extend_from_slice(&[1, 0, 0]); // [DISP_VIOLATION, detail_len=0]
        let (step, status, _) = ring(&mut vmm, ServiceId::Event, &viol);
        assert_eq!((step, status), (Step::SdkStop, Status::Ok as u16));
        assert_eq!(
            vmm.take_sdk_stop(),
            Some(SdkStop::Assertion {
                id: 20,
                data: vec![]
            })
        );

        // setup_complete (lifecycle ns, local 0): NO immediate stop — its
        // snapshot point is deferred (P1) to the next synchronized boundary. The
        // event is still captured; the doorbell continues.
        let setup_id = 4u32 << 24;
        let (step, status, _) = ring(&mut vmm, ServiceId::Event, &setup_id.to_le_bytes());
        assert_eq!((step, status), (Step::Continued, Status::Ok as u16));
        assert!(
            vmm.take_sdk_stop().is_none(),
            "setup_complete does not stop"
        );

        // All three emissions were captured (Moment 0, no vtime wired), and the
        // buggify decision recorded a fire.
        let ids: Vec<u32> = vmm.sdk_events().iter().map(|(_, id, _)| *id).collect();
        assert_eq!(ids, vec![hit_id, viol_id, setup_id]);
        assert_eq!(vmm.sdk_buggify(), &[(0, Answer::Fault(Fault::BuggifyFire))]);
    }

    /// Task 61: the `Net` doorbell decodes a flow decision point, resolves it
    /// through the reproducer, answers the encoded flow policy, captures the
    /// decision at its `Moment`, and — the load-bearing property — a fresh replay
    /// from the same reproducer reproduces the identical answer at the identical
    /// `Moment`. This is the host half of the record→replay closure the box gates
    /// exercise end-to-end.
    #[test]
    fn net_doorbell_decides_records_and_replays() {
        use environment::{Answer, DecisionClass, EnvSpec, Fault, FaultPolicy};

        // Stage a `net_decide` request for one flow and return the decoded answer.
        fn ask_flow(vmm: &mut Vmm<MockBackend>, src: u32, dst: u32, conn: u64) -> (u16, Answer) {
            let mut payload = Vec::new();
            payload.extend_from_slice(&src.to_le_bytes());
            payload.extend_from_slice(&dst.to_le_bytes());
            payload.extend_from_slice(&conn.to_le_bytes());
            payload.extend_from_slice(&0u16.to_le_bytes()); // FlowEvent::Open
            let mut buf = [0u8; HC_PAGE];
            let n =
                hypercall_proto::encode_request(ServiceId::Net, 1, 1, &payload, &mut buf).unwrap();
            vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&buf[..n]);
            let step = vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
            assert_eq!(step, Step::Continued);
            let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
            let (hdr, pl) = decode(&page).expect("a valid response frame");
            (
                hdr.status,
                Answer::decode(pl).expect("a valid encoded answer"),
            )
        }

        // A fault policy that faults every flow with a `NetReset` (1/1), so the
        // seeded answer for the `NetFlow` class is deterministic from the seed.
        let mut policy = FaultPolicy::none();
        policy
            .set_class(DecisionClass::NetFlow, 1, 1, &[Fault::NetReset])
            .unwrap();
        let spec = EnvSpec::Seeded { seed: 7, policy };

        // First run: the doorbell answers the flow and records it at Moment 0. The
        // net decision draws from the SHARED SDK stream (the single-stream ruling),
        // so the SDK channel is wired with the same reproducer + policy.
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        vmm.enable_sdk(spec.materialize(), spec.policy());
        vmm.enable_net();
        let (status, ans) = ask_flow(&mut vmm, 1, 2, 42);
        assert_eq!(status, Status::Ok as u16);
        assert_eq!(ans, Answer::Fault(Fault::NetReset), "seeded flow policy");
        assert_eq!(
            vmm.net_decisions(),
            &[(0, 42, Answer::Fault(Fault::NetReset))],
            "the decision is captured at its Moment/conn"
        );

        // Replay: a fresh VM materialized from the SAME reproducer reproduces the
        // identical answer at the identical Moment — bit-identical decision.
        let mut replay = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        replay.enable_sdk(spec.materialize(), spec.policy());
        replay.enable_net();
        let (rstatus, rans) = ask_flow(&mut replay, 1, 2, 42);
        assert_eq!((rstatus, &rans), (Status::Ok as u16, &ans));
        assert_eq!(replay.net_decisions(), vmm.net_decisions());
    }

    /// Task 61: a `Net` doorbell without a wired channel is impossible (the gate
    /// requires it), but a wrong-length payload and a wrong opcode both fail
    /// closed with a clean status — never a hang or a phantom decision.
    #[test]
    fn net_doorbell_rejects_malformed_requests() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        // A malformed request is rejected before any decide, so no shared stream is
        // needed — enable Net alone (the doorbell is serviced when net is wired).
        vmm.enable_net();

        // A short (non-18-byte) payload → BadRequest, no decision recorded.
        let mut buf = [0u8; HC_PAGE];
        let n = hypercall_proto::encode_request(ServiceId::Net, 1, 1, &[0u8; 4], &mut buf).unwrap();
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&buf[..n]);
        vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
        let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
        let (hdr, _) = decode(&page).unwrap();
        assert_eq!(hdr.status, Status::BadRequest as u16);
        assert!(vmm.net_decisions().is_empty());

        // A wrong opcode on the known Net service → UnknownOpcode.
        let n =
            hypercall_proto::encode_request(ServiceId::Net, 9, 1, &[0u8; 18], &mut buf).unwrap();
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&buf[..n]);
        vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
        let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
        let (hdr, _) = decode(&page).unwrap();
        assert_eq!(hdr.status, Status::UnknownOpcode as u16);
    }

    /// Task 61 (R4): a `net_decide` on a run where **Net was never enabled** (only
    /// the SDK channel is wired, so the doorbell is still serviced) gets a clean
    /// `UnknownService` — NOT out-of-gate behavior — and, critically, does NOT draw
    /// from the shared SDK stream: a following buggify answer is identical to one on
    /// a VM that never saw the net_decide. So an unwired-Net guest cannot perturb
    /// the SDK stream / `state_hash` through the Net service.
    #[test]
    fn net_decide_without_enable_net_is_unknown_service_and_leaves_the_stream() {
        use environment::{DecisionClass, EnvSpec, Fault, FaultPolicy};
        let mut policy = FaultPolicy::none();
        policy
            .set_class(DecisionClass::NetFlow, 1, 1, &[Fault::NetReset])
            .unwrap();
        policy.set_buggify_point(1, 1, 2).unwrap();
        let spec = EnvSpec::Seeded { seed: 9, policy };

        // SDK wired, Net NOT wired.
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        vmm.enable_sdk(spec.materialize(), spec.policy());

        // Ring net_decide → UnknownService (the doorbell is serviced because SDK is
        // wired), no decision captured.
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&2u32.to_le_bytes());
        payload.extend_from_slice(&7u64.to_le_bytes());
        payload.extend_from_slice(&0u16.to_le_bytes());
        let mut buf = [0u8; HC_PAGE];
        let n = hypercall_proto::encode_request(ServiceId::Net, 1, 1, &payload, &mut buf).unwrap();
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&buf[..n]);
        vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
        let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
        let (hdr, _) = decode(&page).unwrap();
        assert_eq!(hdr.status, Status::UnknownService as u16);
        assert!(vmm.net_decisions().is_empty());

        // The rejected net_decide left the shared stream untouched: buggify draws the
        // stream's FIRST word, exactly as on a VM that never rang net_decide.
        let fired = vmm.decide_buggify(1, 1);
        let mut fresh = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        fresh.enable_sdk(spec.materialize(), spec.policy());
        assert_eq!(
            fired,
            fresh.decide_buggify(1, 1),
            "the rejected net_decide did not advance the shared SDK stream"
        );
    }

    /// Round-14 malformed-SDK-event-payload MATRIX: `classify_sdk_event` validates
    /// every payload the host acts on, so a bug (assert violation) or a snapshot
    /// deferral (setup_complete) is never synthesized from garbage. One place, the
    /// whole table — no more one-field-per-round.
    #[test]
    fn classify_sdk_event_payload_matrix() {
        type C = SdkEventAction;
        let assert_id = (u32::from(SDK_NS_ASSERT) << SDK_NS_SHIFT) | 20;
        let setup_id = u32::from(SDK_NS_LIFECYCLE) << SDK_NS_SHIFT;
        let state_id = (2u32 << SDK_NS_SHIFT) | 3; // a state register (link-owned)
        let classify = Vmm::<MockBackend>::classify_sdk_event;

        // --- assert VIOLATION (disposition 1): detail_len must fit EXACTLY. ---
        // Well-formed: no detail (len 0).
        assert_eq!(
            classify(assert_id, &[1, 0, 0]),
            C::Stop(SdkStop::Assertion {
                id: 20,
                data: vec![]
            })
        );
        // Well-formed: 2 detail bytes declared and present.
        assert_eq!(
            classify(assert_id, &[1, 2, 0, 0xAB, 0xCD]),
            C::Stop(SdkStop::Assertion {
                id: 20,
                data: vec![0xAB, 0xCD]
            })
        );
        // Malformed: detail_len (2) OVERFLOWS the frame (0 detail bytes present).
        assert_eq!(classify(assert_id, &[1, 2, 0]), C::Malformed);
        // Malformed: TRAILING bytes past the declared detail_len (0).
        assert_eq!(classify(assert_id, &[1, 0, 0, 0x99]), C::Malformed);
        // Malformed: truncated header (no detail_len u16).
        assert_eq!(classify(assert_id, &[1]), C::Malformed);
        assert_eq!(classify(assert_id, &[1, 0]), C::Malformed);
        // A non-violation disposition (a hit / unknown) is captured raw, no stop —
        // the link tier validates it.
        assert_eq!(classify(assert_id, &[0, 0, 0]), C::Capture); // DISP_HIT
        assert_eq!(classify(assert_id, &[9, 0, 0]), C::Capture); // unknown disposition

        // --- setup_complete: EMPTY payload only. ---
        assert_eq!(classify(setup_id, &[]), C::DeferSnapshot);
        assert_eq!(classify(setup_id, &[0xAB]), C::Malformed); // garbage payload
        assert_eq!(classify(setup_id, &[0; 4]), C::Malformed);

        // --- everything else is captured raw (the link tier owns its validation). ---
        assert_eq!(classify(state_id, &[0, 1, 2, 3]), C::Capture);
        assert_eq!(classify((9u32 << SDK_NS_SHIFT) | 7, &[1, 2, 3]), C::Capture); // unknown ns
    }

    /// End-to-end: a malformed SDK event frame at the doorbell is REJECTED with
    /// BadRequest and is NOT captured, does NOT arm the deferred snapshot point,
    /// and does NOT surface a stop (round-14). A well-formed setup_complete IS
    /// captured (and would arm the deferral).
    #[test]
    fn doorbell_rejects_malformed_sdk_event_payloads() {
        use environment::{EnvSpec, FaultPolicy};

        // Ring an Event(op1) frame carrying `[event_id][data]`; return (status,
        // whether a stop surfaced, sdk_events len after).
        fn ring(vmm: &mut Vmm<MockBackend>, event_id: u32, data: &[u8]) -> (u16, bool, usize) {
            let mut payload = event_id.to_le_bytes().to_vec();
            payload.extend_from_slice(data);
            let mut buf = [0u8; HC_PAGE];
            let n = hypercall_proto::encode_request(ServiceId::Event, 1, 1, &payload, &mut buf)
                .unwrap();
            vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&buf[..n]);
            let step = vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
            let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
            let (hdr, _) = decode(&page).expect("a response frame");
            (hdr.status, step == Step::SdkStop, vmm.sdk_events().len())
        }
        let mk = || {
            let mut v = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            v.enable_sdk(
                EnvSpec::Seeded {
                    seed: 1,
                    policy: FaultPolicy::none(),
                }
                .materialize(),
                &FaultPolicy::none(),
            );
            v
        };
        let assert_id = (u32::from(SDK_NS_ASSERT) << SDK_NS_SHIFT) | 20;
        let setup_id = u32::from(SDK_NS_LIFECYCLE) << SDK_NS_SHIFT;

        // Malformed assert violation (detail_len overflows) → BadRequest, no stop,
        // NOT captured.
        let mut v = mk();
        assert_eq!(
            ring(&mut v, assert_id, &[1, 2, 0]),
            (Status::BadRequest as u16, false, 0),
            "a malformed assert violation is rejected, never a bug from garbage"
        );

        // Malformed setup_complete (carries bytes) → BadRequest, not captured (so
        // it can never arm the deferred snapshot point).
        let mut v = mk();
        assert_eq!(
            ring(&mut v, setup_id, &[0xAB]),
            (Status::BadRequest as u16, false, 0),
            "a non-empty setup_complete is rejected, never arms the deferral"
        );

        // A well-formed setup_complete IS captured (Ok) — the valid path still works.
        let mut v = mk();
        assert_eq!(ring(&mut v, setup_id, &[]), (Status::Ok as u16, false, 1));
    }

    /// A doorbell `OUT` on a Vmm with **no** SDK channel wired stays the
    /// default-deny contract violation (every non-SDK path is unchanged).
    #[test]
    fn doorbell_without_sdk_is_a_contract_violation() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        assert!(matches!(
            vmm.dispatch_out(DOORBELL_PORT, 4, 24),
            Err(VmmError::ContractViolation(_))
        ));
    }

    /// An unrecognized doorbell **service** id is answered with a clean
    /// `UnknownService` frame echoing the raw service/opcode/seq — never a silent
    /// drop that leaves the guest transport hanging on a missing reply (round-9
    /// P2). No `ServiceId` variant names the id, so the request is crafted by
    /// patching the encoded frame's 2-byte service field.
    #[test]
    fn doorbell_unknown_service_returns_an_unknown_service_frame() {
        use environment::{EnvSpec, FaultPolicy};
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        vmm.enable_sdk(
            EnvSpec::Seeded {
                seed: 1,
                policy: FaultPolicy::none(),
            }
            .materialize(),
            &FaultPolicy::none(),
        );
        // Encode a well-formed request, then patch the service field (bytes 6..8)
        // to an id no `ServiceId` represents. opcode 7 / seq 99 are distinct so the
        // echo is observable.
        let mut buf = [0u8; HC_PAGE];
        let n = hypercall_proto::encode_request(ServiceId::Sdk, 7, 99, &[], &mut buf).unwrap();
        let unknown: u16 = 0xABCD;
        buf[6..8].copy_from_slice(&unknown.to_le_bytes());
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&buf[..n]);

        let step = vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
        assert_eq!(step, Step::Continued);
        let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
        let (hdr, pl) = decode(&page).expect("a response frame is written, not a silent drop");
        assert_eq!(
            hdr.status,
            Status::UnknownService as u16,
            "clean UnknownService"
        );
        assert_eq!(
            hdr.service, unknown,
            "echoes the raw service id so the guest correlates the reply"
        );
        assert_eq!(hdr.opcode, 7, "echoes the request opcode");
        assert_eq!(hdr.seq, 99, "echoes the request seq");
        assert!(pl.is_empty(), "an error frame carries no payload");
    }

    /// A **response-typed** frame in the guest's request bytes is rejected with a
    /// clean `BadRequest` (echoing the raw service/opcode/seq), NOT routed as a
    /// request (round-10 P2). `decode` accepts both kinds, so the doorbell must
    /// gate on `is_request()` before routing.
    #[test]
    fn doorbell_rejects_a_non_request_frame() {
        use environment::{EnvSpec, FaultPolicy};
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        vmm.enable_sdk(
            EnvSpec::Seeded {
                seed: 1,
                policy: FaultPolicy::none(),
            }
            .materialize(),
            &FaultPolicy::none(),
        );
        // A well-formed RESPONSE frame (kind == 2) for a real service — it must be
        // rejected as not-a-request rather than serviced (here: the Sdk service,
        // which would otherwise resolve a buggify decision).
        let mut buf = [0u8; HC_PAGE];
        let n = hypercall_proto::encode_response(ServiceId::Sdk, 1, 42, Status::Ok, &[], &mut buf)
            .unwrap();
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&buf[..n]);

        let step = vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
        assert_eq!(
            step,
            Step::Continued,
            "a rejected frame does not stop the run"
        );
        let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
        let (hdr, pl) = decode(&page).expect("a response frame is written");
        assert_eq!(
            hdr.status,
            Status::BadRequest as u16,
            "non-request → BadRequest"
        );
        assert_eq!(hdr.service, ServiceId::Sdk as u16, "echoes the raw service");
        assert_eq!(hdr.seq, 42, "echoes the raw seq");
        // No buggify decision was resolved (the frame never reached the Sdk arm).
        assert!(
            vmm.sdk_buggify().is_empty(),
            "a non-request frame is not serviced as a buggify request"
        );
        assert!(pl.is_empty());
    }

    /// A bad **opcode** on the KNOWN Entropy service returns `UnknownOpcode`
    /// (echoing the service), consistent with the Event/Sdk arms — not the
    /// `UnknownService` fall-through reserved for unregistered service ids
    /// (round-10 P3).
    #[test]
    fn doorbell_bad_entropy_opcode_is_unknown_opcode() {
        use environment::{EnvSpec, FaultPolicy};
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        vmm.enable_sdk(
            EnvSpec::Seeded {
                seed: 1,
                policy: FaultPolicy::none(),
            }
            .materialize(),
            &FaultPolicy::none(),
        );
        // Entropy service, opcode 2 (only op 1 is the entropy_fill source).
        let mut buf = [0u8; HC_PAGE];
        let n = hypercall_proto::encode_request(ServiceId::Entropy, 2, 7, &[], &mut buf).unwrap();
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&buf[..n]);

        vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
        let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
        let (hdr, _) = decode(&page).expect("a response frame is written");
        assert_eq!(
            hdr.status,
            Status::UnknownOpcode as u16,
            "a known service with a bad opcode → UnknownOpcode, not UnknownService"
        );
        assert_eq!(
            hdr.service,
            ServiceId::Entropy as u16,
            "echoes the Entropy service"
        );
        assert_eq!(hdr.opcode, 2, "echoes the bad opcode");
        assert_eq!(hdr.seq, 7);
    }

    /// Comprehensive request-header validation matrix (round-11 P2): each
    /// malformed-header field maps to the right response status in ONE place, so
    /// the whole header is validated at the decode boundary rather than one field
    /// per review round. Header byte layout (`write_header`): magic[0..4],
    /// kind[4..6], service[6..8], opcode[8..10], status[10..12], seq[12..16],
    /// payload_len[16..20], reserved[20..24].
    #[test]
    fn doorbell_request_header_validation_matrix() {
        use environment::{EnvSpec, FaultPolicy};

        // Dispatch a base valid Event(op1) request after `mutate`, returning the
        // decoded response header. Fresh VM per case (dispatch mutates state).
        fn dispatch_header(mutate: impl FnOnce(&mut [u8])) -> hypercall_proto::FrameHeader {
            let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            vmm.enable_sdk(
                EnvSpec::Seeded {
                    seed: 1,
                    policy: FaultPolicy::none(),
                }
                .materialize(),
                &FaultPolicy::none(),
            );
            let mut buf = [0u8; HC_PAGE];
            // Event service, op 1, seq 5, a benign 4-byte event id (ns 0, local 7).
            let n = hypercall_proto::encode_request(
                ServiceId::Event,
                1,
                5,
                &7u32.to_le_bytes(),
                &mut buf,
            )
            .unwrap();
            mutate(&mut buf);
            vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&buf[..n]);
            vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
            let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
            decode(&page).expect("a response frame is always written").0
        }
        let ev = ServiceId::Event as u16;

        // Baseline: a well-formed request is serviced (Ok, echoes Event/op1/seq).
        let h = dispatch_header(|_| {});
        assert_eq!(h.status, Status::Ok as u16, "valid request is serviced");
        assert_eq!((h.service, h.opcode, h.seq), (ev, 1, 5));

        // kind == response (2): not a request → BadRequest, echoes the raw fields.
        let h = dispatch_header(|b| b[4..6].copy_from_slice(&2u16.to_le_bytes()));
        assert_eq!(
            h.status,
            Status::BadRequest as u16,
            "response-typed rejected"
        );
        assert_eq!((h.service, h.seq), (ev, 5), "BadRequest echoes raw fields");

        // Non-zero STATUS on a request (status is response-only) → BadRequest.
        let h = dispatch_header(|b| b[10..12].copy_from_slice(&1u16.to_le_bytes()));
        assert_eq!(
            h.status,
            Status::BadRequest as u16,
            "non-zero-status request rejected (round-11 P2)"
        );
        assert_eq!(h.service, ev);

        // Non-zero RESERVED → `decode` itself rejects (InvalidHeader) → the
        // decode-fail BadRequest path (service/opcode 0, header unparsed).
        let h = dispatch_header(|b| b[20..24].copy_from_slice(&1u32.to_le_bytes()));
        assert_eq!(
            h.status,
            Status::BadRequest as u16,
            "non-zero reserved rejected"
        );

        // Unrecognized kind (3, not request or response) → `decode` rejects →
        // BadRequest.
        let h = dispatch_header(|b| b[4..6].copy_from_slice(&3u16.to_le_bytes()));
        assert_eq!(
            h.status,
            Status::BadRequest as u16,
            "unrecognized message kind rejected"
        );

        // Unknown SERVICE id (no `ServiceId`) → UnknownService, echoing the raw id.
        let h = dispatch_header(|b| b[6..8].copy_from_slice(&0xABCDu16.to_le_bytes()));
        assert_eq!(h.status, Status::UnknownService as u16, "unknown service");
        assert_eq!(h.service, 0xABCD, "echoes the raw service id");

        // Unknown OPCODE on a known service → UnknownOpcode, echoing the service.
        let h = dispatch_header(|b| b[8..10].copy_from_slice(&9u16.to_le_bytes()));
        assert_eq!(h.status, Status::UnknownOpcode as u16, "unknown opcode");
        assert_eq!(
            (h.service, h.opcode),
            (ev, 9),
            "echoes service + bad opcode"
        );
    }

    /// `pending_snapshot` (the deferred `setup_complete` point) is folded into the
    /// state hash (round-8), so a snapshot/restore round-trip MUST preserve it —
    /// else a state sealed with a pending point restores to a DIFFERENT hash (the
    /// point silently lost), diverging on replay (round-9 P1). The flag is toggled
    /// directly (not via the doorbell, whose response write would also dirty guest
    /// RAM and mask the SDK-channel-only difference) so the hash delta is
    /// attributable to `pending_snapshot` alone.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings (task 98 / hm-d8o)"
    )]
    fn sdk_snapshot_round_trips_the_pending_deferred_point_hash() {
        use environment::{EnvSpec, FaultPolicy};
        let spec = EnvSpec::Seeded {
            seed: 7,
            policy: FaultPolicy::none(),
        };
        let mk = || {
            let mut v = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            v.enable_sdk(spec.materialize(), spec.policy());
            v
        };

        // A base whose only mutation is the deferred flag → h_true; a fresh channel
        // (flag `false`) → h_false. Same RAM, same stream, same (empty) events.
        let mut base = mk();
        let h_false = base.state_hash();
        base.sdk.as_mut().unwrap().pending_snapshot = true;
        let h_true = base.state_hash();
        assert_ne!(
            h_false, h_true,
            "pending_snapshot is hash-relevant (round-8 folds it in)"
        );
        let snap = base.sdk_snapshot().expect("a wired channel snapshots");
        assert!(snap.pending_snapshot, "the deferred point is captured");

        // The full verbatim restore carries the flag → reproduces h_true exactly.
        let mut fork = mk();
        fork.sdk_restore(&snap);
        assert_eq!(
            fork.state_hash(),
            h_true,
            "restore round-trips the deferred point → replay hash equality"
        );

        // The branch path (`sdk_restore_events`) deliberately leaves the flag at the
        // fresh `false`, so a reseeded fork does NOT re-surface an already-sealed
        // point — it hashes as h_false, not h_true.
        let mut events_only = mk();
        events_only.sdk_restore_events(&snap);
        assert_eq!(
            events_only.state_hash(),
            h_false,
            "branch restore leaves the deferred flag fresh"
        );
    }

    /// `Vmm::run()` STOPS at a cooperating-SDK assertion — it does not swallow it by
    /// looping on to the later terminal (round-6 P2). A guest rings the doorbell
    /// with an `always` violation, then HLTs: `run` returns `reason == SdkStop` with
    /// the assertion in `sdk_stop`, NOT `reason == Hlt`.
    #[test]
    fn run_stops_on_an_sdk_assertion_not_the_later_terminal() {
        use environment::{EnvSpec, FaultPolicy};
        let viol_id: u32 = (1 << 24) | 20; // assert namespace, point 20
        let mut payload = viol_id.to_le_bytes().to_vec();
        payload.extend_from_slice(&[1, 0, 0]); // [DISP_VIOLATION, detail_len = 0]
        let mut frame = [0u8; HC_PAGE];
        let n =
            hypercall_proto::encode_request(ServiceId::Event, 1, 1, &payload, &mut frame).unwrap();

        let mut vmm = Vmm::new(
            configured_mock(vec![
                Exit::Arch(X86Exit::Io {
                    port: DOORBELL_PORT,
                    size: 4,
                    write: Some(n as u32),
                }),
                Exit::Common(CommonExit::Idle),
            ]),
            GuestRam::new(TEST_RAM).unwrap(),
        );
        vmm.enable_sdk(
            EnvSpec::Seeded {
                seed: 1,
                policy: FaultPolicy::none(),
            }
            .materialize(),
            &FaultPolicy::none(),
        );
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);

        let r = vmm.run().expect("run");
        assert_eq!(
            r.reason,
            TerminalReason::SdkStop,
            "run stops at the assertion, not the HLT that follows"
        );
        assert_eq!(
            r.sdk_stop,
            Some(SdkStop::Assertion {
                id: 20,
                data: vec![]
            })
        );
    }

    /// A `run()` that stops at a **resumable** SDK assertion must NOT cache the
    /// vCPU snapshot (round-5 P2): caching it would make `state_blob` /
    /// `save_vm_state` read the STALE stop-time vCPU after the caller resumes past
    /// the stop. Only a genuine terminal caches. Here: run to the SDK stop, then
    /// model the resumed guest advancing its registers, and assert `state_blob`'s
    /// vCPU reads the live (resumed) state, not the stop's.
    #[test]
    fn run_does_not_cache_the_vcpu_on_a_resumable_sdk_stop() {
        use environment::{EnvSpec, FaultPolicy};
        let viol_id: u32 = (1 << 24) | 20; // assert violation, point 20
        let mut payload = viol_id.to_le_bytes().to_vec();
        payload.extend_from_slice(&[1, 0, 0]); // [DISP_VIOLATION, detail_len = 0]
        let mut frame = [0u8; HC_PAGE];
        let n =
            hypercall_proto::encode_request(ServiceId::Event, 1, 1, &payload, &mut frame).unwrap();

        // The mock reports STOP-time registers `stop_state` when the SDK stop
        // surfaces; the caller then resumes and the guest advances to `resumed_state`.
        let mut stop_state = nonzero_state();
        stop_state.regs.rip = 0x1000;
        let mut resumed_state = nonzero_state();
        resumed_state.regs.rip = 0x2000;

        let mut mock = configured_mock(vec![Exit::Arch(X86Exit::Io {
            port: DOORBELL_PORT,
            size: 4,
            write: Some(n as u32),
        })]);
        mock.set_state(stop_state.clone());
        let mut vmm = Vmm::new(mock, GuestRam::new(TEST_RAM).unwrap());
        vmm.enable_sdk(
            EnvSpec::Seeded {
                seed: 1,
                policy: FaultPolicy::none(),
            }
            .materialize(),
            &FaultPolicy::none(),
        );
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);

        let r = vmm.run().expect("run");
        assert_eq!(r.reason, TerminalReason::SdkStop);
        assert!(
            vmm.saved_state.is_none(),
            "a resumable SDK stop must NOT cache the vCPU (it would go stale on resume)"
        );

        // Model the resume: the guest advanced its registers past the stop.
        vmm.backend.set_state(resumed_state.clone());
        assert_eq!(
            vmm.current_vcpu(),
            resumed_state,
            "state_blob reads the live resumed vCPU, not the stale stop snapshot"
        );
        assert_ne!(vmm.current_vcpu(), stop_state, "not the stop-time vCPU");
    }

    /// Round-5 P1 (semantics, SETTLED): a task-78 reseed marker reseeds ONLY the
    /// entropy stream (`reseed_entropy` → `vt.entropy`), never the buggify/fault
    /// PRNG (`SdkChannel.env`, a separate `RecordedEnv`). So a mid-run reseed cannot
    /// perturb the buggify sequence — the fold (which reseeds entropy only) and the
    /// sequential branch agree. Direct proof: the buggify answers are bit-identical
    /// whether or not the entropy stream is reseeded between decisions — and the
    /// reseed provably DID take effect (distinct reseeds ⇒ distinct RNG draws), so
    /// the invariance is not vacuous.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings (task 98 / hm-d8o)"
    )]
    fn buggify_decisions_are_independent_of_an_entropy_reseed() {
        use environment::{EnvSpec, FaultPolicy};
        let mut policy = FaultPolicy::none();
        policy.set_buggify_point(1, 1, 2).unwrap(); // ~half fire → seed-sensitive
        let spec = EnvSpec::Seeded { seed: 7, policy };

        let build = || {
            let mut v = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            v.wire_vtime(
                VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(0)), 9)
                    .unwrap(),
            );
            v.enable_sdk(spec.materialize(), spec.policy());
            v
        };

        // A: buggify at moments 0..6, no reseed.
        let mut a = build();
        let ans_a: Vec<bool> = (0..6).map(|m| a.decide_buggify(m, 1)).collect();

        // B: buggify at 0..2, reseed the ENTROPY stream to a different seed, 2..6.
        let mut b = build();
        let mut ans_b: Vec<bool> = (0..2).map(|m| b.decide_buggify(m, 1)).collect();
        b.reseed_entropy(0xDEAD_BEEF).unwrap();
        ans_b.extend((2..6).map(|m| b.decide_buggify(m, 1)));

        assert_eq!(
            ans_a, ans_b,
            "buggify answers are invariant under an entropy reseed (buggify ⊥ entropy)"
        );

        // Vacuity guard: distinct entropy reseeds really DO change the entropy-
        // bearing state (the `VTIM` seed/position folded into the hash), so the
        // invariance above is a real independence, not an inert entropy path.
        let mut e1 = build();
        let mut e2 = build();
        e1.reseed_entropy(0xAAAA).unwrap();
        e2.reseed_entropy(0xBBBB).unwrap();
        assert_ne!(
            e1.state_hash(),
            e2.state_hash(),
            "distinct reseeds ⇒ distinct entropy state (the reseed is not a no-op)"
        );
    }

    /// The doorbell is **total** on edge/hostile requests (self-sweep): an empty
    /// request, an oversize length (clamped to one page — never an OOB read), a
    /// garbage frame, and a full-page request all return `Continued` with a clean
    /// (error) response and never a spurious stop. The request page (`0xE000`)
    /// abuts the response page (`0xF000`), so a page-length request reads exactly
    /// its own page and touches neither the response page nor past guest RAM.
    #[test]
    fn doorbell_is_total_on_edge_requests() {
        use environment::{EnvSpec, FaultPolicy};
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        vmm.enable_sdk(
            EnvSpec::Seeded {
                seed: 1,
                policy: FaultPolicy::none(),
            }
            .materialize(),
            &FaultPolicy::none(),
        );

        // Empty request; an oversize length; a full-page request.
        assert_eq!(
            vmm.dispatch_out(DOORBELL_PORT, 4, 0).unwrap(),
            Step::Continued
        );
        // Oversize (> one page) is REJECTED with a clean BadRequest (P2), not
        // clamped: no OOB read, and the response says so.
        assert_eq!(
            vmm.dispatch_out(DOORBELL_PORT, 4, HC_PAGE as u32 + 1)
                .unwrap(),
            Step::Continued
        );
        let resp = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
        let (hdr, _) = decode(&resp).expect("a valid response frame");
        assert_eq!(
            hdr.status,
            Status::BadRequest as u16,
            "an oversize req_len is rejected, not clamped"
        );
        assert_eq!(
            vmm.dispatch_out(DOORBELL_PORT, 4, u32::MAX).unwrap(),
            Step::Continued
        );
        assert_eq!(
            vmm.dispatch_out(DOORBELL_PORT, 4, HC_PAGE as u32).unwrap(),
            Step::Continued
        );

        // A garbage (non-frame) request: decoded as a bad request, never a panic
        // or a stop.
        for (i, b) in vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + 96]
            .iter_mut()
            .enumerate()
        {
            *b = (i as u8).wrapping_mul(37).wrapping_add(1);
        }
        assert_eq!(
            vmm.dispatch_out(DOORBELL_PORT, 4, 96).unwrap(),
            Step::Continued
        );

        assert!(
            vmm.take_sdk_stop().is_none(),
            "no spurious stop from garbage"
        );
        assert!(
            vmm.sdk_events().is_empty(),
            "garbage never captures an event"
        );
    }

    /// `entropy_fill` and RDRAND draw from **one** `SeededEntropy` stream (round-5
    /// P2): interleaving an `entropy_fill(8)` with a guest `RDRAND` yields the SAME
    /// two words as two plain `RDRAND`s from the same seed — i.e. `entropy_fill`
    /// takes stream word 1 and `RDRAND` takes word 2, never a duplicate word 1 from
    /// a second stream.
    #[test]
    fn entropy_fill_and_rdrand_share_one_stream() {
        use environment::{EnvSpec, FaultPolicy};
        // A V-time-wired VM with RAM large enough for the doorbell pages (0xE000).
        let mk = |script: Vec<Exit<X86>>| {
            let mut vmm = Vmm::new(configured_mock(script), GuestRam::new(TEST_RAM).unwrap());
            vmm.wire_vtime(
                VtimeWiring::new(
                    contract_vclock_config(),
                    Box::new(ScriptedWork::at(1)),
                    0x777,
                )
                .unwrap(),
            );
            vmm.enable_sdk(
                EnvSpec::Seeded {
                    seed: 0x777,
                    policy: FaultPolicy::none(),
                }
                .materialize(),
                &FaultPolicy::none(),
            );
            vmm
        };
        // One `entropy_fill(8)` via the doorbell → 8 bytes (one stream word).
        let entropy_fill = |vmm: &mut Vmm<MockBackend>| -> Vec<u8> {
            let mut buf = [0u8; HC_PAGE];
            let len = hypercall_proto::encode_request(
                ServiceId::Entropy,
                1,
                1,
                &8u32.to_le_bytes(),
                &mut buf,
            )
            .unwrap();
            vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + len].copy_from_slice(&buf[..len]);
            vmm.dispatch_out(DOORBELL_PORT, 4, len as u32).unwrap();
            let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
            let (hdr, pl) = decode(&page).expect("a valid response frame");
            assert_eq!(
                hdr.status,
                Status::Ok as u16,
                "entropy is routed via the stream"
            );
            pl.to_vec()
        };
        let reads = |vmm: &Vmm<MockBackend>| -> Vec<u64> {
            vmm.backend
                .completions()
                .iter()
                .map(|c| match c {
                    Completion::Read(v) => *v,
                    other => panic!("expected a Read completion, got {other:?}"),
                })
                .collect()
        };

        // A: entropy_fill (stream word 1), then a guest RDRAND (word 2).
        let mut a = mk(vec![
            Exit::Arch(X86Exit::Rdrand { width: 8 }),
            Exit::Common(CommonExit::Idle),
        ]);
        let word1 = u64::from_le_bytes(entropy_fill(&mut a).try_into().unwrap());
        a.run().unwrap();
        let a_stream = vec![word1, reads(&a)[0]];

        // B (same seed): two plain RDRANDs — the pure stream, words 1 then 2.
        let mut b = mk(vec![
            Exit::Arch(X86Exit::Rdrand { width: 8 }),
            Exit::Arch(X86Exit::Rdrand { width: 8 }),
            Exit::Common(CommonExit::Idle),
        ]);
        b.run().unwrap();

        assert_eq!(
            a_stream,
            reads(&b),
            "entropy_fill + RDRAND is ONE stream (word 1 then word 2)"
        );
        assert_ne!(
            a_stream[0], a_stream[1],
            "consecutive words differ — not two streams from one seed minting a duplicate"
        );
    }

    /// The doorbell routes the **Entropy** service deterministically for a given
    /// seed (finding-4 + round-5 P2): equal seeds ⇒ equal entropy.
    #[test]
    fn doorbell_routes_entropy_deterministically() {
        use environment::{EnvSpec, FaultPolicy};
        let mk = || {
            let mut vmm = Vmm::new(
                configured_mock(vec![Exit::Common(CommonExit::Idle)]),
                GuestRam::new(TEST_RAM).unwrap(),
            );
            vmm.wire_vtime(
                VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(1)), 99)
                    .unwrap(),
            );
            vmm.enable_sdk(
                EnvSpec::Seeded {
                    seed: 99,
                    policy: FaultPolicy::none(),
                }
                .materialize(),
                &FaultPolicy::none(),
            );
            vmm
        };
        let entropy = |vmm: &mut Vmm<MockBackend>, n: u32| -> (u16, Vec<u8>) {
            let mut buf = [0u8; HC_PAGE];
            let len = hypercall_proto::encode_request(
                ServiceId::Entropy,
                1,
                1,
                &n.to_le_bytes(),
                &mut buf,
            )
            .unwrap();
            vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + len].copy_from_slice(&buf[..len]);
            vmm.dispatch_out(DOORBELL_PORT, 4, len as u32).unwrap();
            let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
            let (hdr, pl) = decode(&page).expect("a valid response frame");
            (hdr.status, pl.to_vec())
        };
        let mut a = mk();
        let (status, bytes_a) = entropy(&mut a, 16);
        assert_eq!(status, Status::Ok as u16, "entropy is routed, not rejected");
        assert_eq!(bytes_a.len(), 16);
        let mut b = mk();
        assert_eq!(
            entropy(&mut b, 16).1,
            bytes_a,
            "entropy is deterministic per seed"
        );
    }

    /// The SDK channel snapshot/restore continues the seeded **buggify (fault)**
    /// stream from the captured position (finding-1 fix): a fork resumed at a
    /// snapshot produces the identical buggify continuation, while a fresh channel
    /// (the old reset-on-restore bug) diverges. (Entropy no longer rides the SDK
    /// channel — round-5 P2 routes `entropy_fill` through the VMM `SeededEntropy`
    /// stream, captured by the VM snapshot, not `SdkSnapshot`.)
    #[test]
    fn sdk_snapshot_restore_resumes_the_seeded_streams() {
        use environment::{EnvSpec, FaultPolicy};
        let mut policy = FaultPolicy::none();
        policy.set_buggify_point(1, 1, 2).unwrap();
        let spec = EnvSpec::Seeded { seed: 7, policy };

        let mut base = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        base.enable_sdk(spec.materialize(), spec.policy());
        for i in 0..5 {
            let _ = base.decide_buggify(i, 1);
        }
        let snap = base.sdk_snapshot().expect("a wired channel snapshots");

        // The buggify continuation from the snapshot position.
        let cont = |vmm: &mut Vmm<MockBackend>| -> Vec<bool> {
            (5..10).map(|i| vmm.decide_buggify(i, 1)).collect()
        };
        let expected = cont(&mut base);

        // A fresh channel RESTORED to the snapshot reproduces the continuation.
        let mut fork = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        fork.enable_sdk(spec.materialize(), spec.policy());
        fork.sdk_restore(&snap);
        assert_eq!(
            cont(&mut fork),
            expected,
            "restored fault stream resumes exactly"
        );

        // A fresh channel WITHOUT restore (the old bug) diverges.
        let mut broken = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        broken.enable_sdk(spec.materialize(), spec.policy());
        assert_ne!(
            cont(&mut broken),
            expected,
            "a fresh (position-0) channel is NOT the mid-run continuation"
        );
    }

    /// Task 61: a mid-net-decisions snapshot, restored, reproduces the `net_decide`
    /// continuation BIT-IDENTICALLY. Under the single-stream ruling the flow-policy
    /// stream position rides the **shared SDK stream** (restored by `sdk_restore`),
    /// and the decision log rides `net_restore`; a fresh (position-0) channel
    /// diverges. Uses a multi-fault NetFlow policy so the sampled fault VALUE varies
    /// with the stream position (else divergence could not be witnessed).
    #[test]
    fn net_continuation_resumes_via_the_shared_sdk_stream() {
        use environment::{DecisionClass, EnvSpec, Fault, FaultPolicy, Span};
        let mut policy = FaultPolicy::none();
        policy
            .set_class(
                DecisionClass::NetFlow,
                1,
                1,
                &[
                    Fault::NetReset,
                    Fault::NetLatency(Span(10)),
                    Fault::NetThrottle { bps: 5 },
                ],
            )
            .unwrap();
        let spec = EnvSpec::Seeded { seed: 7, policy };

        // Wire BOTH channels — a net decision draws from the shared SDK stream.
        let wire = |spec: &EnvSpec| -> Vmm<MockBackend> {
            let mut v = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            v.enable_sdk(spec.materialize(), spec.policy());
            v.enable_net();
            v
        };

        let mut base = wire(&spec);
        for i in 0..5 {
            let _ = base.decide_net(i, 1, 2, i, 0);
        }
        // Capture the shared stream (SDK) + the net decision log.
        let sdk_snap = base.sdk_snapshot().expect("a wired SDK channel snapshots");
        let net_snap = base.net_snapshot().expect("a wired Net channel snapshots");

        let cont = |vmm: &mut Vmm<MockBackend>| -> Vec<Vec<u8>> {
            (5..10).map(|i| vmm.decide_net(i, 1, 2, i, 0)).collect()
        };
        let expected = cont(&mut base);

        // RESTORED (shared stream via sdk_restore + decision log via net_restore) →
        // reproduces the continuation bit-identically.
        let mut fork = wire(&spec);
        fork.sdk_restore(&sdk_snap);
        fork.net_restore(&net_snap);
        assert_eq!(
            cont(&mut fork),
            expected,
            "restored shared stream resumes the net continuation exactly"
        );
        assert_eq!(
            fork.net_decisions().len(),
            10,
            "the decision prefix carried over"
        );

        // WITHOUT restore (position 0) → diverges.
        let mut broken = wire(&spec);
        assert_ne!(
            cont(&mut broken),
            expected,
            "a fresh (position-0) shared stream is NOT the mid-run continuation"
        );
    }

    /// Task 61 (R3, the single-stream contract): a `net_decide` draw **advances the
    /// one shared fault stream** that buggify also draws from, so a buggify answer
    /// that follows a net decision matches the canonical one-stream reproducer (net
    /// then buggify from a single `RecordedEnv`), and DIFFERS from a buggify with no
    /// preceding net draw. Under the (fixed) two-stream bug, the net draw would not
    /// shift the buggify sequence and buggify-after-net would equal buggify-first.
    #[test]
    fn a_net_draw_advances_the_shared_stream_seen_by_buggify() {
        use environment::{
            Answer, DecisionClass, DecisionPoint, EnvSpec, Environment, Fault, FaultPolicy,
        };

        // Compute, purely in the environment crate, the canonical one-stream
        // buggify answer with vs. without a preceding net draw for a seed.
        let net_point = DecisionPoint::NetFlow {
            src: environment::NodeId(1),
            dst: environment::NodeId(2),
            conn: environment::ConnId(7),
            event: environment::FlowEvent::Open,
        };
        let fires = |ans: environment::Outcome| {
            matches!(
                ans,
                environment::Outcome::Resolved(Answer::Fault(Fault::BuggifyFire))
            )
        };
        let make_spec = |seed: u64| {
            let mut policy = FaultPolicy::none();
            policy
                .set_class(DecisionClass::NetFlow, 1, 1, &[Fault::NetReset])
                .unwrap();
            policy.set_buggify_point(1, 1, 2).unwrap();
            EnvSpec::Seeded { seed, policy }
        };
        // Pick a seed where the two stream positions give DIFFERENT buggify
        // outcomes, so the test genuinely witnesses the stream advance (a fixed
        // constant could hit a parity collision where word 0 and word 1 agree).
        let (spec, ref_net, ref_bug_after_net, bug_first) = (0u64..64)
            .find_map(|seed| {
                let spec = make_spec(seed);
                let mut e1 = spec.materialize();
                let net = e1.decide(&net_point);
                let bug_after = fires(e1.decide(&DecisionPoint::Buggify { point: 1 }));
                let mut e2 = spec.materialize();
                let bug_first = fires(e2.decide(&DecisionPoint::Buggify { point: 1 }));
                (bug_after != bug_first).then(|| {
                    let net = match net {
                        environment::Outcome::Resolved(a) => a,
                        _ => Answer::Nominal,
                    };
                    (spec, net, bug_after, bug_first)
                })
            })
            .expect("a seed where the net draw shifts the buggify outcome exists");

        // The VMM: net_decide then decide_buggify share ONE stream, so the buggify
        // answer matches the canonical net-then-buggify reference (the net draw
        // shifted the stream) and NOT the buggify-first reference.
        let mut v = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        v.enable_sdk(spec.materialize(), spec.policy());
        v.enable_net();
        let net_bytes = v.decide_net(0, 1, 2, 7, 0);
        assert_eq!(
            Answer::decode(&net_bytes).unwrap(),
            ref_net,
            "net answer matches the canonical stream position 0"
        );
        let fired = v.decide_buggify(1, 1);
        assert_eq!(
            fired, ref_bug_after_net,
            "buggify-after-net matches the canonical one-stream reproducer \
             (the net draw advanced the shared stream)"
        );
        assert_ne!(
            fired, bug_first,
            "buggify-after-net differs from buggify-first — the net draw genuinely \
             advanced the shared stream (would be equal under the two-stream bug)"
        );
    }

    /// The `state_hash` folds the wired SDK channel's replay-relevant state
    /// (round-7): two same-seed VMs whose SDK buggify streams diverge hash
    /// **differently**; and a VM with NO SDK channel carries no `SDK\0` chunk, so
    /// an SDK-less golden (M1/M2/corpus/Linux) is byte-for-byte unchanged.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings (task 98 / hm-d8o)"
    )]
    fn state_hash_folds_the_sdk_stream_and_is_absent_when_unwired() {
        use environment::{EnvSpec, FaultPolicy};
        let mut policy = FaultPolicy::none();
        policy.set_buggify_point(1, 1, 2).unwrap();
        let spec = EnvSpec::Seeded { seed: 7, policy };
        let mk = || Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());

        // Same seed + same stream position ⇒ equal hash; a diverged buggify draw
        // sequence ⇒ DIFFERENT hash (the SDK divergence is IN the determinism hash).
        let mut a = mk();
        a.enable_sdk(spec.materialize(), spec.policy());
        let mut b = mk();
        b.enable_sdk(spec.materialize(), spec.policy());
        assert_eq!(
            a.state_hash(),
            b.state_hash(),
            "same SDK stream position hashes equal"
        );
        for i in 0..3 {
            let _ = b.decide_buggify(i, 1);
        }
        assert_ne!(
            a.state_hash(),
            b.state_hash(),
            "a diverged SDK stream hashes differently"
        );

        // No SDK channel ⇒ no `SDK\0` chunk in the blob (the golden does not move).
        let has_sdk_chunk = |blob: &[u8]| blob.windows(4).any(|w| w == b"SDK\0");
        assert!(
            !has_sdk_chunk(&mk().state_blob()),
            "no SDK chunk when unwired"
        );
        let mut wired = mk();
        wired.enable_sdk(spec.materialize(), spec.policy());
        assert!(
            has_sdk_chunk(&wired.state_blob()),
            "SDK chunk present when wired"
        );
    }

    /// The `state_hash` folds the **active FaultPolicy** (round-8 P1): two same-seed
    /// VMs at the SAME (position-0) stream but with DIFFERENT buggify policies hash
    /// **differently** — a stream position alone does not determine the buggify
    /// fire/nominal sequence, the policy does, so the divergence must be in the hash.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings (task 98 / hm-d8o)"
    )]
    fn state_hash_folds_the_active_buggify_policy() {
        use environment::{EnvSpec, FaultPolicy};
        let mk = |policy: FaultPolicy| {
            let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            let spec = EnvSpec::Seeded { seed: 7, policy };
            vmm.enable_sdk(spec.materialize(), spec.policy());
            vmm
        };
        // Two policies differing ONLY in the buggify biasing at the same point;
        // both channels are at stream position 0, so the seed + stream match.
        let mut p_half = FaultPolicy::none();
        p_half.set_buggify_point(1, 1, 2).unwrap(); // fire 1/2
        let mut p_three_quarters = FaultPolicy::none();
        p_three_quarters.set_buggify_point(1, 3, 4).unwrap(); // fire 3/4 — different policy
        assert_ne!(
            mk(p_half.clone()).state_hash(),
            mk(p_three_quarters).state_hash(),
            "a different active buggify policy hashes differently"
        );
        // The SAME policy at the same stream still hashes equal (sanity).
        assert_eq!(
            mk(p_half.clone()).state_hash(),
            mk(p_half).state_hash(),
            "the same policy at the same stream hashes equal"
        );
    }

    #[test]
    fn rdtsc_completes_with_vtime_tsc_not_host() {
        // work = 10 → vns = 10 (ratio 1:1) → tsc = floor(10 * 2GHz/1e9) = 20.
        let mut vmm = vtime_vmm(
            vec![Exit::Arch(X86Exit::Rdtsc), Exit::Common(CommonExit::Idle)],
            Box::new(ScriptedWork::at(10)),
            1,
        );
        assert!(vmm.vtime_wired(), "wire_vtime reports the path as wired");
        let r = vmm.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Idle);
        assert_eq!(vmm.backend.completions(), &[Completion::Read(20)]);
    }

    /// A `WorkSource` recording `start_run` invocations, to pin that the work-counter
    /// prepare fires **exactly once, at the first guest entry** — not skipped for a
    /// `step()`-then-`run()` consumer, and not re-fired mid-run.
    struct CountingStartWork {
        starts: std::rc::Rc<Cell<u32>>,
    }
    impl WorkSource for CountingStartWork {
        fn work(&self) -> Result<u64, WorkError> {
            Ok(0)
        }
        fn reset(&mut self) -> Result<(), WorkError> {
            Ok(())
        }
        fn start_run(&mut self) -> Result<(), WorkError> {
            self.starts.set(self.starts.get() + 1);
            Ok(())
        }
    }

    #[test]
    fn start_run_fires_once_at_first_guest_entry_via_step_or_run() {
        // A telemetry/diagnostic consumer drives via step() first, then run(): the
        // work-counter prepare must fire on the FIRST step()'s guest entry (not be
        // skipped), and never again at run() (no mid-run restart). This is the P2-1
        // fix — gating on the actual first guest entry, shared by step()/run(), not
        // the top of run().
        let starts = std::rc::Rc::new(Cell::new(0u32));
        let mut vmm = Vmm::new(
            configured_mock(vec![
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Common(CommonExit::Idle),
            ]),
            GuestRam::new(0x1000).unwrap(),
        );
        vmm.wire_vtime(
            VtimeWiring::new(
                contract_vclock_config(),
                Box::new(CountingStartWork {
                    starts: starts.clone(),
                }),
                1,
            )
            .unwrap(),
        );
        assert_eq!(starts.get(), 0, "not prepared before any guest entry");
        vmm.step().expect("step 1"); // first guest entry → prepare fires
        assert_eq!(
            starts.get(),
            1,
            "prepared on the first step()'s guest entry"
        );
        vmm.step().expect("step 2"); // second entry → must NOT re-fire
        let r = vmm.run().expect("run to terminal"); // run() must NOT re-fire either
        assert_eq!(r.reason, TerminalReason::Idle);
        assert_eq!(
            starts.get(),
            1,
            "start_run fires exactly once, never restarting work mid-run"
        );
    }

    #[test]
    fn rdtscp_completes_with_vtime_tsc() {
        // RDTSCP is resolved identically above the trait (the backend supplies
        // ECX=IA32_TSC_AUX below it); the VMM still completes the V-time value.
        let mut vmm = vtime_vmm(
            vec![Exit::Arch(X86Exit::Rdtscp), Exit::Common(CommonExit::Idle)],
            Box::new(ScriptedWork::at(7)),
            1,
        );
        vmm.run().expect("run");
        assert_eq!(vmm.backend.completions(), &[Completion::Read(14)]);
    }

    #[test]
    fn rdtsc_is_strictly_monotonic_when_work_advances() {
        // Three reads, one "branch" between each (step=3): work 0,3,6 → tsc 0,6,12.
        let mut vmm = vtime_vmm(
            vec![
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Common(CommonExit::Idle),
            ],
            Box::new(AutoWork {
                next: Cell::new(0),
                step: 3,
            }),
            1,
        );
        vmm.run().expect("run");
        let reads: Vec<u64> = vmm
            .backend
            .completions()
            .iter()
            .map(|c| match c {
                Completion::Read(v) => *v,
                other => panic!("unexpected completion {other:?}"),
            })
            .collect();
        assert_eq!(reads, vec![0, 6, 12]);
        assert!(reads.windows(2).all(|w| w[1] > w[0]), "strictly monotonic");
    }

    #[test]
    fn rdrand_rdseed_draw_from_the_seeded_stream() {
        const SEED: u64 = 0xABCD_1234;
        let mut vmm = vtime_vmm(
            vec![
                Exit::Arch(X86Exit::Rdrand { width: 8 }),
                Exit::Arch(X86Exit::Rdseed { width: 4 }),
                Exit::Common(CommonExit::Idle),
            ],
            Box::new(ScriptedWork::new()),
            SEED,
        );
        vmm.run().expect("run");
        // The two draws are consecutive words of the same xorshift64* stream the
        // Entropy hypercall uses — recomputed here from a fresh SeededEntropy.
        let mut e = SeededEntropy::new(SEED);
        let mut b8 = [0u8; 8];
        assert_eq!(e.handle(1, &8u32.to_le_bytes(), &mut b8), (Status::Ok, 8));
        let mut b4 = [0u8; 8];
        assert_eq!(
            e.handle(1, &4u32.to_le_bytes(), &mut b4[..4]),
            (Status::Ok, 4)
        );
        assert_eq!(
            vmm.backend.completions(),
            &[
                Completion::Read(u64::from_le_bytes(b8)),
                Completion::Read(u64::from_le_bytes(b4)),
            ]
        );
    }

    #[test]
    fn unwired_rdtsc_and_rdrand_fail_closed() {
        // Stock-style Vmm (no wire_vtime): the four exits must NOT be serviced
        // with a host value — they are loud ContractViolations.
        let mut tsc = Vmm::new(
            configured_mock(vec![Exit::Arch(X86Exit::Rdtsc)]),
            GuestRam::new(0x1000).unwrap(),
        );
        assert!(matches!(tsc.step(), Err(VmmError::ContractViolation(_))));
        let mut rng = Vmm::new(
            configured_mock(vec![Exit::Arch(X86Exit::Rdrand { width: 8 })]),
            GuestRam::new(0x1000).unwrap(),
        );
        assert!(matches!(rng.step(), Err(VmmError::ContractViolation(_))));
    }

    #[test]
    fn snapshot_restore_continues_the_clock_and_rng_exactly() {
        const SEED: u64 = 0x5151_5151;
        // A: draw one RNG word, then step to a CLEAN boundary before snapshotting.
        // The RDRAND step stages an RNG completion (unsafe boundary); the following
        // RDTSC step's re-entry commits it, so `save_vtime` is then valid. (Without
        // the trailing RDTSC, `save_vtime` would fail closed — see
        // `save_vtime_fails_closed_at_rng_mid_exit_boundary`.)
        let mut a = vtime_vmm(
            vec![
                Exit::Arch(X86Exit::Rdrand { width: 8 }),
                Exit::Arch(X86Exit::Rdtsc),
            ],
            Box::new(ScriptedWork::at(50)),
            SEED,
        );
        assert_eq!(a.step().unwrap(), Step::Continued); // RDRAND → first word (staged)
        assert_eq!(a.step().unwrap(), Step::Continued); // RDTSC → commits RDRAND; tsc=100
        let snap = a
            .save_vtime()
            .expect("save at clean boundary")
            .expect("wired");
        assert_eq!(snap.vns, 50); // ratio 1:1 → vns == work

        // Restore into B whose counter sits at a NON-zero 99: restore_vtime must
        // RESET it to 0 (else RDTSC would read work=99 → tsc=298, not 100), set
        // vns_base=50, and resume the RNG stream at the *next* word — not the
        // first. (B starting non-zero is what makes the counter-reset observable.)
        let mut b = vtime_vmm(
            vec![
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Arch(X86Exit::Rdrand { width: 8 }),
            ],
            Box::new(ScriptedWork::at(99)),
            SEED, // a different seed would be overwritten by restore anyway
        );
        b.restore_vtime(&snap).expect("restore");
        b.step().unwrap(); // RDTSC at reset work=0 → tsc(0) = 2*vns_base = 100
        b.step().unwrap(); // RDRAND → the word AFTER A's first draw

        // Clock continuity: B's first post-restore TSC equals A's TSC at the
        // snapshot point (100), even though B's counter restarted at 0.
        assert_eq!(b.backend.completions()[0], Completion::Read(100));
        // RNG continuity: A drew the first word; B (restored) draws the *second* —
        // the stream resumed, it was not replayed.
        let mut ref_stream = SeededEntropy::new(SEED);
        let mut w0 = [0u8; 8];
        let mut w1 = [0u8; 8];
        ref_stream.handle(1, &8u32.to_le_bytes(), &mut w0);
        ref_stream.handle(1, &8u32.to_le_bytes(), &mut w1);
        assert_eq!(
            a.backend.completions()[0],
            Completion::Read(u64::from_le_bytes(w0))
        );
        assert_eq!(
            b.backend.completions()[1],
            Completion::Read(u64::from_le_bytes(w1))
        );
    }

    /// Reviewer round-2 fix (1): `save_vtime` fails closed at an RNG mid-exit
    /// boundary (the seeded draw advanced but its completion is only staged), and
    /// becomes valid again after the next step commits it.
    #[test]
    fn save_vtime_fails_closed_at_rng_mid_exit_boundary() {
        let mut v = vtime_vmm(
            vec![
                Exit::Arch(X86Exit::Rdrand { width: 8 }),
                Exit::Arch(X86Exit::Rdtsc),
            ],
            Box::new(ScriptedWork::at(10)),
            0xABCD,
        );
        v.step().unwrap(); // RDRAND → RNG completion staged (unsafe boundary)
        assert!(
            matches!(v.save_vtime(), Err(VmmError::ContractViolation(_))),
            "save_vtime must refuse while an RNG completion is staged"
        );
        v.step().unwrap(); // RDTSC → re-entry commits the RDRAND; boundary now clean
        assert!(
            v.save_vtime().is_ok(),
            "save_vtime must succeed once the RNG completion is committed"
        );
    }

    /// Reviewer round-2 fix (2): `VtimeWiring::new` fails closed on a fractional
    /// work→ns ratio (`ratio_den != 1`), whose sub-ns remainder a snapshot would
    /// lose. The exact contract ratio (`ratio_den == 1`) is accepted.
    #[test]
    fn vtime_wiring_rejects_fractional_ratio() {
        let frac = vtime::VClockConfig {
            ratio_den: 2,
            ..contract_vclock_config()
        };
        assert!(matches!(
            VtimeWiring::new(frac, Box::new(ScriptedWork::new()), 1),
            Err(VmmError::ContractViolation(_))
        ));
        // The exact contract clock is accepted.
        assert!(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::new()), 1).is_ok()
        );
    }

    #[test]
    fn save_vtime_is_none_when_unwired() {
        let v = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert!(v.save_vtime().unwrap().is_none());
        assert!(!v.vtime_wired());
    }

    /// Task-27 (box-verification cross-model finding): `save_vtime` anchors `vns` to
    /// the deterministic `last_intercept_work`, **not** a live counter read. A fresh VM
    /// is a synchronized point (work 0, V-time = `vns_base`), so the save succeeds; the
    /// source reads `777` but the anchor is `0`, so `vns` must be `0` — a live read
    /// would capture the skid-prone `777` (the terminal-read bug removed from the hash).
    #[test]
    fn save_vtime_anchors_vns_to_last_intercept_not_live_work() {
        let v = vtime_vmm(vec![], Box::new(ScriptedWork::at(777)), 1);
        let snap = v.save_vtime().expect("save").expect("wired");
        assert_eq!(
            snap.vns, 0,
            "vns must anchor to last_intercept_work (0), not the live counter (777)"
        );
    }

    /// Task-27 (integrator ruling): a snapshot's `vns` must be the EXACT V-time, which
    /// is known only at a V-time intercept. At a non-V-time exit (here a UART OUT after
    /// an RDTSC) the post-intercept work is skid — not deterministically measurable —
    /// so `save_vtime` must **fail closed** rather than record a stale `vns` (a
    /// silently-wrong restore, §4). It succeeds again at the next V-time intercept.
    #[test]
    fn save_vtime_fails_closed_at_non_intercept_exit() {
        let mut v = vtime_vmm(
            vec![
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Arch(X86Exit::Io {
                    port: 0x3F8, // UART THR — a non-V-time exit (Continued)
                    size: 1,
                    write: Some(u32::from(b'x')),
                }),
                Exit::Arch(X86Exit::Rdtsc),
            ],
            Box::new(ScriptedWork::at(10)),
            1,
        );
        v.step().unwrap(); // RDTSC → V-time intercept (synchronized)
        assert!(
            v.save_vtime().is_ok(),
            "snapshot at a V-time intercept is exact"
        );
        v.step().unwrap(); // UART OUT → non-V-time exit (desynchronized)
        assert!(
            matches!(v.save_vtime(), Err(VmmError::ContractViolation(_))),
            "snapshot at a non-V-time exit must fail closed (V-time not exactly measurable)"
        );
        v.step().unwrap(); // RDTSC → re-synchronized
        assert!(
            v.save_vtime().is_ok(),
            "snapshot is exact again at the next V-time intercept"
        );
    }

    /// Task-27 (integrator-ruling cross-model finding): the `vtime_synchronized` flag
    /// is cleared **before** `backend.run()`, so a step whose `run()` errors leaves the
    /// VM **desynchronized** — `save_vtime` then fails closed instead of emitting a
    /// stale anchor from the prior intercept. (With a clear-after-run, the `?` would
    /// skip the clear and leave a stale-synchronized state.)
    #[test]
    fn run_error_leaves_vtime_desynchronized() {
        let mut v = vtime_vmm(
            vec![Exit::Arch(X86Exit::Rdtsc)],
            Box::new(ScriptedWork::at(10)),
            1,
        );
        v.step().unwrap(); // RDTSC → synchronized
        assert!(v.save_vtime().is_ok(), "synchronized after the intercept");
        // Next step: the mock's run-queue is empty → backend.run() errors.
        assert!(v.step().is_err(), "run() must error on the empty queue");
        assert!(
            matches!(v.save_vtime(), Err(VmmError::ContractViolation(_))),
            "a failed run() must leave the VM desynchronized (no stale-synchronized save)"
        );
    }

    /// Reviewer fix (3): `restore_vtime` is atomic — a snapshot with an invalid
    /// entropy blob is rejected with the timeline **fully intact** (clock,
    /// `vns_base`, work, and entropy all unchanged), never half-restored.
    #[test]
    fn restore_vtime_rejects_bad_snapshot_atomically() {
        let mut v = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        v.wire_vtime(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(7)), 1).unwrap(),
        );
        let before = v.state_hash();
        // `SeededEntropy::restore_state` rejects an all-zero (value 0) blob. With a
        // non-atomic restore the clock/vns_base/work would already be mutated; the
        // atomic version leaves everything as-is.
        let bad = VtimeSnapshot {
            vns: 9_999,
            guest_clock_offset: 0,
            entropy: vec![0u8; 8],
        };
        assert!(matches!(
            v.restore_vtime(&bad),
            Err(VmmError::ContractViolation(_))
        ));
        assert_eq!(
            v.state_hash(),
            before,
            "a rejected snapshot must leave the V-time/entropy state untouched"
        );
    }

    /// P2 round-11: `restore_vtime` is atomic even when the FALLIBLE backend step fails.
    /// Round-10 placed the backend save/restore round-trip (the counter-B re-arm) AFTER
    /// the work-counter reset + clock/entropy/first_entry commit, so a backend failure
    /// left V-time HALF-restored. The round-trip now runs FIRST (before any V-time
    /// mutation); a failing `Backend::save` must abort with NOTHING changed. The snapshot
    /// is deliberately state-CHANGING (`vns` shifted), so a non-atomic restore would move
    /// `vns_base` and change the hash — proving the unchanged hash is not vacuous.
    #[test]
    fn restore_vtime_atomic_on_backend_failure() {
        let mut v = Vmm::new(
            SaveFailBackend(configured_mock(vec![])),
            GuestRam::new(0x1000).unwrap(),
        );
        v.wire_vtime(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(7)), 1).unwrap(),
        );
        // A VALID snapshot (so validation passes and the failure is at the backend step),
        // but with a SHIFTED vns so a successful restore WOULD change the hash.
        let snap0 = v.save_vtime().expect("clean save").expect("V-time wired");
        let snap = VtimeSnapshot {
            vns: snap0.vns + 4_096,
            guest_clock_offset: snap0.guest_clock_offset,
            entropy: snap0.entropy.clone(),
        };
        let before = v.state_hash();
        assert!(
            matches!(v.restore_vtime(&snap), Err(VmmError::Backend(_))),
            "a failing Backend::save during the round-trip must make restore_vtime fail closed"
        );
        assert_eq!(
            v.state_hash(),
            before,
            "backend failure must leave the V-time state untouched (atomic): the work counter, \
             clock/vns_base, entropy, and tsc_adjust are all unchanged"
        );
    }

    /// Task-27 item 3 (revised per box-verification cross-model finding 2):
    /// `restore_vtime` is **symmetric with `save_vtime`** — it fails closed at an RNG
    /// mid-exit boundary (rewinding entropy while a backend RDRAND/RDSEED completion is
    /// staged would shift the next draw), and does **not** clear the flag (that would
    /// falsely declare the backend clean). At a **clean** boundary a restore-then-save
    /// succeeds (the flag is already clear — item 3's actual requirement); the flag is
    /// cleared only by the next `step`'s commit.
    #[test]
    fn restore_vtime_fails_closed_at_rng_mid_exit_boundary() {
        const SEED: u64 = 0x99;
        let mut v = vtime_vmm(
            vec![
                Exit::Arch(X86Exit::Rdrand { width: 8 }),
                Exit::Arch(X86Exit::Rdtsc),
            ],
            Box::new(ScriptedWork::at(5)),
            SEED,
        );
        // Clean snapshot first (nothing stepped yet → boundary is clean).
        let snap = v.save_vtime().expect("clean save").expect("wired");
        // Step the RDRAND → an RNG completion is staged (the unsafe boundary).
        v.step().unwrap();
        assert!(
            matches!(v.restore_vtime(&snap), Err(VmmError::ContractViolation(_))),
            "restore_vtime must fail closed while an RNG completion is staged"
        );
        // The next step's re-entry commits the RDRAND → boundary clean again.
        v.step().unwrap(); // RDTSC
        // At a clean boundary restore succeeds, and a restore-then-save succeeds
        // (item 3: no spurious ContractViolation at a clean boundary).
        v.restore_vtime(&snap).expect("restore at clean boundary");
        assert!(
            v.save_vtime().is_ok(),
            "restore-then-save at a clean boundary must succeed"
        );
    }

    /// Task-27 item 1: a guest reading `IA32_TSC` via `RDMSR(0x10)` gets the **same**
    /// V-time value the RDTSC instruction would at the same work — both flow through
    /// `guest_clock` (`VClock::guest_ticks` + the default-0 `IA32_TSC_ADJUST`) — and it is
    /// deterministic-twice. (Previously this aborted with a stale "V-time is not
    /// wired in this skeleton" `ContractViolation`.)
    #[test]
    fn rdmsr_ia32_tsc_matches_rdtsc_instruction_and_is_deterministic() {
        const WORK: u64 = 21; // vns(21)=21 → tsc = floor(21·2GHz/1e9) = 42.
        let run_msr = || {
            let mut v = vtime_vmm(
                vec![
                    Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                    Exit::Common(CommonExit::Idle),
                ],
                Box::new(ScriptedWork::at(WORK)),
                1,
            );
            v.run().unwrap();
            v
        };
        let mut insn = vtime_vmm(
            vec![Exit::Arch(X86Exit::Rdtsc), Exit::Common(CommonExit::Idle)],
            Box::new(ScriptedWork::at(WORK)),
            1,
        );
        insn.run().unwrap();

        let msr = run_msr();
        assert_eq!(
            msr.backend.completions(),
            insn.backend.completions(),
            "RDMSR(IA32_TSC) must read the same V-time TSC as the RDTSC instruction"
        );
        assert_eq!(msr.backend.completions(), &[Completion::Read(42)]);
        // Deterministic-twice (same seed/work ⇒ byte-identical state_hash).
        assert_eq!(msr.state_hash(), run_msr().state_hash());
    }

    /// Task-27 item 1, write side: `WRMSR(IA32_TSC_ADJUST, Y)` sets the adjust (and
    /// shifts the visible TSC by `Y`); `WRMSR(IA32_TSC, X)` sets the visible TSC to
    /// `X` (and the adjust to `X − base`). `RDMSR` of both reflects it. Both writes
    /// are honored (`Completion::Ok`).
    #[test]
    fn emulate_vtime_tsc_msr_write_paths() {
        // ScriptedWork fixed at work=10 → base V-time TSC = VClock::guest_ticks(10) = 20.
        let mut vmm = vtime_vmm(
            vec![
                Exit::Arch(X86Exit::Wrmsr {
                    index: 0x3b,
                    value: 1000,
                }), // IA32_TSC_ADJUST = 1000
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }), // IA32_TSC = 20 + 1000 = 1020
                Exit::Arch(X86Exit::Rdmsr { index: 0x3b }), // IA32_TSC_ADJUST = 1000
                Exit::Arch(X86Exit::Wrmsr {
                    index: 0x10,
                    value: 7777,
                }), // IA32_TSC = 7777 → adjust = 7777 − 20 = 7757
                Exit::Arch(X86Exit::Rdmsr { index: 0x10 }), // IA32_TSC = 7777
                Exit::Arch(X86Exit::Rdmsr { index: 0x3b }), // IA32_TSC_ADJUST = 7757
                Exit::Common(CommonExit::Idle),
            ],
            Box::new(ScriptedWork::at(10)),
            1,
        );
        vmm.run().unwrap();
        assert_eq!(
            vmm.backend.completions(),
            &[
                Completion::Ok, // WRMSR(IA32_TSC_ADJUST, 1000)
                Completion::Read(1020),
                Completion::Read(1000),
                Completion::Ok, // WRMSR(IA32_TSC, 7777)
                Completion::Read(7777),
                Completion::Read(7757),
            ]
        );
    }

    /// Task-27 item 1: the written `IA32_TSC_ADJUST` state is in the hash (it governs
    /// future TSC output) — two VMs identical but for the adjust hash differently.
    #[test]
    fn tsc_adjust_state_is_in_the_hash() {
        let with_adjust = |adjust: u64| {
            let mut v = vtime_vmm(
                vec![Exit::Arch(X86Exit::Wrmsr {
                    index: 0x3b,
                    value: adjust,
                })],
                Box::new(ScriptedWork::at(0)),
                1,
            );
            v.step().unwrap();
            v
        };
        assert_ne!(
            with_adjust(0).state_hash(),
            with_adjust(12_345).state_hash(),
            "a written IA32_TSC_ADJUST must change the VTIM hash"
        );
    }

    /// Task-27 item 1 (cross-model review finding 1): an `IA32_TSC_ADJUST` access is a
    /// V-time intercept too, so it records its deterministic work and the hashed
    /// effective V-time stays current — two VMs accessing 0x3b at different work hash
    /// differently (without the fix both would keep the stale anchor `0` and collide).
    #[test]
    fn tsc_adjust_access_records_work_in_the_hash() {
        let at_work = |work: u64| {
            let mut v = vtime_vmm(
                vec![Exit::Arch(X86Exit::Rdmsr { index: 0x3b })],
                Box::new(ScriptedWork::at(work)),
                1,
            );
            v.step().unwrap(); // RDMSR(IA32_TSC_ADJUST) records last_intercept_work
            v
        };
        assert_ne!(
            at_work(100).state_hash(),
            at_work(200).state_hash(),
            "a 0x3b access at different work ⇒ different effective V-time ⇒ different hash"
        );
    }

    /// Task-27 item 1 (revised per box-verification cross-model finding 3):
    /// `IA32_TSC_ADJUST` round-trips through a V-time snapshot — `save_vtime` captures
    /// it (the contract carries TSC/TSC_ADJUST in `vm_state`) and `restore_vtime`
    /// re-applies it, so a guest that wrote the MSR is snapshottable and restores
    /// faithfully (no fail-closed, no silent loss).
    #[test]
    fn vtime_snapshot_round_trips_tsc_adjust() {
        let mut v = vtime_vmm(
            vec![
                Exit::Arch(X86Exit::Wrmsr {
                    index: 0x3b,
                    value: 9,
                }), // tsc_adjust = 9
                Exit::Arch(X86Exit::Wrmsr {
                    index: 0x3b,
                    value: 99,
                }), // tsc_adjust = 99
                Exit::Arch(X86Exit::Rdmsr { index: 0x3b }), // reads back the restored adjust
            ],
            Box::new(ScriptedWork::at(0)),
            1,
        );
        v.step().unwrap(); // WRMSR(0x3b, 9) → tsc_adjust = 9
        let snap = v
            .save_vtime()
            .expect("save with non-zero adjust succeeds")
            .expect("wired");
        assert_eq!(
            snap.guest_clock_offset, 9,
            "snapshot must capture IA32_TSC_ADJUST"
        );
        v.step().unwrap(); // WRMSR(0x3b, 99) → tsc_adjust = 99 (diverge)
        v.restore_vtime(&snap).expect("restore");
        v.step().unwrap(); // RDMSR(0x3b) → must read the restored 9
        assert_eq!(
            v.backend.completions().last(),
            Some(&Completion::Read(9)),
            "restore must re-apply the snapshotted IA32_TSC_ADJUST"
        );
    }

    /// Task-27 item 1: with V-time **unwired** (stock KVM / M1/M2), an `emulate-vtime`
    /// TSC-MSR access still fails closed in both directions — never a laundered host
    /// value. (Mirrors `event_loop::emulate_vtime_msr_fails_closed_both_directions`
    /// for the wiring boundary inside `vmm.rs`.)
    #[test]
    fn emulate_vtime_tsc_msr_unwired_fails_closed() {
        for idx in [0x10u32, 0x3b] {
            let mut rd = Vmm::new(
                configured_mock(vec![Exit::Arch(X86Exit::Rdmsr { index: idx })]),
                GuestRam::new(0x1000).unwrap(),
            );
            assert!(matches!(rd.step(), Err(VmmError::ContractViolation(_))));
            let mut wr = Vmm::new(
                configured_mock(vec![Exit::Arch(X86Exit::Wrmsr {
                    index: idx,
                    value: 0,
                })]),
                GuestRam::new(0x1000).unwrap(),
            );
            assert!(matches!(wr.step(), Err(VmmError::ContractViolation(_))));
        }
    }

    #[test]
    fn rng_width_only_accepts_architectural_2_4_8() {
        let mut w = VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::new()), 1)
            .expect("wiring");
        // Only the 16/32/64-bit forms are valid; everything else fails closed —
        // including the in-`1..=8`-but-non-architectural widths 1/3/5/6/7 (the
        // decoded exit width is untrusted).
        for bad in [0u8, 1, 3, 5, 6, 7, 9, 16, 255] {
            assert!(
                matches!(w.draw_rng(bad), Err(VmmError::ContractViolation(_))),
                "width {bad} must fail closed"
            );
        }
        for good in [2u8, 4, 8] {
            assert!(w.draw_rng(good).is_ok(), "width {good} must be accepted");
        }
    }

    /// Reviewer-required (blocking fix 1): the V-time / seeded-RNG state IS in the
    /// hash. Two states with identical RAM+regs but different seed (or `vns_base`)
    /// must hash **differently** (replay-equivalence); a stock `vtime: None` Vmm
    /// emits no `VTIM` chunk, so M1/M2 hashes are byte-for-byte unchanged.
    #[test]
    fn vtime_state_is_hashed_and_distinguishes_seed_and_vns_base() {
        fn contains_tag(blob: &[u8], tag: &[u8; 4]) -> bool {
            blob.windows(4).any(|w| w == tag)
        }
        fn wired(seed: u64, cfg: vtime::VClockConfig) -> Vmm<MockBackend> {
            let mut v = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
            v.wire_vtime(VtimeWiring::new(cfg, Box::new(ScriptedWork::new()), seed).unwrap());
            v
        }

        // Stock (vtime: None): NO VTIM chunk ⇒ hash unchanged from before.
        let stock = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert!(
            !contains_tag(&stock.state_blob(), b"VTIM"),
            "stock Vmm must not emit a VTIM chunk (M1/M2 hash unchanged)"
        );
        // Two stock Vmms with identical setup still hash identically.
        let stock2 = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert_eq!(stock.state_hash(), stock2.state_hash());

        // Wiring vtime adds the chunk and changes the hash.
        let a = wired(1, contract_vclock_config());
        assert!(contains_tag(&a.state_blob(), b"VTIM"));
        assert_ne!(
            a.state_hash(),
            stock.state_hash(),
            "wiring vtime must change the hash"
        );

        // Differ ONLY in seed ⇒ different hash.
        let b = wired(2, contract_vclock_config());
        assert_ne!(
            a.state_hash(),
            b.state_hash(),
            "different seed ⇒ different state_hash"
        );

        // Differ ONLY in ONE clock-config field (each governs future RDTSC) ⇒
        // different hash. Every variant is still a valid `VClockConfig`. This pins
        // every field of the `VTIM` encoding (a dropped field would let one of these
        // collide with `a`): `ratio_num`/`guest_hz`/`guest_base` are hashed directly,
        // and `vns_base` feeds the canonical effective-V-time field
        // `snapshot_vns(last_intercept_work)` (here work `0`, so `eff == vns_base`).
        // (`ratio_den` is enforced `== 1` by `VtimeWiring::new`, so it cannot vary
        // and is not encoded — see `encode_vtime`.)
        let base = contract_vclock_config();
        let variants = [
            (
                "ratio_num",
                vtime::VClockConfig {
                    ratio_num: 2,
                    ..base
                },
            ),
            (
                "guest_hz",
                vtime::VClockConfig {
                    guest_hz: 3_000_000_000,
                    ..base
                },
            ),
            (
                "guest_base",
                vtime::VClockConfig {
                    guest_base: 5,
                    ..base
                },
            ),
            (
                "vns_base",
                vtime::VClockConfig {
                    vns_base: 12_345,
                    ..base
                },
            ),
        ];
        for (field, cfg) in variants {
            assert_ne!(
                a.state_hash(),
                wired(1, cfg).state_hash(),
                "different {field} ⇒ different state_hash"
            );
        }

        // Differ ONLY in the last-intercept work ⇒ different hash (pins the
        // effective-V-time field). The work counter is no longer hashed via a live
        // read; it folds into the canonical effective-V-time field at each V-time
        // intercept, so a difference is observed by actually stepping an RDTSC. Both
        // VMs step the same script (the mock's saved state is unchanged by a
        // completion's value), so ONLY the `VTIM` chunk — the work the RDTSC read —
        // differs between them.
        fn stepped_with_work(work: u64) -> Vmm<MockBackend> {
            let mut v = Vmm::new(
                configured_mock(vec![Exit::Arch(X86Exit::Rdtsc)]),
                GuestRam::new(0x1000).unwrap(),
            );
            v.wire_vtime(
                VtimeWiring::new(
                    contract_vclock_config(),
                    Box::new(ScriptedWork::at(work)),
                    1,
                )
                .unwrap(),
            );
            v.step().unwrap(); // RDTSC intercept → records last_intercept_work
            v
        }
        assert_ne!(
            stepped_with_work(100).state_hash(),
            stepped_with_work(200).state_hash(),
            "different last-intercept work ⇒ different state_hash"
        );

        // Same seed + same cfg ⇒ same hash (deterministic; no false-different).
        let a2 = wired(1, contract_vclock_config());
        assert_eq!(a.state_hash(), a2.state_hash());
    }

    // -----------------------------------------------------------------------
    // Report channel (corpus box-integration): the dedicated 0x0CA2 OUT lane,
    // its stream, and the observable digest — all mock-driven, every platform.
    // -----------------------------------------------------------------------

    fn report_out(value: u32) -> Exit<X86> {
        Exit::Arch(X86Exit::Io {
            port: REPORT_PORT,
            size: 4,
            write: Some(value),
        })
    }

    #[test]
    fn report_port_out_appends_values_in_order() {
        // Two `report(u64)` calls = four dwords (low, high, low, high). The host
        // appends each in execution order; no completion (it is an OUT write).
        let mut vmm = Vmm::new(
            configured_mock(vec![
                report_out(0x1111_1111),
                report_out(0x0000_0000),
                report_out(0xDEAD_BEEF),
                report_out(0x0000_0001),
                Exit::Common(CommonExit::Idle),
            ]),
            GuestRam::new(0x1000).unwrap(),
        );
        let r = vmm.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Idle);
        assert_eq!(
            vmm.report_stream(),
            [0x1111_1111, 0x0000_0000, 0xDEAD_BEEF, 0x0000_0001]
        );
        // A report write is a pure OUT — it never stages a completion.
        assert!(vmm.backend.completions().is_empty());
    }

    #[test]
    fn report_port_non_dword_fails_closed() {
        // The report channel is dword-addressed; a byte/word write is unmodeled
        // and must fail closed, never silently truncate a reported value.
        for bad_size in [1u8, 2] {
            let mut vmm = Vmm::new(
                configured_mock(vec![Exit::Arch(X86Exit::Io {
                    port: REPORT_PORT,
                    size: bad_size,
                    write: Some(0xAB),
                })]),
                GuestRam::new(0x1000).unwrap(),
            );
            assert!(
                matches!(vmm.step(), Err(VmmError::ContractViolation(_))),
                "report write of size {bad_size} must fail closed"
            );
        }
    }

    #[test]
    fn observable_digest_tracks_report_stream_but_state_hash_does_not() {
        // Two otherwise-identical VMs: A reports values, B reports nothing. The
        // report stream is NOT in state_hash (so M1/M2 hashes are unchanged), but
        // it IS in observable_digest (the O2/O3 conformance signal).
        let mut a = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        let b = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        a.report_stream = vec![0xAA, 0xBB];
        assert_eq!(
            a.state_hash(),
            b.state_hash(),
            "report stream must NOT reach state_hash (M1/M2 hash unchanged)"
        );
        assert_ne!(
            a.observable_digest(),
            b.observable_digest(),
            "report stream MUST reach observable_digest"
        );
        // Deterministic + order-sensitive: same stream ⇒ same digest; a reorder ⇒
        // a different digest (the stream is ordered by execution).
        let mut a2 = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        a2.report_stream = vec![0xAA, 0xBB];
        assert_eq!(a.observable_digest(), a2.observable_digest());
        let mut a_rev = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        a_rev.report_stream = vec![0xBB, 0xAA];
        assert_ne!(a.observable_digest(), a_rev.observable_digest());
    }

    #[test]
    fn observable_digest_also_covers_the_serial_banner() {
        // Same (empty) report stream, different serial ⇒ different digest: the
        // banner is part of the guest-observable output.
        let mut quiet = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        let mut loud = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        for &byte in b"PAYLOAD x PASS\n" {
            loud.devices
                .uart
                .write(crate::vendor::x86::devices::UART_PORT_BASE, byte);
        }
        assert_ne!(quiet.observable_digest(), loud.observable_digest());
        // A length prefix guards against the classic concatenation ambiguity:
        // report-stream bytes can never be confused with serial bytes.
        quiet.report_stream = vec![u32::from_le_bytes(*b"PAYL")];
        assert_ne!(
            quiet.observable_digest(),
            loud.observable_digest(),
            "domain/length-prefixed digest separates the report stream from serial"
        );
    }

    #[test]
    fn state_components_breakdown_is_stable_and_covers_state() {
        // The diagnostic per-component breakdown (PR #51): stable, pure, covers the
        // expected components, and — crucially — does NOT include the report stream
        // (that is the O2/O3 signal, separate from the architectural state it helps
        // bisect).
        let v = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        let comps = v.state_components();
        assert_eq!(comps, v.state_components(), "pure: two calls agree");
        let labels: Vec<&str> = comps.iter().map(|(l, _)| *l).collect();
        for expect in [
            "RAM:0..64K",
            "regs",
            "segments",
            "control-regs",
            "msrs",
            "xsave-legacy",
            "xsave-header",
            "xsave-extended",
            "serial",
            "dev",
        ] {
            assert!(
                labels.contains(&expect),
                "missing component {expect}: {labels:?}"
            );
        }
        // `vtim:*` sub-components only when V-time is wired.
        assert!(
            !labels.iter().any(|l| l.starts_with("vtim")),
            "no vtim components when unwired"
        );
        let mut w = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        w.wire_vtime(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::new()), 1).unwrap(),
        );
        let wlabels: Vec<&str> = w.state_components().iter().map(|(l, _)| *l).collect();
        for expect in [
            "vtim:cfg",
            "vtim:eff-vns",
            "vtim:entropy",
            "vtim:last-intercept",
            "vtim:work-raw",
        ] {
            assert!(wlabels.contains(&expect), "missing {expect}: {wlabels:?}");
        }
        // Two identical VMs ⇒ identical component digests.
        let v2 = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert_eq!(v.state_components(), v2.state_components());
        // The report stream is NOT an architectural component — mutating it leaves
        // the breakdown unchanged (so a report-channel difference can never masquerade
        // as an architectural-state divergence in the bisector).
        let mut v3 = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        v3.report_stream = vec![0xDEAD_BEEF];
        assert_eq!(v.state_components(), v3.state_components());
    }

    #[test]
    fn expected_draw_matches_completion_for_each_width() {
        for width in [2u8, 4, 8] {
            let mut vmm = vtime_vmm(
                vec![
                    Exit::Arch(X86Exit::Rdrand { width }),
                    Exit::Common(CommonExit::Idle),
                ],
                Box::new(ScriptedWork::new()),
                0xFEED,
            );
            vmm.run().unwrap();
            assert_eq!(
                vmm.backend.completions(),
                &[Completion::Read(expected_draw(0xFEED, width))]
            );
        }
    }

    /// Task-27 item 2, the fix itself: `state_hash`/`state_blob` must **not** take a
    /// live read of the work counter. The OLD `encode_vtime` did, at hash time — and
    /// that terminal read carries the non-deterministic post-last-intercept exit-path
    /// skid, which made the `VTIM` chunk diverge across two same-seed box runs (corpus
    /// O1, PR #51). A `WorkSource` that counts its reads proves hashing takes none.
    #[test]
    fn state_hash_does_not_read_the_live_work_counter() {
        use std::rc::Rc;
        struct CountingWork {
            value: u64,
            reads: Rc<Cell<u32>>,
        }
        impl WorkSource for CountingWork {
            fn work(&self) -> Result<u64, WorkError> {
                self.reads.set(self.reads.get() + 1);
                Ok(self.value)
            }
            fn reset(&mut self) -> Result<(), WorkError> {
                Ok(())
            }
        }
        let reads = Rc::new(Cell::new(0u32));
        let mut vmm = Vmm::new(
            configured_mock(vec![
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Common(CommonExit::Idle),
            ]),
            GuestRam::new(0x1000).unwrap(),
        );
        vmm.wire_vtime(
            VtimeWiring::new(
                contract_vclock_config(),
                Box::new(CountingWork {
                    value: 50,
                    reads: Rc::clone(&reads),
                }),
                7,
            )
            .unwrap(),
        );
        vmm.run().unwrap();
        let after_run = reads.get();
        assert!(
            after_run > 0,
            "the run must read the counter at the RDTSC intercept"
        );
        // Hashing must add NO further counter reads (a live read would carry skid).
        let _ = vmm.state_hash();
        let _ = vmm.state_hash();
        let _ = vmm.state_blob();
        assert_eq!(
            reads.get(),
            after_run,
            "state_hash/state_blob must not read the live work counter; the VTIM hash \
             anchors to the recorded deterministic last-intercept work"
        );
    }

    /// Task-27 item 2, test (i) — **deterministic-twice despite terminal skid**. Two
    /// same-seed runs read the same deterministic work at the RDTSC intercept, but a
    /// read taken *after* the run (what the OLD `encode_vtime` did at hash time) would
    /// advance by a per-run, non-deterministic skid. The fix anchors the `VTIM` hash
    /// to the recorded last-intercept work, so the chunk (hence `state_hash`) is
    /// byte-identical regardless of skid — the property the box O1 gate checks.
    #[test]
    fn vtim_is_deterministic_twice_despite_terminal_skid() {
        struct SkiddingWork {
            intercept: u64,
            skid: u64,
            reads: Cell<u32>,
        }
        impl WorkSource for SkiddingWork {
            fn work(&self) -> Result<u64, WorkError> {
                let n = self.reads.get();
                self.reads.set(n + 1);
                // Read #0 is the RDTSC intercept (deterministic, identical in both
                // runs). Any later read (a hypothetical hash-time read) carries the
                // divergent skid — which the fix never takes.
                Ok(if n == 0 {
                    self.intercept
                } else {
                    self.intercept + self.skid
                })
            }
            fn reset(&mut self) -> Result<(), WorkError> {
                Ok(())
            }
        }
        let run_with_skid = |skid: u64| {
            let mut vmm = Vmm::new(
                configured_mock(vec![
                    Exit::Arch(X86Exit::Rdtsc),
                    Exit::Common(CommonExit::Idle),
                ]),
                GuestRam::new(0x1000).unwrap(),
            );
            vmm.wire_vtime(
                VtimeWiring::new(
                    contract_vclock_config(),
                    Box::new(SkiddingWork {
                        intercept: 50,
                        skid,
                        reads: Cell::new(0),
                    }),
                    7,
                )
                .unwrap(),
            );
            vmm.run().unwrap();
            vmm.state_hash()
        };
        assert_eq!(
            run_with_skid(0),
            run_with_skid(0xDEAD),
            "two same-seed runs must produce a byte-identical VTIM (hence state_hash) \
             even when a terminal raw-counter read would diverge by skid"
        );
    }

    /// Task-27 item 2, test (ii) — **restore-transparency**. A fresh VM that advanced
    /// to effective V-time `E` (RDTSC at work `E`; ratio 1:1 ⇒ vns == work) and a VM
    /// restored to a snapshot at that same effective V-time (`vns_base = E`, counter
    /// reset to 0) must hash **identically**: `encode_vtime` folds `vns_base` + work
    /// into one canonical effective-V-time field, so the two indistinguishable
    /// timelines are indistinguishable to `unison::compare_runs`.
    #[test]
    fn restored_and_fresh_at_same_effective_vtime_hash_identically() {
        const E: u64 = 4242;
        const SEED: u64 = 0x1234;

        // Fresh: vns_base=0, step one RDTSC reading work=E ⇒ last_intercept_work=E,
        // effective V-time = snapshot_vns(E) = E.
        let mut fresh = Vmm::new(
            configured_mock(vec![Exit::Arch(X86Exit::Rdtsc)]),
            GuestRam::new(0x1000).unwrap(),
        );
        fresh.wire_vtime(
            VtimeWiring::new(
                contract_vclock_config(),
                Box::new(ScriptedWork::at(E)),
                SEED,
            )
            .unwrap(),
        );
        fresh.step().unwrap();

        // Restored: a fresh VM restored to a snapshot whose vns == E. restore_vtime
        // sets vns_base=E and last_intercept_work=0, so effective V-time =
        // snapshot_vns(0) = vns_base = E. The entropy blob is a freshly-saved
        // same-seed stream, so the restored stream matches fresh's (no draws either).
        let mut restored = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        restored.wire_vtime(
            VtimeWiring::new(
                contract_vclock_config(),
                Box::new(ScriptedWork::new()),
                SEED,
            )
            .unwrap(),
        );
        let snap = VtimeSnapshot {
            vns: E,
            guest_clock_offset: 0,
            entropy: SeededEntropy::new(SEED).save_state(),
        };
        restored.restore_vtime(&snap).unwrap();

        assert_eq!(
            fresh.state_hash(),
            restored.state_hash(),
            "a restored VM and a fresh VM at the same effective V-time must hash identically"
        );
    }

    /// Task-27 item 2 (box-verification cross-model finding): an RNG exit
    /// (RDRAND/RDSEED) is a V-time intercept and MUST advance `last_intercept_work`.
    /// Two states with **different** pre-RNG-exit branch counts but an **identical**
    /// seeded draw must hash **DIFFERENTLY** — otherwise they collide in `VTIM` (a
    /// false determinism MATCH) and then diverge on the next TSC read. Without the fix
    /// both keep the stale anchor (`0` here, no prior TSC) and hash the same.
    #[test]
    fn rng_exit_advances_the_vtim_work_anchor() {
        let after_rng_at_work = |work: u64| {
            let mut v = vtime_vmm(
                vec![Exit::Arch(X86Exit::Rdrand { width: 8 })],
                Box::new(ScriptedWork::at(work)),
                0x7777, // same seed ⇒ identical draw in both
            );
            v.step().unwrap(); // RDRAND draws AND records last_intercept_work = work
            v
        };
        assert_ne!(
            after_rng_at_work(100).state_hash(),
            after_rng_at_work(200).state_hash(),
            "different pre-RNG-exit work ⇒ different VTIM, despite an identical seeded draw"
        );
    }

    // -----------------------------------------------------------------------
    // Linux boot path: xAPIC MMIO + legacy-platform I/O wiring (task 30).
    // -----------------------------------------------------------------------

    /// A `Vmm<MockBackend>` with the Linux platform wired (xAPIC + legacy I/O).
    fn linux_vmm(exits: Vec<Exit<X86>>) -> Vmm<MockBackend> {
        let mut v = Vmm::new(configured_mock(exits), GuestRam::new(0x1000).unwrap());
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        v
    }

    #[test]
    fn apic_mmio_serviced_only_when_lapic_wired() {
        // Wired: a load of the xAPIC Version register (offset 0x30) completes with
        // the architectural value; a store is accepted (Continued).
        let mut v = linux_vmm(vec![
            Exit::Common(CommonExit::Mmio {
                gpa: Gpa(0xFEE0_0030),
                size: 4,
                write: None,
            }),
            Exit::Common(CommonExit::Mmio {
                gpa: Gpa(0xFEE0_00B0),
                size: 4,
                write: Some(0),
            }), // EOI store
            Exit::Common(CommonExit::Idle),
        ]);
        assert!(v.lapic_wired());
        let r = v.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Idle);
        assert_eq!(
            v.backend.completions(),
            &[Completion::Read(u64::from(lapic::APIC_VERSION_VALUE))]
        );

        // Unwired (M1/M2): any MMIO is a loud contract violation, never serviced.
        let mut stock = Vmm::new(
            configured_mock(vec![Exit::Common(CommonExit::Mmio {
                gpa: Gpa(0xFEE0_0030),
                size: 4,
                write: None,
            })]),
            GuestRam::new(0x1000).unwrap(),
        );
        assert!(!stock.lapic_wired(), "stock Vmm has no xAPIC wired");
        assert!(matches!(stock.step(), Err(VmmError::ContractViolation(_))));
    }

    #[test]
    fn mmio_outside_apic_page_fails_closed_even_on_linux_path() {
        // A non-xAPIC MMIO address is unmodeled and fails closed even with the
        // Linux platform wired (the xAPIC page is the only modeled MMIO).
        let mut v = linux_vmm(vec![Exit::Common(CommonExit::Mmio {
            gpa: Gpa(0xFEB0_0000),
            size: 4,
            write: None,
        })]);
        assert!(matches!(v.step(), Err(VmmError::ContractViolation(_))));
    }

    #[test]
    fn legacy_io_serviced_only_when_wired() {
        // Wired: OUT to the PCI CONFIG_ADDRESS latch, then IN from CONFIG_DATA reads
        // "no device" (all-ones).
        let mut v = linux_vmm(vec![
            Exit::Arch(X86Exit::Io {
                port: 0x0CF8,
                size: 4,
                write: Some(0x8000_0000),
            }),
            Exit::Arch(X86Exit::Io {
                port: 0x0CFC,
                size: 4,
                write: None,
            }),
            Exit::Common(CommonExit::Idle),
        ]);
        v.run().expect("run");
        assert_eq!(v.backend.completions(), &[Completion::Read(0xFFFF_FFFF)]);

        // Unwired: the same legacy port OUT is a contract violation.
        let mut stock = Vmm::new(
            configured_mock(vec![Exit::Arch(X86Exit::Io {
                port: 0x0CF8,
                size: 4,
                write: Some(0),
            })]),
            GuestRam::new(0x1000).unwrap(),
        );
        assert!(matches!(stock.step(), Err(VmmError::ContractViolation(_))));
    }

    #[test]
    fn linux_platform_state_in_hash_only_when_wired() {
        fn has(blob: &[u8], tag: &[u8; 4]) -> bool {
            blob.windows(4).any(|w| w == tag)
        }
        // Stock Vmm: no LAPC/LEGY chunks — M1/M2/corpus hash is byte-for-byte
        // unchanged from before this path existed.
        let stock = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        let stock_blob = stock.state_blob();
        assert!(!has(&stock_blob, b"LAPC"));
        assert!(!has(&stock_blob, b"LEGY"));

        // Linux Vmm: both chunks present, and the hash differs from stock.
        let linux = linux_vmm(vec![]);
        let blob = linux.state_blob();
        assert!(has(&blob, b"LAPC"));
        assert!(has(&blob, b"LEGY"));
        assert_ne!(stock.state_hash(), linux.state_hash());

        // The LEGY chunk tracks the PCI latch: two Linux VMs that program different
        // CONFIG_ADDRESS values hash differently.
        let with_pci = |addr: u32| {
            let mut v = linux_vmm(vec![Exit::Arch(X86Exit::Io {
                port: 0x0CF8,
                size: 4,
                write: Some(addr),
            })]);
            v.step().unwrap();
            v
        };
        assert_ne!(with_pci(0x1000).state_hash(), with_pci(0x2000).state_hash());
    }

    #[test]
    fn serial_and_exit_counts_accessors_reflect_the_run() {
        // The box-gate accessors return the real captured console + trap counts (not
        // a constant / Default).
        let mut v = linux_vmm(vec![
            Exit::Arch(X86Exit::Io {
                port: 0x3F8,
                size: 1,
                write: Some(u32::from(b'H')),
            }),
            Exit::Arch(X86Exit::Io {
                port: 0x3F8,
                size: 1,
                write: Some(u32::from(b'i')),
            }),
            Exit::Common(CommonExit::Idle),
        ]);
        v.run().expect("run");
        assert_eq!(v.serial(), b"Hi");
        assert!(v.exit_counts().io >= 2, "exit_counts reflects the IO exits");
    }

    #[test]
    fn mmio_just_past_apic_page_fails_closed() {
        // An access one page above the xAPIC base is outside the modeled page → a
        // loud contract violation (pins the `..APIC_MMIO_END` upper bound).
        let mut v = linux_vmm(vec![Exit::Common(CommonExit::Mmio {
            gpa: Gpa(0xFEE0_1000),
            size: 4,
            write: None,
        })]);
        assert!(matches!(v.step(), Err(VmmError::ContractViolation(_))));
    }

    #[test]
    fn lapic_timer_current_count_tracks_vtime() {
        // The xAPIC timer's now_vns comes from `lapic_now_vns` (the V-time effective
        // ns at the last intercept). Arm the timer at V-time 0, advance V-time via an
        // RDTSC intercept, then read TMCCT: it must have decreased — which can only
        // happen if `lapic_now_vns` reports the advanced V-time (kills `-> 0`).
        const W: u64 = 100_000_000; // 100 ms of V-time at ratio 1:1 → many timer ticks
        let mut v = Vmm::new(
            configured_mock(vec![
                Exit::Common(CommonExit::Mmio {
                    gpa: Gpa(0xFEE0_00F0),
                    size: 4,
                    write: Some(0x1FF),
                }), // SVR: enable
                Exit::Common(CommonExit::Mmio {
                    gpa: Gpa(0xFEE0_0320),
                    size: 4,
                    write: Some(0x40),
                }), // LVT timer: unmasked oneshot, vec 0x40
                Exit::Common(CommonExit::Mmio {
                    gpa: Gpa(0xFEE0_0380),
                    size: 4,
                    write: Some(0xFFFF_FFFF),
                }), // TMICT: arm at now=0
                Exit::Arch(X86Exit::Rdtsc), // V-time intercept → last_intercept_work = W
                Exit::Common(CommonExit::Mmio {
                    gpa: Gpa(0xFEE0_0390),
                    size: 4,
                    write: None,
                }), // read TMCCT at now=W
                Exit::Common(CommonExit::Idle),
            ]),
            GuestRam::new(0x1000).unwrap(),
        );
        v.wire_vtime(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(W)), 1).unwrap(),
        );
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );

        v.run().expect("run");
        let tmcct = match v.backend.completions().last() {
            Some(Completion::Read(v)) => *v,
            other => panic!("expected a TMCCT read completion, got {other:?}"),
        };
        assert!(tmcct > 0, "timer is running (some count remains)");
        assert!(
            tmcct < 0xFFFF_FFFF,
            "TMCCT decreased from the armed initial count — lapic_now_vns advanced with V-time"
        );
    }

    #[test]
    fn lapic_register_state_is_in_the_hash() {
        // Two Linux VMs identical but for one xAPIC register write (TPR) must hash
        // **differently** — i.e. `encode_lapic_state` reflects the register file
        // (kills the `encode_lapic_state -> vec![]/vec![0]/vec![1]` constant mutants,
        // which would erase the register content from the LAPC chunk).
        let base = linux_vmm(vec![]);
        let mut modified = linux_vmm(vec![Exit::Common(CommonExit::Mmio {
            gpa: Gpa(0xFEE0_0080),
            size: 4,
            write: Some(0x20),
        })]);
        modified.step().unwrap(); // write TPR = 0x20
        assert_ne!(
            base.state_hash(),
            modified.state_hash(),
            "an xAPIC register write must change the LAPC hash chunk"
        );
    }

    // -----------------------------------------------------------------------
    // Interrupt injection: the V-time LAPIC timer drives `Backend::inject`
    // (task 32). Driven by a scripted MockBackend that records injections; the
    // ready/window handshake itself is tested below the trait (vmm-backend's
    // synthetic-`kvm_run` `plan_irq_entry` tests).
    // -----------------------------------------------------------------------

    /// A configured mock reporting **stock** capabilities (no deterministic TSC),
    /// so [`Vmm::lapic_now_vns`] reads the live work counter (the Phase B.1 path).
    fn configured_stock_mock(exits: Vec<Exit<X86>>) -> MockBackend {
        let mut m = MockBackend::with_capabilities(vmm_backend::Capabilities {
            name: "mock-stock",
            deterministic_rng: false,
            arch: X86Caps {
                deterministic_tsc: false,
                enforces_tsc_deadline_msr: false,
            },
        });
        m.extend_exits(exits);
        m.set_policy(&X86Policy {
            cpuid: CpuidModel::default(),
            msr_filter: MsrFilter::default(),
        })
        .expect("set_policy");
        m
    }

    /// A work source whose value the test sets between steps (a shared `Cell`),
    /// to drive the live-work LAPIC clock on the stock path deterministically.
    struct SharedWork(std::rc::Rc<Cell<u64>>);
    impl WorkSource for SharedWork {
        fn work(&self) -> Result<u64, WorkError> {
            Ok(self.0.get())
        }
        fn reset(&mut self) -> Result<(), WorkError> {
            self.0.set(0);
            Ok(())
        }
    }

    /// Arm a one-shot LAPIC timer (vector `0x40`) via three MMIO writes: SVR
    /// software-enable, LVT-timer unmasked one-shot, and an Initial Count that
    /// arms it at the current V-time. (Default reset divide-config = ÷2.)
    fn arm_timer_exits(initial_count: u64) -> Vec<Exit<X86>> {
        let w = |off: u64, v: u64| {
            Exit::Common(CommonExit::Mmio {
                gpa: Gpa(APIC_MMIO_BASE + off),
                size: 4,
                write: Some(v),
            })
        };
        vec![
            w(u64::from(lapic::APIC_SVR), 0x1FF), // software-enable, spurious vec 0xFF
            w(u64::from(lapic::APIC_LVT_TIMER), 0x40), // unmasked one-shot, vector 0x40
            w(u64::from(lapic::APIC_TMICT), initial_count), // arm at the current now_vns
        ]
    }

    #[test]
    fn lapic_timer_delivers_off_intercept_anchor_on_deterministic_backend() {
        // Deterministic backend (default mock caps): the timer clock is the skid-free
        // last-intercept anchor. Arm the timer at V-time 0; an ISR read BEFORE the
        // RDTSC sees no delivery (anchor still 0 — a live-work mutant would have fired
        // it), and an ISR read AFTER the RDTSC advances the anchor sees the vector in
        // service (fired, accepted, IRR→ISR completed).
        const W: u64 = 100_000_000; // 100 ms of V-time — far past the timer period
        let mut exits = arm_timer_exits(1000);
        exits.push(read_mmio(isr_gpa(0x40))); // A: anchor still 0 → not delivered
        exits.push(Exit::Arch(X86Exit::Rdtsc)); // V-time intercept → last_intercept_work = W
        exits.push(read_mmio(isr_gpa(0x40))); // B: anchor = W → delivered
        exits.push(Exit::Common(CommonExit::Idle));
        let mut v = lapic_vmm(configured_mock(exits), Box::new(ScriptedWork::at(W)));

        v.run().expect("run");
        let reads = read_completions(&v);
        assert_eq!(
            reads.first().expect("ISR read A") & 1,
            0,
            "not delivered before the anchor advances (off the intercept anchor, not live work)"
        );
        assert_eq!(
            reads.last().expect("ISR read B") & 1,
            1,
            "delivered once the intercept anchor crosses the timer deadline"
        );
    }

    #[test]
    fn lapic_timer_delivers_on_stock_off_live_work() {
        // Stock backend (no RDTSC trap): the timer clock reads the *live* work
        // counter, so the periodic tick advances without TSC-MSR intercepts (the
        // Phase B.1 GUEST_READY path). Bumping live work alone fires + delivers it.
        const W: u64 = 100_000_000;
        let cell = std::rc::Rc::new(Cell::new(0u64));
        let work = Box::new(SharedWork(cell.clone()));
        let mut exits = arm_timer_exits(1000);
        exits.push(read_mmio(isr_gpa(0x40))); // A: live work still 0 → not delivered
        exits.push(read_mmio(isr_gpa(0x40))); // B: after bumping live work → delivered
        exits.push(Exit::Common(CommonExit::Idle));
        let mut v = lapic_vmm(configured_stock_mock(exits), work);

        // Steps 1-3 program the timer (live work 0). Step 4 reads ISR (A): not fired.
        for _ in 0..4 {
            assert!(matches!(v.step().unwrap(), Step::Continued));
        }
        // Advance live work past the deadline; the next step fires + delivers.
        cell.set(W);
        assert!(matches!(v.step().unwrap(), Step::Continued)); // ISR read (B)
        assert!(matches!(
            v.step().unwrap(),
            Step::Terminal(TerminalReason::Idle)
        ));
        let reads = read_completions(&v);
        assert_eq!(
            reads.first().expect("ISR read A") & 1,
            0,
            "not delivered while live work (V-time) is still 0"
        );
        assert_eq!(
            reads.last().expect("ISR read B") & 1,
            1,
            "delivered off the advancing live-work clock (deterministic_tsc=false)"
        );
    }

    #[test]
    fn run_until_deadline_advances_anchor_and_fires_the_timer() {
        // The preemption path (task 47), wiring proof on the portable mock: once a
        // LAPIC timer is armed on the determinism-complete path, the VMM runs via
        // `run_until` (busy-spin preemption). An `CommonExit::Deadline` advances the
        // skid-free anchor to the reached work, so the NEXT entry fires the timer
        // into the IRR and delivers it (IRR→ISR on acceptance). A guest that never
        // exits on its own thus still observes the timer.
        let mut exits = arm_timer_exits(1000);
        // The mock rewrites `reached` to the deadline the VMM passed `run_until`
        // (= work_for_vns(timer deadline)); the literal here is a placeholder.
        exits.push(Exit::Common(CommonExit::Deadline { reached: Moment(0) }));
        exits.push(read_mmio(isr_gpa(0x40))); // after delivery: vector 0x40 in service
        exits.push(Exit::Common(CommonExit::Idle));
        // Default (deterministic) caps so `preemption_deadline` engages; the
        // ScriptedWork value is irrelevant (the clock reads the intercept anchor).
        let mut v = lapic_vmm(configured_mock(exits), Box::new(ScriptedWork::at(0)));

        v.run().expect("run");
        let isr = *read_completions(&v).last().expect("ISR read");
        assert_eq!(
            isr & 1,
            1,
            "the timer vector (0x40, ISR bank bit 0) is delivered after the run_until \
             preemption deadline advanced the anchor past the timer's expiry"
        );
        // The anchor moved to the reached work (a non-zero V-time intercept), proving
        // `on_deadline` recorded the preemption point.
        let reached = v.vtime.as_ref().unwrap().last_intercept_work;
        assert!(
            reached > 0,
            "the preemption deadline advanced the last-intercept anchor"
        );
        // `on_deadline` also recorded the MEASURED landing (the seed-dependence gate reads
        // this): exactly one preemption, at the reached work.
        assert_eq!(
            v.preemption_landings(),
            &[reached],
            "on_deadline records each measured preemption landing"
        );
    }

    #[test]
    fn arrival_deadline_is_cleared_on_restore() {
        // PR #51 round-3 item 3: a host-fault arrival armed against the PRE-restore
        // timeline must not survive a restore — else the stale arm bounds the first
        // post-restore `step` at a now-meaningless work count (the #34/#55 stale-arm
        // class). Both restore primitives clear it.
        // A fresh V-time-wired VM is at a synchronized, snapshottable point with no
        // staged completion — so both `save_vtime` and `save_vm_state` succeed.
        let mut v = vtime_vmm(
            vec![Exit::Common(CommonExit::Idle)],
            Box::new(ScriptedWork::at(100)),
            1,
        );
        let snap = v.save_vtime().unwrap().expect("v-time wired");
        let vm_state = v.save_vm_state().unwrap();

        // restore_vtime clears the arm (the standalone + idle-rebase path).
        assert!(v.arm_arrival(500), "deterministic mock arms arrival");
        assert!(v.arrival_deadline.is_some());
        v.restore_vtime(&snap).unwrap();
        assert!(
            v.arrival_deadline.is_none(),
            "restore_vtime clears the stale arrival arm"
        );

        // restore_vm_state clears it too (the snapshot-restore funnel).
        assert!(v.arm_arrival(500));
        assert!(v.arrival_deadline.is_some());
        v.restore_vm_state(&vm_state).unwrap();
        assert!(
            v.arrival_deadline.is_none(),
            "restore_vm_state clears the stale arrival arm"
        );
    }

    /// P1 round-13 — the comprehensive zero-step invariant: a `run_until` that returns
    /// `CommonExit::Deadline` WITHOUT entering the guest (the overdue/at-deadline path, no
    /// `KVM_RUN`) must NOT clear any entry-side state. A staged completion is committed only
    /// by a real entry, so a no-entry Deadline must leave `completion_staged` /
    /// `rng_completion_staged` SET (else a snapshot here is taken/restored across a live
    /// pending completion → corruption). Then a real entry commits it.
    /// Drive a staged completion → a NO-ENTRY zero-step `Deadline` → a real entry, asserting
    /// the staged-completion guards HOLD across the no-entry step and clear on the real
    /// entry. `work_before_of(deadline_work)` chooses the live work at the no-entry step:
    /// `|d| d + N` is OVERDUE (reached < work_before) and `|d| d` is AT-DEADLINE (reached ==
    /// work_before) — both no-entry, and together they pin the `reached > work_before`
    /// entry-test at both boundaries.
    fn no_entry_deadline_holds_staged_guards(work_before_of: fn(u64) -> u64, label: &str) {
        // SharedWork lets the test advance live work between steps, to drive a no-entry
        // run_until while the intercept anchor stays BEHIND the deadline (so
        // `service_pending_irqs`, which fires off the anchor, does NOT consume the timer).
        let cell = std::rc::Rc::new(Cell::new(0u64));
        let work = Box::new(SharedWork(cell.clone()));
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Arch(X86Exit::Rdrand { width: 8 })); // stages a completion (RNG: both guards)
        exits.push(Exit::Common(CommonExit::Deadline { reached: Moment(0) })); // mock rewrites reached := deadline
        exits.push(Exit::Common(CommonExit::Idle)); // a real entry that commits the staged completion
        let mut v = lapic_vmm(configured_mock(exits), work);

        for _ in 0..3 {
            v.step().expect("arm the one-shot timer"); // live work 0; anchor 0
        }
        // The Rdrand is a REAL entry (work 0 < the future deadline): dispatching it stages
        // an RNG completion. The intercept anchor is set to the live work (0).
        v.step().expect("rdrand");
        assert!(
            v.completion_staged && v.rng_completion_staged,
            "{label}: the RDRAND staged a (non-idempotent RNG) completion"
        );
        // Set LIVE work to the chosen boundary (the anchor stays 0), so the next `run_until`
        // is at/past the deadline → the no-entry zero-step path.
        let deadline_work = v.preemption_deadline().expect("timer armed").0;
        assert!(
            deadline_work > 0,
            "{label}: deadline in the anchor's future"
        );
        cell.set(work_before_of(deadline_work));
        // The scripted Deadline → `run_until` returns `Deadline { reached = deadline_work }`,
        // and `reached <= work_before` ⇒ NO entry. The staged-completion guards MUST hold.
        v.step().expect("no-entry deadline");
        assert!(
            v.completion_staged,
            "{label}: a no-entry zero-step Deadline must NOT drop completion_staged (pending)"
        );
        assert!(
            v.rng_completion_staged,
            "{label}: a no-entry zero-step Deadline must NOT drop rng_completion_staged"
        );
        // A real entry (HLT) now commits the staged completion; the guards update.
        assert!(matches!(
            v.step().expect("real entry commits the staged completion"),
            Step::Terminal(TerminalReason::Idle)
        ));
        assert!(
            !v.completion_staged && !v.rng_completion_staged,
            "{label}: the real entry committed the staged completion → guards clear"
        );
    }

    #[test]
    fn no_entry_zero_step_deadline_keeps_entry_side_state() {
        // Both no-entry boundaries must hold the guards: OVERDUE (reached < work_before) and
        // AT-DEADLINE (reached == work_before). Testing both also pins the `reached >
        // work_before` entry-test exactly (a `<`/`==`/`>=` slip would wrongly treat one
        // boundary as an entry and drop the guards).
        no_entry_deadline_holds_staged_guards(|d| d + 10_000, "overdue");
        no_entry_deadline_holds_staged_guards(|d| d, "at-deadline");
    }

    #[test]
    fn restore_vtime_rearms_counter_a_first_entry_baseline() {
        // P1 round-10: `restore_vtime` must re-arm BOTH counter baselines so a coexisting
        // VM on the shared pinned thread can't contaminate one but not the other (B≡A).
        // Portable proof of the counter-A re-arm: `start_run` fires AGAIN at the entry
        // after `restore_vtime` (re-baselining vmm-core's WorkSource), exactly like a full
        // restore. (The backend counter-B re-arm — a `save`+`restore` round-trip — is
        // box-tested in `live_preemption.rs`, since the mock has no run_until PMU.)
        let starts = std::rc::Rc::new(Cell::new(0u32));
        let mut v = Vmm::new(
            configured_mock(vec![
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Common(CommonExit::Idle),
            ]),
            GuestRam::new(0x1000).unwrap(),
        );
        v.wire_vtime(
            VtimeWiring::new(
                contract_vclock_config(),
                Box::new(CountingStartWork {
                    starts: starts.clone(),
                }),
                1,
            )
            .unwrap(),
        );
        v.step().expect("step 1"); // first guest entry → start_run #1 (A baselined)
        assert_eq!(starts.get(), 1, "start_run fires at the first entry");
        let snap = v.save_vtime().expect("clean save").expect("V-time wired");
        v.restore_vtime(&snap).expect("V-time-only restore");
        assert_eq!(
            starts.get(),
            1,
            "restore_vtime itself does not run the guest, so start_run has not re-fired yet"
        );
        v.step().expect("step 2"); // first entry AFTER restore → start_run #2 (A re-baselined)
        assert_eq!(
            starts.get(),
            2,
            "restore_vtime re-armed counter A: the next entry re-baselines the WorkSource \
             (excluding any coexisting VM's branches), keeping B≡A"
        );
    }

    /// P3 round-12: `restore_vtime`'s counter re-arms are all-or-NOTHING. A backend failure
    /// during the (sole fallible) save/restore round-trip must leave counter A NOT re-armed
    /// — `first_entry_done` is set `false` only in the INFALLIBLE commit AFTER the
    /// round-trip succeeds, so a failure cannot re-arm A while leaving B un-re-armed (the
    /// `B re-armed but A not` bug round-11 still had). Proof: `start_run` does NOT re-fire
    /// at the entry after a FAILED restore (A's first-entry gate was never reset).
    #[test]
    fn restore_vtime_failure_leaves_counter_a_not_rearmed() {
        let starts = std::rc::Rc::new(Cell::new(0u32));
        let mut v = Vmm::new(
            SaveFailBackend(configured_mock(vec![
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Common(CommonExit::Idle),
            ])),
            GuestRam::new(0x1000).unwrap(),
        );
        v.wire_vtime(
            VtimeWiring::new(
                contract_vclock_config(),
                Box::new(CountingStartWork {
                    starts: starts.clone(),
                }),
                1,
            )
            .unwrap(),
        );
        v.step().expect("step 1"); // first entry → start_run #1 (first_entry_done now true)
        assert_eq!(starts.get(), 1, "start_run fires at the first entry");
        let snap = v.save_vtime().expect("clean save").expect("V-time wired");
        // The backend `save()` in the round-trip fails → restore_vtime aborts BEFORE the
        // commit, so `first_entry_done` stays true (A is NOT re-armed).
        assert!(
            matches!(v.restore_vtime(&snap), Err(VmmError::Backend(_))),
            "a failing backend round-trip must make restore_vtime fail closed"
        );
        v.step().expect("step 2"); // entry after the FAILED restore
        assert_eq!(
            starts.get(),
            1,
            "a FAILED restore_vtime must NOT re-arm counter A — start_run does not re-fire \
             (all-or-nothing: neither A nor B is re-armed on failure)"
        );
    }

    #[test]
    fn stale_vector_re_arbitrated_away_after_tpr_raise() {
        // [review P2] If the guest raises TPR above a peeked-but-not-yet-accepted
        // vector while it waits on the interrupt window, the VMM re-arbitrates (re-
        // peeks) every entry and overwrites the backend's pending slot — so the now-
        // stale vector is NOT injected, yet stays pending in the LAPIC IRR (not lost).
        const W: u64 = 100_000_000;
        let tpr_write = Exit::Common(CommonExit::Mmio {
            gpa: Gpa(APIC_MMIO_BASE + u64::from(lapic::APIC_TPR)),
            size: 4,
            write: Some(0xF0), // TPR class 0xF masks vector 0x40 (class 4)
        });
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Arch(X86Exit::Rdtsc)); // advance anchor → timer fires next service (peek 0x40)
        exits.push(tpr_write); // guest raises TPR while 0x40 waits on the window
        exits.push(read_mmio(irr_gpa(0x40))); // 0x40 still pending in IRR
        exits.push(Exit::Common(CommonExit::Idle));
        let mut mock = configured_mock(exits);
        mock.set_defer_accept(true); // hold 0x40 un-accepted across the TPR raise
        let mut v = lapic_vmm(mock, Box::new(ScriptedWork::at(W)));

        // SVR, LVT, TMICT, Rdtsc(anchor→W), tpr_write: 0x40 peeked + set pending,
        // then TPR raised above it.
        for _ in 0..5 {
            assert!(matches!(v.step().unwrap(), Step::Continued));
        }
        // Allow acceptance now: re-arbitration must already have replaced the stale
        // 0x40 with `None` (peek returns None under the raised TPR).
        v.backend.set_defer_accept(false);
        assert!(matches!(v.step().unwrap(), Step::Continued)); // IRR read
        assert!(matches!(
            v.step().unwrap(),
            Step::Terminal(TerminalReason::Idle)
        ));
        assert_eq!(
            v.backend.pending_irq(),
            None,
            "the stale, now-masked vector was re-arbitrated out of the pending slot"
        );
        assert_eq!(
            v.backend.take_accepted_interrupt(),
            None,
            "the stale vector was never accepted (KVM_INTERRUPT not issued for it)"
        );
        let reads = read_completions(&v);
        assert_eq!(
            reads.last().expect("IRR read") & 1,
            1,
            "0x40 is retained in IRR (masked by TPR, not dropped)"
        );
    }

    #[test]
    fn no_injection_when_lapic_unwired() {
        // M1/M2/corpus path: with no xAPIC wired, `service_pending_irqs` is a no-op —
        // it never calls `set_pending_irq`, so those paths' behavior and hash are
        // untouched.
        let mut v = Vmm::new(
            configured_mock(vec![Exit::Common(CommonExit::Idle)]),
            GuestRam::new(0x1000).unwrap(),
        );
        assert!(!v.lapic_wired());
        assert!(matches!(
            v.step().unwrap(),
            Step::Terminal(TerminalReason::Idle)
        ));
        assert!(
            v.backend.injected().is_empty(),
            "an unwired LAPIC never drives an injection"
        );
    }

    // -----------------------------------------------------------------------
    // Serial COM1 (IRQ 4) injection (task 33): the 8250 THRE interrupt drives
    // `set_pending_irq(0x34)` so the kernel's interrupt-driven userspace TX
    // drains. Edge-driven by the guest's IER write + gated by the 8259 mask.
    // -----------------------------------------------------------------------

    /// Unmask IRQ 4 in the 8259 master IMR (port 0x21) — the state after the kernel
    /// `request_irq(4)`s ttyS0 (every other line left masked).
    const UNMASK_IRQ4: Exit<X86> = Exit::Arch(X86Exit::Io {
        port: 0x0021,
        size: 1,
        write: Some(0xEF),
    }); // 0xFF & !(1 << 4)
    /// Enable IER.THRI (port 0x3F9, IER = 0x3F8+1) — the kernel's `start_tx`.
    const ENABLE_THRI: Exit<X86> = Exit::Arch(X86Exit::Io {
        port: 0x03F9,
        size: 1,
        write: Some(0x02),
    });

    #[test]
    fn serial_thre_interrupt_injects_com1_vector() {
        // The Linux userspace TX path: the guest unmasks IRQ 4 in the 8259 and
        // enables IER.THRI; the VMM then injects the COM1 vector (0x34) so the
        // kernel's IRQ-4 handler can drain the TX. Deterministic (edge-driven by the
        // IER write, no V-time), so it works on the deterministic backend at work 0.
        let mut mock = configured_mock(vec![
            UNMASK_IRQ4,
            ENABLE_THRI,
            Exit::Common(CommonExit::Idle),
        ]);
        mock.set_defer_accept(true); // hold the injection so the pending slot is observable
        let mut v = lapic_vmm(mock, Box::new(ScriptedWork::at(0)));

        // Step 1 runs the IMR unmask; THRE not enabled yet → nothing pending.
        assert!(matches!(v.step().unwrap(), Step::Continued));
        assert_eq!(
            v.backend.pending_irq(),
            None,
            "no THRE interrupt before IER.THRI"
        );
        // Step 2 runs the IER=THRI write (service ran before it, so still None).
        assert!(matches!(v.step().unwrap(), Step::Continued));
        // Step 3: service sees THRE asserted + IRQ 4 unmasked → injects 0x34.
        assert!(matches!(
            v.step().unwrap(),
            Step::Terminal(TerminalReason::Idle)
        ));
        assert_eq!(
            v.backend.pending_irq(),
            Some(COM1_IRQ_VECTOR),
            "the THRE interrupt is injected on the legacy COM1 vector 0x34"
        );
        assert_eq!(COM1_IRQ_VECTOR, 0x34, "ISA_IRQ_VECTOR(4) = 0x30 + 4");
    }

    #[test]
    fn serial_irq_suppressed_while_8259_masks_it() {
        // THRE enabled but IRQ 4 still masked in the 8259 (reset IMR = all-masked):
        // no injection — the VMM honors the PIC mask (e.g. while the kernel's handler
        // runs with the line masked), so a masked line is never re-injected.
        let mut mock = configured_mock(vec![ENABLE_THRI, Exit::Common(CommonExit::Idle)]);
        mock.set_defer_accept(true);
        let mut v = lapic_vmm(mock, Box::new(ScriptedWork::at(0)));
        assert!(matches!(v.step().unwrap(), Step::Continued)); // IER = THRI
        assert!(matches!(
            v.step().unwrap(),
            Step::Terminal(TerminalReason::Idle)
        ));
        assert_eq!(
            v.backend.pending_irq(),
            None,
            "a masked COM1 line is not injected even with THRE asserted"
        );
    }

    #[test]
    fn lapic_vector_outranks_the_serial_line() {
        // With both a deliverable LAPIC timer vector (0x40) and the serial line
        // (0x34) pending, the single backend slot gets the higher-priority LAPIC
        // vector (`lapic_vector.or(serial)`), not the legacy ExtINT line.
        const W: u64 = 100_000_000;
        let mut exits = arm_timer_exits(1000);
        exits.push(UNMASK_IRQ4);
        exits.push(ENABLE_THRI);
        exits.push(Exit::Arch(X86Exit::Rdtsc)); // advance the anchor → the timer fires into IRR
        exits.push(Exit::Common(CommonExit::Idle));
        let mut mock = configured_mock(exits);
        mock.set_defer_accept(true);
        let mut v = lapic_vmm(mock, Box::new(ScriptedWork::at(W)));
        v.run().expect("run");
        assert_eq!(
            v.backend.pending_irq(),
            Some(0x40),
            "the LAPIC timer vector outranks the legacy serial ExtINT line"
        );
    }

    #[test]
    fn serial_acceptance_takes_no_lapic_isr_transition() {
        // An accepted serial vector is EOI'd at the 8259, not the userspace LAPIC, so
        // it leaves the LAPIC ISR empty (no IRR→ISR transition). Read the ISR bank for
        // 0x34 after acceptance to confirm it is clear.
        let mut exits = vec![UNMASK_IRQ4, ENABLE_THRI];
        exits.push(read_mmio(isr_gpa(COM1_IRQ_VECTOR))); // accepted before this exit
        exits.push(Exit::Common(CommonExit::Idle));
        // Default mock accepts at run (not deferred).
        let mut v = lapic_vmm(configured_mock(exits), Box::new(ScriptedWork::at(0)));
        v.run().expect("run");
        let isr = *read_completions(&v).last().expect("ISR read");
        assert_eq!(
            isr & (1 << (u32::from(COM1_IRQ_VECTOR) % 32)),
            0,
            "the serial vector never enters the LAPIC ISR (EOI'd at the 8259)"
        );
    }

    /// ISR/IRR MMIO address for vector `v`: bank `v/32`, the read returns the
    /// 32-bit bank word whose bit `v%32` reflects the vector.
    fn isr_gpa(v: u8) -> Gpa {
        Gpa(APIC_MMIO_BASE + u64::from(lapic::APIC_ISR) + u64::from(v / 32) * 0x10)
    }
    fn irr_gpa(v: u8) -> Gpa {
        Gpa(APIC_MMIO_BASE + u64::from(lapic::APIC_IRR) + u64::from(v / 32) * 0x10)
    }
    fn read_mmio(gpa: Gpa) -> Exit<X86> {
        Exit::Common(CommonExit::Mmio {
            gpa,
            size: 4,
            write: None,
        })
    }

    /// The `Completion::Read` values the mock recorded, in order (the resolved
    /// MMIO-load / RDTSC values).
    fn read_completions(v: &Vmm<MockBackend>) -> Vec<u64> {
        v.backend
            .completions()
            .iter()
            .filter_map(|c| match c {
                Completion::Read(x) => Some(*x),
                _ => None,
            })
            .collect()
    }

    fn lapic_vmm(mock: MockBackend, work: Box<dyn WorkSource>) -> Vmm<MockBackend> {
        let mut v = Vmm::new(mock, GuestRam::new(0x1000).unwrap());
        v.wire_vtime(VtimeWiring::new(contract_vclock_config(), work, 1).unwrap());
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        v
    }

    #[test]
    fn injected_vector_stays_in_irr_until_accepted() {
        // [blocking review #1] The LAPIC IRR→ISR transition must NOT happen until
        // the backend accepts the vector. With acceptance deferred (modelling the
        // interrupt-window wait), a guest APIC read sees vector 0x40 **pending in
        // IRR** and **not in service** — so a snapshot/hash in that window is correct.
        const W: u64 = 100_000_000;
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Arch(X86Exit::Rdtsc)); // advance the anchor so the timer fires next service
        exits.push(read_mmio(irr_gpa(0x40))); // IRR bank for vec 0x40
        exits.push(read_mmio(isr_gpa(0x40))); // ISR bank for vec 0x40
        exits.push(Exit::Common(CommonExit::Idle));
        let mut mock = configured_mock(exits);
        mock.set_defer_accept(true); // never accept → vector stays pending
        let mut v = lapic_vmm(mock, Box::new(ScriptedWork::at(W)));

        v.run().expect("run");
        // Completions, in order: RDTSC value, then the IRR read, then the ISR read.
        let reads: Vec<u64> = v
            .backend
            .completions()
            .iter()
            .filter_map(|c| match c {
                Completion::Read(x) => Some(*x),
                _ => None,
            })
            .collect();
        // Last two reads are IRR then ISR.
        let isr = *reads.last().expect("ISR read");
        let irr = reads[reads.len() - 2];
        assert_eq!(irr & 1, 1, "vector 0x40 is pending in IRR while deferred");
        assert_eq!(
            isr & 1,
            0,
            "vector 0x40 is NOT in service before acceptance"
        );
    }

    #[test]
    fn accepted_vector_moves_irr_to_isr() {
        // Complement of the deferral test: once the backend accepts the vector, the
        // VMM completes the IRR→ISR transition, so a guest ISR read sees it in
        // service and the IRR bit cleared.
        const W: u64 = 100_000_000;
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Arch(X86Exit::Rdtsc));
        exits.push(read_mmio(irr_gpa(0x40)));
        exits.push(read_mmio(isr_gpa(0x40)));
        exits.push(Exit::Common(CommonExit::Idle));
        // Default mock accepts at run (not deferred).
        let mut v = lapic_vmm(configured_mock(exits), Box::new(ScriptedWork::at(W)));

        v.run().expect("run");
        let reads: Vec<u64> = v
            .backend
            .completions()
            .iter()
            .filter_map(|c| match c {
                Completion::Read(x) => Some(*x),
                _ => None,
            })
            .collect();
        let isr = *reads.last().expect("ISR read");
        let irr = reads[reads.len() - 2];
        assert_eq!(irr & 1, 0, "IRR bit cleared once the vector is accepted");
        assert_eq!(isr & 1, 1, "vector 0x40 is in service after acceptance");
    }

    #[test]
    fn lapic_now_vns_fails_closed_on_work_error() {
        // [review #3] A work-counter read error must fail-closed (`VmmError::Work`),
        // not silently reuse a stale anchor (which would freeze/shift the timer).
        struct FailingWork;
        impl WorkSource for FailingWork {
            fn work(&self) -> Result<u64, WorkError> {
                Err(WorkError::Untrustworthy("test-induced"))
            }
            fn reset(&mut self) -> Result<(), WorkError> {
                Ok(())
            }
        }
        // Stock caps so `lapic_now_vns` reads the live work counter (the failing one).
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Common(CommonExit::Idle));
        let mut v = lapic_vmm(configured_stock_mock(exits), Box::new(FailingWork));

        // The first step's `service_pending_irqs` reads the work counter → error.
        let err = v.step().unwrap_err();
        assert!(
            matches!(err, VmmError::Work(_)),
            "a work-counter error must fail closed, got {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Deterministic HLT-resume (task 52): discriminate idle-HLT from terminal-
    // HLT (RFLAGS.IF + armed timer), and on a resumable idle warp V-time to the
    // deadline (the jump) instead of terminating. Mock-driven; the end-to-end
    // box proof is the task-48 `live_runc_postgres` gate (foreman).
    // -----------------------------------------------------------------------

    /// A vCPU state with `RFLAGS.IF` (interrupt-enable) set — the guest is
    /// waiting for an interrupt it can take (`0x2` is the always-1 reserved bit).
    fn if_set_state() -> VcpuState {
        VcpuState {
            regs: vmm_backend::VcpuRegs {
                rflags: RFLAGS_IF | 0x2,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn idle_hlt_with_if_and_armed_timer_jumps_to_deadline_and_fires() {
        // The headline path: a guest that idles (HLT) with IF==1 while a one-shot
        // LAPIC timer is armed is NOT terminal — V-time jumps to the timer
        // deadline, the timer fires + is delivered, and the run continues. The jump
        // rebases the work epoch (resets the counter, folds the landing into vns_base
        // at anchor 0) and fabricates ZERO retired branches AND does NOT read the live
        // (skid-tainted) HLT work counter.
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Common(CommonExit::Idle)); // the guest idles, waiting for the timer
        exits.push(read_mmio(isr_gpa(0x40))); // after delivery: 0x40 in service
        exits.push(Exit::Common(CommonExit::Idle)); // one-shot fired → no timer armed → terminal
        let mut mock = configured_mock(exits);
        mock.set_state(if_set_state());
        let mut v = lapic_vmm(mock, Box::new(ScriptedWork::at(0)));

        // Arm the timer (3 MMIO writes); the anchor stays 0, no timer fires yet.
        for _ in 0..3 {
            assert!(matches!(v.step().unwrap(), Step::Continued));
        }
        let deadline = v.preemption_deadline().expect("timer armed").0;
        assert!(deadline > 0, "a future timer deadline");

        // The idle HLT resumes instead of terminating.
        assert!(
            matches!(v.step().unwrap(), Step::Continued),
            "idle HLT resumes"
        );
        // The trace records the landed V-time (the deadline), skid-free.
        assert_eq!(
            v.idle_landings(),
            &[deadline],
            "the idle resume records the landed V-time (the deadline)"
        );
        // Epoch rebase: the anchor resets to 0, the effective V-time is exactly the
        // deadline, and the point is a clean synchronized boundary (no skid read).
        let vt = v.vtime.as_ref().unwrap();
        assert_eq!(
            vt.last_intercept_work, 0,
            "the work epoch is rebased (anchor 0)"
        );
        assert!(
            v.vtime_synchronized,
            "post-rebase the effective V-time is exactly the deadline (synchronized)"
        );
        assert_eq!(
            v.vtime.as_ref().unwrap().clock.snapshot_vns(0),
            deadline,
            "V-time jumped to D (folded into vns_base at the rebased anchor 0)"
        );

        // Next step: the timer fires off the warped anchor and is delivered.
        assert!(matches!(v.step().unwrap(), Step::Continued));
        assert_eq!(
            *read_completions(&v).last().expect("ISR read") & 1,
            1,
            "the timer vector is in service after the idle jump"
        );
        // The final HLT (one-shot already fired → no armed timer) is terminal.
        assert!(matches!(
            v.step().unwrap(),
            Step::Terminal(TerminalReason::Idle)
        ));
    }

    #[test]
    fn idle_hlt_before_a_staged_arrival_wakes_at_the_arrival_not_the_timer() {
        // PR #51 round-4: a guest that idles (HLT, IF=1) with its LAPIC timer armed
        // BEYOND a staged host-fault arrival must jump to the ARRIVAL `Moment` (so the
        // fault applies there), NOT sail past it to the timer. The idle jump routes
        // through the same min-fold as `run_until_deadline`.
        let mut exits = arm_timer_exits(1_000_000); // a FAR one-shot timer deadline
        exits.push(Exit::Common(CommonExit::Idle)); // the guest idles before the arrival
        let mut mock = configured_mock(exits);
        mock.set_state(if_set_state());
        let mut v = lapic_vmm(mock, Box::new(ScriptedWork::at(0)));
        for _ in 0..3 {
            assert!(matches!(v.step().unwrap(), Step::Continued)); // arm the timer
        }
        let timer_vns = v.armed_timer_deadline_vns().expect("timer armed");
        let m = timer_vns / 2; // an arrival strictly before the timer
        assert!(
            m > 0 && m < timer_vns,
            "arrival is a future point before the timer"
        );
        assert!(v.arm_arrival(m), "arms the arrival");

        // The idle HLT jumps to the ARRIVAL, not the (far) timer.
        assert!(matches!(v.step().unwrap(), Step::Continued), "idle resumes");
        assert_eq!(
            v.idle_landings(),
            &[m],
            "V-time jumped to the arrival Moment, not the far timer"
        );
        assert_eq!(
            v.effective_vns(),
            Some(m),
            "effective V-time is exactly the arrival Moment (the fault can apply here)"
        );
    }

    #[test]
    fn idle_hlt_with_no_timer_wakes_at_a_staged_arrival() {
        // A staged arrival is a wake event in its own right: a guest that idles
        // (HLT, IF=1) with NO timer but a staged host fault wakes at the arrival
        // `Moment` (so e.g. a host-injected interrupt lands there) rather than being
        // declared terminal.
        let mut v = lapic_vmm(
            {
                let mut m = configured_mock(vec![Exit::Common(CommonExit::Idle)]);
                m.set_state(if_set_state());
                m
            },
            Box::new(ScriptedWork::at(0)),
        );
        assert!(
            v.armed_timer_deadline_vns().is_none(),
            "no timer armed in this test"
        );
        assert!(v.arm_arrival(4242), "arms the arrival");
        assert!(
            matches!(v.step().unwrap(), Step::Continued),
            "the idle HLT wakes at the arrival instead of terminating"
        );
        assert_eq!(v.idle_landings(), &[4242]);
        assert_eq!(v.effective_vns(), Some(4242));
    }

    #[test]
    fn idle_hlt_with_no_lapic_wakes_at_a_staged_arrival() {
        // PR #51 round-6 item 2: a host-fault arrival is a host-plane event, not a
        // guest interrupt, so it wakes the idle jump **independent of the LAPIC gate**.
        // A V-time-wired guest with NO LAPIC that idles (HLT, IF=1) before a staged
        // `Moment` must still wake at the arrival to apply it — otherwise it goes
        // Terminal and silently never applies an accepted perturb.
        let mut mock = configured_mock(vec![Exit::Common(CommonExit::Idle)]);
        mock.set_state(if_set_state());
        let mut v = Vmm::new(mock, GuestRam::new(0x1000).unwrap());
        v.wire_vtime(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(0)), 1).unwrap(),
        );
        // NO `wire_lapic` — a deterministic V-time backend without a userspace xAPIC.
        assert!(!v.lapic_wired(), "no LAPIC in this test");
        assert!(v.can_arm_arrival(), "V-time + deterministic ⇒ armable");
        assert!(v.arm_arrival(3333), "arms the arrival");
        assert!(
            matches!(v.step().unwrap(), Step::Continued),
            "the idle HLT wakes at the arrival even with no LAPIC"
        );
        assert_eq!(v.idle_landings(), &[3333]);
        assert_eq!(v.effective_vns(), Some(3333));
    }

    #[test]
    fn idle_hlt_without_if_is_terminal() {
        // IF==0 (the kernel's final `cli; hlt`): terminal even with a timer armed
        // — a wait nothing will satisfy. The byte-identical existing behavior.
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Common(CommonExit::Idle));
        // Default mock state: rflags == 0 (IF clear).
        let mut v = lapic_vmm(configured_mock(exits), Box::new(ScriptedWork::at(1000)));
        let r = v.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Idle);
        assert!(
            v.idle_landings().is_empty(),
            "an IF==0 HLT is terminal, never resumed"
        );
    }

    #[test]
    fn hlt_without_armed_timer_is_terminal_even_with_if() {
        // IF==1 but no timer armed (LAPIC wired, never programmed): terminal. The
        // no-timer gate short-circuits before the RFLAGS read.
        let mut mock = configured_mock(vec![Exit::Common(CommonExit::Idle)]);
        mock.set_state(if_set_state());
        let mut v = lapic_vmm(mock, Box::new(ScriptedWork::at(0)));
        let r = v.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Idle);
        assert!(v.idle_landings().is_empty(), "no armed timer ⇒ terminal");
    }

    #[test]
    fn idle_hlt_on_stock_backend_is_terminal() {
        // Stock backend (no deterministic counter): never idle-resumes, even with
        // IF==1 and a timer armed — the determinism gate (deterministic_tsc)
        // returns no deadline, so the HLT stays terminal (Phase B.1 unchanged).
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Common(CommonExit::Idle));
        let mut mock = configured_stock_mock(exits);
        mock.set_state(if_set_state());
        let mut v = lapic_vmm(mock, Box::new(ScriptedWork::at(0)));
        let r = v.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Idle);
        assert!(
            v.idle_landings().is_empty(),
            "a non-deterministic backend never idle-resumes"
        );
    }

    #[test]
    fn preemption_deadline_is_none_off_the_determinism_path() {
        // `preemption_deadline` / `armed_timer_deadline_vns` gate run_until on the
        // determinism path: V-time wired AND a deterministic counter. A stock backend (no
        // `deterministic_tsc`) with a LAPIC timer ARMED must still return None — run_until
        // is determinism-only — so the gate is `vtime.is_none() || !deterministic_tsc`, not
        // `&&` (which would wrongly run_until on stock).
        let mut v = lapic_vmm(
            configured_stock_mock(arm_timer_exits(1000)),
            Box::new(ScriptedWork::at(0)),
        );
        for _ in 0..3 {
            assert!(matches!(v.step().unwrap(), Step::Continued)); // arm the timer
        }
        // The timer IS armed (a LAPIC query, backend-independent)...
        assert!(
            v.devices
                .lapic
                .as_ref()
                .unwrap()
                .next_timer_deadline()
                .is_some(),
            "the LAPIC timer is armed",
        );
        // ...yet preemption_deadline is None on the non-deterministic backend.
        assert!(
            v.preemption_deadline().is_none(),
            "a non-deterministic backend never uses run_until (preemption_deadline is None)"
        );
    }

    #[test]
    fn idle_hlt_with_undeliverable_timer_is_terminal() {
        // Robustness (review P2): an ARMED but UNDELIVERABLE timer at a HLT(IF==1) must be
        // TERMINAL, not a resumable idle. Jumping would fire the timer into the IRR but
        // peek_interrupt returns None (no deliverable vector), so nothing injects and a
        // one-shot leaves no future wake — the vCPU would be stuck warping V-time. Treat
        // it like IF==0: terminate, do NOT advance V-time or re-enter. (Deterministic, not
        // a determinism bug; Linux's timer is deliverable so runc/Postgres are unaffected
        // — this hardens the keystone against adversarial guests.)
        let w = |off: u64, val: u64| {
            Exit::Common(CommonExit::Mmio {
                gpa: Gpa(APIC_MMIO_BASE + off),
                size: 4,
                write: Some(val),
            })
        };
        let undeliverable_timer_hlt_terminates = |setup: Vec<Exit<X86>>| {
            let mut exits = setup;
            exits.push(Exit::Common(CommonExit::Idle));
            let mut mock = configured_mock(exits);
            mock.set_state(if_set_state());
            let mut v = lapic_vmm(mock, Box::new(ScriptedWork::at(0)));
            let r = v.run().expect("run");
            assert_eq!(
                r.reason,
                TerminalReason::Idle,
                "an armed-but-undeliverable timer HLT is terminal"
            );
            assert!(
                v.idle_landings().is_empty(),
                "no idle resume / no V-time advance for an undeliverable timer"
            );
        };

        // (a) Reserved vector (< 16): armed (next_timer_deadline is Some) but the vector
        //     can never be delivered (SDM §11.5.3).
        undeliverable_timer_hlt_terminates(vec![
            w(u64::from(lapic::APIC_SVR), 0x1FF),
            w(u64::from(lapic::APIC_LVT_TIMER), 0x05), // one-shot, unmasked, RESERVED vec 5
            w(u64::from(lapic::APIC_TMICT), 1000),
        ]);
        // (b) Valid vector but masked by a raised TPR (class 0xF outranks the timer's
        //     class 4): armed and would fire into the IRR, but peek_interrupt returns None.
        undeliverable_timer_hlt_terminates(vec![
            w(u64::from(lapic::APIC_SVR), 0x1FF),
            w(u64::from(lapic::APIC_LVT_TIMER), 0x40), // one-shot, unmasked, vec 0x40 (class 4)
            w(u64::from(lapic::APIC_TMICT), 1000),
            w(u64::from(lapic::APIC_TPR), 0xF0), // TPR class 15 masks the timer vector
        ]);
    }

    #[test]
    fn idle_discriminator_save_error_fails_closed() {
        // The RFLAGS read for the idle/terminal discriminator is a backend save;
        // a save error must fail closed (VmmError::Backend), never guess the
        // disposition (which would risk a wrong terminate/resume).
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Common(CommonExit::Idle));
        let mut inner = configured_mock(exits);
        inner.set_state(if_set_state()); // irrelevant — save() fails before the read
        let mut v = Vmm::new(SaveFailBackend(inner), GuestRam::new(0x1000).unwrap());
        v.wire_vtime(
            VtimeWiring::new(
                contract_vclock_config(),
                Box::new(ScriptedWork::at(1000)),
                1,
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

        // Arm the timer (no save() on this path), then hit the idle HLT.
        for _ in 0..3 {
            assert!(matches!(v.step().unwrap(), Step::Continued));
        }
        let err = v.step().unwrap_err();
        assert!(
            matches!(err, VmmError::Backend(_)),
            "a save error during the idle discriminator must fail closed, got {err:?}"
        );
    }

    #[test]
    fn periodic_timer_idle_loop_advances_vtime_each_tick() {
        // Gate-4 corollary mechanism: a guest that stays idle while a PERIODIC
        // timer ticks makes V-time progress on its own — each idle resume warps
        // to the next period and re-arms, WITHOUT executing. This is what lets
        // timer-driven waits (nanosleep/futex-timeout) wake up (they froze before
        // task 52). The guest never executes between ticks, yet V-time advances by a
        // period between the two idle resumes — pure event-driven clock progress.
        // Periodic one-shot→periodic: LVT vector 0x40 with mode bit 17 set.
        let w = |off: u64, val: u64| {
            Exit::Common(CommonExit::Mmio {
                gpa: Gpa(APIC_MMIO_BASE + off),
                size: 4,
                write: Some(val),
            })
        };
        let exits = vec![
            w(u64::from(lapic::APIC_SVR), 0x1FF),
            w(u64::from(lapic::APIC_LVT_TIMER), 0x40 | (1 << 17)), // periodic
            w(u64::from(lapic::APIC_TMICT), 1000),
            Exit::Common(CommonExit::Idle),   // idle #1
            w(u64::from(lapic::APIC_EOI), 0), // the timer ISR EOIs (retires 0x40 from ISR)
            Exit::Common(CommonExit::Idle),   // idle #2 (re-armed period; deliverable again)
        ];
        let mut mock = configured_mock(exits);
        mock.set_state(if_set_state());
        // Each idle resume rebases the work epoch (anchor→0), so the clock at work 0 reads
        // the landed V-time. The work source is never consulted by the idle path.
        let mut v = lapic_vmm(mock, Box::new(ScriptedWork::at(0)));

        for _ in 0..3 {
            assert!(matches!(v.step().unwrap(), Step::Continued));
        }
        let period = v.preemption_deadline().expect("armed").0;

        // Idle #1 → jump to one period (V-time at the rebased anchor 0 reads `period`).
        assert!(matches!(v.step().unwrap(), Step::Continued));
        let after_1 = v.vtime.as_ref().unwrap().clock.snapshot_vns(0);
        assert_eq!(after_1, period, "idle #1 warped to the first period");
        // Fire + re-arm (the periodic reload).
        assert!(matches!(v.step().unwrap(), Step::Continued));
        // Idle #2 → jump to the next period, still without the guest executing.
        assert!(matches!(v.step().unwrap(), Step::Continued));
        let after_2 = v.vtime.as_ref().unwrap().clock.snapshot_vns(0);
        assert_eq!(
            after_2,
            period.saturating_mul(2),
            "idle #2 warped to the second period — V-time progressed by a tick \
             with no guest execution"
        );
        assert_eq!(
            v.idle_landings(),
            &[period, period.saturating_mul(2)],
            "two idle resumes record the successive V-time landings (one tick apart)"
        );
    }

    #[test]
    fn pending_irr_then_sti_hlt_resumes_and_delivers_not_terminal() {
        // Review P1: a one-shot timer that ALREADY fired into the IRR (its deadline hit
        // while IF==0), then `sti; hlt` — `next_timer_deadline()` is None but a deliverable
        // vector is pending in the IRR. The discriminator must RESUME (zero V-time advance)
        // and deliver it, NOT terminate (a normal Linux pattern: timer fires in a critical
        // section, then idle). With defer_accept the fired vector is held in the IRR
        // (un-accepted) across the HLT, modelling the IF==0 window.
        const W: u64 = 100_000_000; // past the timer deadline → the timer fires into IRR
        let mut exits = arm_timer_exits(1000); // one-shot, vector 0x40
        exits.push(Exit::Arch(X86Exit::Rdtsc)); // advance the anchor past the deadline → timer fires into IRR
        exits.push(Exit::Common(CommonExit::Idle)); // sti; hlt with the fired vector still pending, no future timer
        exits.push(read_mmio(isr_gpa(0x40))); // after delivery: 0x40 in service
        exits.push(Exit::Common(CommonExit::Idle)); // terminal (vector delivered, no timer armed)
        let mut mock = configured_mock(exits);
        mock.set_state(if_set_state());
        mock.set_defer_accept(true); // hold the fired vector in the IRR across the HLT
        let mut v = lapic_vmm(mock, Box::new(ScriptedWork::at(W)));

        // Arm (3) + RDTSC (advances the anchor past the deadline). The one-shot fires into
        // the IRR at the NEXT service — i.e. at the top of the HLT step below.
        for _ in 0..4 {
            assert!(matches!(v.step().unwrap(), Step::Continued));
        }
        let vns_before = v.vtime.as_ref().unwrap().clock.snapshot_vns(W);

        // The HLT step: its service fires the one-shot into the IRR (anchor past the
        // deadline → disarmed), then on_idle sees a pending deliverable vector with NO
        // future deadline → DeliverPending. It must RESUME (not terminate).
        assert!(
            matches!(v.step().unwrap(), Step::Continued),
            "a pending deliverable IRR vector wakes the HLT (not terminal)"
        );
        // The one-shot has fired and disarmed — there is no future deadline; the resume
        // was driven purely by the pending IRR vector.
        assert!(
            v.preemption_deadline().is_none(),
            "the one-shot fired/disarmed: the wake came from the pending IRR, not a deadline"
        );
        // Zero V-time advance: pending-now delivery does NOT jump the clock or record a
        // landing (the clock is unchanged; no idle warp).
        assert!(
            v.idle_landings().is_empty(),
            "pending-now delivery is a zero-advance resume (no idle landing recorded)"
        );
        assert_eq!(
            v.vtime.as_ref().unwrap().clock.snapshot_vns(W),
            vns_before,
            "the clock is unchanged by a pending-now resume"
        );

        // Allow acceptance; the next service injects the pending vector → in service.
        v.backend.set_defer_accept(false);
        assert!(matches!(v.step().unwrap(), Step::Continued)); // ISR read
        assert_eq!(
            *read_completions(&v).last().expect("ISR read") & 1,
            1,
            "the pending vector is delivered after the resume"
        );
        assert!(matches!(
            v.step().unwrap(),
            Step::Terminal(TerminalReason::Idle)
        ));
    }

    #[test]
    fn idle_jump_rebases_work_epoch_so_next_tick_is_future() {
        // Review P2 (work-axis epoch): after an idle jump the work counter must be REBASED
        // (reset), so post-idle deadline→work conversions count from the new epoch and the
        // next tick lands a FULL period in the future — not overdue. Reproduce the
        // stale-anchor scenario (a low anchor, then the guest retires many branches before
        // idling) and assert (1) the jump resets the work counter and (2) the next periodic
        // deadline converts to a future work count.
        let cell = std::rc::Rc::new(Cell::new(0u64));
        let work = Box::new(SharedWork(cell.clone()));
        let w = |off: u64, val: u64| {
            Exit::Common(CommonExit::Mmio {
                gpa: Gpa(APIC_MMIO_BASE + off),
                size: 4,
                write: Some(val),
            })
        };
        let exits = vec![
            w(u64::from(lapic::APIC_SVR), 0x1FF),
            w(u64::from(lapic::APIC_LVT_TIMER), 0x40 | (1 << 17)), // periodic
            w(u64::from(lapic::APIC_TMICT), 1000),
            Exit::Common(CommonExit::Idle), // idle #1 (with a STALE anchor + high live work)
            w(u64::from(lapic::APIC_EOI), 0), // timer ISR EOIs
            Exit::Common(CommonExit::Idle), // idle #2 (full period later)
        ];
        let mut mock = configured_mock(exits);
        mock.set_state(if_set_state());
        let mut v = lapic_vmm(mock, work);

        for _ in 0..3 {
            assert!(matches!(v.step().unwrap(), Step::Continued)); // arm; anchor 0
        }
        let period = v.preemption_deadline().expect("armed").0;
        // The guest retires MANY branches before idling: the cumulative counter is well
        // past the (stale) anchor AND past the deadline's work — the pre-fix over-count.
        cell.set(period + 12_345);

        // Idle #1.
        assert!(matches!(v.step().unwrap(), Step::Continued));
        // (1) The epoch is REBASED — the work counter resets to 0 (the pre-idle branches
        //     are absorbed into the jump). The pre-fix code (no rebase) left it unchanged.
        assert_eq!(
            cell.get(),
            0,
            "the idle jump rebases the work epoch (counter reset to 0)"
        );
        assert_eq!(v.idle_landings(), &[period]);

        // Simulate a few handler branches in the new epoch, then fire + re-arm the tick.
        cell.set(50);
        assert!(matches!(v.step().unwrap(), Step::Continued)); // fires periodic, re-arms to 2·period
        // (2) The next deadline converts to a FUTURE work count (a full period ahead),
        //     NOT overdue — the cadence is preserved across the idle.
        let next = v.preemption_deadline().expect("re-armed").0;
        assert!(
            next > cell.get(),
            "post-idle deadline→work is in the future ({next}), not overdue vs live work {}",
            cell.get()
        );
        assert_eq!(
            next.saturating_sub(0),
            period,
            "the next tick is exactly one period ahead in the rebased epoch"
        );

        // Idle #2 lands a full period after idle #1 (cadence preserved).
        cell.set(60);
        assert!(matches!(v.step().unwrap(), Step::Continued));
        assert_eq!(
            v.idle_landings(),
            &[period, period.saturating_mul(2)],
            "tick cadence preserved across the idle (a full period per tick)"
        );
    }

    #[test]
    fn idle_resume_is_immune_to_hlt_work_skid() {
        // Closes the portable/SimCpu blind spot (task-52 review): the live work counter
        // is SKID-TAINTED at a HLT (task-27 box O1 — a non-V-time-intercept live read
        // diverges across same-seed runs), so the idle path must NEVER fold it into
        // V-time. Model that skid directly — two same-seed runs identical EXCEPT the work
        // read AT THE HLT differs — and assert deterministic-twice (bit-identical
        // state_hash), measured after the NEXT skid-free intercept (W_next), which is
        // exactly where a folded skid term would surface (it does not cancel against the
        // skid-free W_next). With the pre-fix code that read work() at the HLT, the two
        // runs diverge here; with the intercept-aligned fix they are identical.
        fn run_with_hlt_skid(hlt_skid: u64) -> [u8; 32] {
            let cell = std::rc::Rc::new(Cell::new(0u64));
            let work = Box::new(SharedWork(cell.clone()));
            let mut exits = arm_timer_exits(1000);
            exits.push(Exit::Arch(X86Exit::Rdtsc)); // intercept → skid-free anchor (identical both runs)
            exits.push(Exit::Common(CommonExit::Idle)); // idle: the live work read here is skid-tainted
            exits.push(Exit::Arch(X86Exit::Rdtsc)); // W_next intercept — where a folded skid would surface
            exits.push(Exit::Common(CommonExit::Idle)); // terminal (one-shot already fired)
            let mut mock = configured_mock(exits);
            mock.set_state(if_set_state());
            let mut v = lapic_vmm(mock, work);

            for _ in 0..3 {
                assert!(matches!(v.step().unwrap(), Step::Continued)); // arm
            }
            cell.set(1000); // RDTSC anchor read — skid-free, identical both runs
            assert!(matches!(v.step().unwrap(), Step::Continued)); // RDTSC → anchor 1000
            cell.set(1000 + hlt_skid); // the HLT live read — skid-perturbed per run
            assert!(matches!(v.step().unwrap(), Step::Continued)); // idle resume
            cell.set(2000); // W_next anchor read — skid-free, identical both runs
            // Drive to terminal (timer fires, the W_next RDTSC lands, terminal HLT).
            let reason = loop {
                if let Step::Terminal(r) = v.step().unwrap() {
                    break r;
                }
            };
            assert_eq!(reason, TerminalReason::Idle);
            v.state_hash()
        }

        assert_eq!(
            run_with_hlt_skid(0),
            run_with_hlt_skid(5),
            "the HLT live work read is skid-tainted; folding it would diverge state_hash \
             at W_next — the idle path must ignore it (deterministic-twice across HLT skid)"
        );
    }

    // -----------------------------------------------------------------------
    // Full vm_state snapshot / restore / branch (task 39). Mock-driven; the
    // live box gate is tests/live_snapshot_branch.rs.
    // -----------------------------------------------------------------------

    /// A `Vmm<MockBackend>` with V-time + the Linux platform (xAPIC + legacy I/O)
    /// all wired — the full surface `save_vm_state` captures.
    fn full_vmm(
        state: VcpuState,
        exits: Vec<Exit<X86>>,
        work_at: u64,
        seed: u64,
    ) -> Vmm<MockBackend> {
        let mut m = configured_mock(exits);
        m.set_state(state);
        let mut v = Vmm::new(m, GuestRam::new(0x2000).unwrap());
        v.wire_vtime(
            VtimeWiring::new(
                contract_vclock_config(),
                Box::new(ScriptedWork::at(work_at)),
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
        v
    }

    /// A non-trivial but quiescent-point-representable `VcpuState` (the dropped
    /// `kvm_vcpu_events` injection bookkeeping and `kvm_sregs2` flags/pdptrs are
    /// zero, as they are after an exit is fully serviced).
    fn nonzero_state() -> VcpuState {
        let mut msrs = std::collections::BTreeMap::new();
        msrs.insert(0xC000_0080u32, 0x501);
        VcpuState {
            regs: vmm_backend::VcpuRegs {
                rax: 0x1111,
                rbx: 0x2222,
                rip: 0x10_0000,
                rsp: 0x8000,
                rflags: 0x2,
                ..Default::default()
            },
            sregs: vmm_backend::VcpuSregs {
                cs: vmm_backend::Segment {
                    selector: 0x10,
                    limit: 0xFFFF_FFFF,
                    type_: 0xB,
                    present: 1,
                    s: 1,
                    l: 1,
                    g: 1,
                    ..Default::default()
                },
                cr0: 0x8000_0011,
                cr3: 0x1000,
                cr4: 0x20,
                efer: 0x500,
                apic_base: 0xFEE0_0900,
                ..Default::default()
            },
            xcr0: 0x7,
            msrs,
            xsave: (0u16..512).map(|i| i as u8).collect(),
            ..Default::default()
        }
    }

    /// The exits that drive the device + V-time + entropy state into a non-default,
    /// clean (post-RDTSC, no staged RNG) configuration: WRMSR TSC_ADJUST, an xAPIC
    /// TPR write, a PIC IMR unmask, a serial byte, one RDRAND, then an RDTSC.
    fn mutate_exits() -> Vec<Exit<X86>> {
        vec![
            Exit::Arch(X86Exit::Wrmsr {
                index: 0x3b,
                value: 0x1234,
            }),
            Exit::Common(CommonExit::Mmio {
                gpa: Gpa(0xFEE0_0080),
                size: 4,
                write: Some(0x20),
            }), // TPR = 0x20
            Exit::Arch(X86Exit::Io {
                port: 0x0021,
                size: 1,
                write: Some(0xEF),
            }), // PIC master IMR
            Exit::Arch(X86Exit::Io {
                port: 0x3F8,
                size: 1,
                write: Some(u32::from(b'H')),
            }), // serial 'H'
            Exit::Arch(X86Exit::Rdrand { width: 8 }), // advance the entropy stream
            Exit::Arch(X86Exit::Rdtsc), // V-time intercept → clean, synchronized boundary
        ]
    }

    fn step_n(v: &mut Vmm<MockBackend>, n: usize) {
        for _ in 0..n {
            assert_eq!(v.step().unwrap(), Step::Continued);
        }
    }

    #[test]
    fn save_vm_state_round_trips_through_the_codec() {
        let mut a = full_vmm(nonzero_state(), mutate_exits(), 500, 0xABCD);
        step_n(&mut a, 6);
        let s = a.save_vm_state().expect("clean synchronized boundary");
        // The adapter's output is a faithful, encodable vm_state blob.
        let bytes = s.encode().expect("encodable (ratio_den == 1)");
        assert_eq!(vm_state::VmState::decode(&bytes).unwrap(), s);
        // The captured surface is non-trivial: regs, the V-time block, entropy
        // position, and the device blob all reflect the run.
        assert_eq!(s.regs.rax, 0x1111);
        assert_eq!(s.vtime.snapshot_vns, 500); // ratio 1:1 → vns == work
        assert_eq!(
            s.contract_hash,
            crate::vendor::x86::contract::contract_hash()
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings (task 98 / hm-d8o)"
    )]
    fn restore_vm_state_reproduces_the_blob_byte_for_byte() {
        // Live round-trip: save on A, restore into a fresh equivalently-wired B,
        // re-save — the second blob equals the first (the adapter is lossless over
        // the representable + device + V-time + entropy + tsc_adjust surface).
        let mut a = full_vmm(nonzero_state(), mutate_exits(), 500, 0xABCD);
        step_n(&mut a, 6);
        let s = a.save_vm_state().unwrap();

        let mut b = full_vmm(VcpuState::default(), vec![], 9999, 0x0000);
        b.restore_vm_state(&s).expect("restore");
        let s2 = b.save_vm_state().expect("re-save after restore");
        assert_eq!(s, s2, "restore-then-save must reproduce the snapshot blob");
    }

    #[test]
    fn restore_vm_state_resumes_tsc_and_forked_entropy_exactly() {
        // After a restore the V-time clock continues from the snapshot's vns and the
        // entropy stream resumes at its captured position (not replayed) — and a
        // counter sitting at a NON-zero value is reset to 0 (else the TSC would read
        // high). B reads the SECOND stream word (A drew the first) and a TSC that
        // continues from the snapshot point.
        const SEED: u64 = 0x5151_5151;
        let mut a = full_vmm(VcpuState::default(), mutate_exits(), 500, SEED);
        step_n(&mut a, 6);
        let s = a.save_vm_state().unwrap();

        // B's counter starts at 700 (non-zero) so the reset is observable.
        let mut b = full_vmm(
            VcpuState::default(),
            vec![
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Arch(X86Exit::Rdrand { width: 8 }),
            ],
            700,
            0xDEAD, // overwritten by the restored stream
        );
        b.restore_vm_state(&s).unwrap();
        b.step().unwrap(); // RDTSC at reset work=0 → visible = 2*vns_base + tsc_adjust
        b.step().unwrap(); // RDRAND → the word AFTER A's first draw

        // guest_clock = VClock::guest_ticks(0) [= 2 * vns_base = 1000] + IA32_TSC_ADJUST
        // [0x1234, set by mutate_exits and round-tripped through the snapshot].
        assert_eq!(
            b.backend.completions()[0],
            Completion::Read(2 * 500 + 0x1234)
        );
        let mut ref_stream = SeededEntropy::new(SEED);
        let mut w0 = [0u8; 8];
        let mut w1 = [0u8; 8];
        ref_stream.handle(1, &8u32.to_le_bytes(), &mut w0);
        ref_stream.handle(1, &8u32.to_le_bytes(), &mut w1);
        assert_eq!(
            b.backend.completions()[1],
            Completion::Read(u64::from_le_bytes(w1)),
            "restored entropy resumes at the next word (not replayed)"
        );
    }

    #[test]
    fn save_vm_state_fails_closed_at_rng_and_non_synchronized_boundaries() {
        // RNG mid-exit: a staged RDRAND completion ⇒ refuse.
        let mut rng = full_vmm(
            VcpuState::default(),
            vec![Exit::Arch(X86Exit::Rdrand { width: 8 })],
            10,
            1,
        );
        rng.step().unwrap();
        assert!(matches!(
            rng.save_vm_state(),
            Err(VmmError::ContractViolation(_))
        ));
        // Non-synchronized: a UART OUT after an RDTSC desynchronizes ⇒ refuse.
        let mut io = full_vmm(
            VcpuState::default(),
            vec![
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Arch(X86Exit::Io {
                    port: 0x3F8,
                    size: 1,
                    write: Some(u32::from(b'x')),
                }),
            ],
            10,
            1,
        );
        io.step().unwrap();
        assert!(io.save_vm_state().is_ok(), "exact at the intercept");
        io.step().unwrap();
        assert!(matches!(
            io.save_vm_state(),
            Err(VmmError::ContractViolation(_))
        ));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings (task 98 / hm-d8o)"
    )]
    fn restore_vm_state_rejects_a_different_contract_atomically() {
        let mut a = full_vmm(nonzero_state(), mutate_exits(), 500, 0xABCD);
        step_n(&mut a, 6);
        let mut s = a.save_vm_state().unwrap();
        s.contract_hash = [0xFFu8; 32]; // a different ratified contract

        let mut b = full_vmm(nonzero_state(), vec![], 100, 0xABCD);
        let before = b.state_hash();
        assert!(matches!(
            b.restore_vm_state(&s),
            Err(VmmError::Snapshot(
                crate::snapshot::SnapshotError::ContractMismatch
            ))
        ));
        assert_eq!(
            b.state_hash(),
            before,
            "a rejected snapshot leaves the VM fully intact (atomic)"
        );
    }

    #[test]
    fn branch_restores_then_forks_the_entropy_stream() {
        // branch(snap, seed') = restore + reseed: memory + V-time continue from the
        // snapshot, but the entropy stream forks to a divergent sequence.
        const PARENT_SEED: u64 = 0x1111;
        const BRANCH_SEED: u64 = 0x2222;
        let mut a = full_vmm(VcpuState::default(), mutate_exits(), 500, PARENT_SEED);
        step_n(&mut a, 6);
        let s = a.save_vm_state().unwrap();

        let mut b = full_vmm(
            VcpuState::default(),
            vec![Exit::Arch(X86Exit::Rdrand { width: 8 })],
            0,
            0xDEAD,
        );
        b.restore_vm_state(&s).unwrap();
        b.reseed_entropy(BRANCH_SEED).unwrap();
        b.step().unwrap(); // RDRAND draws from the BRANCH seed, not the parent's

        let mut branch_stream = SeededEntropy::new(BRANCH_SEED);
        let mut w = [0u8; 8];
        branch_stream.handle(1, &8u32.to_le_bytes(), &mut w);
        assert_eq!(
            b.backend.completions()[0],
            Completion::Read(u64::from_le_bytes(w)),
            "the branch draws from the reseeded stream"
        );
    }

    #[test]
    fn reseed_entropy_requires_a_wired_stream() {
        let mut stock = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert!(matches!(
            stock.reseed_entropy(7),
            Err(VmmError::ContractViolation(_))
        ));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings (task 98 / hm-d8o)"
    )]
    fn restore_vm_state_rejects_a_clock_rate_mismatch() {
        // Restoring a blob whose V-time *rate* differs from this VM's wired clock is
        // refused (the rate is not applied from the blob, so a silent accept would
        // run the restored timeline at the wrong rate). Each field perturbed alone
        // pins every disjunct of the rate-mismatch check.
        let mut a = full_vmm(VcpuState::default(), mutate_exits(), 500, 1);
        step_n(&mut a, 6);
        let s = a.save_vm_state().unwrap();
        let reject = |bad: &vm_state::VmState, name: &str| {
            let mut b = full_vmm(VcpuState::default(), vec![], 100, 1);
            assert!(
                matches!(b.restore_vm_state(bad), Err(VmmError::ContractViolation(_))),
                "a {name} clock-rate mismatch must be rejected"
            );
        };
        // Each disjunct of the rate-mismatch check, perturbed alone.
        let mut bad = s.clone();
        bad.vtime.ratio_num += 1;
        reject(&bad, "ratio_num");
        let mut bad = s.clone();
        bad.vtime.ratio_den = 2;
        reject(&bad, "ratio_den");
        let mut bad = s.clone();
        bad.vtime.guest_hz += 1;
        reject(&bad, "guest_hz");
        let mut bad = s.clone();
        bad.vtime.guest_base += 1;
        reject(&bad, "guest_base");
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings (task 98 / hm-d8o)"
    )]
    fn restore_into_unwired_vm_rejects_a_vtime_bearing_blob() {
        // A V-time-wired (no-LAPIC) source yields a blob carrying a live V-time
        // block; restoring it into a VM with no V-time wired is refused (wiring must
        // match the snapshot source). Both the guest_hz and the snapshot_vns disjuncts
        // are pinned individually.
        let mut a = vtime_vmm(
            vec![Exit::Arch(X86Exit::Rdtsc)],
            Box::new(ScriptedWork::at(5)),
            1,
        );
        a.step().unwrap();
        let s = a.save_vm_state().unwrap();
        assert!(
            s.vtime.guest_hz != 0,
            "source blob carries a live V-time block"
        );

        let mut only_hz = s.clone();
        only_hz.vtime.snapshot_vns = 0; // guest_hz still nonzero
        let mut stock1 = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert!(matches!(
            stock1.restore_vm_state(&only_hz),
            Err(VmmError::ContractViolation(_))
        ));

        let mut only_vns = s.clone();
        only_vns.vtime.guest_hz = 0;
        only_vns.vtime.snapshot_vns = 7;
        let mut stock2 = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert!(matches!(
            stock2.restore_vm_state(&only_vns),
            Err(VmmError::ContractViolation(_))
        ));
    }

    /// A backend that forwards to an inner mock but **fails `save()`** — to prove
    /// `save_vm_state` fails closed rather than sealing a `VcpuState::default()`.
    struct SaveFailBackend(MockBackend);
    impl Backend for SaveFailBackend {
        type A = vmm_backend::X86;

        fn set_policy(&mut self, policy: &X86Policy) -> vmm_backend::Result<()> {
            self.0.set_policy(policy)
        }
        unsafe fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> vmm_backend::Result<()> {
            // SAFETY: forwards to the inner mock, which only records the region
            // (no dereference); this adds no obligation beyond the trait contract.
            unsafe { self.0.map_memory(gpa, host) }
        }
        fn run(&mut self) -> vmm_backend::Result<Exit<vmm_backend::X86>> {
            self.0.run()
        }
        fn run_until(
            &mut self,
            d: vmm_backend::Moment,
        ) -> vmm_backend::Result<Exit<vmm_backend::X86>> {
            self.0.run_until(d)
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
        fn save(&self) -> vmm_backend::Result<VcpuState> {
            Err(vmm_backend::BackendError::Memory("induced save failure"))
        }
        fn restore(&mut self, s: &VcpuState) -> vmm_backend::Result<()> {
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

    #[test]
    fn save_vm_state_fails_closed_on_backend_save_error() {
        // A backend `save()` failure must abort the snapshot (fail closed), never
        // seal a zeroed vCPU and return Ok (the bug `current_vcpu`'s unwrap_or_default
        // would have hidden).
        let mut v = Vmm::new(
            SaveFailBackend(configured_mock(vec![])),
            GuestRam::new(0x1000).unwrap(),
        );
        assert!(
            matches!(v.save_vm_state(), Err(VmmError::Backend(_))),
            "a failing Backend::save must make save_vm_state fail closed"
        );
    }

    #[test]
    fn report_stream_round_trips_through_save_restore() {
        // The conformance report stream is captured + restored, so a branch resumes
        // the guest's observable output (its observable_digest), not just the vCPU.
        let mut a = full_vmm(VcpuState::default(), vec![], 0, 1);
        a.report_stream = vec![0xAA, 0x0000_0000, 0xDEAD_BEEF];
        let s = a.save_vm_state().unwrap();

        let mut b = full_vmm(VcpuState::default(), vec![], 0, 1);
        assert!(b.report_stream().is_empty(), "B starts with no reports");
        b.restore_vm_state(&s).unwrap();
        assert_eq!(
            b.report_stream(),
            &[0xAA, 0x0000_0000, 0xDEAD_BEEF],
            "the report stream is restored in execution order"
        );
        assert_eq!(
            b.observable_digest(),
            a.observable_digest(),
            "the restored VM's O2 observable_digest matches the snapshot source"
        );
    }

    #[test]
    fn restore_vm_state_rejects_a_legacy_wiring_mismatch() {
        // A malformed blob whose legacy subrecord is absent while the LAPIC matches
        // must be rejected (not silently skipped, which would leave stale 8259/PCI
        // state) — fail-closed, symmetric with the LAPIC wiring check.
        let mut a = full_vmm(VcpuState::default(), mutate_exits(), 500, 1);
        step_n(&mut a, 6);
        let mut s = a.save_vm_state().unwrap();
        let mut dev = snapshot::decode_device_blob(&s.devices.0).unwrap();
        assert!(
            dev.legacy.is_some() && dev.lapic.is_some(),
            "the full-VM blob carries both LAPIC and legacy state"
        );
        dev.legacy = None; // drop legacy while LAPIC stays → wiring mismatch
        s.devices = snapshot::encode_device_blob(&dev);

        let mut b = full_vmm(VcpuState::default(), vec![], 100, 1);
        assert!(
            matches!(b.restore_vm_state(&s), Err(VmmError::ContractViolation(_))),
            "a dropped legacy subrecord must be rejected, not silently skipped"
        );
    }

    #[test]
    fn restore_vm_state_rearms_the_first_entry_work_prepare() {
        // A restored VM is a **fresh spawn** for the work counter: the next step must
        // re-run `WorkSource::start_run` (the per-VM baseline) — else, on the shared
        // perf counter, a coexisting VM's branches between restore and entry would be
        // miscounted into the restored V-time. Resetting `first_entry_done` makes
        // `start_run` fire again. (The serial-OUT steps stage no completion, so the
        // restore is at a clean boundary — see the staged-completion guard.)
        let starts = std::rc::Rc::new(Cell::new(0u32));
        let out = |b: u8| {
            Exit::Arch(X86Exit::Io {
                port: 0x3F8,
                size: 1,
                write: Some(u32::from(b)),
            })
        };
        let mut v = Vmm::new(
            configured_mock(vec![out(b'x'), out(b'y')]),
            GuestRam::new(0x1000).unwrap(),
        );
        v.wire_vtime(
            VtimeWiring::new(
                contract_vclock_config(),
                Box::new(CountingStartWork {
                    starts: starts.clone(),
                }),
                1,
            )
            .unwrap(),
        );
        // Snapshot the fresh VM (synchronized, no staged completion).
        let snap = v.save_vm_state().expect("fresh VM is a clean boundary");
        assert_eq!(starts.get(), 0, "no guest entry yet");
        v.step().unwrap(); // first guest entry (OUT) → start_run fires (1)
        assert_eq!(starts.get(), 1);
        v.restore_vm_state(&snap)
            .expect("restore at a clean (no staged completion) boundary");
        v.step().unwrap(); // restored VM's first entry → start_run fires AGAIN (2)
        assert_eq!(
            starts.get(),
            2,
            "restore must re-arm the first-entry work prepare (treat as a fresh spawn)"
        );
    }

    #[test]
    fn restore_vm_state_rejects_a_staged_non_rng_completion() {
        // Restoring into a backend that just serviced a non-RNG read/MSR/CPUID/
        // determinism exit (a completion pending in kvm_run that restore does not
        // clear) is refused — it would commit the old exit on the next run.
        let mut src = full_vmm(VcpuState::default(), mutate_exits(), 500, 1);
        step_n(&mut src, 6);
        let snap = src.save_vm_state().unwrap();

        // A target VM that just serviced an RDTSC (non-RNG) has a staged completion.
        let mut tgt = full_vmm(
            VcpuState::default(),
            vec![Exit::Arch(X86Exit::Rdtsc)],
            10,
            1,
        );
        tgt.step().unwrap(); // RDTSC serviced → completion staged, NOT an RNG draw
        assert!(matches!(
            tgt.restore_vm_state(&snap),
            Err(VmmError::ContractViolation(_))
        ));
    }

    #[test]
    fn restore_vm_state_rejects_a_non_empty_timer_queue() {
        // vmm-core has no TimerQueue, so a non-empty `timers` section can't be applied
        // — it must be rejected, not silently dropped.
        let mut a = full_vmm(VcpuState::default(), mutate_exits(), 500, 1);
        step_n(&mut a, 6);
        let mut s = a.save_vm_state().unwrap();
        s.timers.entries.push(vm_state::TimerEntry {
            deadline_vns: 1000,
            seq: 0,
            token: 7,
            period_vns: 0,
        });
        s.timers.next_seq = 1;
        let mut b = full_vmm(VcpuState::default(), vec![], 100, 1);
        assert!(matches!(
            b.restore_vm_state(&s),
            Err(VmmError::ContractViolation(_))
        ));
    }

    #[test]
    fn save_vm_state_fails_closed_on_unrepresentable_sregs() {
        // `kvm_sregs2` flags/pdptrs are not carried; the determinism guest is 64-bit /
        // paging-off (they are 0). A non-zero value would be silently zeroed on
        // restore, so the snapshot fails closed instead of sealing a lossy blob.
        let mut flags = nonzero_state();
        flags.sregs.flags = 1; // e.g. PDPTRS_VALID
        let mut v = full_vmm(flags, vec![], 0, 1);
        assert!(matches!(
            v.save_vm_state(),
            Err(VmmError::ContractViolation(_))
        ));

        let mut pdptr = nonzero_state();
        pdptr.sregs.pdptrs[2] = 0xDEAD_BEEF;
        let mut v2 = full_vmm(pdptr, vec![], 0, 1);
        assert!(matches!(
            v2.save_vm_state(),
            Err(VmmError::ContractViolation(_))
        ));

        // `kvm_debugregs.flags` (not carried) — DR0..3/DR6/DR7 ARE carried.
        let mut dbg = nonzero_state();
        dbg.debugregs.flags = 1;
        let mut v3 = full_vmm(dbg, vec![], 0, 1);
        assert!(matches!(
            v3.save_vm_state(),
            Err(VmmError::ContractViolation(_))
        ));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings (task 98 / hm-d8o)"
    )]
    fn save_vm_state_captures_in_flight_events_at_a_non_quiescent_point() {
        // Task 41 — the headline inversion: a point with an interrupt/exception **in
        // flight** (the very state task 39 fail-closed-rejected) is now snapshottable,
        // and the full kvm_vcpu_events round-trips through save → restore → re-save.
        let in_flight = |events: vmm_backend::VcpuEvents, name: &str| {
            let mut st = nonzero_state();
            st.events = events;
            let mut a = full_vmm(st, vec![], 0, 1);
            // Save SUCCEEDS at the non-quiescent point (no fail-closed rejection).
            let s = a
                .save_vm_state()
                .unwrap_or_else(|e| panic!("{name}: an in-flight point must snapshot, got {e:?}"));
            // The events are carried in the device blob in **canonical** form (active
            // injection preserved; KVM's inert modifier residuals collapsed).
            let want = snapshot::canonical_events(&events);
            let dev = snapshot::decode_device_blob(&s.devices.0).unwrap();
            assert_eq!(
                dev.events, want,
                "{name}: canonical kvm_vcpu_events captured"
            );
            // Restore into a fresh, equivalently-wired VM and confirm the backend received the
            // restore-form events: canonical payloads, but with the clear-on-restore validity
            // bits forced on (`events_for_restore` — PR #12 round 6) so KVM clears stale state
            // on a non-fresh target. The active injection is preserved either way.
            let mut b = full_vmm(VcpuState::default(), vec![], 0, 1);
            b.restore_vm_state(&s)
                .expect("restore the in-flight snapshot");
            assert_eq!(
                b.backend.save().unwrap().events,
                snapshot::events_for_restore(&events),
                "{name}: restore re-establishes the in-flight events (restore form) on the backend"
            );
        };
        // Each in-flight injection class that task 39 rejected, now captured.
        in_flight(
            vmm_backend::VcpuEvents {
                nmi_masked: 1,
                ..Default::default()
            },
            "nmi_masked",
        );
        in_flight(
            vmm_backend::VcpuEvents {
                interrupt_injected: 1,
                interrupt_nr: 0x34,
                ..Default::default()
            },
            "interrupt_injected",
        );
        in_flight(
            vmm_backend::VcpuEvents {
                exception_injected: 1,
                exception_nr: 14,
                exception_has_error_code: 1,
                exception_error_code: 0xCAFE,
                ..Default::default()
            },
            "exception_error_code",
        );
        // Two cap-gated event fields are fail-closed-REJECTED at save (PR #12 round 7): their
        // `KVM_SET_VCPU_EVENTS` validity bits need `KVM_CAP_X86_TRIPLE_FAULT_EVENT` /
        // `KVM_CAP_EXCEPTION_PAYLOAD`, which this backend does not enable — a captured value
        // could not be restored, so save fails closed rather than seal an unrestorable snapshot.
        let rejects = |events: vmm_backend::VcpuEvents, needle: &str| {
            let mut st = nonzero_state();
            st.events = events;
            let mut v = full_vmm(st, vec![], 0, 1);
            match v.save_vm_state() {
                Err(VmmError::ContractViolation(msg)) => assert!(
                    msg.contains(needle),
                    "reject reason should name {needle:?}, got: {msg}"
                ),
                other => panic!("a cap-gated event field must fail closed at save, got {other:?}"),
            }
        };
        rejects(
            vmm_backend::VcpuEvents {
                triple_fault_pending: 1,
                ..Default::default()
            },
            "triple_fault_pending",
        );
        rejects(
            vmm_backend::VcpuEvents {
                exception_has_payload: 1,
                exception_payload: 0xCAFE,
                ..Default::default()
            },
            "exception_has_payload",
        );
        // A clean quiescent point still snapshots (no regression), and the validity-mask
        // `flags` is carried like any other field now.
        let mut v_ok = full_vmm(nonzero_state(), vec![], 0, 1);
        assert!(
            v_ok.save_vm_state().is_ok(),
            "a quiescent point still snapshots"
        );
    }

    #[test]
    fn restore_canonicalizes_raw_events_from_an_external_blob() {
        // PR #12 round 3 — restore symmetry. This VM's own save path stores CANONICAL events
        // in the device blob, but an *external or older* v3 blob (hand-built, or from a
        // different/buggy encoder) may carry RAW KVM modifier residuals. `restore_vm_state`
        // must canonicalize them (mirror the save side), so a foreign/corrupt blob cannot
        // reintroduce the exact residuals `KVM_SET_VCPU_EVENTS` would choke on.
        let mut a = full_vmm(nonzero_state(), vec![], 0, 1);
        let mut s = a.save_vm_state().expect("quiescent save");
        // Forge an external blob: raw inert residuals (a stale interrupt.nr / exception.nr /
        // has_error_code + the GET-only validity flags), as a non-canonicalizing encoder
        // would leave them. Every active bit is clear → canonical form is the clean record.
        let raw = vmm_backend::VcpuEvents {
            interrupt_nr: 0x34,
            exception_nr: 13,
            exception_has_error_code: 1,
            flags: 0x0D,
            ..Default::default()
        };
        let mut dev = snapshot::decode_device_blob(&s.devices.0).unwrap();
        dev.events = raw;
        s.devices = snapshot::encode_device_blob(&dev);
        // Restore the forged blob: the backend must receive the RESTORE-FORM events — the
        // residuals stripped (clean payloads), with the clear-on-restore validity bits forced
        // on (`events_for_restore` — PR #12 round 6), NOT the raw residuals (which would
        // corrupt the guest).
        let mut b = full_vmm(VcpuState::default(), vec![], 0, 1);
        b.restore_vm_state(&s).expect("restore the external blob");
        let restored = b.backend.save().unwrap().events;
        assert_eq!(
            restored,
            snapshot::events_for_restore(&raw),
            "restore strips the residuals and forces the clear-on-restore validity bits"
        );
        // The residual PAYLOADS are stripped (the stale interrupt.nr / exception.nr /
        // has_error_code are gone), even though the validity-mask flags are set:
        assert_eq!(restored.interrupt_nr, 0, "stale interrupt.nr stripped");
        assert_eq!(restored.exception_nr, 0, "stale exception.nr stripped");
        assert_eq!(
            restored.exception_has_error_code, 0,
            "stale has_error_code stripped"
        );
        assert_ne!(
            restored, raw,
            "the raw residuals were NOT forwarded verbatim"
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings (task 98 / hm-d8o)"
    )]
    fn restore_vm_state_rejects_a_cap_gated_event_blob_before_mutation() {
        // PR #12 round 8 — restore's reject-before-mutation (atomic) contract. A foreign /
        // malformed v3 blob whose `kvm_vcpu_events` would set a cap-disabled validity bit
        // (`VALID_TRIPLE_FAULT` / `VALID_PAYLOAD`) makes `KVM_SET_VCPU_EVENTS` return `-EINVAL`
        // only AFTER earlier `KVM_SET_*` ioctls inside `Backend::restore` already mutated the
        // target vCPU. `restore_vm_state` must reject the blob up front (mirroring the
        // `save_vm_state` guard) so it never half-mutates the target.
        let reject = |bad: vmm_backend::VcpuEvents, needle: &str| {
            // A target vCPU with a recognizable state, to prove it is NOT mutated on reject.
            let mut marked = nonzero_state();
            marked.events.interrupt_injected = 1;
            marked.events.interrupt_nr = 0x99;
            let mut b = full_vmm(marked, vec![], 0, 1);
            let before = b.backend.save().unwrap();
            // Forge an external blob (valid except for the cap-gated event field).
            let mut a = full_vmm(nonzero_state(), vec![], 0, 1);
            let mut s = a.save_vm_state().unwrap();
            let mut dev = snapshot::decode_device_blob(&s.devices.0).unwrap();
            dev.events = bad;
            s.devices = snapshot::encode_device_blob(&dev);
            // Restore must reject, naming the offending field...
            match b.restore_vm_state(&s) {
                Err(VmmError::ContractViolation(msg)) => assert!(
                    msg.contains(needle),
                    "reject reason should name {needle:?}, got: {msg}"
                ),
                other => panic!("restore must reject a cap-gated event blob, got {other:?}"),
            }
            // ...and must NOT have mutated the target vCPU (reject before mutation).
            assert_eq!(
                b.backend.save().unwrap(),
                before,
                "restore must not mutate the target vCPU when it rejects the blob"
            );
        };
        reject(
            vmm_backend::VcpuEvents {
                triple_fault_pending: 1,
                ..Default::default()
            },
            "triple_fault_pending",
        );
        reject(
            vmm_backend::VcpuEvents {
                exception_injected: 1,
                exception_nr: 14,
                exception_has_payload: 1,
                exception_payload: 0xCAFE,
                ..Default::default()
            },
            "exception_has_payload",
        );
    }

    #[test]
    fn has_inflight_event_injection_reflects_the_live_vcpu() {
        // The public accessor the gate-1 measurement quotes: `false` at a quiescent
        // point, `true` when the live vCPU has an interrupt/exception in flight.
        let quiescent = full_vmm(nonzero_state(), vec![], 0, 1);
        assert!(
            !quiescent.has_inflight_event_injection(),
            "a quiescent vCPU is not a non-quiescent point"
        );
        let mut st = nonzero_state();
        st.events.interrupt_injected = 1;
        st.events.interrupt_nr = 0x34;
        let in_flight = full_vmm(st, vec![], 0, 1);
        assert!(
            in_flight.has_inflight_event_injection(),
            "an injected-but-undelivered interrupt is a non-quiescent point"
        );
    }

    #[test]
    fn has_active_event_injection_reflects_the_live_vcpu() {
        // The accessor the gate-1 SEAL uses: `false` at a quiescent point AND at an inert
        // residual point, `true` only for a GENUINE injected/pending event. This is the
        // active/residual distinction at the `Vmm` seam — sealing on a residual would
        // snapshot a quiescent-equivalent point that does not prove the headline (PR #12
        // round 2). Pins the wrapper so a `-> true`/`-> false` mutant is caught.
        let quiescent = full_vmm(nonzero_state(), vec![], 0, 1);
        assert!(
            !quiescent.has_active_event_injection(),
            "a quiescent vCPU carries no active event"
        );
        // A stale modifier residual (interrupt.nr set, injected clear) is a task-39-reject
        // point (`has_inflight`) but NOT active — the gate must never seal here.
        let mut residual = nonzero_state();
        residual.events.interrupt_nr = 0x34; // injected stays 0 → inert residual
        let residual_vmm = full_vmm(residual, vec![], 0, 1);
        assert!(
            residual_vmm.has_inflight_event_injection(),
            "an inert residual is still a task-39-reject point"
        );
        assert!(
            !residual_vmm.has_active_event_injection(),
            "but an inert residual is NOT a genuine active injection — never seal here"
        );
        // A genuine injected-but-undelivered interrupt IS active.
        let mut st = nonzero_state();
        st.events.interrupt_injected = 1;
        st.events.interrupt_nr = 0x34;
        let in_flight = full_vmm(st, vec![], 0, 1);
        assert!(
            in_flight.has_active_event_injection(),
            "an injected-but-undelivered interrupt is a genuine active injection"
        );
    }

    #[test]
    fn has_pending_guest_interrupt_reflects_a_pending_lapic_vector() {
        // The OTHER genuine seal condition — the one a synchronized (snapshottable)
        // boundary can actually carry: a real interrupt raised into the LAPIC IRR but not
        // yet accepted (the in-flight event captured in the device blob, re-derived on
        // restore). A quiescent LAPIC is `false`; a deferred-accept timer vector pending in
        // the IRR is `true`. Pins the wrapper (`-> true`/`-> false` mutant) and the
        // `lapic_pending || serial` arbitration.
        const W: u64 = 100_000_000;
        // Quiescent: a wired LAPIC with no timer programmed → nothing pending in the IRR.
        let mut q = lapic_vmm(
            configured_mock(vec![
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Common(CommonExit::Idle),
            ]),
            Box::new(ScriptedWork::at(W)),
        );
        q.step().unwrap();
        assert!(
            !q.has_pending_guest_interrupt().unwrap(),
            "a quiescent LAPIC has no pending guest interrupt"
        );
        // In flight: arm the timer, let it fire into the IRR, hold it un-accepted
        // (defer_accept) — exactly a snapshottable in-flight point. `peek_interrupt`
        // re-derives 0x40 without moving IRR→ISR.
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Arch(X86Exit::Rdtsc));
        exits.push(Exit::Arch(X86Exit::Rdtsc));
        let mut mock = configured_mock(exits);
        mock.set_defer_accept(true);
        let mut a = lapic_vmm(mock, Box::new(ScriptedWork::at(W)));
        step_n(&mut a, 5);
        assert_eq!(
            a.backend.pending_irq(),
            Some(0x40),
            "0x40 is in flight in the IRR (routed to the seam, not yet accepted)"
        );
        assert!(
            a.has_pending_guest_interrupt().unwrap(),
            "a vector pending in the LAPIC IRR is a genuine in-flight guest interrupt"
        );
    }

    #[test]
    fn snapshot_restore_re_derives_the_in_flight_lapic_irq() {
        // Task 41 — the inject-seam round-trip. Snapshot a VM with a LAPIC timer vector
        // pending in IRR but **not yet accepted** (an IRQ raised+routed but not
        // injected — the `set_pending_irq` slot is live). The seam is NOT serialized;
        // on restore the vector survives in the LAPIC IRR (device blob) and the restored
        // VM's first `service_pending_irqs` re-derives the identical pending vector. So
        // the in-flight injection is reproduced, not dropped.
        const W: u64 = 100_000_000;
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Arch(X86Exit::Rdtsc)); // advance the anchor + synchronize (no vector yet)
        exits.push(Exit::Arch(X86Exit::Rdtsc)); // service fires 0x40 into IRR + sets pending; re-sync
        let mut mock = configured_mock(exits);
        mock.set_defer_accept(true); // hold 0x40 un-accepted → it stays pending in IRR
        let mut a = lapic_vmm(mock, Box::new(ScriptedWork::at(W)));
        step_n(&mut a, 5);
        assert_eq!(
            a.backend.pending_irq(),
            Some(0x40),
            "the timer vector is in flight (routed to the seam, not yet accepted)"
        );

        // Save at this non-quiescent, synchronized boundary (now permitted).
        let s = a
            .save_vm_state()
            .expect("a point with an in-flight LAPIC vector is snapshottable");
        // The in-flight vector survived in the captured LAPIC IRR (vector 0x40 → bank
        // 0x40/32 = 2, bit 0).
        let dev = snapshot::decode_device_blob(&s.devices.0).unwrap();
        let irr = dev.lapic.expect("lapic captured").irr;
        assert_eq!(irr[2] & 1, 1, "vector 0x40 is pending in the captured IRR");

        // Restore into a fresh, equivalently-wired VM and take one step: its first
        // service must re-derive the SAME pending vector from the restored IRR.
        let mut bmock = configured_mock(vec![
            Exit::Arch(X86Exit::Rdtsc),
            Exit::Common(CommonExit::Idle),
        ]);
        bmock.set_defer_accept(true);
        let mut b = lapic_vmm(bmock, Box::new(ScriptedWork::at(W)));
        b.restore_vm_state(&s)
            .expect("restore the in-flight LAPIC snapshot");
        b.step().unwrap();
        assert_eq!(
            b.backend.pending_irq(),
            Some(0x40),
            "the restored VM re-derives the in-flight vector from the LAPIC IRR (seam re-armed)"
        );
    }

    #[test]
    fn save_vm_state_captures_the_uart_dlm() {
        // The divisor-latch-high byte (a DLAB-window write) is captured into the
        // device blob — pins the `Uart8250::dlm()` accessor.
        let mut v = full_vmm(
            VcpuState::default(),
            vec![
                Exit::Arch(X86Exit::Io {
                    port: 0x3FB,
                    size: 1,
                    write: Some(0x80),
                }), // LCR: DLAB = 1
                Exit::Arch(X86Exit::Io {
                    port: 0x3F9,
                    size: 1,
                    write: Some(0x07),
                }), // offset+1 under DLAB ⇒ DLM = 7
                Exit::Arch(X86Exit::Rdtsc), // re-synchronize for the save
            ],
            0,
            1,
        );
        step_n(&mut v, 3);
        let s = v.save_vm_state().unwrap();
        let dev = snapshot::decode_device_blob(&s.devices.0).unwrap();
        assert_eq!(dev.uart.dlm, 7, "save_vm_state must capture the UART DLM");
        assert!(dev.uart.dlab, "and the latched DLAB window state");
    }

    #[test]
    fn restore_guest_memory_overwrites_the_backing_and_checks_length() {
        let mut v = Vmm::new(configured_mock(vec![]), GuestRam::new(0x2000).unwrap());
        let image = vec![0xABu8; 0x2000];
        v.restore_guest_memory(&image).unwrap();
        assert_eq!(v.guest_memory(), &image[..]);
        // Wrong length fails closed (never a partial overwrite).
        assert!(matches!(
            v.restore_guest_memory(&[0u8; 0x1000]),
            Err(VmmError::ContractViolation(_))
        ));
    }

    // --- the canonical-vm_state hash gate ----------------------------------

    fn has_tag(blob: &[u8], tag: &[u8; 4]) -> bool {
        blob.windows(4).any(|w| w == tag)
    }

    #[test]
    fn snapshot_hashing_is_gated_off_by_default() {
        // Default-off: no VMST chunk, so M1/M2/corpus/Linux-boot hashes are
        // byte-for-byte unchanged from before this path existed.
        let v = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert!(!v.snapshot_hashing_wired());
        assert!(!has_tag(&v.state_blob(), b"VMST"));
        // A second identical VM hashes identically (no nondeterminism introduced).
        let v2 = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert_eq!(v.state_hash(), v2.state_hash());
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "sha256-dominated (each state_hash/state_blob over the TEST_RAM image interprets ~2 s/KiB under Miri and this test hashes repeatedly); pure safe code over the mock backend — no map_memory on this path (both seams stay Miri-run in bringup); logic covered natively, and the family keeps Miri-run siblings (task 98 / hm-d8o)"
    )]
    fn wiring_snapshot_hashing_folds_the_canonical_blob_into_the_hash() {
        // Enabling it adds the VMST chunk and changes the hash; two states whose
        // canonical blob differs (here a TPR write) then hash differently, while the
        // unwired twin's hash is untouched.
        let base = full_vmm(VcpuState::default(), vec![], 0, 1);
        let base_hash_unwired = base.state_hash();

        let mut on = full_vmm(VcpuState::default(), vec![], 0, 1);
        on.wire_snapshot_hashing();
        assert!(on.snapshot_hashing_wired());
        assert!(has_tag(&on.state_blob(), b"VMST"));
        assert_ne!(
            on.state_hash(),
            base_hash_unwired,
            "folding the canonical blob changes the hash"
        );

        // A vm_state difference (a TPR write) changes the VMST-folded hash.
        let mut a = full_vmm(VcpuState::default(), vec![], 0, 1);
        a.wire_snapshot_hashing();
        let mut b = full_vmm(
            VcpuState::default(),
            vec![Exit::Common(CommonExit::Mmio {
                gpa: Gpa(0xFEE0_0080),
                size: 4,
                write: Some(0x30),
            })],
            0,
            1,
        );
        b.wire_snapshot_hashing();
        b.step().unwrap();
        assert_ne!(
            a.state_hash(),
            b.state_hash(),
            "a vm_state difference reaches state_hash when snapshot-hashing is wired"
        );
    }

    // -----------------------------------------------------------------------
    // Task 110: the paravirt work-derived clock page (docs/PARAVIRT-CLOCK.md).
    // Portable halves of the G1/G2/G3 gates + the registration transport,
    // driven by the scripted MockBackend — no /dev/kvm, runs on every platform.
    // -----------------------------------------------------------------------

    use vtime::pvclock::{PVCLOCK_ABI_VERSION, PVCLOCK_PAGE_LEN};

    /// A pvclock-offered `Vmm<MockBackend>` with the determinism path wired and
    /// RAM covering the doorbell frame pages.
    fn pvclock_vmm(
        exits: Vec<Exit<X86>>,
        work: Box<dyn WorkSource>,
        seed: u64,
    ) -> Vmm<MockBackend> {
        let mut vmm = Vmm::new(configured_mock(exits), GuestRam::new(TEST_RAM).unwrap());
        vmm.wire_vtime(VtimeWiring::new(contract_vclock_config(), work, seed).unwrap());
        vmm.enable_pvclock(PVCLOCK_DEFAULT_DELTA_WORK);
        vmm
    }

    /// Stage a `pvclock_register(gpa)` request frame at `REQ_GPA` and ring the
    /// doorbell; return the decoded response `(status, payload)`.
    fn ring_pvclock_register(vmm: &mut Vmm<MockBackend>, gpa: u64) -> (u16, Vec<u8>) {
        let mut frame = [0_u8; 64];
        let len = hypercall_proto::encode_request(
            ServiceId::Pvclock,
            1,
            1,
            &gpa.to_le_bytes(),
            &mut frame,
        )
        .expect("encode register request");
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + len].copy_from_slice(&frame[..len]);
        assert_eq!(
            vmm.service_doorbell(len as u32).expect("doorbell serviced"),
            Step::Continued
        );
        let resp = &vmm.ram.as_bytes()[RESP_GPA..RESP_GPA + HC_PAGE];
        let (header, payload) = decode(resp).expect("well-formed response frame");
        (header.status, payload.to_vec())
    }

    /// A page GPA inside `TEST_RAM`, clear of the doorbell pages and the
    /// booted-image regions the other tests use.
    const PV_GPA: u64 = 0x4000;

    /// Registration validates + records the GPA, stamps the page to canonical
    /// form at the current anchor, answers the ABI version, and marks the page
    /// host-dirty (the task-95 M2.1 safety rule).
    #[test]
    fn pvclock_registration_stamps_canonical_page_and_answers_abi() {
        let mut vmm = pvclock_vmm(vec![], Box::new(ScriptedWork::at(500)), 7);
        // Make the anchor non-trivial: pretend the last intercept was at 500.
        vmm.vtime.as_mut().unwrap().last_intercept_work = 500;
        let (status, payload) = ring_pvclock_register(&mut vmm, PV_GPA);
        assert_eq!(status, Status::Ok as u16);
        assert_eq!(payload, PVCLOCK_ABI_VERSION.to_le_bytes());
        assert_eq!(vmm.pvclock_registration(), Some(PV_GPA));
        // The page is canonical (seq 0) and publishes the anchor clock: the
        // contract clock is 1 ns/branch, 2 GHz -> vns 500, guest_clock 1000.
        let f = vtime::pvclock::read(vmm.pvclock_page().unwrap()).expect("stable frame");
        assert_eq!((f.seq, f.vns, f.guest_clock), (0, 500, 1000));
        assert_eq!(f.guest_clock_hz, 2_000_000_000);
        // The oracle check holds at registration (G2's function equality).
        vmm.pvclock_check_oracle()
            .expect("page matches the trap oracle");
        // Host-dirty: the stamped page and the doorbell response page.
        let mut gfns = vmm.host_dirty.iter().copied().collect::<Vec<_>>();
        gfns.sort_unstable();
        assert_eq!(gfns, vec![PV_GPA / 4096, (RESP_GPA as u64) / 4096]);
    }

    /// Bad GPAs are clean `OutOfRange` rejections that record nothing:
    /// misaligned, past-the-end, address-overflow, and the doorbell pages.
    #[test]
    fn pvclock_registration_rejects_bad_gpas() {
        for bad in [
            PV_GPA + 1,      // misaligned
            TEST_RAM as u64, // one past the end
            u64::MAX - 4095, // aligned, but end overflows
            REQ_GPA as u64,  // the doorbell request page
            RESP_GPA as u64, // the doorbell response page
        ] {
            let mut vmm = pvclock_vmm(vec![], Box::new(ScriptedWork::new()), 7);
            let (status, payload) = ring_pvclock_register(&mut vmm, bad);
            assert_eq!(status, Status::OutOfRange as u16, "gpa {bad:#x}");
            assert!(payload.is_empty());
            assert_eq!(vmm.pvclock_registration(), None, "gpa {bad:#x} recorded");
        }
    }

    /// The pure-opt-in gate, host side: an offered-but-vtime-unwired VM and a
    /// backend without a deterministic work counter both answer
    /// `UnknownService` — the probing guest keeps its trap-backstopped paths.
    #[test]
    fn pvclock_registration_requires_the_determinism_path() {
        // Offered, but no V-time wired.
        let mut no_vtime = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        no_vtime.enable_pvclock(PVCLOCK_DEFAULT_DELTA_WORK);
        let (status, _) = ring_pvclock_register(&mut no_vtime, PV_GPA);
        assert_eq!(status, Status::UnknownService as u16);
        assert_eq!(no_vtime.pvclock_registration(), None);

        // Offered + V-time wired, but the backend reports no deterministic clock.
        let mut caps = MOCK_TEST_CAPS;
        caps.arch.deterministic_tsc = false;
        let mut m = MockBackend::with_capabilities(caps);
        m.set_policy(&X86Policy {
            cpuid: CpuidModel::default(),
            msr_filter: MsrFilter::default(),
        })
        .unwrap();
        let mut no_det = Vmm::new(m, GuestRam::new(TEST_RAM).unwrap());
        no_det.wire_vtime(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::new()), 7).unwrap(),
        );
        no_det.enable_pvclock(PVCLOCK_DEFAULT_DELTA_WORK);
        let (status, _) = ring_pvclock_register(&mut no_det, PV_GPA);
        assert_eq!(status, Status::UnknownService as u16);
        assert_eq!(no_det.pvclock_registration(), None);
        // And no forced-refresh deadline can ever arm there.
        assert_eq!(no_det.pvclock_refresh_deadline(), None);
    }

    /// The pure-opt-in gate, guest side (the "page off = byte-identical" half
    /// of "Done means"): a VM that OFFERS the page but whose guest never
    /// registers is **guest-observably identical** to an un-offered VM over
    /// the same script — identical RAM, serial, and observable digest; no
    /// stamp is ever written. The `state_blob`s differ by EXACTLY the `PVCK`
    /// channel-configuration chunk (cross-model r1 P1: the offer + Δ govern
    /// future execution, so they are state identity — the SDK fault-policy
    /// precedent), and an un-offered blob carries no chunk at all (its bytes
    /// are unchanged from before the feature existed).
    #[test]
    fn pvclock_unregistered_guest_is_guest_identical_and_differs_only_in_pvck() {
        let script = || {
            vec![
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Arch(X86Exit::Rdrand { width: 8 }),
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Common(CommonExit::Shutdown),
            ]
        };
        let run = |offer: bool| {
            let mut vmm = Vmm::new(configured_mock(script()), GuestRam::new(TEST_RAM).unwrap());
            vmm.wire_vtime(
                VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(42)), 7)
                    .unwrap(),
            );
            if offer {
                vmm.enable_pvclock(PVCLOCK_DEFAULT_DELTA_WORK);
            }
            vmm.run().unwrap();
            (
                vmm.guest_memory().to_vec(),
                vmm.serial().to_vec(),
                vmm.observable_digest(),
                vmm.state_blob(),
            )
        };
        let (ram_on, serial_on, digest_on, blob_on) = run(true);
        let (ram_off, serial_off, digest_off, blob_off) = run(false);
        assert_eq!(ram_on, ram_off, "offering alone touched guest RAM");
        assert_eq!(serial_on, serial_off);
        assert_eq!(digest_on, digest_off);
        // The blobs differ by exactly the PVCK chunk: splice it out of the
        // offered blob (tag + u64 LE length + body, the put_chunk framing)
        // and require byte equality with the un-offered blob.
        let tag = blob_on
            .windows(4)
            .position(|w| w == b"PVCK")
            .expect("offered blob carries the PVCK chunk");
        let len = u64::from_le_bytes(blob_on[tag + 4..tag + 12].try_into().unwrap()) as usize;
        let mut spliced = blob_on.clone();
        spliced.drain(tag..tag + 12 + len);
        assert_eq!(
            spliced, blob_off,
            "the offered and un-offered blobs differ beyond the PVCK chunk"
        );
        assert!(
            !blob_off.windows(4).any(|w| w == b"PVCK"),
            "an un-offered blob must carry no PVCK chunk"
        );
    }

    /// The `PVCK` chunk is real state identity: same configuration ⇒ same
    /// blob; a different Δ, or a registration, each change it (two states
    /// identical in RAM but differing there have different futures).
    #[test]
    fn pvclock_channel_configuration_reaches_state_identity() {
        let build = |delta: u64| {
            let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            vmm.wire_vtime(
                VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(42)), 7)
                    .unwrap(),
            );
            vmm.enable_pvclock(delta);
            vmm
        };
        let base = build(PVCLOCK_DEFAULT_DELTA_WORK).state_blob();
        assert_eq!(
            base,
            build(PVCLOCK_DEFAULT_DELTA_WORK).state_blob(),
            "same configuration must hash identically"
        );
        assert_ne!(
            base,
            build(PVCLOCK_DEFAULT_DELTA_WORK + 1).state_blob(),
            "a different Δ is a different future — must reach the hash"
        );
        let mut registered = build(PVCLOCK_DEFAULT_DELTA_WORK);
        let (status, _) = ring_pvclock_register(&mut registered, PV_GPA);
        assert_eq!(status, Status::Ok as u16);
        assert_ne!(
            base,
            registered.state_blob(),
            "a registration is a different future — must reach the hash"
        );
    }

    /// The §2 point-1 natural-exit refresh runs at NON-intercept exits too
    /// (cross-model r1 P1, resolved with the anchor value): between clock
    /// advances the stamp is a byte no-op (value-keyed), but it observably
    /// runs — a page the guest scribbled is repaired at the very next exit
    /// (here a UART OUT, not a V-time intercept), publishing the same
    /// anchor-derived values the trap oracle would return.
    #[test]
    fn pvclock_natural_exits_refresh_with_the_anchor_value() {
        let mut vmm = pvclock_vmm(
            vec![
                Exit::Arch(X86Exit::Rdtsc),
                // A serial byte OUT: an ordinary PIO exit, no V-time intercept.
                Exit::Arch(X86Exit::Io {
                    port: 0x3F8,
                    size: 1,
                    write: Some(u32::from(b'x')),
                }),
            ],
            Box::new(ScriptedWork::at(10)),
            7,
        );
        ring_pvclock_register(&mut vmm, PV_GPA);
        vmm.step().unwrap(); // RDTSC: anchor 10, page stamped
        let stamped = vtime::pvclock::read(vmm.pvclock_page().unwrap()).unwrap();
        // The guest scribbles its own page (deterministic guest behavior).
        let off = PV_GPA as usize + vtime::pvclock::VNS_OFF;
        vmm.ram.as_mut_bytes()[off] ^= 0xA5;
        assert!(vmm.pvclock_check_oracle().is_err(), "scribble visible");
        // The next exit is a plain UART write — NOT synchronized — and the
        // natural-exit refresh still repairs the page to the anchor values.
        vmm.step().unwrap();
        assert!(
            !vmm.is_synchronized(),
            "a UART OUT is not a V-time intercept"
        );
        let repaired = vtime::pvclock::read(vmm.pvclock_page().unwrap()).unwrap();
        assert_eq!(
            (repaired.vns, repaired.guest_clock),
            (stamped.vns, stamped.guest_clock)
        );
        vmm.pvclock_check_oracle()
            .expect("the natural-exit refresh restored oracle equality");
    }

    /// A REJECTED seal attempt mutates nothing (cross-model r1 P2): with a
    /// registered page mid-run (non-zero seq) and a vCPU that fails the
    /// sealability check, `save_vm_state` errors AND leaves the page bytes,
    /// the refresh log, and the host-dirty set byte-for-byte untouched — the
    /// `NotQuiescent` retry loops and sealability probes are side-effect-free.
    #[test]
    fn pvclock_rejected_seal_does_not_canonicalize_the_page() {
        let mut vmm = pvclock_vmm(
            vec![Exit::Arch(X86Exit::Rdtsc)],
            Box::new(ScriptedWork::at(10)),
            7,
        );
        ring_pvclock_register(&mut vmm, PV_GPA);
        vmm.step().unwrap(); // synchronized; page stamped, seq moved off 0
        // Drain the registration's dirty bookkeeping so the assertion below
        // isolates the seal attempt.
        vmm.host_dirty.clear();
        let page_before = vmm.pvclock_page().unwrap().to_vec();
        assert_ne!(
            vtime::pvclock::read(&page_before).unwrap().seq,
            0,
            "precondition: the live page is non-canonical"
        );
        let refreshes_before = vmm.pvclock_refreshes().to_vec();
        // Make the vCPU unsealable (PAE-only sregs flags — the same lever the
        // existing fail-closed seal tests use).
        let mut bad = vmm.backend.save().unwrap();
        bad.sregs.flags = 1;
        vmm.backend.restore(&bad).unwrap();
        vmm.saved_state = None;
        assert!(
            matches!(vmm.save_vm_state(), Err(VmmError::ContractViolation(_))),
            "the unsealable vCPU must reject the seal"
        );
        assert_eq!(
            vmm.pvclock_page().unwrap(),
            page_before.as_slice(),
            "a rejected seal canonicalized the page (reject-before-mutation broken)"
        );
        assert_eq!(vmm.pvclock_refreshes(), refreshes_before.as_slice());
        assert!(
            vmm.host_dirty.is_empty(),
            "a rejected seal marked host-dirty state"
        );
        // And the SAME point seals fine once the vCPU is sealable again —
        // canonicalizing exactly then.
        bad.sregs.flags = 0;
        vmm.backend.restore(&bad).unwrap();
        vmm.save_vm_state().unwrap();
        assert_eq!(
            vtime::pvclock::read(vmm.pvclock_page().unwrap())
                .unwrap()
                .seq,
            0
        );
    }

    /// G1's portable analogue: two same-seed, same-script runs with the page
    /// registered produce bit-identical `state_blob`s (page bytes included) —
    /// the stamping machinery leaks no run-local entropy into guest RAM.
    #[test]
    fn pvclock_same_seed_runs_are_bit_identical_with_the_page_on() {
        let run = || {
            let mut work = ScriptedWork::new();
            work.advance(100);
            let mut vmm = pvclock_vmm(
                vec![
                    Exit::Arch(X86Exit::Rdtsc),
                    Exit::Arch(X86Exit::Rdtsc),
                    Exit::Common(CommonExit::Shutdown),
                ],
                Box::new(work),
                7,
            );
            let (status, _) = ring_pvclock_register(&mut vmm, PV_GPA);
            assert_eq!(status, Status::Ok as u16);
            vmm.run().unwrap();
            vmm.state_blob()
        };
        assert_eq!(run(), run());
    }

    /// G2's portable analogue: every V-time intercept re-stamps the page with
    /// exactly the value the trap completed with (the same `guest_clock`
    /// function at the same anchor), including after an `IA32_TSC_ADJUST`
    /// offset write — and the refresh log records the read-back values.
    #[test]
    fn pvclock_refresh_tracks_the_trap_oracle_through_intercepts() {
        let mut vmm = pvclock_vmm(
            vec![
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Arch(X86Exit::Wrmsr {
                    index: IA32_TSC_ADJUST,
                    value: 5,
                }),
                Exit::Arch(X86Exit::Rdtsc),
            ],
            Box::new(ScriptedWork::at(10)),
            7,
        );
        let (status, _) = ring_pvclock_register(&mut vmm, PV_GPA);
        assert_eq!(status, Status::Ok as u16);

        // Step 1: RDTSC at work 10 -> trap value 20; page must match it.
        assert_eq!(vmm.step().unwrap(), Step::Continued);
        let f = vtime::pvclock::read(vmm.pvclock_page().unwrap()).unwrap();
        assert_eq!((f.vns, f.guest_clock), (10, 20));
        let trap_value = match vmm.backend.completions().last().unwrap() {
            Completion::Read(v) => *v,
            other => panic!("RDTSC completes as a read, got {other:?}"),
        };
        assert_eq!(f.guest_clock, trap_value, "page == what the trap returned");
        vmm.pvclock_check_oracle().unwrap();

        // Step 2: the guest writes IA32_TSC_ADJUST = 5 — a V-time MSR intercept;
        // the page must re-publish the offset-adjusted visible clock.
        assert_eq!(vmm.step().unwrap(), Step::Continued);
        let f = vtime::pvclock::read(vmm.pvclock_page().unwrap()).unwrap();
        assert_eq!(f.guest_clock, 25, "guest_clock = ticks(10) + adjust 5");
        vmm.pvclock_check_oracle().unwrap();

        // Step 3: the next RDTSC returns the same 25 (work unchanged), and the
        // value-keyed stamp leaves the page bytes untouched (no epoch churn).
        let seq_before = f.seq;
        assert_eq!(vmm.step().unwrap(), Step::Continued);
        let f = vtime::pvclock::read(vmm.pvclock_page().unwrap()).unwrap();
        assert_eq!((f.guest_clock, f.seq), (25, seq_before));

        // The refresh log recorded the two distinct-value publishes (the
        // registration's canonical stamp is not a refresh entry).
        assert_eq!(vmm.pvclock_refreshes(), &[(10, 10, 20), (10, 10, 25)]);
    }

    /// G2's evidence-integrity bar (the deliberate-fault test the task spec
    /// mandates): a page that diverges from the oracle — here corrupted in
    /// guest RAM after a good stamp, simulating a stamping bug — must FAIL the
    /// oracle check loudly, proving the gate cannot pass vacuously.
    #[test]
    fn pvclock_oracle_check_fails_on_a_corrupted_page() {
        let mut vmm = pvclock_vmm(
            vec![Exit::Arch(X86Exit::Rdtsc)],
            Box::new(ScriptedWork::at(10)),
            7,
        );
        ring_pvclock_register(&mut vmm, PV_GPA);
        vmm.step().unwrap();
        vmm.pvclock_check_oracle().expect("clean page passes");
        // Corrupt the published guest_clock in place.
        let off = PV_GPA as usize + vtime::pvclock::GUEST_CLOCK_OFF;
        vmm.ram.as_mut_bytes()[off] ^= 0xFF;
        assert!(
            matches!(
                vmm.pvclock_check_oracle(),
                Err(VmmError::ContractViolation(_))
            ),
            "a diverged page must fail the G2 check"
        );
        // A frozen page fails too once the clock advances past it (the G3
        // deliberate-fault shape): restore the byte, then move the anchor.
        vmm.ram.as_mut_bytes()[off] ^= 0xFF;
        vmm.pvclock_check_oracle()
            .expect("repaired page passes again");
        vmm.vtime.as_mut().unwrap().last_intercept_work = 999;
        assert!(
            matches!(
                vmm.pvclock_check_oracle(),
                Err(VmmError::ContractViolation(_))
            ),
            "a frozen page must fail once the clock has moved on"
        );
    }

    /// G3's portable analogue: with a page registered and nothing else armed,
    /// the run loop bounds every entry at `anchor + delta` (the staleness
    /// bound), the Deadline landing advances the anchor, and the page follows
    /// within delta — a busy-wait on the page clock cannot hang. Without a
    /// registration the deadline is `None` (page-off arms exactly as before).
    #[test]
    fn pvclock_forced_refresh_bounds_staleness_within_delta() {
        const DELTA: u64 = 1_000;
        let mut vmm = Vmm::new(
            configured_mock(vec![
                Exit::Common(CommonExit::Deadline { reached: Moment(0) }),
                Exit::Common(CommonExit::Deadline { reached: Moment(0) }),
            ]),
            GuestRam::new(TEST_RAM).unwrap(),
        );
        vmm.wire_vtime(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::new()), 7).unwrap(),
        );
        vmm.enable_pvclock(DELTA);
        // Un-registered: no deadline, plain open-ended run (page-off unchanged).
        assert_eq!(vmm.pvclock_refresh_deadline(), None);
        assert_eq!(vmm.run_until_deadline(), None);
        let (status, _) = ring_pvclock_register(&mut vmm, PV_GPA);
        assert_eq!(status, Status::Ok as u16);
        // Registered: the next entry is bounded at anchor + delta.
        assert_eq!(vmm.run_until_deadline(), Some(Moment(DELTA)));
        // The forced refresh lands exactly at the bound (the mock rewrites the
        // scripted Deadline to the requested one), advances the anchor, and the
        // step-tail stamp publishes the advanced clock.
        assert_eq!(vmm.step().unwrap(), Step::Continued);
        let f = vtime::pvclock::read(vmm.pvclock_page().unwrap()).unwrap();
        assert_eq!(f.vns, DELTA, "page advanced to the forced-refresh landing");
        // And the next bound moves forward by delta again — monotonic progress.
        assert_eq!(vmm.run_until_deadline(), Some(Moment(2 * DELTA)));
        assert_eq!(vmm.step().unwrap(), Step::Continued);
        let f = vtime::pvclock::read(vmm.pvclock_page().unwrap()).unwrap();
        assert_eq!(f.vns, 2 * DELTA);
        // Every consecutive pair of refreshes is within delta on the work axis
        // (the G3 harness assertion, portable form).
        let log = vmm.pvclock_refreshes();
        for pair in log.windows(2) {
            assert!(pair[1].0 - pair[0].0 <= DELTA, "staleness bound violated");
        }
    }

    /// §1.1: a seal re-stamps the page to canonical form (`seq = 0`) at the
    /// exact seal values, and a restored sibling continues byte-identically —
    /// while `restore_vm_state` itself clears the (stale-timeline)
    /// registration until the caller re-establishes it.
    #[test]
    fn pvclock_seal_canonicalizes_and_restore_carries_the_registration() {
        let mut a = pvclock_vmm(
            vec![Exit::Arch(X86Exit::Rdtsc), Exit::Arch(X86Exit::Rdtsc)],
            Box::new(ScriptedWork::at(10)),
            7,
        );
        ring_pvclock_register(&mut a, PV_GPA);
        a.step().unwrap(); // RDTSC at work 10: stamped, seq moved
        let live = vtime::pvclock::read(a.pvclock_page().unwrap()).unwrap();
        assert_ne!(live.seq, 0, "a mid-run refresh bumped the epoch");
        // Seal: the page canonicalizes (seq 0, same values) inside save_vm_state.
        let vm_state = a.save_vm_state().unwrap();
        let sealed = vtime::pvclock::read(a.pvclock_page().unwrap()).unwrap();
        assert_eq!(sealed.seq, 0, "sealed page is canonical");
        assert_eq!(
            (sealed.vns, sealed.guest_clock),
            (live.vns, live.guest_clock)
        );
        let image = a.guest_memory().to_vec();

        // Restore into a fresh, like-composed VM.
        let mut b = pvclock_vmm(
            vec![Exit::Arch(X86Exit::Rdtsc)],
            Box::new(ScriptedWork::new()),
            7,
        );
        ring_pvclock_register(&mut b, PV_GPA);
        // Capture the channel state the seal-time carry would (offer + Δ +
        // registration), as the control server does at `snapshot()`.
        let carried = a.pvclock_snapshot().expect("offered");
        b.restore_snapshot(&image, &vm_state).unwrap();
        // The restore cleared the stale-timeline registration...
        assert_eq!(b.pvclock_registration(), None);
        // ...and the caller (the control server, in production) re-establishes
        // and cross-validates the snapshot's own channel state.
        b.pvclock_restore(Some(&carried)).unwrap();
        assert_eq!(b.pvclock_registration(), Some(PV_GPA));
        // The restored page is byte-identical to the sealed one, and the next
        // intercept stamps it exactly as a never-restored run would.
        assert_eq!(
            &b.guest_memory()[PV_GPA as usize..PV_GPA as usize + PVCLOCK_PAGE_LEN],
            &image[PV_GPA as usize..PV_GPA as usize + PVCLOCK_PAGE_LEN],
        );
        b.pvclock_check_oracle().unwrap();
    }

    /// Composition mismatches fail loud, **symmetrically**: an offered
    /// snapshot into an unoffered target (registered or not), an unoffered
    /// snapshot into an offered target, a Δ mismatch, a GPA that no longer
    /// validates, and a registration onto a non-deterministic-clock backend.
    #[test]
    fn pvclock_restore_mismatch_fails_loud() {
        // Channel states as a like-composed source VM would seal them.
        let registered = pvclock_vmm(vec![], Box::new(ScriptedWork::new()), 7);
        let offered_unregistered_snap = registered.pvclock_snapshot().expect("offered");
        let mut src = pvclock_vmm(vec![], Box::new(ScriptedWork::new()), 7);
        ring_pvclock_register(&mut src, PV_GPA);
        let registered_snap = src.pvclock_snapshot().expect("offered");

        // Offered snapshot (even UNREGISTERED) → unoffered target: rejected.
        let mut unoffered = vtime_vmm(vec![], Box::new(ScriptedWork::new()), 7);
        assert!(matches!(
            unoffered.pvclock_restore(Some(&offered_unregistered_snap)),
            Err(VmmError::ContractViolation(_))
        ));
        assert!(matches!(
            unoffered.pvclock_restore(Some(&registered_snap)),
            Err(VmmError::ContractViolation(_))
        ));
        // Unoffered snapshot → unoffered target: fine.
        unoffered.pvclock_restore(None).unwrap();

        // Unoffered snapshot → OFFERED target: rejected (a guest registering
        // here would fork the timeline off the sealed one).
        let mut offered = pvclock_vmm(vec![], Box::new(ScriptedWork::new()), 7);
        assert!(matches!(
            offered.pvclock_restore(None),
            Err(VmmError::ContractViolation(_))
        ));

        // Δ mismatch: rejected (the forced-refresh schedule would diverge).
        let mut other_delta = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        other_delta.wire_vtime(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::new()), 7).unwrap(),
        );
        other_delta.enable_pvclock(PVCLOCK_DEFAULT_DELTA_WORK + 1);
        assert!(matches!(
            other_delta.pvclock_restore(Some(&registered_snap)),
            Err(VmmError::ContractViolation(_))
        ));

        // A GPA that no longer validates on the target (smaller RAM): rejected.
        let mut small = Vmm::new(configured_mock(vec![]), GuestRam::new(0x2000).unwrap());
        small.wire_vtime(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::new()), 7).unwrap(),
        );
        small.enable_pvclock(PVCLOCK_DEFAULT_DELTA_WORK);
        assert!(matches!(
            small.pvclock_restore(Some(&registered_snap)),
            Err(VmmError::ContractViolation(_))
        ));
        assert_eq!(small.pvclock_registration(), None);

        // A registration onto a backend with no deterministic work counter:
        // rejected (the original registration required one).
        let mut caps = MOCK_TEST_CAPS;
        caps.arch.deterministic_tsc = false;
        let mut m = MockBackend::with_capabilities(caps);
        m.set_policy(&X86Policy {
            cpuid: CpuidModel::default(),
            msr_filter: MsrFilter::default(),
        })
        .unwrap();
        let mut no_det = Vmm::new(m, GuestRam::new(TEST_RAM).unwrap());
        no_det.wire_vtime(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::new()), 7).unwrap(),
        );
        no_det.enable_pvclock(PVCLOCK_DEFAULT_DELTA_WORK);
        assert!(matches!(
            no_det.pvclock_restore(Some(&registered_snap)),
            Err(VmmError::ContractViolation(_))
        ));
        // The same target accepts the UNREGISTERED channel state (no stamps
        // will ever be derived until a registration, which the doorbell gate
        // would itself refuse there).
        no_det
            .pvclock_restore(Some(&offered_unregistered_snap))
            .unwrap();
    }

    /// An unknown pvclock opcode answers `UnknownOpcode`; a malformed payload
    /// answers `BadRequest` — never a silent drop, never a registration.
    #[test]
    fn pvclock_doorbell_rejects_bad_frames() {
        let mut vmm = pvclock_vmm(vec![], Box::new(ScriptedWork::new()), 7);
        // Opcode 2 does not exist.
        let mut frame = [0_u8; 64];
        let len = hypercall_proto::encode_request(
            ServiceId::Pvclock,
            2,
            1,
            &PV_GPA.to_le_bytes(),
            &mut frame,
        )
        .unwrap();
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + len].copy_from_slice(&frame[..len]);
        vmm.service_doorbell(len as u32).unwrap();
        let (header, _) = decode(&vmm.ram.as_bytes()[RESP_GPA..RESP_GPA + HC_PAGE]).unwrap();
        assert_eq!(header.status, Status::UnknownOpcode as u16);

        // A 7-byte payload is malformed.
        let len =
            hypercall_proto::encode_request(ServiceId::Pvclock, 1, 2, &[0; 7], &mut frame).unwrap();
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + len].copy_from_slice(&frame[..len]);
        vmm.service_doorbell(len as u32).unwrap();
        let (header, _) = decode(&vmm.ram.as_bytes()[RESP_GPA..RESP_GPA + HC_PAGE]).unwrap();
        assert_eq!(header.status, Status::BadRequest as u16);
        assert_eq!(vmm.pvclock_registration(), None);
    }

    /// The mock's default capabilities, named so the no-deterministic-clock
    /// test can flip one bit without restating the rest.
    const MOCK_TEST_CAPS: vmm_backend::Capabilities<X86Caps> = vmm_backend::Capabilities {
        name: "mock",
        deterministic_rng: true,
        arch: X86Caps {
            deterministic_tsc: true,
            enforces_tsc_deadline_msr: true,
        },
    };
}
