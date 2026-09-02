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
    MAX_PAYLOAD, NetFlowPoint, SDK_COVERAGE_QUANTUM, SDK_COVERAGE_REQUEST_LEN,
    SDK_COVERAGE_RESPONSE_LEN, SeededEntropy, Service, ServiceId, Status, decode, encode_error,
    encode_response,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use vm_state::SnapshotRecords;
use vmm_backend::{Arch, Backend, CommonExit, Exit};
use vtime::{IdlePlanner, VClock, VClockConfig};

use crate::vendor::Vendor;
use crate::virtual_time::LiveVirtualTimeTrace;

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
/// `hypercall_doorbell::REQ_GPA`.
const REQ_GPA: usize = 0x0000_E000;
/// Guest-physical address of the fixed hypercall **response** page. Mirrors
/// `hypercall_doorbell::RESP_GPA`.
const RESP_GPA: usize = 0x0000_F000;
/// The hypercall shared-page size (one frame per page). Mirrors
/// `hypercall_doorbell::PAGE_SIZE` == `hypercall_proto::MAX_FRAME`.
const HC_PAGE: usize = 4096;
/// Base of the dedicated arm64 transport memslot. Apple HVF requires both the
/// GPA and length of a mapped region to use the host's 16-KiB page granule, so
/// the two ABI pages at `0xE000`/`0xF000` ride the upper half of one canonical
/// 16-KiB slot starting at `0xC000`. The lower two pages are retained guest
/// state too: they are mapped, hashed, snapshotted, and restored, never padding
/// hidden from the determinism surface.
const DOORBELL_MAP_GPA: usize = 0x0000_C000;
/// The arm64 control memslot is exactly one 16-KiB HVF page.
const DOORBELL_MAP_LEN: usize = 4 * HC_PAGE;

// The SDK event-id wire layout (task 73), mirrored from `consonance/harmony-linux/sdk/src/wire.rs`
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
    /// The branch's ordered payload tape is exhausted. This is a terminal
    /// cooperating-workload stop, independent of the client's stop mask.
    Quiescent,
    /// An `assert_always` violation (or an `assert_unreachable` reached) — a bug.
    /// `id` is the assertion's catalog point id; `data` its detail bytes.
    Assertion {
        /// The assertion's catalog point id.
        id: u32,
        /// Opaque assertion detail bytes.
        data: Vec<u8>,
    },
    // NB: lifecycle yields no longer surface an immediate `SnapshotPoint` stop —
    // their doorbell OUT is unsealable, so each is **deferred** (see
    // `SdkChannel::pending_snapshot`) to the next synchronized boundary, surfaced
    // by the control loop as `StopReason::SnapshotPoint` there.
}

/// The host-side action a captured SDK Event emission drives, after
/// [`Vmm::classify_sdk_event`] validates its payload (task 73 seam 3, round-14).
#[derive(Clone, Debug, Eq, PartialEq)]
enum SdkEventAction {
    /// Surface a cooperating-SDK stop (an assert violation) as a bug.
    Stop(SdkStop),
    /// A validated lifecycle yield: arm the deferred snapshot point.
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

/// The V-time + seeded-RNG wiring for the deterministic path: the assigned
/// [`VClock`] and the [`SeededEntropy`] stream
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
    pub(crate) entropy: SeededEntropy,
    /// The signed offset added to the base V-time guest clock to form the
    /// **guest-visible** clock (`visible = VClock::guest_ticks + offset`, wrapping
    /// mod 2⁶⁴ as the architectural 64-bit counter does). `0` at reset and for
    /// every audited payload, so the visible clock is exactly
    /// `VClock::guest_ticks()`. The vendor's clock-offset register writes it
    /// (x86: `IA32_TSC_ADJUST`, and a `WRMSR(IA32_TSC, X)` that sets the visible
    /// clock to `X`). Stored as `u64` (two's-complement); hashed (it governs
    /// future clock output).
    pub(crate) guest_clock_offset: u64,
}

impl VtimeWiring {
    /// Build assigned-at-exit V-time wiring.
    pub fn new_virtual_time(cfg: VClockConfig, seed: u64) -> Result<VtimeWiring, VmmError> {
        Ok(VtimeWiring {
            cfg,
            clock: VClock::new(cfg)?,
            entropy: SeededEntropy::new(seed),
            guest_clock_offset: 0,
        })
    }

    /// Assign `vns_delta` at an exit. This is the only ordinary clock mutation
    /// used by the virtual-time run loop and saturates rather than wrapping.
    pub fn advance_virtual_time(&mut self, vns_delta: u64) {
        self.clock.advance(vns_delta);
    }

    /// Current assigned V-time.
    pub fn virtual_time_vns(&self) -> u64 {
        self.clock.vns()
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

    /// The **guest-visible** clock: the assigned base V-time guest clock plus
    /// [`guest_clock_offset`](Self::guest_clock_offset), wrapping mod 2⁶⁴ as the
    /// architectural 64-bit counter does. Every guest clock read the vendor
    /// dispatches (x86: `RDTSC`, `RDTSCP`, `RDMSR(IA32_TSC)`) resolves to this, so
    /// they agree exactly; with the default zero offset it is exactly
    /// `VClock::guest_ticks()`.
    pub(crate) fn guest_clock(&self) -> u64 {
        self.clock
            .guest_ticks()
            .wrapping_add(self.guest_clock_offset)
    }
}

/// A V-time snapshot for mid-run save/restore (INTEGRATION.md §4): the effective
/// V-time in whole nanoseconds, the `IA32_TSC_ADJUST` register, and the entropy
/// stream position. On restore `vns` becomes the clock's new starting value, so
/// the guest clock continues exactly, the offset is re-applied, and the RNG
/// stream resumes where it left off.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VtimeSnapshot {
    /// The exact effective V-time in whole nanoseconds at the snapshot point.
    pub vns: u64,
    /// The guest clock-offset register at snapshot time (x86: `IA32_TSC_ADJUST`;
    /// the contract places it in `vm_state`), so a guest that wrote it
    /// snapshots/restores faithfully.
    pub guest_clock_offset: u64,
    /// `SeededEntropy::save_state()` (the PRNG position).
    pub entropy: Vec<u8>,
}

/// Upper bound on diagnostic event traces. Recording stops at the cap so a
/// long-running guest cannot grow host-only observability state without bound.
const EVENT_TRACE_CAP: usize = 4096;

/// Upper bound on the diagnostic pvclock refresh log
/// ([`Vmm::pvclock_refreshes`]) — the same cap as the landing traces. A gate
/// asserting per-refresh properties over a window must re-arm the log at the
/// window's start ([`Vmm::pvclock_clear_refreshes`]) and treat a saturated
/// window (`len() == this`) as a measurement failure, never a pass: a full
/// log proves only that at least this many refreshes happened, not that any
/// bound held.
pub const PVCLOCK_REFRESH_TRACE_CAP: usize = EVENT_TRACE_CAP;

/// Which stamp [`Vmm::pvclock_stamp`] writes: the mid-run seqlock refresh, or
/// the one-shot canonical form written at **registration** (§1.1 — `seq = 0`,
/// zeroed tail). Canonical is a registration-only form: applying it to a page a
/// running guest may be mid-read of is an ABA (see
/// [`vtime::pvclock::stamp_canonical`]), so the seal path does not use it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StampKind {
    Refresh,
    Canonical,
}

fn synchronous_checkpoint_due(checkpoint: bool, deferred: bool) -> bool {
    checkpoint && !deferred
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
    /// The exact next basic-block count each stable logical thread must report.
    /// Absent means the protocol-defined initial threshold of one.
    coverage_thresholds: BTreeMap<u32, u64>,
    /// Coverage scheduling evidence in arrival order:
    /// `(moment, thread, observed, ready, selected)`.
    coverage: Vec<(u64, u32, u64, u32, u32)>,
    /// A pending SDK stop to surface at the next step boundary.
    pending_stop: Option<SdkStop>,
    /// A `setup_complete` was seen but its doorbell `OUT` is **not** a sealable
    /// point (PMU-host-noisy V-time — `save_vm_state` would report
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

/// The paravirtual clock channel (`docs/PARAVIRT-CLOCK.md`):
/// the host side of the materialized clock page. Offered per composition by
/// [`Vmm::enable_pvclock`]; the **guest** opts in by publishing its page GPA
/// over the hypercall doorbell ([`hypercall_proto::ServiceId::Pvclock`]), after
/// which the run loop re-stamps the page at every deterministic clock-advance
/// boundary ([`Vmm::pvclock_refresh`]). A guest that never
/// registers gets exactly today's behavior — no stamp is ever written — and
/// an un-offered composition is byte-for-byte
/// unchanged (the doorbell stays default-deny for it).
///
/// **State identity**: the page *bytes* live in guest RAM (already inside
/// `MEM\0`); the channel registration folds into
/// [`Vmm::state_blob`] as the `PVCK` chunk when offered — it governs future
/// guest-visible time, so two states identical in RAM but differing here must
/// hash differently (the SDK fault-policy precedent). Across snapshot/branch
/// the configuration is carried and cross-validated by the control server
/// ([`Vmm::pvclock_snapshot`] / [`Vmm::pvclock_restore`]), like the SDK
/// channel; the diagnostic refresh log stays out of the hash (like the
/// landing traces).
pub(crate) struct PvclockChannel {
    /// The registered page GPA (page-aligned, wholly inside guest RAM, clear
    /// of the doorbell frame pages — validated at registration). `None` until
    /// the guest publishes one; **one-shot** — re-registration is rejected as
    /// a guest fault and the stamping target never moves for the machine's
    /// life (the PR #110 r2 GPA ruling). Set at the doorbell `OUT`, but only
    /// *pending* until the handshake completes ([`armed`](Self::armed)).
    gpa: Option<u64>,
    /// Whether the registration handshake has completed (the r8 ruling). The
    /// doorbell `OUT` records the GPA (pending); the **first stamp** happens
    /// only at the **handshake intercept** — the guest's
    /// required post-doorbell V-time intercept (the reference kernel's RDTSC,
    /// now protocol, not courtesy), whose anchor is deterministic by construction.
    /// Nothing publishes in between: a doorbell `OUT` is a PIO, not a V-time
    /// intercept, so arming or stamping off it (or off the possibly-stale
    /// pre-`OUT` anchor) would risk publishing a non-fresh — and, in the
    /// host-noisy — clock. A guest that never performs
    /// the handshake is **out of contract**: its page stays at the pre-
    /// registration bytes (stale but deterministic).
    /// Restore sets this `true` directly — a restored VM's anchor is exactly 0,
    /// a synchronized boundary by construction, so it needs no handshake.
    armed: bool,
    /// Diagnostic refresh log (**not** hashed): `(vns, guest_clock)` for
    /// every *value-publishing* stamp, **read back from the page bytes** after
    /// the write — so a stamping bug (wrong offset, wrong endianness, torn
    /// write) surfaces as a log/oracle mismatch, not a silently-wrong page.
    /// The G2 gate's evidence. Capped at [`PREEMPTION_TRACE_CAP`].
    refreshes: Vec<(u64, u64)>,
}

/// The SDK channel's **replay-relevant** state, captured with a snapshot (task
/// 73): the seeded stream position and the emitted event log. Held by the
/// control server keyed by snapshot handle; restored on branch/replay so a fork
/// from a mid-run SDK snapshot reproduces (the seeded streams continue from the
/// right position) and keeps the declared catalog. Kilobytes, not a full state.
#[derive(Clone, Debug)]
pub struct SdkSnapshot {
    /// The seeded stream position (buggify fault + entropy supply), 16 bytes.
    pub(crate) stream: [u8; 16],
    /// The `Moment`-stamped event log emitted up to the snapshot (incl. the
    /// declared catalog), which a fork carries forward.
    pub(crate) events: Vec<(u64, u32, Vec<u8>)>,
    /// The deferred `setup_complete` snapshot-point flag
    /// ([`SdkChannel::pending_snapshot`]). Round-8 folds this into `state_blob`
    /// (the hash), so a verbatim replay MUST restore it — a snapshot sealed while
    /// it is `true` (an unarmed run that ran past `setup_complete` to a later
    /// sealable boundary) would otherwise restore to a state that hashes
    /// differently (the deferred point silently lost), breaking replay's
    /// round-trip hash equality.
    pub(crate) pending_snapshot: bool,
    /// Canonical ordered-input state: only the unconsumed payload suffix.
    /// `None` means the service was not offered; `Some([])` is offered and
    /// exhausted.
    pub(crate) payloads: Option<Vec<Vec<u8>>>,
    /// Per-logical-thread next coverage thresholds. The guest's counters live
    /// in RAM; these host-side expectations govern whether its next callback is
    /// accepted, so both sides are replay-relevant.
    pub(crate) coverage_thresholds: BTreeMap<u32, u64>,
}

impl SdkSnapshot {
    /// Clone the unconsumed ordered payload suffix captured by this snapshot.
    pub(crate) fn remaining_payloads(&self) -> Option<Vec<Vec<u8>>> {
        self.payloads.clone()
    }
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
    pub(crate) decisions: Vec<(u64, u64, environment::Answer)>,
}

/// The task-110 pvclock channel's **replay-relevant** state, captured with a
/// snapshot ([`Vmm::pvclock_snapshot`], `Some` iff the page is offered): the
/// guest's registration and availability. A restore carries and
/// cross-validates them; the page bytes themselves ride the RAM
/// image. Held by the control server keyed by snapshot handle, like
/// [`SdkSnapshot`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PvclockSnapshot {
    /// The registered page GPA, if the guest had published one.
    pub(crate) gpa: Option<u64>,
    /// Whether the sealing VM could actually *register* a page
    /// ([`Vmm::pvclock_available`]: offered + V-time + a deterministic work
    /// counter). Carried because a snapshot taken **before** registration has no
    /// GPA to re-validate, yet still promises a future: restore it onto a VM
    /// whose backend has no deterministic counter and the guest's next
    /// registration — which succeeded on the source — answers `UnknownService`
    /// (cross-model r5 P1). Cross-validated for *equality* on restore, so the
    /// reverse (a child that can register where its parent could not) fails
    /// loud too.
    pub(crate) registrable: bool,
}

pub struct Vmm<B: Backend>
where
    B::A: Vendor,
{
    pub(crate) backend: B,
    pub(crate) ram: RamBacking,
    /// Guest-physical base of the main RAM memslot: `0` on x86; `RAM_BASE` on
    /// arm64 (whose RAM sits high). The engine resolves absolute guest GPAs — the
    /// hypercall-transport ABI pages (`REQ_GPA`/`RESP_GPA`) — to a RAM offset
    /// through this, so the transport magic (the absolute ABI GPAs) stays
    /// vendor-invariant (`tasks/112`: "the transport magic is unchanged").
    pub(crate) ram_base_gpa: u64,
    /// The hypercall-transport ABI pages when they are a **dedicated low-GPA
    /// memslot** rather than part of the main RAM — the arm64 case, where RAM is
    /// high so the absolute ABI GPAs fall below it. `None` on x86 (the pages live
    /// inside the GPA-0 RAM) and on an arm64 boot that never mapped them (the
    /// doorbell then fails closed, never reads the wrong RAM offset). Mapped by
    /// [`Vmm::map_doorbell_pages`].
    pub(crate) doorbell_pages: Option<GuestRam>,
    /// The vendor's device state ([`Vendor::Devices`]) — the interrupt fabric,
    /// the platform shims, and the serial device. The engine never names one; it
    /// reaches them only through [`Vendor`], which is what makes the engine
    /// compiler-provably arch-blind (`docs/ARCH-BOUNDARY.md` §B).
    pub(crate) devices: <B::A as Vendor>::Devices,
    /// Guest frame numbers written **host-side** since the last
    /// [`Vmm::reset_dirty_tracking`] / [`Vmm::drain_dirty_pages`] drain (task 95
    /// M2.1). The backend's dirty log sees only *guest* writes (KVM tracks sptes,
    /// not the userspace mapping), so every place vmm-core itself writes guest
    /// RAM — the doorbell response page, a `CorruptMemory` host fault — records
    /// the touched gfns here, and the drain unions them in. A `BTreeSet` so no
    /// order can reach the (already order-insensitive) capture. **Not hashed**
    /// (host bookkeeping, like the exit counters); the writes themselves are what
    /// the hash sees.
    pub(crate) host_dirty: std::collections::BTreeSet<u64>,
    /// Latched when guest RAM was host-written **wholesale or untrackably**
    /// ([`Vmm::restore_guest_memory`]'s full-image overwrite). While set,
    /// [`Vmm::drain_dirty_pages`] answers `None` — the safety rule: a dirty set
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
    /// Diagnostic trace of the idle-resume landings (task 52): the **V-time** (ns) the
    /// clock was warped to when the guest went idle (`CommonExit::Idle` with `RFLAGS.IF == 1`
    /// and an armed timer) and [`Self::resume_idle`] jumped to the timer deadline. The
    /// dual of [`Self::virtual_time_trace`] — *jumped to* the next event instead of
    /// *executed to* it. It records the **landed V-time** (the deadline), **not** a work
    /// count: a `HLT` live work read is host-noisy (task-27 O1), so the idle path never
    /// reads it; the landing is derived deterministic from the last-intercept anchor + the
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
    /// Host-side production trace for assigned-at-exit V-time. This is oracle
    /// evidence only: it is deliberately excluded from snapshots and hashes,
    /// while the device state and assigned clock that produced it remain part
    /// of both.
    pub(crate) virtual_time_trace: Option<LiveVirtualTimeTrace>,
    /// Count of exact hypercall-doorbell rings. This is host-only profile
    /// evidence, deliberately excluded from VM state, hashes, and snapshots;
    /// unlike [`Vmm::exit_counts`], it does not include unrelated port I/O or
    /// MMIO exits.
    pub(crate) doorbell_exits: u64,
    /// Host-only performance mode: checkpoint events retain their exact state
    /// but leave the hash slot empty for the caller to fill from an owned
    /// [`Vmm::state_blob`] on a worker. Default-off preserves synchronous trace
    /// semantics for every existing composition.
    pub(crate) deferred_virtual_time_checkpoints: bool,
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
    /// A `setup_complete` doorbell requested a deferred snapshot point and the
    /// guest has not yet re-entered to commit that userspace-I/O completion.
    /// Cleared only after a successful subsequent backend entry. This is a
    /// host-control latch, not guest or replay state.
    pub(crate) sdk_snapshot_reentry_required: bool,
    /// `true` when the current point is a **V-time intercept boundary** — the last
    /// serviced exit was a V-time intercept (RDTSC/RDTSCP/RDRAND/RDSEED or a TSC
    /// MSR), or the VM is fresh (work 0) — so the **exact** effective V-time is known:
    /// `assigned_clock` is the current, deterministic work. At any other exit
    /// (HLT/PIO/CPUID) the work retired since the last intercept is not
    /// deterministically measurable (exit-boundary variability), so the exact V-time is unknown.
    /// [`Vmm::save_vtime`] requires this (a snapshot's `vns` must be exact — restore
    /// resumes the TSC from it; §4), failing closed otherwise rather than recording a
    /// stale `vns`. Set `false` **before** each `step`'s `backend.run()` (so a failed
    /// run leaves it `false`, not stale-`true`) and back to `true` only by a
    /// V-time-intercept completion. **Not** part of the hash — `state_blob` is
    /// replay-equivalence *to the last intercept* and is correct at any exit (see
    /// [`encode_vtime`]); only the *snapshot* needs exactness here.
    /// Whether the synchronized boundary this step ended on was reached by an
    /// **RDTSC/RDTSCP counter read specifically** (`complete_tsc`) — as opposed to
    /// the other V-time intercepts (a TSC MSR, RDRAND/RDSEED) or the other
    /// synchronized points (a deadline landing, an idle-warp restore). Gates the
    /// pvclock registration **handshake** (r17): the wire contract (§3.1, r8)
    /// promises the guest publishes its page GPA over the doorbell and then does a
    /// **counter read**, so only that read may complete the handshake (stamp + arm
    /// the pending page). A superset of nothing — it is a strict subset of
    /// `clock_boundary` (every RDTSC boundary is synchronized, but not vice
    /// versa). Cleared with `clock_boundary` before each entry; set `true`
    /// **only** by [`complete_tsc`](crate::vendor::x86::dispatch). Not hashed (a
    /// transient run-control flag).
    pub(crate) tsc_read_intercept: bool,
    /// When set ([`Vmm::wire_snapshot_hashing`]), [`Vmm::state_blob`] folds the
    /// **canonical `vm_state` encoding** into the hash as a `VMST` chunk — the
    /// snapshot/branch path's "the canonical `vm_state` blob drives `state_hash`"
    /// (BRINGUP). Default **off**, so M1/M2/corpus/Linux-boot blobs are byte-for-
    /// byte unchanged (their goldens do not move); a snapshot/branch consumer opts
    /// in. The chunk is the same bytes a [`Vmm::save_vm_state`] would seal, so two
    /// states whose canonical blob differs hash differently.
    pub(crate) snapshot_hashing: bool,
    /// Earliest host-scheduled event that may wake an idle guest. This is only
    /// consulted after an `Idle` exit; normal execution always runs to the next
    /// VM exit and never asks the backend to stop at this value.
    pub(crate) idle_wake_vns: Option<u64>,
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
    /// pvclock service and no page is ever stamped.
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
    /// **Diagnostic only**: the live vCPU record set (a pure [`Backend::save`],
    /// running no guest code), so a determinism localizer can print the exact
    /// differing registers/MSRs that [`Vmm::state_components`] only digests.
    pub fn vcpu_record(&self) -> Result<VcpuOf<B>, VmmError> {
        Ok(self.backend.save()?)
    }

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
            ram_base_gpa: 0,
            doorbell_pages: None,
            devices: <B::A as Vendor>::new_devices(),
            host_dirty: std::collections::BTreeSet::new(),
            host_dirty_wholesale: false,
            report_stream: Vec::new(),
            idle_landings: Vec::new(),
            terminal: None,
            saved_state: None,
            vtime: None,
            virtual_time_trace: None,
            doorbell_exits: 0,
            deferred_virtual_time_checkpoints: false,
            rng_completion_staged: false,
            completion_staged: false,
            sdk_snapshot_reentry_required: false,
            // A fresh VM is at work 0: the effective V-time is exactly `vns_base`, so
            // a snapshot here is exact (synchronized).
            // No intercept has happened yet, let alone a counter read — a fresh VM's
            // synchronized boundary is not an RDTSC handshake.
            tsc_read_intercept: false,
            snapshot_hashing: false,
            idle_wake_vns: None,
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
        self.virtual_time_trace = Some(LiveVirtualTimeTrace::default());
        self.vtime = Some(wiring);
        self
    }

    /// Production normalized trace, present only for assigned-at-exit V-time.
    pub fn virtual_time_trace(&self) -> Option<&LiveVirtualTimeTrace> {
        self.virtual_time_trace.as_ref()
    }

    /// Close the current host-only trace segment while keeping tracing enabled
    /// on this same VM. Restore-in-place uses this to preserve the existing
    /// per-branch session segmentation without replacing the VMM.
    pub(crate) fn take_virtual_time_trace(&mut self) -> Option<LiveVirtualTimeTrace> {
        self.virtual_time_trace.as_mut().map(std::mem::take)
    }

    /// Defer sparse virtual-time checkpoint hashing so a host runner can hash
    /// owned [`state_blob`](Self::state_blob) captures in parallel and install
    /// the byte-identical results with
    /// [`checkpoint_virtual_time_trace_at`](Self::checkpoint_virtual_time_trace_at).
    ///
    /// This is host-side evidence plumbing only: it changes neither guest state
    /// nor the normalized event sequence. It must be enabled before the first
    /// event, and every due checkpoint must be installed before the trace is
    /// accepted or encoded.
    ///
    /// # Errors
    /// Returns [`VmmError::ContractViolation`] when virtual time is not wired or
    /// the trace already contains an event.
    pub fn defer_virtual_time_checkpoint_hashes(&mut self) -> Result<(), VmmError> {
        let trace = self.virtual_time_trace.as_ref().ok_or_else(|| {
            VmmError::ContractViolation(
                "deferred checkpoint hashing without virtual_time trace".to_string(),
            )
        })?;
        if !trace.raw_log().is_empty() || !trace.normalized_log().events.is_empty() {
            return Err(VmmError::ContractViolation(
                "deferred checkpoint hashing enabled after the trace started".to_string(),
            ));
        }
        self.deferred_virtual_time_checkpoints = true;
        Ok(())
    }

    /// Install one deferred sparse checkpoint hash at its exact portable event.
    ///
    /// # Errors
    /// Returns [`VmmError::ContractViolation`] unless deferred mode is enabled,
    /// the event is a due 256-event checkpoint, the event exists, and its hash
    /// slot is still empty.
    pub fn checkpoint_virtual_time_trace_at(
        &mut self,
        event_index: u64,
        state_hash: [u8; 32],
    ) -> Result<(), VmmError> {
        if !self.deferred_virtual_time_checkpoints {
            return Err(VmmError::ContractViolation(
                "deferred checkpoint installed while synchronous hashing is active".to_string(),
            ));
        }
        if !event_index.saturating_add(1).is_multiple_of(256) {
            return Err(VmmError::ContractViolation(format!(
                "deferred checkpoint event {event_index} is not a 256-event boundary"
            )));
        }
        self.virtual_time_trace
            .as_mut()
            .ok_or_else(|| {
                VmmError::ContractViolation(
                    "deferred checkpoint installed without virtual_time trace".to_string(),
                )
            })?
            .checkpoint_at(event_index, state_hash)
            .map_err(|message| VmmError::ContractViolation(message.to_string()))
    }

    /// Attach the current full-state hash to the trace's final event.
    ///
    /// # Errors
    /// Returns [`VmmError::ContractViolation`] when the production trace is not
    /// wired or no exit has completed.
    pub fn checkpoint_virtual_time_trace(&mut self) -> Result<(), VmmError> {
        let hash = self.state_hash();
        self.virtual_time_trace
            .as_mut()
            .ok_or_else(|| {
                VmmError::ContractViolation(
                    "virtual_time trace checkpoint without virtual_time V-time".to_string(),
                )
            })?
            .checkpoint_last(hash)
            .map_err(|message| VmmError::ContractViolation(message.to_string()))
    }

    /// Record a guest-clock deadline in the independent production schedule.
    pub(crate) fn trace_arm_clockevent_schedule(
        &mut self,
        deadline_ticks: u64,
        interrupt_id: u32,
    ) -> Result<(), VmmError> {
        let deadline_vns = self.guest_clock_deadline_vns(deadline_ticks)?;
        self.trace_clockevent_schedule_vns(deadline_vns, interrupt_id)
    }

    /// Record an absolute V-ns deadline in the independent production schedule.
    /// The x86 LAPIC timer's deadlines are already V-ns, so no guest-clock
    /// conversion applies (the arm64 clockevent converts ticks via
    /// [`Self::trace_arm_clockevent_schedule`]). Scheduling implicitly cancels
    /// any active schedule, so a re-arm is one call.
    pub(crate) fn trace_clockevent_schedule_vns(
        &mut self,
        deadline_vns: u64,
        interrupt_id: u32,
    ) -> Result<(), VmmError> {
        let Some(trace) = self.virtual_time_trace.as_mut() else {
            return Ok(());
        };
        trace
            .schedule_clockevent(deadline_vns, interrupt_id)
            .map_err(|message| VmmError::ContractViolation(message.to_string()))
    }

    /// Convert an absolute guest-clock tick deadline to the first whole V-ns
    /// at which that counter value is observable.
    pub(crate) fn guest_clock_deadline_vns(&self, deadline_ticks: u64) -> Result<u64, VmmError> {
        let vt = self.vtime.as_ref().ok_or_else(|| {
            VmmError::ContractViolation(
                "virtual_time clockevent schedule without V-time wiring".to_string(),
            )
        })?;
        if vt.guest_clock_offset != 0 {
            return Err(VmmError::ContractViolation(
                "virtual_time clockevent schedule with nonzero guest-clock offset".to_string(),
            ));
        }
        let ticks = deadline_ticks.saturating_sub(vt.cfg.guest_base);
        let numerator = u128::from(ticks) * 1_000_000_000_u128;
        let hz = u128::from(vt.cfg.guest_hz);
        if hz == 0 {
            return Err(VmmError::ContractViolation(
                "virtual_time clockevent schedule with zero guest frequency".to_string(),
            ));
        }
        let deadline_vns = numerator
            .saturating_add(hz - 1)
            .checked_div(hz)
            .unwrap_or(u128::MAX)
            .min(u128::from(u64::MAX)) as u64;
        Ok(deadline_vns)
    }

    /// Mark the active production clockevent schedule canceled at this exit.
    pub(crate) fn trace_clockevent_cancel(&mut self) -> Result<(), VmmError> {
        let Some(trace) = self.virtual_time_trace.as_mut() else {
            return Ok(());
        };
        trace
            .cancel_clockevent()
            .map_err(|message| VmmError::ContractViolation(message.to_string()))
    }

    /// Record that a due ARM clockevent remained masked at this exit and is
    /// first eligible again at the next normalized event.
    pub(crate) fn trace_arm_clockevent_defer(&mut self) -> Result<(), VmmError> {
        let Some(trace) = self.virtual_time_trace.as_mut() else {
            return Ok(());
        };
        trace
            .defer_clockevent()
            .map_err(|message| VmmError::ContractViolation(message.to_string()))
    }

    /// Bind a delivered clockevent to the schedule active at this exit.
    pub(crate) fn trace_clockevent_delivery(&mut self) -> Result<(), VmmError> {
        let Some(trace) = self.virtual_time_trace.as_mut() else {
            return Ok(());
        };
        trace
            .deliver_clockevent()
            .map_err(|message| VmmError::ContractViolation(message.to_string()))
    }

    /// Assign V-time at a serviced exit exactly once per classified exit.
    pub(crate) fn advance_virtual_time_vtime(&mut self, delta_vns: u64) -> Result<(), VmmError> {
        let Some(vt) = self.vtime.as_mut() else {
            return Err(VmmError::ContractViolation(
                "virtual_time exit advancement without V-time wiring".to_string(),
            ));
        };
        vt.advance_virtual_time(delta_vns);
        Ok(())
    }

    /// `true` once the determinism V-time path is wired.
    pub fn vtime_wired(&self) -> bool {
        self.vtime.is_some()
    }

    /// Whether this VM uses assigned-at-exit V-time.
    pub(crate) fn virtual_time_vtime_enabled(&self) -> bool {
        self.vtime.is_some()
    }

    /// Map the hypercall-transport ABI pages (`REQ_GPA`/`RESP_GPA`) in a
    /// **dedicated 16-KiB low-GPA memslot**, backed by a buffer this `Vmm` owns. For a
    /// machine whose main RAM does not start at GPA 0 (arm64, RAM at `RAM_BASE`)
    /// the absolute ABI GPAs fall below the RAM and cannot be its offset
    /// `REQ_GPA`; mapping them here keeps the transport magic unchanged
    /// (`tasks/112`) with no per-arch GPA translation. The arm64 composition root
    /// calls this; x86 keeps the pages inside its GPA-0 RAM and never does.
    pub(crate) fn map_doorbell_pages(&mut self) -> Result<(), VmmError> {
        let mut pages = GuestRam::new(DOORBELL_MAP_LEN)?;
        // SAFETY: `pages` is moved into `self.doorbell_pages` below; its backing
        // is pinned (an mmap/Vec that does not move when `self` moves) and lives
        // for the backend's lifetime because `self` owns it, and the run loop
        // holds `&mut self` so it is never aliased mid-run — the `map_memory`
        // contract, exactly as the main RAM mapping upholds it.
        unsafe {
            self.backend.map_memory(
                vmm_backend::Gpa(DOORBELL_MAP_GPA as u64),
                pages.as_mut_bytes(),
            )?;
        }
        self.doorbell_pages = Some(pages);
        Ok(())
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

    /// Write a batch of complete 4-KiB pages into the main guest-RAM backing.
    ///
    /// Each GFN is relative to the main RAM memslot, including when that slot
    /// has a non-zero guest-physical base. The complete batch is validated
    /// before any byte is written, so malformed input is atomic.
    ///
    /// # Errors
    /// Returns [`VmmError::ContractViolation`] for a duplicate or out-of-range
    /// GFN, a malformed RAM backing, or address arithmetic that cannot be
    /// represented.
    pub fn write_guest_pages(&mut self, pages: &[(u64, [u8; 4096])]) -> Result<(), VmmError> {
        const PAGE_SIZE: usize = 4096;
        const PAGE_SIZE_U64: u64 = PAGE_SIZE as u64;

        if pages.is_empty() {
            return Ok(());
        }
        let ram_len = self.ram.len();
        if ram_len == 0 || !ram_len.is_multiple_of(PAGE_SIZE) {
            return Err(VmmError::ContractViolation(format!(
                "write_guest_pages: guest RAM length {ram_len} is not a non-zero multiple of {PAGE_SIZE}"
            )));
        }
        if !self.ram_base_gpa.is_multiple_of(PAGE_SIZE_U64) {
            return Err(VmmError::ContractViolation(format!(
                "write_guest_pages: main RAM base GPA {:#x} is not page-aligned",
                self.ram_base_gpa
            )));
        }

        let mut seen = std::collections::BTreeSet::new();
        let mut validated = Vec::with_capacity(pages.len());
        for (gfn, page) in pages {
            if !seen.insert(*gfn) {
                return Err(VmmError::ContractViolation(format!(
                    "write_guest_pages: duplicate GFN {gfn}"
                )));
            }
            let gfn_usize = usize::try_from(*gfn).map_err(|_| {
                VmmError::ContractViolation(format!(
                    "write_guest_pages: GFN {gfn} does not fit the host address space"
                ))
            })?;
            let offset = gfn_usize.checked_mul(PAGE_SIZE).ok_or_else(|| {
                VmmError::ContractViolation(format!(
                    "write_guest_pages: GFN {gfn} page offset overflows the host address space"
                ))
            })?;
            let end = offset.checked_add(PAGE_SIZE).ok_or_else(|| {
                VmmError::ContractViolation(format!(
                    "write_guest_pages: GFN {gfn} page end overflows the host address space"
                ))
            })?;
            if end > ram_len {
                return Err(VmmError::ContractViolation(format!(
                    "write_guest_pages: GFN {gfn} is outside guest RAM ({}) pages",
                    ram_len / PAGE_SIZE
                )));
            }
            let offset_u64 = u64::try_from(offset).map_err(|_| {
                VmmError::ContractViolation(format!(
                    "write_guest_pages: GFN {gfn} page offset does not fit a GPA"
                ))
            })?;
            let gpa = self.ram_base_gpa.checked_add(offset_u64).ok_or_else(|| {
                VmmError::ContractViolation(format!(
                    "write_guest_pages: GFN {gfn} GPA overflows the guest address space"
                ))
            })?;
            if !gpa.is_multiple_of(PAGE_SIZE_U64) || gpa.checked_add(PAGE_SIZE_U64).is_none() {
                return Err(VmmError::ContractViolation(format!(
                    "write_guest_pages: GFN {gfn} has an invalid GPA range"
                )));
            }
            validated.push((offset, end, page));
        }

        let ram = self.ram.as_mut_bytes();
        for &(offset, end, page) in &validated {
            ram[offset..end].copy_from_slice(page);
        }
        // KVM does not log userspace writes through the mapped backing. Refuse
        // to vouch for a delta until the restore caller resets the baseline.
        self.host_dirty_wholesale = true;
        Ok(())
    }

    /// Retire a userspace completion left in the backend's exit buffer without
    /// advancing the guest to its next instruction.
    ///
    /// This is the in-place restore boundary: after the backend confirms the
    /// old transaction is consumed, the VMM's completion and deferred SDK
    /// latches no longer belong to the state that will be restored.
    pub(crate) fn retire_pending_completion(&mut self) -> Result<(), VmmError> {
        if !self.completion_staged {
            return Ok(());
        }
        self.backend.retire_pending_completion()?;
        self.completion_staged = false;
        self.rng_completion_staged = false;
        self.sdk_snapshot_reentry_required = false;
        Ok(())
    }

    /// Inject bytes on the guest's serial input (the 8250 RBR) — the crude,
    /// off-record transport of task 81's `exec` improvisation. The bytes are
    /// consumed FIFO by the guest's serial shell as it reads the RBR; while any are
    /// queued, the COM1 receive line asserts (so an interrupt-driven console picks
    /// them up). **No determinism guarantee**: `exec` taints its timeline by ruling
    /// (git history's `docs/RESOLUTION.md`), so this input is never recorded, hashed, or
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
        // poison the drain (fail closed to the full scan) until the caller
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
    /// synchronized, deterministic point where `assigned_clock` *is* the current
    /// work. At any other exit (HLT/PIO/CPUID) the work retired since the last
    /// intercept is **not deterministically measurable** (exit-boundary variability; the box O1 evidence
    /// shows a terminal live read diverges), so the exact V-time is unknown and
    /// `save_vtime` **fails closed** (`clock_boundary == false`) rather than record
    /// a stale `vns`. (Project rule: never silently wrong.) **Integrator/design note:**
    /// this constrains the control plane to snapshot at V-time-intercept boundaries —
    /// the dissonance design snapshots at quiescent `HLT`, which is *not* such a point,
    /// so it needs either a backend deterministic quiescent work read (not established
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
            Some(vt) => Ok(Some(VtimeSnapshot {
                vns: vt.clock.vns(),
                guest_clock_offset: vt.guest_clock_offset,
                entropy: vt.entropy.save_state(),
            })),
        }
    }

    /// The **effective V-time** in whole nanoseconds — `snapshot_vns` of the
    /// deterministic last-intercept anchor, i.e. exactly the V-time the `VTIM`
    /// hash chunk folds in (see [`Vmm::state_blob`]) — or `None` when the
    /// determinism path is not wired. Exit-boundary variability-free (never a live counter read) and
    /// identical across same-seed runs at the same point, so the control
    /// transport's `run(until)` deadline check (task 58) can compare it against a
    /// V-time deadline without perturbing determinism. Unlike
    /// [`Vmm::save_vtime`] it is **total**: at a non-synchronized point it
    /// reports the last-intercept V-time (a lower bound on the true V-time) —
    /// fine for a monotone deadline check, but never a snapshot's `vns` (that
    /// exactness is `save_vtime`'s job, which fails closed instead).
    pub fn effective_vns(&self) -> Option<u64> {
        self.vtime.as_ref().map(|vt| vt.clock.vns())
    }

    /// `true` iff [`effective_vns`](Vmm::effective_vns) is **exact** — the VM is at a
    /// V-time-intercept boundary (`RDTSC`/`RDTSCP`/`RDRAND`/`RDSEED` / a TSC MSR / an
    /// serviced-exit boundary, an idle warp, or fresh / just-restored), so
    /// `assigned_clock` *is* the current exit count. At any other point (a
    /// terminal `HLT`, a `Shutdown`/debug exit, a serial/MMIO exit) the guest may
    /// have VM exits since the last intercept, so `effective_vns` is only a
    /// **lower bound** and this is `false`.
    ///
    /// The control plane (PR #51 round-7) requires this wherever it trusts
    /// `effective_vns` as an exact position — the `perturb` floor and the `m == vns`
    /// exit-boundary drain — so a fault is never recorded at a `Moment` the guest has
    /// already executed past (the same exactness [`save_vtime`](Vmm::save_vtime) fails
    /// closed on). `false` when V-time is unwired.
    /// The **boundary** preconditions [`Vmm::save_vm_state`] requires to seal a
    /// snapshot: no staged RNG completion (`rng_completion_staged`), and — when
    /// V-time is wired — at a `clock_boundary` intercept. This is the SINGLE
    /// source of truth both `save_vm_state` and the deferred SDK snapshot-point
    /// gate ([`Vmm::take_snapshot_point`]) consult, so "can I seal
    /// here?" can never drift from what `save_vm_state` actually accepts (round-4
    /// P1: the snapshot point used to gate on exact time alone, which
    /// does NOT exclude a staged RNG completion, so it surfaced points the seal
    /// then rejected). NOT included here: the vCPU-state representability check
    /// (`unrepresentable_state`) — that is a property of the captured state, not
    /// the boundary.
    pub(crate) fn can_snapshot(&self) -> bool {
        !self.rng_completion_staged
    }

    /// Restore the V-time + entropy state captured by [`Vmm::save_vtime`]: rebuild
    /// the clock at `snap.vns`, re-apply the guest clock offset, and restore the
    /// entropy stream position.
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
    /// boundary, or if the entropy blob is rejected; [`VmmError::Vtime`] on a
    /// clock error. **Atomic:** every fallible step
    /// that can reject an untrusted `snap` (the clock-config rebuild and the
    /// entropy-blob validation) runs **before** any live state is mutated, so a bad
    /// snapshot leaves the timeline fully intact rather than half-restored. The one
    /// fallible step AFTER the commit — the armed-pvclock re-stamp (an
    /// epoch-advancing refresh, never canonical `seq = 0` on the live page) — is a
    /// mechanism step, not `snap` validation: it can only fail on a host-side
    /// stamping bug (fail-closed), never on untrusted input.
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
        // 2. Commit the validated state. All remaining assignments are infallible,
        // so malformed clock or entropy state cannot leave a partial restore.
        let vt = self.vtime.as_mut().ok_or_else(|| {
            VmmError::ContractViolation("restore_vtime called but V-time is not wired".to_string())
        })?;
        vt.clock = clock;
        vt.cfg = cfg;
        vt.entropy = entropy;
        vt.guest_clock_offset = snap.guest_clock_offset;
        // The restored VM's effective V-time is exactly `snap.vns`.
        // P1 (cross-model r12, corrected r13). Re-stamp an ARMED registration's
        // page to the just-restored anchor BEFORE returning, via the
        // epoch-advancing REFRESH protocol — NOT canonical `seq = 0`. Unlike a full
        // `restore_vm_state` — whose page bytes ride the RAM image and are already
        // canonical from the seal — a V-time-only restore rebases the timeline but
        // leaves the live RAM (and its page) untouched, so the page still holds the
        // PRE-restore stamp: a value from the old timeline that can sit AHEAD of the
        // new effective V-time (`vns_base` at anchor 0). If we returned now, the
        // guest's next entry would read that stale-ahead value and then, at the step
        // tail's refresh, watch it drop to the restored value — a backward vns jump.
        // `stamp` (Refresh) republishes the restored value AND advances the seqlock
        // epoch on the distinct value, so a reader straddling this restore (sampled
        // `seq`, took a V-time exit mid-read, resumes here) sees the changed epoch
        // and RETRIES — exactly the guarantee a LIVE page needs. Canonical form
        // (`seq = 0`) is for snapshot COPIES only: resetting a live epoch to a value
        // a paused reader may already hold is an ABA (the r4 seal ruling; the module
        // doc on `stamp_canonical`) — which is why the r12 canonical re-stamp here
        // was wrong. A PENDING registration is left alone — its first stamp still
        // belongs to the handshake intercept. This re-stamp is a MECHANISM step, not
        // snapshot validation: its only failure is a host-side stamping bug (RAM
        // slice moved / read-back mismatch) that fails closed exactly as on the
        // step-tail refresh; an untrusted `snap` was already fully rejected in step 1
        // before any mutation, so the all-or-nothing guarantee for bad input holds.
        if self
            .pvclock
            .as_ref()
            .is_some_and(|pv| pv.gpa.is_some() && pv.armed)
        {
            self.pvclock_stamp(StampKind::Refresh)?;
        }
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
    /// re-draw), and (b), when V-time is wired, at a non-`clock_boundary` point (the
    /// exact V-time the restored TSC resumes from is known only at a V-time intercept).
    /// These are the same guards [`Vmm::save_vtime`] enforces, and are the deliberate
    /// "staged completion is defined out" choice — a non-idempotent staged completion is
    /// excluded by snapshotting only at a clean, synchronized boundary, of which an
    /// interrupt-driven guest has many (every RDTSC the workload takes).
    ///
    /// **The pvclock page is sealed VERBATIM (task 110 §1.1, amended at r4).** A
    /// seal does not touch guest RAM at all: the clock page rides the memory
    /// image exactly as the guest sees it. Canonicalizing it here — the design
    /// doc's original ruling — would reset a *live* seqlock epoch to a value the
    /// page has held before, and since task 41 a seal is taken at any
    /// V-time-synchronized intercept (not only at an HLT quiescent point) a guest
    /// reader can be straddling one: it would re-read the restored epoch, match,
    /// and accept the values it loaded before the last refresh. Taking a snapshot
    /// would change the guest's future. It would also make the sealed image
    /// *differ* from live guest RAM, which the snapshot engine's derive path and
    /// `restore ⇒ same future` both rely on not happening.
    ///
    /// History-freedom is instead structural: [`vtime::pvclock::stamp`] is
    /// value-keyed, so the epoch advances only on distinct-value publications and
    /// is already a pure function of the deterministic execution. A restored run
    /// inherits the parent's epoch and continues in lockstep with it.
    ///
    /// # Errors
    /// [`VmmError::ContractViolation`] at an RNG mid-exit boundary, a non-synchronized
    /// point, or if the live vCPU carries the PAE-only `kvm_sregs2` flags/pdptrs or
    /// `debugregs.flags` (all zero for the 64-bit determinism guest — see
    /// [`crate::snapshot::unrepresentable_state`]); [`VmmError::Backend`] if reading the
    /// live vCPU state fails (a snapshot **fails closed** rather than sealing a zeroed or
    /// lossy vCPU).
    pub fn save_vm_state(&self) -> Result<<B::A as Vendor>::Snapshot, VmmError> {
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
        // Task 110 (r13 P1). A PENDING pvclock registration is UNSEALABLE. Between
        // the doorbell `OUT` (which records the GPA) and the handshake intercept
        // (the guest's post-doorbell RDTSC, which arms the page and lays the first,
        // canonical stamp), `armed` is false — but the v4 device record carries the
        // GPA and NOT the pending-vs-armed bit, so `pvclock_commit_restore` would
        // bring a restored child up ARMED. That child would then perform a normal
        // ordinary refresh where the source still owes the canonical handshake
        // stamp — different page bytes, different future. This
        // is a property of the captured *state* the representable subset cannot
        // hold, so it fails closed here alongside `check_sealable_vcpu` rather than
        // in the boundary predicate. In normal operation a pending registration is
        // never at a synchronized boundary (the `OUT` is a PIO; the first
        // synchronized point after it is the arming handshake); the one path that
        // can pair pending with a synchronized seal is `restore_vtime`.
        if self
            .pvclock
            .as_ref()
            .is_some_and(|pv| pv.gpa.is_some() && !pv.armed)
        {
            return Err(VmmError::ContractViolation(
                "save_vm_state with a PENDING pvclock registration: the page GPA is \
                 recorded but the registration handshake (the guest's post-doorbell \
                 RDTSC) has not completed, so `armed` is false — a state the v4 device \
                 record cannot represent (a restore would come up armed and skip the \
                 canonical handshake stamp the source still owes). Snapshot after the \
                 handshake intercept (step once more first)."
                    .to_string(),
            ));
        }
        // Task 110 (r4): NO pvclock re-stamp here. The page is sealed exactly as
        // the guest sees it — see this method's doc comment for why
        // canonicalizing a live page is an ABA on a straddling reader, and why
        // value-keyed stamping already makes the epoch reproducible. A seal
        // mutates nothing, so the `NotQuiescent` retry loops and sealability
        // probes stay side-effect-free for free rather than by careful ordering.
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
    pub fn restore_vm_state(&mut self, s: &<B::A as Vendor>::Snapshot) -> Result<(), VmmError> {
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
        // 1. Validate, committing nothing. The engine reads only the arch-neutral
        //    blocks of the snapshot — the V-time clock, the timer queue, and the
        //    entropy bytes — through the `SnapshotRecords` accessors; the vendor
        //    record set stays the vendor's own (`validate_restore` below).
        // 1a-bis. A non-empty timer queue cannot be applied: the engine has no
        //     `vtime::TimerQueue` (the only timer is the vendor fabric's, carried in
        //     the device blob), so a non-default `timers` section would be silently
        //     dropped. Fail closed (a well-formed vmm-core blob always seals it empty).
        if *s.timers() != vm_state::TimerQueueState::default() {
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
        let svt = s.vtime();
        let vtime_commit = match self.vtime.as_ref() {
            Some(vt) => {
                if svt.guest_hz != vt.cfg.guest_hz || svt.guest_base != vt.cfg.guest_base {
                    return Err(VmmError::ContractViolation(
                        "restore_vm_state: V-time clock mismatch (the snapshot's guest_hz/\
                         guest_base differ from this VM's wired clock)."
                            .to_string(),
                    ));
                }
                let mut cfg = vt.cfg;
                cfg.vns_base = svt.snapshot_vns;
                let clock = VClock::new(cfg)?;
                let mut entropy = vt.entropy.clone();
                entropy.restore_state(s.entropy_bytes()).map_err(|e| {
                    VmmError::ContractViolation(format!(
                        "entropy snapshot rejected on restore: {e:?}"
                    ))
                })?;
                Some((cfg, clock, entropy))
            }
            None => {
                // Unwired VM: the snapshot must carry the COMPLETE unwired V-time
                // sentinel that the save path stamps for a V-time-less VM — every
                // field at its unwired value AND no entropy/hypercall bytes.
                // Checking only guest_hz/snapshot_vns let a blob with a nonzero
                // guest_base (or entropy bytes) through, and this arm then
                // silently DISCARDS that live clock/entropy state — a fail-closed
                // snapshot-contract violation. The sentinel has every clock field
                // zero and no entropy bytes.
                let is_unwired_sentinel = svt.guest_hz == 0
                    && svt.guest_base == 0
                    && svt.snapshot_vns == 0
                    && s.entropy_bytes().is_empty();
                if !is_unwired_sentinel {
                    return Err(VmmError::ContractViolation(
                        "restore_vm_state: snapshot carries live V-time/entropy state but this VM \
                         has no V-time wired — restore into a VM composed like the snapshot source."
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
        // 4. Commit the validated state (all infallible from here).
        if let Some((cfg, clock, entropy)) = vtime_commit {
            let vt = self.vtime.as_mut().expect("vtime_commit implies wired");
            vt.cfg = cfg;
            vt.clock = clock;
            vt.entropy = entropy;
            vt.guest_clock_offset = clock_offset;
        }
        // The vendor half of the commit (all infallible): the prepared fabric /
        // platform / serial devices, and the restored guest-observable report stream
        // (so a branch resumes the guest's `observable_digest` / O2 signal instead of
        // losing every report emitted before the snapshot).
        <B::A as Vendor>::commit_restore(self, prep);
        // The control server closes the old host-only trace segment before a
        // restore. Recreate the placement oracle's active schedule from the
        // restored timer fabric so a due first post-restore clockevent has the
        // deadline record that produced it. This is evidence state only.
        let restored_clockevent = <B::A as Vendor>::clockevent_trace_schedule(self);
        if let Some(trace) = self.virtual_time_trace.as_mut() {
            trace
                .restore_clockevent_schedule(restored_clockevent)
                .map_err(|message| VmmError::ContractViolation(message.to_string()))?;
        }
        // A restored VM is runnable again from the snapshot point: clear the latched
        // terminal + cached vCPU so `step`/`run` resume and `state_blob` re-reads the
        // restored backend state.
        self.terminal = None;
        self.saved_state = None;
        self.rng_completion_staged = false;
        // The restored backend is fresh (the next run re-executes from the restored
        // RIP) — no completion is pending.
        self.completion_staged = false;
        // A deferred SDK snapshot re-entry belongs to the displaced timeline.
        // The restored SDK channel's own `pending_snapshot` bit is applied by
        // the control server after this call; no old doorbell completion may
        // suppress or synthesize that restored point.
        self.sdk_snapshot_reentry_required = false;
        // (Task 110: the pvclock channel needs no reset here — the blob's own
        // v4 record was validated in the vendor's `validate_restore` and
        // committed in `commit_restore` above, replacing any stale-timeline
        // registration with the sealed one — including clearing it when the
        // sealed VM had none. The same-state ⇒ same-future contract holds for
        // direct `restore_snapshot` callers with no side channel.)
        Ok(())
    }

    /// Restore a full snapshot — guest memory **and** the non-memory `vm_state` — in
    /// one call. The materialized image (from [`crate::snapshot::SnapshotEngine::materialize`])
    /// goes into guest RAM, then [`Vmm::restore_vm_state`] resumes the vCPU/V-time/
    /// devices. *Same state ⇒ same future* (gate 1).
    pub fn restore_snapshot(
        &mut self,
        memory: &[u8],
        vm_state: &<B::A as Vendor>::Snapshot,
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
        // Clear the pvclock registration-handshake flag before entry. Only a
        // successfully serviced architecture time read sets it again.
        self.tsc_read_intercept = false;
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
        let exit = self.backend.run()?;
        if <B::A as Vendor>::is_doorbell_exit(&exit) {
            self.doorbell_exits = self.doorbell_exits.saturating_add(1);
        }
        // A successful entry commits the prior exit's userspace-I/O completion.
        // If `setup_complete` armed the deferred snapshot latch on that prior
        // exit, the point may now surface after this exit is serviced.
        self.sdk_snapshot_reentry_required = false;
        let trace_started = if let Some(trace) = self.virtual_time_trace.as_mut() {
            if let Some((class, payload)) = <B::A as Vendor>::normalize_virtual_time_exit(&exit) {
                let reason = exit.reason();
                let backend_debug = format!("{reason:?}");
                trace
                    .begin(reason, backend_debug, class, payload)
                    .map_err(|message| VmmError::ContractViolation(message.to_string()))?;
                true
            } else {
                let reason = exit.reason();
                let backend_debug = format!("{reason:?}");
                trace
                    .record_raw_only(reason, backend_debug)
                    .map_err(|message| VmmError::ContractViolation(message.to_string()))?;
                false
            }
        } else {
            false
        };
        self.rng_completion_staged = false;
        self.completion_staged = exit.stages_completion();
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
            Exit::Arch(e) => <B::A as Vendor>::dispatch_arch(self, e),
        }?;
        // Task 110: refresh the pvclock page at the tail of EVERY serviced
        // exit (the §2 point-1 natural-exit refresh) with the deterministic
        // anchor's clock — value-keyed, so only the deterministic
        // clock-advance boundaries (V-time intercepts, `Deadline` landings,
        // idle warps) actually move the page bytes; see `pvclock_refresh`.
        // Stamping BEFORE the next entry is what closes the §7
        // kill-condition-1 ordering: a timer whose landing advanced the
        // anchor is injected at the NEXT entry, so the ISR reads a page
        // already stamped at (or beyond) the interrupt's own V-time. A no-op
        // unless a page is registered.
        self.pvclock_refresh()?;
        <B::A as Vendor>::post_exit(self)?;
        if trace_started {
            let event_index = self
                .virtual_time_trace
                .as_ref()
                .expect("trace was started")
                .current_event_index()
                .map_err(|message| VmmError::ContractViolation(message.to_string()))?;
            let checkpoint = (event_index + 1).is_multiple_of(256);
            let state_hash =
                synchronous_checkpoint_due(checkpoint, self.deferred_virtual_time_checkpoints)
                    .then(|| self.state_hash());
            let vns_after = self
                .vtime
                .as_ref()
                .ok_or_else(|| {
                    VmmError::ContractViolation(
                        "virtual_time trace completed without V-time wiring".to_string(),
                    )
                })?
                .virtual_time_vns();
            self.virtual_time_trace
                .as_mut()
                .expect("trace was started")
                .finish(vns_after, state_hash)
                .map_err(|message| VmmError::ContractViolation(message.to_string()))?;
        }
        Ok(step)
    }

    /// `step()` to a `Terminal`. Returns the serial capture, terminal reason, and
    /// exit counts.
    pub fn run(&mut self) -> Result<RunResult, VmmError> {
        // The virtual-time clock is prepared at the first guest entry inside `step`
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
        out.extend_from_slice(&self.state_blob_suffix());
        out
    }

    /// The canonical state-blob bytes after the RAM chunk.
    ///
    /// Keeping this suffix separate lets [`Vmm::state_hash`] stream the large RAM
    /// slice directly into SHA-256 instead of first allocating a second full-image
    /// `Vec`. It is also the seal-time hash recipe used by portable snapshot export;
    /// the suffix contains only fixed-size machine/channel state.
    pub(crate) fn state_blob_suffix(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let vcpu = self.current_vcpu();
        // The dedicated hypercall-transport ABI pages are guest-visible memory
        // (arm64: a separate low-GPA memslot; x86 keeps them inside `MEM`). Fold
        // them into the hash so two states differing only in the request/response
        // pages hash **differently** (the determinism-completeness contract).
        // Present ONLY when mapped, so an x86 / doorbell-less blob is byte-for-
        // byte unchanged (the `VTIM`/`SDK`/device-chunk discipline).
        if let Some(db) = &self.doorbell_pages {
            put_chunk(&mut out, b"DOOR", db.as_bytes());
        }
        put_chunk(
            &mut out,
            b"VCPU",
            &<B::A as Vendor>::encode_vcpu_chunk(&vcpu),
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
        <B::A as Vendor>::hash_device_chunks(&vcpu, &self.devices, &mut out);
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
        // composition's blob is byte-for-byte unchanged. The registration
        // governs future guest-visible time, so — like the SDK channel's
        // fault-policy fold — two states identical in RAM but differing here
        // have different futures and must hash differently. The refresh log
        // stays out (diagnostic, like the landing traces).
        if let Some(pv) = &self.pvclock {
            // Preserve the frozen N1 PVCK preimage. N2 removed the configurable
            // retired branch-clock refresh configuration. Production ARM/x86
            // compositions used the value 1; retaining those historical bytes keeps attested state
            // hashes stable without retaining the mechanism or configuration.
            let mut bytes = 1_u64.to_le_bytes().to_vec();
            match pv.gpa {
                Some(gpa) => {
                    bytes.push(1);
                    bytes.extend_from_slice(&gpa.to_le_bytes());
                }
                None => bytes.push(0),
            }
            // The registration CAPABILITY (V-time wired + a deterministic work
            // clock), not just the offer (cross-model r6 P1). Two offered VMs
            // with V-time wired but different deterministic backends have
            // different futures — the next registration succeeds on one and
            // answers `UnknownService` on the other — so they must hash
            // differently, exactly as `registrable` makes them restore-
            // incompatible. Without this the fold would carry the offer but not
            // the capability the offer's future turns on.
            bytes.push(u8::from(self.pvclock_available()));
            // The HANDSHAKE state (cross-model r11 P2): a *pending* registration
            // (`armed == false`, between the doorbell `OUT` and the handshake
            // intercept) and an *armed* one have DIFFERENT futures — the pending
            // one's next synchronized step lays the canonical page stamp,
            // the armed one refreshes normally — so pending-vs-armed belongs in
            // state identity. (This bit is only ever observed mid-run: a snapshot
            // is taken only at a synchronized point, and a pending registration
            // exists only at non-synchronized points, so a sealed state is always
            // armed — restore derives it. It is folded here for the mid-run hash.)
            bytes.push(u8::from(pv.armed));
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
        // Stream the potentially hundreds-of-megabytes MEM chunk directly into
        // the digest. The remaining canonical suffix is small and already owned
        // by a temporary Vec, so this preserves the exact pre-change digest while
        // removing the full-image allocation from every hash request.
        hasher.update(b"MEM\0");
        hasher.update((self.ram.as_bytes().len() as u64).to_le_bytes());
        hasher.update(self.ram.as_bytes());
        hasher.update(self.state_blob_suffix());
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
        // The dedicated hypercall-transport ABI pages (arm64) fold into
        // `state_hash` via the `DOOR` chunk but are their own memslot — invisible
        // to the RAM regions above — so a divergence living only in the
        // request/response pages would hash differently while every RAM component
        // matched (the bisector-blind spot codex flagged, the exact analogue of
        // the GICV case). A labeled component **when mapped** (present only then,
        // mirroring the `DOOR` chunk; x86 / doorbell-less runs emit nothing, so
        // their breakdown is byte-unchanged). Additive — never a rename of an
        // O1-pinned label.
        if let Some(db) = &self.doorbell_pages {
            out.push(("doorbell", dig(db.as_bytes())));
        }

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
        // The vendor's per-device digests — the device hash chunks
        // (`hash_device_chunks`) fold into `state_hash` but are otherwise
        // invisible to this breakdown, so a divergence living only in a device
        // (arm64: the `GICV` chunk's register files / pending-active / timer)
        // would hash differently while every component above matched. Additive:
        // the vendor appends new labels only (never renames a pinned one).
        <B::A as Vendor>::device_components(&vcpu, &self.devices, &mut out);
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
            // deterministic effective field `snapshot_vns(assigned_clock)`. Mirroring
            // the live read as a hashed component would falsely indict the
            // post-intercept exit-boundary variability the hash deliberately excludes.)
            let mut cfg = 1_u64.to_le_bytes().to_vec();
            for x in [vt.cfg.guest_hz, vt.cfg.guest_base, vt.guest_clock_offset] {
                cfg.extend_from_slice(&x.to_le_bytes());
            }
            out.push(("vtim:cfg", dig(&cfg)));
            // The effective V-time `encode_vtime` hashes: `snapshot_vns` of the
            // **deterministic** `assigned_clock` (NOT a live counter read).
            out.push(("vtim:eff-vns", dig(&vt.clock.vns().to_le_bytes())));
            out.push(("vtim:entropy", dig(&vt.entropy.save_state())));
        }
        out
    }

    /// The ordered conformance **report stream**: every value the guest wrote to
    /// [`REPORT_PORT`] (`OUT`), in execution order. Empty for stock / M1/M2 runs
    /// that never touch the port. Feeds [`Vmm::observable_digest`].
    pub fn report_stream(&self) -> &[u32] {
        &self.report_stream
    }

    /// The idle-resume landings (task 52): the **V-time** (ns) the clock was warped to
    /// each time the guest went idle (`HLT` with `RFLAGS.IF == 1` and an armed timer) and
    /// [`Self::resume_idle`] jumped to the timer deadline. The dual of
    /// [`Self::virtual_time_trace`] (jumped-to vs executed-to the next event); empty when
    /// the run never idled. Exit-boundary variability-free (derived from the last-intercept anchor + the timer
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

    /// The number of exact hypercall-doorbell rings since this VM was created.
    /// This host-only diagnostic counter is not part of state, hashes, or
    /// snapshots; it is narrower than [`Vmm::exit_counts`]'s I/O/MMIO totals.
    pub fn doorbell_exits(&self) -> u64 {
        self.doorbell_exits
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
            coverage_thresholds: BTreeMap::new(),
            coverage: Vec::new(),
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

    /// Offer the paravirtual clock page to the guest
    /// (`docs/PARAVIRT-CLOCK.md`). Offering alone changes nothing: the page
    /// engages only when the guest publishes a page GPA over the doorbell
    /// ([`hypercall_proto::ServiceId::Pvclock`], op 1), and registration is
    /// accepted only when virtual time is wired.
    pub fn enable_pvclock(&mut self) -> &mut Self {
        self.pvclock = Some(PvclockChannel {
            gpa: None,
            armed: false,
            refreshes: Vec::new(),
        });
        self
    }

    /// `true` once [`enable_pvclock`](Self::enable_pvclock) offered the clock
    /// page (regardless of whether the guest has registered one).
    pub fn pvclock_offered(&self) -> bool {
        self.pvclock.is_some()
    }

    /// Is the pvclock service **available** on this composition — offered, V-time
    /// wired, and a *deterministic* virtual-time clock behind it? This is the exact
    /// precondition for a registration to succeed, and it is used in three
    /// places, which is the point of naming it:
    ///
    /// - the doorbell gate, which must answer `UnknownService` for an
    ///   unavailable service **before** it classifies the payload or the opcode
    ///   (cross-model r5 P2);
    /// - [`pvclock_register`](Self::pvclock_register) itself;
    /// - and the restore validation, which requires a snapshot's availability to
    ///   MATCH this VM's (r5 P1). A source that could register, restored onto a
    ///   VM that could not, would answer `UnknownService` to the very
    ///   registration the source accepted — same state, different future — and
    ///   the reverse would let a child register where its parent never could.
    ///   Neither is caught by the GPA check, because a snapshot taken *before*
    ///   registration carries no GPA to check.
    fn pvclock_available(&self) -> bool {
        self.pvclock.is_some() && self.vtime.is_some()
    }

    /// Is a doorbell `service` id **offered** by this composition? The generic
    /// dispatcher contract (INTEGRATION.md §1) is that an unoffered service
    /// answers `UnknownService` for **any** request — before opcode or payload
    /// classification, so a composition that keeps the doorbell alive for one
    /// channel never advertises another by grading its requests `UnknownOpcode`
    /// (cross-model r7 P2). Each arm below already checks availability for its
    /// `op == 1`; this is the single source of truth the fall-through shares so
    /// a bad opcode is gated the same way.
    fn doorbell_service_offered(&self, service: u16) -> bool {
        match service {
            s if s == ServiceId::Event as u16 => self.sdk.is_some(),
            s if s == ServiceId::Sdk as u16 => self.sdk.is_some(),
            s if s == ServiceId::Net as u16 => self.net.is_some(),
            s if s == ServiceId::Entropy as u16 => self.sdk.is_some() || self.net.is_some(),
            s if s == ServiceId::Payload as u16 => self
                .sdk
                .as_ref()
                .is_some_and(|sdk| sdk.env.payload_configured()),
            s if s == ServiceId::Pvclock as u16 => self.pvclock_available(),
            _ => false,
        }
    }

    /// The registered pvclock page GPA, or `None` when the guest has not
    /// published one (or the page is not offered).
    pub fn pvclock_registration(&self) -> Option<u64> {
        self.pvclock.as_ref().and_then(|pv| pv.gpa)
    }

    /// Capture the pvclock channel's **complete replay-relevant configuration**
    /// for a snapshot: `Some` iff the page is offered, carrying the registration
    /// (if any). The page *bytes* ride the RAM image; the offer shapes a future registration,
    /// so the control server carries it across snapshot/branch like the SDK
    /// channel's, restoring (and cross-validating) via
    /// [`pvclock_restore`](Self::pvclock_restore).
    pub fn pvclock_snapshot(&self) -> Option<PvclockSnapshot> {
        let registrable = self.pvclock_available();
        self.pvclock.as_ref().map(|pv| PvclockSnapshot {
            gpa: pv.gpa,
            registrable,
        })
    }

    /// Validate a snapshot's pvclock channel record against **this** VM's
    /// composition, symmetrically and without mutating. The snapshot and this
    /// VM must agree on whether the page is offered; a mismatch fails loud
    /// ([`VmmError::ContractViolation`], the LAPIC wiring-mismatch posture),
    /// never a silently different clock. A carried registration additionally
    /// re-validates the GPA against this VM's RAM and requires the
    /// deterministic-clock backend the original registration required.
    /// Runs in the vendor's `validate_restore` phase — before any commit —
    /// so a rejected blob leaves the VM fully intact.
    ///
    /// # Errors
    /// [`VmmError::ContractViolation`] on any offer mismatch, a GPA that no
    /// longer validates, or a registration restored onto a backend with no
    /// deterministic virtual-time clock.
    pub(crate) fn pvclock_validate_restore(
        &self,
        rec: Option<&(Option<u64>, bool)>,
    ) -> Result<(), VmmError> {
        match (rec, self.pvclock.as_ref()) {
            (None, None) => Ok(()),
            (None, Some(_)) => Err(VmmError::ContractViolation(
                "restore_vm_state: this VM offers the pvclock page but the snapshot's VM did \
                 not — a guest registering here would fork the timeline off the sealed one; \
                 restore into a VM composed like the snapshot source."
                    .to_string(),
            )),
            (Some((gpa, _)), None) => Err(VmmError::ContractViolation(format!(
                "restore_vm_state: snapshot carries a pvclock channel (registration \
                 {gpa:#x?}) but this VM was composed without enable_pvclock — \
                 restore into a VM composed like the snapshot source."
            ))),
            (Some((gpa, registrable)), Some(_pv)) => {
                // REGISTRATION CAPABILITY, independent of whether a GPA is
                // present (cross-model r5 P1). A snapshot taken BEFORE the guest
                // registered carries no GPA — so the GPA check below never runs —
                // yet it still promises a future in which the guest registers.
                // Restored onto a backend with no deterministic virtual-time clock, that
                // registration would answer `UnknownService` where the source
                // accepted it: same state, different future. Equality (not merely
                // "this VM can too") because the converse — a child that CAN
                // register where its parent never could — forks the timeline just
                // as hard.
                if *registrable != self.pvclock_available() {
                    return Err(VmmError::ContractViolation(format!(
                        "restore_vm_state: pvclock registration capability mismatch (the \
                         snapshot's VM {} register a clock page; this VM {}) — the restored \
                         guest's next registration would take a different branch than the \
                         sealed timeline's. Restore into a VM composed like the snapshot \
                         source (V-time wired, deterministic virtual-time clock).",
                        if *registrable { "could" } else { "could NOT" },
                        if self.pvclock_available() {
                            "can"
                        } else {
                            "can NOT"
                        }
                    )));
                }
                if let Some(gpa) = gpa {
                    self.pvclock_validate_gpa(*gpa).map_err(|reason| {
                        VmmError::ContractViolation(format!(
                            "restore_vm_state: snapshot pvclock page GPA {gpa:#x} does not \
                             validate on this VM ({reason}) — restore into a VM composed like \
                             the snapshot source."
                        ))
                    })?;
                }
                Ok(())
            }
        }
    }

    /// Commit a snapshot's (already-validated) pvclock channel record: the
    /// blob's registration state replaces this VM's — a carried registration
    /// resumes stamping into the restored RAM's page (the same-state ⇒
    /// same-future half the direct `restore_snapshot` path owes its callers),
    /// and an unregistered record clears any stale-timeline registration
    /// this VM held. Infallible, per
    /// the restore commit phase's contract.
    pub(crate) fn pvclock_commit_restore(&mut self, rec: Option<&(Option<u64>, bool)>) {
        if let Some(pv) = self.pvclock.as_mut() {
            // A restored VM's anchor is exactly 0 (the virtual-time clock restarts and
            // `restore_vm_state` anchors there — a synchronized boundary by
            // construction), so a restored registration is **already armed**: it
            // needs no live handshake. Only a *registered* record is armed; an
            // unregistered one clears any stale-timeline registration.
            // Arming every carried GPA is faithful because `save_vm_state` refuses
            // to seal a PENDING (un-armed) registration (r13 P1): a sealed
            // `Some(gpa)` was therefore armed on the source, so the source owed no
            // handshake stamp and the restored child owes none either.
            let gpa = rec.and_then(|(g, _)| *g);
            pv.armed = gpa.is_some();
            pv.gpa = gpa;
            pv.refreshes.clear();
        }
    }

    /// Reset the diagnostic refresh log at a measurement boundary. Never
    /// touches the page or the registration.
    pub fn pvclock_clear_refreshes(&mut self) {
        if let Some(pv) = self.pvclock.as_mut() {
            pv.refreshes.clear();
        }
    }

    /// The diagnostic pvclock refresh log: `(vns, guest_clock)`
    /// per value-publishing stamp, **read back from the page bytes** (never
    /// the computed values — see [`PvclockChannel`]). Empty when nothing is
    /// registered. The G2 gate's evidence; capped at
    /// [`PREEMPTION_TRACE_CAP`], not hashed.
    pub fn pvclock_refreshes(&self) -> &[(u64, u64)] {
        self.pvclock
            .as_ref()
            .map(|pv| pv.refreshes.as_slice())
            .unwrap_or(&[])
    }

    /// The current pvclock page bytes (the registered 4 KiB window of guest
    /// RAM), or `None` when nothing is registered. For gates and tests — reads
    /// the live RAM, so it sees exactly what the guest would.
    pub fn pvclock_page(&self) -> Option<&[u8]> {
        // Resolve the absolute registration GPA to a main-RAM offset (arm64 RAM
        // is high; x86 base 0 → identical offset). Review r14.
        let off = self.ram_offset_of(self.pvclock_registration()?)?;
        self.ram
            .as_bytes()
            .get(off..off + vtime::pvclock::PVCLOCK_PAGE_LEN)
    }

    /// G2's function-equality check, callable at any point (the box gate calls
    /// it at chosen boundaries; the deliberate-fault test proves it can fail):
    /// the page's current stable frame must publish **exactly** the values the
    /// RDTSC-trap oracle would return at the current deterministic anchor —
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
        let want_vns = vt.clock.vns();
        let want_gc = vt.guest_clock();
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
                "pvclock page diverges from the RDTSC-trap oracle: page \
                 (vns {}, guest_clock {}, hz {}) vs oracle (vns {want_vns}, guest_clock \
                 {want_gc}, hz {want_hz})",
                f.vns, f.guest_clock, f.guest_clock_hz
            )));
        }
        // The ABI-v1 flags word: MATERIALIZED | EXIT_COUNT_DERIVED, remaining bits
        // reserved-zero (the PR #108 r9 coordination ruling — bit 1 is what a
        // static placeholder page deliberately lacks, so a page nothing is
        // actually deriving can never pass this gate).
        if f.flags != vtime::pvclock::PVCLOCK_FLAGS_V1 {
            return Err(VmmError::ContractViolation(format!(
                "pvclock page flags {:#x} != the ABI-v1 MATERIALIZED|EXIT_COUNT_DERIVED word {:#x} — a \
                 placeholder or corrupted page, never a real exit-count-derived stamp",
                f.flags,
                vtime::pvclock::PVCLOCK_FLAGS_V1
            )));
        }
        Ok(())
    }

    /// Validate a pvclock page GPA against this VM: page-aligned, wholly
    /// inside guest RAM, clear of the doorbell frame pages (a stamp there would
    /// clobber an in-flight hypercall exchange), and clear of the vendor's
    /// device-MMIO holes. Returns the failing reason for the caller's
    /// status/error mapping.
    ///
    /// **The hole check is not redundant with the RAM bound** (cross-model r5
    /// P2): the RAM *image* spans the vendor's MMIO holes, but the backend
    /// deliberately leaves them out of its memslots (x86: the xAPIC page at
    /// `0xFEE00000`). A page there passes "inside guest RAM" while the guest's
    /// own loads go to the *device model*, not to the bytes the host stamps — so
    /// registration would report success and then publish time into backing the
    /// guest can never read. Fail closed instead.
    fn pvclock_validate_gpa(&self, gpa: u64) -> Result<(), &'static str> {
        let page_len = vtime::pvclock::PVCLOCK_PAGE_LEN as u64;
        if !gpa.is_multiple_of(page_len) {
            return Err("not page-aligned");
        }
        // Resolve the ABSOLUTE GPA to a main-RAM offset (review r14 — the last of
        // the r11 GPA family). arm64 RAM is high, and the pvclock region is
        // reserved at `RAM_BASE + off` (the `hm-rk5` seam), so a valid absolute
        // GPA must be *routed*, not rejected as beyond RAM; a GPA below the base
        // is not backed. x86 (base 0) resolves to the identical offset.
        let off = self.ram_offset_of(gpa).ok_or("below the guest RAM base")? as u64;
        let end = off.checked_add(page_len).ok_or("address overflow")?;
        if end > self.ram.as_bytes().len() as u64 {
            return Err("past the end of guest RAM");
        }
        // The doorbell frame pages are contiguous ([REQ_GPA, RESP_GPA + page]);
        // a 4 KiB-aligned page overlaps them iff it IS one of them.
        if gpa == REQ_GPA as u64 || gpa == RESP_GPA as u64 {
            return Err("overlaps a doorbell frame page");
        }
        // Device MMIO the backend never maps as RAM (x86: the xAPIC page).
        for &(hole, hole_len) in <B::A as Vendor>::mmio_holes() {
            let hole_end = hole.saturating_add(hole_len);
            if gpa < hole_end && hole < end {
                return Err("overlaps a device-MMIO hole (not backed as guest RAM)");
            }
        }
        Ok(())
    }

    /// Register the guest-published pvclock page: validate the GPA and **record
    /// it as pending**. Returns the doorbell `Status` + the ABI version to
    /// answer. Gated on the determinism-complete path — a stock/M1/M2
    /// composition answers `UnknownService` ("not offered"), so a probing
    /// guest cleanly keeps its trap-backstopped time paths.
    ///
    /// **One-shot** (the PR #110 r2 GPA ruling, flagged for integrator veto): the first
    /// accepted registration pins the page for the machine's life;
    /// re-registration — same GPA or not — is a guest fault, rejected with
    /// `BadRequest` and touching nothing. The stamping target never moves.
    ///
    /// **The doorbell `OUT` only records a PENDING registration (the r8
    /// handshake ruling).** It does **not** stamp the page — a doorbell `OUT`
    /// is a plain PIO exit, not a V-time
    /// intercept, so the counter read there is host-noisy on the real backend
    /// (task-27 O1) and the pre-`OUT` anchor may be stale. The first stamp happens
    /// only at the **handshake intercept**: the guest's required
    /// post-doorbell V-time intercept (the reference kernel's RDTSC, now
    /// protocol), whose anchor is deterministic and fresh. See
    /// [`pvclock_refresh`](Self::pvclock_refresh) for the handshake and
    /// [`PvclockChannel::armed`]. A guest that never performs the handshake is
    /// out of contract: it gets a stale-but-deterministic page and no refresh.
    pub(crate) fn pvclock_register(&mut self, gpa: u64) -> (Status, Option<u32>) {
        if !self.pvclock_available() {
            return (Status::UnknownService, None);
        }
        if self.pvclock_registration().is_some() {
            return (Status::BadRequest, None);
        }
        if self.pvclock_validate_gpa(gpa).is_err() {
            return (Status::OutOfRange, None);
        }
        // Record the GPA as PENDING. No stamp — publication waits for the
        // handshake intercept (r8). `armed` stays false until then.
        let pv = self.pvclock.as_mut().expect("checked above");
        pv.gpa = Some(gpa);
        (Status::Ok, Some(vtime::pvclock::PVCLOCK_ABI_VERSION))
    }

    /// Re-stamp the registered pvclock page from the current clock — the §2
    /// refresh. Called by [`step`](Self::step) at every deterministic
    /// clock-advance boundary (V-time intercepts, deadline landings, idle
    /// warps — wherever `clock_boundary` holds at the end of a step); in
    /// canonical form only at the registration **handshake** (the first stamp)
    /// and the armed re-stamp of a V-time-only restore. **[`save_vm_state`] does
    /// NOT call this** — a seal captures the page **verbatim** (§1.1, the r4
    /// verbatim-seal ruling; canonicalizing a live page is the ABA hazard that
    /// ruling removed), so the seal path is side-effect-free. A no-op without a
    /// registration.
    ///
    /// The stamped values derive from the **deterministic anchor**
    /// (`assigned_clock`), never a live counter read — the page is hashed
    /// guest RAM, so a host-noisy stamp would be a determinism bug, and the
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
        let vns = vt.clock.vns();
        let gc = vt.guest_clock();
        let hz = vt.cfg.guest_hz;
        // Resolve the absolute GPA to a main-RAM offset (arm64 RAM is high; x86
        // base 0 → identical offset). Computed before the &mut borrow. Review r14.
        let off = self.ram_offset_of(gpa);
        let ram = self.ram.as_mut_bytes();
        let Some(page) = off.and_then(|o| ram.get_mut(o..o + vtime::pvclock::PVCLOCK_PAGE_LEN))
        else {
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
                "pvclock stamp read-back mismatch: wrote (vns {vns}, \
                 guest_clock {gc}, hz {hz}) but the page decodes to {readback:?}"
            )));
        }
        // A host-side RAM write the backend's dirty log cannot see (task 95
        // M2.1 safety rule).
        self.mark_host_dirty(gpa, vtime::pvclock::PVCLOCK_PAGE_LEN as u64);
        // Log value publishes (not canonical seq-resets, which republish the
        // same values) — the G2 gate's per-refresh evidence.
        if kind == StampKind::Refresh {
            let pv = self.pvclock.as_mut().expect("checked above");
            if pv.refreshes.len() < EVENT_TRACE_CAP {
                pv.refreshes.push((vns, gc));
            }
        }
        Ok(())
    }

    /// The [`step`](Self::step)-tail refresh — the §2 point-1 "every natural
    /// exit" refresh, plus the **registration handshake** (r8). Re-stamp the page
    /// at the tail of **every** serviced exit, publishing the clock at the
    /// **deterministic anchor** (`assigned_clock`), the exit's deterministic work
    /// count. Between two clock advances the anchor cannot move, so the stamp at a
    /// non-intercept exit (PIO/MMIO/doorbell/serial) republishes identical values
    /// and the value-keyed [`pvclock_stamp`](Self::pvclock_stamp) leaves the page
    /// untouched — the refresh *runs* at all four §2 points, and the published
    /// value stream advances exactly at the deterministic clock-advance
    /// boundaries.
    ///
    /// **The handshake (r8 ruling, sharpened r17).** A registration recorded at
    /// the doorbell `OUT` is *pending* ([`PvclockChannel::armed`] `== false`) — no
    /// stamp. It becomes active at the **first RDTSC/RDTSCP counter read**
    /// after the `OUT` — the specific exit the §3.1 wire contract promises the guest
    /// performs — where `assigned_clock` is a fresh, deterministic anchor, so the
    /// first stamp is canonical from it, never from a stale or PIO boundary.
    /// Other synchronized boundaries
    /// do NOT complete the handshake: a TSC MSR read/write, an RDRAND/RDSEED draw, a
    /// deadline landing, or an idle-warp restore is `clock_boundary` too, but
    /// arming off one would publish the page on an exit the contract does not
    /// promise. [`tsc_read_intercept`](Self::tsc_read_intercept) — cleared before
    /// every entry, set `true` **only** by the RDTSC/RDTSCP completion — is exactly
    /// the "did this step end on the promised counter read" signal the handshake
    /// needs. A pending registration at any other exit stamps nothing (the page
    /// keeps its pre-registration bytes — deterministic, and out of contract for a
    /// guest that never does the counter read).
    fn pvclock_refresh(&mut self) -> Result<(), VmmError> {
        let Some(pv) = self.pvclock.as_ref() else {
            return Ok(());
        };
        if pv.gpa.is_none() {
            return Ok(()); // nothing registered
        }
        if !pv.armed {
            // Pending registration: arm ONLY at the RDTSC/RDTSCP handshake
            // intercept (r17). The wire contract (§3.1, r8) promises the guest
            // publishes its GPA over the doorbell and then does a COUNTER READ, so
            // only that read completes the handshake. Every other synchronized
            // boundary — a TSC MSR, an RDRAND/RDSEED draw, a deadline landing, an
            // idle-warp restore — is `clock_boundary` too, but must NOT stamp or
            // publish the pending page (an RDRAND draw or timer boundary would
            // publish the clock where the contract says only the counter read may).
            // The handshake: promote to armed and lay down the first (canonical)
            // stamp from this fresh, deterministic anchor.
            self.pvclock.as_mut().expect("checked above").armed = true;
            return self.pvclock_stamp(StampKind::Canonical);
        }
        self.pvclock_stamp(StampKind::Refresh)
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
            payloads: s.env.remaining_payloads(),
            coverage_thresholds: s.coverage_thresholds.clone(),
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
            s.env.restore_payloads(snap.payloads.clone());
            s.coverage_thresholds = snap.coverage_thresholds.clone();
        }
    }

    /// Restore only the **event prefix** of a captured [`SdkSnapshot`] (the branch
    /// path): a branch reseeds, so the seeded streams start fresh from the new
    /// seed (`enable_sdk`), but the shared prefix events — the declared catalog —
    /// carry over so the fork's never-fired report is complete.
    pub fn sdk_restore_events(&mut self, snap: &SdkSnapshot) {
        if let Some(s) = self.sdk.as_mut() {
            s.events = snap.events.clone();
            s.coverage_thresholds = snap.coverage_thresholds.clone();
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

    /// Clone the canonical live ordered-input state: only the unconsumed
    /// payload suffix. `None` means no payload service is offered; an exhausted
    /// offered tape is `Some([])`.
    pub(crate) fn sdk_remaining_payloads(&self) -> Option<Vec<Vec<u8>>> {
        self.sdk
            .as_ref()
            .and_then(|sdk| sdk.env.remaining_payloads())
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

    /// Instrumented coverage scheduling decisions as
    /// `(moment, thread, observed, ready, selected)`, in arrival order.
    pub fn sdk_coverage(&self) -> &[(u64, u32, u64, u32, u32)] {
        self.sdk
            .as_ref()
            .map(|s| s.coverage.as_slice())
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
        // Copy the request out of the transport request page (an owned `Vec`, so
        // the borrow ends before we compute the response and write the response
        // page). Fail closed if the page is not backed — on arm64 a boot that
        // never mapped the ABI pages faults here rather than reading the wrong
        // RAM offset (review r10).
        let Some(req) = self
            .guest_slice(REQ_GPA as u64, req_len)
            .map(<[u8]>::to_vec)
        else {
            return Err(VmmError::ContractViolation(format!(
                "doorbell request page {REQ_GPA:#x}+{req_len} is not backed — x86 keeps the \
                 transport ABI pages in the GPA-0 RAM; an arm64 boot must map them (a dedicated \
                 low-GPA memslot; RAM is high)"
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

    /// The offset into the main RAM memslot for absolute guest `gpa` — i.e.
    /// `gpa - ram_base_gpa` — or `None` if `gpa` is **below** the base (e.g. the
    /// arm64 transport ABI pages, which sit below `RAM_BASE`). The **upper** bound
    /// is the caller's ([`Self::guest_slice`]'s `get(off..off+len)`), so a
    /// zero-length access at the exact RAM end (`gpa == ram_len`, `len == 0`)
    /// stays a valid empty read. x86's RAM base is `0`, so an absolute low GPA
    /// maps to the identical offset (unchanged).
    pub(crate) fn ram_offset_of(&self, gpa: u64) -> Option<usize> {
        usize::try_from(gpa.checked_sub(self.ram_base_gpa)?).ok()
    }

    /// Resolve an absolute guest `[gpa, gpa+len)` to a read-only host slice across
    /// **every** mapped RAM-class region — the main RAM (based at `ram_base_gpa`)
    /// and the dedicated low-GPA doorbell memslot ([`Self::doorbell_pages`], at
    /// `[DOORBELL_MAP_GPA, DOORBELL_MAP_GPA + 4·HC_PAGE)`). `None` if the range is not backed by a
    /// single region. Every absolute-GPA path — the doorbell, the control-plane
    /// `read`, `corrupt_memory` — routes through this, so a GPA resolves
    /// identically on every arch (x86: RAM at base 0, so a low GPA *is* its own
    /// offset — the pre-existing behavior, unchanged).
    pub(crate) fn guest_slice(&self, gpa: u64, len: usize) -> Option<&[u8]> {
        if let Some(off) = self.ram_offset_of(gpa) {
            return self.ram.as_bytes().get(off..off.checked_add(len)?);
        }
        let db = self.doorbell_pages.as_ref()?;
        let off = usize::try_from(gpa.checked_sub(DOORBELL_MAP_GPA as u64)?).ok()?;
        db.as_bytes().get(off..off.checked_add(len)?)
    }

    /// The `&mut` twin of [`Self::guest_slice`] (the host-write paths:
    /// `write_doorbell_response`, `corrupt_memory`).
    pub(crate) fn guest_slice_mut(&mut self, gpa: u64, len: usize) -> Option<&mut [u8]> {
        if let Some(off) = self.ram_offset_of(gpa) {
            let end = off.checked_add(len)?;
            return self.ram.as_mut_bytes().get_mut(off..end);
        }
        let db = self.doorbell_pages.as_mut()?;
        let off = usize::try_from(gpa.checked_sub(DOORBELL_MAP_GPA as u64)?).ok()?;
        let end = off.checked_add(len)?;
        db.as_mut_bytes().get_mut(off..end)
    }

    /// Write a doorbell response frame into the response page. The guest zeroed
    /// that page before ringing, so writing only the frame leaves a clean tail.
    fn write_doorbell_response(&mut self, resp: &[u8]) -> Result<(), VmmError> {
        let Some(dst) = self.guest_slice_mut(RESP_GPA as u64, resp.len()) else {
            return Err(VmmError::ContractViolation(format!(
                "doorbell response page {RESP_GPA:#x}+{} is not backed",
                resp.len()
            )));
        };
        dst.copy_from_slice(resp);
        // A host-side RAM write the backend's dirty log cannot see — record it
        // for the drain union (task 95 M2.1 safety rule). The ABI GPA is
        // absolute (vendor-invariant), so the dirty gfn is correct on both arches.
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
        if header.service == ServiceId::Pvclock as u16 {
            // AVAILABILITY FIRST (cross-model r5 P2): an unoffered service answers
            // `UnknownService` — before any payload or opcode classification. A
            // composition that keeps the doorbell alive for some *other* channel
            // must not leak the pvclock service's existence by grading its
            // requests (`BadRequest` for a malformed payload, `UnknownOpcode` for
            // a bad op) when the service is not there at all. That is the generic
            // dispatcher's contract (INTEGRATION.md §1) and the same posture Event
            // / Sdk / Entropy / Net take below.
            if !self.pvclock_available() {
                let n = encode_error(
                    header.service,
                    header.opcode,
                    header.seq,
                    Status::UnknownService,
                    resp,
                );
                return (n, None);
            }
            if header.opcode != 1 {
                let n = encode_error(
                    header.service,
                    header.opcode,
                    header.seq,
                    Status::UnknownOpcode,
                    resp,
                );
                return (n, None);
            }
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
        // The ordered Payload service (id 8, op 1): consume exactly one entry
        // from the branch's recorded payload tape. Availability is checked
        // before opcode/payload classification, matching every other service.
        // Exhaustion returns a framed OutOfRange response and surfaces a
        // terminal Quiescent stop at this same atomic doorbell exit.
        if header.service == ServiceId::Payload as u16 {
            if !self.doorbell_service_offered(header.service) {
                let n = encode_error(
                    header.service,
                    header.opcode,
                    header.seq,
                    Status::UnknownService,
                    resp,
                );
                return (n, None);
            }
            if header.opcode != 1 {
                let n = encode_error(
                    header.service,
                    header.opcode,
                    header.seq,
                    Status::UnknownOpcode,
                    resp,
                );
                return (n, None);
            }
            if payload.len() != 4 {
                let n = encode_response(
                    ServiceId::Payload,
                    1,
                    header.seq,
                    Status::BadRequest,
                    &[],
                    resp,
                )
                .unwrap_or(0);
                return (n, None);
            }
            let bytes = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
            if bytes == 0 || bytes as usize > MAX_PAYLOAD {
                let n = encode_response(
                    ServiceId::Payload,
                    1,
                    header.seq,
                    Status::BadRequest,
                    &[],
                    resp,
                )
                .unwrap_or(0);
                return (n, None);
            }
            let pulled = self
                .sdk
                .as_mut()
                .expect("payload availability requires SDK")
                .env
                .pull_payload(bytes);
            return match pulled {
                Ok(Some(entry)) => {
                    let n = encode_response(
                        ServiceId::Payload,
                        1,
                        header.seq,
                        Status::Ok,
                        &entry,
                        resp,
                    )
                    .unwrap_or(0);
                    (n, None)
                }
                Ok(None) => {
                    let n = encode_response(
                        ServiceId::Payload,
                        1,
                        header.seq,
                        Status::OutOfRange,
                        &[],
                        resp,
                    )
                    .unwrap_or(0);
                    (n, Some(SdkStop::Quiescent))
                }
                Err(_) => {
                    let n = encode_response(
                        ServiceId::Payload,
                        1,
                        header.seq,
                        Status::BadRequest,
                        &[],
                        resp,
                    )
                    .unwrap_or(0);
                    (n, None)
                }
            };
        }
        // The Event service (id 4, op 1): capture the `Moment`-stamped emission
        // and, for an assert violation / `setup_complete`, arm a stop.
        //
        // Gated on the SDK channel being wired (r3 — the PR-68 `SdkStop`
        // lesson): the doorbell is serviced whenever ANY channel is enabled
        // (SDK / Net / pvclock), so a pvclock-only or net-only composition
        // can reach this arm. Without the gate an assert-violation Event
        // would answer Ok and even surface `Step::SdkStop` into a session
        // with no SDK channel; an unoffered service answers a clean
        // `UnknownService` instead — never a fake success.
        if header.service == ServiceId::Event as u16 && header.opcode == 1 {
            if self.sdk.is_none() {
                let n = encode_response(
                    ServiceId::Event,
                    1,
                    header.seq,
                    Status::UnknownService,
                    &[],
                    resp,
                )
                .unwrap_or(0);
                return (n, None);
            }
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
                // host-noisy doorbell OUT — not sealable here. Defer a snapshot
                // point; the control loop surfaces it at the next synchronized
                // boundary, where a seal succeeds.
                if defer {
                    sdk.pending_snapshot = true;
                }
            }
            if defer {
                self.sdk_snapshot_reentry_required = true;
            }
            let n = encode_response(ServiceId::Event, 1, header.seq, Status::Ok, &[], resp)
                .unwrap_or(0);
            return (n, stop);
        }
        // The SDK service (id 6, op 1): resolve a buggify decision.
        //
        // Gated on the SDK channel being wired (r3): without it,
        // `decide_buggify` on an SDK-less VM would answer a fabricated
        // nominal "don't fire" as a SUCCESS — a guest probing for buggify
        // support must get `UnknownService` (the same posture as Net below).
        if header.service == ServiceId::Sdk as u16 && header.opcode == 1 {
            if self.sdk.is_none() {
                let n = encode_response(
                    ServiceId::Sdk,
                    1,
                    header.seq,
                    Status::UnknownService,
                    &[],
                    resp,
                )
                .unwrap_or(0);
                return (n, None);
            }
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
        // M6 SDK threshold protocol (id 6, op 2): a cooperating instrumented
        // runtime reports the exact basic-block count prescribed at its prior
        // exit. The response prescribes the next threshold and resolves one
        // scheduler decision through the same RecordedEnv used by every other
        // guest control-plane choice.
        if header.service == ServiceId::Sdk as u16 && header.opcode == 2 {
            if self.sdk.is_none() {
                let n = encode_response(
                    ServiceId::Sdk,
                    2,
                    header.seq,
                    Status::UnknownService,
                    &[],
                    resp,
                )
                .unwrap_or(0);
                return (n, None);
            }
            if payload.len() != SDK_COVERAGE_REQUEST_LEN {
                let n =
                    encode_response(ServiceId::Sdk, 2, header.seq, Status::BadRequest, &[], resp)
                        .unwrap_or(0);
                return (n, None);
            }
            let thread = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
            let observed = u64::from_le_bytes([
                payload[4],
                payload[5],
                payload[6],
                payload[7],
                payload[8],
                payload[9],
                payload[10],
                payload[11],
            ]);
            let ready = u32::from_le_bytes([payload[12], payload[13], payload[14], payload[15]]);
            match self.decide_coverage(moment, thread, observed, ready) {
                Ok((next, selected)) => {
                    let mut answer = [0_u8; SDK_COVERAGE_RESPONSE_LEN];
                    answer[0..8].copy_from_slice(&next.to_le_bytes());
                    answer[8..12].copy_from_slice(&selected.to_le_bytes());
                    let n =
                        encode_response(ServiceId::Sdk, 2, header.seq, Status::Ok, &answer, resp)
                            .unwrap_or(0);
                    return (n, None);
                }
                Err(status) => {
                    let n = encode_response(ServiceId::Sdk, 2, header.seq, status, &[], resp)
                        .unwrap_or(0);
                    return (n, None);
                }
            }
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
            // Gated on an SDK or Net channel being wired (r3): entropy_fill
            // is the cooperating-guest supply riding those channels, and a
            // draw ADVANCES the one shared seeded stream RDRAND uses — a
            // pvclock-only composition must not let a doorbell ring perturb
            // the RNG stream of a run that never offered the service. Its
            // pre-pvclock reachability was exactly (sdk || net); preserved.
            if self.sdk.is_none() && self.net.is_none() {
                let n = encode_response(
                    ServiceId::Entropy,
                    1,
                    header.seq,
                    Status::UnknownService,
                    &[],
                    resp,
                )
                .unwrap_or(0);
                return (n, None);
            }
            let mut buf = [0_u8; MAX_PAYLOAD];
            let (status, got) = match self.vtime.as_mut() {
                Some(vt) => vt.draw_entropy(payload, &mut buf),
                None => (Status::BadRequest, 0),
            };
            let m = encode_response(ServiceId::Entropy, 1, header.seq, status, &buf[..got], resp)
                .unwrap_or(0);
            return (m, None);
        }
        // Any other service/opcode. Two rules, in order (cross-model r7 P2):
        // (1) AVAILABILITY BEFORE OPCODE — an unoffered service answers
        //     `UnknownService` for any opcode, so a composition that keeps the
        //     doorbell alive for one channel never advertises another by grading
        //     a bad opcode `UnknownOpcode`. This gates every known service the
        //     same way its `op == 1` arm already does.
        // (2) A KNOWN, OFFERED service with a bad opcode answers `UnknownOpcode`;
        //     an entirely unrecognized service id echoes the raw fields as
        //     `UnknownService` (round-9 P2).
        // Never a silent drop — the guest reads an unwritten response page as a
        // host rejection and hangs, violating the hypercall error contract.
        let known = matches!(
            header.service,
            s if s == ServiceId::Event as u16
                || s == ServiceId::Sdk as u16
                || s == ServiceId::Net as u16
                || s == ServiceId::Entropy as u16
                || s == ServiceId::Pvclock as u16
                || s == ServiceId::Payload as u16
        );
        if known && self.doorbell_service_offered(header.service) {
            let n = encode_error(
                header.service,
                header.opcode,
                header.seq,
                Status::UnknownOpcode,
                resp,
            );
            (n, None)
        } else {
            // Unoffered known service OR an unrecognized id: both `UnknownService`.
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
    /// - **`frame_complete`** (`SDK_NS_LIFECYCLE`, local 1): also arms the
    ///   deferred snapshot point and carries exactly one little-endian `u64`
    ///   cumulative emulated-frame count. Any other width is malformed.
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
            // `frame_complete` carries exactly one cumulative u64 frame count.
            SDK_NS_LIFECYCLE if local == 1 => {
                if data.len() == 8 {
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
    /// exact exit-count V-time to stamp the point. The control loop
    /// calls this after a `Continued` step; `true` means surface
    /// `StopReason::SnapshotPoint` here — a point where the explorer's eager
    /// `save_vm_state` seal succeeds, not `NotQuiescent`.
    ///
    /// **Round-4 P1:** gating on exact time alone surfaced a point at the
    /// first synchronized exit after `setup_complete` even when that exit was an
    /// RDRAND/RDSEED (a staged RNG completion), which `save_vm_state` rejects — and
    /// clearing `pending_snapshot` on that unsealable surface LOST the point. Now
    /// `pending_snapshot` is cleared ONLY when the point is actually surfaced (a
    /// sealable boundary), so an RNG boundary defers it to the next clean one.
    pub fn take_snapshot_point(&mut self) -> bool {
        if !self.sdk_snapshot_reentry_required
            && self.can_snapshot()
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

    /// Validate one crossed coverage threshold and resolve the runnable index.
    /// No state advances on rejection: a stale/skipped count, zero runnable
    /// set, overflow, or malformed environment answer is a clean protocol
    /// failure and cannot mint a phantom schedule decision.
    fn decide_coverage(
        &mut self,
        moment: u64,
        thread: u32,
        observed: u64,
        ready: u32,
    ) -> Result<(u64, u32), Status> {
        use environment::{Answer, DecisionPoint, Environment, Outcome};
        let Some(sdk) = self.sdk.as_mut() else {
            return Err(Status::UnknownService);
        };
        let expected = sdk
            .coverage_thresholds
            .get(&thread)
            .copied()
            .unwrap_or(SDK_COVERAGE_QUANTUM);
        if ready == 0 || observed != expected {
            return Err(Status::BadRequest);
        }
        let next = observed
            .checked_add(SDK_COVERAGE_QUANTUM)
            .ok_or(Status::OutOfRange)?;
        sdk.env.set_moment(moment);
        let answer = match sdk.env.decide(&DecisionPoint::Scheduler { ready }) {
            Outcome::Resolved(answer) => answer,
            Outcome::NeedsHost => return Err(Status::Internal),
        };
        let Answer::Supply(bytes) = answer else {
            return Err(Status::Internal);
        };
        let selected_bytes: [u8; 4] = bytes.as_slice().try_into().map_err(|_| Status::Internal)?;
        let selected = u32::from_le_bytes(selected_bytes);
        if selected >= ready {
            return Err(Status::Internal);
        }
        sdk.coverage_thresholds.insert(thread, next);
        sdk.coverage
            .push((moment, thread, observed, ready, selected));
        Ok((next, selected))
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
    ///   the mock): the **deterministic last-intercept anchor** — the same value the
    ///   `VTIM`/`LAPC` hash uses. The patched backend traps every `RDTSC`, so the
    ///   anchor advances densely *and* deterministically, and two same-seed boots
    ///   fire the timer at bit-identical V-times (Phase B.2 / task-30 Phase C).
    /// - **Stock backend** (no `RDTSC` trap): the anchor advances only at the rare
    ///   `RDMSR(IA32_TSC)` intercepts and would freeze post-boot, so the periodic
    ///   tick would never advance jiffies and the userspace serial-TX drain would
    ///   stall. Read the **live** virtual-time clock instead — it advances with guest
    ///   branches, so the timer keeps firing and the boot reaches `GUEST_READY`.
    ///   Stock claims no determinism (Phase B.1 only *reaches* the milestone), so a
    ///   host-noisy live read is sound here. The live read at this exit boundary is
    ///   the work retired up to the faulting instruction (no guest code runs between
    ///   the exit and this call).
    ///
    /// A failed virtual-time-clock read is **fail-closed** ([`VmmError::Work`]) — the same
    /// posture as the TSC/RNG completions — rather than silently reusing a stale
    /// `assigned_clock` (which would freeze or shift the timer, a determinism
    /// hazard) or fabricating a clock value.
    pub(crate) fn now_vns(&self) -> Result<u64, VmmError> {
        match &self.vtime {
            Some(vt) => Ok(vt.clock.vns()),
            None => Ok(0),
        }
    }

    /// Publish the next host-scheduled V-time solely for idle wakeup planning.
    pub(crate) fn set_idle_wake_vns(&mut self, vns: Option<u64>) {
        self.idle_wake_vns = vns;
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
        // Resolve the absolute GPA through the shared resolver (arm64 GPAs are
        // absolute over RAM_BASE; x86's RAM is at base 0, so the offset is
        // unchanged). A GPA outside every backed region fails closed — never a
        // wrong-offset upset into the main RAM (review r11).
        let Some(dst) = self.guest_slice_mut(gpa, 8) else {
            return Err(VmmError::ContractViolation(format!(
                "CorruptMemory gpa {gpa:#x} + 8 is not backed by guest RAM — refusing to clip, \
                 wrap, or apply the upset at a wrong offset"
            )));
        };
        let word = u64::from_le_bytes(<[u8; 8]>::try_from(&dst[..]).expect("exactly 8 bytes"));
        dst.copy_from_slice(&(word ^ mask).to_le_bytes());
        // A host-side RAM write the backend's dirty log cannot see — record it
        // (the 8-byte upset may straddle a page boundary; the helper covers both).
        self.mark_host_dirty(gpa, 8);
        Ok(())
    }

    /// Record `[gpa, gpa + len)` as **host-written** for the dirty drain (task
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

    /// Drain the **complete dirty-gfn set since the last drain** — the
    /// backend's guest-write log unioned with the host-side writes this `Vmm`
    /// performed — sorted ascending, deduplicated; and re-arm both for the next
    /// window (task 95 M2.1).
    ///
    /// Returns `None` on **any doubt**: the backend cannot drain (no dirty
    /// tracking, an ioctl error) or an untrackable full-image host write
    /// happened ([`Vmm::restore_guest_memory`]). `None` obliges the caller to
    /// full-scan — the dirty set is a cost hint, never a correctness input, so
    /// this deliberately returns an `Option`, not a `Result` whose error a
    /// caller could act on. After a `None` the tracking window is NOT re-armed; call
    /// [`Vmm::reset_dirty_tracking`] at the next baseline.
    pub fn drain_dirty_pages(&mut self) -> Option<Vec<u64>> {
        if self.host_dirty_wholesale {
            return None;
        }
        let mut gfns = self.backend.drain_dirty_pages().ok()?;
        // Fold in the host-side gfns.
        gfns.extend(self.host_dirty.iter().copied());
        self.host_dirty.clear();
        // Normalize ABSOLUTE guest GFNs to **main-RAM-relative** indices (review
        // r15 — the last of the r11 GPA family, in GFN space). The backend dirty
        // log and `mark_host_dirty` report absolute GFNs (`gpa / 4096`), but
        // `SnapshotEngine::snapshot_derive` indexes the main RAM **0-based**. On
        // arm64 (RAM based high) an absolute GFN is out of range (forcing a
        // wasteful full-scan fallback), and the dedicated doorbell memslot's GFNs
        // are NOT main-RAM pages — that page rides the device blob (r11), never
        // the main-RAM snapshot. So keep only GFNs inside the main RAM's
        // `[base, base + pages)` window and rebase them; x86 (base 0) is the
        // identity, so its dirty set is byte-for-byte unchanged.
        let base = self.ram_base_gpa / 4096;
        let pages = (self.ram.len() / 4096) as u64;
        let mut rel: Vec<u64> = gfns
            .into_iter()
            .filter_map(|g| g.checked_sub(base).filter(|&r| r < pages))
            .collect();
        rel.sort_unstable();
        rel.dedup();
        Some(rel)
    }

    /// Drain-and-discard: reset the dirty tracking so the **current** state is
    /// the baseline the next [`Vmm::drain_dirty_pages`] measures from (task 95
    /// M2.1's arm point — right after a seal or a branch restore). Clears the
    /// host-side set and the wholesale latch, and drains the backend log.
    /// Returns `true` iff the backend log was actually reset — `false` means
    /// tracking is not armed and the next capture must full-scan.
    pub fn reset_dirty_tracking(&mut self) -> bool {
        self.host_dirty.clear();
        self.host_dirty_wholesale = false;
        self.backend.drain_dirty_pages().is_ok()
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
        // Either determinism clock: descriptive mode needs the exact hardware
        // counter, while assigned-at-exit mode carries its clock entirely in
        // `vns_base` and intentionally needs no hardware counter.
        let Some(_vt) = self.vtime.as_ref() else {
            return Ok(IdleAction::Terminal);
        };
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
        //     **and** a staged host-fault arrival ([`set_idle_wake_vns`](Vmm::set_idle_wake_vns)).
        //     Jump to `min(timer, arrival)`, waking at the arrival to apply.
        //
        //     **The arrival wakes independent of the fabric (PR #51 round-6).** A host
        //     fault is a host-plane event, not a guest interrupt — so a V-time-wired
        //     guest with **no fabric wired** that idles before a staged `Moment` still
        //     wakes at the arrival to apply it, rather than going `Terminal` and
        //     silently never applying an accepted perturb. The timer half stays
        //     fabric-gated (there is no timer without a fabric). With neither a timer
        //     nor a staged arrival the guest is terminal — byte-identical to before.
        let timer = <B::A as Vendor>::deliverable_timer_deadline_vns(self);
        let wake = match (timer, self.idle_wake_vns) {
            (Some(timer), Some(host)) => Some(timer.min(host)),
            (Some(timer), None) => Some(timer),
            (None, host) => host,
        };
        match wake {
            Some(vns) => Ok(IdleAction::JumpToDeadline(vns)),
            // Neither a pending, a timer, nor an arrival wake → terminal.
            None => Ok(IdleAction::Terminal),
        }
    }

    /// Resume a *resumable idle* `HLT` by **jumping** V-time to the armed timer's
    /// deadline `deadline_vns` — reaching the next scheduled event without executing a
    /// single instruction.
    ///
    /// **Exit-boundary variability-free + work-axis epoch rebase (task-52 review fixes).** Two intertwined
    /// determinism requirements drive this:
    ///
    /// 1. *No host-noisy read.* A live `work()` read at a `HLT` is host-noisy (the
    ///    task-27 box O1 evidence shows a non-V-time-intercept live read **diverges**
    ///    across same-seed runs). So the landing V-time is derived from the **deterministic**
    ///    anchor [`assigned_clock`](VtimeWiring) + the (seed-deterministic) timer
    ///    deadline — never the live counter at the halt.
    /// 2. *Work-axis epoch consistency.* The clock is `vns(work) = vns_base + work·ratio`
    ///    over the **cumulative** counter. Simply bumping `vns_base` (against the stale
    ///    anchor) while the counter keeps counting leaves the two axes inconsistent: the
    ///    pre-idle branches (between the last intercept and the halt) would be counted a
    ///    second time at the next intercept, and the next deadline→work conversion
    ///    ([`Self::preemption_deadline`] via [`VClock::work_for_vns`]) would land *behind*
    ///    the live counter → the periodic tick fires immediately (overdue), breaking
    ///    cadence. So the jump **rebases the work epoch**: it resets the VM-exit
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
        // The landing V-time, decided by the planner from the EXIT-BOUNDARY VARIABILITY-FREE anchor clock
        // (never a live HLT read). For a future deadline (guaranteed by `idle_action`'s
        // "not already fired" gate) this is exactly `deadline_vns`.
        let (landing, snap) = {
            let vt = self
                .vtime
                .as_ref()
                .expect("JumpToDeadline implies V-time wired");
            let now_vns = vt.clock.vns();
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
        // Trace the idle V-time landing (deterministic; observability only, not hashed; capped).
        if self.idle_landings.len() < EVENT_TRACE_CAP {
            self.idle_landings.push(landing);
        }
        Ok(Step::Continued)
    }

    /// Current V-time for diagnostic logs, or `None` when V-time is unwired.
    pub(crate) fn current_vns(&self) -> Option<u64> {
        self.vtime.as_ref().map(VtimeWiring::virtual_time_vns)
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

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
impl Vmm<vmm_backend::HvfBackend> {
    /// Handle for the host-only liveness monitor to abort a stuck HVF entry.
    pub fn hvf_exit_handle(&self) -> vmm_backend::HvfExitHandle {
        self.backend.exit_handle()
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
/// - **Restore-transparency.** `vns_base` and the virtual-time clock are **not** hashed
///   separately; they are folded into one effective-V-time field,
///   `clock.snapshot_vns(assigned_clock) = vns_base + assigned_clock·ratio`.
///   So a restored VM (`vns_base = E`, work `0`) and a fresh VM at the same effective
///   V-time (`vns_base = 0`, work `E`) hash **identically** — the equivalence
///   `unison::compare_runs` relies on.
/// - **Determinism-twice.** The effective V-time is anchored to
///   `assigned_clock` — the **deterministic** work at the last V-time intercept
///   (every determinism-cap trap RDTSC/RDTSCP/RDRAND/RDSEED, and the
///   `IA32_TSC`/`IA32_TSC_ADJUST` MSR paths) — **never** a live
///   `work()` read at hash time. A terminal live read carries the non-deterministic
///   post-last-intercept exit-path exit-boundary variability, which is exactly what made the `VTIM` chunk
///   diverge intermittently across two same-seed runs (box corpus O1, PR #51). The
///   encoding is now **total and infallible** (no counter read, no poison sentinel).
///
/// **Deliberate property — `state_blob` is V-time replay-equivalence up to the last
/// synchronized intercept (integrator ruling).** The effective V-time is the V-time at
/// the **last V-time intercept** — the synchronized, deterministic point — **not** the
/// live counter at the hashing exit. So **two states are equal iff identical at that
/// last intercept**; post-intercept work — distinguishable only by re-synchronizing at
/// the next RDTSC/RNG — is **intentionally not captured because it is not
/// deterministically measurable** (only the determinism-cap traps + TSC MSRs are
/// deterministic; the raw counter at a non-V-time exit carries the non-deterministic
/// exit-boundary variability that was the original O1 bug). This is **not a silent bug — it is the correct
/// hash**: it is **exact for same-seed determinism (O1)** — box-proven, both runs reach
/// the same intercepts with the same deterministic work — and the under-capture for
/// *differential* comparison is resolved at the very next intercept. Hashing at
/// non-intercept exits is **required** (the corpus checkpoints at `isa-debug-exit`, a
/// non-intercept), so "refuse to hash off an intercept" would be wrong; hashing the
/// live counter would reintroduce exit-boundary variability. The **snapshot** path has the *opposite*
/// requirement (it needs the exact current V-time, so [`Vmm::save_vtime`] fails closed
/// off an intercept) — same exit-boundary variability fact, different correct resolution.
fn encode_vtime(vt: &VtimeWiring) -> Vec<u8> {
    let mut v = Vec::new();
    // Preserve the frozen N1 VTIM preimage. These are the historical v1
    // assigned-clock marker and integer conversion numerator; neither is live
    // configuration after N2, but removing their bytes would rewrite every
    // previously attested state hash.
    v.push(1);
    v.extend_from_slice(&1_u64.to_le_bytes());
    for x in [vt.cfg.guest_hz, vt.cfg.guest_base, vt.guest_clock_offset] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.extend_from_slice(&vt.clock.vns().to_le_bytes());
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
        Some(SdkStop::Quiescent) => v.push(2),
    }
    v.push(u8::from(sdk.pending_snapshot));
    // The active fault policy (round-8 P1): a stream position alone does not
    // determine the buggify fire/nominal sequence — the policy does — so two
    // same-stream forks under different policies must hash differently.
    v.extend_from_slice(&(sdk.policy.len() as u32).to_le_bytes());
    v.extend_from_slice(&sdk.policy);
    // Preserve every pre-M2 SDK hash byte when no payload service is offered.
    // When offered, fold the canonical live state: only the unconsumed suffix.
    if let Some(payloads) = sdk.env.remaining_payloads() {
        v.extend_from_slice(b"PAYL");
        let count = u64::try_from(payloads.len()).unwrap_or(u64::MAX);
        v.extend_from_slice(&count.to_le_bytes());
        for entry in payloads {
            let len = u64::try_from(entry.len()).unwrap_or(u64::MAX);
            v.extend_from_slice(&len.to_le_bytes());
            v.extend_from_slice(&entry);
        }
    }
    // Preserve every pre-M6 SDK hash byte until the threshold protocol is
    // actually exercised. Once it is, the host-side expected counters govern
    // which future guest callback is accepted and are therefore state.
    if !sdk.coverage_thresholds.is_empty() {
        v.extend_from_slice(b"COVR");
        let count = u64::try_from(sdk.coverage_thresholds.len()).unwrap_or(u64::MAX);
        v.extend_from_slice(&count.to_le_bytes());
        for (thread, threshold) in &sdk.coverage_thresholds {
            v.extend_from_slice(&thread.to_le_bytes());
            v.extend_from_slice(&threshold.to_le_bytes());
        }
    }
    v
}

#[cfg(test)]
mod tests {
    //! Engine tests, driven over the x86 vendor (`MockBackend`'s `Arch` is `X86`) —
    //! the engine is generic, but a test needs *a* vendor to run against.

    use super::*;
    use crate::virtual_time::NormalizedEventClass;
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

    /// A `Vmm<MockBackend>` with deterministic virtual time wired.
    fn vtime_vmm(exits: Vec<Exit<X86>>, seed: u64) -> Vmm<MockBackend> {
        let mut vmm = Vmm::new(configured_mock(exits), GuestRam::new(0x1000).unwrap());
        vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), seed).unwrap());
        vmm
    }

    #[test]
    fn state_hash_streaming_keeps_the_frozen_blob_digest() {
        let vmm = vtime_vmm(Vec::new(), 0x5eed);
        let mut expected = Sha256::new();
        expected.update(vmm.state_blob());
        let expected: [u8; 32] = expected.finalize().into();
        assert_eq!(vmm.state_hash(), expected);
    }

    #[test]
    fn deferred_checkpoint_hash_is_byte_identical_and_cannot_overwrite() {
        let exits = || {
            (0..256)
                .map(|_| Exit::Arch(X86Exit::Rdtsc))
                .collect::<Vec<_>>()
        };
        let mut synchronous = vtime_vmm(exits(), 7);
        let mut deferred = vtime_vmm(exits(), 7);
        deferred.defer_virtual_time_checkpoint_hashes().unwrap();

        for _ in 0..256 {
            assert_eq!(synchronous.step().unwrap(), Step::Continued);
            assert_eq!(deferred.step().unwrap(), Step::Continued);
        }

        let expected = synchronous
            .virtual_time_trace()
            .unwrap()
            .normalized_log()
            .events[255]
            .state_hash
            .unwrap();
        assert_eq!(deferred.state_hash(), expected);
        assert_eq!(
            deferred
                .virtual_time_trace()
                .unwrap()
                .normalized_log()
                .events[255]
                .state_hash,
            None
        );
        deferred
            .checkpoint_virtual_time_trace_at(255, expected)
            .unwrap();
        assert_eq!(
            synchronous.virtual_time_trace().unwrap().normalized_log(),
            deferred.virtual_time_trace().unwrap().normalized_log()
        );
        assert!(
            deferred
                .checkpoint_virtual_time_trace_at(255, expected)
                .is_err()
        );
        assert!(
            deferred
                .checkpoint_virtual_time_trace_at(254, expected)
                .is_err()
        );
    }

    #[test]
    fn trace_and_snapshot_boundary_guards_fail_closed_independently() {
        assert!(!synchronous_checkpoint_due(false, false));
        assert!(!synchronous_checkpoint_due(false, true));
        assert!(synchronous_checkpoint_due(true, false));
        assert!(!synchronous_checkpoint_due(true, true));
        let mut unwired = Vmm::new(configured_mock(Vec::new()), GuestRam::new(0x1000).unwrap());
        assert!(unwired.checkpoint_virtual_time_trace().is_err());
        assert_eq!(unwired.current_vns(), None);
        unwired.set_idle_wake_vns(Some(123));
        assert_eq!(unwired.idle_wake_vns, Some(123));
        unwired.set_idle_wake_vns(None);
        assert_eq!(unwired.idle_wake_vns, None);
        unwired.rng_completion_staged = true;
        assert!(!unwired.can_snapshot());

        // A substrate-private raw record alone is enough to make enabling
        // deferred hashing too late; it need not also have a portable event.
        let mut raw_only = vtime_vmm(Vec::new(), 1);
        raw_only
            .virtual_time_trace
            .as_mut()
            .unwrap()
            .record_raw_only(vmm_backend::ExitReason::Sysreg, "raw-only".to_string())
            .unwrap();
        assert!(raw_only.defer_virtual_time_checkpoint_hashes().is_err());
        assert_eq!(raw_only.current_vns(), Some(0));

        // Deferral during a portable event requires an active schedule. The
        // wrapper must propagate that trace-level contract failure.
        let mut no_schedule = vtime_vmm(Vec::new(), 1);
        no_schedule
            .virtual_time_trace
            .as_mut()
            .unwrap()
            .begin(
                vmm_backend::ExitReason::Rdtsc,
                "rdtsc".to_string(),
                NormalizedEventClass::TimeRead,
                Vec::new(),
            )
            .unwrap();
        assert!(no_schedule.trace_arm_clockevent_defer().is_err());
    }

    #[test]
    fn lifecycle_event_classifier_distinguishes_frame_complete_from_neighbors() {
        let id = |local| (u32::from(SDK_NS_LIFECYCLE) << SDK_NS_SHIFT) | local;
        assert_eq!(
            Vmm::<MockBackend>::classify_sdk_event(id(1), &[0; 8]),
            SdkEventAction::DeferSnapshot
        );
        assert_eq!(
            Vmm::<MockBackend>::classify_sdk_event(id(2), &[0; 8]),
            SdkEventAction::Capture
        );
        assert_eq!(
            Vmm::<MockBackend>::classify_sdk_event(id(1), &[]),
            SdkEventAction::Malformed
        );
    }

    #[test]
    fn doorbell_offer_predicate_is_exact_for_an_unconfigured_composition() {
        let mut vmm = Vmm::new(
            configured_mock(Vec::new()),
            GuestRam::new(TEST_RAM).unwrap(),
        );
        for service in [
            ServiceId::Event,
            ServiceId::Sdk,
            ServiceId::Net,
            ServiceId::Entropy,
            ServiceId::Payload,
            ServiceId::Pvclock,
        ] {
            assert!(!vmm.doorbell_service_offered(service as u16));
        }

        let spec = environment::EnvSpec::Seeded {
            seed: 7,
            policy: environment::FaultPolicy::none(),
        };
        vmm.enable_sdk(spec.materialize(), spec.policy());
        assert!(vmm.doorbell_service_offered(ServiceId::Event as u16));
        assert!(vmm.doorbell_service_offered(ServiceId::Sdk as u16));
        assert!(vmm.doorbell_service_offered(ServiceId::Entropy as u16));
        assert!(!vmm.doorbell_service_offered(ServiceId::Net as u16));
        assert!(!vmm.doorbell_service_offered(ServiceId::Payload as u16));
        assert!(!vmm.doorbell_service_offered(ServiceId::Pvclock as u16));

        let mut payload_spec = spec;
        payload_spec.set_payloads(Some(Vec::new()));
        let mut payload_vmm = Vmm::new(
            configured_mock(Vec::new()),
            GuestRam::new(TEST_RAM).unwrap(),
        );
        payload_vmm.enable_sdk(payload_spec.materialize(), payload_spec.policy());
        assert!(payload_vmm.doorbell_service_offered(ServiceId::Payload as u16));
        assert!(!payload_vmm.doorbell_service_offered(ServiceId::Pvclock as u16));
        assert!(!payload_vmm.doorbell_service_offered(u16::MAX));
    }

    // ---- task 95 M2.1: the dirty drain (backend log ∪ host-side writes) ----

    /// The drain unions the backend's guest-write log with the host-side
    /// writes the Vmm performed (here a `CorruptMemory` straddling a page
    /// boundary), sorted + deduplicated — and draining re-arms the window.
    #[test]
    fn drain_unions_backend_log_with_host_writes_and_drains() {
        let mut m = configured_mock(vec![]);
        m.push_dirty_gfns(vec![5, 3, 5]); // scripted guest writes, unsorted + dup
        let mut vmm = Vmm::new(m, GuestRam::new(TEST_RAM).unwrap());
        // An 8-byte upset straddling the page-6/page-7 boundary: both gfns count.
        vmm.apply_host_fault(&environment::HostFault::CorruptMemory {
            gpa: 7 * 4096 - 4,
            mask: environment::BitMask(0xFFFF_FFFF_FFFF_FFFF),
        })
        .unwrap();
        assert_eq!(vmm.drain_dirty_pages(), Some(vec![3, 5, 6, 7]));
        // Drained: the next window starts empty (the mock's exhausted script is
        // an empty guest-write set, and the host set was cleared).
        assert_eq!(vmm.drain_dirty_pages(), Some(vec![]));
    }

    /// The doorbell response write — the run loop's host-side RAM write — lands
    /// in the drain (the safety rule's production case).
    #[test]
    fn doorbell_response_write_is_drained_as_host_dirty() {
        let mut m = configured_mock(vec![]);
        m.enable_dirty_tracking();
        let mut vmm = Vmm::new(m, GuestRam::new(TEST_RAM).unwrap());
        vmm.write_doorbell_response(&[0xAB; 16]).unwrap();
        // RESP_GPA = 0xF000 → gfn 15.
        assert_eq!(
            vmm.drain_dirty_pages(),
            Some(vec![(RESP_GPA as u64) / 4096])
        );
    }

    /// Review r15: on a high-RAM-base (arm64) machine the backend dirty log
    /// reports **absolute** GFNs, but `SnapshotEngine::snapshot_derive` indexes
    /// the main RAM 0-based. The drain must rebase them to main-RAM-relative
    /// indices and exclude the dedicated doorbell memslot's GFNs (that page rides
    /// the device blob, r11) — otherwise high GFNs are out-of-range (full-scan
    /// fallback) and the doorbell slot mis-indexes the main RAM. x86 (base 0) is
    /// byte-identical (the two tests above are the neutrality proof).
    #[test]
    fn drain_rebases_high_base_gfns_and_excludes_the_doorbell_slot() {
        let mut m = configured_mock(vec![]);
        // Absolute GFNs as an arm64 backend logs them: two main-RAM pages (RAM at
        // 0x4000_0000 → base GFN 0x40000) plus the doorbell RESP page (GFN 15, a
        // separate low memslot).
        m.push_dirty_gfns(vec![0x4_0001, 0x4_0003, 15]);
        let mut vmm = Vmm::new(m, GuestRam::new(TEST_RAM).unwrap());
        vmm.ram_base_gpa = 0x4000_0000;

        let dirty = vmm.drain_dirty_pages().unwrap();
        assert_eq!(
            dirty,
            vec![1, 3],
            "high GFNs rebased to main-RAM indices; the doorbell slot's GFN excluded"
        );
        let pages = (vmm.ram.len() / 4096) as u64;
        assert!(
            dirty.iter().all(|&g| g < pages),
            "every drained GFN indexes the main RAM — no snapshot_derive fallback"
        );
    }

    /// A full-image host write (`restore_guest_memory`) poisons the drain —
    /// per-gfn tracking cannot vouch for it — until the explicit re-arm.
    #[test]
    fn wholesale_host_write_poisons_the_drain_until_reset() {
        let mut m = configured_mock(vec![]);
        m.enable_dirty_tracking();
        let mut vmm = Vmm::new(m, GuestRam::new(TEST_RAM).unwrap());
        vmm.restore_guest_memory(&vec![7u8; TEST_RAM]).unwrap();
        assert_eq!(vmm.drain_dirty_pages(), None, "untrackable ⇒ no dirty set");
        assert!(vmm.reset_dirty_tracking(), "re-arm at the new baseline");
        assert_eq!(vmm.drain_dirty_pages(), Some(vec![]));
    }

    /// Without backend dirty tracking the drain always declines (`None`) and
    /// the window never arms — the caller full-scans forever, never corrupts.
    #[test]
    fn drain_declines_without_backend_tracking() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        vmm.write_doorbell_response(&[1]).unwrap();
        assert_eq!(vmm.drain_dirty_pages(), None);
        assert!(!vmm.reset_dirty_tracking());
    }

    #[test]
    fn write_guest_pages_writes_pages_and_poison_dirty_drain_until_reset() {
        let mut backend = configured_mock(vec![]);
        backend.enable_dirty_tracking();
        let mut vmm = Vmm::new(backend, GuestRam::new(TEST_RAM).unwrap());
        let page_a = [0xA5_u8; 4096];
        let page_b = [0x5A_u8; 4096];

        vmm.write_guest_pages(&[(2, page_a), (5, page_b)]).unwrap();
        assert_eq!(&vmm.guest_memory()[2 * 4096..3 * 4096], &page_a);
        assert_eq!(&vmm.guest_memory()[5 * 4096..6 * 4096], &page_b);
        assert_eq!(vmm.drain_dirty_pages(), None);
        assert!(vmm.reset_dirty_tracking());
        assert_eq!(vmm.drain_dirty_pages(), Some(vec![]));
    }

    #[test]
    fn write_guest_pages_empty_input_is_a_noop() {
        let mut backend = configured_mock(vec![]);
        backend.enable_dirty_tracking();
        let mut vmm = Vmm::new(backend, GuestRam::new(TEST_RAM).unwrap());
        let before = vmm.guest_memory().to_vec();

        vmm.write_guest_pages(&[]).unwrap();

        assert_eq!(vmm.guest_memory(), &before);
        assert!(!vmm.host_dirty_wholesale);
        assert_eq!(vmm.drain_dirty_pages(), Some(vec![]));
    }

    #[test]
    fn write_guest_pages_out_of_range_is_atomic() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        let before = vmm.guest_memory().to_vec();
        let page = [0xCC_u8; 4096];
        let out_of_range = (TEST_RAM / 4096) as u64;

        assert!(matches!(
            vmm.write_guest_pages(&[(1, page), (out_of_range, page)]),
            Err(VmmError::ContractViolation(_))
        ));
        assert_eq!(vmm.guest_memory(), &before);
        assert!(vmm.host_dirty.is_empty());
        assert!(!vmm.host_dirty_wholesale);
    }

    #[test]
    fn write_guest_pages_duplicate_gfn_is_atomic() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        let before = vmm.guest_memory().to_vec();
        let page_a = [0x11_u8; 4096];
        let page_b = [0x22_u8; 4096];

        assert!(matches!(
            vmm.write_guest_pages(&[(3, page_a), (3, page_b)]),
            Err(VmmError::ContractViolation(_))
        ));
        assert_eq!(vmm.guest_memory(), &before);
        assert!(vmm.host_dirty.is_empty());
        assert!(!vmm.host_dirty_wholesale);
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

    /// M6 production doorbell path: op 2 consumes the Scheduler decision class,
    /// returns the next threshold + selected runnable, and records the exact
    /// per-call evidence. A separately materialized environment is the expected
    /// selection comparator; it does not call the VMM's coverage helper.
    #[test]
    fn coverage_doorbell_uses_the_scheduler_vocabulary() {
        use environment::{Answer, DecisionPoint, EnvSpec, Environment, FaultPolicy, Outcome};

        let spec = EnvSpec::Seeded {
            seed: 0x6d36,
            policy: FaultPolicy::none(),
        };
        let mut expected_env = spec.materialize();
        expected_env.set_moment(0);
        let expected = match expected_env.decide(&DecisionPoint::Scheduler { ready: 3 }) {
            Outcome::Resolved(Answer::Supply(bytes)) => {
                u32::from_le_bytes(bytes.try_into().expect("scheduler answer is four bytes"))
            }
            other => panic!("unexpected scheduler answer: {other:?}"),
        };

        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        vmm.enable_sdk(spec.materialize(), spec.policy());
        let mut request = [0_u8; SDK_COVERAGE_REQUEST_LEN];
        request[0..4].copy_from_slice(&7_u32.to_le_bytes());
        request[4..12].copy_from_slice(&SDK_COVERAGE_QUANTUM.to_le_bytes());
        request[12..16].copy_from_slice(&3_u32.to_le_bytes());
        let mut frame = [0_u8; HC_PAGE];
        let n = hypercall_proto::encode_request(ServiceId::Sdk, 2, 9, &request, &mut frame)
            .expect("coverage request");
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        assert_eq!(vmm.service_doorbell(n as u32).unwrap(), Step::Continued);
        let page = &vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE];
        let (header, payload) = decode(page).expect("coverage response");
        assert_eq!(header.status, Status::Ok as u16);
        assert_eq!(payload.len(), SDK_COVERAGE_RESPONSE_LEN);
        assert_eq!(
            u64::from_le_bytes(payload[0..8].try_into().unwrap()),
            SDK_COVERAGE_QUANTUM * 2
        );
        assert_eq!(
            u32::from_le_bytes(payload[8..12].try_into().unwrap()),
            expected
        );
        assert_eq!(
            vmm.sdk_coverage(),
            &[(0, 7, SDK_COVERAGE_QUANTUM, 3, expected)]
        );
    }

    /// Planted protocol negative plus replay closure: a skipped count is
    /// rejected without moving the stream or expected threshold; the correct
    /// count still succeeds, and snapshot restore reproduces the same future.
    #[test]
    fn coverage_threshold_is_enforced_and_snapshot_replay_reproduces() {
        use environment::{EnvSpec, FaultPolicy};

        let spec = EnvSpec::Seeded {
            seed: 0x51ced,
            policy: FaultPolicy::none(),
        };
        let build = || {
            let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            vmm.enable_sdk(spec.materialize(), spec.policy());
            vmm
        };
        let mut base = build();
        assert!(
            !encode_sdk_channel(base.sdk.as_ref().unwrap())
                .windows(4)
                .any(|window| window == b"COVR"),
            "an unused coverage channel preserves the pre-M6 hash bytes"
        );
        let first = base.decide_coverage(4, 1, 1, 2).unwrap();
        let mut coverage_suffix = b"COVR".to_vec();
        coverage_suffix.extend_from_slice(&1_u64.to_le_bytes());
        coverage_suffix.extend_from_slice(&1_u32.to_le_bytes());
        coverage_suffix.extend_from_slice(&first.0.to_le_bytes());
        assert!(
            encode_sdk_channel(base.sdk.as_ref().unwrap()).ends_with(&coverage_suffix),
            "an exercised threshold is canonical hash state"
        );
        let snap = base.sdk_snapshot().unwrap();
        let hash_after_first = base.state_hash();
        let log_after_first = base.sdk_coverage().to_vec();

        assert_eq!(
            base.decide_coverage(5, 1, 3, 2),
            Err(Status::BadRequest),
            "a skipped threshold is the planted negative"
        );
        assert_eq!(base.state_hash(), hash_after_first);
        assert_eq!(base.sdk_coverage(), log_after_first);
        let continuation = base.decide_coverage(5, 1, first.0, 2).unwrap();

        let mut replay = build();
        replay.sdk_restore(&snap);
        assert_eq!(replay.state_hash(), hash_after_first);
        assert_eq!(
            replay.decide_coverage(5, 1, first.0, 2).unwrap(),
            continuation
        );
        assert_eq!(replay.state_hash(), base.state_hash());
    }

    #[test]
    fn payload_tape_is_exact_hash_visible_snapshot_complete_and_exhausts_loudly() {
        use environment::{EnvSpec, FaultPolicy};

        fn spec(entries: Vec<Vec<u8>>) -> EnvSpec {
            let mut spec = EnvSpec::Seeded {
                seed: 7,
                policy: FaultPolicy::none(),
            };
            spec.set_payloads(Some(entries));
            spec
        }

        fn ring(vmm: &mut Vmm<MockBackend>, bytes: u32) -> (Step, u16, Vec<u8>) {
            let mut frame = [0_u8; HC_PAGE];
            let n = hypercall_proto::encode_request(
                ServiceId::Payload,
                1,
                1,
                &bytes.to_le_bytes(),
                &mut frame,
            )
            .unwrap();
            vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
            let step = vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
            let page = &vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE];
            let (header, payload) = decode(page).unwrap();
            (step, header.status, payload.to_vec())
        }

        let make_vmm = |entries| {
            let tape = spec(entries);
            let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            vmm.enable_sdk(tape.materialize(), tape.policy());
            vmm
        };

        let mut zero = make_vmm(vec![Vec::new()]);
        assert_eq!(
            ring(&mut zero, 0),
            (Step::Continued, Status::BadRequest as u16, Vec::new())
        );
        assert_eq!(
            zero.sdk_snapshot().unwrap().remaining_payloads(),
            Some(vec![Vec::new()]),
            "zero is rejected before the tape can consume an empty entry"
        );

        let max_entry = vec![0x5a; MAX_PAYLOAD];
        let mut exact_max = make_vmm(vec![max_entry.clone()]);
        assert_eq!(
            ring(&mut exact_max, MAX_PAYLOAD as u32),
            (Step::Continued, Status::Ok as u16, max_entry),
            "the inclusive maximum remains admissible"
        );

        let oversized_len = MAX_PAYLOAD + 2;
        let mut oversized = make_vmm(vec![vec![0x33; oversized_len]]);
        assert_eq!(
            ring(&mut oversized, oversized_len as u32),
            (Step::Continued, Status::BadRequest as u16, Vec::new())
        );
        assert_eq!(
            oversized.sdk_snapshot().unwrap().remaining_payloads(),
            Some(vec![vec![0x33; oversized_len]]),
            "every value above the maximum is rejected before tape consumption"
        );

        let tape = spec(vec![vec![0x81, 4], vec![0, 2]]);
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        vmm.enable_sdk(tape.materialize(), tape.policy());

        let before = encode_sdk_channel(vmm.sdk.as_ref().unwrap());
        for invalid in [0, (MAX_PAYLOAD as u32) + 1] {
            assert_eq!(
                ring(&mut vmm, invalid),
                (Step::Continued, Status::BadRequest as u16, Vec::new())
            );
            assert_eq!(encode_sdk_channel(vmm.sdk.as_ref().unwrap()), before);
        }
        assert_eq!(
            ring(&mut vmm, 1),
            (Step::Continued, Status::BadRequest as u16, Vec::new()),
            "wrong length rejects without consuming"
        );
        assert_eq!(encode_sdk_channel(vmm.sdk.as_ref().unwrap()), before);

        assert_eq!(
            ring(&mut vmm, 2),
            (Step::Continued, Status::Ok as u16, vec![0x81, 4])
        );
        let after_first = encode_sdk_channel(vmm.sdk.as_ref().unwrap());
        assert_ne!(after_first, before, "the remaining suffix is hash state");
        let snap = vmm.sdk_snapshot().unwrap();
        assert_eq!(snap.remaining_payloads(), Some(vec![vec![0, 2]]));

        assert_eq!(
            ring(&mut vmm, 2),
            (Step::Continued, Status::Ok as u16, vec![0, 2])
        );
        assert_ne!(encode_sdk_channel(vmm.sdk.as_ref().unwrap()), after_first);
        vmm.sdk_restore(&snap);
        assert_eq!(
            encode_sdk_channel(vmm.sdk.as_ref().unwrap()),
            after_first,
            "restoring the snapshot restores the exact remaining suffix"
        );

        assert_eq!(ring(&mut vmm, 2).2, vec![0, 2]);
        assert_eq!(
            ring(&mut vmm, 2),
            (Step::SdkStop, Status::OutOfRange as u16, Vec::new())
        );
        assert_eq!(vmm.take_sdk_stop(), Some(SdkStop::Quiescent));

        let a = spec(vec![vec![1, 2]]);
        let b = spec(vec![vec![1, 3]]);
        let mut va = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        let mut vb = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        va.enable_sdk(a.materialize(), a.policy());
        vb.enable_sdk(b.materialize(), b.policy());
        assert_ne!(
            va.state_hash(),
            vb.state_hash(),
            "altering one staged chord changes full state"
        );
    }

    /// Review r10: the transport ABI pages (`REQ_GPA`/`RESP_GPA`) are **absolute**
    /// GPAs. On a machine whose RAM is based high (arm64, `RAM_BASE`) they fall
    /// below the RAM and must be a **dedicated low memslot**, never the main RAM's
    /// offset `REQ_GPA`. Exercised through the engine's `ram_base_gpa` resolution
    /// — the same path the arm64 vendor drives — with the existing mock: unmapped
    /// pages fail closed; mapped pages carry the exchange in the dedicated slot,
    /// leaving the wrong RAM offsets untouched; the host write marks the absolute
    /// gfn. (x86 keeps `ram_base_gpa == 0`, so its doorbell path is unchanged.)
    #[test]
    fn doorbell_uses_a_dedicated_memslot_when_ram_is_based_high() {
        use environment::{EnvSpec, FaultPolicy};

        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        vmm.ram_base_gpa = 0x4000_0000; // arm64 RAM_BASE: the ABI GPAs sit below it
        let spec = EnvSpec::Seeded {
            seed: 7,
            policy: FaultPolicy::none(),
        };
        vmm.enable_sdk(spec.materialize(), spec.policy());

        // Unmapped ABI pages → fail closed (the r10 bug was a silent wrong-offset
        // read of the high RAM at offset REQ_GPA).
        let err = vmm.service_doorbell(16).unwrap_err();
        assert!(
            format!("{err}").contains("not backed"),
            "unmapped ABI pages must fault: {err}"
        );

        // Map the dedicated pages (a second memslot at REQ_GPA) and stage a
        // request frame the Event service answers Ok.
        vmm.map_doorbell_pages().unwrap();
        let hit_id = (1u32 << 24) | 1;
        let mut payload = hit_id.to_le_bytes().to_vec();
        payload.extend_from_slice(&[0, 0, 0]); // DISP_HIT, detail_len = 0
        let mut buf = [0u8; HC_PAGE];
        let n =
            hypercall_proto::encode_request(ServiceId::Event, 1, 1, &payload, &mut buf).unwrap();
        let req_off = REQ_GPA - DOORBELL_MAP_GPA;
        vmm.doorbell_pages.as_mut().unwrap().as_mut_bytes()[req_off..req_off + n]
            .copy_from_slice(&buf[..n]);

        let step = vmm.service_doorbell(n as u32).unwrap();
        assert_eq!(step, Step::Continued);

        // The response landed in the dedicated slot, one page past the request.
        let resp_off = RESP_GPA - DOORBELL_MAP_GPA;
        let resp =
            vmm.doorbell_pages.as_ref().unwrap().as_bytes()[resp_off..resp_off + HC_PAGE].to_vec();
        let (hdr, _) = decode(&resp).expect("a valid response frame in the dedicated page");
        assert_eq!(hdr.status, Status::Ok as u16);

        // The main RAM at the ABI offsets was NEVER touched — the bug read/wrote
        // there (GPA 0x4000_E000/0x4000_F000), corrupting guest RAM.
        assert!(
            vmm.guest_memory()[REQ_GPA..RESP_GPA + HC_PAGE]
                .iter()
                .all(|&b| b == 0),
            "the dedicated memslot is used, never the main RAM's offset REQ_GPA"
        );

        // Dirty-range: the host response write records the ABSOLUTE RESP gfn
        // (0xF000 / 4096 = 15) for the drain union — not a RAM_BASE-relative
        // one (the bug would have marked 0x4000_F000's gfn, or none).
        assert!(
            vmm.host_dirty.contains(&(RESP_GPA as u64 / 4096)),
            "the response's absolute gfn 15 must be host-dirty: {:?}",
            vmm.host_dirty
        );
    }

    /// Review r11: with the ABI pages a dedicated region, the engine's hash and
    /// its absolute-GPA read/corrupt paths must cover / resolve across it.
    /// Exercised via a high `ram_base_gpa` (the arm64 memory model) on the mock;
    /// x86 (`ram_base_gpa == 0`) keeps every path unchanged (the existing x86
    /// read/corrupt/hash tests are the neutrality proof).
    #[test]
    fn high_ram_base_resolves_absolute_gpas_and_hashes_the_doorbell() {
        use environment::{BitMask, HostFault};

        let make = || {
            let mut v = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            v.ram_base_gpa = 0x4000_0000; // arm64 RAM_BASE
            v.map_doorbell_pages().unwrap();
            v
        };

        // P1(1) hash: two states differing ONLY in doorbell bytes hash
        // differently (the `DOOR` chunk); identical content hashes identically.
        let a = make();
        let mut b = make();
        assert_eq!(a.state_hash(), b.state_hash());
        b.doorbell_pages.as_mut().unwrap().as_mut_bytes()[7] ^= 0xFF;
        assert_ne!(
            a.state_hash(),
            b.state_hash(),
            "doorbell bytes must fold into state_hash"
        );

        // P1(2) control-plane resolution: an ABSOLUTE arm64 address (over
        // RAM_BASE) reads the right main-RAM bytes; a low unmapped GPA is
        // out-of-range, never a wrong offset into the main RAM; the dedicated ABI
        // page resolves at its absolute low GPA.
        let mut v = make();
        v.ram.as_mut_bytes()[0x100..0x104].copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(v.guest_slice(0x4000_0100, 4), Some(&[1u8, 2, 3, 4][..]));
        assert_eq!(v.guest_slice(0x100, 4), None);
        assert!(v.guest_slice(REQ_GPA as u64, HC_PAGE).is_some());

        // P1(2) corrupt_memory: a valid absolute GPA upsets the right byte; a low
        // unmapped GPA fails closed (never a wrong-offset upset).
        v.apply_host_fault(&HostFault::CorruptMemory {
            gpa: 0x4000_0100,
            mask: BitMask(0xFF),
        })
        .unwrap();
        assert_eq!(v.ram.as_bytes()[0x100], 1 ^ 0xFF);
        assert!(
            v.apply_host_fault(&HostFault::CorruptMemory {
                gpa: 0x500,
                mask: BitMask(0xFF),
            })
            .is_err(),
            "a low unmapped GPA corrupt must fail closed"
        );
    }

    /// Review r11 P1(1): the dedicated ABI pages must survive save/restore/branch
    /// — they ride the arm64 device blob (a separate memslot, not in the main-RAM
    /// snapshot). A full arm64 save→restore cycle, plus the wiring-mismatch guard.
    #[test]
    fn arm64_save_restore_preserves_the_doorbell_pages() {
        use crate::vendor::arm64::board::RAM_BASE as ARM_RAM_BASE;
        use vmm_backend::{Arm64Policy, MockArm64Backend};

        let arm_vmm = || {
            let mut b = MockArm64Backend::new();
            b.set_policy(&Arm64Policy::default()).unwrap();
            let mut v = Vmm::new(b, GuestRam::new(0x10_0000).unwrap());
            v.ram_base_gpa = ARM_RAM_BASE;
            v.map_doorbell_pages().unwrap();
            v
        };

        let mut src = arm_vmm();
        src.doorbell_pages.as_mut().unwrap().as_mut_bytes()[..4]
            .copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        let blob = src.save_vm_state().unwrap();

        // Restore into a freshly-composed twin (doorbell mapped): the bytes survive.
        let mut dst = arm_vmm();
        dst.restore_vm_state(&blob).unwrap();
        assert_eq!(
            &dst.doorbell_pages.as_ref().unwrap().as_bytes()[..4],
            &[0xAA, 0xBB, 0xCC, 0xDD]
        );

        // Restore into a VM WITHOUT the dedicated pages is a loud wiring mismatch.
        let mut nodoor = {
            let mut b = MockArm64Backend::new();
            b.set_policy(&Arm64Policy::default()).unwrap();
            Vmm::new(b, GuestRam::new(0x10_0000).unwrap())
        };
        let err = nodoor.restore_vm_state(&blob).unwrap_err();
        assert!(
            format!("{err}").contains("doorbell wiring mismatch"),
            "{err}"
        );
    }

    /// Review r12 (the GICV analogue): the `DOOR` hash chunk must have a matching
    /// `state_components()` entry, or a doorbell-only divergence reports a
    /// `state_hash` mismatch with **every** component matching — blinding the
    /// bisector. A doorbell-only difference must localize to the `doorbell`
    /// component; an unmapped VM exposes none.
    #[test]
    fn state_components_localizes_a_doorbell_only_divergence() {
        let make = || {
            let mut v = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            v.ram_base_gpa = 0x4000_0000;
            v.map_doorbell_pages().unwrap();
            v
        };
        let a = make();
        let mut b = make();
        b.doorbell_pages.as_mut().unwrap().as_mut_bytes()[3] ^= 0xFF;

        // state_hash differs (the DOOR chunk folds the pages in)...
        assert_ne!(a.state_hash(), b.state_hash());

        // ...and the `doorbell` component is exactly what localizes it: it
        // differs, and it is the ONLY differing component.
        let ca = a.state_components();
        let cb = b.state_components();
        let da = ca
            .iter()
            .find(|(l, _)| *l == "doorbell")
            .expect("a doorbell component");
        let db_ = cb
            .iter()
            .find(|(l, _)| *l == "doorbell")
            .expect("a doorbell component");
        assert_ne!(da.1, db_.1, "the doorbell component must localize it");
        for (la, dga) in &ca {
            if *la == "doorbell" {
                continue;
            }
            let dgb = cb.iter().find(|(lb, _)| lb == la).map(|(_, d)| d);
            assert_eq!(
                Some(dga),
                dgb,
                "component {la} must match (only doorbell differs)"
            );
        }

        // An unmapped VM (x86-style) exposes no `doorbell` component (additive;
        // present exactly when the `DOOR` chunk is).
        let plain = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        assert!(
            !plain
                .state_components()
                .iter()
                .any(|(l, _)| *l == "doorbell")
        );
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
        let frame_id = setup_id | 1;
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

        // --- frame_complete: EXACTLY one little-endian u64. ---
        assert_eq!(classify(frame_id, &17_u64.to_le_bytes()), C::DeferSnapshot);
        assert_eq!(classify(frame_id, &[]), C::Malformed);
        assert_eq!(classify(frame_id, &[0; 7]), C::Malformed);
        assert_eq!(classify(frame_id, &[0; 9]), C::Malformed);

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
        let frame_id = setup_id | 1;

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

        // A malformed frame_complete is rejected without capture or deferral.
        let mut v = mk();
        assert_eq!(
            ring(&mut v, frame_id, &[0; 7]),
            (Status::BadRequest as u16, false, 0),
            "a short frame_complete is rejected, never arms the deferral"
        );

        // The exact-width positive path is captured and arms the deferral.
        let mut v = mk();
        assert_eq!(
            ring(&mut v, frame_id, &17_u64.to_le_bytes()),
            (Status::Ok as u16, false, 1)
        );
    }

    /// A doorbell `OUT` on a Vmm with **no** channels reaches the default-deny
    /// dispatcher and answers `UnknownService` — the protocol's own promise — NOT
    /// a fatal `ContractViolation` (cross-model r10 P1). A clock-aware guest may
    /// probe service 7 (pvclock) on a VM that never offered it, and must be able
    /// to fall back to its trap-backstopped time instead of being killed.
    #[test]
    fn doorbell_probe_on_a_channel_less_vm_answers_unknown_service() {
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        // Stage the exact probe: a service-7 (pvclock) op-1 register request.
        let mut buf = [0u8; HC_PAGE];
        let n = hypercall_proto::encode_request(
            ServiceId::Pvclock,
            1,
            5,
            &0x4000u64.to_le_bytes(),
            &mut buf,
        )
        .unwrap();
        vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + n].copy_from_slice(&buf[..n]);
        // The doorbell port is modeled — the write is serviced (not fatal).
        let step = vmm.dispatch_out(DOORBELL_PORT, 4, n as u32).unwrap();
        assert_eq!(step, Step::Continued);
        // The composition offers no pvclock, so the dispatcher answers a clean
        // `UnknownService` frame echoing the probed service/opcode/seq.
        let page = vmm.guest_memory()[RESP_GPA..RESP_GPA + HC_PAGE].to_vec();
        let (hdr, pl) = decode(&page).expect("a response frame is written, not a silent drop");
        assert_eq!(
            hdr.status,
            Status::UnknownService as u16,
            "clean UnknownService"
        );
        assert_eq!(
            hdr.service,
            ServiceId::Pvclock as u16,
            "echoes the probed service id"
        );
        assert_eq!(
            (hdr.opcode, hdr.seq),
            (1, 5),
            "echoes the request opcode + seq"
        );
        assert!(pl.is_empty(), "an error frame carries no payload");
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
            v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 9).unwrap());
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
            vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 0x777).unwrap());
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
            vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 99).unwrap());
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
            1,
        );
        assert!(vmm.vtime_wired(), "wire_vtime reports the path as wired");
        let r = vmm.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Idle);
        assert_eq!(vmm.backend.completions(), &[Completion::Read(2)]);
    }

    #[test]
    fn rdtscp_completes_with_vtime_tsc() {
        // RDTSCP is resolved identically above the trait (the backend supplies
        // ECX=IA32_TSC_AUX below it); the VMM still completes the V-time value.
        let mut vmm = vtime_vmm(
            vec![Exit::Arch(X86Exit::Rdtscp), Exit::Common(CommonExit::Idle)],
            1,
        );
        vmm.run().expect("run");
        assert_eq!(vmm.backend.completions(), &[Completion::Read(2)]);
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
            SEED,
        );
        assert_eq!(a.step().unwrap(), Step::Continued); // RDRAND → first word (staged)
        assert_eq!(a.step().unwrap(), Step::Continued); // RDTSC → commits RDRAND; tsc=100
        let snap = a
            .save_vtime()
            .expect("save at clean boundary")
            .expect("wired");
        assert_eq!(snap.vns, 1001); // RNG control exit (1000) + time read (1)

        // Restore into B whose counter sits at a NON-zero 99: restore_vtime must
        // RESET it to 0 (else RDTSC would read work=99 → tsc=298, not 100), set
        // vns_base=50, and resume the RNG stream at the *next* word — not the
        // first. (B starting non-zero is what makes the counter-reset observable.)
        let mut b = vtime_vmm(
            vec![
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Arch(X86Exit::Rdrand { width: 8 }),
            ],
            SEED, // a different seed would be overwritten by restore anyway
        );
        b.restore_vtime(&snap).expect("restore");
        b.step().unwrap(); // RDTSC at reset work=0 → tsc(0) = 2*vns_base = 100
        b.step().unwrap(); // RDRAND → the word AFTER A's first draw

        // Clock continuity: B's first post-restore TSC equals A's TSC at the
        // snapshot point (100), even though B's counter restarted at 0.
        assert_eq!(b.backend.completions()[0], Completion::Read(2004));
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

    #[test]
    fn save_vtime_is_none_when_unwired() {
        let v = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert!(v.save_vtime().unwrap().is_none());
        assert!(!v.vtime_wired());
    }

    /// Task-27 (box-verification cross-model finding): `save_vtime` anchors `vns` to
    /// the deterministic `assigned_clock`, **not** a live counter read. A fresh VM
    /// is a synchronized point (work 0, V-time = `vns_base`), so the save succeeds; the
    /// source reads `777` but the anchor is `0`, so `vns` must be `0` — a live read
    /// would capture the host-noisy `777` (the terminal-read bug removed from the hash).
    #[test]
    fn save_vtime_anchors_vns_to_last_intercept_not_live_work() {
        let v = vtime_vmm(vec![], 1);
        let snap = v.save_vtime().expect("save").expect("wired");
        assert_eq!(
            snap.vns, 0,
            "vns must anchor to assigned_clock (0), not the live counter (777)"
        );
    }

    /// Reviewer fix (3): `restore_vtime` is atomic — a snapshot with an invalid
    /// entropy blob is rejected with the timeline **fully intact** (clock,
    /// `vns_base`, work, and entropy all unchanged), never half-restored.
    #[test]
    fn restore_vtime_rejects_bad_snapshot_atomically() {
        let mut v = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 1).unwrap());
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

    /// V-time restore is independent of the backend's vCPU-save path. The retired
    /// exit-count clock needed a backend round trip to re-arm its counter; the
    /// exit-count clock has no such dependency. This guard ensures it stays gone.
    #[test]
    fn restore_vtime_does_not_touch_backend_save() {
        let mut v = Vmm::new(
            SaveFailBackend(configured_mock(vec![])),
            GuestRam::new(0x1000).unwrap(),
        );
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 1).unwrap());
        // A valid, state-changing snapshot must restore even though this backend's
        // save method always fails.
        let snap0 = v.save_vtime().expect("clean save").expect("V-time wired");
        let snap = VtimeSnapshot {
            vns: snap0.vns + 4_096,
            guest_clock_offset: snap0.guest_clock_offset,
            entropy: snap0.entropy.clone(),
        };
        v.restore_vtime(&snap).expect("V-time-only restore");
        assert_eq!(v.effective_vns(), Some(snap.vns));
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
        let run_msr = || {
            let mut v = vtime_vmm(
                vec![
                    Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
                    Exit::Common(CommonExit::Idle),
                ],
                1,
            );
            v.run().unwrap();
            v
        };
        let mut insn = vtime_vmm(
            vec![Exit::Arch(X86Exit::Rdtsc), Exit::Common(CommonExit::Idle)],
            1,
        );
        insn.run().unwrap();

        let msr = run_msr();
        assert_eq!(
            msr.backend.completions(),
            insn.backend.completions(),
            "RDMSR(IA32_TSC) must read the same V-time TSC as the RDTSC instruction"
        );
        assert_eq!(msr.backend.completions(), &[Completion::Read(2)]);
        // Deterministic-twice (same seed/work ⇒ byte-identical state_hash).
        assert_eq!(msr.state_hash(), run_msr().state_hash());
    }

    /// Task-27 item 1, write side: `WRMSR(IA32_TSC_ADJUST, Y)` sets the adjust (and
    /// shifts the visible TSC by `Y`); `WRMSR(IA32_TSC, X)` sets the visible TSC to
    /// `X` (and the adjust to `X − base`). `RDMSR` of both reflects it. Both writes
    /// are honored (`Completion::Ok`).
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
        let at_vns = |vns: u64| {
            let mut v = vtime_vmm(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x3b })], 1);
            v.vtime.as_mut().unwrap().advance_virtual_time(vns);
            v.step().unwrap(); // RDMSR(IA32_TSC_ADJUST) records assigned_clock
            v
        };
        assert_ne!(
            at_vns(100).state_hash(),
            at_vns(200).state_hash(),
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
        let mut w = VtimeWiring::new_virtual_time(contract_vclock_config(), 1).expect("wiring");
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
            v.wire_vtime(VtimeWiring::new_virtual_time(cfg, seed).unwrap());
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
        // collide with `a`): `guest_hz`/`guest_base` are hashed directly, and
        // `vns_base` initializes the canonical effective-V-time field.
        let base = contract_vclock_config();
        let variants = [
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

        // Same seed + same cfg ⇒ same hash (deterministic; no false-different).
        let a2 = wired(1, contract_vclock_config());
        assert_eq!(a.state_hash(), a2.state_hash());
    }

    #[test]
    fn vtime_hash_preimage_keeps_the_frozen_n1_prefix() {
        let wiring =
            VtimeWiring::new_virtual_time(contract_vclock_config(), 1).expect("valid wiring");
        let encoded = encode_vtime(&wiring);
        assert_eq!(encoded[0], 1, "historical assigned-clock marker");
        assert_eq!(&encoded[1..9], &1_u64.to_le_bytes(), "historical ratio");
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
    fn report_port_advances_the_paravirtual_exit_budget() {
        let mut vmm = vtime_vmm(vec![report_out(0xA5A5_5A5A)], 1);
        let before = vmm.effective_vns().unwrap();
        assert_eq!(vmm.step().unwrap(), Step::Continued);
        assert_eq!(
            vmm.effective_vns().unwrap() - before,
            crate::vendor::x86::contract::PARAVIRTUAL_EXIT_VNS
        );
        assert_eq!(vmm.report_stream(), [0xA5A5_5A5A]);
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
        w.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 1).unwrap());
        let wlabels: Vec<&str> = w.state_components().iter().map(|(l, _)| *l).collect();
        for expect in ["vtim:cfg", "vtim:eff-vns", "vtim:entropy"] {
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
    /// live read of the virtual-time clock. The OLD `encode_vtime` did, at hash time — and
    /// that terminal read carries the non-deterministic post-last-intercept exit-path
    /// exit-boundary variability, which made the `VTIM` chunk diverge across two same-seed box runs (corpus
    /// O1, PR #51). A `TestAxis` that counts its reads proves hashing takes none.
    /// Task-27 item 2, test (i) — **deterministic-twice despite terminal exit-boundary variability**. Two
    /// same-seed runs read the same deterministic work at the RDTSC intercept, but a
    /// read taken *after* the run (what the OLD `encode_vtime` did at hash time) would
    /// advance by a per-run, non-deterministic exit-boundary variability. The fix anchors the `VTIM` hash
    /// to the recorded last-intercept work, so the chunk (hence `state_hash`) is
    /// byte-identical regardless of exit-boundary variability — the property the box O1 gate checks.
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

        // Fresh: vns_base=0, step one RDTSC reading work=E ⇒ assigned_clock=E,
        // effective V-time = snapshot_vns(E) = E.
        let mut fresh = Vmm::new(
            configured_mock(vec![Exit::Arch(X86Exit::Rdtsc)]),
            GuestRam::new(0x1000).unwrap(),
        );
        let mut cfg = contract_vclock_config();
        cfg.vns_base = E - 1;
        fresh.wire_vtime(VtimeWiring::new_virtual_time(cfg, SEED).unwrap());
        fresh.step().unwrap();

        // Restored: a fresh VM restored to a snapshot whose vns == E. restore_vtime
        // sets vns_base=E and assigned_clock=0, so effective V-time =
        // snapshot_vns(0) = vns_base = E. The entropy blob is a freshly-saved
        // same-seed stream, so the restored stream matches fresh's (no draws either).
        let mut restored = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        restored.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), SEED).unwrap());
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
    /// (RDRAND/RDSEED) is a V-time intercept and MUST advance `assigned_clock`.
    /// Two states with **different** pre-RNG-exit exit counts but an **identical**
    /// seeded draw must hash **DIFFERENTLY** — otherwise they collide in `VTIM` (a
    /// false determinism MATCH) and then diverge on the next TSC read. Without the fix
    /// both keep the stale anchor (`0` here, no prior TSC) and hash the same.
    #[test]
    fn rng_exit_advances_the_vtim_work_anchor() {
        let after_rng_at_vns = |vns: u64| {
            let mut v = vtime_vmm(
                vec![Exit::Arch(X86Exit::Rdrand { width: 8 })],
                0x7777, // same seed ⇒ identical draw in both
            );
            v.vtime.as_mut().unwrap().advance_virtual_time(vns);
            v.step().unwrap(); // RDRAND draws AND records assigned_clock = work
            v
        };
        assert_ne!(
            after_rng_at_vns(100).state_hash(),
            after_rng_at_vns(200).state_hash(),
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
                Exit::Arch(X86Exit::Rdtsc), // V-time intercept → assigned_clock = W
                Exit::Common(CommonExit::Mmio {
                    gpa: Gpa(0xFEE0_0390),
                    size: 4,
                    write: None,
                }), // read TMCCT at now=W
                Exit::Common(CommonExit::Idle),
            ]),
            GuestRam::new(0x1000).unwrap(),
        );
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 1).unwrap());
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
    /// so [`Vmm::lapic_now_vns`] reads the live virtual-time clock (the Phase B.1 path).
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
        // Deterministic backend (default mock caps): the timer clock is the deterministic
        // last-intercept anchor. Arm the timer at V-time 0; an ISR read BEFORE the
        // RDTSC sees no delivery (anchor still 0 — a live-work mutant would have fired
        // it), and an ISR read AFTER the RDTSC advances the anchor sees the vector in
        // service (fired, accepted, IRR→ISR completed).
        let mut exits = arm_timer_exits(1);
        exits.push(read_mmio(isr_gpa(0x40))); // A: anchor still 0 → not delivered
        exits.push(Exit::Arch(X86Exit::Rdtsc)); // V-time intercept → assigned_clock = W
        exits.push(read_mmio(isr_gpa(0x40))); // B: anchor = W → delivered
        exits.push(Exit::Common(CommonExit::Idle));
        let mut v = lapic_vmm(configured_mock(exits));

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

    /// P3 round-12: `restore_vtime`'s counter re-arms are all-or-NOTHING. A backend failure
    /// during the (sole fallible) save/restore round-trip must leave counter A NOT re-armed
    /// — `first_entry_done` is set `false` only in the INFALLIBLE commit AFTER the
    /// round-trip succeeds, so a failure cannot re-arm A while leaving B un-re-armed (the
    /// `B re-armed but A not` bug round-11 still had). Proof: `start_run` does NOT re-fire
    /// at the entry after a FAILED restore (A's first-entry gate was never reset).
    #[test]
    fn stale_vector_re_arbitrated_away_after_tpr_raise() {
        // [review P2] If the guest raises TPR above a peeked-but-not-yet-accepted
        // vector while it waits on the interrupt window, the VMM re-arbitrates (re-
        // peeks) every entry and overwrites the backend's pending slot — so the now-
        // stale vector is NOT injected, yet stays pending in the LAPIC IRR (not lost).
        let tpr_write = Exit::Common(CommonExit::Mmio {
            gpa: Gpa(APIC_MMIO_BASE + u64::from(lapic::APIC_TPR)),
            size: 4,
            write: Some(0xF0), // TPR class 0xF masks vector 0x40 (class 4)
        });
        let mut exits = arm_timer_exits(1);
        exits.push(read_mmio(isr_gpa(0x20)));
        exits.push(tpr_write); // guest raises TPR while 0x40 waits on the window
        exits.push(read_mmio(irr_gpa(0x40))); // 0x40 still pending in IRR
        exits.push(Exit::Common(CommonExit::Idle));
        let mut mock = configured_mock(exits);
        mock.set_defer_accept(true); // hold 0x40 un-accepted across the TPR raise
        let mut v = lapic_vmm(mock);

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
        let mut v = lapic_vmm(mock);

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
        let mut v = lapic_vmm(mock);
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
        let mut exits = arm_timer_exits(1);
        exits.push(UNMASK_IRQ4);
        exits.push(ENABLE_THRI);
        exits.push(Exit::Arch(X86Exit::Rdtsc)); // advance the anchor → the timer fires into IRR
        exits.push(Exit::Common(CommonExit::Idle));
        let mut mock = configured_mock(exits);
        mock.set_defer_accept(true);
        let mut v = lapic_vmm(mock);
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
        let mut v = lapic_vmm(configured_mock(exits));
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

    fn lapic_vmm(mock: MockBackend) -> Vmm<MockBackend> {
        let mut v = Vmm::new(mock, GuestRam::new(0x1000).unwrap());
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 1).unwrap());
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        v
    }

    /// A virtual_time-wired mock Vmm with the userspace xAPIC: the
    /// assigned-at-exit engine plus the production trace, for the x86
    /// schedule-oracle tests.
    fn virtual_time_lapic_vmm(mock: MockBackend) -> Vmm<MockBackend> {
        let mut v = Vmm::new(mock, GuestRam::new(0x1000).unwrap());
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 1).unwrap());
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
    fn virtual_time_lapic_timer_records_schedule_and_delivery() {
        // Arm the one-shot timer (vector 0x40, TMICT=1 ⇒ an 84 vns period at
        // the 24 MHz test clock, ÷2 reset divide), then cross the deadline
        // with one more xAPIC read (each xAPIC access is a 1000 vns
        // virtual_time exit). The fire must be recorded inside the crossing
        // event — the placement oracle requires each delivery at the first
        // event whose post-advance V-time covers the deadline.
        let mut exits = arm_timer_exits(1);
        exits.push(read_mmio(isr_gpa(0x40)));
        exits.push(Exit::Common(CommonExit::Shutdown));
        let mut v = virtual_time_lapic_vmm(configured_mock(exits));
        v.run().expect("run");

        let trace = v.virtual_time_trace().expect("virtual_time trace wired");
        let schedule = trace.schedule();
        assert_eq!(schedule.len(), 1, "the one-shot arm is one schedule record");
        assert_eq!(schedule[0].interrupt_id, 0x40);
        assert_eq!(schedule[0].canceled_at_event, None);
        let log = trace.normalized_log();
        crate::virtual_time::check_delivery_placement(schedule, log)
            .expect("the delivery sits at the first event whose V-time covers the deadline");
        let delivery_events: Vec<u64> = log
            .events
            .iter()
            .filter(|e| !e.interrupts.is_empty())
            .map(|e| e.event_index)
            .collect();
        assert_eq!(
            delivery_events,
            vec![3],
            "fired inside the crossing (ISR-read) event"
        );
    }

    #[test]
    fn virtual_time_lapic_timer_disarm_cancels_the_schedule() {
        // Arm far in the future, then write TMICT=0: the disarm must cancel
        // the schedule record, and the placement oracle must accept the log
        // with no delivery.
        let mut exits = arm_timer_exits(1_000_000);
        exits.push(Exit::Common(CommonExit::Mmio {
            gpa: Gpa(APIC_MMIO_BASE + u64::from(lapic::APIC_TMICT)),
            size: 4,
            write: Some(0),
        }));
        exits.push(Exit::Common(CommonExit::Shutdown));
        let mut v = virtual_time_lapic_vmm(configured_mock(exits));
        v.run().expect("run");

        let trace = v.virtual_time_trace().expect("virtual_time trace wired");
        let schedule = trace.schedule();
        assert_eq!(schedule.len(), 1);
        assert_eq!(
            schedule[0].canceled_at_event,
            Some(3),
            "the TMICT=0 event canceled it"
        );
        crate::virtual_time::check_delivery_placement(schedule, trace.normalized_log())
            .expect("a canceled deadline needs no delivery");
    }

    #[test]
    fn injected_vector_stays_in_irr_until_accepted() {
        // [blocking review #1] The LAPIC IRR→ISR transition must NOT happen until
        // the backend accepts the vector. With acceptance deferred (modelling the
        // interrupt-window wait), a guest APIC read sees vector 0x40 **pending in
        // IRR** and **not in service** — so a snapshot/hash in that window is correct.
        let mut exits = arm_timer_exits(1);
        exits.push(read_mmio(isr_gpa(0x20)));
        exits.push(read_mmio(irr_gpa(0x40))); // IRR bank for vec 0x40
        exits.push(read_mmio(isr_gpa(0x40))); // ISR bank for vec 0x40
        exits.push(Exit::Common(CommonExit::Idle));
        let mut mock = configured_mock(exits);
        mock.set_defer_accept(true); // never accept → vector stays pending
        let mut v = lapic_vmm(mock);

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
        let mut exits = arm_timer_exits(1);
        exits.push(read_mmio(isr_gpa(0x20)));
        exits.push(read_mmio(irr_gpa(0x40)));
        exits.push(read_mmio(isr_gpa(0x40)));
        exits.push(Exit::Common(CommonExit::Idle));
        // Default mock accepts at run (not deferred).
        let mut v = lapic_vmm(configured_mock(exits));

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
    fn idle_hlt_without_if_is_terminal() {
        // IF==0 (the kernel's final `cli; hlt`): terminal even with a timer armed
        // — a wait nothing will satisfy. The byte-identical existing behavior.
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Common(CommonExit::Idle));
        // Default mock state: rflags == 0 (IF clear).
        let mut v = lapic_vmm(configured_mock(exits));
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
        let mut v = lapic_vmm(mock);
        let r = v.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Idle);
        assert!(v.idle_landings().is_empty(), "no armed timer ⇒ terminal");
    }

    #[test]
    fn idle_hlt_on_stock_backend_is_terminal() {
        // A composition without virtual time never idle-resumes, even with IF==1
        // and a timer armed.
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Common(CommonExit::Idle));
        let mut mock = configured_stock_mock(exits);
        mock.set_state(if_set_state());
        let mut v = Vmm::new(mock, GuestRam::new(0x1000).unwrap());
        v.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .unwrap(),
        );
        let r = v.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Idle);
        assert!(
            v.idle_landings().is_empty(),
            "a non-deterministic backend never idle-resumes"
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
            let mut v = lapic_vmm(mock);
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
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 1).unwrap());
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

    // -----------------------------------------------------------------------
    // Full vm_state snapshot / restore / branch (task 39). Mock-driven; the
    // live box gate is tests/live_snapshot_branch.rs.
    // -----------------------------------------------------------------------

    /// A `Vmm<MockBackend>` with V-time + the Linux platform (xAPIC + legacy I/O)
    /// all wired — the full surface `save_vm_state` captures.
    fn full_vmm(
        state: VcpuState,
        exits: Vec<Exit<X86>>,
        _retired_initial_work: u64,
        seed: u64,
    ) -> Vmm<MockBackend> {
        let mut m = configured_mock(exits);
        m.set_state(state);
        let mut v = Vmm::new(m, GuestRam::new(0x2000).unwrap());
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), seed).unwrap());
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
        assert_eq!(s.vtime.snapshot_vns, 5002);
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

        // The RDTSC exit advances one V-ns beyond the restored snapshot before
        // reading the visible clock; IA32_TSC_ADJUST is restored too.
        assert_eq!(
            b.backend.completions()[0],
            Completion::Read(2 * (s.vtime.snapshot_vns + 1) + 0x1234)
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
        let mut a = vtime_vmm(vec![Exit::Arch(X86Exit::Rdtsc)], 1);
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
        let v = Vmm::new(
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
    fn retiring_a_completion_clears_displaced_vmm_latches_before_restore() {
        let mut src = full_vmm(VcpuState::default(), mutate_exits(), 500, 1);
        step_n(&mut src, 6);
        let snap = src.save_vm_state().unwrap();

        let mut tgt = full_vmm(
            VcpuState::default(),
            vec![Exit::Arch(X86Exit::Rdtsc)],
            10,
            1,
        );
        tgt.step().unwrap();
        tgt.sdk_snapshot_reentry_required = true;
        assert!(tgt.completion_staged);

        tgt.retire_pending_completion().unwrap();
        assert!(!tgt.completion_staged);
        assert!(!tgt.rng_completion_staged);
        assert!(!tgt.sdk_snapshot_reentry_required);
        tgt.restore_vm_state(&snap).unwrap();
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
        let v = full_vmm(flags, vec![], 0, 1);
        assert!(matches!(
            v.save_vm_state(),
            Err(VmmError::ContractViolation(_))
        ));

        let mut pdptr = nonzero_state();
        pdptr.sregs.pdptrs[2] = 0xDEAD_BEEF;
        let v2 = full_vmm(pdptr, vec![], 0, 1);
        assert!(matches!(
            v2.save_vm_state(),
            Err(VmmError::ContractViolation(_))
        ));

        // `kvm_debugregs.flags` (not carried) — DR0..3/DR6/DR7 ARE carried.
        let mut dbg = nonzero_state();
        dbg.debugregs.flags = 1;
        let v3 = full_vmm(dbg, vec![], 0, 1);
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
            let a = full_vmm(st, vec![], 0, 1);
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
            let v = full_vmm(st, vec![], 0, 1);
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
        let v_ok = full_vmm(nonzero_state(), vec![], 0, 1);
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
        let a = full_vmm(nonzero_state(), vec![], 0, 1);
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
            let a = full_vmm(nonzero_state(), vec![], 0, 1);
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
        // Quiescent: a wired LAPIC with no timer programmed → nothing pending in the IRR.
        let mut q = lapic_vmm(configured_mock(vec![
            Exit::Arch(X86Exit::Rdtsc),
            Exit::Common(CommonExit::Idle),
        ]));
        q.step().unwrap();
        assert!(
            !q.has_pending_guest_interrupt().unwrap(),
            "a quiescent LAPIC has no pending guest interrupt"
        );
        // In flight: arm the timer, let it fire into the IRR, hold it un-accepted
        // (defer_accept) — exactly a snapshottable in-flight point. `peek_interrupt`
        // re-derives 0x40 without moving IRR→ISR.
        let mut exits = arm_timer_exits(1);
        exits.push(read_mmio(isr_gpa(0x20)));
        exits.push(Exit::Arch(X86Exit::Rdtsc));
        let mut mock = configured_mock(exits);
        mock.set_defer_accept(true);
        let mut a = lapic_vmm(mock);
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
        let mut exits = arm_timer_exits(1);
        exits.push(read_mmio(isr_gpa(0x20)));
        exits.push(Exit::Arch(X86Exit::Rdtsc));
        let mut mock = configured_mock(exits);
        mock.set_defer_accept(true); // hold 0x40 un-accepted → it stays pending in IRR
        let mut a = lapic_vmm(mock);
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
        let mut b = lapic_vmm(bmock);
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
    // Task 110: the paravirt exit-count-derived clock page (docs/PARAVIRT-CLOCK.md).
    // Portable halves of the G1/G2 gates + the registration transport,
    // driven by the scripted MockBackend — no /dev/kvm, runs on every platform.
    // -----------------------------------------------------------------------

    use vtime::pvclock::PVCLOCK_PAGE_LEN;

    /// A pvclock-offered `Vmm<MockBackend>` with the determinism path wired and
    /// RAM covering the doorbell frame pages.
    fn pvclock_vmm(exits: Vec<Exit<X86>>, seed: u64) -> Vmm<MockBackend> {
        let mut vmm = Vmm::new(configured_mock(exits), GuestRam::new(TEST_RAM).unwrap());
        vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), seed).unwrap());
        vmm.enable_pvclock();
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

    /// Review r14 (the last of the r11 GPA family): the pvclock GPA helpers
    /// (`pvclock_validate_gpa` / `pvclock_page` / `pvclock_stamp`) must resolve
    /// absolute GPAs through the region resolver. On a high-RAM-base (arm64)
    /// machine the pvclock region is reserved at `RAM_BASE + off` (the `hm-rk5`
    /// seam), so a valid absolute GPA validates and its page resolves; a GPA below
    /// the base is not backed. x86 (base 0) is byte-identical.
    #[test]
    fn pvclock_gpa_helpers_resolve_a_high_ram_base() {
        let mut vmm = pvclock_vmm(vec![], 7);
        vmm.ram_base_gpa = 0x4000_0000; // arm64: RAM is high

        // A valid HIGH absolute GPA (page-aligned, inside `[RAM_BASE, +RAM)`)
        // validates — the pre-r14 raw bound rejected it as "past the end of RAM".
        let high = 0x4000_0000 + PV_GPA;
        assert!(
            vmm.pvclock_validate_gpa(high).is_ok(),
            "a high arm64 pvclock GPA must validate: {:?}",
            vmm.pvclock_validate_gpa(high)
        );
        // A GPA below the RAM base is not backed.
        assert_eq!(
            vmm.pvclock_validate_gpa(0x1000),
            Err("below the guest RAM base")
        );

        // Register the high GPA (validate + record); `pvclock_page` then resolves
        // it at the high absolute GPA, not a wrong host offset.
        assert_eq!(vmm.pvclock_register(high).0 as u16, Status::Ok as u16);
        assert_eq!(vmm.pvclock_registration(), Some(high));
        assert!(
            vmm.pvclock_page().is_some(),
            "pvclock_page must resolve the high GPA"
        );

        // x86 (base 0): a valid low GPA validates unchanged (the full x86 pvclock
        // suite is the rest of the neutrality proof).
        let x86 = pvclock_vmm(vec![], 7);
        assert!(x86.pvclock_validate_gpa(PV_GPA).is_ok());
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
            let mut vmm = pvclock_vmm(vec![], 7);
            let (status, payload) = ring_pvclock_register(&mut vmm, bad);
            assert_eq!(status, Status::OutOfRange as u16, "gpa {bad:#x}");
            assert!(payload.is_empty());
            assert_eq!(vmm.pvclock_registration(), None, "gpa {bad:#x} recorded");
        }
    }

    /// A page-aligned GPA inside the RAM *image* but inside a **device-MMIO
    /// hole** must be refused (cross-model r5 P2). `KvmBackend::map_memory`
    /// splits its memslots around the xAPIC page, so RAM there is not mapped:
    /// the host would happily stamp its backing bytes while the guest's own loads
    /// went to the LAPIC device model. Registration would report `Ok` and the
    /// guest would read a clock that never ticks — the worst failure shape there
    /// is (silently wrong, not loud).
    ///
    /// Needs a RAM image that actually extends past `0xFEE00000`; the mapping is
    /// lazy (untouched pages never commit), but it is far too big for Miri.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "4 GiB guest RAM image — lazy mmap, but not for the interpreter"
    )]
    fn pvclock_registration_rejects_the_lapic_mmio_hole() {
        const LAPIC_HOLE: u64 = 0xFEE0_0000;
        // Just past the hole, so both a normal page and the hole are in range.
        let ram_len = (LAPIC_HOLE + 0x2000) as usize;
        let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(ram_len).unwrap());
        vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 7).unwrap());
        vmm.enable_pvclock();
        // The hole itself: inside the image, page-aligned — and refused.
        let (status, _) = ring_pvclock_register(&mut vmm, LAPIC_HOLE);
        assert_eq!(
            status,
            Status::OutOfRange as u16,
            "the xAPIC MMIO page was accepted as a clock page — the host would stamp backing \
             the guest cannot read, and the guest would read the LAPIC instead of its clock"
        );
        assert_eq!(vmm.pvclock_registration(), None);
        // ...while a normal page just past it still registers, so the check is a
        // hole test and not a blanket ban on high memory.
        let (status, _) = ring_pvclock_register(&mut vmm, LAPIC_HOLE + 0x1000);
        assert_eq!(status, Status::Ok as u16);
        assert_eq!(vmm.pvclock_registration(), Some(LAPIC_HOLE + 0x1000));
    }

    /// The doorbell answers `UnknownService` for an unavailable pvclock service
    /// **before** it classifies the payload or the opcode (cross-model r5 P2). A
    /// composition that keeps the doorbell alive for some other channel must not
    /// leak the service's existence by grading its requests — a malformed payload
    /// must not come back `BadRequest`, and a bad opcode must not come back
    /// `UnknownOpcode`, when the service is not there at all.
    #[test]
    fn pvclock_unavailable_answers_unknown_service_before_classifying() {
        // Offered without virtual time ⇒ unavailable.
        let mut m = MockBackend::new();
        m.set_policy(&X86Policy {
            cpuid: CpuidModel::default(),
            msr_filter: MsrFilter::default(),
        })
        .unwrap();
        let mut vmm = Vmm::new(m, GuestRam::new(TEST_RAM).unwrap());
        vmm.enable_pvclock();
        assert!(!vmm.pvclock_available());

        let ring_raw = |vmm: &mut Vmm<MockBackend>, opcode: u32, payload: &[u8]| -> u16 {
            let mut frame = [0_u8; 64];
            let len =
                hypercall_proto::encode_request(ServiceId::Pvclock, 1, opcode, payload, &mut frame)
                    .expect("encode");
            vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + len].copy_from_slice(&frame[..len]);
            vmm.service_doorbell(len as u32).expect("doorbell");
            let resp = &vmm.ram.as_bytes()[RESP_GPA..RESP_GPA + HC_PAGE];
            decode(resp).expect("well-formed response frame").0.status
        };
        // A well-formed register, a MALFORMED payload, and a bad opcode: all three
        // must be UnknownService, not Ok / BadRequest / UnknownOpcode.
        assert_eq!(
            ring_raw(&mut vmm, 1, &PV_GPA.to_le_bytes()),
            Status::UnknownService as u16
        );
        assert_eq!(
            ring_raw(&mut vmm, 1, &[0u8; 3]),
            Status::UnknownService as u16,
            "a malformed payload was graded BadRequest on a service that is not offered"
        );
        assert_eq!(
            ring_raw(&mut vmm, 9, &[]),
            Status::UnknownService as u16,
            "a bad opcode was graded UnknownOpcode on a service that is not offered"
        );
        assert_eq!(vmm.pvclock_registration(), None);
    }

    /// The pure-opt-in gate, host side: an offered-but-vtime-unwired VM and a
    /// backend without a deterministic virtual-time clock both answer
    /// `UnknownService` — the probing guest keeps its trap-backstopped paths.
    /// The pure-opt-in gate, guest side (the "page off = byte-identical" half
    /// of "Done means"): a VM that OFFERS the page but whose guest never
    /// registers is **guest-observably identical** to an un-offered VM over
    /// the same script — identical RAM, serial, and observable digest; no
    /// stamp is ever written. The `state_blob`s differ by EXACTLY the `PVCK`
    /// channel-configuration chunk (cross-model r1 P1: the offer governs future
    /// execution, so it is state identity — the SDK fault-policy
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
            vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 7).unwrap());
            if offer {
                vmm.enable_pvclock();
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
    /// blob; a registration changes it because the future stamping target changes.
    #[test]
    fn pvclock_channel_configuration_reaches_state_identity() {
        let build = || {
            let mut vmm = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
            vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 7).unwrap());
            vmm.enable_pvclock();
            vmm
        };
        let base = build().state_blob();
        assert_eq!(
            base,
            build().state_blob(),
            "same configuration must hash identically"
        );
        let mut registered = build();
        let (status, _) = ring_pvclock_register(&mut registered, PV_GPA);
        assert_eq!(status, Status::Ok as u16);
        let pending = registered.state_blob();
        assert_ne!(
            base, pending,
            "a registration is a different future — must reach the hash"
        );
        // The HANDSHAKE state is in the hash too (cross-model r11 P2): a PENDING
        // registration (just recorded at the OUT) and an ARMED one (handshake
        // done) have different futures — the pending one still owes its first
        // stamp — so they must hash differently even at the same GPA.
        registered.pvclock.as_mut().unwrap().armed = true;
        assert_ne!(
            pending,
            registered.state_blob(),
            "pending vs armed is a different future — the handshake bit must reach the hash"
        );
    }

    /// A crafted v4 device blob with the impossible tuple `(Some(gpa),
    /// registrable=false)` is rejected at decode (cross-model r6 P1) — a
    /// registered page can only exist on a VM that could register, so this cannot
    /// come from a valid seal. Accepting it would commit an active registration
    /// onto a target the capability check would refuse (the next refresh errors
    /// with no V-time; the page freezes with no deterministic backend).
    #[test]
    fn pvclock_decode_rejects_registered_but_non_registrable() {
        use crate::vendor::x86::records::{DeviceState, encode_device_blob};
        // A well-formed source: registered AND registrable.
        let good = DeviceState {
            pvclock: Some((Some(PV_GPA), true)),
            ..DeviceState::default()
        };
        let mut blob = encode_device_blob(&good).0;
        // The registrable flag is the LAST byte of the blob (trailing pvclock
        // record). Flip it to the impossible `false` and re-decode.
        assert_eq!(*blob.last().unwrap(), 1, "the registrable byte is the tail");
        *blob.last_mut().unwrap() = 0;
        assert!(
            crate::vendor::x86::records::decode_device_blob(&blob).is_err(),
            "a registered-but-non-registrable v4 record must be rejected at the wire"
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
            7,
        );
        ring_pvclock_register(&mut vmm, PV_GPA);
        vmm.step().unwrap(); // RDTSC: anchor 10, page stamped
        let stamped = vtime::pvclock::read(vmm.pvclock_page().unwrap()).unwrap();
        // The guest scribbles its own page (deterministic guest behavior).
        let off = PV_GPA as usize + vtime::pvclock::VNS_OFF;
        vmm.ram.as_mut_bytes()[off] ^= 0xA5;
        assert!(vmm.pvclock_check_oracle().is_err(), "scribble visible");
        // The next exit is a plain UART write. It advances by the contract's
        // serial-exit duration and its tail refresh repairs the page.
        vmm.step().unwrap();
        let repaired = vtime::pvclock::read(vmm.pvclock_page().unwrap()).unwrap();
        assert!(repaired.vns > stamped.vns);
        vmm.pvclock_check_oracle()
            .expect("the natural-exit refresh restored oracle equality");
    }

    /// NO seal — accepted or rejected — touches the live page (cross-model r4
    /// P1, superseding the r1 reject-before-mutation ordering with a stronger
    /// property: there is no mutation to order). Canonicalizing a live page
    /// would reset its seqlock epoch to a value the page has held before, and a
    /// guest reader straddling the seal would then accept the values it loaded
    /// before the last refresh — a snapshot would change the guest's future.
    #[test]
    fn pvclock_seal_never_touches_the_live_page() {
        // A handshake (RDTSC) canonical-stamps at seq 0; then a distinct-value
        // refresh (TSC_ADJUST) moves the epoch off 0 — the non-canonical live
        // page this test seals.
        let mut vmm = pvclock_vmm(
            vec![
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Arch(X86Exit::Wrmsr {
                    index: IA32_TSC_ADJUST,
                    value: 5,
                }),
            ],
            7,
        );
        ring_pvclock_register(&mut vmm, PV_GPA);
        vmm.step().unwrap(); // RDTSC: the handshake (canonical, seq 0)
        vmm.step().unwrap(); // TSC_ADJUST: distinct value, seq moves off 0
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
        // And a SUCCESSFUL seal at the same point leaves the page equally
        // untouched — the epoch keeps its mid-run value (r4: sealed verbatim).
        bad.sregs.flags = 0;
        vmm.backend.restore(&bad).unwrap();
        vmm.save_vm_state().unwrap();
        assert_eq!(
            vmm.pvclock_page().unwrap(),
            page_before.as_slice(),
            "a successful seal rewrote the live page — the ABA the r4 P1 rules out"
        );
        assert!(
            vmm.host_dirty.is_empty(),
            "a seal marked host-dirty state — it wrote to guest RAM"
        );
    }

    /// G1's portable analogue: two same-seed, same-script runs with the page
    /// registered produce bit-identical `state_blob`s (page bytes included) —
    /// the stamping machinery leaks no run-local entropy into guest RAM.
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
            7,
        );
        let (status, _) = ring_pvclock_register(&mut vmm, PV_GPA);
        assert_eq!(status, Status::Ok as u16);

        // Step 1: RDTSC advances one V-ns -> trap value 2; page must match it.
        assert_eq!(vmm.step().unwrap(), Step::Continued);
        let f = vtime::pvclock::read(vmm.pvclock_page().unwrap()).unwrap();
        assert_eq!((f.vns, f.guest_clock), (1, 2));
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
        assert_eq!(f.guest_clock, 9, "guest_clock = ticks(2) + adjust 5");
        vmm.pvclock_check_oracle().unwrap();

        // Step 3: the next RDTSC advances again and the page follows it.
        let seq_before = f.seq;
        assert_eq!(vmm.step().unwrap(), Step::Continued);
        let f = vtime::pvclock::read(vmm.pvclock_page().unwrap()).unwrap();
        assert_eq!(f.guest_clock, 11);
        assert_ne!(f.seq, seq_before);

        // The refresh log records ONE distinct-value publish: step 2's TSC_ADJUST
        // (clock 25). Step 1's RDTSC is the r8 HANDSHAKE — a canonical first stamp
        // (vns 10, clock 20), which is not a refresh-log entry — so the first
        // logged publish is step 2, and step 3's RDTSC is a value-keyed no-op.
        assert_eq!(vmm.pvclock_refreshes(), &[(2, 9), (3, 11)]);
        vmm.pvclock_clear_refreshes();
        assert!(vmm.pvclock_refreshes().is_empty());
    }

    /// G2's evidence-integrity bar (the deliberate-fault test the task spec
    /// mandates): a page that diverges from the oracle — here corrupted in
    /// guest RAM after a good stamp, simulating a stamping bug — must FAIL the
    /// oracle check loudly, proving the gate cannot pass vacuously.
    #[test]
    fn pvclock_oracle_check_fails_on_a_corrupted_page() {
        let mut vmm = pvclock_vmm(vec![Exit::Arch(X86Exit::Rdtsc)], 7);
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
        vmm.vtime.as_mut().unwrap().advance_virtual_time(999);
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
    /// registration the deadline is `None` (page-off arms exactly as before),
    /// and the FIRST arm waits for a deterministic clock advance (r2 P1).
    /// The r2 GPA ruling: registration is **one-shot**. A second register —
    /// same GPA or a different valid one — is a guest fault (`BadRequest`)
    /// that touches nothing: the stamping target never moves, the original
    /// page keeps tracking the oracle, and the first-arm state is undisturbed.
    /// The r2 P1 fix (restore side) + the r3 direct-carry P1 in one: a plain
    /// `restore_snapshot` — no control server, no side channel — reinstates
    /// the sealed registration from the vm_state device blob (v4), and the
    /// restored registration arms the Δ refresh immediately (the restored
    /// anchor is exactly 0 against a re-baselined counter, so `0 + Δ` is
    /// strictly ahead; no stale-anchor window to wait out).
    /// P1 (cross-model r12, corrected r13). A **V-time-only** `restore_vtime` on
    /// a VM with an ARMED registration re-stamps the page to the restored
    /// timeline BEFORE returning — unlike a full `restore_vm_state`, it never
    /// touches the RAM page, so without the re-stamp the page would keep its
    /// PRE-restore value, which can sit AHEAD of the restored effective V-time
    /// (the guest's next read then step-tail refresh would see the clock jump
    /// BACKWARD). Crucially the re-stamp uses the epoch-advancing REFRESH
    /// protocol, NOT canonical `seq = 0`: the page is LIVE, so a reader
    /// straddling the restore must see the epoch CHANGE and retry — resetting to
    /// a `seq` it may already hold would be the same ABA the seal ruling forbids.
    #[test]
    fn restore_vtime_restamps_the_armed_page_to_the_restored_timeline() {
        const SEED: u64 = 7;
        // Advance an armed VM to a LARGE clock value (the handshake lays the
        // canonical seq-0 stamp at the RDTSC anchor `work`).
        let mut a = pvclock_vmm(vec![Exit::Arch(X86Exit::Rdtsc)], SEED);
        a.vtime.as_mut().unwrap().advance_virtual_time(999);
        ring_pvclock_register(&mut a, PV_GPA);
        a.step().unwrap(); // handshake: arm + canonical stamp at 1000
        let ahead = vtime::pvclock::read(a.pvclock_page().unwrap()).unwrap();
        assert_eq!(
            (ahead.seq, ahead.vns),
            (0, 1000),
            "armed at the large anchor"
        );

        // A rewind snapshot: effective V-time 42, far BEHIND the page's 1000.
        let snap = VtimeSnapshot {
            vns: 42,
            guest_clock_offset: 0,
            entropy: SeededEntropy::new(SEED).save_state(),
        };
        a.restore_vtime(&snap).unwrap();

        // The page now reflects the RESTORED timeline immediately (vns 42), so a
        // guest reading it post-restore never sees the stale 1000 — no backward
        // jump when the next step tail refreshes.
        let after = vtime::pvclock::read(a.pvclock_page().unwrap()).unwrap();
        assert_eq!(
            after.vns, 42,
            "restore_vtime must re-stamp the armed page to the restored anchor"
        );
        assert!(
            after.vns < ahead.vns,
            "the page moved BACK to the restored time"
        );
        // ABA-safety: the epoch ADVANCED off the pre-restore value (a LIVE
        // re-stamp is a refresh, never a canonical seq-0 reset a straddling reader
        // could mistake for its own sampled epoch).
        assert_ne!(
            after.seq, ahead.seq,
            "the seqlock epoch must advance across a live-page restore (ABA-safety)"
        );
        assert_ne!(
            after.seq, 0,
            "a LIVE re-stamp must use the epoch-advancing refresh, not canonical seq=0"
        );
        a.pvclock_check_oracle()
            .expect("re-stamped page matches the restored-clock oracle");
    }

    /// The re-stamp above is gated on `armed`: a **pending** registration (GPA
    /// recorded at the doorbell OUT but the handshake not yet done) keeps its
    /// pre-registration bytes across a `restore_vtime` — the first stamp still
    /// belongs to the handshake intercept, never to a restore.
    #[test]
    fn restore_vtime_leaves_a_pending_registration_unstamped() {
        let mut v = pvclock_vmm(vec![], 7);
        ring_pvclock_register(&mut v, PV_GPA); // pending: OUT recorded, no handshake
        assert!(
            vtime::pvclock::read(v.pvclock_page().unwrap()).is_none(),
            "pending registration is un-stamped before restore"
        );
        let snap = VtimeSnapshot {
            vns: 42,
            guest_clock_offset: 0,
            entropy: SeededEntropy::new(7).save_state(),
        };
        v.restore_vtime(&snap).unwrap();
        assert!(
            vtime::pvclock::read(v.pvclock_page().unwrap()).is_none(),
            "restore_vtime must not stamp a pending (un-armed) registration"
        );
    }

    /// r13 P1: a PENDING pvclock registration is UNSEALABLE. `restore_vtime` can
    /// leave a registration pending yet mark V-time synchronized (a would-be
    /// sealable boundary), but the v4 device record carries only the GPA, not the
    /// pending-vs-armed bit — so `save_vm_state` fails closed rather than seal a
    /// state that would restore as ARMED and skip the canonical handshake stamp
    /// the source still owes. Once the handshake arms the page, the same VM seals.
    #[test]
    fn save_vm_state_rejects_a_pending_pvclock_registration() {
        let mut v = pvclock_vmm(vec![Exit::Arch(X86Exit::Rdtsc)], 7);
        ring_pvclock_register(&mut v, PV_GPA); // pending: OUT recorded, no handshake
        // Reach a synchronized boundary while STILL pending — the restore_vtime
        // path the reviewer identified (the doorbell OUT alone never synchronizes).
        let snap = VtimeSnapshot {
            vns: 42,
            guest_clock_offset: 0,
            entropy: SeededEntropy::new(7).save_state(),
        };
        v.restore_vtime(&snap).unwrap();
        assert!(
            v.pvclock_registration().is_some(),
            "the registration is present but pending"
        );
        assert!(
            matches!(v.save_vm_state(), Err(VmmError::ContractViolation(_))),
            "a pending (un-armed) pvclock registration must fail closed at the seal"
        );
        // The handshake (the queued RDTSC) arms the page; the seal then succeeds.
        v.step().unwrap();
        v.save_vm_state()
            .expect("an armed registration seals cleanly");
    }

    /// §1.1 as amended at r4: a seal captures the page **verbatim**, so the
    /// image is a faithful copy of live guest RAM (what the snapshot engine's
    /// derive path assumes, `control.rs`'s
    /// `seal_derives_from_tracked_parent_and_reproduces_the_image`) and a
    /// restored sibling inherits the parent's epoch — the two stay in lockstep
    /// instead of diverging by a canonicalized-away `seq`. The sealed
    /// registration riding the vm_state device blob (v4) authoritatively
    /// replaces whatever registration the target VM held (r3: the direct restore
    /// path carries the channel with the state).
    #[test]
    fn pvclock_seal_is_verbatim_and_restore_carries_the_registration() {
        // The handshake (RDTSC) lays down a canonical seq-0 stamp; then a
        // distinct-value refresh (a TSC_ADJUST write, which changes the guest-
        // visible clock at the same anchor) advances the epoch off 0. Only then
        // is the epoch non-canonical, which is the state this test seals.
        let mut a = pvclock_vmm(
            vec![
                Exit::Arch(X86Exit::Rdtsc),
                Exit::Arch(X86Exit::Wrmsr {
                    index: IA32_TSC_ADJUST,
                    value: 5,
                }),
            ],
            7,
        );
        ring_pvclock_register(&mut a, PV_GPA);
        a.step().unwrap(); // RDTSC: the handshake — canonical stamp, seq 0
        a.step().unwrap(); // TSC_ADJUST: the clock changes, so the epoch moves
        let live = vtime::pvclock::read(a.pvclock_page().unwrap()).unwrap();
        assert_ne!(live.seq, 0, "a mid-run refresh bumped the epoch");
        let page_before_seal = a.pvclock_page().unwrap().to_vec();
        // Seal: guest RAM is untouched, so the image IS the live machine.
        let vm_state = a.save_vm_state().unwrap();
        let image = a.guest_memory().to_vec();
        assert_eq!(
            a.pvclock_page().unwrap(),
            page_before_seal.as_slice(),
            "the seal rewrote the live page"
        );
        assert_eq!(
            &image[PV_GPA as usize..PV_GPA as usize + PVCLOCK_PAGE_LEN],
            page_before_seal.as_slice(),
            "the sealed image must reproduce live guest memory, page included"
        );
        let sealed = vtime::pvclock::read(&image[PV_GPA as usize..]).unwrap();
        assert_eq!(
            (sealed.seq, sealed.vns, sealed.guest_clock),
            (live.seq, live.vns, live.guest_clock),
            "the sealed page carries the live epoch and values verbatim"
        );

        // Restore into a fresh, like-composed VM whose guest even registered a
        // DIFFERENT page first: the blob's sealed registration is
        // authoritative — the stale-timeline registration is replaced, not
        // merely cleared (the arrival-deadline stale-arm class).
        let mut b = pvclock_vmm(vec![Exit::Arch(X86Exit::Rdtsc)], 7);
        ring_pvclock_register(&mut b, PV_GPA + 0x1000);
        b.restore_snapshot(&image, &vm_state).unwrap();
        assert_eq!(
            b.pvclock_registration(),
            Some(PV_GPA),
            "the blob's sealed registration is authoritative after a direct restore"
        );
        // The restored page is byte-identical to the sealed one, and the next
        // intercept stamps it exactly as a never-restored run would.
        assert_eq!(
            &b.guest_memory()[PV_GPA as usize..PV_GPA as usize + PVCLOCK_PAGE_LEN],
            &image[PV_GPA as usize..PV_GPA as usize + PVCLOCK_PAGE_LEN],
        );
        b.pvclock_check_oracle().unwrap();
    }

    /// Composition mismatches fail loud, **symmetrically**, through the real
    /// restore path (`restore_vm_state` validates the blob's v4 pvclock
    /// record before mutating anything): an offered snapshot into an
    /// unoffered target (registered or not), an unoffered snapshot into an
    /// offered target, a Δ mismatch, a GPA that no longer validates, and a
    /// registration onto a non-deterministic-clock backend. Every rejection
    /// leaves the target VM intact (reject-before-mutation).
    #[test]
    fn pvclock_restore_mismatch_fails_loud() {
        // Sealed states from differently-configured source VMs.
        let seal = |register: bool| {
            let mut src = pvclock_vmm(vec![Exit::Arch(X86Exit::Rdtsc)], 7);
            if register {
                ring_pvclock_register(&mut src, PV_GPA);
            }
            src.step().unwrap();
            src.save_vm_state().unwrap()
        };
        let registered_state = seal(true);
        let offered_unregistered_state = seal(false);
        let unoffered_state = {
            let mut src = vtime_vmm(vec![Exit::Arch(X86Exit::Rdtsc)], 7);
            // vtime_vmm uses a small RAM; rebuild with TEST_RAM for image parity.
            let _ = &mut src;
            let mut src = Vmm::new(
                configured_mock(vec![Exit::Arch(X86Exit::Rdtsc)]),
                GuestRam::new(TEST_RAM).unwrap(),
            );
            src.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 7).unwrap());
            src.step().unwrap();
            src.save_vm_state().unwrap()
        };
        let reject = |vmm: &mut Vmm<MockBackend>, s: &vm_state::VmState, why: &str| {
            assert!(
                matches!(vmm.restore_vm_state(s), Err(VmmError::ContractViolation(_))),
                "expected loud rejection: {why}"
            );
        };

        // Offered snapshot (even UNREGISTERED) -> unoffered target: rejected.
        let mut unoffered = Vmm::new(configured_mock(vec![]), GuestRam::new(TEST_RAM).unwrap());
        unoffered.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 7).unwrap());
        reject(&mut unoffered, &registered_state, "registered -> unoffered");
        reject(
            &mut unoffered,
            &offered_unregistered_state,
            "offered-unregistered -> unoffered",
        );
        // Unoffered snapshot -> unoffered target: fine.
        unoffered.restore_vm_state(&unoffered_state).unwrap();

        // Unoffered snapshot -> OFFERED target: rejected (a guest registering
        // here would fork the timeline off the sealed one).
        let mut offered = pvclock_vmm(vec![], 7);
        reject(&mut offered, &unoffered_state, "unoffered -> offered");

        // A GPA that no longer validates on the target (smaller RAM): rejected.
        let mut small = Vmm::new(configured_mock(vec![]), GuestRam::new(0x2000).unwrap());
        small.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 7).unwrap());
        small.enable_pvclock();
        reject(&mut small, &registered_state, "GPA past the target's RAM");
        assert_eq!(small.pvclock_registration(), None, "rejection mutated");
    }

    /// An unknown pvclock opcode answers `UnknownOpcode`; a malformed payload
    /// answers `BadRequest` — never a silent drop, never a registration.
    #[test]
    fn pvclock_doorbell_rejects_bad_frames() {
        let mut vmm = pvclock_vmm(vec![], 7);
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

    /// r3 P1: per-service doorbell gating. A pvclock-ONLY composition
    /// services the doorbell (the pvclock offer opens it) but every OTHER
    /// service answers `UnknownService`: an assert-violation Event must not
    /// answer Ok — and must NOT surface `Step::SdkStop` into a session with
    /// no SDK channel (the PR-68 lesson); a buggify ask must not fabricate a
    /// nominal answer; an entropy_fill must not advance the one shared seeded
    /// stream of a run that never offered the service.
    #[test]
    fn pvclock_only_composition_rejects_unoffered_doorbell_services() {
        let mut vmm = pvclock_vmm(vec![], 7);
        let entropy_before = vmm.entropy_state();

        let ring = |vmm: &mut Vmm<MockBackend>, frame: &[u8]| -> (Step, u16) {
            vmm.ram.as_mut_bytes()[REQ_GPA..REQ_GPA + frame.len()].copy_from_slice(frame);
            let step = vmm.service_doorbell(frame.len() as u32).unwrap();
            let (header, _) = decode(&vmm.ram.as_bytes()[RESP_GPA..RESP_GPA + HC_PAGE]).unwrap();
            (step, header.status)
        };

        // An Event frame shaped like an ASSERT VIOLATION (the SdkStop shape).
        let mut frame = [0_u8; 128];
        let mut payload = Vec::new();
        payload.extend_from_slice(&0x0001_0001u32.to_le_bytes()); // an event id
        payload.extend_from_slice(b"assert detail bytes");
        let n =
            hypercall_proto::encode_request(ServiceId::Event, 1, 1, &payload, &mut frame).unwrap();
        let (step, status) = ring(&mut vmm, &frame[..n]);
        assert_eq!(
            status,
            Status::UnknownService as u16,
            "Event must be unoffered"
        );
        assert_eq!(step, Step::Continued, "no SdkStop without an SDK channel");

        // A buggify ask.
        let n =
            hypercall_proto::encode_request(ServiceId::Sdk, 1, 2, &7u32.to_le_bytes(), &mut frame)
                .unwrap();
        let (step, status) = ring(&mut vmm, &frame[..n]);
        assert_eq!(
            status,
            Status::UnknownService as u16,
            "Sdk must be unoffered"
        );
        assert_eq!(step, Step::Continued);

        // An entropy_fill for 8 bytes.
        let n = hypercall_proto::encode_request(
            ServiceId::Entropy,
            1,
            3,
            &8u32.to_le_bytes(),
            &mut frame,
        )
        .unwrap();
        let (step, status) = ring(&mut vmm, &frame[..n]);
        assert_eq!(
            status,
            Status::UnknownService as u16,
            "Entropy must be unoffered"
        );
        assert_eq!(step, Step::Continued);
        assert_eq!(
            vmm.entropy_state(),
            entropy_before,
            "an unoffered entropy ask advanced the shared seeded stream"
        );

        // AVAILABILITY BEFORE OPCODE (cross-model r7 P2): a **non-1** opcode for
        // an unoffered service must ALSO answer `UnknownService`, not
        // `UnknownOpcode` — grading the opcode of an unoffered service leaks that
        // the service id is known. Event/Sdk/Net/Entropy all share the structure.
        for (svc, name) in [
            (ServiceId::Event, "Event"),
            (ServiceId::Sdk, "Sdk"),
            (ServiceId::Net, "Net"),
            (ServiceId::Entropy, "Entropy"),
        ] {
            let n = hypercall_proto::encode_request(svc, 1, 7, &[], &mut frame).unwrap();
            let (step, status) = ring(&mut vmm, &frame[..n]);
            assert_eq!(
                status,
                Status::UnknownService as u16,
                "{name} opcode 7 on a pvclock-only VM must be UnknownService (unoffered), not \
                 UnknownOpcode — that would advertise the service"
            );
            assert_eq!(step, Step::Continued);
        }

        // The pvclock service itself still works on the same composition.
        let (status, _) = ring_pvclock_register(&mut vmm, PV_GPA);
        assert_eq!(status, Status::Ok as u16);
    }
}
