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
use vmm_backend::{Backend, Exit, Gpa, VcpuState};
use vtime::{VClock, VClockConfig};

use crate::contract::{self, MsrDisposition};
use crate::devices::{ISA_DEBUG_EXIT_PORT, LegacyPlatform, REPORT_PORT, Uart8250};
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
    /// read. Reset to `0` by [`Vmm::restore_vtime`] (the counter restarts at 0 and
    /// the effective V-time moves into `vns_base`). Starts at `0`: before the first
    /// intercept the effective V-time is exactly `vns_base`.
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
    /// (`snapshot_vns`) and [`restore_vtime`](Vmm::restore_vtime) resets the work
    /// counter to 0, so a fractional ratio's sub-ns remainder
    /// `(work · num) mod den` would be silently lost across a snapshot — a
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
            terminal: None,
            saved_state: None,
            vtime: None,
            rng_completion_staged: false,
            // A fresh VM is at work 0: the effective V-time is exactly `vns_base`, so
            // a snapshot here is exact (synchronized).
            vtime_synchronized: true,
            first_entry_done: false,
            lapic: None,
            legacy: None,
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
        let vt = self.vtime.as_mut().ok_or_else(|| {
            VmmError::ContractViolation("restore_vtime called but V-time is not wired".to_string())
        })?;
        // 1. Validate, committing nothing. Rebuild the clock (validates the cfg)
        //    and validate the entropy blob into a CLONE (its `restore_state`
        //    rejects a malformed/untrusted blob without touching the live stream).
        let mut cfg = vt.cfg;
        cfg.vns_base = snap.vns;
        let clock = VClock::new(cfg)?;
        let mut entropy = vt.entropy.clone();
        entropy.restore_state(&snap.entropy).map_err(|e| {
            VmmError::ContractViolation(format!("entropy snapshot rejected on restore: {e:?}"))
        })?;
        // 2. Reset the hardware counter — the last fallible step. A failure here
        //    leaves clock/cfg/entropy at their old, consistent values (nothing
        //    below this line can fail).
        vt.work.reset()?;
        // 3. Commit the validated state (all infallible). The hardware counter
        //    restarts at 0 and the snapshot's effective V-time now lives in
        //    `cfg.vns_base`, so the last-intercept anchor for the hash resets to 0
        //    too (effective V-time = `vns_base` until the next intercept advances
        //    work) — keeping a restored VM byte-identical to a fresh one at the
        //    same effective V-time (task-27 item 2, restore-transparency).
        //    `IA32_TSC_ADJUST` is re-applied from the snapshot (the contract carries
        //    it in `vm_state`).
        vt.clock = clock;
        vt.cfg = cfg;
        vt.entropy = entropy;
        vt.last_intercept_work = 0;
        vt.tsc_adjust = snap.tsc_adjust;
        // The restored VM is at work 0 with effective V-time exactly `vns_base`, a
        // synchronized point — an immediate `save_vtime` is exact. (`vt`'s borrow of
        // `self.vtime` has ended by this disjoint-field write.)
        self.vtime_synchronized = true;
        Ok(())
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
        if !self.first_entry_done {
            if let Some(vt) = self.vtime.as_mut() {
                vt.work.start_run()?;
            }
            self.first_entry_done = true;
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
        let exit = self.backend.run()?;
        self.rng_completion_staged = false;
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
            Exit::Hlt => Ok(self.terminate(TerminalReason::Hlt)),
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
            Exit::Deadline { .. } => Err(VmmError::ContractViolation(
                "unexpected run_until deadline (V-time is a later phase)".to_string(),
            )),
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
    v.extend_from_slice(&[
        seg.type_,
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

fn encode_events(v: &mut Vec<u8>, e: &vmm_backend::VcpuEvents) {
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
}
