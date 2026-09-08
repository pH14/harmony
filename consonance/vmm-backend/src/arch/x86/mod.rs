// SPDX-License-Identifier: AGPL-3.0-or-later
//! The x86-64 vendor: the [`Arch`] implementation ([`X86`]) and its value-type
//! vocabulary — the exit variants ([`X86Exit`]), the full register record set
//! ([`VcpuState`] and its subrecords), the installed-policy tables
//! ([`X86Policy`]: [`CpuidModel`] + [`MsrFilter`]), the injectable events
//! ([`Injection`]), and the capability flags ([`X86Caps`]).

mod config;
mod state;

pub use config::{CpuidEntry, CpuidModel, MsrFilter, MsrRange};
pub use state::{
    DebugRegs, DescriptorTable, Segment, VcpuEvents, VcpuRegs, VcpuSregs, VcpuState,
    canonicalize_regs, canonicalize_sregs, canonicalize_xsave,
};

use crate::arch::{Arch, ArchExit};
use crate::exit::{CommonExit, ExitReason};

/// The x86-64 vendor (a zero-sized type; `docs/ARCHITECTURE.md`).
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

/// The x86-specific exit variants — the per-ISA half of the two-level
/// [`Exit`](crate::Exit). Cross-arch exits (MMIO, hypercall, idle, shutdown,
/// deadline) live in [`CommonExit`](crate::CommonExit).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum X86Exit {
    /// Port I/O. `write = Some(v)` is `OUT(v)`; KVM retains its fast-PIO
    /// callback until the next entry. `write = None` is `IN`, resolved by
    /// `complete_read`.
    Io {
        /// I/O port.
        port: u16,
        /// Access width in bytes (1/2/4).
        size: u8,
        /// `Some(v)` = OUT value; `None` = IN (awaits `complete_read`).
        write: Option<u32>,
    },
    /// A filtered MSR read → `complete_read(value)` (allow/fixed/emulate) or
    /// `complete_fault()` (the contract's `deny-gp`).
    Rdmsr {
        /// The MSR index the guest read.
        index: u32,
    },
    /// A filtered MSR write → `complete_ok()` (allow/drop) or
    /// `complete_fault()` (`deny-gp`). Stays pending until one is called:
    /// resuming without a completion is taken by KVM as a silent *allow*
    /// (`msr.error == 0`).
    Wrmsr {
        /// The MSR index the guest wrote.
        index: u32,
        /// The value the guest wrote.
        value: u64,
    },
    /// CPUID → `complete_arch` with the result quad
    /// ([`X86Completion::Cpuid`]). **Stock `KvmBackend` services CPUID
    /// in-kernel from the installed table and does not surface this**; a
    /// backend that does is completed with the dyn-overlaid quad.
    Cpuid {
        /// CPUID leaf (`EAX`).
        leaf: u32,
        /// CPUID subleaf (`ECX`).
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

/// The installed x86 CPU-contract policy: the frozen guest-visible CPUID model
/// and the default-deny MSR filter, installed together (before the first run)
/// through [`Backend::set_policy`](crate::Backend::set_policy).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct X86Policy {
    /// The frozen guest-visible CPUID table (→ `KVM_SET_CPUID2`).
    pub cpuid: CpuidModel,
    /// The default-deny MSR policy (→ `KVM_X86_SET_MSR_FILTER`).
    pub msr_filter: MsrFilter,
}

/// Empty x86 feature payload: the shared capability record remains architecture
/// typed, while arm64 carries its live in-kernel GIC ownership flag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct X86Caps;

/// The x86 arch-payload completions ([`Arch::Completion`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum X86Completion {
    /// The result quad for a pending [`X86Exit::Cpuid`].
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

/// An event the VMM injects at a V-time-chosen boundary. Aligned to R1's roster:
/// under `KVM_IRQCHIP_NONE` maskable IRQs come only from the `KVM_INTERRUPT`
/// queue and NMIs via `KVM_NMI` — no other producer exists.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Injection {
    /// A maskable interrupt vector (`KVM_INTERRUPT`).
    Interrupt {
        /// The 8-bit interrupt vector.
        vector: u8,
    },
    /// A non-maskable interrupt (`KVM_NMI`).
    Nmi,
}
