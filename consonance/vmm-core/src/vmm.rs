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

use hypercall_proto::{SeededEntropy, Service, Status};
use sha2::{Digest, Sha256};
use vmm_backend::{Backend, Exit, Gpa, VcpuState, Vtime};
use vtime::{IdlePlanner, VClock, VClockConfig};

use crate::contract::{self, MsrDisposition};
use crate::devices::{ISA_DEBUG_EXIT_PORT, LegacyPlatform, REPORT_PORT, Uart8250};
use crate::snapshot::{self, DeviceState, LegacyState, SnapshotError, UartState};
use crate::work::{WorkError, WorkSource};

/// xAPIC MMIO base (`0xFEE0_0000`, the architectural default the contract fixes
/// `IA32_APIC_BASE` to — its relocation write is deny-ignore, so the page never
/// moves). The Linux boot path routes loads/stores in `[APIC_MMIO_BASE,
/// APIC_MMIO_BASE + 0x1000)` into the userspace [`lapic::Lapic`].
const APIC_MMIO_BASE: u64 = 0xFEE0_0000;
/// One past the xAPIC MMIO page (`APIC_MMIO_BASE` + one 4 KiB page). A literal (not
/// `BASE + SIZE`) so the page-range check carries no arithmetic mutant.
const APIC_MMIO_END: u64 = 0xFEE0_1000;

/// Legacy ISA IRQ line for COM1 (the modeled 8250 at `0x3F8`). The kernel
/// registers `ttyS0` with this IRQ.
const COM1_IRQ: u8 = 4;
/// The interrupt **vector** the guest delivers COM1's IRQ 4 on. With no IO-APIC
/// and a real 8259, Linux maps the legacy ISA IRQs to a static vector window
/// starting at `ISA_IRQ_VECTOR(0) = 0x30` (the master PIC's ICW2 offset), so
/// `ISA_IRQ_VECTOR(4) = 0x30 + 4 = 0x34`. The VMM injects this vector (via the
/// `KVM_INTERRUPT` legacy-injection seam, exactly as a PIC `INTR`/`ExtINT` would)
/// when the 8250 raises its THRE interrupt; the guest IRQ-4 handler then drains
/// the userspace TX and EOIs the 8259. (Verified against the boot log: the guest
/// uses the 8259 in virtual-wire mode, no IO-APIC — see IMPLEMENTATION.md.)
const COM1_IRQ_VECTOR: u8 = 0x34;

/// `IA32_TSC` — the architectural time-stamp-counter MSR. The contract marks it
/// `emulate-vtime`; a guest `RDMSR(0x10)` reads the same V-time TSC the RDTSC
/// instruction returns, and `WRMSR(0x10)` sets it.
const IA32_TSC: u32 = 0x10;
/// `IA32_TSC_ADJUST` — the architectural per-logical-processor TSC offset MSR.
/// Also `emulate-vtime`; backs [`VtimeWiring::tsc_adjust`].
const IA32_TSC_ADJUST: u32 = 0x3b;

/// Why a run stopped. M1 requires `DebugExit { code: 0 }` specifically — **not**
/// `Hlt` (the payload's fallback) and **not** a non-zero code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalReason {
    /// isa-debug-exit (`0xF4`) wrote `code`. PASS = 0, FAIL = 1.
    DebugExit {
        /// The code byte the guest wrote to `0xF4`.
        code: u8,
    },
    /// `HLT` (the payload's fallback when isa-debug-exit is absent) — terminal.
    Hlt,
    /// Backend `Shutdown` (triple fault / explicit shutdown).
    Shutdown,
}

/// Errors that abort a run. A `ContractViolation` is the default-deny posture made
/// loud: an exit the skeleton does not model fails closed here — never silently.
#[derive(Debug, thiserror::Error)]
pub enum VmmError {
    /// A `Backend` operation failed.
    #[error("backend error")]
    Backend(#[from] vmm_backend::BackendError),
    /// The loader rejected the image or RAM was too small.
    #[error("load error")]
    Load(#[from] crate::multiboot::LoadError),
    /// The Linux bzImage loader rejected the kernel/initramfs (malformed image,
    /// does-not-fit, etc.) — the direct 64-bit boot path's trust boundary.
    #[error("linux load error")]
    LinuxLoad(#[from] crate::linux_loader::LinuxLoadError),
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

/// One serviced exit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Step {
    /// The exit was serviced; the run continues.
    Continued,
    /// The run reached a terminal state.
    Terminal(TerminalReason),
}

/// What a completed run produced (and what the M2 hash is taken over).
pub struct RunResult {
    /// Why the run stopped.
    pub reason: TerminalReason,
    /// The serial capture buffer, in order.
    pub serial: Vec<u8>,
    /// Per-exit-reason counts read from the backend (R-Backend observability).
    pub exit_counts: vmm_backend::ExitCounts,
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

/// The frozen V-time clock config (CPU-MSR-CONTRACT: the guest TSC is **2.0 GHz**,
/// leaf `0x15`). The work→nanosecond ratio is **1 ns per retired conditional
/// branch** — an integer ratio (`ratio_den == 1`), which INTEGRATION.md §4
/// requires for any snapshot-bearing config (a fractional ratio's sub-ns
/// remainder cannot survive `snapshot_vns`). So `tsc(work) = 2 · work` ticks,
/// strictly increasing whenever the guest retires a branch between two reads.
pub fn contract_vclock_config() -> VClockConfig {
    VClockConfig {
        ratio_num: 1,
        ratio_den: 1,
        tsc_hz: 2_000_000_000,
        tsc_base: 0,
        vns_base: 0,
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
    cfg: VClockConfig,
    clock: VClock,
    work: Box<dyn WorkSource>,
    entropy: SeededEntropy,
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
    last_intercept_work: u64,
    /// `IA32_TSC_ADJUST` (MSR `0x3b`): the architectural signed offset added to the
    /// base V-time TSC to form the **guest-visible** TSC (`visible = VClock::tsc +
    /// tsc_adjust`, wrapping mod 2⁶⁴ as the 64-bit counter does). `0` at reset and
    /// for every audited payload (none touches the TSC MSRs), so the visible TSC is
    /// exactly `VClock::tsc(work)`. A guest `WRMSR(IA32_TSC, X)` sets it so the
    /// visible TSC reads `X`; `WRMSR(IA32_TSC_ADJUST, Y)` sets it to `Y`. Stored as
    /// `u64` (two's-complement); hashed (it governs future TSC output).
    tsc_adjust: u64,
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
            tsc_adjust: 0,
        })
    }

    /// Draw `width` (2/4/8) bytes from the seeded stream for an `RDRAND`/`RDSEED`
    /// completion, using the **exact** byte convention of the `Entropy`
    /// hypercall service (opcode 1, a `u32` count) so the two never diverge. The
    /// value is returned with the low `width` bytes set (the backend writes only
    /// those to the destination register).
    fn draw_rng(&mut self, width: u8) -> Result<u64, VmmError> {
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

    /// The **guest-visible** TSC at work `work`: the base V-time TSC
    /// `VClock::tsc(work)` plus [`IA32_TSC_ADJUST`](Self::tsc_adjust), wrapping mod
    /// 2⁶⁴ as the architectural 64-bit counter does. RDTSC, RDTSCP, and
    /// `RDMSR(IA32_TSC)` all read this, so they agree exactly; with the default
    /// `tsc_adjust == 0` it is exactly `VClock::tsc(work)`.
    fn visible_tsc(&self, work: u64) -> u64 {
        self.clock.tsc(work).wrapping_add(self.tsc_adjust)
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
    /// `IA32_TSC_ADJUST` at snapshot time (the contract places TSC/TSC_ADJUST in
    /// `vm_state`), so a guest that wrote the MSR snapshots/restores faithfully.
    pub tsc_adjust: u64,
    /// `SeededEntropy::save_state()` (the PRNG position).
    pub entropy: Vec<u8>,
}

/// Upper bound on the diagnostic [`Vmm::preemption_landings`] trace, so a long-running
/// guest that preempts constantly (task 48 Postgres) cannot grow it unbounded. The trace
/// is observability only (not hashed); the task-47 gate payloads land far fewer than this
/// (`irq-landing` 8, `irq-landing-rng` 4). Recording stops at the cap.
const PREEMPTION_TRACE_CAP: usize = 4096;

/// `RFLAGS.IF` (interrupt-enable flag, bit 9) — the guest's own signal for "I am
/// waiting for an interrupt I can take". [`Vmm::idle_resume_target`] uses it to
/// tell a *resumable idle* `HLT` (`IF == 1`, an armed timer will wake the guest)
/// from a *terminal* one (`IF == 0` — the kernel's final `cli; hlt`, a wait
/// nothing will satisfy).
const RFLAGS_IF: u64 = 1 << 9;

/// The deterministic VMM, generic over `B: Backend`. **No method here mentions a
/// concrete backend.**
pub struct Vmm<B: Backend> {
    backend: B,
    ram: GuestRam,
    uart: Uart8250,
    /// The ordered **report stream** (corpus box-integration): every value the
    /// guest wrote to [`REPORT_PORT`] via `OUT`, in execution order. Each
    /// `report(u64)` payload call is two dwords (low then high). This is the
    /// guest-observable conformance output — it feeds [`Vmm::observable_digest`]
    /// (the O2/O3 digest), **not** [`Vmm::state_hash`] (the O1 full-state hash),
    /// so a stock / M1/M2 run that never touches the port leaves it empty and its
    /// `state_hash` is byte-for-byte unchanged from before this channel existed.
    report_stream: Vec<u32>,
    /// Diagnostic trace of the MEASURED preemption landings: the retired-branch work
    /// (`Exit::Deadline { reached }`) at which `run_until` actually delivered each LAPIC
    /// timer — the value the backend/VMM measured, NOT the ICR the guest programmed.
    /// **Not** hashed (observability only, like [`Self::report_stream`]); the task-47
    /// gate-2 seed-dependence assertion compares THIS (the actual landing work) across
    /// seeds, since the guest's self-reported ICR differs by seed for any backend (the
    /// RDRAND inputs differ) and so cannot prove seed-dependent *preemption*. Capped at
    /// [`PREEMPTION_TRACE_CAP`] so a long-running guest (task 48 Postgres, which preempts
    /// constantly) cannot grow it unbounded.
    preemption_landings: Vec<u64>,
    /// Diagnostic trace of the idle-resume landings (task 52): the **V-time** (ns) the
    /// clock was warped to when the guest went idle (`Exit::Hlt` with `RFLAGS.IF == 1`
    /// and an armed timer) and [`Self::resume_idle`] jumped to the timer deadline. The
    /// dual of [`Self::preemption_landings`] — *jumped to* the next event instead of
    /// *executed to* it. It records the **landed V-time** (the deadline), **not** a work
    /// count: a `HLT` live work read is skid-tainted (task-27 O1), so the idle path never
    /// reads it; the landing is derived skid-free from the last-intercept anchor + the
    /// timer deadline. **Not** hashed (observability only); deterministic across same-seed
    /// runs and seed-dependent for a seed-consuming guest, so it witnesses the idle path
    /// engaged. Capped at [`PREEMPTION_TRACE_CAP`].
    idle_landings: Vec<u64>,
    terminal: Option<TerminalReason>,
    /// The vCPU state captured at terminal (so `state_blob` is consistent and the
    /// fallible `save` is resolved once, where errors can propagate from `run`).
    saved_state: Option<VcpuState>,
    /// V-time + seeded-RNG wiring for the determinism-complete path. `None` for
    /// stock KVM / M1/M2 (RDTSC/RNG never surface there).
    vtime: Option<VtimeWiring>,
    /// Set when the most-recently-serviced exit staged an **RNG** completion
    /// (RDRAND/RDSEED) whose seeded draw advanced the entropy stream but whose
    /// register-write/RIP-advance is only staged for the next `KVM_RUN` (not in
    /// `Backend::save`/`VtimeSnapshot`). Snapshotting here is unsound — restore
    /// would re-execute the instruction against the already-advanced stream and
    /// draw the *next* word. [`Vmm::save_vtime`] refuses at this boundary. Cleared
    /// at the next `step` (its re-entry commits the staged completion). RDTSC/
    /// RDTSCP/IO/MSR/CPUID completions are **idempotent on replay** (positional
    /// work / re-queried device-or-contract value), so they do not set this.
    rng_completion_staged: bool,
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
    completion_staged: bool,
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
    vtime_synchronized: bool,
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
    first_entry_done: bool,
    /// The userspace xAPIC (ruling R1), wired **only** on the Linux boot path
    /// ([`crate::bringup::boot_linux`]); `None` for M1/M2/corpus payloads, which
    /// never touch the APIC page — so their `state_hash` is byte-for-byte
    /// unchanged (no `LAPC` chunk) and an MMIO exit there stays the default-deny
    /// [`VmmError::ContractViolation`]. When wired, a load/store in the
    /// `0xFEE0_0000` page is serviced from the register file (and its timer is
    /// driven off V-time). The register state is folded into the hash so two
    /// same-seed Linux runs that leave the APIC in different state diverge.
    lapic: Option<lapic::Lapic>,
    /// Minimal legacy PC platform I/O (PCI/PIC/PIT/CMOS/POST/extra-COM = "no
    /// device"), wired with the xAPIC on the Linux boot path; `None` for
    /// M1/M2/corpus (which never touch these ports), so their port-I/O default-deny
    /// and `state_hash` are unchanged.
    legacy: Option<LegacyPlatform>,
    /// When set ([`Vmm::wire_snapshot_hashing`]), [`Vmm::state_blob`] folds the
    /// **canonical `vm_state` encoding** into the hash as a `VMST` chunk — the
    /// snapshot/branch path's "the canonical `vm_state` blob drives `state_hash`"
    /// (BRINGUP). Default **off**, so M1/M2/corpus/Linux-boot blobs are byte-for-
    /// byte unchanged (their goldens do not move); a snapshot/branch consumer opts
    /// in. The chunk is the same bytes a [`Vmm::save_vm_state`] would seal, so two
    /// states whose canonical blob differs hash differently.
    snapshot_hashing: bool,
}

impl<B: Backend> Vmm<B> {
    /// Construct over an already-configured backend (CPUID/MSR-filter installed,
    /// entry state restored, RAM mapped) **and the [`GuestRam`] it owns**.
    pub fn new(backend: B, guest_ram: GuestRam) -> Self {
        Self {
            backend,
            ram: guest_ram,
            uart: Uart8250::new(),
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
            lapic: None,
            legacy: None,
            snapshot_hashing: false,
        }
    }

    /// Wire the userspace xAPIC **and** the minimal legacy PC platform I/O for the
    /// Linux boot path: after this, a guest load/store in the `0xFEE0_0000` MMIO
    /// page is serviced by `lapic`, and the curated legacy ISA/PCI ports return
    /// "no device" instead of failing closed. M1/M2/corpus leave both unwired (they
    /// never touch the page or those ports), keeping their `state_hash` unchanged.
    pub fn wire_lapic(&mut self, lapic: lapic::Lapic) -> &mut Self {
        self.lapic = Some(lapic);
        self.legacy = Some(LegacyPlatform::new());
        self
    }

    /// `true` once the userspace xAPIC is wired (the Linux boot path).
    pub fn lapic_wired(&self) -> bool {
        self.lapic.is_some()
    }

    /// Wire the determinism-complete V-time + seeded-RNG path (the
    /// `PatchedKvmBackend` composition root calls this; stock KVM leaves it
    /// unwired). After this, `RDTSC`/`RDTSCP` resolve to `VClock::tsc(work)` and
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

    /// The current full guest-memory image (the owned [`GuestRam`] backing) — the
    /// memory half a snapshot captures into [`crate::snapshot::SnapshotEngine`].
    pub fn guest_memory(&self) -> &[u8] {
        self.ram.as_bytes()
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
        snapshot::has_inflight_injection(&self.current_vcpu().events)
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
        snapshot::has_active_event_injection(&self.current_vcpu().events)
    }

    /// `true` iff a **genuine guest interrupt is pending delivery but not yet accepted** —
    /// a real vector raised into the LAPIC IRR and re-arbitrated as deliverable (e.g. the
    /// periodic V-time LAPIC timer), or the legacy COM1 ExtINT line asserting — held in the
    /// inject seam awaiting the next safe VM-entry.
    ///
    /// This is the **architecturally in-flight event** that the determinism overlay makes
    /// observable at a *synchronized* (snapshottable) boundary. Unlike a `kvm_vcpu_events`
    /// `interrupt_injected` bit — which exists only at a non-synchronized interrupt-window
    /// exit, where [`Vmm::save_vm_state`] fails closed — a vector pending in the IRR sits in
    /// the captured LAPIC state (device blob) and is **re-derived exactly** on restore (the
    /// IRR→ISR acceptance transition models a hypervisor-side event, so vmm-core leaves the
    /// vector in IRR until acceptance — see [`lapic::Lapic::peek_interrupt`] and
    /// `snapshot_restore_re_derives_the_in_flight_lapic_irq`). It is **distinct from an
    /// inert `kvm_vcpu_events` modifier residual** (a stale post-delivery `interrupt.nr`):
    /// this is a committed, *undelivered* interrupt. The live gate seals on this (or on
    /// [`Vmm::has_active_event_injection`]) to prove restore of a true in-flight event.
    ///
    /// Re-arbitrates (`advance_to(now)`, idempotent with the run loop's per-step service)
    /// and peeks **without** moving IRR→ISR, so it does not perturb the snapshot. Returns
    /// `false` when no LAPIC is wired (M1/M2/corpus) and no serial line is asserting.
    pub fn has_pending_guest_interrupt(&mut self) -> Result<bool, VmmError> {
        if self.lapic.is_none() {
            return Ok(self.pending_serial_vector().is_some());
        }
        let now = self.lapic_now_vns()?;
        // Scope the `&mut lapic` borrow so it ends before `self.pending_serial_vector()`.
        let lapic_pending = {
            let lapic = self.lapic.as_mut().expect("is_some checked above");
            lapic.advance_to(now);
            lapic.peek_interrupt().is_some()
        };
        Ok(lapic_pending || self.pending_serial_vector().is_some())
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
                    tsc_adjust: vt.tsc_adjust,
                    entropy: vt.entropy.save_state(),
                }))
            }
        }
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
        //    `Box<dyn Backend>` and the FROZEN trait must not grow a re-arm method.)
        let vcpu = self.backend.save()?;
        self.backend.restore(&vcpu)?;
        // 3. Commit the validated state — ALL infallible (the round-trip above was the last
        //    HARD-fallible step), so the commit is true all-or-nothing. The snapshot's
        //    effective V-time lives in `cfg.vns_base` and the last-intercept anchor resets to
        //    0 (effective V-time = `vns_base` until the next intercept advances work) —
        //    keeping a restored VM byte-identical to a fresh one at the same effective V-time
        //    (task-27 item 2). `IA32_TSC_ADJUST` is re-applied from the snapshot.
        let vt = self.vtime.as_mut().ok_or_else(|| {
            VmmError::ContractViolation("restore_vtime called but V-time is not wired".to_string())
        })?;
        vt.clock = clock;
        vt.cfg = cfg;
        vt.entropy = entropy;
        vt.last_intercept_work = 0;
        vt.tsc_adjust = snap.tsc_adjust;
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
        Ok(())
    }

    // --- full vm_state snapshot / restore (task 39) ------------------------

    /// Build the canonical [`vm_state::VmState`] from `vcpu` + the **current** live
    /// machine (the memory-less half of a snapshot): the supplied vCPU registers, the
    /// V-time block + entropy stream, and a vmm-core-owned device blob carrying the
    /// xAPIC, the legacy 8259/PCI latches, the 8250 UART, the ordered report stream,
    /// and `IA32_TSC_ADJUST`. The `contract_hash` is stamped so a restore can reject a
    /// blob taken under a different contract. The caller supplies `vcpu` (so the
    /// fallible `Backend::save` is resolved where the error can propagate —
    /// [`Vmm::save_vm_state`] — rather than swallowed). Infallible; the V-time block is
    /// anchored to the deterministic `last_intercept_work`, exactly like
    /// [`encode_vtime`], so it is byte-deterministic at any exit.
    fn build_vm_state(&self, vcpu: &VcpuState) -> vm_state::VmState {
        let mut s = vm_state::VmState::default();
        snapshot::fill_vcpu_state(&mut s, vcpu);
        let tsc_adjust = match &self.vtime {
            Some(vt) => {
                s.vtime = vm_state::VtimeState {
                    ratio_num: vt.cfg.ratio_num,
                    // `VtimeWiring::new` enforces `ratio_den == 1`; carry it so the
                    // blob is encodable (a fractional ratio is rejected at encode).
                    ratio_den: 1,
                    tsc_hz: vt.cfg.tsc_hz,
                    tsc_base: vt.cfg.tsc_base,
                    snapshot_vns: vt.clock.snapshot_vns(vt.last_intercept_work),
                };
                // The entropy PRNG position rides the `hypercall` section
                // (INTEGRATION.md §4: `Dispatcher::save_state()`, "notably the
                // entropy PRNG position") — vmm-core's hypercall RNG and RDRAND draw
                // from this one stream.
                s.hypercall = vt.entropy.save_state();
                vt.tsc_adjust
            }
            None => {
                // Unwired (M1/M2): a sentinel encodable V-time block, no entropy.
                s.vtime.ratio_den = 1;
                0
            }
        };
        let dev = DeviceState {
            tsc_adjust,
            // The ordered conformance report stream is guest-observable output (it
            // feeds `observable_digest` / the O2 oracle), captured here so a restore
            // resumes it — else a branch taken after `REPORT_PORT` writes would lose
            // them and its `observable_digest` would diverge from the reference. It is
            // NOT in the default `state_hash` (O1): that path never emits a `VMST`
            // chunk (snapshot-hashing is opt-in), so O1/O2 stay separate.
            report_stream: self.report_stream.clone(),
            uart: UartState {
                capture: self.uart.capture().to_vec(),
                regs: *self.uart.shadow_regs(),
                dlab: self.uart.dlab(),
                dlm: self.uart.dlm(),
            },
            lapic: self.lapic.as_ref().map(|l| l.snapshot()),
            legacy: self.legacy.as_ref().map(|l| {
                let imr = l.pic_imr();
                LegacyState {
                    config_address: l.config_address(),
                    master_imr: imr[0],
                    slave_imr: imr[1],
                }
            }),
            // The full `kvm_vcpu_events` (task 41), **canonicalized** so an in-flight
            // interrupt/exception injection round-trips while KVM's inert modifier
            // residuals (a stale `interrupt.nr`/`exception.nr`, the GET-only validity
            // bits) collapse to the clean record — replaying those raw into
            // `KVM_SET_VCPU_EVENTS` corrupts the resumed guest. All-zero at a quiescent
            // point, so M1/M2/corpus blobs are unchanged.
            events: snapshot::canonical_events(&vcpu.events),
        };
        s.devices = snapshot::encode_device_blob(&dev);
        s.contract_hash = contract::contract_hash();
        s
    }

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
    /// # Errors
    /// [`VmmError::ContractViolation`] at an RNG mid-exit boundary, a non-synchronized
    /// point, or if the live vCPU carries the PAE-only `kvm_sregs2` flags/pdptrs or
    /// `debugregs.flags` (all zero for the 64-bit determinism guest — see
    /// [`crate::snapshot::unrepresentable_state`]); [`VmmError::Backend`] if reading the
    /// live vCPU state fails (a snapshot **fails closed** rather than sealing a zeroed or
    /// lossy vCPU).
    pub fn save_vm_state(&self) -> Result<vm_state::VmState, VmmError> {
        if self.rng_completion_staged {
            return Err(VmmError::ContractViolation(
                "save_vm_state at an RNG mid-exit boundary: the seeded RDRAND/RDSEED draw advanced \
                 the stream but its completion is staged, not committed — snapshot only at a clean \
                 boundary (step once more first)."
                    .to_string(),
            ));
        }
        if self.vtime.is_some() && !self.vtime_synchronized {
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
        if let Some(reason) = snapshot::unrepresentable_state(&vcpu) {
            return Err(VmmError::ContractViolation(format!(
                "save_vm_state: {reason}"
            )));
        }
        Ok(self.build_vm_state(&vcpu))
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
        // 1a. Contract: a blob taken under a different CPUID/MSR contract would
        //     silently diverge on restore (INTEGRATION.md §4 `contract_hash`).
        if s.contract_hash != contract::contract_hash() {
            return Err(VmmError::Snapshot(SnapshotError::ContractMismatch));
        }
        // 1a-bis. A non-empty timer queue cannot be applied: vmm-core has no
        //     `vtime::TimerQueue` (the only timer is the xAPIC timer, carried in the
        //     device blob), so a non-default `timers` section would be silently
        //     dropped. Fail closed (a well-formed vmm-core blob always seals it empty).
        if s.timers != vm_state::TimerQueueState::default() {
            return Err(VmmError::ContractViolation(
                "restore_vm_state: snapshot carries a non-empty timer queue, but vmm-core has no \
                 TimerQueue to apply it — restoring would silently drop it. (A vmm-core snapshot \
                 always seals an empty timer queue; the xAPIC timer rides the device blob.)"
                    .to_string(),
            ));
        }
        // 1b. Decode the vmm-core device blob (total, never panics).
        let dev = snapshot::decode_device_blob(&s.devices.0)?;
        // 1b. Reject an UNRESTORABLE `kvm_vcpu_events` blob up front — a foreign / malformed
        //     v3 blob that sets a cap-disabled validity bit (`VALID_TRIPLE_FAULT`/`VALID_PAYLOAD`)
        //     would make `KVM_SET_VCPU_EVENTS` return `-EINVAL` only AFTER earlier `KVM_SET_*`
        //     ioctls inside `Backend::restore` already mutated the target vCPU. Validate here,
        //     while committing nothing, to preserve restore's reject-before-mutation (atomic)
        //     contract — symmetric with the `save_vm_state` guard (PR #12 round 8).
        if let Some(reason) = snapshot::cap_unrestorable_events(&dev.events) {
            return Err(VmmError::ContractViolation(format!(
                "restore_vm_state: {reason}"
            )));
        }
        // 1c. The blob's LAPIC must be coherent AND match this VM's wiring.
        let new_lapic = match (&dev.lapic, self.lapic.is_some()) {
            (Some(ls), true) => Some(
                lapic::Lapic::restore(ls)
                    .map_err(|_| SnapshotError::Lapic("incoherent LapicState in device blob"))?,
            ),
            (Some(_), false) | (None, true) => {
                return Err(VmmError::ContractViolation(
                    "restore_vm_state: snapshot/VM xAPIC wiring mismatch (one has a LAPIC, the other \
                     does not) — restore into a VM composed like the snapshot source."
                        .to_string(),
                ));
            }
            (None, false) => None,
        };
        // 1c-bis. The legacy platform must match this VM's wiring too — a blob whose
        // legacy subrecord is absent (or present) where the VM's is not is a malformed
        // snapshot, **rejected** rather than silently skipped (which would leave the
        // 8259 IMRs / PCI latch stale). (LAPIC + legacy are wired together by
        // `wire_lapic`, so a well-formed blob always agrees; this fails closed on one
        // that does not.)
        if dev.legacy.is_some() != self.legacy.is_some() {
            return Err(VmmError::ContractViolation(
                "restore_vm_state: snapshot/VM legacy-platform wiring mismatch (one has the 8259/PCI \
                 latches, the other does not) — restore into a VM composed like the snapshot source."
                    .to_string(),
            ));
        }
        // 1d. V-time: validate the rate matches and pre-build the clock + entropy.
        let vtime_commit = match self.vtime.as_ref() {
            Some(vt) => {
                if s.vtime.ratio_num != vt.cfg.ratio_num
                    || s.vtime.ratio_den != 1
                    || s.vtime.tsc_hz != vt.cfg.tsc_hz
                    || s.vtime.tsc_base != vt.cfg.tsc_base
                {
                    return Err(VmmError::ContractViolation(
                        "restore_vm_state: V-time clock-rate mismatch (the snapshot's ratio/tsc_hz/\
                         tsc_base differ from this VM's wired clock)."
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
                if s.vtime.tsc_hz != 0 || s.vtime.snapshot_vns != 0 {
                    return Err(VmmError::ContractViolation(
                        "restore_vm_state: snapshot carries a V-time block but this VM has no V-time \
                         wired — restore into a VM composed like the snapshot source."
                            .to_string(),
                    ));
                }
                None
            }
        };
        // 2. Build the vCPU state (pure). The typed records yield the reduced
        //    `vm_state` event subset; overwrite `events` with the device blob's **full**
        //    `kvm_vcpu_events` (task 41) so an in-flight interrupt/exception injection is
        //    re-established exactly (`KVM_SET_VCPU_EVENTS`), not silently zeroed — the
        //    device-blob record is a strict superset of the typed one and is
        //    authoritative. (The inject-seam `set_pending_irq` slot is NOT serialized:
        //    it is re-derived from the restored LAPIC IRR / UART THRE on the restored
        //    VM's first `service_pending_irqs`, so there is no separate plan to carry.)
        let mut vcpu = snapshot::vcpu_state_from(s);
        // Canonicalize on restore too — mirror the save side (`build_vm_state`, which stores
        // `canonical_events` in the device blob). This VM's own save path already strips KVM's
        // inert modifier residuals, but an **external or older v3 blob** (hand-built, or from a
        // different/buggy encoder) may carry RAW residuals; forwarding them verbatim to
        // `KVM_SET_VCPU_EVENTS` would reintroduce the exact corruption canonicalization exists
        // to prevent. Use `events_for_restore` (not `canonical_events`): it additionally forces
        // the cap-free NMI_PENDING/SHADOW/SMM validity bits ON, so KVM **clears** any stale
        // NMI-pending / interrupt-shadow / SMM left on a NON-fresh target vCPU (a clear bit means
        // "leave unchanged" to `KVM_SET_VCPU_EVENTS`) — restore is then independent of the prior
        // occupant (the branch / restore-in-place case; PR #12 round 6). The cap-gated
        // TRIPLE_FAULT/PAYLOAD were already rejected up front (step 1b).
        // Idempotent for a self-produced blob; the `state_hash` still uses `canonical_events`.
        vcpu.events = snapshot::events_for_restore(&dev.events);
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
            vt.tsc_adjust = dev.tsc_adjust;
            self.vtime_synchronized = true;
        }
        if let Some(l) = new_lapic {
            self.lapic = Some(l);
        }
        if let (Some(legacy), Some(ls)) = (self.legacy.as_mut(), dev.legacy) {
            legacy.restore(ls.config_address, ls.master_imr, ls.slave_imr);
        }
        self.uart
            .restore(dev.uart.capture, dev.uart.regs, dev.uart.dlab, dev.uart.dlm);
        // The ordered report stream is restored so a branch resumes the guest's
        // observable output (its `observable_digest` / O2 signal) instead of losing
        // every report emitted before the snapshot.
        self.report_stream = dev.report_stream;
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
        self.service_pending_irqs()?;
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
        // NO `KVM_RUN` (round-12). `preemption_deadline()` is `Some` ⇒ V-time is wired.
        let deadline = self.preemption_deadline();
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
            Exit::Deadline { reached } => work_before.is_some_and(|wb| reached.0 > wb),
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
            self.completion_staged = exit_stages_completion(&exit);
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
        self.complete_irq_delivery();
        match exit {
            Exit::Io {
                port,
                size,
                write: Some(v),
            } => self.dispatch_out(port, size, v),
            Exit::Io {
                port,
                size,
                write: None,
            } => self.dispatch_in(port, size),
            Exit::Rdmsr { index } => self.dispatch_rdmsr(index),
            Exit::Wrmsr { index, value } => self.dispatch_wrmsr(index, value),
            Exit::Cpuid { leaf, subleaf } => self.dispatch_cpuid(leaf, subleaf),
            Exit::Hlt => self.on_hlt(),
            Exit::Shutdown => Ok(self.terminate(TerminalReason::Shutdown)),
            Exit::Mmio { gpa, size, write } => self.dispatch_mmio(gpa, size, write),
            Exit::Hypercall(_) => Err(VmmError::ContractViolation(
                "unmodeled VMCALL hypercall (host handler is a later phase)".to_string(),
            )),
            // Determinism-complete path: RDTSC/RDTSCP → V-time TSC; RDRAND/RDSEED
            // → seeded stream. Computed here, above the trait; the backend only
            // surfaced + will complete the exit. Unwired (stock KVM / M1/M2) is a
            // loud contract violation, never a host-derived value.
            Exit::Rdtsc | Exit::Rdtscp => self.complete_tsc(),
            Exit::Rdrand { width } | Exit::Rdseed { width } => self.complete_rng(width),
            Exit::Deadline { reached } => self.on_deadline(reached),
        }
    }

    /// `step()` to a `Terminal`. Returns the serial capture, terminal reason, and
    /// exit counts.
    pub fn run(&mut self) -> Result<RunResult, VmmError> {
        // The work counter is prepared at the first guest entry inside `step`
        // (`first_entry_done`), so a `step()`-then-`run()` consumer is handled
        // correctly — `run` itself does not touch it.
        let reason = loop {
            if let Step::Terminal(r) = self.step()? {
                break r;
            }
        };
        // Capture the final vCPU state once (propagating any save error here, so
        // the infallible `state_blob` reads a consistent snapshot).
        self.saved_state = Some(self.backend.save()?);
        Ok(RunResult {
            reason,
            serial: self.uart.capture().to_vec(),
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
    /// output — the V-time clock rate (`ratio`/`tsc_hz`/`tsc_base`/`tsc_adjust`),
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
        put_chunk(&mut out, b"VCPU", &encode_vcpu_state(&self.current_vcpu()));
        put_chunk(&mut out, b"SERL", self.uart.capture());
        put_chunk(&mut out, b"DEV\0", &self.encode_device_terminal());
        if let Some(vt) = &self.vtime {
            put_chunk(&mut out, b"VTIM", &encode_vtime(vt));
        }
        // The xAPIC chunk is present **only** on the Linux boot path (`lapic`
        // wired); M1/M2/corpus emit none, so their hash is byte-for-byte
        // unchanged. It captures the register file + timer bookkeeping that
        // governs future interrupt delivery, so two same-seed Linux runs that
        // leave the APIC in different state hash differently.
        if let Some(lapic) = &self.lapic {
            put_chunk(&mut out, b"LAPC", &encode_lapic_state(&lapic.snapshot()));
        }
        // Legacy-platform state (the PCI CONFIG_ADDRESS latch + the 8259 master/
        // slave IMR) — Linux path only. The IMR governs which IRQ lines deliver, so
        // two same-seed runs that leave it different (hence future interrupt
        // delivery different) hash differently.
        if let Some(legacy) = &self.legacy {
            let mut legy = legacy.config_address().to_le_bytes().to_vec();
            legy.extend_from_slice(&legacy.pic_imr());
            put_chunk(&mut out, b"LEGY", &legy);
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
            let bytes = self
                .build_vm_state(&self.current_vcpu())
                .encode()
                .unwrap_or_default();
            put_chunk(&mut out, b"VMST", &bytes);
        }
        out
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
        let s = self.current_vcpu();
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

        // GPRs.
        let r = &s.regs;
        let mut regs = Vec::new();
        for x in [
            r.rax, r.rbx, r.rcx, r.rdx, r.rsi, r.rdi, r.rsp, r.rbp, r.r8, r.r9, r.r10, r.r11,
            r.r12, r.r13, r.r14, r.r15, r.rip, r.rflags,
        ] {
            regs.extend_from_slice(&x.to_le_bytes());
        }
        out.push(("regs", dig(&regs)));

        // Segment selectors/descriptors.
        let mut segs = Vec::new();
        for seg in [
            &s.sregs.cs,
            &s.sregs.ds,
            &s.sregs.es,
            &s.sregs.fs,
            &s.sregs.gs,
            &s.sregs.ss,
            &s.sregs.tr,
            &s.sregs.ldt,
        ] {
            encode_segment(&mut segs, seg);
        }
        out.push(("segments", dig(&segs)));

        // Descriptor tables (GDT, IDT).
        let mut dt = Vec::new();
        for d in [&s.sregs.gdt, &s.sregs.idt] {
            dt.extend_from_slice(&d.base.to_le_bytes());
            dt.extend_from_slice(&d.limit.to_le_bytes());
        }
        out.push(("desc-tables", dig(&dt)));

        // Control registers.
        let mut cr = Vec::new();
        for x in [
            s.sregs.cr0,
            s.sregs.cr2,
            s.sregs.cr3,
            s.sregs.cr4,
            s.sregs.cr8,
            s.sregs.efer,
            s.sregs.apic_base,
            s.sregs.flags,
        ] {
            cr.extend_from_slice(&x.to_le_bytes());
        }
        out.push(("control-regs", dig(&cr)));

        // PDPTRs + XCR0.
        let mut pd = Vec::new();
        for p in s.sregs.pdptrs {
            pd.extend_from_slice(&p.to_le_bytes());
        }
        out.push(("pdptrs", dig(&pd)));
        out.push(("xcr0", dig(&s.xcr0.to_le_bytes())));

        // Debug registers.
        let mut dr = Vec::new();
        for d in s.debugregs.db {
            dr.extend_from_slice(&d.to_le_bytes());
        }
        dr.extend_from_slice(&s.debugregs.dr6.to_le_bytes());
        dr.extend_from_slice(&s.debugregs.dr7.to_le_bytes());
        dr.extend_from_slice(&s.debugregs.flags.to_le_bytes());
        out.push(("debugregs", dig(&dr)));

        // Pending events.
        let mut ev = Vec::new();
        encode_events(&mut ev, &s.events);
        out.push(("events", dig(&ev)));

        // MP state.
        let mp = match s.mp_state {
            vmm_backend::MpState::Runnable => 0u8,
            vmm_backend::MpState::Halted => 1,
        };
        out.push(("mp_state", dig(&[mp])));

        // MSRs (BTreeMap, ascending key order).
        let mut msr = Vec::new();
        for (idx, val) in &s.msrs {
            msr.extend_from_slice(&idx.to_le_bytes());
            msr.extend_from_slice(&val.to_le_bytes());
        }
        out.push(("msrs", dig(&msr)));

        // XSAVE split into legacy (x87 + SSE), the 64-byte XSAVE header, and the
        // extended area — the prime suspects for host-leaked / init-optimization
        // bytes.
        let xs = &s.xsave;
        let part = |lo: usize, hi: usize| {
            let (lo, hi) = (lo.min(xs.len()), hi.min(xs.len()));
            dig(&xs[lo..hi])
        };
        out.push(("xsave-legacy", part(0, 512)));
        out.push(("xsave-header", part(512, 576)));
        out.push(("xsave-extended", part(576, xs.len())));

        // Serial + device + V-time.
        out.push(("serial", dig(self.uart.capture())));
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
                vt.cfg.tsc_hz,
                vt.cfg.tsc_base,
                vt.tsc_adjust,
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
    /// delivered each LAPIC timer (`Exit::Deadline { reached }`), in order. This is the
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
        self.uart.capture()
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
    /// `Machine::state_hash`, which folds in latent RAM / V-time / seeded-entropy
    /// state): the report stream is what the guest *deliberately emits*, so it is
    /// the right conformance signal — a constant payload that happens to be
    /// perfectly deterministic still produces a meaningful (and seed-sensitive,
    /// for an RNG payload) digest here. Pure, length-prefixed, domain-tagged
    /// (`OBSV`); each report dword is hashed little-endian in execution order, so
    /// two runs that emit different reported values digest differently even with
    /// byte-identical serial output.
    pub fn observable_digest(&self) -> [u8; 32] {
        crate::corpus::observable_digest_of(&self.report_stream, self.uart.capture())
    }

    // --- dispatch helpers --------------------------------------------------

    fn terminate(&mut self, reason: TerminalReason) -> Step {
        self.terminal = Some(reason);
        Step::Terminal(reason)
    }

    fn dispatch_out(&mut self, port: u16, size: u8, value: u32) -> Result<Step, VmmError> {
        if port == ISA_DEBUG_EXIT_PORT {
            require_byte_io("OUT", port, size)?;
            return Ok(self.terminate(TerminalReason::DebugExit { code: value as u8 }));
        }
        if Uart8250::owns(port) {
            require_byte_io("OUT", port, size)?;
            self.uart.write(port, value as u8);
            return Ok(Step::Continued);
        }
        if port == REPORT_PORT {
            // The conformance report channel: a 32-bit `OUT REPORT_PORT, EAX`
            // appends `EAX` to the ordered report stream (corpus box-integration).
            // It is a write (no completion); the value is already deterministic
            // (a V-time TSC / seeded-PRNG word / retired-count the guest computed),
            // and the stream is ordered by execution, so it is a pure function of
            // the run. The 4-byte width is the ABI — a non-dword access is unmodeled
            // and fails closed (never a truncated/extended report value).
            require_dword_io("OUT", port, size)?;
            self.report_stream.push(value);
            return Ok(Step::Continued);
        }
        // Linux path: the curated legacy ISA/PCI ports accept-and-drop.
        if let Some(legacy) = self.legacy.as_mut()
            && LegacyPlatform::owns(port)
        {
            legacy.write(port, size, value);
            return Ok(Step::Continued);
        }
        Err(VmmError::ContractViolation(format!(
            "unmodeled OUT to port {port:#06x} value {value:#x} (size {size})"
        )))
    }

    fn dispatch_in(&mut self, port: u16, size: u8) -> Result<Step, VmmError> {
        if Uart8250::owns(port) {
            require_byte_io("IN", port, size)?;
            if let Some(byte) = self.uart.read(port) {
                self.backend.complete_read(u64::from(byte))?;
                return Ok(Step::Continued);
            }
        }
        // Linux path: the curated legacy ISA/PCI ports read back "no device".
        if let Some(legacy) = self.legacy.as_ref()
            && LegacyPlatform::owns(port)
        {
            let value = legacy.read(port, size);
            self.backend.complete_read(value)?;
            return Ok(Step::Continued);
        }
        Err(VmmError::ContractViolation(format!(
            "unmodeled IN from port {port:#06x} (size {size})"
        )))
    }

    /// Service an MMIO exit. On the Linux path (`lapic` wired) a load/store in the
    /// `0xFEE0_0000` xAPIC page is routed to the userspace [`lapic::Lapic`]; every
    /// other MMIO — and **all** MMIO when the LAPIC is unwired (M1/M2/corpus) —
    /// stays the default-deny [`VmmError::ContractViolation`]. xAPIC registers are
    /// 32-bit; a load completes with the register value, a store updates the
    /// register file (no completion). A bad offset / out-of-page access fails
    /// closed (never a silent value).
    fn dispatch_mmio(&mut self, gpa: Gpa, size: u8, write: Option<u64>) -> Result<Step, VmmError> {
        let in_apic_page = self.lapic.is_some() && (APIC_MMIO_BASE..APIC_MMIO_END).contains(&gpa.0);
        if !in_apic_page {
            return Err(VmmError::ContractViolation(format!(
                "unmodeled MMIO at {:#x} (size {size}); only the xAPIC page is modeled, and only on \
                 the Linux boot path",
                gpa.0
            )));
        }
        let now_vns = self.lapic_now_vns()?;
        let offset = (gpa.0 - APIC_MMIO_BASE) as u32;
        let lapic = self.lapic.as_mut().expect("in_apic_page implies wired");
        match write {
            None => {
                // xAPIC register load (32-bit). `complete_read` masks to `size`.
                let value = lapic.mmio_read(offset, now_vns).map_err(|e| {
                    VmmError::ContractViolation(format!("xAPIC read {offset:#x}: {e}"))
                })?;
                self.backend.complete_read(u64::from(value))?;
                Ok(Step::Continued)
            }
            Some(v) => {
                // xAPIC register store (32-bit); no completion.
                lapic.mmio_write(offset, v as u32, now_vns).map_err(|e| {
                    VmmError::ContractViolation(format!("xAPIC write {offset:#x}: {e}"))
                })?;
                Ok(Step::Continued)
            }
        }
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
    fn lapic_now_vns(&self) -> Result<u64, VmmError> {
        match &self.vtime {
            Some(vt) => {
                let work = if self.backend.capabilities().deterministic_tsc {
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
    fn preemption_deadline(&self) -> Option<Vtime> {
        let deadline_vns = self.armed_timer_deadline_vns()?;
        let vt = self
            .vtime
            .as_ref()
            .expect("armed_timer_deadline_vns implies V-time wired");
        Some(Vtime(vt.clock.work_for_vns(deadline_vns)))
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
    fn armed_timer_deadline_vns(&self) -> Option<u64> {
        if self.vtime.is_none() || !self.backend.capabilities().deterministic_tsc {
            return None;
        }
        self.lapic.as_ref()?.next_timer_deadline()
    }

    /// Handle [`Exit::Deadline`]: the guest was preempted at exactly `reached`
    /// retired branches (a pure function of the seed — bit-identical across same-seed
    /// runs even mid-spin). Advance the skid-free last-intercept anchor to it — a
    /// deterministic V-time intercept, like an RDTSC trap — so the NEXT `step`'s
    /// [`Self::service_pending_irqs`] sees [`Self::lapic_now_vns`] at the timer
    /// deadline, fires the timer into the LAPIC IRR, and injects it at the first
    /// injectable entry. No completion (the backend left nothing pending).
    fn on_deadline(&mut self, reached: Vtime) -> Result<Step, VmmError> {
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
                "Exit::Deadline (reached {}) with no V-time wired — run_until is the \
                 determinism-path preemption seam and is never issued without it",
                reached.0
            ))),
        }
    }

    /// Handle [`Exit::Hlt`]: discriminate a **resumable idle** halt from a **terminal**
    /// one and act. The guest is either *waiting for an interrupt that will come* or
    /// *dead* — the same signal a real CPU uses tells them apart: the interrupt-enable
    /// flag (`RFLAGS.IF`) plus whether a timer is armed
    /// ([`Self::idle_resume_target`]). A resumable idle warps V-time to the deadline and
    /// resumes ([`Self::resume_idle`]); everything else (the kernel's final `cli; hlt`
    /// after poweroff, or any wait nothing will satisfy) terminates exactly as before —
    /// the strictly-additive change of task 52.
    fn on_hlt(&mut self) -> Result<Step, VmmError> {
        match self.idle_resume_target()? {
            Some(deadline_vns) => Ok(self.resume_idle(deadline_vns)),
            None => Ok(self.terminate(TerminalReason::Hlt)),
        }
    }

    /// The armed timer's V-time deadline (ns) **iff** this `HLT` is a *resumable idle* —
    /// the guest can take an interrupt (`RFLAGS.IF == 1`) **and** a LAPIC timer is armed
    /// on the determinism path ([`Self::armed_timer_deadline_vns`]) **and** that timer
    /// would actually be **delivered** when it fires ([`lapic::Lapic::armed_timer_deliverable`]);
    /// otherwise `None` (terminal halt).
    ///
    /// **Deliverability, not just armed (robustness).** A timer can be *armed* yet
    /// *undeliverable* — a reserved vector (`< 16`), or masked by TPR/PPR. Jumping for
    /// such a timer would fire it into the LAPIC IRR but never inject it
    /// ([`peek_interrupt`](lapic::Lapic::peek_interrupt) returns `None`), so a one-shot
    /// leaves **no future wake** and the vCPU would be stuck (warping V-time forever) or
    /// prematurely terminated. So an undeliverable armed timer is treated as **terminal**
    /// (exactly like `IF == 0` — a wait nothing will satisfy), never a resumable idle.
    /// Real Linux programs a deliverable timer vector, so `runc`/Postgres are unaffected;
    /// this only hardens the keystone against adversarial (dissonance-fuzzed) guests.
    ///
    /// The no-timer and deliverability checks come **first** and are cheap (no vCPU read):
    /// the common terminal paths — minimal-boot poweroff and every existing terminal, all
    /// `IF == 0`/no-timer — never reach the `RFLAGS` read, so their behavior and
    /// `state_hash` are byte-for-byte unchanged (the no-regression gate). The `RFLAGS`
    /// read is a [`Backend::save`] (a pure vCPU-state read that runs no guest code, so
    /// the work counter is untouched) and **fails closed** ([`VmmError::Backend`]) on a
    /// save error rather than guessing the halt's disposition.
    fn idle_resume_target(&self) -> Result<Option<u64>, VmmError> {
        let Some(deadline_vns) = self.armed_timer_deadline_vns() else {
            return Ok(None);
        };
        // Armed is not enough — the timer must be DELIVERABLE (would wake the guest).
        // `armed_timer_deadline_vns` returning `Some` implies the LAPIC is wired.
        if !self
            .lapic
            .as_ref()
            .is_some_and(lapic::Lapic::armed_timer_deliverable)
        {
            return Ok(None);
        }
        let rflags = self.backend.save()?.regs.rflags;
        Ok((rflags & RFLAGS_IF != 0).then_some(deadline_vns))
    }

    /// Resume a *resumable idle* `HLT` by **jumping** V-time to the armed timer's
    /// deadline `deadline_vns` — reaching the next scheduled event without executing a
    /// single instruction (the idle dual of the `run_until` execution path).
    ///
    /// **Intercept-aligned, skid-free (task-52 review fix).** A live `work()` read at a
    /// `HLT` is **skid-tainted** — the box O1 evidence (task-27, see [`Vmm::save_vtime`]
    /// / [`encode_vtime`]) shows a non-V-time-intercept live read **diverges** across
    /// same-seed runs (post-last-intercept exit-path skid). Folding such a read into
    /// `vns_base` would leave a skid term that does **not** cancel at the next
    /// (skid-free) intercept, so `vns_base`/TSC/`state_hash` would diverge once the
    /// guest idles before a state-affecting read — defeating deterministic-twice. So
    /// this **never reads the live counter at the `HLT`**: it anchors the idle advance
    /// on the existing skid-free [`last_intercept_work`](VtimeWiring::last_intercept_work)
    /// (the last V-time intercept's deterministic work), adds
    /// `deadline − vns(last_intercept_work)` to the idle accumulator
    /// ([`vtime::IdlePlanner`] decides the amount; [`VClock::advance_idle`] applies it),
    /// and leaves the anchor **unmoved** and the point **un-synchronized**. The guest
    /// retires **zero** branches during the halt, so the execution span
    /// `last_intercept_work → W_next` is skid-free at **both** ends: the exact V-time
    /// anchor is re-established only at the next real intercept `W_next` (`complete_tsc`
    /// etc.), where `vns(W_next) = vns_base + W_next·ratio` carries no skid term. The
    /// jump never touches the retired-branch count (zero fabricated branches; `B ≡ A`
    /// holds over execution); after it `vns(last_intercept_work) == deadline`, so the
    /// next [`Self::service_pending_irqs`] fires the timer into the LAPIC IRR and injects
    /// it, and `step` re-enters. An overdue deadline plans a zero jump and fires
    /// immediately (the HLT analogue of the planner's `TargetInPast`).
    fn resume_idle(&mut self, deadline_vns: u64) -> Step {
        let vt = self
            .vtime
            .as_mut()
            .expect("idle_resume_target returning Some implies V-time wired");
        // Anchor on the SKID-FREE last intercept, NOT a live HLT read (which is
        // skid-tainted on-box). `lapic_now_vns` reads this same anchor, so this is the
        // clock's current effective V-time.
        let now_vns = vt.clock.snapshot_vns(vt.last_intercept_work);
        let advance = IdlePlanner::new().plan(now_vns, deadline_vns);
        // Move ONLY the idle accumulator (vns_base). Crucially: do NOT move
        // `last_intercept_work` and do NOT set `vtime_synchronized` — the `HLT` is not a
        // skid-free point, so the exact anchor stays at the last real intercept and is
        // re-established only at the next one (no skid term ever enters `vns_base`).
        vt.clock.advance_idle(advance.advance_vns);
        // Trace the idle V-time landing (the deadline reached; skid-free, derived from
        // the anchor + planner — never a live work read). Observability, not hashed; capped.
        if self.idle_landings.len() < PREEMPTION_TRACE_CAP {
            self.idle_landings.push(advance.landed_vns);
        }
        Step::Continued
    }

    /// Arbitrate and hand the backend the one IRQ vector to inject at the next safe
    /// VM-entry — the V-time LAPIC timer **and** the legacy COM1 serial line — via
    /// [`Backend::set_pending_irq`] (the `KVM_INTERRUPT` / interrupt-window handshake
    /// lives below the trait). Runs once before every entry.
    ///
    /// **LAPIC timer.** Advance the timer to the current [`Self::lapic_now_vns`]
    /// (firing the timer vector into IRR when due, re-arming if periodic), then
    /// **peek** the current highest-priority deliverable vector. Peeking
    /// (not taking) leaves it in the IRR; the IRR→ISR transition happens in
    /// [`Self::complete_irq_delivery`] only once the backend confirms acceptance, so
    /// a snapshot/`state_hash` taken while a vector waits on the interrupt window
    /// shows it pending in IRR, not prematurely in-service.
    ///
    /// **Serial COM1 (IRQ 4).** [`Self::pending_serial_vector`] returns
    /// [`COM1_IRQ_VECTOR`] while the 8250 asserts its THRE interrupt and the 8259
    /// has not masked the line — the legacy ExtINT path (no LAPIC IRR/ISR; the guest
    /// EOIs the 8259). It is **edge-driven by the guest's own `IER` write**, so its
    /// timing is a deterministic function of guest execution.
    ///
    /// **Arbitration.** The backend holds **one** slot, so we re-arbitrate every
    /// entry and pass the higher-priority pending vector. Local-APIC interrupts
    /// outrank the legacy ExtINT line, so a deliverable LAPIC vector wins; the serial
    /// vector is injected only when the LAPIC has nothing pending. Re-arbitrating
    /// every entry means the backend never injects a stale vector (the serial line
    /// de-asserts the moment the kernel drains the TX and clears `IER.THRI`).
    ///
    /// A **no-op when the xAPIC is unwired** (M1/M2/corpus/multiboot never wire the
    /// LAPIC *or* the legacy platform), so those paths call neither `set_pending_irq`
    /// nor `advance_to` — their state and `state_hash` are byte-for-byte unchanged.
    fn service_pending_irqs(&mut self) -> Result<(), VmmError> {
        if self.lapic.is_none() {
            return Ok(());
        }
        let now_vns = self.lapic_now_vns()?;
        // Scope the `&mut lapic` borrow so it ends before `self.backend`.
        let lapic_vector = {
            let lapic = self.lapic.as_mut().expect("is_some checked above");
            lapic.advance_to(now_vns);
            lapic.peek_interrupt() // re-arbitrate; do NOT move IRR→ISR
        };
        // Local-APIC interrupts outrank the legacy ExtINT serial line.
        let vector = lapic_vector.or_else(|| self.pending_serial_vector());
        self.backend.set_pending_irq(vector)?;
        Ok(())
    }

    /// The COM1 serial interrupt vector ([`COM1_IRQ_VECTOR`]) if the 8250 is
    /// currently asserting its THRE interrupt (the guest enabled `IER.THRI` and THR
    /// is empty) **and** the 8259 has not masked IRQ 4 — else `None`. Gated on the
    /// legacy platform being wired (the Linux path; it is wired together with the
    /// xAPIC), so M1/M2/corpus never see a serial vector.
    fn pending_serial_vector(&self) -> Option<u8> {
        let legacy = self.legacy.as_ref()?;
        (self.uart.thre_irq_asserted() && !legacy.irq_masked(COM1_IRQ)).then_some(COM1_IRQ_VECTOR)
    }

    /// Complete delivery of every vector the backend **accepted** (issued
    /// `KVM_INTERRUPT` for) during the last `backend.run()`. Called after the entry
    /// and before dispatching the exit, so a guest APIC read / EOI in that exit — and
    /// any snapshot — observes a LAPIC vector in-service exactly once KVM accepted it
    /// (never during the interrupt-window wait).
    ///
    /// An accepted **LAPIC** vector moves IRR→ISR ([`lapic::Lapic::take_interrupt`]).
    /// An accepted **legacy COM1** vector is an ExtINT serviced + EOI'd at the 8259,
    /// not the userspace LAPIC, so it must take **no** IRR/ISR transition — and it
    /// doesn't: [`Self::service_pending_irqs`] only injects the serial vector when the
    /// LAPIC has *nothing* deliverable ([`lapic::Lapic::peek_interrupt`] returned
    /// `None`), and the LAPIC IRR cannot change between that arbitration and here (the
    /// timer fires in `service_pending_irqs`, and any guest LAPIC write is a later
    /// exit), so `take_interrupt` is a no-op exactly when a serial vector was the one
    /// accepted. No-op overall when the xAPIC is unwired (the backend never accepts a
    /// maskable IRQ there).
    fn complete_irq_delivery(&mut self) {
        while self.backend.take_accepted_interrupt().is_some() {
            if let Some(lapic) = self.lapic.as_mut() {
                lapic.take_interrupt();
            }
        }
    }

    fn dispatch_rdmsr(&mut self, index: u32) -> Result<Step, VmmError> {
        let disp = contract::rdmsr_disposition(index);
        loud_msr(
            MsrDir::Read,
            index,
            None,
            self.guest_rip(),
            self.current_work(),
            &disp,
        );
        match disp {
            MsrDisposition::AllowFixed(v) => {
                self.backend.complete_read(v)?;
                Ok(Step::Continued)
            }
            MsrDisposition::DenyGp => {
                self.backend.complete_fault()?;
                Ok(Step::Continued)
            }
            MsrDisposition::EmulateVtime => self.rdmsr_vtime(index),
            // allow-stateful is in-kernel and should never surface; a read-side
            // deny-ignore-write does not exist in the contract.
            MsrDisposition::AllowStateful | MsrDisposition::DenyIgnoreWrite => {
                Err(VmmError::ContractViolation(format!(
                    "RDMSR {index:#x} surfaced with a non-userspace disposition {disp:?}"
                )))
            }
        }
    }

    fn dispatch_wrmsr(&mut self, index: u32, value: u64) -> Result<Step, VmmError> {
        let disp = contract::wrmsr_disposition(index, value);
        loud_msr(
            MsrDir::Write,
            index,
            Some(value),
            self.guest_rip(),
            self.current_work(),
            &disp,
        );
        match disp {
            MsrDisposition::DenyIgnoreWrite => {
                // Drop the write (already logged), then resume.
                self.backend.complete_ok()?;
                Ok(Step::Continued)
            }
            // A write to a read-only allow-fixed row, or any deny-gp row, faults.
            MsrDisposition::DenyGp | MsrDisposition::AllowFixed(_) => {
                self.backend.complete_fault()?;
                Ok(Step::Continued)
            }
            MsrDisposition::EmulateVtime => self.wrmsr_vtime(index, value),
            MsrDisposition::AllowStateful => Err(VmmError::ContractViolation(format!(
                "WRMSR {index:#x} surfaced but is allow-stateful (should be in-kernel)"
            ))),
        }
    }

    fn dispatch_cpuid(&mut self, leaf: u32, subleaf: u32) -> Result<Step, VmmError> {
        // Stock KVM answers CPUID in-kernel and never reaches here; a backend that
        // surfaces it gets the frozen model overlaid with the live dynamic cells.
        let state = self.backend.save()?;
        let base = lookup_cpuid(leaf, subleaf);
        let resolved = contract::resolve_cpuid(base, state.sregs.cr4, state.xcr0);
        self.backend
            .complete_cpuid(resolved.eax, resolved.ebx, resolved.ecx, resolved.edx)?;
        Ok(Step::Continued)
    }

    /// Complete a pending `RDTSC`/`RDTSCP` with the **V-time** TSC,
    /// [`VtimeWiring::visible_tsc`] (`VClock::tsc(work)` + `IA32_TSC_ADJUST`) — never
    /// a host TSC, and identical to what `RDMSR(IA32_TSC)` returns. `work` is read
    /// from the host counter at this exit; the backend writes the value to EDX:EAX
    /// (and, for RDTSCP, the guest's `IA32_TSC_AUX` to ECX, which the backend
    /// supplies from guest state). Fails closed if V-time is unwired (stock KVM /
    /// M1/M2 never surface these exits, so reaching here without wiring is a contract
    /// bug).
    fn complete_tsc(&mut self) -> Result<Step, VmmError> {
        let tsc = {
            let Some(vt) = self.vtime.as_mut() else {
                return Err(VmmError::ContractViolation(
                    "RDTSC/RDTSCP surfaced but V-time is not wired (stock backend?) — refusing to \
                     supply a host TSC"
                        .to_string(),
                ));
            };
            let work = vt.work.work()?;
            // This is a V-time intercept (a synchronized point): record its
            // *deterministic* work so the `VTIM` hash anchors here, not to a
            // skid-laden live read at hash time (task-27 item 2).
            vt.last_intercept_work = work;
            vt.visible_tsc(work)
        };
        self.backend.complete_read(tsc)?;
        // A V-time intercept: `last_intercept_work` is now the exact current work, so
        // a snapshot here would be exact (see `save_vtime`).
        self.vtime_synchronized = true;
        Ok(Step::Continued)
    }

    /// Service an `emulate-vtime` `RDMSR` (`IA32_TSC` 0x10 → the guest-visible
    /// V-time TSC, the **same** value the RDTSC instruction returns; `IA32_TSC_ADJUST`
    /// 0x3b → the stored adjust). Fails closed if V-time is unwired (stock KVM /
    /// M1/M2 never surface these), or if an unexpected index is routed here. Both are
    /// V-time MSR intercepts, so each records its deterministic work as the hash
    /// anchor (like [`complete_tsc`](Self::complete_tsc)).
    fn rdmsr_vtime(&mut self, index: u32) -> Result<Step, VmmError> {
        let value = {
            let Some(vt) = self.vtime.as_mut() else {
                return Err(VmmError::ContractViolation(format!(
                    "emulate-vtime RDMSR {index:#x} surfaced but V-time is not wired (stock \
                     backend?) — refusing to supply a host value"
                )));
            };
            match index {
                IA32_TSC => {
                    let work = vt.work.work()?;
                    vt.last_intercept_work = work;
                    vt.visible_tsc(work)
                }
                IA32_TSC_ADJUST => {
                    // A TSC_ADJUST access is a V-time MSR intercept too: sample its
                    // deterministic work so the hashed effective V-time stays current
                    // (the returned value — the adjust — does not depend on work).
                    let work = vt.work.work()?;
                    vt.last_intercept_work = work;
                    vt.tsc_adjust
                }
                other => {
                    return Err(VmmError::ContractViolation(format!(
                        "emulate-vtime RDMSR {other:#x} is not a V-time MSR (only IA32_TSC 0x10 and \
                         IA32_TSC_ADJUST 0x3b are emulate-vtime)"
                    )));
                }
            }
        };
        self.backend.complete_read(value)?;
        // A V-time MSR intercept: `last_intercept_work` is the exact current work.
        self.vtime_synchronized = true;
        Ok(Step::Continued)
    }

    /// Service an `emulate-vtime` `WRMSR`. `WRMSR(IA32_TSC, X)` sets the guest-visible
    /// TSC to `X` by choosing the adjust `X − VClock::tsc(work)` (architecturally a
    /// TSC write also moves `IA32_TSC_ADJUST` by the same delta — this is exactly
    /// that); `WRMSR(IA32_TSC_ADJUST, Y)` sets the adjust to `Y`, shifting the visible
    /// TSC by `Y − old`. Both are honored (`complete_ok`); the write is deterministic
    /// (guest-driven at a deterministic work point) and folds into the hashed
    /// `tsc_adjust`. Fails closed if V-time is unwired or the index is unexpected.
    fn wrmsr_vtime(&mut self, index: u32, value: u64) -> Result<Step, VmmError> {
        {
            let Some(vt) = self.vtime.as_mut() else {
                return Err(VmmError::ContractViolation(format!(
                    "emulate-vtime WRMSR {index:#x} surfaced but V-time is not wired (stock \
                     backend?) — refusing to emulate"
                )));
            };
            match index {
                IA32_TSC => {
                    let work = vt.work.work()?;
                    vt.last_intercept_work = work;
                    // visible_tsc(work) == value ⇔ adjust = value − VClock::tsc(work).
                    vt.tsc_adjust = value.wrapping_sub(vt.clock.tsc(work));
                }
                IA32_TSC_ADJUST => {
                    // V-time MSR intercept — sample work to keep the hashed effective
                    // V-time current (see the RDMSR side).
                    let work = vt.work.work()?;
                    vt.last_intercept_work = work;
                    vt.tsc_adjust = value;
                }
                other => {
                    return Err(VmmError::ContractViolation(format!(
                        "emulate-vtime WRMSR {other:#x} is not a V-time MSR (only IA32_TSC 0x10 and \
                         IA32_TSC_ADJUST 0x3b are emulate-vtime)"
                    )));
                }
            }
        }
        self.backend.complete_ok()?;
        // A V-time MSR intercept: `last_intercept_work` is the exact current work.
        self.vtime_synchronized = true;
        Ok(Step::Continued)
    }

    /// Complete a pending `RDRAND`/`RDSEED` with `width` bytes from the **seeded**
    /// entropy stream (the same one the `Entropy` hypercall uses) — never the host
    /// RNG. The backend masks to `width` and sets CF (deterministic success).
    /// Fails closed if V-time/RNG is unwired.
    ///
    /// An RNG exit is a **V-time intercept** (one of the four determinism-cap traps),
    /// so it records its deterministic work as the hash anchor — exactly like
    /// [`complete_tsc`](Self::complete_tsc) and the TSC-MSR paths. Without this, if an
    /// RNG exit were the last intercept before a checkpoint, the `VTIM` hash would use
    /// a stale (prior-intercept) work value, so two states that burned different
    /// branch counts before the same seeded draw would collide — a false determinism
    /// MATCH that then diverges on the next TSC read.
    fn complete_rng(&mut self, width: u8) -> Result<Step, VmmError> {
        let value = {
            let Some(vt) = self.vtime.as_mut() else {
                return Err(VmmError::ContractViolation(
                    "RDRAND/RDSEED surfaced but the seeded entropy stream is not wired (stock \
                     backend?) — refusing to supply host RNG"
                        .to_string(),
                ));
            };
            // Record the synchronized work at this RNG intercept (the draw itself
            // retires no guest branches, so the order vs `draw_rng` is irrelevant).
            let work = vt.work.work()?;
            vt.last_intercept_work = work;
            vt.draw_rng(width)?
        };
        self.backend.complete_read(value)?;
        // A V-time intercept: the V-time is exact (`last_intercept_work` is current).
        // Independently, the seeded draw advanced the stream but `complete_read` only
        // STAGES the reg-write/RIP-advance for the next `KVM_RUN`, so this is an unsafe
        // *entropy* snapshot boundary until the next `step` re-enters and commits it
        // (see `save_vtime`, which fails on the RNG flag even though V-time is exact).
        self.vtime_synchronized = true;
        self.rng_completion_staged = true;
        Ok(Step::Continued)
    }

    /// Best-effort guest RIP at the faulting instruction, for the loud §1 MSR
    /// log. Logging must never abort the run, so a `save()` failure degrades to
    /// `0` rather than propagating — the architectural effect (the completion) is
    /// still serviced afterward, where its own error path applies.
    fn guest_rip(&self) -> u64 {
        self.backend.save().map(|s| s.regs.rip).unwrap_or_default()
    }

    /// The current V-time work count for the loud §1 MSR log, or `None` when
    /// V-time is unwired (stock KVM / M1/M2) — logged honestly as `unwired`
    /// rather than a fake `0` that would read as a real branch count. A bad
    /// counter read also degrades to `None` (logging must never abort a run; the
    /// architectural effect is serviced afterward where its error path applies).
    fn current_work(&self) -> Option<u64> {
        self.vtime.as_ref().and_then(|vt| vt.work.work().ok())
    }

    /// The vCPU state for the hash: the snapshot captured at terminal if present,
    /// else a best-effort live `save` (default on a backend that cannot save —
    /// never happens for the mock or `KvmBackend` post-run).
    fn current_vcpu(&self) -> VcpuState {
        match &self.saved_state {
            Some(s) => s.clone(),
            None => self.backend.save().unwrap_or_default(),
        }
    }

    /// Device + terminal state for the hash: the UART register shadows + `LCR.DLAB`
    /// **and** the latched terminal reason / debug-exit code. The serial *bytes*
    /// are hashed separately (the `SERL` chunk); the UART config captured here is
    /// the device's residual state, so two runs that drive the UART into a
    /// different register/DLAB configuration — even with byte-identical serial
    /// output — produce different hashes (their future port-I/O behavior differs).
    fn encode_device_terminal(&self) -> Vec<u8> {
        let mut v = Vec::new();
        // UART register shadows (offsets 0..=7) + the latched LCR.DLAB window.
        v.extend_from_slice(self.uart.shadow_regs());
        v.push(u8::from(self.uart.dlab()));
        // The latched terminal reason / isa-debug-exit code.
        match self.terminal {
            None => v.push(0),
            Some(TerminalReason::DebugExit { code }) => {
                v.push(1);
                v.push(code);
            }
            Some(TerminalReason::Hlt) => v.push(2),
            Some(TerminalReason::Shutdown) => v.push(3),
        }
        v
    }
}

/// Whether servicing `exit` stages a backend completion (a register-write and/or
/// RIP-advance committed on the next `KVM_RUN`): every read-style / MSR / CPUID /
/// determinism exit calls a `complete_*`. Write-style (`Io`/`Mmio` store), `Hlt`,
/// `Shutdown`, `Deadline`, and the unmodeled `Hypercall` resume with nothing pending.
/// Drives [`Vmm::completion_staged`] (restore must not run over a staged completion).
fn exit_stages_completion(exit: &Exit) -> bool {
    matches!(
        exit,
        Exit::Io { write: None, .. }
            | Exit::Mmio { write: None, .. }
            | Exit::Rdmsr { .. }
            | Exit::Wrmsr { .. }
            | Exit::Cpuid { .. }
            | Exit::Rdtsc
            | Exit::Rdtscp
            | Exit::Rdrand { .. }
            | Exit::Rdseed { .. }
    )
}

/// A modeled byte port (the 8250 UART block and isa-debug-exit) is
/// **byte-addressed**; a wider access (`size != 1`) is unmodeled by the M1/M2
/// payloads and must **fail closed** (CPU-MSR-CONTRACT default-deny), never a
/// silent `value as u8` truncation — an `outl $x, $0xF4` must not become a fake
/// debug-exit `PASS`, and a wide UART write must not drop its high bytes.
fn require_byte_io(dir: &str, port: u16, size: u8) -> Result<(), VmmError> {
    if size != 1 {
        return Err(VmmError::ContractViolation(format!(
            "{dir} to modeled byte port {port:#06x} with size {size} != 1 — the 8250 UART and \
             isa-debug-exit are byte-addressed; a wider access is unmodeled (fail closed, not a \
             truncation)"
        )));
    }
    Ok(())
}

/// The report channel ([`REPORT_PORT`]) is **dword-addressed** (`OUT …, EAX`):
/// a non-32-bit access is unmodeled and must **fail closed** (default-deny),
/// never a silent truncation/extension of a reported value — a reported value
/// rides exactly one 4-byte write, and `report(u64)` is two of them.
fn require_dword_io(dir: &str, port: u16, size: u8) -> Result<(), VmmError> {
    if size != 4 {
        return Err(VmmError::ContractViolation(format!(
            "{dir} to report port {port:#06x} with size {size} != 4 — the report channel is \
             dword-addressed (a reported value is one 32-bit OUT); a different width is unmodeled \
             (fail closed)"
        )));
    }
    Ok(())
}

/// Append a domain-tagged, length-prefixed chunk: `tag(4) ‖ len(u64 LE) ‖ bytes`.
fn put_chunk(out: &mut Vec<u8>, tag: &[u8; 4], bytes: &[u8]) {
    out.extend_from_slice(tag);
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// MSR access direction for the loud §1 log line — carries both the human
/// direction (`RDMSR`/`WRMSR`) and the KVM userspace exit reason it surfaces as.
#[derive(Clone, Copy)]
enum MsrDir {
    Read,
    Write,
}

impl MsrDir {
    fn dir(self) -> &'static str {
        match self {
            MsrDir::Read => "RDMSR",
            MsrDir::Write => "WRMSR",
        }
    }
    /// The KVM exit reason this direction surfaces as (CPU-MSR-CONTRACT §1).
    fn exit_reason(self) -> &'static str {
        match self {
            MsrDir::Read => "KVM_EXIT_X86_RDMSR",
            MsrDir::Write => "KVM_EXIT_X86_WRMSR",
        }
    }
}

/// Loud host-side log of an MSR access, emitted **before** any architectural
/// effect and never perturbing guest-visible state (CPU-MSR-CONTRACT §1
/// loud-event policy). §1 mandates the full context: access direction, the KVM
/// exit reason, the MSR index, the WRMSR data (`n/a` on a read), the guest RIP at
/// the faulting instruction, the current work counter / V-time, and the
/// disposition applied. `work` is the retired-branch counter at this exit
/// (task-21 P3): `Some(n)` on the determinism-complete path, `None` →
/// `work=unwired` when V-time is not wired (stock KVM / M1/M2) — logged honestly
/// rather than a fake `0` that would read as a real count.
fn loud_msr(
    dir: MsrDir,
    index: u32,
    data: Option<u64>,
    rip: u64,
    work: Option<u64>,
    disp: &MsrDisposition,
) {
    let data = match data {
        Some(v) => format!("{v:#x}"),
        None => "n/a".to_string(),
    };
    let work = match work {
        Some(n) => n.to_string(),
        None => "unwired".to_string(),
    };
    eprintln!(
        "[vmm-core] msr-exit dir={} exit-reason={} index={index:#x} data={data} rip={rip:#x} \
         work={work} disposition={disp:?}",
        dir.dir(),
        dir.exit_reason(),
    );
}

/// Look up the frozen CPUID entry for `(leaf, subleaf)`: an exact `(leaf,
/// subleaf)` match, else a `leaf`-only (insignificant-subleaf) match, else a
/// zeroed entry (the `cpuid-default zeroed` rule).
fn lookup_cpuid(leaf: u32, subleaf: u32) -> vmm_backend::CpuidEntry {
    let model = contract::cpuid_model();
    let mut leaf_only = None;
    for e in &model.entries {
        if e.leaf == leaf {
            if e.subleaf == subleaf {
                return *e;
            }
            if !e.subleaf_significant {
                leaf_only = Some(*e);
            }
        }
    }
    leaf_only.unwrap_or(vmm_backend::CpuidEntry {
        leaf,
        subleaf,
        ..Default::default()
    })
}

/// Deterministic, fixed-layout encoding of the V-time + seeded-RNG state for the
/// `VTIM` hash chunk: the clock-rate config (4 × `u64` LE — `ratio_num`, `tsc_hz`,
/// `tsc_base`, `tsc_adjust`), then the **single canonical effective-V-time field**
/// (`u64` LE), then the entropy stream position (`SeededEntropy::save_state`, the
/// trailing bytes — the enclosing chunk is length-prefixed by `put_chunk`). A change
/// in seed, ratio, `tsc_hz`/`tsc_base`, `tsc_adjust` (`IA32_TSC_ADJUST`), effective
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
        vt.cfg.tsc_hz,
        vt.cfg.tsc_base,
        vt.tsc_adjust,
    ] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.extend_from_slice(&vt.clock.snapshot_vns(vt.last_intercept_work).to_le_bytes());
    v.extend_from_slice(&vt.entropy.save_state());
    v
}

/// Deterministic, fixed-layout encoding of an xAPIC [`lapic::LapicState`] for the
/// `LAPC` hash chunk: every field little-endian in declaration order (all plain
/// `u32`/`u64`/`[u32; N]`/`bool` POD — no map iteration, no float). A change in any
/// register, the timer bookkeeping, or the armed/pending flags changes the hash.
fn encode_lapic_state(s: &lapic::LapicState) -> Vec<u8> {
    let mut v = Vec::new();
    for x in [s.version, s.id] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.extend_from_slice(&s.timer_hz.to_le_bytes());
    for x in [
        s.tpr,
        s.svr,
        s.ldr,
        s.dfr,
        s.esr,
        s.icr_low,
        s.icr_high,
        s.divide_config,
    ] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    for word in s.isr.iter().chain(&s.tmr).chain(&s.irr).chain(&s.lvt) {
        v.extend_from_slice(&word.to_le_bytes());
    }
    v.extend_from_slice(&s.initial_count.to_le_bytes());
    v.extend_from_slice(&s.count_at_arm.to_le_bytes());
    v.extend_from_slice(&s.timer_arm_vns.to_le_bytes());
    v.push(u8::from(s.timer_running));
    v.push(u8::from(s.timer_pending));
    v
}

/// Deterministic, fixed-layout encoding of a `VcpuState` (no map iteration into
/// bytes beyond the already-sorted `BTreeMap`; no float; no host clock).
fn encode_vcpu_state(s: &VcpuState) -> Vec<u8> {
    let mut v = Vec::new();
    let r = &s.regs;
    for x in [
        r.rax, r.rbx, r.rcx, r.rdx, r.rsi, r.rdi, r.rsp, r.rbp, r.r8, r.r9, r.r10, r.r11, r.r12,
        r.r13, r.r14, r.r15, r.rip, r.rflags,
    ] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    for seg in [
        &s.sregs.cs,
        &s.sregs.ds,
        &s.sregs.es,
        &s.sregs.fs,
        &s.sregs.gs,
        &s.sregs.ss,
        &s.sregs.tr,
        &s.sregs.ldt,
    ] {
        encode_segment(&mut v, seg);
    }
    for dt in [&s.sregs.gdt, &s.sregs.idt] {
        v.extend_from_slice(&dt.base.to_le_bytes());
        v.extend_from_slice(&dt.limit.to_le_bytes());
    }
    for cr in [
        s.sregs.cr0,
        s.sregs.cr2,
        s.sregs.cr3,
        s.sregs.cr4,
        s.sregs.cr8,
        s.sregs.efer,
        s.sregs.apic_base,
        s.sregs.flags,
    ] {
        v.extend_from_slice(&cr.to_le_bytes());
    }
    for p in s.sregs.pdptrs {
        v.extend_from_slice(&p.to_le_bytes());
    }
    v.extend_from_slice(&s.xcr0.to_le_bytes());
    for d in s.debugregs.db {
        v.extend_from_slice(&d.to_le_bytes());
    }
    v.extend_from_slice(&s.debugregs.dr6.to_le_bytes());
    v.extend_from_slice(&s.debugregs.dr7.to_le_bytes());
    v.extend_from_slice(&s.debugregs.flags.to_le_bytes());
    encode_events(&mut v, &s.events);
    v.push(match s.mp_state {
        vmm_backend::MpState::Runnable => 0,
        vmm_backend::MpState::Halted => 1,
    });
    // MSRs: BTreeMap iterates in ascending key order (deterministic).
    v.extend_from_slice(&(s.msrs.len() as u64).to_le_bytes());
    for (idx, val) in &s.msrs {
        v.extend_from_slice(&idx.to_le_bytes());
        v.extend_from_slice(&val.to_le_bytes());
    }
    v.extend_from_slice(&(s.xsave.len() as u64).to_le_bytes());
    v.extend_from_slice(&s.xsave);
    v
}

fn encode_segment(v: &mut Vec<u8>, seg: &vmm_backend::Segment) {
    v.extend_from_slice(&seg.base.to_le_bytes());
    v.extend_from_slice(&seg.limit.to_le_bytes());
    v.extend_from_slice(&seg.selector.to_le_bytes());
    // An **unusable** segment's `type` (and the rest of its access-rights byte) is
    // architecturally **don't-care**: the CPU never consults the descriptor cache of a
    // segment whose unusable bit is set (SDM Vol. 3 §24.4.1 — the VMX "unusable"
    // attribute means the segment is treated as absent; the hidden type/attr bits are
    // ignored on every use). KVM **normalizes** it (a `KVM_GET` of an unusable segment
    // reports `type = 0`, but after `KVM_SET_SREGS` a `KVM_GET` reports `type = 1`), so
    // a snapshot/restore round-trip otherwise perturbs this don't-care field and breaks
    // restore-transparency on `state_hash`. Canonicalize it to `0` so the hash reflects
    // only architecturally-meaningful state. Golden-safe: every live-`KVM_GET` value
    // already reports `type = 0` for unusable segments, so no existing (relative) golden
    // moves; the only effect is making a restored unusable segment hash like a live one.
    let type_ = if seg.unusable != 0 { 0 } else { seg.type_ };
    v.extend_from_slice(&[
        type_,
        seg.present,
        seg.dpl,
        seg.db,
        seg.s,
        seg.l,
        seg.g,
        seg.avl,
        seg.unusable,
    ]);
}

/// Encode the pending-event state into the `state_hash` in **canonical** form
/// ([`snapshot::canonical_events`]): an inert `kvm_vcpu_events` modifier residual KVM
/// leaves set when its active bit is clear (a stale `interrupt.nr`/`exception.nr`, the
/// GET-only validity `flags` bits) has **no architectural effect** — the VM-entry
/// interruption-information / exception fields are consumed only when their valid bit is
/// set (SDM Vol. 3 §24.8.3, §26.5). Hashing the canonical form makes a restored VM
/// (whose events were canonicalized at restore for soundness — see
/// [`snapshot::canonical_events`]) hash **identically** to a never-restored VM at the
/// same point, so restore-transparency holds on the full `state_hash`. Determinism is
/// unaffected (canonical is a pure function; two same-seed runs share identical raw
/// events ⇒ identical canonical), and it is golden-safe (the M1/M2/corpus paths carry
/// all-zero events ⇒ canonical == raw; the Linux paths' goldens are relative
/// deterministic-twice checks, so no pinned value moves).
fn encode_events(v: &mut Vec<u8>, raw: &vmm_backend::VcpuEvents) {
    let e = &snapshot::canonical_events(raw);
    v.extend_from_slice(&[
        e.exception_injected,
        e.exception_nr,
        e.exception_has_error_code,
        e.exception_pending,
    ]);
    v.extend_from_slice(&e.exception_error_code.to_le_bytes());
    v.push(e.exception_has_payload);
    v.extend_from_slice(&e.exception_payload.to_le_bytes());
    v.extend_from_slice(&[
        e.interrupt_injected,
        e.interrupt_nr,
        e.interrupt_soft,
        e.interrupt_shadow,
        e.nmi_injected,
        e.nmi_pending,
        e.nmi_masked,
    ]);
    v.extend_from_slice(&e.sipi_vector.to_le_bytes());
    v.extend_from_slice(&e.flags.to_le_bytes());
    v.extend_from_slice(&[
        e.smi_smm,
        e.smi_pending,
        e.smi_inside_nmi,
        e.smi_latched_init,
        e.triple_fault_pending,
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn configured_mock(exits: Vec<Exit>) -> MockBackend {
        let mut m = MockBackend::with_exits(exits);
        m.set_cpuid(&CpuidModel::default()).expect("set_cpuid");
        m.set_msr_filter(&MsrFilter::default())
            .expect("set_msr_filter");
        m
    }

    /// A `Vmm<MockBackend>` with the determinism path wired (clock + work + seed).
    fn vtime_vmm(exits: Vec<Exit>, work: Box<dyn WorkSource>, seed: u64) -> Vmm<MockBackend> {
        let mut vmm = Vmm::new(configured_mock(exits), GuestRam::new(0x1000).unwrap());
        vmm.wire_vtime(VtimeWiring::new(contract_vclock_config(), work, seed).unwrap());
        vmm
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

    #[test]
    fn rdtsc_completes_with_vtime_tsc_not_host() {
        // work = 10 → vns = 10 (ratio 1:1) → tsc = floor(10 * 2GHz/1e9) = 20.
        let mut vmm = vtime_vmm(
            vec![Exit::Rdtsc, Exit::Hlt],
            Box::new(ScriptedWork::at(10)),
            1,
        );
        assert!(vmm.vtime_wired(), "wire_vtime reports the path as wired");
        let r = vmm.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Hlt);
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
            configured_mock(vec![Exit::Rdtsc, Exit::Rdtsc, Exit::Hlt]),
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
        assert_eq!(r.reason, TerminalReason::Hlt);
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
            vec![Exit::Rdtscp, Exit::Hlt],
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
            vec![Exit::Rdtsc, Exit::Rdtsc, Exit::Rdtsc, Exit::Hlt],
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
                Exit::Rdrand { width: 8 },
                Exit::Rdseed { width: 4 },
                Exit::Hlt,
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
            configured_mock(vec![Exit::Rdtsc]),
            GuestRam::new(0x1000).unwrap(),
        );
        assert!(matches!(tsc.step(), Err(VmmError::ContractViolation(_))));
        let mut rng = Vmm::new(
            configured_mock(vec![Exit::Rdrand { width: 8 }]),
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
            vec![Exit::Rdrand { width: 8 }, Exit::Rdtsc],
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
            vec![Exit::Rdtsc, Exit::Rdrand { width: 8 }],
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
            vec![Exit::Rdrand { width: 8 }, Exit::Rdtsc],
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
                Exit::Rdtsc,
                Exit::Io {
                    port: 0x3F8, // UART THR — a non-V-time exit (Continued)
                    size: 1,
                    write: Some(u32::from(b'x')),
                },
                Exit::Rdtsc,
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
        let mut v = vtime_vmm(vec![Exit::Rdtsc], Box::new(ScriptedWork::at(10)), 1);
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
            tsc_adjust: 0,
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
            tsc_adjust: snap0.tsc_adjust,
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
            vec![Exit::Rdrand { width: 8 }, Exit::Rdtsc],
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
    /// `visible_tsc` (`VClock::tsc` + the default-0 `IA32_TSC_ADJUST`) — and it is
    /// deterministic-twice. (Previously this aborted with a stale "V-time is not
    /// wired in this skeleton" `ContractViolation`.)
    #[test]
    fn rdmsr_ia32_tsc_matches_rdtsc_instruction_and_is_deterministic() {
        const WORK: u64 = 21; // vns(21)=21 → tsc = floor(21·2GHz/1e9) = 42.
        let run_msr = || {
            let mut v = vtime_vmm(
                vec![Exit::Rdmsr { index: 0x10 }, Exit::Hlt],
                Box::new(ScriptedWork::at(WORK)),
                1,
            );
            v.run().unwrap();
            v
        };
        let mut insn = vtime_vmm(
            vec![Exit::Rdtsc, Exit::Hlt],
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
        // ScriptedWork fixed at work=10 → base V-time TSC = VClock::tsc(10) = 20.
        let mut vmm = vtime_vmm(
            vec![
                Exit::Wrmsr {
                    index: 0x3b,
                    value: 1000,
                }, // IA32_TSC_ADJUST = 1000
                Exit::Rdmsr { index: 0x10 }, // IA32_TSC = 20 + 1000 = 1020
                Exit::Rdmsr { index: 0x3b }, // IA32_TSC_ADJUST = 1000
                Exit::Wrmsr {
                    index: 0x10,
                    value: 7777,
                }, // IA32_TSC = 7777 → adjust = 7777 − 20 = 7757
                Exit::Rdmsr { index: 0x10 }, // IA32_TSC = 7777
                Exit::Rdmsr { index: 0x3b }, // IA32_TSC_ADJUST = 7757
                Exit::Hlt,
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
                vec![Exit::Wrmsr {
                    index: 0x3b,
                    value: adjust,
                }],
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
                vec![Exit::Rdmsr { index: 0x3b }],
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
                Exit::Wrmsr {
                    index: 0x3b,
                    value: 9,
                }, // tsc_adjust = 9
                Exit::Wrmsr {
                    index: 0x3b,
                    value: 99,
                }, // tsc_adjust = 99
                Exit::Rdmsr { index: 0x3b }, // reads back the restored adjust
            ],
            Box::new(ScriptedWork::at(0)),
            1,
        );
        v.step().unwrap(); // WRMSR(0x3b, 9) → tsc_adjust = 9
        let snap = v
            .save_vtime()
            .expect("save with non-zero adjust succeeds")
            .expect("wired");
        assert_eq!(snap.tsc_adjust, 9, "snapshot must capture IA32_TSC_ADJUST");
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
                configured_mock(vec![Exit::Rdmsr { index: idx }]),
                GuestRam::new(0x1000).unwrap(),
            );
            assert!(matches!(rd.step(), Err(VmmError::ContractViolation(_))));
            let mut wr = Vmm::new(
                configured_mock(vec![Exit::Wrmsr {
                    index: idx,
                    value: 0,
                }]),
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
        // collide with `a`): `ratio_num`/`tsc_hz`/`tsc_base` are hashed directly,
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
                "tsc_hz",
                vtime::VClockConfig {
                    tsc_hz: 3_000_000_000,
                    ..base
                },
            ),
            (
                "tsc_base",
                vtime::VClockConfig {
                    tsc_base: 5,
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
                configured_mock(vec![Exit::Rdtsc]),
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

    fn report_out(value: u32) -> Exit {
        Exit::Io {
            port: REPORT_PORT,
            size: 4,
            write: Some(value),
        }
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
                Exit::Hlt,
            ]),
            GuestRam::new(0x1000).unwrap(),
        );
        let r = vmm.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Hlt);
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
                configured_mock(vec![Exit::Io {
                    port: REPORT_PORT,
                    size: bad_size,
                    write: Some(0xAB),
                }]),
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
            loud.uart.write(crate::devices::UART_PORT_BASE, byte);
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
                vec![Exit::Rdrand { width }, Exit::Hlt],
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
            configured_mock(vec![Exit::Rdtsc, Exit::Hlt]),
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
                configured_mock(vec![Exit::Rdtsc, Exit::Hlt]),
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
            configured_mock(vec![Exit::Rdtsc]),
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
            tsc_adjust: 0,
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
                vec![Exit::Rdrand { width: 8 }],
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
    fn linux_vmm(exits: Vec<Exit>) -> Vmm<MockBackend> {
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
            Exit::Mmio {
                gpa: Gpa(0xFEE0_0030),
                size: 4,
                write: None,
            },
            Exit::Mmio {
                gpa: Gpa(0xFEE0_00B0),
                size: 4,
                write: Some(0),
            }, // EOI store
            Exit::Hlt,
        ]);
        assert!(v.lapic_wired());
        let r = v.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Hlt);
        assert_eq!(
            v.backend.completions(),
            &[Completion::Read(u64::from(lapic::APIC_VERSION_VALUE))]
        );

        // Unwired (M1/M2): any MMIO is a loud contract violation, never serviced.
        let mut stock = Vmm::new(
            configured_mock(vec![Exit::Mmio {
                gpa: Gpa(0xFEE0_0030),
                size: 4,
                write: None,
            }]),
            GuestRam::new(0x1000).unwrap(),
        );
        assert!(!stock.lapic_wired(), "stock Vmm has no xAPIC wired");
        assert!(matches!(stock.step(), Err(VmmError::ContractViolation(_))));
    }

    #[test]
    fn mmio_outside_apic_page_fails_closed_even_on_linux_path() {
        // A non-xAPIC MMIO address is unmodeled and fails closed even with the
        // Linux platform wired (the xAPIC page is the only modeled MMIO).
        let mut v = linux_vmm(vec![Exit::Mmio {
            gpa: Gpa(0xFEB0_0000),
            size: 4,
            write: None,
        }]);
        assert!(matches!(v.step(), Err(VmmError::ContractViolation(_))));
    }

    #[test]
    fn legacy_io_serviced_only_when_wired() {
        // Wired: OUT to the PCI CONFIG_ADDRESS latch, then IN from CONFIG_DATA reads
        // "no device" (all-ones).
        let mut v = linux_vmm(vec![
            Exit::Io {
                port: 0x0CF8,
                size: 4,
                write: Some(0x8000_0000),
            },
            Exit::Io {
                port: 0x0CFC,
                size: 4,
                write: None,
            },
            Exit::Hlt,
        ]);
        v.run().expect("run");
        assert_eq!(v.backend.completions(), &[Completion::Read(0xFFFF_FFFF)]);

        // Unwired: the same legacy port OUT is a contract violation.
        let mut stock = Vmm::new(
            configured_mock(vec![Exit::Io {
                port: 0x0CF8,
                size: 4,
                write: Some(0),
            }]),
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
            let mut v = linux_vmm(vec![Exit::Io {
                port: 0x0CF8,
                size: 4,
                write: Some(addr),
            }]);
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
            Exit::Io {
                port: 0x3F8,
                size: 1,
                write: Some(u32::from(b'H')),
            },
            Exit::Io {
                port: 0x3F8,
                size: 1,
                write: Some(u32::from(b'i')),
            },
            Exit::Hlt,
        ]);
        v.run().expect("run");
        assert_eq!(v.serial(), b"Hi");
        assert!(v.exit_counts().io >= 2, "exit_counts reflects the IO exits");
    }

    #[test]
    fn mmio_just_past_apic_page_fails_closed() {
        // An access one page above the xAPIC base is outside the modeled page → a
        // loud contract violation (pins the `..APIC_MMIO_END` upper bound).
        let mut v = linux_vmm(vec![Exit::Mmio {
            gpa: Gpa(0xFEE0_1000),
            size: 4,
            write: None,
        }]);
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
                Exit::Mmio {
                    gpa: Gpa(0xFEE0_00F0),
                    size: 4,
                    write: Some(0x1FF),
                }, // SVR: enable
                Exit::Mmio {
                    gpa: Gpa(0xFEE0_0320),
                    size: 4,
                    write: Some(0x40),
                }, // LVT timer: unmasked oneshot, vec 0x40
                Exit::Mmio {
                    gpa: Gpa(0xFEE0_0380),
                    size: 4,
                    write: Some(0xFFFF_FFFF),
                }, // TMICT: arm at now=0
                Exit::Rdtsc, // V-time intercept → last_intercept_work = W
                Exit::Mmio {
                    gpa: Gpa(0xFEE0_0390),
                    size: 4,
                    write: None,
                }, // read TMCCT at now=W
                Exit::Hlt,
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
        let mut modified = linux_vmm(vec![Exit::Mmio {
            gpa: Gpa(0xFEE0_0080),
            size: 4,
            write: Some(0x20),
        }]);
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
    fn configured_stock_mock(exits: Vec<Exit>) -> MockBackend {
        let mut m = MockBackend::with_capabilities(vmm_backend::Capabilities {
            name: "mock-stock",
            deterministic_tsc: false,
            deterministic_rng: false,
            enforces_tsc_deadline_msr: false,
        });
        m.extend_exits(exits);
        m.set_cpuid(&CpuidModel::default()).expect("set_cpuid");
        m.set_msr_filter(&MsrFilter::default())
            .expect("set_msr_filter");
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
    fn arm_timer_exits(initial_count: u64) -> Vec<Exit> {
        let w = |off: u64, v: u64| Exit::Mmio {
            gpa: Gpa(APIC_MMIO_BASE + off),
            size: 4,
            write: Some(v),
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
        exits.push(Exit::Rdtsc); // V-time intercept → last_intercept_work = W
        exits.push(read_mmio(isr_gpa(0x40))); // B: anchor = W → delivered
        exits.push(Exit::Hlt);
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
        exits.push(Exit::Hlt);
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
            Step::Terminal(TerminalReason::Hlt)
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
        // `run_until` (busy-spin preemption). An `Exit::Deadline` advances the
        // skid-free anchor to the reached work, so the NEXT entry fires the timer
        // into the IRR and delivers it (IRR→ISR on acceptance). A guest that never
        // exits on its own thus still observes the timer.
        let mut exits = arm_timer_exits(1000);
        // The mock rewrites `reached` to the deadline the VMM passed `run_until`
        // (= work_for_vns(timer deadline)); the literal here is a placeholder.
        exits.push(Exit::Deadline { reached: Vtime(0) });
        exits.push(read_mmio(isr_gpa(0x40))); // after delivery: vector 0x40 in service
        exits.push(Exit::Hlt);
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

    /// P1 round-13 — the comprehensive zero-step invariant: a `run_until` that returns
    /// `Exit::Deadline` WITHOUT entering the guest (the overdue/at-deadline path, no
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
        exits.push(Exit::Rdrand { width: 8 }); // stages a completion (RNG: both guards)
        exits.push(Exit::Deadline { reached: Vtime(0) }); // mock rewrites reached := deadline
        exits.push(Exit::Hlt); // a real entry that commits the staged completion
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
            Step::Terminal(TerminalReason::Hlt)
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
            configured_mock(vec![Exit::Rdtsc, Exit::Rdtsc, Exit::Hlt]),
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
            SaveFailBackend(configured_mock(vec![Exit::Rdtsc, Exit::Rdtsc, Exit::Hlt])),
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
        let tpr_write = Exit::Mmio {
            gpa: Gpa(APIC_MMIO_BASE + u64::from(lapic::APIC_TPR)),
            size: 4,
            write: Some(0xF0), // TPR class 0xF masks vector 0x40 (class 4)
        };
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Rdtsc); // advance anchor → timer fires next service (peek 0x40)
        exits.push(tpr_write); // guest raises TPR while 0x40 waits on the window
        exits.push(read_mmio(irr_gpa(0x40))); // 0x40 still pending in IRR
        exits.push(Exit::Hlt);
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
            Step::Terminal(TerminalReason::Hlt)
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
            configured_mock(vec![Exit::Hlt]),
            GuestRam::new(0x1000).unwrap(),
        );
        assert!(!v.lapic_wired());
        assert!(matches!(
            v.step().unwrap(),
            Step::Terminal(TerminalReason::Hlt)
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
    const UNMASK_IRQ4: Exit = Exit::Io {
        port: 0x0021,
        size: 1,
        write: Some(0xEF),
    }; // 0xFF & !(1 << 4)
    /// Enable IER.THRI (port 0x3F9, IER = 0x3F8+1) — the kernel's `start_tx`.
    const ENABLE_THRI: Exit = Exit::Io {
        port: 0x03F9,
        size: 1,
        write: Some(0x02),
    };

    #[test]
    fn serial_thre_interrupt_injects_com1_vector() {
        // The Linux userspace TX path: the guest unmasks IRQ 4 in the 8259 and
        // enables IER.THRI; the VMM then injects the COM1 vector (0x34) so the
        // kernel's IRQ-4 handler can drain the TX. Deterministic (edge-driven by the
        // IER write, no V-time), so it works on the deterministic backend at work 0.
        let mut mock = configured_mock(vec![UNMASK_IRQ4, ENABLE_THRI, Exit::Hlt]);
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
            Step::Terminal(TerminalReason::Hlt)
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
        let mut mock = configured_mock(vec![ENABLE_THRI, Exit::Hlt]);
        mock.set_defer_accept(true);
        let mut v = lapic_vmm(mock, Box::new(ScriptedWork::at(0)));
        assert!(matches!(v.step().unwrap(), Step::Continued)); // IER = THRI
        assert!(matches!(
            v.step().unwrap(),
            Step::Terminal(TerminalReason::Hlt)
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
        exits.push(Exit::Rdtsc); // advance the anchor → the timer fires into IRR
        exits.push(Exit::Hlt);
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
        exits.push(Exit::Hlt);
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
    fn read_mmio(gpa: Gpa) -> Exit {
        Exit::Mmio {
            gpa,
            size: 4,
            write: None,
        }
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
        exits.push(Exit::Rdtsc); // advance the anchor so the timer fires next service
        exits.push(read_mmio(irr_gpa(0x40))); // IRR bank for vec 0x40
        exits.push(read_mmio(isr_gpa(0x40))); // ISR bank for vec 0x40
        exits.push(Exit::Hlt);
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
        exits.push(Exit::Rdtsc);
        exits.push(read_mmio(irr_gpa(0x40)));
        exits.push(read_mmio(isr_gpa(0x40)));
        exits.push(Exit::Hlt);
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
        exits.push(Exit::Hlt);
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
        // moves ONLY the idle accumulator (vns_base); it fabricates ZERO retired
        // branches AND — the task-52 review fix — does NOT read the live (skid-tainted)
        // HLT work counter, so the skid-free anchor (last_intercept_work) is unmoved
        // and the point is left un-synchronized.
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Hlt); // the guest idles, waiting for the timer
        exits.push(read_mmio(isr_gpa(0x40))); // after delivery: 0x40 in service
        exits.push(Exit::Hlt); // one-shot fired → no timer armed → terminal
        let mut mock = configured_mock(exits);
        mock.set_state(if_set_state());
        // No intercept before the HLT, so the skid-free anchor is the initial 0.
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
        // Skid-free invariant: the anchor is UNMOVED (still the last real intercept, 0)
        // and the point is NOT marked synchronized — no live HLT read entered V-time.
        let vt = v.vtime.as_ref().unwrap();
        assert_eq!(
            vt.last_intercept_work, 0,
            "the anchor stays at the last skid-free intercept (not a HLT read)"
        );
        assert!(
            !v.vtime_synchronized,
            "the HLT is not a skid-free intercept, so it is NOT marked synchronized"
        );
        // The jump landed the clock at D via the idle accumulator alone.
        assert_eq!(
            v.vtime.as_ref().unwrap().clock.snapshot_vns(0),
            deadline,
            "V-time jumped to D via the idle accumulator (vns_base), not by executing"
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
            Step::Terminal(TerminalReason::Hlt)
        ));
    }

    #[test]
    fn idle_hlt_without_if_is_terminal() {
        // IF==0 (the kernel's final `cli; hlt`): terminal even with a timer armed
        // — a wait nothing will satisfy. The byte-identical existing behavior.
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Hlt);
        // Default mock state: rflags == 0 (IF clear).
        let mut v = lapic_vmm(configured_mock(exits), Box::new(ScriptedWork::at(1000)));
        let r = v.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Hlt);
        assert!(
            v.idle_landings().is_empty(),
            "an IF==0 HLT is terminal, never resumed"
        );
    }

    #[test]
    fn hlt_without_armed_timer_is_terminal_even_with_if() {
        // IF==1 but no timer armed (LAPIC wired, never programmed): terminal. The
        // no-timer gate short-circuits before the RFLAGS read.
        let mut mock = configured_mock(vec![Exit::Hlt]);
        mock.set_state(if_set_state());
        let mut v = lapic_vmm(mock, Box::new(ScriptedWork::at(0)));
        let r = v.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Hlt);
        assert!(v.idle_landings().is_empty(), "no armed timer ⇒ terminal");
    }

    #[test]
    fn idle_hlt_on_stock_backend_is_terminal() {
        // Stock backend (no deterministic counter): never idle-resumes, even with
        // IF==1 and a timer armed — the determinism gate (deterministic_tsc)
        // returns no deadline, so the HLT stays terminal (Phase B.1 unchanged).
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Hlt);
        let mut mock = configured_stock_mock(exits);
        mock.set_state(if_set_state());
        let mut v = lapic_vmm(mock, Box::new(ScriptedWork::at(0)));
        let r = v.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Hlt);
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
        let w = |off: u64, val: u64| Exit::Mmio {
            gpa: Gpa(APIC_MMIO_BASE + off),
            size: 4,
            write: Some(val),
        };
        let undeliverable_timer_hlt_terminates = |setup: Vec<Exit>| {
            let mut exits = setup;
            exits.push(Exit::Hlt);
            let mut mock = configured_mock(exits);
            mock.set_state(if_set_state());
            let mut v = lapic_vmm(mock, Box::new(ScriptedWork::at(0)));
            let r = v.run().expect("run");
            assert_eq!(
                r.reason,
                TerminalReason::Hlt,
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
    fn overdue_idle_resume_is_a_zero_jump() {
        // The overdue branch of `resume_idle`: when the (skid-free) anchor's V-time is
        // already at/after the deadline, the planner returns a zero jump — the clock
        // does not move (never backward) and the landing records the current V-time.
        // Unit-tested directly: in the full loop an anchor past D would already have
        // fired the timer (so the timer disarms before a HLT), making this the
        // planner's robustness edge rather than a naturally-reached state.
        let mut v = vtime_vmm(vec![Exit::Hlt], Box::new(ScriptedWork::at(0)), 1);
        // Establish a skid-free anchor at work 5000 (vns 5000 at ratio 1:1).
        v.vtime.as_mut().unwrap().last_intercept_work = 5000;
        // Mimic the step() top, which clears the synchronized flag before the entry.
        v.vtime_synchronized = false;

        // Deadline already in the past relative to the anchor.
        assert!(matches!(v.resume_idle(4000), Step::Continued));

        let vt = v.vtime.as_ref().unwrap();
        assert_eq!(
            vt.clock.snapshot_vns(5000),
            5000,
            "zero jump: an overdue deadline does not move the clock (no backward motion)"
        );
        assert_eq!(vt.last_intercept_work, 5000, "anchor unmoved");
        assert_eq!(
            v.idle_landings(),
            &[5000],
            "the landing records the current (unchanged) V-time"
        );
        assert!(
            !v.vtime_synchronized,
            "resume_idle never marks the HLT synchronized"
        );
    }

    #[test]
    fn idle_discriminator_save_error_fails_closed() {
        // The RFLAGS read for the idle/terminal discriminator is a backend save;
        // a save error must fail closed (VmmError::Backend), never guess the
        // disposition (which would risk a wrong terminate/resume).
        let mut exits = arm_timer_exits(1000);
        exits.push(Exit::Hlt);
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
        let w = |off: u64, val: u64| Exit::Mmio {
            gpa: Gpa(APIC_MMIO_BASE + off),
            size: 4,
            write: Some(val),
        };
        let exits = vec![
            w(u64::from(lapic::APIC_SVR), 0x1FF),
            w(u64::from(lapic::APIC_LVT_TIMER), 0x40 | (1 << 17)), // periodic
            w(u64::from(lapic::APIC_TMICT), 1000),
            Exit::Hlt,                        // idle #1
            w(u64::from(lapic::APIC_EOI), 0), // the timer ISR EOIs (retires 0x40 from ISR)
            Exit::Hlt,                        // idle #2 (re-armed period; deliverable again)
        ];
        let mut mock = configured_mock(exits);
        mock.set_state(if_set_state());
        // No intercept ever fires here, so the skid-free anchor stays at the initial 0;
        // the work source is never consulted by the idle path (skid-free by construction).
        let mut v = lapic_vmm(mock, Box::new(ScriptedWork::at(0)));

        for _ in 0..3 {
            assert!(matches!(v.step().unwrap(), Step::Continued));
        }
        let period = v.preemption_deadline().expect("armed").0;

        // Idle #1 → jump to one period (V-time at the unchanged anchor 0 reads `period`).
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
            exits.push(Exit::Rdtsc); // intercept → skid-free anchor (identical both runs)
            exits.push(Exit::Hlt); // idle: the live work read here is skid-tainted
            exits.push(Exit::Rdtsc); // W_next intercept — where a folded skid would surface
            exits.push(Exit::Hlt); // terminal (one-shot already fired)
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
            assert_eq!(reason, TerminalReason::Hlt);
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
    fn full_vmm(state: VcpuState, exits: Vec<Exit>, work_at: u64, seed: u64) -> Vmm<MockBackend> {
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
    fn mutate_exits() -> Vec<Exit> {
        vec![
            Exit::Wrmsr {
                index: 0x3b,
                value: 0x1234,
            },
            Exit::Mmio {
                gpa: Gpa(0xFEE0_0080),
                size: 4,
                write: Some(0x20),
            }, // TPR = 0x20
            Exit::Io {
                port: 0x0021,
                size: 1,
                write: Some(0xEF),
            }, // PIC master IMR
            Exit::Io {
                port: 0x3F8,
                size: 1,
                write: Some(u32::from(b'H')),
            }, // serial 'H'
            Exit::Rdrand { width: 8 }, // advance the entropy stream
            Exit::Rdtsc,               // V-time intercept → clean, synchronized boundary
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
        assert_eq!(s.contract_hash, crate::contract::contract_hash());
    }

    #[test]
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
            vec![Exit::Rdtsc, Exit::Rdrand { width: 8 }],
            700,
            0xDEAD, // overwritten by the restored stream
        );
        b.restore_vm_state(&s).unwrap();
        b.step().unwrap(); // RDTSC at reset work=0 → visible = 2*vns_base + tsc_adjust
        b.step().unwrap(); // RDRAND → the word AFTER A's first draw

        // visible_tsc = VClock::tsc(0) [= 2 * vns_base = 1000] + IA32_TSC_ADJUST
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
        let mut rng = full_vmm(VcpuState::default(), vec![Exit::Rdrand { width: 8 }], 10, 1);
        rng.step().unwrap();
        assert!(matches!(
            rng.save_vm_state(),
            Err(VmmError::ContractViolation(_))
        ));
        // Non-synchronized: a UART OUT after an RDTSC desynchronizes ⇒ refuse.
        let mut io = full_vmm(
            VcpuState::default(),
            vec![
                Exit::Rdtsc,
                Exit::Io {
                    port: 0x3F8,
                    size: 1,
                    write: Some(u32::from(b'x')),
                },
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
            vec![Exit::Rdrand { width: 8 }],
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
        bad.vtime.tsc_hz += 1;
        reject(&bad, "tsc_hz");
        let mut bad = s.clone();
        bad.vtime.tsc_base += 1;
        reject(&bad, "tsc_base");
    }

    #[test]
    fn restore_into_unwired_vm_rejects_a_vtime_bearing_blob() {
        // A V-time-wired (no-LAPIC) source yields a blob carrying a live V-time
        // block; restoring it into a VM with no V-time wired is refused (wiring must
        // match the snapshot source). Both the tsc_hz and the snapshot_vns disjuncts
        // are pinned individually.
        let mut a = vtime_vmm(vec![Exit::Rdtsc], Box::new(ScriptedWork::at(5)), 1);
        a.step().unwrap();
        let s = a.save_vm_state().unwrap();
        assert!(
            s.vtime.tsc_hz != 0,
            "source blob carries a live V-time block"
        );

        let mut only_hz = s.clone();
        only_hz.vtime.snapshot_vns = 0; // tsc_hz still nonzero
        let mut stock1 = Vmm::new(configured_mock(vec![]), GuestRam::new(0x1000).unwrap());
        assert!(matches!(
            stock1.restore_vm_state(&only_hz),
            Err(VmmError::ContractViolation(_))
        ));

        let mut only_vns = s.clone();
        only_vns.vtime.tsc_hz = 0;
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
        fn set_cpuid(&mut self, m: &CpuidModel) -> vmm_backend::Result<()> {
            self.0.set_cpuid(m)
        }
        fn set_msr_filter(&mut self, f: &MsrFilter) -> vmm_backend::Result<()> {
            self.0.set_msr_filter(f)
        }
        unsafe fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> vmm_backend::Result<()> {
            // SAFETY: forwards to the inner mock, which only records the region
            // (no dereference); this adds no obligation beyond the trait contract.
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
        fn capabilities(&self) -> vmm_backend::Capabilities {
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
    fn restore_vm_state_rearms_the_first_entry_work_prepare() {
        // A restored VM is a **fresh spawn** for the work counter: the next step must
        // re-run `WorkSource::start_run` (the per-VM baseline) — else, on the shared
        // perf counter, a coexisting VM's branches between restore and entry would be
        // miscounted into the restored V-time. Resetting `first_entry_done` makes
        // `start_run` fire again. (The serial-OUT steps stage no completion, so the
        // restore is at a clean boundary — see the staged-completion guard.)
        let starts = std::rc::Rc::new(Cell::new(0u32));
        let out = |b: u8| Exit::Io {
            port: 0x3F8,
            size: 1,
            write: Some(u32::from(b)),
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
        let mut tgt = full_vmm(VcpuState::default(), vec![Exit::Rdtsc], 10, 1);
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
        const W: u64 = 100_000_000;
        // Quiescent: a wired LAPIC with no timer programmed → nothing pending in the IRR.
        let mut q = lapic_vmm(
            configured_mock(vec![Exit::Rdtsc, Exit::Hlt]),
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
        exits.push(Exit::Rdtsc);
        exits.push(Exit::Rdtsc);
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
        exits.push(Exit::Rdtsc); // advance the anchor + synchronize (no vector yet)
        exits.push(Exit::Rdtsc); // service fires 0x40 into IRR + sets pending; re-sync
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
        let mut bmock = configured_mock(vec![Exit::Rdtsc, Exit::Hlt]);
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
                Exit::Io {
                    port: 0x3FB,
                    size: 1,
                    write: Some(0x80),
                }, // LCR: DLAB = 1
                Exit::Io {
                    port: 0x3F9,
                    size: 1,
                    write: Some(0x07),
                }, // offset+1 under DLAB ⇒ DLM = 7
                Exit::Rdtsc, // re-synchronize for the save
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
            vec![Exit::Mmio {
                gpa: Gpa(0xFEE0_0080),
                size: 4,
                write: Some(0x30),
            }],
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
}
