// SPDX-License-Identifier: AGPL-3.0-or-later
//! `KvmBackend` — the **box-only syscall orchestration** for the stock-KVM
//! [`Backend`] (`#[cfg(target_os = "linux")]`).
//!
//! Creates the VM with **`KVM_IRQCHIP_NONE`** (R1): no in-kernel
//! irqchip/LAPIC/PIT, one vCPU, a single memslot for bring-up. The guest LAPIC is
//! a userspace xAPIC whose MMIO page falls through to `KVM_EXIT_MMIO`. It is
//! **not** determinism-complete: it cannot surface RDTSC/RDTSCP/RDRAND/RDSEED
//! (the declared holes — `capabilities()` reports them `false`).
//!
//! **This module is the one excluded from the coverage + mutation gates**
//! (`.cargo/mutants.toml` `exclude_globs`, the coverage `--ignore-filename-regex`):
//! every function here either issues a KVM syscall (`KVM_RUN` / `KVM_GET/SET_*` /
//! `mmap`) or is a trivial wrapper that cannot run without `/dev/kvm`, so the
//! coverage/mutation oracles (which run with no VM) cannot reach it. All the
//! actual translation/validation **logic is factored into [`crate::kvm`]** and is
//! covered + mutation-tested by that module's non-`#[ignore]` synthetic-`kvm_run`
//! tests; this file just wires those helpers to the ioctls.
//!
//! The two granted `unsafe` purposes (rule #7), each with a `// SAFETY:` comment:
//! (1) `KVM_SET_USER_MEMORY_REGION` registration in [`KvmBackend::map_memory`],
//! and (2) `mmap`-ing the `kvm_run` shared page in [`KvmBackend::new`]. The raw
//! `mmap`/`ioctl` syscalls sit behind `#[cfg(not(miri))]` seams with `#[cfg(miri)]`
//! stubs.

use std::collections::{BTreeMap, VecDeque};
use std::os::fd::AsRawFd;

use kvm_bindings::{
    CpuId, KVM_CAP_X86_USER_SPACE_MSR, KVM_GUESTDBG_ENABLE, KVM_GUESTDBG_SINGLESTEP,
    KVM_MSR_EXIT_REASON_FILTER, KVM_MSR_EXIT_REASON_INVAL, KVM_MSR_EXIT_REASON_UNKNOWN,
    KVM_MSR_FILTER_MAX_RANGES, Msrs, kvm_enable_cap, kvm_guest_debug, kvm_interrupt, kvm_mp_state,
    kvm_msr_entry, kvm_msr_filter, kvm_msr_filter_range, kvm_run, kvm_sregs2,
    kvm_userspace_memory_region, kvm_xsave,
};
use kvm_ioctls::{Cap, Kvm, VcpuFd, VmFd};
use vtime::{CpuBackend, InjectionPlanner, PlannerConfig};

use crate::backend::Backend;
use crate::config::{CpuidModel, MsrFilter};
use crate::error::{BackendError, Result};
use crate::exit::{Capabilities, Event, Exit, ExitCounts};
use crate::kvm::*;
use crate::pmu_sys::PmuBranchCounter;
use crate::region::{MemRegions, split_around_hole};
use crate::run_until::{
    ExitPoison, FirstEntryReset, PreemptCpu, RunUntilStart, SKID_MARGIN, classify_run_until,
    drive_run_until, free_run_decision,
};
use crate::state::VcpuState;
use crate::types::{Gpa, Vtime};

/// `KVM_MSR_FILTER_READ | KVM_MSR_FILTER_WRITE` — apply the in-kernel allow to
/// both directions (the `allow-stateful` rows are bidirectional).
const KVM_MSR_FILTER_READ: u32 = 1 << 0;
const KVM_MSR_FILTER_WRITE: u32 = 1 << 1;
/// `kvm_msr_filter.flags` bit: deny any MSR not named by a range (→ userspace
/// exit, given the `KVM_MSR_EXIT_REASON_FILTER` cap). This is the default-deny
/// floor.
const KVM_MSR_FILTER_DEFAULT_DENY: u32 = 1 << 0;

/// Build a Linux ioctl request number (`_IOC` encoding): dir bits 30-31, size
/// bits 16-29, type bits 8-15, nr bits 0-7.
const fn ioc(dir: u64, typ: u64, nr: u64, size: u64) -> u64 {
    (dir << 30) | (size << 16) | (typ << 8) | nr
}

/// `_IO(KVMIO, 0x80)` — enter guest mode. No argument.
const KVM_RUN: u64 = ioc(0, 0xAE, 0x80, 0);
/// `_IOW(KVMIO, 0x86, struct kvm_interrupt)` — queue a maskable IRQ for the next
/// VM-entry (the userspace-irqchip injection path; kvm-ioctls exposes no safe
/// wrapper for it, unlike `KVM_NMI`, so it is a direct ioctl like the MSR filter).
const KVM_INTERRUPT: u64 = ioc(1, 0xAE, 0x86, size_of::<kvm_interrupt>() as u64);
/// `_IO(KVMIO, 0xe5)` — harmony 0005: arm a one-shot MTF single-step.
const KVM_ARM_MTF_STEP: u64 = ioc(0, 0xAE, 0xe5, 0);
/// `_IO(KVMIO, 0xE4)` — arm the one-shot in-kernel force-exit (patch 0004). No
/// argument: it sets the per-vCPU `preempt_armed` flag so the next perf-overflow NMI
/// VM-exit returns `KVM_EXIT_PREEMPT` to userspace instead of re-entering (gated on
/// the determinism opt-in cap; EINVAL on stock KVM). One-shot — the kernel clears it
/// when it fires — so `run_armed` re-arms before every free-run entry.
const KVM_ARM_PREEMPT_EXIT: u64 = ioc(0, 0xAE, 0xE4, 0);
/// `_IOW(KVMIO, 0xC6, struct kvm_msr_filter)` — install the MSR filter.
const KVM_X86_SET_MSR_FILTER: u64 = ioc(1, 0xAE, 0xC6, size_of::<kvm_msr_filter>() as u64);
/// `_IOR(KVMIO, 0xCC, struct kvm_sregs2)` — read sregs incl. PDPTRs/flags.
const KVM_GET_SREGS2: u64 = ioc(2, 0xAE, 0xCC, size_of::<kvm_sregs2>() as u64);
/// `_IOW(KVMIO, 0xCD, struct kvm_sregs2)` — write sregs incl. PDPTRs/flags.
const KVM_SET_SREGS2: u64 = ioc(1, 0xAE, 0xCD, size_of::<kvm_sregs2>() as u64);
/// `_IOR(KVMIO, 0xCF, struct kvm_xsave)` — read the host-sized XSAVE2 image (the
/// ioctl number encodes the *base* `kvm_xsave` size; the kernel copies the
/// `KVM_CAP_XSAVE2`-reported number of bytes).
const KVM_GET_XSAVE2: u64 = ioc(2, 0xAE, 0xCF, size_of::<kvm_xsave>() as u64);
/// `_IOW(KVMIO, 0xA5, struct kvm_xsave)` — write the XSAVE image (same ioctl for
/// the 4 KiB legacy and the larger XSAVE2 buffer; the kernel reads the right size).
const KVM_SET_XSAVE: u64 = ioc(1, 0xAE, 0xA5, size_of::<kvm_xsave>() as u64);

/// The stock-KVM bring-up backend. Holds the KVM/VM/vCPU handles, the `mmap`-ed
/// `kvm_run`, the memslot table, the retained MSR filter (for `save`/`restore`),
/// the exit counters, and the pending-completion state.
pub struct KvmBackend {
    // Field order matters for drop: the `mmap` is released in `Drop`, the fds
    // close after. `kvm` is kept alive so the VM/vCPU fds stay valid.
    vcpu: VcpuFd,
    vm: VmFd,
    #[allow(dead_code)] // retained so its fd outlives `vm`/`vcpu`
    kvm: Kvm,
    run: *mut kvm_run,
    mmap_size: usize,
    /// `KVM_CAP_XSAVE2`-reported XSAVE image size in bytes (`Some`, ≥ 4 KiB) on a
    /// kernel that supports it (5.17+, incl. the determinism box); `None` falls
    /// back to the fixed 4 KiB `kvm_xsave`. `save`/`restore` carry exactly this
    /// many bytes so a host with dynamically-enabled XSTATE (e.g. AMX) does not
    /// truncate guest xstate.
    xsave2_size: Option<usize>,
    regions: MemRegions,
    /// The number of KVM memslots registered so far — i.e. the next free `slot`
    /// index. Tracked **separately** from `regions` (which records one LOGICAL
    /// region per `map_memory` for GPA→host translation) because a single
    /// `map_memory` may register MORE than one KVM memslot when it splits the
    /// backing around the LAPIC MMIO hole. Deriving the slot id from the logical
    /// region count would make a second `map_memory` reuse the first split's high
    /// slot and clobber it; this counter advances by the number of parts, only on a
    /// fully-successful map.
    mem_slot_count: u32,
    /// Whether guest-RAM memslots are registered with `KVM_MEM_LOG_DIRTY_PAGES`
    /// (task 95 M2.1). Default **on**: dirty logging is guest-inert (write-protect
    /// faults are host-side; gate a0 proves bit-identical `state_hash`), and it is
    /// what makes [`Backend::harvest_dirty_gfns`] answer. [`Self::set_dirty_log_enabled`]
    /// exists as the A/B arm of that gate and the emergency revert; it affects
    /// only memslots registered **after** the call.
    dirty_log: bool,
    /// The registered RAM memslots — `(slot, gpa, size)` per split part — the
    /// harvest walks: one `KVM_GET_DIRTY_LOG` per entry, decoded back to absolute
    /// gfns via the recorded base gpa. Populated by `map_memory` in slot order
    /// (both LAPIC-split halves of one backing get their own entries); entries of
    /// a failed (rolled-back) map are removed with it.
    dirty_slots: Vec<(u32, u64, u64)>,
    /// Latched (never cleared) when a RAM memslot is registered **without**
    /// `KVM_MEM_LOG_DIRTY_PAGES`: that slot's guest writes are permanently
    /// invisible to the log, so `harvest_dirty_gfns` declines for this
    /// backend's whole lifetime — completeness is a property of the slots, not
    /// of the current [`Self::set_dirty_log_enabled`] knob position.
    unlogged_slot: bool,
    msr_filter: Option<MsrFilter>,
    cpuid_installed: bool,
    msr_filter_installed: bool,
    pending: Pending,
    /// The single pending maskable IRQ vector ([`Backend::set_pending_irq`]),
    /// `None` if none. Held (not issued eagerly) so [`Self::enter_guest`] runs the
    /// userspace-irqchip handshake against the *current* post-exit
    /// `ready_for_interrupt_injection`: queue it when the guest can take it, else
    /// arm the interrupt window and retry on `KVM_EXIT_IRQ_WINDOW_OPEN`. **One slot,
    /// overwritten** every entry by the VMM's re-arbitration (the LAPIC IRR is the
    /// real queue, above the trait), so an injected vector is never stale and a
    /// second IRQ is never dropped. Distinct from `pending` (the read/Wrmsr
    /// completion the last exit awaits); an in-flight injection never blocks a run.
    pending_irq: Option<u8>,
    /// Vectors for which `KVM_INTERRUPT` has actually been issued (accepted into
    /// the guest) since the last [`Backend::take_accepted_interrupt`] drain, in
    /// acceptance order. The VMM reads this to complete its userspace-LAPIC
    /// IRR→ISR transition only on confirmed acceptance. (Holds ≤1 in normal
    /// single-source operation; a `VecDeque` for robustness.)
    accepted_irq: VecDeque<u8>,
    counts: ExitCounts,
    /// The backend-owned retired-conditional-branch counter driving
    /// [`Backend::run_until`]'s overflow-early phase (task 47, the live
    /// [`vtime::CpuBackend`]). Opened **lazily-at-build but non-fatally**: `None`
    /// when `perf_event` is unavailable (no Intel PMU / `perf_event_paranoid` too
    /// high), in which case `run_until` returns a clear [`BackendError::Capability`]
    /// and the `run()`-only paths (M1/M2/corpus) are unaffected. Opened with the
    /// **same** event/flags/baseline as vmm-core's V-time `PerfWorkCounter`, on the
    /// same thread, so on a deterministic guest stream the two read identical work
    /// counts (the box-validation invariant — see `pmu` module docs).
    pmu: Option<PmuBranchCounter>,
    /// Whether single-step is currently armed for `run_until`, so it is set up once
    /// per `run_until` (not per step) and always disarmed before the next `run`.
    single_step_armed: bool,
    /// Whether this backend opted into `KVM_CAP_X86_DETERMINISTIC_INTERCEPTS` (the
    /// patched-KVM path). Gates both patched determinism mechanisms:
    /// - the single-step mechanism — the patched one-shot MTF (`KVM_ARM_MTF_STEP` →
    ///   `KVM_EXIT_DET_STEP`, which steps *through* the guest's own syscall/exception)
    ///   when `true`; stock `KVM_GUESTDBG_SINGLESTEP` when `false`;
    /// - the free-run force-exit (task 55, patch 0004) — when `true`, `run_until`'s
    ///   free-run arms the one-shot in-kernel `KVM_ARM_PREEMPT_EXIT` before every entry,
    ///   so the V-time deadline is hit with the bounded hardware-PMI skid rather than the
    ///   unbounded `SIGIO` kick; when `false` (stock KVM, which cannot honor the cap) the
    ///   arm is skipped and the `pmu_sys` `SIGIO` kick remains the (non-deterministic)
    ///   fallback — stock KVM is not a determinism backend, so this is acceptable.
    ///
    /// Both ioctls are patched-only and would `EINVAL` on stock KVM or any VM that did
    /// not enable the cap, so the stock `run_until` caller (e.g. `live_preemption`) must
    /// never issue them.
    deterministic_intercepts: bool,
    /// The first-entry PMU-reset discipline (P1(b)): resets the shared-thread branch
    /// counter at the first `run`/`run_until` of this VM's life — and again at the
    /// first entry after a `restore` — so it shares vmm-core's work-counter baseline
    /// and excludes a coexisting VM's branches. The discipline (and the determinism
    /// invariant it encodes) lives in the portable, mutation/stateful-tested
    /// [`FirstEntryReset`]; this field is just its state. (Touches only the unhashed
    /// PMU counter — the `run()` path's observable state stays byte-identical.)
    reset_arm: FirstEntryReset,
    /// Fail-closed poison for a guest exit decoded during `run_until` (consumed by KVM,
    /// guest-visible) whose post-exit PMU read then failed (P2 round-9). Armed before the
    /// read, cleared on success; if it stays armed, the next `run`/`run_until` fails
    /// closed so a no-completion exit (PIO OUT / MMIO write / HLT / shutdown) the VMM
    /// never observed is not silently skipped. State only; the discipline is in the
    /// portable, tested [`ExitPoison`].
    exit_poison: ExitPoison,
}

impl KvmBackend {
    /// Open `/dev/kvm`, `KVM_CREATE_VM` (declining the in-kernel irqchip —
    /// `KVM_IRQCHIP_NONE`), `KVM_CREATE_VCPU` (one vCPU), and `mmap` the `kvm_run`
    /// page. Memory is mapped separately via [`Backend::map_memory`]; CPUID and
    /// the MSR filter via [`Backend::set_cpuid`]/[`Backend::set_msr_filter`]
    /// before the first run. Stock KVM (no determinism intercepts).
    pub fn new() -> Result<KvmBackend> {
        Self::build(false)
    }

    /// Shared constructor for [`KvmBackend::new`] (stock) and `PatchedKvmBackend`
    /// (`deterministic_intercepts = true`). The patched path opts into
    /// `KVM_CAP_X86_DETERMINISTIC_INTERCEPTS` **before** vCPU creation (the patch
    /// honors the cap only while `created_vcpus == 0`); default-off leaves stock
    /// behavior byte-identical, which is why the two are distinct backends rather
    /// than a runtime mode. The resulting backend surfaces / completes the four
    /// determinism exits via the shared pure [`crate::kvm`] helpers; nothing
    /// above the `Backend` trait branches on which constructor ran.
    pub(crate) fn build(deterministic_intercepts: bool) -> Result<KvmBackend> {
        let kvm = Kvm::new().map_err(kvm_err)?;
        let vm = kvm.create_vm().map_err(kvm_err)?;
        if deterministic_intercepts {
            // Opt into RDTSC/RDTSCP/RDRAND/RDSEED → KVM_EXIT_DETERMINISM. MUST
            // precede create_vcpu. A plain EINVAL here means the patched modules
            // are not loaded — surface that as a clear Capability error.
            let mut cap = kvm_enable_cap {
                cap: KVM_CAP_X86_DETERMINISTIC_INTERCEPTS,
                ..Default::default()
            };
            cap.args[0] = 1;
            vm.enable_cap(&cap).map_err(|_| BackendError::Capability {
                cap: "KVM_CAP_X86_DETERMINISTIC_INTERCEPTS (patched KVM not loaded?)",
            })?;
        }
        // KVM_IRQCHIP_NONE: we deliberately do NOT call create_irq_chip / split
        // irqchip. The guest LAPIC is the userspace xAPIC (R1).
        let vcpu = vm.create_vcpu(0).map_err(kvm_err)?;
        let mmap_size = kvm.get_vcpu_mmap_size().map_err(kvm_err)?;
        if mmap_size < size_of::<kvm_run>() {
            return Err(BackendError::Internal("kvm_run mmap size too small"));
        }
        // The host-sized XSAVE image (KVM_CAP_XSAVE2, 5.17+). A positive value is
        // the full image size (≥ 4 KiB); 0 means the cap is absent → use the
        // fixed 4 KiB `kvm_xsave`.
        let xsave2 = vm.check_extension_int(Cap::Xsave2);
        let xsave2_size = (xsave2 > 0).then_some(xsave2 as usize);
        // SAFETY (granted purpose 2): map the per-vCPU shared `kvm_run` structure.
        // `vcpu`'s fd is valid for `mmap`; offset 0 is the `kvm_run`. The
        // resulting pointer is owned by this backend and unmapped exactly once in
        // `Drop`. `mmap_kvm_run` returns an error (never a null/`MAP_FAILED`
        // pointer) on failure.
        let run = unsafe { mmap_kvm_run(vcpu.as_raw_fd(), mmap_size)? };
        Ok(KvmBackend {
            vcpu,
            vm,
            kvm,
            run,
            mmap_size,
            xsave2_size,
            regions: MemRegions::new(),
            mem_slot_count: 0,
            dirty_log: true,
            dirty_slots: Vec::new(),
            unlogged_slot: false,
            msr_filter: None,
            cpuid_installed: false,
            msr_filter_installed: false,
            pending: Pending::None,
            pending_irq: None,
            accepted_irq: VecDeque::new(),
            counts: ExitCounts::default(),
            // Open the run_until branch counter now (before any guest entry, so it
            // shares vmm-core's V-time counter baseline) but never let its absence
            // fail VM creation — only `run_until` needs it. `.ok()`: a box without
            // perf access still creates a backend that can `run()`/save/restore.
            pmu: PmuBranchCounter::open().ok(),
            single_step_armed: false,
            deterministic_intercepts,
            reset_arm: FirstEntryReset::new(),
            exit_poison: ExitPoison::default(),
        })
    }

    /// Enable/disable `KVM_MEM_LOG_DIRTY_PAGES` on memslots registered by
    /// **subsequent** [`Backend::map_memory`] calls (task 95 M2.1). Default
    /// **enabled**. Call before mapping guest RAM; already-registered slots are
    /// unaffected. Disabling is the `flags: 0` A/B arm of the tracking-is-inert
    /// box gate (a0) — with it disabled, [`Backend::harvest_dirty_gfns`] answers
    /// [`Unsupported`](BackendError::Unsupported) so every capture falls back to
    /// the (always-correct) full scan.
    pub fn set_dirty_log_enabled(&mut self, enabled: bool) {
        self.dirty_log = enabled;
    }

    /// Copy `bytes` into guest memory at `gpa` through the registered memslots
    /// (bounds-checked; the loader path). Errors if the range is unmapped.
    pub fn write_guest(&mut self, gpa: Gpa, bytes: &[u8]) -> Result<()> {
        self.regions.write(gpa.0, bytes)
    }

    /// Copy guest memory at `gpa` into `buf` (bounds-checked; the M2-hash read
    /// path). Errors if the range is unmapped.
    pub fn read_guest(&self, gpa: Gpa, buf: &mut [u8]) -> Result<()> {
        self.regions.read(gpa.0, buf)
    }

    /// `true` once both config calls have landed.
    fn configured(&self) -> bool {
        self.cpuid_installed && self.msr_filter_installed
    }

    /// A raw view over the `mmap`-ed `kvm_run` page, handed to the pure
    /// `decode_*`/`apply_*` functions.
    fn run_page(&self) -> RunPage {
        // SAFETY: `self.run` is the live `mmap` of `self.mmap_size` bytes, owned
        // by this backend and not aliased by any live reference.
        unsafe { RunPage::new(self.run, self.mmap_size) }
    }

    /// Issue `KVM_RUN`, then map the raw exit via the pure [`decode_exit`]. Retries
    /// on `EINTR` and on the internally-consumed run-loop control exits — including
    /// `KVM_EXIT_IRQ_WINDOW_OPEN`, on which the pending IRQ becomes injectable and
    /// is queued on the next loop iteration.
    fn enter_guest(&mut self) -> Result<Exit> {
        loop {
            // Userspace-irqchip injection handshake (KVM_IRQCHIP_NONE): if a
            // maskable IRQ is queued, deliver it now when the guest can take it,
            // else arm the interrupt window so KVM exits the instant it can. That
            // `KVM_EXIT_IRQ_WINDOW_OPEN` is consumed below (`decode_exit` → `None`)
            // and we re-enter here, now injectable. The decision + window-flag
            // write is the pure [`plan_irq_entry`]; only the KVM_INTERRUPT ioctl is
            // the box-only syscall.
            match plan_irq_entry(self.run_page(), self.pending_irq) {
                IrqEntry::Queue(vector) => {
                    // SAFETY (raw ioctl seam): `KVM_INTERRUPT` queues `vector` on
                    // the owned vCPU; `kvm_interrupt` is valid for the call.
                    // Excluded under Miri.
                    unsafe { raw_interrupt(self.vcpu.as_raw_fd(), u32::from(vector))? };
                    // Accepted: clear the slot and record it for the VMM to complete
                    // its LAPIC IRR→ISR transition (confirmed acceptance).
                    self.pending_irq = None;
                    self.accepted_irq.push_back(vector);
                }
                IrqEntry::Run => {}
            }
            // SAFETY (raw ioctl seam): `KVM_RUN` takes no argument; the kernel
            // reads/writes the `mmap`-ed `kvm_run` we own. Excluded under Miri.
            let rc = unsafe { raw_kvm_run(self.vcpu.as_raw_fd()) };
            if rc < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue; // signal interrupted the entry; re-enter
                }
                return Err(BackendError::Io(err));
            }
            match decode_exit(self.run_page())? {
                Some((exit, pending)) => {
                    self.counts.bump(exit.reason());
                    self.pending = pending;
                    return Ok(exit);
                }
                None => continue, // run-loop control exit; re-enter
            }
        }
    }

    // -- run_until (§2 inversion seam: PMU overflow-early + single-step) --------

    /// On the FIRST guest entry of this VM's life (via `run` or `run_until`), reset
    /// the backend PMU counter so it shares vmm-core's first-entry baseline (the
    /// V-time `PerfWorkCounter` does the same via `start_run`). No-op if perf is
    /// unavailable; touches only the unhashed PMU counter, so `run()`'s observable
    /// state stays byte-identical.
    ///
    /// INVARIANT (see [`FirstEntryReset`]): this is the SOLE consumer of the pending
    /// first-entry reset, and it MUST be called only immediately before an actual
    /// `KVM_RUN` — `run`'s `enter_guest` and `run_until`'s `Drive` branch. A no-entry
    /// path (`AtOrPastDeadline`/`restore`) must NOT call it, or a coexisting VM would
    /// contaminate this VM's baseline before its next real entry.
    /// Fail closed if a prior `run_until` decoded a guest exit whose post-exit PMU read
    /// failed (P2 round-9): that exit was consumed by KVM but never delivered, so
    /// re-entering would skip it. The poison latches until an exit is delivered cleanly.
    fn check_not_poisoned(&self) -> Result<()> {
        if self.exit_poison.is_poisoned() {
            return Err(BackendError::Internal(
                "backend poisoned: a prior guest exit was decoded but its PMU read failed — its \
                 state is unreliable and re-entering would skip a consumed exit (fail closed)",
            ));
        }
        Ok(())
    }

    fn ensure_first_run(&mut self) -> Result<()> {
        // `take_reset()` always runs (and disarms); the reset only fires when armed
        // AND a counter exists. (No-op when perf is unavailable.)
        if self.reset_arm.take_reset()
            && let Some(pmu) = self.pmu.as_mut()
            && let Err(e) = pmu.reset()
        {
            // P2(b): a failed RESET would leave a stale counter (foreign branches
            // from a coexisting VM) → past/late deadlines. Re-arm so a later entry
            // retries the reset, and fail closed now rather than continuing stale.
            self.reset_arm.rearm();
            return Err(e);
        }
        Ok(())
    }

    /// The userspace-irqchip injection handshake run before **every** `KVM_RUN` in
    /// `run_until` (free-run AND single-step): deliver a pending, injectable maskable
    /// vector now (recording acceptance), else arm/clear the interrupt window. Shared
    /// by both phases so a pending injectable IRQ (queued by `service_pending_irqs`
    /// before `run_until`) is delivered at the next entry in BOTH — the
    /// `set_pending_irq` next-entry contract (P2(a)) — and never injected stale.
    fn inject_pending(&mut self, fd: std::os::fd::RawFd) -> Result<()> {
        if let IrqEntry::Queue(vector) = plan_irq_entry(self.run_page(), self.pending_irq) {
            // SAFETY (raw ioctl seam): queue `vector` on the owned vCPU; `enter`'s
            // `ready_for_interrupt_injection` was just checked by `plan_irq_entry`.
            unsafe { raw_interrupt(fd, u32::from(vector))? };
            self.pending_irq = None;
            self.accepted_irq.push_back(vector);
        }
        Ok(())
    }

    /// The backend PMU work count (run_until's V-time axis). Errors if the counter
    /// is unavailable (perf open failed at build) or unreadable.
    fn pmu_work(&self) -> Result<u64> {
        self.pmu
            .as_ref()
            .ok_or(BackendError::Capability {
                cap: "perf_event PMU branch counter (run_until)",
            })?
            .work()
    }

    /// Arm the PMU overflow at absolute work `armed_at` (planner phase 1).
    fn pmu_arm(&self, armed_at: u64) -> Result<()> {
        self.pmu
            .as_ref()
            .ok_or(BackendError::Capability {
                cap: "perf_event PMU branch counter (run_until)",
            })?
            .arm_overflow(armed_at)
    }

    /// Disarm the PMU overflow + drain its ring buffer. Fallible so a disarm failure
    /// is propagated (P2) rather than silently leaving the overflow armed for the
    /// next `run()`. `Ok` when there is no counter (nothing to disarm).
    fn pmu_disarm(&self) -> Result<()> {
        match self.pmu.as_ref() {
            Some(pmu) => pmu.disarm(),
            None => Ok(()),
        }
    }

    /// Arm the patched KVM's **one-shot in-kernel force-exit** (`KVM_ARM_PREEMPT_EXIT`,
    /// patch 0004): the next perf-overflow PMI's NMI VM-exit returns to userspace with
    /// `KVM_EXIT_PREEMPT` instead of re-entering, so the free-run stops with only the
    /// bounded hardware-PMI skid (task 55). One-shot — the kernel clears the flag when
    /// it fires — so `run_armed` calls this before EVERY free-run entry (re-arming
    /// after a spurious NMI is idempotent and cheap).
    ///
    /// No-op on stock KVM (`!deterministic_intercepts`): the cap is off, so the ioctl
    /// would `EINVAL`; the `pmu_sys` `SIGIO` kick remains the (non-deterministic) fallback
    /// there. The determinism box always runs the patched backend.
    ///
    /// **Disarm asymmetry with 0005 (defense-in-depth for a stale arm).** Unlike 0005's
    /// MTF step — which the kernel disarms on its *own* exit (`vmx_handle_exit`) — patch
    /// 0004 clears `preempt_armed` **only** when the perf-overflow NMI fires it; there is
    /// no clear-on-userspace-return and no disarm ioctl (see
    /// `kvm-patches/patches/README.md`). So an arm set here can outlive an **early guest
    /// exit** (the guest took a genuine PIO/MMIO exit before the overflow), leaving the
    /// kernel flag set until some later NMI fires it — potentially on a plain `run()`,
    /// surfacing as `KVM_EXIT_PREEMPT`. That stale exit is swallowed as a transparent
    /// re-entry in [`decode_exit`](crate::kvm::decode_exit) (it does not corrupt guest
    /// state or the work counter), so leaving the arm set across an early exit is safe and
    /// re-arming here every entry is idempotent.
    fn arm_preempt_exit(&self) -> Result<()> {
        if !self.deterministic_intercepts {
            return Ok(());
        }
        // SAFETY (raw ioctl seam): `KVM_ARM_PREEMPT_EXIT` takes no argument; it sets
        // the one-shot `preempt_armed` flag on the owned vCPU. Excluded under Miri.
        let rc = unsafe { raw_arm_preempt_exit(self.vcpu.as_raw_fd()) };
        if rc < 0 {
            return Err(BackendError::Io(std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Planner phase 1: arm the overflow at `armed_at`, free-run the guest until the
    /// overflow signal kicks `KVM_RUN` out (an `EINTR`) at-or-past `armed_at`, or a
    /// genuine guest exit happens first. A pending maskable IRQ set before
    /// `run_until` rides this entry via the normal injection handshake. The overflow
    /// is disarmed by the caller.
    fn run_armed(&mut self, armed_at: u64) -> Result<LiveStop> {
        self.pmu_arm(armed_at)?;
        let fd = self.vcpu.as_raw_fd();
        loop {
            // Deliver any pending injectable vector (or arm the window). P2(a): the
            // same handshake the single-step phase uses, so a pending IRQ is never
            // delayed past the deadline.
            self.inject_pending(fd)?;
            // Arm the in-kernel force-exit (patch 0004) so the perf-overflow PMI's NMI
            // VM-exit returns KVM_EXIT_PREEMPT instead of re-entering — the bounded-skid
            // kick (task 55). One-shot, so re-arm before EVERY entry. No-op on stock KVM.
            self.arm_preempt_exit()?;
            // SAFETY (raw ioctl seam): `KVM_RUN` on the owned vCPU.
            let rc = unsafe { raw_kvm_run(fd) };
            if rc < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    // A signal broke us out (the overflow kick, or a spurious one).
                    if let Some(stop) = self.free_run_stop(armed_at)? {
                        return Ok(stop);
                    }
                    continue;
                }
                return Err(BackendError::Io(err));
            }
            match classify_step_exit(self.run_page()) {
                // P1(b): EVERY non-guest-exit stop — the signal-as-KVM_EXIT_INTR path,
                // the in-kernel KVM_EXIT_PREEMPT force-exit (patch 0004, task 55), AND
                // the IRQ_WINDOW_OPEN re-entry — reads the PMU and stops if the overflow
                // already crossed `armed_at`, rather than blindly re-entering (which
                // would overshoot the exact preemption point / inject a stale IRQ past
                // the deadline). KVM_EXIT_PREEMPT is the bounded-skid kick that makes
                // this stop land STRICTLY before the deadline on the patched box.
                StepStop::Interrupted | StepStop::Preempt | StepStop::Reenter => {
                    if let Some(stop) = self.free_run_stop(armed_at)? {
                        return Ok(stop);
                    }
                    continue;
                }
                // Single-step is not armed during the free-run; a debug exit here is
                // a contract violation, never silently treated as progress.
                StepStop::SingleStepTrap => {
                    return Err(BackendError::Internal(
                        "run_until: unexpected single-step debug exit during free-run",
                    ));
                }
                StepStop::GuestExit => {
                    return self.take_guest_exit_stop();
                }
            }
        }
    }

    /// Shared free-run check (P1(b)): read the PMU and decide stop-vs-reenter via the
    /// portable [`free_run_decision`]. `Some` ⇒ the overflow reached `armed_at`.
    fn free_run_stop(&self, armed_at: u64) -> Result<Option<LiveStop>> {
        Ok(free_run_decision(self.pmu_work()?, armed_at).map(LiveStop::Count))
    }

    /// Decode the genuine guest exit, record its pending-completion, and capture the
    /// work AT the exit (no guest code runs between the exit and this read), so
    /// `drive_run_until` can tell deliver (work ≤ deadline) from overshoot
    /// (work > deadline) — P1(a). Shared by both phases; a real exit is NEVER dropped.
    fn take_guest_exit_stop(&mut self) -> Result<LiveStop> {
        let (exit, pending) = decode_exit(self.run_page())?.ok_or(BackendError::Internal(
            "run_until: control exit misclassified as guest",
        ))?;
        // Record state BEFORE the fallible `pmu_work()` so a PMU-read failure leaves the
        // backend FAIL-CLOSED — the exit was already consumed by KVM (guest-visible), and
        // a retry must not re-enter past it. Two mechanisms, both armed first:
        //  - P2(a) round-5: a read-style exit (IN/RDMSR/…) sets the pending completion, so
        //    a retry hits the `PendingCompletion` guard.
        //  - P2 round-9: ALL exits (incl. no-completion — PIO OUT, MMIO write, HLT,
        //    shutdown, which leave `pending == None`) arm the exit poison, so a retry
        //    fails closed there.
        // P2 round-12: the poison stays armed THROUGH every fallible step that still stands
        // between here and the exit being RETURNED — the `pmu_work()` below, but ALSO
        // `drive_run_until`'s at/past-deadline rejection and `run_until`'s cleanup. It is
        // cleared (`delivered()`) ONLY when `run_until` is about to hand the exit to the
        // caller, so any error path in between stays fail-closed (a retry won't re-enter
        // past a consumed-but-undelivered exit).
        self.pending = pending;
        self.exit_poison.arm();
        let work = self.pmu_work()?;
        Ok(LiveStop::Guest { exit, work })
    }

    /// Planner phase 2: single-step **one** instruction (`KVM_GUESTDBG_SINGLESTEP`)
    /// to land at the exact deadline despite skid. Returns the new work count, or a
    /// genuine guest exit taken by the stepped instruction. P2(a): it runs the SAME
    /// injection handshake as the free-run phase ([`Self::inject_pending`]), so a
    /// pending injectable vector (queued before `run_until`) is delivered at the next
    /// entry rather than delayed past the deadline + reordered.
    fn single_step_once(&mut self) -> Result<LiveStop> {
        self.enable_single_step()?;
        let fd = self.vcpu.as_raw_fd();
        loop {
            // Deliver any pending injectable IRQ (P2(a)); clears the window when none
            // is pending, so an IRQ-window exit cannot loop.
            self.inject_pending(fd)?;
            // Patched path only: arm the one-shot MTF for the next instruction (exits
            // `KVM_EXIT_DET_STEP`, stepping THROUGH the guest's own syscall/exception).
            // The stock path uses the sticky `KVM_GUESTDBG_SINGLESTEP` armed in
            // `enable_single_step`; the MTF ioctl is patched-only and `EINVAL`s on a
            // non-determinism VM, so it must never be issued there (regressed the stock
            // `run_until`/`live_preemption` gate).
            if self.deterministic_intercepts {
                // SAFETY (raw ioctl seam): one-shot MTF arm on the owned vCPU.
                unsafe { raw_mtf_step(fd) }?;
            }
            // SAFETY (raw ioctl seam): single-stepped `KVM_RUN` on the owned vCPU.
            let rc = unsafe { raw_kvm_run(fd) };
            if rc < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue; // spurious signal during a step; re-step.
                }
                return Err(BackendError::Io(err));
            }
            match classify_step_exit(self.run_page()) {
                // The single-step trap is the stop signal (one instruction retired);
                // the overflow is disarmed in this phase, so a signal/IRQ-window exit
                // is just a re-entry (the planner bounds progress via the work count).
                StepStop::SingleStepTrap => {
                    let w = self.pmu_work()?;
                    return Ok(LiveStop::Count(w));
                }
                // Preempt is not armed during single-step (the force-exit is a free-run
                // mechanism), but a late/stray NMI exit is just a re-entry like a signal.
                StepStop::Interrupted | StepStop::Preempt | StepStop::Reenter => continue,
                StepStop::GuestExit => {
                    // The stepped instruction took its own exit to userspace; the
                    // in-kernel one-shot MTF (mtf_step_armed + the exec-control) is
                    // disarmed on that exit by vmx_handle_exit (harmony 0005), so no
                    // stale KVM_EXIT_DET_STEP survives into the next run or a snapshot.
                    return self.take_guest_exit_stop();
                }
            }
        }
    }

    /// Arm single-step (once per `run_until`) and disarm the PMU overflow + drain its
    /// ring: phase 1's overflow already fired, so for the (≤ skid_margin) exact-landing
    /// steps the counter must only count (no second overflow could fire mid-skid-margin,
    /// and the phase-1 record is consumed so a long run never fills the ring).
    ///
    /// Mechanism by backend: the **patched** path arms a per-step one-shot MTF in
    /// [`Self::single_step_once`] (it steps *through* the guest's own syscall/exception,
    /// which TF cannot — issue #34); the **stock** path arms the sticky
    /// `KVM_GUESTDBG_ENABLE | KVM_GUESTDBG_SINGLESTEP` here (the MTF ioctl is
    /// patched-only and would `EINVAL` on stock KVM).
    fn enable_single_step(&mut self) -> Result<()> {
        if self.single_step_armed {
            return Ok(());
        }
        self.pmu_disarm()?;
        if !self.deterministic_intercepts {
            // Stock KVM: TF-based single-step (each retired instruction → KVM_EXIT_DEBUG).
            let dbg = kvm_guest_debug {
                control: KVM_GUESTDBG_ENABLE | KVM_GUESTDBG_SINGLESTEP,
                ..Default::default()
            };
            self.vcpu.set_guest_debug(&dbg).map_err(kvm_err)?;
        }
        self.single_step_armed = true;
        Ok(())
    }

    /// Disarm single-step, restoring normal execution for the next `run`. No-op if it
    /// was never armed. Only the stock path armed guest-debug; the patched path's MTF
    /// is a per-step one-shot (no sticky vCPU state to clear).
    fn disable_single_step(&mut self) -> Result<()> {
        if !self.single_step_armed {
            return Ok(());
        }
        if !self.deterministic_intercepts {
            let dbg = kvm_guest_debug::default(); // control = 0: disable
            self.vcpu.set_guest_debug(&dbg).map_err(kvm_err)?;
        }
        self.single_step_armed = false;
        Ok(())
    }

    /// Read the `allow-stateful` MSR set via `KVM_GET_MSRS` (the index list is the
    /// retained filter); fail-closed on a short count via [`saved_msrs`].
    fn save_msrs(&self) -> Result<BTreeMap<u32, u64>> {
        let Some(filter) = &self.msr_filter else {
            return Ok(BTreeMap::new());
        };
        let indices: Vec<u32> = filter.allow_indices().collect();
        if indices.is_empty() {
            return Ok(BTreeMap::new());
        }
        let entries: Vec<kvm_msr_entry> = indices
            .iter()
            .map(|&index| kvm_msr_entry {
                index,
                ..Default::default()
            })
            .collect();
        let mut kmsrs = Msrs::from_entries(&entries)
            .map_err(|_| BackendError::Internal("MSR list too large"))?;
        let got = self.vcpu.get_msrs(&mut kmsrs).map_err(kvm_err)?;
        saved_msrs(kmsrs.as_slice(), got, indices.len())
    }

    /// Write the snapshot's MSRs via `KVM_SET_MSRS`; fail-closed on a short count.
    fn restore_msrs(&self, state: &VcpuState) -> Result<()> {
        if state.msrs.is_empty() {
            return Ok(());
        }
        let entries: Vec<kvm_msr_entry> = state
            .msrs
            .iter()
            .map(|(&index, &data)| kvm_msr_entry {
                index,
                data,
                ..Default::default()
            })
            .collect();
        let kmsrs = Msrs::from_entries(&entries)
            .map_err(|_| BackendError::Internal("MSR list too large"))?;
        let set = self.vcpu.set_msrs(&kmsrs).map_err(kvm_err)?;
        ensure_full_msr_count(set, entries.len())
    }

    /// Read the host-sized XSAVE image: `KVM_GET_XSAVE2` (the
    /// `KVM_CAP_XSAVE2`-reported size) where available, else the fixed 4 KiB
    /// `KVM_GET_XSAVE`. The returned bytes are `VcpuState.xsave` verbatim.
    fn save_xsave(&self) -> Result<Vec<u8>> {
        match self.xsave2_size {
            // SAFETY: `vcpu` is a valid vCPU fd; `raw_get_xsave2` allocates and
            // fills exactly `n` bytes (`n >= size_of::<kvm_xsave>()`). Miri-excluded.
            Some(n) => unsafe { raw_get_xsave2(self.vcpu.as_raw_fd(), n) },
            None => Ok(xsave_to_bytes(&self.vcpu.get_xsave().map_err(kvm_err)?)),
        }
    }

    /// Read the guest's `IA32_TSC_AUX` (`0xC000_0103`) via `KVM_GET_MSRS`, for an
    /// `RDTSCP` determinism completion (its `ECX` is the guest's `TSC_AUX`). This
    /// reflects guest architectural state (the contract's `allow-stateful`
    /// `TSC_AUX`, vm_state-echoed — never a host per-core value), so it stays a
    /// faithful instruction completion, not a contract-policy decision. Host
    /// `KVM_GET_MSRS` bypasses the guest MSR filter, so it works regardless of
    /// the installed policy.
    fn read_tsc_aux(&self) -> Result<u64> {
        const IA32_TSC_AUX: u32 = 0xC000_0103;
        let entries = [kvm_msr_entry {
            index: IA32_TSC_AUX,
            ..Default::default()
        }];
        let mut kmsrs = Msrs::from_entries(&entries)
            .map_err(|_| BackendError::Internal("MSR list too large"))?;
        let got = self.vcpu.get_msrs(&mut kmsrs).map_err(kvm_err)?;
        ensure_full_msr_count(got, 1)?;
        Ok(kmsrs.as_slice()[0].data)
    }

    /// Restore the XSAVE image saved by [`Self::save_xsave`]. The byte length was
    /// already validated by `validate_restore_shape`; the `None` legacy path
    /// re-checks defensively.
    fn restore_xsave(&self, bytes: &[u8]) -> Result<()> {
        match self.xsave2_size {
            // SAFETY: `vcpu` is valid; `raw_set_xsave` reads `bytes` (the validated
            // host XSAVE2 size). Miri-excluded.
            Some(_) => unsafe { raw_set_xsave(self.vcpu.as_raw_fd(), bytes) },
            None => {
                let xsave = xsave_from_bytes(bytes)?;
                // SAFETY: `xsave` is a validated, fully-initialized 4 KiB
                // `kvm_xsave`; `set_xsave` only reads it into the vCPU.
                unsafe { self.vcpu.set_xsave(&xsave).map_err(kvm_err) }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The raw syscall seams (the genuinely un-CI-testable / un-Miri-able lines).
// ---------------------------------------------------------------------------

/// `mmap` the per-vCPU `kvm_run` page. Returns `Err` (never `MAP_FAILED`) on
/// failure.
///
/// # Safety
/// `fd` must be a valid vCPU fd and `len` its `KVM_GET_VCPU_MMAP_SIZE`.
#[cfg(not(miri))]
unsafe fn mmap_kvm_run(fd: std::os::fd::RawFd, len: usize) -> Result<*mut kvm_run> {
    // SAFETY: standard shared mapping of the vCPU fd at offset 0; `len` is the
    // kernel-reported size.
    let p = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if p == libc::MAP_FAILED {
        return Err(BackendError::Io(std::io::Error::last_os_error()));
    }
    Ok(p.cast::<kvm_run>())
}

/// Miri stub: the `mmap` syscall is un-interpretable. Never reached under Miri
/// (the live `KvmBackend` tests are `#[ignore]`); present only so the crate
/// compiles under `cargo miri test`.
#[cfg(miri)]
unsafe fn mmap_kvm_run(_fd: std::os::fd::RawFd, _len: usize) -> Result<*mut kvm_run> {
    Err(BackendError::Internal("mmap unavailable under miri"))
}

/// Issue the `KVM_RUN` ioctl. Returns the raw `ioctl` result (`< 0` on error).
///
/// # Safety
/// `fd` must be a valid vCPU fd whose `kvm_run` is currently mapped.
#[cfg(not(miri))]
unsafe fn raw_kvm_run(fd: std::os::fd::RawFd) -> libc::c_int {
    // SAFETY: `KVM_RUN` takes no argument; the kernel uses the mapped `kvm_run`.
    unsafe { libc::ioctl(fd, KVM_RUN as libc::c_ulong, 0) }
}

/// Miri stub for the `KVM_RUN` ioctl (never reached under Miri).
#[cfg(miri)]
unsafe fn raw_kvm_run(_fd: std::os::fd::RawFd) -> libc::c_int {
    -1
}

/// Issue the `KVM_X86_SET_MSR_FILTER` ioctl with a prepared `kvm_msr_filter`.
///
/// # Safety
/// `fd` must be a valid VM fd; `filter`'s range bitmap pointers must be valid for
/// the duration of the call (KVM copies them in).
#[cfg(not(miri))]
unsafe fn raw_set_msr_filter(fd: std::os::fd::RawFd, filter: &kvm_msr_filter) -> Result<()> {
    // SAFETY: `filter` is a valid `kvm_msr_filter`; the ioctl reads it (and the
    // bitmaps it points to) and copies them into the kernel.
    let rc = unsafe {
        libc::ioctl(
            fd,
            KVM_X86_SET_MSR_FILTER as libc::c_ulong,
            filter as *const kvm_msr_filter,
        )
    };
    if rc < 0 {
        return Err(BackendError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Miri stub for the MSR-filter ioctl (never reached under Miri).
#[cfg(miri)]
unsafe fn raw_set_msr_filter(_fd: std::os::fd::RawFd, _filter: &kvm_msr_filter) -> Result<()> {
    Err(BackendError::Internal("ioctl unavailable under miri"))
}

/// harmony 0005: arm a one-shot MTF single-step. The next `KVM_RUN` exits with
/// `KVM_EXIT_DET_STEP` after exactly one instruction — including THROUGH the guest's
/// own syscall/exception (unlike TF single-step, cleared on event delivery / FMASK).
///
/// # Safety
/// `fd` must be a valid vCPU fd.
#[cfg(not(miri))]
unsafe fn raw_mtf_step(fd: std::os::fd::RawFd) -> Result<()> {
    // SAFETY: argument-less ioctl on the owned vCPU fd.
    let rc = unsafe { libc::ioctl(fd, KVM_ARM_MTF_STEP as libc::c_ulong) };
    if rc < 0 {
        return Err(BackendError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(miri)]
unsafe fn raw_mtf_step(_fd: std::os::fd::RawFd) -> Result<()> {
    Ok(())
}

/// Issue the `KVM_INTERRUPT` ioctl, queueing maskable IRQ `vector` for the next
/// VM-entry. The caller (`enter_guest`) only reaches this when
/// `ready_for_interrupt_injection` is set (via [`plan_irq_entry`]), so KVM accepts
/// the vector rather than rejecting it as un-injectable.
///
/// # Safety
/// `fd` must be a valid vCPU fd.
#[cfg(not(miri))]
unsafe fn raw_interrupt(fd: std::os::fd::RawFd, vector: u32) -> Result<()> {
    let irq = kvm_interrupt { irq: vector };
    // SAFETY: the ioctl reads a `kvm_interrupt` from `&irq` (valid for the call)
    // and copies it into the kernel.
    let rc = unsafe {
        libc::ioctl(
            fd,
            KVM_INTERRUPT as libc::c_ulong,
            &irq as *const kvm_interrupt,
        )
    };
    if rc < 0 {
        return Err(BackendError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Miri stub for the `KVM_INTERRUPT` ioctl (never reached under Miri).
#[cfg(miri)]
unsafe fn raw_interrupt(_fd: std::os::fd::RawFd, _vector: u32) -> Result<()> {
    Err(BackendError::Internal("ioctl unavailable under miri"))
}

/// Issue the `KVM_ARM_PREEMPT_EXIT` ioctl (patch 0004): arm the one-shot in-kernel
/// force-exit on the next perf-overflow NMI VM-exit. Returns the raw `ioctl` result
/// (`< 0` on error — e.g. `EINVAL` if the determinism cap is not enabled). The caller
/// only issues it when `deterministic_intercepts` (the patched backend).
///
/// # Safety
/// `fd` must be a valid vCPU fd.
#[cfg(not(miri))]
unsafe fn raw_arm_preempt_exit(fd: std::os::fd::RawFd) -> libc::c_int {
    // SAFETY: `KVM_ARM_PREEMPT_EXIT` takes no argument; the kernel sets the one-shot
    // `vcpu->arch.preempt_armed` flag on the owned vCPU.
    unsafe { libc::ioctl(fd, KVM_ARM_PREEMPT_EXIT as libc::c_ulong, 0) }
}

/// Miri stub for the `KVM_ARM_PREEMPT_EXIT` ioctl (never reached under Miri).
#[cfg(miri)]
unsafe fn raw_arm_preempt_exit(_fd: std::os::fd::RawFd) -> libc::c_int {
    -1
}

/// `KVM_GET_SREGS2` — read sregs incl. PDPTRs/flags (kvm-ioctls exposes no SREGS2
/// wrapper, so this is a direct ioctl, like the MSR filter).
///
/// # Safety
/// `fd` must be a valid vCPU fd.
#[cfg(not(miri))]
unsafe fn raw_get_sregs2(fd: std::os::fd::RawFd) -> Result<kvm_sregs2> {
    let mut sregs2 = kvm_sregs2::default();
    // SAFETY: the ioctl writes a full `kvm_sregs2` into our out-param.
    let rc = unsafe {
        libc::ioctl(
            fd,
            KVM_GET_SREGS2 as libc::c_ulong,
            &mut sregs2 as *mut kvm_sregs2,
        )
    };
    if rc < 0 {
        return Err(BackendError::Io(std::io::Error::last_os_error()));
    }
    Ok(sregs2)
}

/// Miri stub for `KVM_GET_SREGS2` (never reached under Miri).
#[cfg(miri)]
unsafe fn raw_get_sregs2(_fd: std::os::fd::RawFd) -> Result<kvm_sregs2> {
    Err(BackendError::Internal("ioctl unavailable under miri"))
}

/// `KVM_SET_SREGS2` — write sregs incl. PDPTRs/flags.
///
/// # Safety
/// `fd` must be a valid vCPU fd.
#[cfg(not(miri))]
unsafe fn raw_set_sregs2(fd: std::os::fd::RawFd, sregs2: &kvm_sregs2) -> Result<()> {
    // SAFETY: the ioctl reads a full `kvm_sregs2` from `sregs2`.
    let rc = unsafe {
        libc::ioctl(
            fd,
            KVM_SET_SREGS2 as libc::c_ulong,
            sregs2 as *const kvm_sregs2,
        )
    };
    if rc < 0 {
        return Err(BackendError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Miri stub for `KVM_SET_SREGS2` (never reached under Miri).
#[cfg(miri)]
unsafe fn raw_set_sregs2(_fd: std::os::fd::RawFd, _sregs2: &kvm_sregs2) -> Result<()> {
    Err(BackendError::Internal("ioctl unavailable under miri"))
}

/// `KVM_GET_XSAVE2` — read `len` bytes of the host-sized XSAVE image into a fresh
/// buffer (`len` is the `KVM_CAP_XSAVE2`-reported size, ≥ `size_of::<kvm_xsave>()`).
///
/// # Safety
/// `fd` must be a valid vCPU fd and `len` the host's `KVM_CAP_XSAVE2` size, so the
/// kernel's `copy_to_user` of `len` bytes stays within the buffer.
#[cfg(not(miri))]
unsafe fn raw_get_xsave2(fd: std::os::fd::RawFd, len: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    // SAFETY: the ioctl writes exactly `len` bytes into `buf` (its capacity).
    let rc = unsafe { libc::ioctl(fd, KVM_GET_XSAVE2 as libc::c_ulong, buf.as_mut_ptr()) };
    if rc < 0 {
        return Err(BackendError::Io(std::io::Error::last_os_error()));
    }
    Ok(buf)
}

/// Miri stub for `KVM_GET_XSAVE2` (never reached under Miri).
#[cfg(miri)]
unsafe fn raw_get_xsave2(_fd: std::os::fd::RawFd, len: usize) -> Result<Vec<u8>> {
    Ok(vec![0u8; len])
}

/// `KVM_SET_XSAVE` — write the XSAVE image from `bytes` (the same ioctl serves the
/// 4 KiB legacy and the larger XSAVE2 buffer; the kernel reads the host size).
///
/// # Safety
/// `fd` must be a valid vCPU fd and `bytes` at least the host's XSAVE image size,
/// so the kernel's `copy_from_user` stays within the buffer.
#[cfg(not(miri))]
unsafe fn raw_set_xsave(fd: std::os::fd::RawFd, bytes: &[u8]) -> Result<()> {
    // SAFETY: the ioctl reads the host XSAVE size from `bytes` (its length is the
    // validated `KVM_CAP_XSAVE2` size).
    let rc = unsafe { libc::ioctl(fd, KVM_SET_XSAVE as libc::c_ulong, bytes.as_ptr()) };
    if rc < 0 {
        return Err(BackendError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Miri stub for `KVM_SET_XSAVE` (never reached under Miri).
#[cfg(miri)]
unsafe fn raw_set_xsave(_fd: std::os::fd::RawFd, _bytes: &[u8]) -> Result<()> {
    Err(BackendError::Internal("ioctl unavailable under miri"))
}

impl Drop for KvmBackend {
    fn drop(&mut self) {
        // SAFETY: `self.run` came from `mmap_kvm_run(.., self.mmap_size)` and is
        // unmapped exactly once here. Excluded under Miri (never mapped there).
        #[cfg(not(miri))]
        unsafe {
            libc::munmap(self.run.cast::<libc::c_void>(), self.mmap_size);
        }
    }
}

/// Where a single planner-driven `KVM_RUN` (free-run or single-step) stopped.
enum LiveStop {
    /// Stopped at this work count (overflow kick or single-step trap) — no guest
    /// exit; the planner advances toward the deadline.
    Count(u64),
    /// A genuine guest exit was taken; `work` is the retired-branch count **at** the
    /// exit (P1(a)). Its pending-completion was armed on the backend. The planner is
    /// told it reached the deadline (so it stops); `drive_run_until` then compares
    /// `work` to the deadline — only `work < deadline` is a true early exit.
    Guest { exit: Exit, work: u64 },
}

/// The live [`vtime::CpuBackend`] (+ [`PreemptCpu`]): a thin adapter binding the
/// pure precise-injection planner to the real PMU counter + KVM single-step on a
/// borrowed [`KvmBackend`]. `work()` is infallible (the trait demands it), so the
/// current count is cached from the fallible read `run_until` did up front and
/// refreshed after each overflow-run/single-step; a guest exit / syscall error is
/// stashed for `run_until` to recover after the planner returns.
struct LiveCpu<'a> {
    backend: &'a mut KvmBackend,
    deadline: u64,
    work_cache: u64,
    /// A stashed genuine guest exit + the real work count at it (P1(a)).
    pending_exit: Option<(Exit, u64)>,
    err: Option<BackendError>,
}

impl CpuBackend for LiveCpu<'_> {
    fn work(&self) -> u64 {
        self.work_cache
    }

    fn run_until_overflow(
        &mut self,
        armed_at: u64,
    ) -> std::result::Result<u64, vtime::BackendError> {
        match self.backend.run_armed(armed_at) {
            Ok(LiveStop::Count(work)) => {
                self.work_cache = work;
                Ok(work)
            }
            // A guest exit during the FREE-RUN: report a count STRICTLY BELOW the
            // deadline so the planner does NOT mistake this sentinel for an overflow
            // skid (round-6: an overflow stop `>= target` is `SkidExceeded`). The
            // single-step phase then short-circuits to the deadline (below), stopping
            // the planner at ReadyToInject; the real exit + its work are recovered via
            // `take_guest_exit` (P1 classifies early/at/past from the WORK, not this
            // sentinel — so an at/past-deadline exit still fails closed there).
            Ok(LiveStop::Guest { exit, work }) => {
                self.pending_exit = Some((exit, work));
                Ok(self.deadline.saturating_sub(1))
            }
            Err(e) => {
                self.err = Some(e);
                Err(vtime::BackendError::new(
                    "run_until_overflow: KVM/PMU failure",
                ))
            }
        }
    }

    fn single_step(&mut self) -> std::result::Result<u64, vtime::BackendError> {
        // Once a guest exit is captured, stop advancing (never re-enter past a
        // pending-completion exit): tell the planner the deadline was reached.
        if self.pending_exit.is_some() {
            return Ok(self.deadline);
        }
        match self.backend.single_step_once() {
            Ok(LiveStop::Count(work)) => {
                self.work_cache = work;
                Ok(work)
            }
            Ok(LiveStop::Guest { exit, work }) => {
                self.pending_exit = Some((exit, work));
                Ok(self.deadline)
            }
            Err(e) => {
                self.err = Some(e);
                Err(vtime::BackendError::new("single_step: KVM/PMU failure"))
            }
        }
    }
}

impl PreemptCpu for LiveCpu<'_> {
    fn take_guest_exit(&mut self) -> Option<(Exit, u64)> {
        self.pending_exit.take()
    }
    fn take_error(&mut self) -> Option<BackendError> {
        self.err.take()
    }
}

impl Backend for KvmBackend {
    fn set_cpuid(&mut self, model: &CpuidModel) -> Result<()> {
        let entries = cpuid_entries(model);
        let cpuid = CpuId::from_entries(&entries)
            .map_err(|_| BackendError::Internal("CPUID table too large for KVM"))?;
        self.vcpu.set_cpuid2(&cpuid).map_err(kvm_err)?;
        self.cpuid_installed = true;
        Ok(())
    }

    fn set_msr_filter(&mut self, filter: &MsrFilter) -> Result<()> {
        if filter.allow_inkernel.len() > KVM_MSR_FILTER_MAX_RANGES as usize {
            return Err(BackendError::Memory("too many MSR filter ranges"));
        }
        // 1) Route filtered / unknown / invalid MSR accesses to userspace.
        let mut cap = kvm_enable_cap {
            cap: KVM_CAP_X86_USER_SPACE_MSR,
            ..Default::default()
        };
        cap.args[0] = u64::from(
            KVM_MSR_EXIT_REASON_FILTER | KVM_MSR_EXIT_REASON_UNKNOWN | KVM_MSR_EXIT_REASON_INVAL,
        );
        self.vm.enable_cap(&cap).map_err(kvm_err)?;

        // 2) Build the default-deny filter. Each named range gets an all-ones
        //    bitmap (allow in-kernel for every index in the range); everything
        //    else is denied → userspace exit. The bitmaps stay alive until the
        //    ioctl returns (KVM copies them in).
        let mut bitmaps: Vec<Vec<u8>> = Vec::with_capacity(filter.allow_inkernel.len());
        let mut ranges = [kvm_msr_filter_range::default(); KVM_MSR_FILTER_MAX_RANGES as usize];
        for (i, r) in filter.allow_inkernel.iter().enumerate() {
            let nbytes = r.count.div_ceil(8) as usize;
            bitmaps.push(vec![0xFFu8; nbytes]);
            ranges[i] = kvm_msr_filter_range {
                flags: KVM_MSR_FILTER_READ | KVM_MSR_FILTER_WRITE,
                nmsrs: r.count,
                base: r.base,
                // KVM only reads the bitmap, but the field is typed `*mut u8`.
                bitmap: bitmaps[i].as_mut_ptr(),
            };
        }
        let kfilter = kvm_msr_filter {
            flags: KVM_MSR_FILTER_DEFAULT_DENY,
            ranges,
        };
        // SAFETY: `kfilter` and its bitmap pointers are valid for this call; KVM
        // copies them in. Excluded under Miri.
        unsafe { raw_set_msr_filter(self.vm.as_raw_fd(), &kfilter)? };

        self.msr_filter = Some(filter.clone());
        self.msr_filter_installed = true;
        Ok(())
    }

    unsafe fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> Result<()> {
        // Validate + record the FULL region via the portable seam (alignment /
        // overlap / size). `read_guest`/`write_guest` translate through this record,
        // so it spans the whole contiguous host backing — including the 4 KiB LAPIC
        // MMIO page, which stays real host memory (only its KVM *mapping* is omitted
        // below). The slot index is the table's next index; a failed registration
        // rolls the record back, so a failed map never leaves a stale host pointer
        // for a later translate to dereference.
        self.regions
            .insert(gpa.0, host.as_mut_ptr(), host.len() as u64)?;
        // Register the backing as KVM memslots that leave the LAPIC MMIO page
        // (`0xFEE00000`, 4 KiB) UNMAPPED, so the guest's xAPIC accesses fault to
        // `KVM_EXIT_MMIO` → `dispatch_mmio` → the userspace deterministic `Lapic`
        // rather than being serviced from RAM by a covering memslot. (A RAM-backed
        // LAPIC page was the root cause of the runc/Postgres deadlock: the model
        // stayed at reset and its V-time timer never fired.) The region-splitting
        // LOGIC is the pure, covered + Kani-verified `split_around_hole`; here we
        // only iterate it and issue one ioctl per part. The KVM slot ids come from
        // the backend's `mem_slot_count` (NOT the logical-region count), so a split
        // map consumes a contiguous block and a later map cannot collide with this
        // split's high half. For RAM below the page there is one part, nothing holed.
        const LAPIC_MMIO_PAGE: u64 = 0xFEE0_0000;
        let host_base = host.as_ptr() as u64;
        let base_slot = self.mem_slot_count;
        let mut registered = 0u32;
        // For the error-path rollback: entries are pushed only when `dirty_log`
        // is on, so the truncate target is the length at entry — NOT
        // `len - registered`, which would underflow (or eat earlier slots'
        // entries) on a partial failure of an unlogged map.
        let dirty_slots_before = self.dirty_slots.len();
        // Task 95 M2.1: register guest RAM with dirty logging on (both split
        // parts), so `harvest_dirty_gfns` can feed the O(dirty) snapshot derive.
        // Guest-inert (gate a0); `set_dirty_log_enabled(false)` is the A/B arm.
        let flags = if self.dirty_log {
            kvm_bindings::KVM_MEM_LOG_DIRTY_PAGES
        } else {
            0
        };
        for (i, part) in
            split_around_hole(gpa.0, host.len() as u64, LAPIC_MMIO_PAGE, 0x1000).enumerate()
        {
            let region = kvm_userspace_memory_region {
                slot: base_slot + i as u32,
                flags,
                guest_phys_addr: part.gpa,
                memory_size: part.size,
                userspace_addr: host_base + part.host_off,
            };
            // SAFETY (granted purpose 1): register a sub-range of the host backing
            // with KVM. The caller's `map_memory` contract guarantees `host` stays
            // live, pinned, page-aligned, and unaliased for the backend's lifetime;
            // `split_around_hole` keeps every part within `[host_base, host_base +
            // host.len())` and page-aligned (proven), so `userspace_addr` /
            // `memory_size` address only that backing. KVM retains the address and
            // the guest writes through it on every run.
            if let Err(e) = unsafe { self.vm.set_user_memory_region(region) }.map_err(kvm_err) {
                // Roll back the logical-region record AND every part already mapped
                // in THIS call (a partial split leaves no stale KVM mapping). The
                // counter is not advanced, so a retry reuses these slot ids cleanly.
                // The harvest slot table shrinks with it (no stale harvest target).
                self.regions.rollback_last();
                self.dirty_slots.truncate(dirty_slots_before);
                for j in 0..registered {
                    let undo = kvm_userspace_memory_region {
                        slot: base_slot + j,
                        flags: 0,
                        guest_phys_addr: 0,
                        memory_size: 0, // a 0-size region deletes the slot
                        userspace_addr: 0,
                    };
                    // SAFETY (granted purpose 1): `memory_size == 0` deletes a slot
                    // we just registered; it references no host memory. Best-effort
                    // cleanup on the error path.
                    let _ = unsafe { self.vm.set_user_memory_region(undo) };
                }
                return Err(e);
            }
            if self.dirty_log {
                self.dirty_slots
                    .push((base_slot + i as u32, part.gpa, part.size));
            }
            registered += 1;
        }
        self.mem_slot_count += registered;
        if !self.dirty_log {
            // A RAM slot now exists whose guest writes KVM will never log —
            // latch the harvest closed for this backend's whole lifetime (the
            // safety rule: never vouch for a set that cannot be complete, even
            // if the knob is flipped back on later).
            self.unlogged_slot = true;
        }
        Ok(())
    }

    fn harvest_dirty_gfns(&mut self) -> Result<Vec<u64>> {
        // Without the log flag the ioctl would fail per-slot anyway; answer the
        // honest capability error so callers take the full-scan path up front.
        if !self.dirty_log {
            return Err(BackendError::Unsupported {
                what: "harvest_dirty_gfns (dirty logging disabled)",
            });
        }
        // Completeness is a property of the SLOTS, not the current knob: if any
        // RAM slot was ever registered without `KVM_MEM_LOG_DIRTY_PAGES`, its
        // guest writes are permanently invisible to the log, so no harvest from
        // this backend can be vouched complete — decline forever.
        if self.unlogged_slot {
            return Err(BackendError::Unsupported {
                what: "harvest_dirty_gfns (a RAM slot was mapped without dirty logging)",
            });
        }
        let mut gfns = Vec::new();
        for &(slot, gpa, size) in &self.dirty_slots {
            // `KVM_GET_DIRTY_LOG` retrieves-and-resets the slot's bitmap. A failure
            // mid-walk leaves earlier slots already reset — safe under the harvest
            // contract: on `Err` the caller MUST full-scan (never trust a partial
            // set), and the *next* harvest window is re-armed by the caller's own
            // harvest-and-discard at its next parent point.
            let bitmap = self
                .vm
                .get_dirty_log(slot, size as usize)
                .map_err(kvm_err)?;
            crate::region::decode_dirty_bitmap(gpa, size, &bitmap, &mut gfns);
        }
        // Ascending per slot and disjoint across slots, but the slot table's order
        // is registration order — sort + dedup is the stated contract, cheap here.
        gfns.sort_unstable();
        gfns.dedup();
        Ok(gfns)
    }

    fn run(&mut self) -> Result<Exit> {
        if !self.configured() {
            return Err(BackendError::NotConfigured);
        }
        if self.pending != Pending::None {
            return Err(BackendError::PendingCompletion);
        }
        self.check_not_poisoned()?;
        self.ensure_first_run()?;
        self.enter_guest()
    }

    fn run_until(&mut self, deadline: Vtime) -> Result<Exit> {
        // The §2 inversion seam (task 47): drive the pure precise-injection planner
        // over the live PMU + KVM single-step. Arm the retired-branch overflow at
        // `deadline − skid_margin`, free-run until it kicks `KVM_RUN` out, then
        // single-step to land at EXACTLY `deadline` retired branches → `Exit::Deadline`.
        // A genuine guest exit before the deadline returns that exit instead, short
        // of `deadline`. Completion/observability discipline matches `run`.
        if !self.configured() {
            return Err(BackendError::NotConfigured);
        }
        if self.pending != Pending::None {
            return Err(BackendError::PendingCompletion);
        }
        self.check_not_poisoned()?;
        // Read the current work — proves the PMU is present + readable — but DEFER the
        // first-entry reset: per the `FirstEntryReset` invariant it is consumed only by
        // a real `KVM_RUN`, NOT here (where the classify below may pick a no-entry
        // branch). When the reset is still pending, the next real entry zeroes the
        // counter, so the run STARTS at work 0 regardless of the current (possibly
        // foreign-contaminated) reading; on a no-entry branch the reset stays pending so
        // a later real entry still re-baselines.
        let current = self.pmu_work()?;
        let start = if self.reset_arm.is_pending() {
            0
        } else {
            current
        };
        // P1 round-8 — the complete run_until contract (deadline vs current work), a
        // pure decision in the gated portable layer; see `classify_run_until`.
        // Scope the adapter's `&mut self` borrow so cleanup can use `self` after.
        let outcome = {
            match classify_run_until(deadline.0, start) {
                // deadline > current: drive the planner to EXACTLY the deadline.
                RunUntilStart::Drive => {
                    // A real entry follows → NOW consume the first-entry reset (the only
                    // place run_until may, per the invariant). After the reset the
                    // counter reads 0, matching `start` on the pending path.
                    self.ensure_first_run()?;
                    let planner = InjectionPlanner::new(PlannerConfig {
                        skid_margin: SKID_MARGIN,
                    });
                    let mut cpu = LiveCpu {
                        backend: self,
                        deadline: deadline.0,
                        work_cache: start,
                        pending_exit: None,
                        err: None,
                    };
                    drive_run_until(&planner, &mut cpu, deadline.0)
                }
                // deadline <= current: at OR PAST the deadline → fire the timer NOW with
                // ZERO guest steps (never step a guest instruction past the deadline —
                // round-8 P1). The `<` (overdue) case is LEGITIMATE, not an error
                // (round-12 P1): `preemption_deadline()` derives the absolute deadline
                // from a stale `last_intercept_work`, so the live count can already be
                // past it (Postgres/Linux re-arm LAPIC one-shots constantly) — an overdue
                // timer fires immediately, the VM does NOT abort. Same fire-now outcome as
                // the planner's `TargetInPast` for the in-flight skid case. NO `KVM_RUN`
                // happens, so the first-entry reset stays PENDING (round-11 invariant): a
                // later real entry re-baselines, so a coexisting VM in between cannot
                // contaminate this VM's counter. Any completion staged by the prior step
                // stays in the run page and is committed by the NEXT entry's `KVM_RUN`.
                RunUntilStart::AtOrPastDeadline => Ok(Exit::Deadline {
                    reached: Vtime(start),
                }),
            }
        };
        // Cleanup MUST run and MUST succeed (P2): a failed single-step disarm leaves
        // the vCPU single-stepping and a failed PMU disarm leaves the overflow armed,
        // either of which corrupts the next `run()` (a stray `KVM_EXIT_DEBUG` / SIGIO).
        // Attempt both, then propagate the first failure — fail closed, never return
        // the exit as success over a backend left in a broken state.
        let step_cleanup = self.disable_single_step();
        let pmu_cleanup = self.pmu_disarm();
        step_cleanup?;
        pmu_cleanup?;
        let exit = outcome?;
        // P2 round-12: the exit is now genuinely DELIVERED to the caller (cleanup succeeded,
        // `drive_run_until` did not reject it) — clear the exit poison here, the single
        // point past every fallible step. If `take_guest_exit_stop` armed it for a decoded
        // guest exit, this is where it is consumed; for a `Deadline` outcome (no guest exit
        // decoded) the poison was never armed and this is a no-op.
        self.exit_poison.delivered();
        // P1 round-4: a `Deadline` is ONLY ever the no-guest-exit land (the single-step
        // stopped AT the deadline branch), so it never carries a pending completion —
        // a real exit at/ past the deadline now fails closed in `drive_run_until`
        // instead of being absorbed. So there is nothing to clear here. (If a future
        // regression ever left a stale pending, the `PendingCompletion` guard at the
        // top of the next `run`/`run_until` fails closed loudly — never a silent drop.)
        debug_assert!(
            !matches!(exit, Exit::Deadline { .. }) || self.pending == Pending::None,
            "a Deadline must not carry a pending completion (it is the no-exit land)"
        );
        self.counts.bump(exit.reason());
        Ok(exit)
    }

    fn inject(&mut self, event: Event) -> Result<()> {
        match event {
            // Set the pending maskable vector (overwrite); same as set_pending_irq.
            Event::Interrupt { vector } => {
                self.pending_irq = Some(vector);
                Ok(())
            }
            // NMIs are non-maskable; queue immediately via the KVM_NMI ioctl. (Not
            // needed by the Linux boot — timer IRQ only — but honoured so the trait
            // method is complete.)
            Event::Nmi => self.vcpu.nmi().map_err(kvm_err),
        }
    }

    fn set_pending_irq(&mut self, vector: Option<u8>) -> Result<()> {
        // Overwrite the single pending-IRQ slot with the VMM's freshly re-arbitrated
        // vector. `None` clears it; the next `enter_guest` then disarms the window
        // (via `plan_irq_entry(None)`), so a previously-armed vector that is no
        // longer the highest deliverable (TPR raised / EOI'd / preempted) is not
        // injected stale. The actual KVM_INTERRUPT / window handshake runs in
        // `enter_guest` against the current `ready_for_interrupt_injection`.
        self.pending_irq = vector;
        Ok(())
    }

    fn take_accepted_interrupt(&mut self) -> Option<u8> {
        self.accepted_irq.pop_front()
    }

    fn complete_read(&mut self, value: u64) -> Result<()> {
        // A pending KVM_EXIT_DETERMINISM (patched backend) completes through the
        // determinism payload, not the IO/MMIO/MSR data buffers. RDTSCP also
        // needs the guest's IA32_TSC_AUX → ECX (read here, below the trait, as a
        // faithful instruction completion); the seeded value itself is computed
        // above the trait in vmm-core. Stock KVM never sets this pending.
        if let Pending::Determinism { rdtscp, .. } = self.pending {
            let aux = if rdtscp { self.read_tsc_aux()? } else { 0 };
            apply_complete_determinism(self.run_page(), self.pending, value, aux)?;
            self.pending = Pending::None;
            return Ok(());
        }
        apply_complete_read(self.run_page(), self.pending, value)?;
        self.pending = Pending::None;
        Ok(())
    }

    fn complete_fault(&mut self) -> Result<()> {
        apply_complete_fault(self.run_page(), self.pending)?;
        self.pending = Pending::None;
        Ok(())
    }

    fn complete_ok(&mut self) -> Result<()> {
        apply_complete_ok(self.run_page(), self.pending)?;
        self.pending = Pending::None;
        Ok(())
    }

    fn complete_hypercall(&mut self, _rax: u64) -> Result<()> {
        // Stock KVM services VMCALL in-kernel; it never surfaces Exit::Hypercall,
        // so there is never a hypercall pending to complete.
        Err(BackendError::NoPendingRead)
    }

    fn complete_cpuid(&mut self, _eax: u32, _ebx: u32, _ecx: u32, _edx: u32) -> Result<()> {
        // Stock KVM answers CPUID in-kernel from the set_cpuid table; it never
        // surfaces Exit::Cpuid.
        Err(BackendError::BadCompletion)
    }

    fn save(&self) -> Result<VcpuState> {
        let regs = self.vcpu.get_regs().map_err(kvm_err)?;
        // SAFETY: `vcpu` is a valid vCPU fd; `raw_get_sregs2` writes a full
        // `kvm_sregs2` (incl. flags/PDPTRs). Excluded under Miri.
        let sregs2 = unsafe { raw_get_sregs2(self.vcpu.as_raw_fd())? };
        let dregs = self.vcpu.get_debug_regs().map_err(kvm_err)?;
        let kevents = self.vcpu.get_vcpu_events().map_err(kvm_err)?;
        let mp = self.vcpu.get_mp_state().map_err(kvm_err)?;
        let xcrs = self.vcpu.get_xcrs().map_err(kvm_err)?;
        let xsave = self.save_xsave()?;
        let msrs = self.save_msrs()?;

        Ok(VcpuState {
            regs: from_kvm_regs(&regs),
            sregs: from_kvm_sregs2(&sregs2),
            xcr0: xcr0_of(&xcrs),
            debugregs: from_kvm_debugregs(&dregs),
            events: from_kvm_events(&kevents),
            mp_state: mp_from_kvm(mp.mp_state),
            msrs,
            xsave,
        })
    }

    fn restore(&mut self, state: &VcpuState) -> Result<()> {
        // Fail closed *before any `SET_*` ioctl* (no half-mutation of the live
        // vCPU): the snapshot's MSR key set must equal the configured allow-stateful
        // indices, and the XSAVE image must be the host image size.
        let xsave_len = self.xsave2_size.unwrap_or(size_of::<kvm_xsave>());
        validate_restore_shape(state, self.msr_filter.as_ref(), xsave_len)?;

        self.vcpu
            .set_regs(&to_kvm_regs(&state.regs))
            .map_err(kvm_err)?;
        // SAFETY: `vcpu` is valid; `raw_set_sregs2` reads a full `kvm_sregs2`
        // (incl. flags/PDPTRs preserved from `save`). Excluded under Miri.
        unsafe { raw_set_sregs2(self.vcpu.as_raw_fd(), &to_kvm_sregs2(&state.sregs))? };
        self.vcpu
            .set_debug_regs(&to_kvm_debugregs(&state.debugregs))
            .map_err(kvm_err)?;
        self.vcpu
            .set_vcpu_events(&to_kvm_events(&state.events))
            .map_err(kvm_err)?;
        let mp = kvm_mp_state {
            mp_state: mp_to_kvm(state.mp_state),
        };
        self.vcpu.set_mp_state(mp).map_err(kvm_err)?;
        self.vcpu.set_xcrs(&xcrs_of(state.xcr0)).map_err(kvm_err)?;
        self.restore_xsave(&state.xsave)?;
        self.restore_msrs(state)?;
        // P1(b): re-arm the first-entry PMU reset rather than resetting now. Snapshot
        // restore zeroes the V-time work counter, but the box `perf_event` counter is
        // shared across the vCPU thread: if another (coexisting) VM runs between this
        // restore and the restored VM's NEXT entry, resetting *here* would let that
        // VM's guest branches accumulate into the backend counter, diverging it from
        // vmm-core's V-time counter (which re-arms its own first-entry reset — see
        // `Vmm::restore_vm_state`). Deferring the reset to `ensure_first_run` at the
        // next entry excludes the foreign branches and keeps the B≡A invariant across
        // a restore (the explorer / branching N-VM path, task 48). The discipline is
        // pinned by [`FirstEntryReset`]'s portable stateful property test.
        self.reset_arm.rearm();
        Ok(())
    }

    fn exit_counts(&self) -> ExitCounts {
        self.counts
    }

    fn reset_exit_counts(&mut self) {
        self.counts = ExitCounts::default();
    }

    fn capabilities(&self) -> Capabilities {
        kvm_capabilities()
    }
}
