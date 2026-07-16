// SPDX-License-Identifier: AGPL-3.0-or-later
//! `MockArm64Backend` — a deterministic, in-process [`Backend`] over the
//! [`Arm64`] vendor (behind the non-default **`mock`** feature), the sibling of
//! [`MockBackend`](crate::MockBackend).
//!
//! This is the M1 keystone's driver (`tasks/112`): the first *second* vendor to
//! instantiate every `Backend` method the engine calls, proving the seam is
//! genuinely additive in a way no cross-compile gate can (a signature only a
//! second implementor could refute stays invisible until one exists —
//! `docs/ARCH-BOUNDARY.md` §D). It is scripted with a queue of [`Exit`]s and
//! enforces the same run-loop / completion contract as the x86 mock:
//! fail-closed `NotConfigured` until the policy is installed,
//! `PendingCompletion` on a missed completion, and
//! `NoPendingRead`/`BadCompletion` on a mismatched one.

use std::collections::VecDeque;

use crate::arch::arm64::{
    Arm64, Arm64Completion, Arm64Exit, Arm64Injection, Arm64Policy, Arm64VcpuState, GicIntId,
};
use crate::backend::Backend;
use crate::error::{BackendError, Result};
use crate::exit::{Capabilities, CommonExit, Exit, ExitCounts};
use crate::types::{Gpa, Moment};

/// The arm64 mock's capability type — the arm64 vendor's arch flags.
pub type MockArm64Caps = Capabilities<crate::arch::arm64::Arm64Caps>;

/// A completion the VMM applied to a pending exit, recorded for test
/// assertions (the arm64 sibling of [`Completion`](crate::Completion); no
/// arch-payload variant — [`Arm64Completion`] is uninhabited).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arm64MockCompletion {
    /// `complete_read(value)` for a read-style exit (MMIO load / sysreg read).
    Read(u64),
    /// `complete_fault()` (the deny-UNDEF sysreg disposition).
    Fault,
    /// `complete_ok()` (a non-fault sysreg-write resolution).
    Ok,
    /// `complete_hypercall(ret)`.
    Hypercall(u64),
}

/// What the last returned exit is waiting for, if anything.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pending {
    /// Nothing pending; `run` may resume.
    None,
    /// An MMIO load: only `complete_read` resolves it.
    Read,
    /// A sysreg read: `complete_read` (value) or `complete_fault` (deny-UNDEF).
    SysregRead,
    /// A sysreg write: `complete_ok` (allow/drop) or `complete_fault`.
    SysregWrite,
    /// `Hypercall`: `complete_hypercall`.
    Hypercall,
}

/// What an exit, once returned, is waiting on. Both levels of the two-level
/// [`Exit`] are matched **exhaustively** — no wildcard arms over arch exits
/// (default-deny discipline).
fn pending_for(exit: &Exit<Arm64>) -> Pending {
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
            Arm64Exit::Sysreg { write: None, .. } => Pending::SysregRead,
            Arm64Exit::Sysreg { write: Some(_), .. } => Pending::SysregWrite,
        },
    }
}

/// Default capabilities of a fresh arm64 mock: fully deterministic (it is a
/// controlled in-process model). Override with
/// [`MockArm64Backend::with_capabilities`] to test the "refuse to claim
/// determinism" path (the stock arm64 backend reports everything `false`).
const MOCK_ARM64_CAPS: MockArm64Caps = Capabilities {
    name: "mock-arm64",
    deterministic_rng: true,
    arch: crate::arch::arm64::Arm64Caps {
        deterministic_cntvct: true,
        enforces_cntv_cval: true,
    },
};

/// A deterministic, scripted arm64 [`Backend`] with no KVM dependency.
#[derive(Debug)]
pub struct MockArm64Backend {
    caps: MockArm64Caps,
    policy: Option<Arm64Policy>,
    script: VecDeque<Exit<Arm64>>,
    pending: Pending,
    counts: ExitCounts,
    state: Arm64VcpuState,
    regions: Vec<(Gpa, usize)>,
    injected: Vec<Arm64Injection>,
    /// The single pending maskable-IRQ identity ([`Backend::set_pending_irq`]),
    /// overwritten each entry by the VMM's re-arbitration.
    pending_irq: Option<GicIntId>,
    /// INTIDs the mock has "accepted" into the guest (drained by
    /// [`Backend::take_accepted_interrupt`]).
    accepted_irq: VecDeque<GicIntId>,
    /// When `true`, `run`/`run_until` do **not** accept the pending IRQ —
    /// modelling an injection-window wait.
    defer_accept: bool,
    completions: Vec<Arm64MockCompletion>,
}

impl Default for MockArm64Backend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockArm64Backend {
    /// A fresh, unconfigured mock with an empty exit script and default
    /// (fully-deterministic) capabilities.
    pub fn new() -> Self {
        Self {
            caps: MOCK_ARM64_CAPS,
            policy: None,
            script: VecDeque::new(),
            pending: Pending::None,
            counts: ExitCounts::default(),
            state: Arm64VcpuState::default(),
            regions: Vec::new(),
            injected: Vec::new(),
            pending_irq: None,
            accepted_irq: VecDeque::new(),
            defer_accept: false,
            completions: Vec::new(),
        }
    }

    /// A fresh mock reporting `caps` instead of the default.
    pub fn with_capabilities(caps: MockArm64Caps) -> Self {
        Self {
            caps,
            ..Self::new()
        }
    }

    /// A fresh mock pre-loaded with a script of exits to return from successive
    /// `run`/`run_until` calls.
    pub fn with_exits(exits: impl IntoIterator<Item = Exit<Arm64>>) -> Self {
        let mut m = Self::new();
        m.extend_exits(exits);
        m
    }

    /// Enqueue one exit to be returned by a future `run`/`run_until`.
    pub fn push_exit(&mut self, exit: Exit<Arm64>) -> &mut Self {
        self.script.push_back(exit);
        self
    }

    /// Enqueue several exits, in order.
    pub fn extend_exits(&mut self, exits: impl IntoIterator<Item = Exit<Arm64>>) -> &mut Self {
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

    /// The policy installed by `set_policy`, if any (for test assertions).
    pub fn installed_policy(&self) -> Option<&Arm64Policy> {
        self.policy.as_ref()
    }

    /// The events passed to `inject`, in order.
    pub fn injected(&self) -> &[Arm64Injection] {
        &self.injected
    }

    /// The current pending maskable-IRQ identity (`None` once accepted or
    /// cleared) — so a test can observe the VMM's per-entry re-arbitration.
    pub fn pending_irq(&self) -> Option<GicIntId> {
        self.pending_irq
    }

    /// The completions applied so far, in order.
    pub fn completions(&self) -> &[Arm64MockCompletion] {
        &self.completions
    }

    /// The `(gpa, len)` regions recorded by `map_memory`, in order.
    pub fn regions(&self) -> &[(Gpa, usize)] {
        &self.regions
    }

    /// Set the `Arm64VcpuState` the next `save` will return (test convenience,
    /// outside the `restore` path).
    pub fn set_state(&mut self, state: Arm64VcpuState) -> &mut Self {
        self.state = state;
        self
    }

    /// When `true`, queued maskable IRQs are **not** accepted at
    /// `run`/`run_until` (they stay pending) — modelling an injection-window
    /// wait so a test can observe an identity held pending before acceptance.
    pub fn set_defer_accept(&mut self, defer: bool) -> &mut Self {
        self.defer_accept = defer;
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
    fn next_scripted(&mut self) -> Result<Exit<Arm64>> {
        self.script
            .pop_front()
            .ok_or(BackendError::Internal("mock-arm64 run-queue empty"))
    }

    /// Account for and arm a returned exit: bump its counter and record what
    /// completion it now awaits.
    fn deliver(&mut self, exit: Exit<Arm64>) -> Exit<Arm64> {
        self.counts.bump(exit.reason());
        self.pending = pending_for(&exit);
        exit
    }

    /// Resolve the current pending exit, recording `completion`.
    fn finish(&mut self, completion: Arm64MockCompletion) {
        self.completions.push(completion);
        self.pending = Pending::None;
    }

    /// Model interrupt acceptance at a VM-entry (the mock is always injectable
    /// unless [`Self::set_defer_accept`] holds the identity pending).
    fn accept_pending_irqs(&mut self) {
        if self.defer_accept {
            return;
        }
        if let Some(id) = self.pending_irq.take() {
            self.accepted_irq.push_back(id);
        }
    }
}

impl Backend for MockArm64Backend {
    type A = Arm64;

    fn set_policy(&mut self, policy: &Arm64Policy) -> Result<()> {
        self.policy = Some(policy.clone());
        Ok(())
    }

    unsafe fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> Result<()> {
        // The mock performs no registration; it only records the region (no
        // `unsafe` block — the host pointer is not retained or dereferenced).
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

    fn run(&mut self) -> Result<Exit<Arm64>> {
        self.ensure_runnable()?;
        self.accept_pending_irqs();
        let exit = self.next_scripted()?;
        Ok(self.deliver(exit))
    }

    fn run_until(&mut self, deadline: Moment) -> Result<Exit<Arm64>> {
        self.ensure_runnable()?;
        self.accept_pending_irqs();
        let exit = match self.next_scripted()? {
            Exit::Common(CommonExit::Deadline { .. }) => {
                Exit::Common(CommonExit::Deadline { reached: deadline })
            }
            other => other,
        };
        Ok(self.deliver(exit))
    }

    fn inject(&mut self, event: Arm64Injection) -> Result<()> {
        self.injected.push(event);
        let Arm64Injection::Interrupt { intid } = event;
        self.pending_irq = Some(intid);
        Ok(())
    }

    fn set_pending_irq(&mut self, id: Option<GicIntId>) -> Result<()> {
        self.pending_irq = id;
        Ok(())
    }

    fn take_accepted_interrupt(&mut self) -> Option<GicIntId> {
        self.accepted_irq.pop_front()
    }

    fn complete_read(&mut self, value: u64) -> Result<()> {
        match self.pending {
            Pending::Read | Pending::SysregRead => {
                self.finish(Arm64MockCompletion::Read(value));
                Ok(())
            }
            _ => Err(BackendError::NoPendingRead),
        }
    }

    fn complete_fault(&mut self) -> Result<()> {
        match self.pending {
            Pending::SysregRead | Pending::SysregWrite => {
                self.finish(Arm64MockCompletion::Fault);
                Ok(())
            }
            _ => Err(BackendError::BadCompletion),
        }
    }

    fn complete_ok(&mut self) -> Result<()> {
        match self.pending {
            Pending::SysregWrite => {
                self.finish(Arm64MockCompletion::Ok);
                Ok(())
            }
            _ => Err(BackendError::BadCompletion),
        }
    }

    fn complete_hypercall(&mut self, ret: u64) -> Result<()> {
        match self.pending {
            Pending::Hypercall => {
                self.finish(Arm64MockCompletion::Hypercall(ret));
                Ok(())
            }
            Pending::None => Err(BackendError::NoPendingRead),
            _ => Err(BackendError::BadCompletion),
        }
    }

    fn complete_arch(&mut self, completion: Arm64Completion) -> Result<()> {
        // `Arm64Completion` is uninhabited (no arch-payload completions in the
        // skeleton), so this is statically unreachable — spelled as the empty
        // match so adding a variant forces a decision here.
        match completion {}
    }

    fn save(&self) -> Result<Arm64VcpuState> {
        Ok(self.state)
    }

    fn restore(&mut self, state: &Arm64VcpuState) -> Result<()> {
        // The mock has no host to reject the blob; it accepts any well-typed
        // `Arm64VcpuState`. `restore` then `save` reproduces an identical
        // state by construction.
        self.state = *state;
        Ok(())
    }

    fn exit_counts(&self) -> ExitCounts {
        self.counts
    }

    fn reset_exit_counts(&mut self) {
        self.counts = ExitCounts::default();
    }

    fn capabilities(&self) -> MockArm64Caps {
        self.caps
    }
}
