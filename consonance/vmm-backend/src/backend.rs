// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `Backend` trait — the trap apparatus decoupled from the deterministic VMM
//! above it (ruling R-Backend), generic over the ISA it traps
//! (`docs/ARCH-BOUNDARY.md` §A).
//!
//! One impl per (substrate, arch) pair; **nothing above this trait may branch on
//! which substrate is in use, and nothing above the arch seam may branch on
//! which ISA is in use**. The trait is **object-safe / dyn-compatible** so the
//! binary's composition root can hold a `Box<dyn Backend<A = X86>>` and inject
//! `KvmBackend` vs `PatchedKvmBackend` at `fn main` — no generic methods, no
//! `Self`-by-value returns. The composition root is the one place a concrete
//! `(Backend impl, Arch vendor)` pair is named.
//!
//! **Designed, NOT frozen.** This trait's shape (and `run_until`'s
//! late-only-stop contract, which stays exactly as-is) is the ruled §A design;
//! the AA-3 trait-freeze memo (the ARM spike) owns the freeze decision — ARM's
//! PMU-overflow-to-exit path may pressure `run_until` before the trait may be
//! declared frozen. Do not treat compiles-for-x86 as frozen-for-every-vendor.

use crate::arch::Arch;
use crate::error::Result;
use crate::exit::{Capabilities, Exit, ExitCounts};
use crate::types::{Gpa, Moment};

/// The trap apparatus, decoupled from the deterministic VMM above it.
///
/// See the crate docs for the run-loop / completion contract. Implementations:
/// [`MockBackend`](crate::MockBackend) (portable, deterministic, for tests) and
/// `KvmBackend` (Linux-only, the bring-up stock-KVM impl), both over the
/// [`X86`](crate::arch::x86::X86) vendor.
pub trait Backend {
    /// The ISA this backend traps ([`Arch`]); the vendor is a zero-sized type.
    type A: Arch;

    // --- configuration (installed once, before the first run) ----------------

    /// Install the frozen guest-visible CPU-contract policy — on x86 the CPUID
    /// model (`KVM_SET_CPUID2`) **then** the default-deny MSR filter
    /// (`KVM_CAP_X86_USER_SPACE_MSR` with the full mask
    /// `FILTER | UNKNOWN | INVAL`, then `KVM_X86_SET_MSR_FILTER`), so a
    /// denied/unknown/invalid MSR access surfaces as an exit (loud) instead of
    /// a silent in-kernel `#GP`. MUST be called before the first
    /// `run`/`run_until`; otherwise the guest would see the host-derived
    /// defaults (boot- and determinism-breaking).
    fn set_policy(&mut self, policy: &<Self::A as Arch>::Policy) -> Result<()>;

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
    /// read-style, MSR, `Hypercall`, or `Cpuid` exit, the VMM MUST call the
    /// matching completion method; calling `run` again with such an exit
    /// un-serviced is [`PendingCompletion`](crate::BackendError::PendingCompletion)
    /// (fail closed). Increments the per-reason counter for the exit it returns.
    /// Returns [`NotConfigured`](crate::BackendError::NotConfigured) if called
    /// before `set_policy` has succeeded.
    fn run(&mut self) -> Result<Exit<Self::A>>;

    /// Run until an exact V-time (retired-branch) deadline, then exit with
    /// `CommonExit::Deadline` — the §2 inversion seam (PMU overflow-early +
    /// single-step under the hood; task 07 supplies the skid margin). A guest
    /// exit before the deadline returns that exit instead, short of `deadline`.
    /// The **late-only-stop contract stays exactly as ruled** (see the trait
    /// docs' freeze note). **Bring-up `KvmBackend` returns
    /// [`Unsupported`](crate::BackendError::Unsupported)`{ what: "run_until" }`.**
    fn run_until(&mut self, deadline: Moment) -> Result<Exit<Self::A>>;

    /// Inject an event immediately (x86: an **NMI** via `KVM_NMI`), or set the
    /// pending maskable-IRQ identity (equivalent to
    /// [`set_pending_irq`](Backend::set_pending_irq)`(Some(id))`). The VMM
    /// decides WHEN (a V-time boundary). For the V-time timer the VMM drives
    /// the maskable path through [`set_pending_irq`](Backend::set_pending_irq)
    /// directly (re-arbitrated each entry); `inject` exists for the
    /// non-maskable path and as a one-shot maskable convenience.
    fn inject(&mut self, event: <Self::A as Arch>::Injection) -> Result<()>;

    /// Set (overwrite) the single pending **maskable** interrupt identity
    /// ([`Arch::IntId`]) to inject at the next injectable VM-entry — `None`
    /// clears it (and disarms the interrupt window). The backend holds **one**
    /// identity, not a queue: the VMM owns the userspace interrupt fabric,
    /// whose pending-register file *is* the multi-IRQ queue, and
    /// **re-arbitrates** (re-peeks the current highest-priority deliverable
    /// identity) at **every** entry, overwriting this slot. So an identity is
    /// never injected stale — if the guest raised its priority threshold or a
    /// higher-priority IRQ arrived since (any fabric access exits to the VMM),
    /// the next entry's call passes the re-arbitrated identity (or `None`) —
    /// and a second/lower IRQ is never dropped (it stays pending in the
    /// fabric).
    ///
    /// The backend delivers the set identity at the next entry: `KVM_INTERRUPT`
    /// when the guest can take it, else it arms the interrupt window and
    /// retries on `KVM_EXIT_IRQ_WINDOW_OPEN`. An identity becomes observable as
    /// *accepted* only once `KVM_INTERRUPT` is actually issued — see
    /// [`take_accepted_interrupt`].
    ///
    /// [`take_accepted_interrupt`]: Backend::take_accepted_interrupt
    fn set_pending_irq(&mut self, id: Option<<Self::A as Arch>::IntId>) -> Result<()>;

    /// Drain (and return) the next maskable interrupt identity the backend has
    /// **accepted** into the guest — i.e. for which `KVM_INTERRUPT` was actually
    /// issued — since the last call; `None` when none is pending report.
    ///
    /// The VMM models its userspace fabric's pending→in-service transition as
    /// *interrupt acceptance*, which happens inside the backend on VM-entry. So
    /// the VMM leaves an identity pending in the fabric when it sets it via
    /// [`set_pending_irq`](Backend::set_pending_irq), and completes the
    /// transition only when this method reports the identity accepted — keeping
    /// the register file (and any snapshot taken while one waits on the
    /// interrupt window) showing it pending, not prematurely in-service.
    /// Backends that never accept a maskable IRQ return `None`.
    fn take_accepted_interrupt(&mut self) -> Option<<Self::A as Arch>::IntId>;

    // --- exit completion (the read/write/hypercall round-trip) ----------------

    /// Supply the value for a pending **read-style** exit: an MMIO load, or an
    /// arch read-style exit (x86: `Io` IN, `Rdmsr`, `Rdtsc`, `Rdtscp`,
    /// `Rdrand`, `Rdseed`). The low `size`/`width` bytes are delivered to the
    /// guest's destination. Errors
    /// [`NoPendingRead`](crate::BackendError::NoPendingRead) if no read-style
    /// exit is pending. (Stock `KvmBackend` never surfaces the instruction-read
    /// exits, so it completes only IO/MMIO/MSR reads; the instruction-read
    /// completions exist for `PatchedKvmBackend`/`DirectVmxBackend`.)
    fn complete_read(&mut self, value: u64) -> Result<()>;

    /// The contract's `deny-gp` disposition for a pending MSR exit: inject
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

    /// Set the hypercall return slot (the response-frame length per
    /// INTEGRATION.md §1, or 0 on transport error) for a pending `Hypercall`.
    /// Which guest register carries the return is the backend's per-arch
    /// knowledge (x86: `RAX`). Errors if none pending.
    fn complete_hypercall(&mut self, ret: u64) -> Result<()>;

    /// Resolve a pending arch exit whose completion carries an **arch payload**
    /// ([`Arch::Completion`]; x86: the CPUID result quad). Errors
    /// [`BadCompletion`](crate::BackendError::BadCompletion) if the pending exit
    /// does not match the completion.
    fn complete_arch(&mut self, completion: <Self::A as Arch>::Completion) -> Result<()>;

    // --- snapshot / restore ---------------------------------------------------

    /// Full guest-visible vCPU state for snapshot/restore. `[refinement]`:
    /// fallible here — the underlying `KVM_GET_*` ioctls can fail and library
    /// code must not `unwrap` (rule #4).
    fn save(&self) -> Result<<Self::A as Arch>::VcpuState>;

    /// Restore a vCPU state produced by `save`. Validates internal consistency;
    /// [`InvalidState`](crate::BackendError::InvalidState) on a
    /// malformed/incompatible blob (never a panic).
    fn restore(&mut self, state: &<Self::A as Arch>::VcpuState) -> Result<()>;

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
    fn capabilities(&self) -> Capabilities<<Self::A as Arch>::Caps>;
}

/// Blanket forward so the composition root can inject a concrete backend as a
/// `Box<dyn Backend<A = …>>` and run a `Vmm` over it (R-Backend / task-21 P5:
/// the one place a concrete backend is named is `fn main`; everything above the
/// trait is backend-agnostic). `Backend` is dyn-compatible (no generic methods,
/// no `Self`-by-value returns), so `Box<dyn Backend<A = …>>` is a `Backend` too.
impl<B: Backend + ?Sized> Backend for Box<B> {
    type A = B::A;

    fn set_policy(&mut self, policy: &<Self::A as Arch>::Policy) -> Result<()> {
        (**self).set_policy(policy)
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

    fn run(&mut self) -> Result<Exit<Self::A>> {
        (**self).run()
    }

    fn run_until(&mut self, deadline: Moment) -> Result<Exit<Self::A>> {
        (**self).run_until(deadline)
    }

    fn inject(&mut self, event: <Self::A as Arch>::Injection) -> Result<()> {
        (**self).inject(event)
    }

    fn set_pending_irq(&mut self, id: Option<<Self::A as Arch>::IntId>) -> Result<()> {
        (**self).set_pending_irq(id)
    }

    fn take_accepted_interrupt(&mut self) -> Option<<Self::A as Arch>::IntId> {
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

    fn complete_hypercall(&mut self, ret: u64) -> Result<()> {
        (**self).complete_hypercall(ret)
    }

    fn complete_arch(&mut self, completion: <Self::A as Arch>::Completion) -> Result<()> {
        (**self).complete_arch(completion)
    }

    fn save(&self) -> Result<<Self::A as Arch>::VcpuState> {
        (**self).save()
    }

    fn restore(&mut self, state: &<Self::A as Arch>::VcpuState) -> Result<()> {
        (**self).restore(state)
    }

    fn exit_counts(&self) -> ExitCounts {
        (**self).exit_counts()
    }

    fn reset_exit_counts(&mut self) {
        (**self).reset_exit_counts()
    }

    fn capabilities(&self) -> Capabilities<<Self::A as Arch>::Caps> {
        (**self).capabilities()
    }
}
