// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::VecDeque;

use crate::arch::arm64::{
    Arm64, Arm64Completion, Arm64Exit, Arm64Injection, Arm64Policy, Arm64VcpuState, GicIntId,
};
use crate::backend::Backend;
use crate::error::{BackendError, Result};
use crate::exit::{Capabilities, CommonExit, Exit, ExitCounts};
use crate::types::Gpa;

pub type MockArm64Caps = Capabilities<crate::arch::arm64::Arm64Caps>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arm64MockCompletion {
    Read(u64),
    Fault,
    Ok,
    Hypercall(u64),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pending {
    None,
    Read,
    SysregRead,
    SysregWrite,
    Hypercall,
}

fn pending_for(exit: &Exit<Arm64>) -> Pending {
    match exit {
        Exit::Common(c) => match c {
            CommonExit::Mmio { write: None, .. } => Pending::Read,
            CommonExit::Hypercall(_) => Pending::Hypercall,
            CommonExit::Mmio { write: Some(_), .. } | CommonExit::Idle | CommonExit::Shutdown => {
                Pending::None
            }
        },
        Exit::Arch(e) => match e {
            Arm64Exit::Sysreg { write: None, .. } => Pending::SysregRead,
            Arm64Exit::Sysreg { write: Some(_), .. } => Pending::SysregWrite,
        },
    }
}

const MOCK_ARM64_CAPS: MockArm64Caps = Capabilities {
    name: "mock-arm64",
    arch: crate::arch::arm64::Arm64Caps {
        in_kernel_gic: false,
    },
};

#[derive(Debug)]
pub struct MockArm64Backend {
    caps: MockArm64Caps,
    policy: Option<Arm64Policy>,
    script: VecDeque<Exit<Arm64>>,
    pending: Pending,
    completion_staged: bool,
    counts: ExitCounts,
    state: Arm64VcpuState,
    regions: Vec<(Gpa, usize)>,
    injected: Vec<Arm64Injection>,
    pending_irq: Option<GicIntId>,
    accepted_irq: VecDeque<GicIntId>,
    defer_accept: bool,
    completions: Vec<Arm64MockCompletion>,
}

impl Default for MockArm64Backend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockArm64Backend {
    pub fn new() -> Self {
        Self {
            caps: MOCK_ARM64_CAPS,
            policy: None,
            script: VecDeque::new(),
            pending: Pending::None,
            completion_staged: false,
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

    pub fn with_capabilities(caps: MockArm64Caps) -> Self {
        Self {
            caps,
            ..Self::new()
        }
    }

    pub fn with_exits(exits: impl IntoIterator<Item = Exit<Arm64>>) -> Self {
        let mut m = Self::new();
        m.extend_exits(exits);
        m
    }

    pub fn push_exit(&mut self, exit: Exit<Arm64>) -> &mut Self {
        self.script.push_back(exit);
        self
    }

    pub fn extend_exits(&mut self, exits: impl IntoIterator<Item = Exit<Arm64>>) -> &mut Self {
        self.script.extend(exits);
        self
    }

    pub fn is_configured(&self) -> bool {
        self.policy.is_some()
    }

    pub fn has_pending(&self) -> bool {
        self.pending != Pending::None
    }

    pub fn installed_policy(&self) -> Option<&Arm64Policy> {
        self.policy.as_ref()
    }

    pub fn injected(&self) -> &[Arm64Injection] {
        &self.injected
    }

    pub fn pending_irq(&self) -> Option<GicIntId> {
        self.pending_irq
    }

    pub fn completions(&self) -> &[Arm64MockCompletion] {
        &self.completions
    }

    pub fn regions(&self) -> &[(Gpa, usize)] {
        &self.regions
    }

    pub fn set_state(&mut self, state: Arm64VcpuState) -> &mut Self {
        self.state = state;
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

    fn next_scripted(&mut self) -> Result<Exit<Arm64>> {
        self.script
            .pop_front()
            .ok_or(BackendError::Internal("mock-arm64 run-queue empty"))
    }

    fn deliver(&mut self, exit: Exit<Arm64>) -> Exit<Arm64> {
        self.counts.bump(exit.reason());
        self.pending = pending_for(&exit);
        exit
    }

    fn finish(&mut self, completion: Arm64MockCompletion) {
        self.completions.push(completion);
        self.pending = Pending::None;
        self.completion_staged = true;
    }

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
        self.completion_staged = false;
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
        match completion {}
    }

    fn retire_pending_completion(&mut self) -> Result<()> {
        if !self.completion_staged {
            return Ok(());
        }
        Err(BackendError::Unsupported {
            what: "retire_pending_completion (arm64 sysreg)",
        })
    }

    fn save(&self) -> Result<Arm64VcpuState> {
        if self.pending != Pending::None || self.completion_staged {
            return Err(BackendError::PendingCompletion);
        }
        Ok(self.state)
    }

    fn restore(&mut self, state: &Arm64VcpuState) -> Result<()> {
        if self.pending != Pending::None || self.completion_staged {
            return Err(BackendError::PendingCompletion);
        }
        self.state = *state;
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

    fn capabilities(&self) -> MockArm64Caps {
        self.caps
    }
}

#[cfg(test)]
mod tests {
    use super::{Arm64MockCompletion, MockArm64Backend, Pending};
    use crate::arch::arm64::{Arm64Policy, Arm64VcpuState, GicIntId};
    use crate::backend::Backend;
    use crate::exit::{Capabilities, CommonExit, Exit};

    fn configured() -> MockArm64Backend {
        let mut mock = MockArm64Backend::new();
        mock.set_policy(&Arm64Policy::default()).unwrap();
        mock
    }

    #[test]
    fn finish_resolves_pending_exit_and_stages_completion() {
        let mut mock = configured();
        mock.pending = Pending::SysregRead;

        mock.finish(Arm64MockCompletion::Read(0x90));

        assert_eq!(mock.pending, Pending::None);
        assert!(mock.completion_staged);
        assert_eq!(mock.completions, vec![Arm64MockCompletion::Read(0x90)]);
    }

    #[test]
    fn retirement_matches_live_arm64_and_rejects_a_staged_completion() {
        let mut mock = configured();
        mock.pending = Pending::None;
        mock.completion_staged = true;
        mock.completions.push(Arm64MockCompletion::Ok);

        assert!(matches!(
            mock.retire_pending_completion(),
            Err(crate::BackendError::Unsupported {
                what: "retire_pending_completion (arm64 sysreg)"
            })
        ));
        assert!(mock.completion_staged);
        assert_eq!(mock.pending, Pending::None);
        assert_eq!(mock.completions, vec![Arm64MockCompletion::Ok]);
    }

    #[test]
    fn restore_rejects_completion_state_then_clears_interrupt_bookkeeping() {
        let mut mock = configured();
        let mut state = Arm64VcpuState::default();
        state.core.x[0] = 0x1234;
        state.core.pc = 0x4000;

        mock.pending = Pending::SysregWrite;
        mock.completion_staged = true;
        mock.pending_irq = Some(GicIntId(32));
        mock.accepted_irq.push_back(GicIntId(33));
        assert!(matches!(
            mock.restore(&state),
            Err(crate::BackendError::PendingCompletion)
        ));
        mock.pending = Pending::None;
        assert!(matches!(
            mock.restore(&state),
            Err(crate::BackendError::PendingCompletion)
        ));
        mock.completion_staged = false;
        mock.restore(&state).unwrap();

        assert_eq!(mock.save().unwrap(), state);
        assert_eq!(mock.pending, Pending::None);
        assert!(!mock.completion_staged);
        assert_eq!(mock.pending_irq, None);
        assert!(mock.accepted_irq.is_empty());
    }

    #[test]
    fn save_rejects_pending_or_staged_completion_without_mutating_state() {
        let mut mock = configured();
        let mut state = Arm64VcpuState::default();
        state.core.x[0] = 0x1234;
        state.core.pc = 0x4000;
        mock.set_state(state);

        for (pending, complete) in [
            (Pending::Read, Arm64MockCompletion::Read(0x90)),
            (Pending::SysregRead, Arm64MockCompletion::Fault),
            (Pending::SysregWrite, Arm64MockCompletion::Ok),
        ] {
            mock.pending = pending;
            mock.completion_staged = false;
            let completions = mock.completions.clone();
            assert!(matches!(
                mock.save(),
                Err(crate::BackendError::PendingCompletion)
            ));
            assert_eq!(mock.pending, pending);
            assert!(!mock.completion_staged);
            assert_eq!(mock.completions, completions);
            assert_eq!(mock.state, state);

            match complete {
                Arm64MockCompletion::Read(value) => mock.complete_read(value).unwrap(),
                Arm64MockCompletion::Fault => mock.complete_fault().unwrap(),
                Arm64MockCompletion::Ok => mock.complete_ok().unwrap(),
                Arm64MockCompletion::Hypercall(_) => unreachable!(),
            }
            assert_eq!(mock.pending, Pending::None);
            assert!(mock.completion_staged);
            assert!(matches!(
                mock.save(),
                Err(crate::BackendError::PendingCompletion)
            ));
            assert_eq!(mock.pending, Pending::None);
            assert!(mock.completion_staged);

            mock.push_exit(Exit::Common(CommonExit::Idle));
            assert_eq!(mock.run().unwrap(), Exit::Common(CommonExit::Idle));
            assert!(!mock.completion_staged);
            assert_eq!(mock.save().unwrap(), state);
        }
    }

    #[test]
    fn with_capabilities_preserves_the_supplied_capability_record() {
        let caps = Capabilities {
            name: "arm64-test",
            arch: crate::arch::arm64::Arm64Caps {
                in_kernel_gic: true,
            },
        };
        let mock = MockArm64Backend::with_capabilities(caps);
        assert_eq!(mock.capabilities(), caps);
    }
}
