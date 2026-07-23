// SPDX-License-Identifier: AGPL-3.0-or-later
//! `MockBackend` — a deterministic, in-process [`Backend`] for unit/property
//! tests (behind the non-default **`mock`** feature), over the
//! [`X86`](crate::arch::x86::X86) vendor.
//!
//! It proves the trait is implementable with no KVM and is **the substrate task
//! 15 unit-tests vmm-core against** (turned on under that crate's
//! `[dev-dependencies]`). It is scripted with a queue of [`Exit`]s and enforces
//! the full run-loop / completion contract exactly as a live backend must:
//! fail-closed `NotConfigured` until the policy is installed,
//! `PendingCompletion` on a missed completion, and
//! `NoPendingRead`/`BadCompletion` on a mismatched one. It records injections
//! and completions so a test can assert what the VMM asked the backend to do.
//!
//! Unlike `KvmBackend`, the mock *implements* `run_until` and `inject`: a
//! scripted `CommonExit::Deadline` is returned from `run_until` with the
//! requested deadline, and `inject` records the event (so vmm-core's injection
//! planning is testable). Determinism: a `MockBackend` driven by the same
//! script + same completions produces the same counters and the same saved
//! `VcpuState`.

use std::collections::VecDeque;

use crate::arch::x86::{
    CpuidModel, Injection, MsrFilter, VcpuState, X86, X86Completion, X86Exit, X86Policy,
};
use crate::backend::Backend;
use crate::error::{BackendError, Result};
use crate::exit::{Capabilities, CommonExit, Exit, ExitCounts};
use crate::types::{Gpa, Moment};

/// The mock's capability type — the x86 vendor's arch flags.
pub type MockCaps = Capabilities<crate::arch::x86::X86Caps>;

/// A completion the VMM applied to a pending exit, recorded for test assertions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Completion {
    /// `complete_read(value)` for a read-style exit.
    Read(u64),
    /// `complete_fault()` (the `deny-gp` MSR disposition).
    Fault,
    /// `complete_ok()` (a non-fault `Wrmsr` resolution).
    Ok,
    /// `complete_hypercall(ret)`.
    Hypercall(u64),
    /// `complete_arch` with the CPUID result quad.
    Cpuid {
        /// Result `EAX`.
        eax: u32,
        /// Result `EBX`.
        ebx: u32,
        /// Result `ECX`.
        ecx: u32,
        /// Result `EDX`.
        edx: u32,
    },
}

/// What the last returned exit is waiting for, if anything. Drives the
/// completion-discipline checks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pending {
    /// Nothing pending; `run` may resume.
    None,
    /// A read-style exit (`Io` IN / `Mmio` load / instruction-read): only
    /// `complete_read` resolves it.
    Read,
    /// `Rdmsr`: `complete_read` (value) or `complete_fault` (deny-gp).
    Rdmsr,
    /// `Wrmsr`: `complete_ok` (allow/drop) or `complete_fault` (deny-gp).
    Wrmsr,
    /// `Hypercall`: `complete_hypercall`.
    Hypercall,
    /// `Cpuid`: `complete_arch` (the result quad).
    Cpuid,
}

/// What an exit, once returned, is waiting on (its completion discipline).
/// Both levels of the two-level [`Exit`] are matched **exhaustively** — no
/// wildcard arms over arch exits (default-deny discipline).
fn pending_for(exit: &Exit<X86>) -> Pending {
    match exit {
        Exit::Common(c) => match c {
            CommonExit::Mmio { write: None, .. } => Pending::Read,
            CommonExit::Hypercall(_) => Pending::Hypercall,
            CommonExit::Mmio { write: Some(_), .. }
            | CommonExit::Idle
            | CommonExit::Shutdown
            | CommonExit::Deadline { .. } => Pending::None,
        },
        Exit::Arch(e) => match e {
            X86Exit::Io { write: None, .. }
            | X86Exit::Rdtsc
            | X86Exit::Rdtscp
            | X86Exit::Rdrand { .. }
            | X86Exit::Rdseed { .. } => Pending::Read,
            X86Exit::Rdmsr { .. } => Pending::Rdmsr,
            X86Exit::Wrmsr { .. } => Pending::Wrmsr,
            X86Exit::Cpuid { .. } => Pending::Cpuid,
            X86Exit::Io { write: Some(_), .. } => Pending::None,
        },
    }
}

/// Default capabilities of a fresh mock: fully deterministic (it is a controlled
/// in-process model). Override with [`MockBackend::with_capabilities`] to test
/// vmm-core's "refuse to claim determinism" path against a backend that reports
/// a hole.
const MOCK_CAPS: MockCaps = Capabilities {
    name: "mock",
    deterministic_rng: true,
    arch: crate::arch::x86::X86Caps {
        deterministic_tsc: true,
        enforces_tsc_deadline_msr: true,
    },
};

/// A deterministic, scripted [`Backend`] with no KVM dependency.
#[derive(Debug)]
pub struct MockBackend {
    caps: MockCaps,
    policy: Option<X86Policy>,
    script: VecDeque<Exit<X86>>,
    pending: Pending,
    counts: ExitCounts,
    state: VcpuState,
    regions: Vec<(Gpa, usize)>,
    injected: Vec<Injection>,
    /// The single pending maskable-IRQ vector ([`Backend::set_pending_irq`]),
    /// overwritten each entry by the VMM's re-arbitration — mirroring the live
    /// backend's one-slot `pending_irq`.
    pending_irq: Option<u8>,
    /// Vectors the mock has "accepted" into the guest (drained by
    /// [`Backend::take_accepted_interrupt`]). The mock is always injectable, so a
    /// pending vector is accepted at the next `run`/`run_until` — unless
    /// [`Self::defer_accept`] is set.
    accepted_irq: VecDeque<u8>,
    /// When `true`, `run`/`run_until` do **not** accept the pending IRQ (it stays in
    /// `pending_irq`) — modelling the live backend's interrupt-window wait, so a
    /// test can observe a vector held pending (in the LAPIC IRR, not in service)
    /// before acceptance.
    defer_accept: bool,
    completions: Vec<Completion>,
    /// Scripted dirty-page tracking (task 95 M2.1): `None` = no dirty tracking
    /// (`harvest_dirty_gfns` answers `Unsupported`, like a `flags: 0` live
    /// backend); `Some(pending)` = tracking enabled — [`Self::push_dirty_gfns`]
    /// **accumulates** gfns exactly as guest writes accumulate in KVM's log,
    /// and each harvest drains the whole accumulated set (retrieve-and-reset).
    /// An accumulate-then-drain model, NOT a queue of per-harvest sets: a
    /// caller is free to harvest more than once per operation (e.g. a
    /// harvest-and-discard re-arm right after a seal) without a scripted set
    /// being silently swallowed by the extra call. The mock cannot observe real
    /// guest writes (it never writes RAM), so the set is scripted.
    dirty_pending: Option<Vec<u64>>,
    /// Scripted **late landings** (task 142, hm-40na): absolute `reached` work
    /// counts, one consumed per scripted [`CommonExit::Deadline`], each modelling
    /// the guest **free-running PAST the requested `run_until` deadline** to the
    /// next natural boundary (the box @3e7 overshoot shape — a staged Moment's
    /// arrival the exact-count seam could not clamp). Empty ⇒ default behavior:
    /// `run_until` rewrites `reached := deadline` and lands EXACTLY where asked,
    /// byte-identical to every existing test. See [`Self::push_late_landing`].
    late_landings: VecDeque<Moment>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockBackend {
    /// A fresh, unconfigured mock with an empty exit script and default
    /// (fully-deterministic) capabilities.
    pub fn new() -> Self {
        Self {
            caps: MOCK_CAPS,
            policy: None,
            script: VecDeque::new(),
            pending: Pending::None,
            counts: ExitCounts::default(),
            state: VcpuState::default(),
            regions: Vec::new(),
            injected: Vec::new(),
            pending_irq: None,
            accepted_irq: VecDeque::new(),
            defer_accept: false,
            completions: Vec::new(),
            dirty_pending: None,
            late_landings: VecDeque::new(),
        }
    }

    /// A fresh mock reporting `caps` instead of the default.
    pub fn with_capabilities(caps: MockCaps) -> Self {
        Self {
            caps,
            ..Self::new()
        }
    }

    /// A fresh mock pre-loaded with a script of exits to return from successive
    /// `run`/`run_until` calls.
    pub fn with_exits(exits: impl IntoIterator<Item = Exit<X86>>) -> Self {
        let mut m = Self::new();
        m.extend_exits(exits);
        m
    }

    /// Enqueue one exit to be returned by a future `run`/`run_until`.
    pub fn push_exit(&mut self, exit: Exit<X86>) -> &mut Self {
        self.script.push_back(exit);
        self
    }

    /// Enqueue several exits, in order.
    pub fn extend_exits(&mut self, exits: impl IntoIterator<Item = Exit<X86>>) -> &mut Self {
        self.script.extend(exits);
        self
    }

    /// `true` once `set_policy` has been called.
    pub fn is_configured(&self) -> bool {
        self.policy.is_some()
    }

    /// `true` if the last returned exit still awaits a completion.
    pub fn has_pending(&self) -> bool {
        self.pending != Pending::None
    }

    /// The CPUID model installed by `set_policy`, if any (for test assertions).
    pub fn installed_cpuid(&self) -> Option<&CpuidModel> {
        self.policy.as_ref().map(|p| &p.cpuid)
    }

    /// The MSR filter installed by `set_policy`, if any.
    pub fn installed_msr_filter(&self) -> Option<&MsrFilter> {
        self.policy.as_ref().map(|p| &p.msr_filter)
    }

    /// The events passed to `inject`, in order.
    pub fn injected(&self) -> &[Injection] {
        &self.injected
    }

    /// The current pending maskable-IRQ vector (set by `set_pending_irq`/`inject`,
    /// `None` once accepted or cleared) — so a test can observe the VMM's per-entry
    /// re-arbitration (e.g. a stale vector overwritten with `None` after a TPR raise).
    pub fn pending_irq(&self) -> Option<u8> {
        self.pending_irq
    }

    /// The completions applied so far, in order.
    pub fn completions(&self) -> &[Completion] {
        &self.completions
    }

    /// The `(gpa, len)` regions recorded by `map_memory`, in order.
    pub fn regions(&self) -> &[(Gpa, usize)] {
        &self.regions
    }

    /// Set the `VcpuState` the next `save` will return (test convenience, outside
    /// the `restore` path).
    pub fn set_state(&mut self, state: VcpuState) -> &mut Self {
        self.state = state;
        self
    }

    /// Turn on dirty tracking (task 95 M2.1): after this, `harvest_dirty_gfns`
    /// answers `Ok` — whatever [`Self::push_dirty_gfns`] has accumulated since
    /// the last harvest (empty if nothing). Off (`Unsupported`, the trait
    /// default's shape) until called.
    pub fn enable_dirty_tracking(&mut self) -> &mut Self {
        self.dirty_pending.get_or_insert_with(Vec::new);
        self
    }

    /// Record scripted guest writes (implies [`Self::enable_dirty_tracking`]):
    /// the gfns **accumulate** — as writes do in KVM's log — until the next
    /// `harvest_dirty_gfns` drains them all, however many harvests a caller
    /// issues in between operations.
    pub fn push_dirty_gfns(&mut self, gfns: Vec<u64>) -> &mut Self {
        self.dirty_pending.get_or_insert_with(Vec::new).extend(gfns);
        self
    }

    /// When `true`, queued maskable IRQs are **not** accepted at `run`/`run_until`
    /// (they stay in the pending queue) — modelling the live backend's
    /// interrupt-window wait, so a test can observe a vector held pending before
    /// acceptance (the userspace-LAPIC IRR→ISR deferral).
    pub fn set_defer_accept(&mut self, defer: bool) -> &mut Self {
        self.defer_accept = defer;
        self
    }

    /// Script the next scripted [`CommonExit::Deadline`] to land **LATE** — at
    /// `reached` instead of exactly at the requested `run_until` deadline (task
    /// 142, hm-40na). This is the portable model of the box @3e7 failure shape:
    /// the exact-count arrival seam could not clamp the guest at the staged
    /// `Moment`, so the guest free-ran PAST it to the next natural boundary
    /// (`reached`), an overshoot < 1 quantum. Late landings are a queue consumed
    /// one per `Deadline` exit, in order; once drained, `run_until` reverts to the
    /// default `reached := deadline` (exact landing).
    ///
    /// `reached` is on the same retired-work axis as the `run_until` deadline; a
    /// faithful model scripts a value **strictly greater** than the deadline the
    /// leg will be asked to stop at (a live backend never stops *before* its
    /// deadline — see `Vmm::on_deadline`). The mock does not enforce that: the
    /// lateness is an **explicit, deterministic test input** (no clock, no
    /// randomness), and a test that scripts an at-or-before value is simply
    /// asserting the exact-landing default, which is the caller's business.
    pub fn push_late_landing(&mut self, reached: Moment) -> &mut Self {
        self.late_landings.push_back(reached);
        self
    }

    /// Fail-closed config + completion-discipline gate shared by `run`/`run_until`.
    fn ensure_runnable(&self) -> Result<()> {
        if !self.is_configured() {
            return Err(BackendError::NotConfigured);
        }
        if self.pending != Pending::None {
            return Err(BackendError::PendingCompletion);
        }
        Ok(())
    }

    /// Pop the next scripted exit, or fail closed if the script is exhausted (a
    /// test bug — a live backend would block, but the mock surfaces it loudly).
    fn next_scripted(&mut self) -> Result<Exit<X86>> {
        self.script
            .pop_front()
            .ok_or(BackendError::Internal("mock run-queue empty"))
    }

    /// Account for and arm a returned exit: bump its counter and record what
    /// completion it now awaits.
    fn deliver(&mut self, exit: Exit<X86>) -> Exit<X86> {
        self.counts.bump(exit.reason());
        self.pending = pending_for(&exit);
        exit
    }

    /// Resolve the current pending exit, recording `completion`.
    fn finish(&mut self, completion: Completion) {
        self.completions.push(completion);
        self.pending = Pending::None;
    }

    /// Model interrupt acceptance at a VM-entry: the mock is always injectable, so
    /// every queued maskable vector is accepted (moved to the accepted report that
    /// [`Backend::take_accepted_interrupt`] drains). Mirrors the live backend
    /// issuing `KVM_INTERRUPT` for the queued vectors.
    fn accept_pending_irqs(&mut self) {
        if self.defer_accept {
            return; // model the interrupt-window wait: the pending IRQ stays pending.
        }
        if let Some(v) = self.pending_irq.take() {
            self.accepted_irq.push_back(v);
        }
    }
}

impl Backend for MockBackend {
    type A = X86;

    fn set_policy(&mut self, policy: &X86Policy) -> Result<()> {
        self.policy = Some(policy.clone());
        Ok(())
    }

    unsafe fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> Result<()> {
        // The mock performs no registration; it only records the region (no
        // `unsafe` block — the host pointer is not retained or dereferenced).
        // Validate the spec's invariants so a test exercises the error path.
        if host.is_empty() {
            return Err(BackendError::Memory("zero-length memory region"));
        }
        if !gpa.0.is_multiple_of(4096) {
            return Err(BackendError::Memory("gpa is not 4 KiB-aligned"));
        }
        if !host.len().is_multiple_of(4096) {
            return Err(BackendError::Memory("region length is not 4 KiB-aligned"));
        }
        let end = gpa
            .0
            .checked_add(host.len() as u64)
            .ok_or(BackendError::Memory("region wraps the address space"))?;
        for &(g, len) in &self.regions {
            let g_end = g.0 + len as u64;
            if gpa.0 < g_end && g.0 < end {
                return Err(BackendError::Memory("region overlaps an existing map"));
            }
        }
        self.regions.push((gpa, host.len()));
        Ok(())
    }

    fn harvest_dirty_gfns(&mut self) -> Result<Vec<u64>> {
        match self.dirty_pending.as_mut() {
            None => Err(BackendError::Unsupported {
                what: "harvest_dirty_gfns (mock dirty tracking not enabled)",
            }),
            Some(pending) => {
                // Retrieve-and-reset, like the live log.
                let mut gfns = std::mem::take(pending);
                // Honor the trait contract regardless of how the test scripted it.
                gfns.sort_unstable();
                gfns.dedup();
                Ok(gfns)
            }
        }
    }

    fn run(&mut self) -> Result<Exit<X86>> {
        self.ensure_runnable()?;
        self.accept_pending_irqs();
        let exit = self.next_scripted()?;
        Ok(self.deliver(exit))
    }

    fn run_until(&mut self, deadline: Moment) -> Result<Exit<X86>> {
        self.ensure_runnable()?;
        self.accept_pending_irqs();
        let exit = match self.next_scripted()? {
            Exit::Common(CommonExit::Deadline { .. }) => {
                // Default: honor the deadline exactly (`reached := deadline`). A
                // scripted **late landing** (task 142) instead lands at the next
                // natural boundary PAST the deadline — the box @3e7 overshoot the
                // exact-count seam could not clamp. `pop_front` keeps default
                // behavior byte-identical when no late landing is scripted.
                let reached = self.late_landings.pop_front().unwrap_or(deadline);
                Exit::Common(CommonExit::Deadline { reached })
            }
            other => other,
        };
        Ok(self.deliver(exit))
    }

    fn inject(&mut self, event: Injection) -> Result<()> {
        self.injected.push(event);
        // Set the pending maskable vector (overwrite) for acceptance at the next
        // entry, mirroring the live backend. NMIs do not flow through this path.
        if let Injection::Interrupt { vector } = event {
            self.pending_irq = Some(vector);
        }
        Ok(())
    }

    fn set_pending_irq(&mut self, id: Option<u8>) -> Result<()> {
        self.pending_irq = id;
        Ok(())
    }

    fn take_accepted_interrupt(&mut self) -> Option<u8> {
        self.accepted_irq.pop_front()
    }

    fn complete_read(&mut self, value: u64) -> Result<()> {
        match self.pending {
            Pending::Read | Pending::Rdmsr => {
                self.finish(Completion::Read(value));
                Ok(())
            }
            _ => Err(BackendError::NoPendingRead),
        }
    }

    fn complete_fault(&mut self) -> Result<()> {
        match self.pending {
            Pending::Rdmsr | Pending::Wrmsr => {
                self.finish(Completion::Fault);
                Ok(())
            }
            _ => Err(BackendError::BadCompletion),
        }
    }

    fn complete_ok(&mut self) -> Result<()> {
        match self.pending {
            Pending::Wrmsr => {
                self.finish(Completion::Ok);
                Ok(())
            }
            _ => Err(BackendError::BadCompletion),
        }
    }

    fn complete_hypercall(&mut self, ret: u64) -> Result<()> {
        match self.pending {
            Pending::Hypercall => {
                self.finish(Completion::Hypercall(ret));
                Ok(())
            }
            Pending::None => Err(BackendError::NoPendingRead),
            _ => Err(BackendError::BadCompletion),
        }
    }

    fn complete_arch(&mut self, completion: X86Completion) -> Result<()> {
        match (self.pending, completion) {
            (Pending::Cpuid, X86Completion::Cpuid { eax, ebx, ecx, edx }) => {
                self.finish(Completion::Cpuid { eax, ebx, ecx, edx });
                Ok(())
            }
            _ => Err(BackendError::BadCompletion),
        }
    }

    fn save(&self) -> Result<VcpuState> {
        Ok(self.state.clone())
    }

    fn restore(&mut self, state: &VcpuState) -> Result<()> {
        // The mock has no host to reject the blob; it accepts any well-typed
        // `VcpuState` (the malformed-blob → `InvalidState` path is a `KvmBackend`
        // concern). `restore` then `save` reproduces an identical state by
        // construction.
        self.state = state.clone();
        Ok(())
    }

    fn exit_counts(&self) -> ExitCounts {
        self.counts
    }

    fn reset_exit_counts(&mut self) {
        self.counts = ExitCounts::default();
    }

    fn capabilities(&self) -> MockCaps {
        self.caps
    }
}
