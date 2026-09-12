// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::VecDeque;

use crate::arch::x86::{
    CpuidModel, Injection, MsrFilter, VcpuState, X86, X86Completion, X86Exit, X86Policy,
};
use crate::backend::Backend;
use crate::error::{BackendError, Result};
use crate::exit::{Capabilities, CommonExit, Exit, ExitCounts};
use crate::types::Gpa;

pub type MockCaps = Capabilities<crate::arch::x86::X86Caps>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Completion {
    Read(u64),
    Fault,
    Ok,
    Hypercall(u64),
    Cpuid {
        eax: u32,
        ebx: u32,
        ecx: u32,
        edx: u32,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pending {
    None,
    Read,
    Rdmsr,
    Wrmsr,
    Hypercall,
    Cpuid,
}

fn pending_for(exit: &Exit<X86>) -> Pending {
    match exit {
        Exit::Common(c) => match c {
            CommonExit::Mmio { write: None, .. } => Pending::Read,
            CommonExit::Hypercall(_) => Pending::Hypercall,
            CommonExit::Mmio { write: Some(_), .. } | CommonExit::Idle | CommonExit::Shutdown => {
                Pending::None
            }
        },
        Exit::Arch(e) => match e {
            X86Exit::Io { write: None, .. } => Pending::Read,
            X86Exit::Rdmsr { .. } => Pending::Rdmsr,
            X86Exit::Wrmsr { .. } => Pending::Wrmsr,
            X86Exit::Cpuid { .. } => Pending::Cpuid,
            X86Exit::Io { write: Some(_), .. } => Pending::None,
        },
    }
}

const MOCK_CAPS: MockCaps = Capabilities {
    name: "mock",
    arch: crate::arch::x86::X86Caps,
};

#[derive(Debug)]
pub struct MockBackend {
    caps: MockCaps,
    policy: Option<X86Policy>,
    script: VecDeque<Exit<X86>>,
    pending: Pending,
    completion_staged: bool,
    counts: ExitCounts,
    state: VcpuState,
    regions: Vec<(Gpa, usize)>,
    injected: Vec<Injection>,
    pending_irq: Option<u8>,
    accepted_irq: VecDeque<u8>,
    defer_accept: bool,
    completions: Vec<Completion>,
    dirty_pending: Option<Vec<u64>>,
    cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockBackend {
    pub fn new() -> Self {
        Self {
            caps: MOCK_CAPS,
            policy: None,
            script: VecDeque::new(),
            pending: Pending::None,
            completion_staged: false,
            counts: ExitCounts::default(),
            state: VcpuState::default(),
            regions: Vec::new(),
            injected: Vec::new(),
            pending_irq: None,
            accepted_irq: VecDeque::new(),
            defer_accept: false,
            completions: Vec::new(),
            dirty_pending: None,
            cancellation: None,
        }
    }

    pub fn with_cancellation_flag(
        mut self,
        latch: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        self.cancellation = Some(latch);
        self
    }

    pub fn with_capabilities(caps: MockCaps) -> Self {
        Self {
            caps,
            ..Self::new()
        }
    }

    pub fn with_exits(exits: impl IntoIterator<Item = Exit<X86>>) -> Self {
        let mut m = Self::new();
        m.extend_exits(exits);
        m
    }

    pub fn push_exit(&mut self, exit: Exit<X86>) -> &mut Self {
        self.script.push_back(exit);
        self
    }

    pub fn extend_exits(&mut self, exits: impl IntoIterator<Item = Exit<X86>>) -> &mut Self {
        self.script.extend(exits);
        self
    }

    pub fn is_configured(&self) -> bool {
        self.policy.is_some()
    }

    pub fn has_pending(&self) -> bool {
        self.pending != Pending::None
    }

    pub fn installed_cpuid(&self) -> Option<&CpuidModel> {
        self.policy.as_ref().map(|p| &p.cpuid)
    }

    pub fn installed_msr_filter(&self) -> Option<&MsrFilter> {
        self.policy.as_ref().map(|p| &p.msr_filter)
    }

    pub fn injected(&self) -> &[Injection] {
        &self.injected
    }

    pub fn pending_irq(&self) -> Option<u8> {
        self.pending_irq
    }

    pub fn completions(&self) -> &[Completion] {
        &self.completions
    }

    pub fn regions(&self) -> &[(Gpa, usize)] {
        &self.regions
    }

    pub fn set_state(&mut self, state: VcpuState) -> &mut Self {
        self.state = state;
        self
    }

    pub fn enable_dirty_tracking(&mut self) -> &mut Self {
        self.dirty_pending.get_or_insert_with(Vec::new);
        self
    }

    pub fn push_dirty_gfns(&mut self, gfns: Vec<u64>) -> &mut Self {
        self.dirty_pending.get_or_insert_with(Vec::new).extend(gfns);
        self
    }

    pub fn set_defer_accept(&mut self, defer: bool) -> &mut Self {
        self.defer_accept = defer;
        self
    }

    fn ensure_runnable(&self) -> Result<()> {
        if !self.is_configured() {
            return Err(BackendError::NotConfigured);
        }
        if self.pending != Pending::None {
            return Err(BackendError::PendingCompletion);
        }
        Ok(())
    }

    fn next_scripted(&mut self) -> Result<Exit<X86>> {
        self.script
            .pop_front()
            .ok_or(BackendError::Internal("mock run-queue empty"))
    }

    fn deliver(&mut self, exit: Exit<X86>) -> Exit<X86> {
        self.counts.bump(exit.reason());
        self.pending = pending_for(&exit);
        exit
    }

    fn finish(&mut self, completion: Completion) {
        self.completions.push(completion);
        self.pending = Pending::None;
        self.completion_staged = true;
    }

    fn accept_pending_irqs(&mut self) {
        if self.defer_accept {
            return;
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

    fn drain_dirty_pages(&mut self) -> Result<Vec<u64>> {
        match self.dirty_pending.as_mut() {
            None => Err(BackendError::Unsupported {
                what: "drain_dirty_pages (mock dirty tracking not enabled)",
            }),
            Some(pending) => {
                let mut gfns = std::mem::take(pending);
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
        self.completion_staged = false;
        Ok(self.deliver(exit))
    }

    fn inject(&mut self, event: Injection) -> Result<()> {
        self.injected.push(event);
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

    fn retire_pending_completion(&mut self) -> Result<()> {
        if !self.completion_staged {
            return Ok(());
        }
        self.completion_staged = false;
        self.pending = Pending::None;
        Ok(())
    }

    fn save(&self) -> Result<VcpuState> {
        Ok(self.state.clone())
    }

    fn restore(&mut self, state: &VcpuState) -> Result<()> {
        if self.pending != Pending::None || self.completion_staged {
            return Err(BackendError::PendingCompletion);
        }
        self.state = state.clone();
        self.pending_irq = None;
        self.accepted_irq.clear();
        Ok(())
    }

    fn exit_counts(&self) -> ExitCounts {
        self.counts
    }

    fn reset_exit_counts(&mut self) {
        self.counts = ExitCounts::default();
    }

    fn cancellation_flag(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
        self.cancellation.clone()
    }

    fn capabilities(&self) -> MockCaps {
        self.caps
    }
}

#[cfg(test)]
mod tests {
    use super::{Completion, MockBackend, Pending};
    use crate::backend::Backend;

    #[test]
    fn retirement_consumes_a_staged_completion_marker() {
        let mut mock = MockBackend::new();
        mock.pending = Pending::None;
        mock.completion_staged = true;
        mock.completions.push(Completion::Read(0x55));

        mock.retire_pending_completion().unwrap();

        assert!(!mock.completion_staged);
        assert_eq!(mock.pending, Pending::None);
        assert_eq!(mock.completions, vec![Completion::Read(0x55)]);
    }
}
