// SPDX-License-Identifier: AGPL-3.0-or-later

mod config;
mod state;

pub use config::{CpuidEntry, CpuidModel, MsrFilter, MsrRange};
pub use state::{
    DebugRegs, DescriptorTable, Segment, VcpuEvents, VcpuRegs, VcpuSregs, VcpuState,
    canonicalize_regs, canonicalize_sregs, canonicalize_xsave, canonicalize_xsave_with_restore_bv,
    restore_xsave_image,
};

use crate::arch::{Arch, ArchExit};
use crate::exit::{CommonExit, ExitReason};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct X86;

impl Arch for X86 {
    type Exit = X86Exit;
    type Injection = Injection;
    type VcpuState = VcpuState;
    type Policy = X86Policy;
    type IntId = u8;
    type Caps = X86Caps;
    type Completion = X86Completion;

    fn stages_common_completion(exit: &CommonExit) -> bool {
        matches!(exit, CommonExit::Mmio { .. }) || exit.stages_completion()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum X86Exit {
    Io {
        port: u16,
        size: u8,
        write: Option<u32>,
    },
    Rdmsr {
        index: u32,
    },
    Wrmsr {
        index: u32,
        value: u64,
    },
    Cpuid {
        leaf: u32,
        subleaf: u32,
    },
}

impl ArchExit for X86Exit {
    fn reason(&self) -> ExitReason {
        match self {
            X86Exit::Io { .. } => ExitReason::Io,
            X86Exit::Rdmsr { .. } => ExitReason::Rdmsr,
            X86Exit::Wrmsr { .. } => ExitReason::Wrmsr,
            X86Exit::Cpuid { .. } => ExitReason::Cpuid,
        }
    }

    fn stages_completion(&self) -> bool {
        match self {
            X86Exit::Io { .. }
            | X86Exit::Rdmsr { .. }
            | X86Exit::Wrmsr { .. }
            | X86Exit::Cpuid { .. } => true,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct X86Policy {
    pub cpuid: CpuidModel,
    pub msr_filter: MsrFilter,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct X86Caps;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum X86Completion {
    Cpuid {
        eax: u32,
        ebx: u32,
        ecx: u32,
        edx: u32,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Injection {
    Interrupt { vector: u8 },
    Nmi,
}
