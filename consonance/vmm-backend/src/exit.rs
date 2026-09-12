// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::arch::{Arch, ArchExit};
use crate::types::Gpa;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Exit<A: Arch> {
    Common(CommonExit),
    Arch(A::Exit),
}

impl<A: Arch> Exit<A> {
    pub fn reason(&self) -> ExitReason {
        match self {
            Exit::Common(c) => c.reason(),
            Exit::Arch(e) => e.reason(),
        }
    }

    pub fn stages_completion(&self) -> bool {
        match self {
            Exit::Common(c) => A::stages_common_completion(c),
            Exit::Arch(e) => e.stages_completion(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommonExit {
    Mmio {
        gpa: Gpa,
        size: u8,
        write: Option<u64>,
    },
    Hypercall(HypercallFrame),
    Idle,
    Shutdown,
}

impl CommonExit {
    pub fn reason(&self) -> ExitReason {
        match self {
            CommonExit::Mmio { .. } => ExitReason::Mmio,
            CommonExit::Hypercall(_) => ExitReason::Hypercall,
            CommonExit::Idle => ExitReason::Idle,
            CommonExit::Shutdown => ExitReason::Shutdown,
        }
    }

    pub fn stages_completion(&self) -> bool {
        match self {
            CommonExit::Mmio { write: None, .. } => true,
            CommonExit::Mmio { write: Some(_), .. }
            | CommonExit::Hypercall(_)
            | CommonExit::Idle
            | CommonExit::Shutdown => false,
        }
    }
}

impl<A: Arch> From<CommonExit> for Exit<A> {
    fn from(c: CommonExit) -> Self {
        Exit::Common(c)
    }
}

#[cfg(test)]
mod completion_tests {
    use super::*;

    #[test]
    fn only_an_mmio_load_stages_a_common_completion_by_default() {
        let load = CommonExit::Mmio {
            gpa: Gpa(0x1000),
            size: 4,
            write: None,
        };
        let store = CommonExit::Mmio {
            gpa: Gpa(0x1000),
            size: 4,
            write: Some(7),
        };
        assert!(load.stages_completion());
        assert!(!store.stages_completion());
        assert!(!CommonExit::Idle.stages_completion());
        assert!(!CommonExit::Shutdown.stages_completion());
    }

    #[test]
    fn x86_mmio_store_uses_the_architecture_specific_staging_rule() {
        use crate::arch::x86::X86;

        let store = CommonExit::Mmio {
            gpa: Gpa(0x1000),
            size: 4,
            write: Some(7),
        };
        assert!(!store.stages_completion());
        assert!(Exit::<X86>::Common(store).stages_completion());
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct HypercallFrame {
    pub args: [u64; 4],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capabilities<C> {
    pub name: &'static str,
    pub arch: C,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum ExitReason {
    Io,
    Mmio,
    Rdmsr,
    Wrmsr,
    Hypercall,
    Cpuid,
    Idle,
    Shutdown,
    Sysreg,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ExitCounts {
    pub io: u64,
    pub mmio: u64,
    pub rdmsr: u64,
    pub wrmsr: u64,
    pub hypercall: u64,
    pub cpuid: u64,
    pub idle: u64,
    pub shutdown: u64,
    pub sysreg: u64,
}

impl ExitCounts {
    pub fn total(&self) -> u64 {
        self.entries()
            .iter()
            .fold(0u64, |acc, (_, n)| acc.saturating_add(*n))
    }

    pub fn entries(&self) -> [(ExitReason, u64); 9] {
        [
            (ExitReason::Io, self.io),
            (ExitReason::Mmio, self.mmio),
            (ExitReason::Rdmsr, self.rdmsr),
            (ExitReason::Wrmsr, self.wrmsr),
            (ExitReason::Hypercall, self.hypercall),
            (ExitReason::Cpuid, self.cpuid),
            (ExitReason::Idle, self.idle),
            (ExitReason::Shutdown, self.shutdown),
            (ExitReason::Sysreg, self.sysreg),
        ]
    }

    #[cfg_attr(
        not(any(
            feature = "mock",
            test,
            all(target_os = "linux", target_arch = "x86_64")
        )),
        allow(dead_code)
    )]
    pub(crate) fn bump(&mut self, reason: ExitReason) {
        let slot = match reason {
            ExitReason::Io => &mut self.io,
            ExitReason::Mmio => &mut self.mmio,
            ExitReason::Rdmsr => &mut self.rdmsr,
            ExitReason::Wrmsr => &mut self.wrmsr,
            ExitReason::Hypercall => &mut self.hypercall,
            ExitReason::Cpuid => &mut self.cpuid,
            ExitReason::Idle => &mut self.idle,
            ExitReason::Shutdown => &mut self.shutdown,
            ExitReason::Sysreg => &mut self.sysreg,
        };
        *slot = slot.saturating_add(1);
    }
}
