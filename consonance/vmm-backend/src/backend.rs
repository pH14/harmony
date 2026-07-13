// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `Backend` trait — the trap apparatus decoupled from the deterministic VMM
//! above it (ruling R-Backend).
//!
//! One impl per substrate; **nothing above this trait may branch on which one**.
//! The trait is **object-safe / dyn-compatible** so the binary's composition root
//! can hold a `Box<dyn Backend>` and inject `KvmBackend` vs `PatchedKvmBackend`
//! at `fn main` — no generic methods, no `Self`-by-value returns.

use crate::config::{CpuidModel, MsrFilter};
use crate::error::Result;
use crate::exit::{Capabilities, Exit, ExitCounts, Injection};
use crate::state::VcpuState;
use crate::types::{Gpa, Moment};

/// The trap apparatus, decoupled from the deterministic VMM above it.
///
/// See the crate docs for the run-loop / completion contract. Implementations:
/// [`MockBackend`](crate::MockBackend) (portable, deterministic, for tests) and
/// `KvmBackend` (Linux-only, the bring-up stock-KVM impl).
pub trait Backend {
    // --- configuration (installed once, before the first run) ----------------

    /// Install the frozen guest-visible CPUID model (`KVM_SET_CPUID2` on KVM).
    /// MUST be called before the first `run`/`run_until`; otherwise the guest
    /// would see KVM's host-derived defaults (boot- and determinism-breaking).
    fn set_cpuid(&mut self, model: &CpuidModel) -> Result<()>;

    /// Install the default-deny MSR policy. On KVM this enables
    /// `KVM_CAP_X86_USER_SPACE_MSR` with the full mask
    /// (`FILTER | UNKNOWN | INVAL`, CPU-MSR-CONTRACT §1) **then**
    /// `KVM_X86_SET_MSR_FILTER`, so a denied/unknown/invalid MSR access surfaces
    /// as `Exit::Rdmsr`/`Exit::Wrmsr` (loud) instead of a silent in-kernel `#GP`.
    fn set_msr_filter(&mut self, filter: &MsrFilter) -> Result<()>;

    // --- memory ---------------------------------------------------------------

    /// Map a guest-physical region to host-owned, pinned, pre-populated backing
    /// store (no demand paging — a determinism choice). `gpa` and `host.len()`
    /// MUST be 4 KiB-aligned. Bring-up uses a single memslot;
    /// overlapping/duplicate maps error.
    ///
    /// # Safety
    /// The caller MUST guarantee that `host`'s backing (a) stays live at a fixed
    /// address — pinned, never reallocated or moved — until the backend is
    /// dropped or the region is replaced; (b) is not aliased by any other live
    /// `&`/`&mut` while a `run`/`run_until` is in flight; and (c) starts at a
    /// **4 KiB-aligned host address** (`host.as_ptr() as usize % 4096 == 0`).
    /// `KVM_SET_USER_MEMORY_REGION` requires the *userspace address itself* to be
    /// page-aligned, which a plain `Vec<u8>`/slice does NOT guarantee (KVM
    /// rejects it with `EINVAL`) — back the region with an `mmap`/page-aligned
    /// allocation. Violating (a)/(b) is a use-after-free or data race; that
    /// unenforceable invariant is why this is `unsafe`. The backend records the
    /// region and retains `host`'s pointer past this call — the `&mut [u8]`
    /// borrow ends at return, but the guest writes through that pointer during
    /// every later `run`.
    unsafe fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> Result<()>;

    /// Harvest-and-reset the backend's **guest-write dirty-page log** (task 95
    /// M2.1): return the guest frame numbers dirtied *by guest execution* since
    /// the previous harvest (or since the region was mapped), **sorted ascending
    /// and deduplicated**, and atomically reset the log so the next harvest
    /// covers exactly the span from this call. On KVM this is `KVM_GET_DIRTY_LOG`
    /// (retrieve-and-reset) per RAM memslot, decoded and translated back to
    /// absolute gfns.
    ///
    /// **A cost hint, never a correctness input.** Callers use the result only
    /// to bound how much memory a snapshot capture re-reads; on `Err` they MUST
    /// fall back to a full scan (which is correct-by-dedup), never fail the
    /// operation, and never trust a set they cannot prove complete. An
    /// over-report (superset) is harmless — capture-side dedup discards no-op
    /// writes; an implementation must never under-report a guest write.
    /// **Host-side writes through the mapped backing are invisible to this log**
    /// (KVM tracks sptes, not the userspace mapping) — the layer that writes
    /// guest RAM from the host must track those itself.
    ///
    /// The default returns [`Unsupported`](crate::BackendError::Unsupported): a
    /// backend without dirty tracking simply makes every caller take the
    /// full-scan path.
    fn harvest_dirty_gfns(&mut self) -> Result<Vec<u64>> {
        Err(crate::error::BackendError::Unsupported {
            what: "harvest_dirty_gfns",
        })
    }

    // --- run loop -------------------------------------------------------------

    /// Run the vCPU until an exit needs the VMM. Blocking. The returned `Exit` is
    /// the ONLY channel by which the guest becomes observable. Before resuming a
    /// read-style, `Wrmsr`, `Hypercall`, or `Cpuid` exit, the VMM MUST call the
    /// matching completion method; calling `run` again with such an exit
    /// un-serviced is [`PendingCompletion`](crate::BackendError::PendingCompletion)
    /// (fail closed). Increments the per-reason counter for the exit it returns.
    /// Returns [`NotConfigured`](crate::BackendError::NotConfigured) if called
    /// before `set_cpuid` AND `set_msr_filter` have both succeeded.
    fn run(&mut self) -> Result<Exit>;

    /// Run until an exact V-time (retired-branch) deadline, then exit with
    /// `Exit::Deadline` — the §2 inversion seam (PMU overflow-early + single-step
    /// under the hood; task 07 supplies the skid margin). A guest exit before the
    /// deadline returns that exit instead, short of `deadline`. **Bring-up
    /// `KvmBackend` returns [`Unsupported`](crate::BackendError::Unsupported)`{ what: "run_until" }`** —
    /// the live PMU/single-step path is Phase 2 (needs task 07 + the lapic
    /// injection seam); the trait declares it now so task 15 can compile against
    /// it.
    fn run_until(&mut self, deadline: Moment) -> Result<Exit>;

    /// Inject an **NMI** (`KVM_NMI`) immediately, or set the pending maskable-IRQ
    /// vector (equivalent to [`set_pending_irq`](Backend::set_pending_irq)`(Some(v))`).
    /// The VMM decides WHEN (a V-time boundary). For the V-time LAPIC timer the VMM
    /// drives the maskable path through [`set_pending_irq`](Backend::set_pending_irq)
    /// directly (re-arbitrated each entry); `inject` exists for the NMI path and as
    /// a one-shot maskable convenience.
    fn inject(&mut self, event: Injection) -> Result<()>;

    /// Set (overwrite) the single pending **maskable** IRQ vector to inject at the
    /// next injectable VM-entry — `None` clears it (and disarms the interrupt
    /// window). The backend holds **one** vector, not a queue: the VMM owns the
    /// userspace LAPIC, whose IRR *is* the multi-IRQ queue, and **re-arbitrates**
    /// (re-peeks the current highest-priority deliverable vector) at **every** entry,
    /// overwriting this slot. So a vector is never injected stale — if the guest
    /// raised TPR or a higher-priority IRQ arrived since (any LAPIC access exits to
    /// the VMM), the next entry's call passes the re-arbitrated vector (or `None`) —
    /// and a second/lower IRQ is never dropped (it stays in the LAPIC IRR).
    ///
    /// The backend delivers the set vector at the next entry: `KVM_INTERRUPT` when
    /// the guest can take it, else it arms the interrupt window and retries on
    /// `KVM_EXIT_IRQ_WINDOW_OPEN`. A vector becomes observable as *accepted* only
    /// once `KVM_INTERRUPT` is actually issued — see [`take_accepted_interrupt`].
    ///
    /// [`take_accepted_interrupt`]: Backend::take_accepted_interrupt
    fn set_pending_irq(&mut self, vector: Option<u8>) -> Result<()>;

    /// Drain (and return) the next maskable-IRQ vector the backend has **accepted**
    /// into the guest — i.e. for which `KVM_INTERRUPT` was actually issued — since
    /// the last call; `None` when none is pending report.
    ///
    /// The VMM models its userspace LAPIC's IRR→ISR transition as *interrupt
    /// acceptance*, which happens inside the backend on VM-entry. So the VMM leaves
    /// a vector pending in the LAPIC IRR when it sets it via
    /// [`set_pending_irq`](Backend::set_pending_irq), and completes the IRR→ISR
    /// transition only when this method reports the vector accepted — keeping the
    /// register file (and any snapshot taken while a vector waits on the interrupt
    /// window) showing it pending, not prematurely in-service. Backends that never
    /// accept a maskable IRQ return `None`.
    fn take_accepted_interrupt(&mut self) -> Option<u8>;

    // --- exit completion (the read/write/hypercall round-trip) ----------------

    /// Supply the value for a pending **read-style** exit: `Io { write: None }`,
    /// `Mmio { write: None }`, `Rdmsr`, or an instruction-read exit (`Rdtsc`,
    /// `Rdtscp`, `Rdrand`, `Rdseed`). The low `size`/`width` bytes are delivered
    /// to the guest's destination. Errors
    /// [`NoPendingRead`](crate::BackendError::NoPendingRead) if no read-style exit
    /// is pending. (Stock `KvmBackend` never surfaces the instruction-read exits,
    /// so it completes only IO/MMIO/MSR reads; the instruction-read completions
    /// exist for `PatchedKvmBackend`/`DirectVmxBackend`.)
    fn complete_read(&mut self, value: u64) -> Result<()>;

    /// The contract's `deny-gp` disposition for a pending `Rdmsr`/`Wrmsr`: inject
    /// `#GP` into the guest (on KVM, set `kvm_run.msr.error != 0`). Errors
    /// [`BadCompletion`](crate::BackendError::BadCompletion) if the pending exit
    /// is not an MSR exit.
    fn complete_fault(&mut self) -> Result<()>;

    /// Resolve a pending `Wrmsr` whose contract disposition is **not** `deny-gp`:
    /// `allow` (the write is acknowledged) or `deny-ignore` (the write is
    /// dropped). On KVM both resume with `kvm_run.msr.error == 0`; the
    /// apply-vs-drop distinction is the VMM's own bookkeeping. Errors
    /// [`BadCompletion`](crate::BackendError::BadCompletion) if the pending exit
    /// is not a `Wrmsr`.
    fn complete_ok(&mut self) -> Result<()>;

    /// Set guest `RAX` (the response-frame length per INTEGRATION.md §1, or 0 on
    /// transport error) for a pending `Hypercall`. Errors if none pending.
    fn complete_hypercall(&mut self, rax: u64) -> Result<()>;

    /// Supply the four result registers `(eax, ebx, ecx, edx)` for a pending
    /// `Exit::Cpuid`. Stock `KvmBackend` never surfaces `Cpuid` (it answers
    /// in-kernel from the `set_cpuid` table); a backend that does gets the
    /// dyn-overlaid quad from vmm-core. Errors
    /// [`BadCompletion`](crate::BackendError::BadCompletion) if the pending exit
    /// is not a `Cpuid`.
    fn complete_cpuid(&mut self, eax: u32, ebx: u32, ecx: u32, edx: u32) -> Result<()>;

    // --- snapshot / restore ---------------------------------------------------

    /// Full guest-visible vCPU state for snapshot/restore. `[refinement]`:
    /// fallible here — the underlying `KVM_GET_*` ioctls can fail and library
    /// code must not `unwrap` (rule #4).
    fn save(&self) -> Result<VcpuState>;

    /// Restore a `VcpuState` produced by `save`. Validates internal consistency;
    /// [`InvalidState`](crate::BackendError::InvalidState) on a
    /// malformed/incompatible blob (never a panic).
    fn restore(&mut self, state: &VcpuState) -> Result<()>;

    // --- observability (R-Backend normative) ----------------------------------

    /// Per-exit-reason trap counts since the last reset. **Recorded every run**
    /// and surfaced in the unison report; the empirical input that gates the
    /// deferred RDTSC optimization. Cheap, always on. Deterministic order.
    fn exit_counts(&self) -> ExitCounts;

    /// Reset every per-reason counter to zero.
    fn reset_exit_counts(&mut self);

    /// What determinism this backend can and cannot honestly provide. The
    /// unison report reads this to refuse to *claim* determinism for a
    /// payload that needs a capability the backend lacks.
    fn capabilities(&self) -> Capabilities;
}

/// Blanket forward so the composition root can inject a concrete backend as a
/// `Box<dyn Backend>` and run a `Vmm` over it (R-Backend / task-21 P5: the one
/// place a concrete backend is named is `fn main`; everything above the trait is
/// backend-agnostic). `Backend` is dyn-compatible (no generic methods, no
/// `Self`-by-value returns), so `Box<dyn Backend>` is a `Backend` too.
impl<B: Backend + ?Sized> Backend for Box<B> {
    fn set_cpuid(&mut self, model: &CpuidModel) -> Result<()> {
        (**self).set_cpuid(model)
    }

    fn set_msr_filter(&mut self, filter: &MsrFilter) -> Result<()> {
        (**self).set_msr_filter(filter)
    }

    unsafe fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> Result<()> {
        // SAFETY: the caller upholds `map_memory`'s contract; we only forward the
        // call to the boxed backend, adding no new obligation.
        unsafe { (**self).map_memory(gpa, host) }
    }

    fn harvest_dirty_gfns(&mut self) -> Result<Vec<u64>> {
        // Explicit forward: without this the default (Unsupported) body would
        // shadow the boxed backend's real dirty log.
        (**self).harvest_dirty_gfns()
    }

    fn run(&mut self) -> Result<Exit> {
        (**self).run()
    }

    fn run_until(&mut self, deadline: Moment) -> Result<Exit> {
        (**self).run_until(deadline)
    }

    fn inject(&mut self, event: Injection) -> Result<()> {
        (**self).inject(event)
    }

    fn set_pending_irq(&mut self, vector: Option<u8>) -> Result<()> {
        (**self).set_pending_irq(vector)
    }

    fn take_accepted_interrupt(&mut self) -> Option<u8> {
        (**self).take_accepted_interrupt()
    }

    fn complete_read(&mut self, value: u64) -> Result<()> {
        (**self).complete_read(value)
    }

    fn complete_fault(&mut self) -> Result<()> {
        (**self).complete_fault()
    }

    fn complete_ok(&mut self) -> Result<()> {
        (**self).complete_ok()
    }

    fn complete_hypercall(&mut self, rax: u64) -> Result<()> {
        (**self).complete_hypercall(rax)
    }

    fn complete_cpuid(&mut self, eax: u32, ebx: u32, ecx: u32, edx: u32) -> Result<()> {
        (**self).complete_cpuid(eax, ebx, ecx, edx)
    }

    fn save(&self) -> Result<VcpuState> {
        (**self).save()
    }

    fn restore(&mut self, state: &VcpuState) -> Result<()> {
        (**self).restore(state)
    }

    fn exit_counts(&self) -> ExitCounts {
        (**self).exit_counts()
    }

    fn reset_exit_counts(&mut self) {
        (**self).reset_exit_counts()
    }

    fn capabilities(&self) -> Capabilities {
        (**self).capabilities()
    }
}
