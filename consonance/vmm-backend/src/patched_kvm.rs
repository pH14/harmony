// SPDX-License-Identifier: AGPL-3.0-or-later
//! `PatchedKvmBackend` — the first **determinism-complete** [`Backend`]
//! (`#[cfg(target_os = "linux")]`, box-only), ruling R-Backend's ratified
//! determinism baseline.
//!
//! It is a thin wrapper over [`KvmBackend`] that opts into the out-of-tree
//! determinism patch (`consonance/vmm-backend/kvm-patches/`, GO'd by task 16): the
//! constructor enables `KVM_CAP_X86_DETERMINISTIC_INTERCEPTS` **before** vCPU
//! creation, so the kernel traps `RDTSC`/`RDTSCP`/`RDRAND`/`RDSEED` to
//! userspace as `KVM_EXIT_DETERMINISM`. Those surface through the **shared**
//! pure [`crate::kvm`] decode/complete helpers as
//! [`X86Exit::Rdtsc`]/[`X86Exit::Rdtscp`]/[`X86Exit::Rdrand`]/[`X86Exit::Rdseed`], which the
//! VMM resolves to a V-time TSC / a seeded RNG draw above the trait — so
//! `capabilities()` honestly reports `deterministic_tsc`/`deterministic_rng`.
//!
//! Everything else (CPUID/MSR-filter install, memory mapping, the run loop,
//! save/restore, exit counting) is **identical to stock KVM** and delegated to
//! the inner [`KvmBackend`] verbatim — the patch surfaces four extra exits and
//! nothing more (it leaves the TSC offset/scaling and TSC-deadline machinery
//! untouched; see the spike's patch 0003). The backend stays a **thin KVM
//! wrapper**: it surfaces and completes the determinism exits and computes no
//! deterministic value itself (the V-time TSC and the seeded RNG bytes are
//! computed in vmm-core, above the trait — R-Backend's hard layering rule). The
//! one completion detail it owns, `RDTSCP`'s `ECX = IA32_TSC_AUX`, reflects
//! guest architectural state (read via `KVM_GET_MSRS` in
//! [`KvmBackend::complete_read`]), not contract policy.
//!
//! Like [`crate::kvm_sys`], this module is **box-only syscall orchestration**
//! (it cannot run without the patched `/dev/kvm`) and is excluded from the
//! coverage + mutation gates; its logic — the `KVM_EXIT_DETERMINISM` decode and
//! completion — lives in the pure, unit-tested [`crate::kvm`] module and is
//! exercised on macOS via a scripted [`crate::MockBackend`] determinism exit
//! plus vmm-core's completion path.

use crate::arch::x86::{Injection, VcpuState, X86, X86Caps, X86Completion, X86Policy};
use crate::backend::Backend;
use crate::error::Result;
use crate::exit::{Capabilities, Exit, ExitCounts};
use crate::kvm::patched_capabilities;
use crate::kvm_sys::KvmBackend;
use crate::types::{Gpa, Moment};

/// The patched-KVM determinism backend (R-Backend baseline). Wraps an inner
/// [`KvmBackend`] built with `KVM_CAP_X86_DETERMINISTIC_INTERCEPTS` enabled.
pub struct PatchedKvmBackend {
    inner: KvmBackend,
}

impl PatchedKvmBackend {
    /// Open `/dev/kvm`, enable the determinism intercepts (before vCPU
    /// creation), then create the VM/vCPU exactly as [`KvmBackend::new`] does.
    /// Returns [`crate::BackendError::Capability`] if the patched modules are not
    /// loaded (the cap is absent / `KVM_ENABLE_CAP` fails).
    pub fn new() -> Result<PatchedKvmBackend> {
        Ok(PatchedKvmBackend {
            inner: KvmBackend::build(true)?,
        })
    }

    /// Copy `bytes` into guest memory at `gpa` (the loader path) — forwarded to
    /// the inner [`KvmBackend`].
    pub fn write_guest(&mut self, gpa: Gpa, bytes: &[u8]) -> Result<()> {
        self.inner.write_guest(gpa, bytes)
    }

    /// Copy guest memory at `gpa` into `buf` (the result-read path) — forwarded
    /// to the inner [`KvmBackend`].
    pub fn read_guest(&self, gpa: Gpa, buf: &mut [u8]) -> Result<()> {
        self.inner.read_guest(gpa, buf)
    }

    /// Enable/disable dirty logging on subsequently-mapped memslots (task 95
    /// M2.1) — forwarded to [`KvmBackend::set_dirty_log_enabled`].
    pub fn set_dirty_log_enabled(&mut self, enabled: bool) {
        self.inner.set_dirty_log_enabled(enabled);
    }

    /// Cumulative overflow-ring record counts for the `run_until` branch counter
    /// — forwarded to [`KvmBackend::pmu_overflow_stats`] (the per-record PMI
    /// accounting instrument of the nested-x86 re-certification, bead hm-b5b).
    pub fn pmu_overflow_stats(&self) -> Option<crate::pmu::PmuOverflowStats> {
        self.inner.pmu_overflow_stats()
    }
}

impl Backend for PatchedKvmBackend {
    type A = X86;

    fn set_policy(&mut self, policy: &X86Policy) -> Result<()> {
        self.inner.set_policy(policy)
    }

    unsafe fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> Result<()> {
        // SAFETY: the caller upholds `Backend::map_memory`'s contract (host stays
        // live, pinned, page-aligned, unaliased); we only forward it unchanged.
        unsafe { self.inner.map_memory(gpa, host) }
    }

    fn harvest_dirty_gfns(&mut self) -> Result<Vec<u64>> {
        // Explicit forward (the trait default would shadow the inner dirty log).
        self.inner.harvest_dirty_gfns()
    }

    fn run(&mut self) -> Result<Exit<X86>> {
        self.inner.run()
    }

    fn run_until(&mut self, deadline: Moment) -> Result<Exit<X86>> {
        self.inner.run_until(deadline)
    }

    fn inject(&mut self, event: Injection) -> Result<()> {
        self.inner.inject(event)
    }

    fn set_pending_irq(&mut self, vector: Option<u8>) -> Result<()> {
        self.inner.set_pending_irq(vector)
    }

    fn take_accepted_interrupt(&mut self) -> Option<u8> {
        self.inner.take_accepted_interrupt()
    }

    fn complete_read(&mut self, value: u64) -> Result<()> {
        self.inner.complete_read(value)
    }

    fn complete_fault(&mut self) -> Result<()> {
        self.inner.complete_fault()
    }

    fn complete_ok(&mut self) -> Result<()> {
        self.inner.complete_ok()
    }

    fn complete_hypercall(&mut self, ret: u64) -> Result<()> {
        self.inner.complete_hypercall(ret)
    }

    fn complete_arch(&mut self, completion: X86Completion) -> Result<()> {
        self.inner.complete_arch(completion)
    }

    fn save(&self) -> Result<VcpuState> {
        self.inner.save()
    }

    fn restore(&mut self, state: &VcpuState) -> Result<()> {
        self.inner.restore(state)
    }

    fn exit_counts(&self) -> ExitCounts {
        self.inner.exit_counts()
    }

    fn reset_exit_counts(&mut self) {
        self.inner.reset_exit_counts()
    }

    /// The one method that differs from stock KVM: honestly report determinism
    /// completeness (the four intercepts are surfaced + V-time/seed-resolved).
    fn capabilities(&self) -> Capabilities<X86Caps> {
        patched_capabilities()
    }
}
