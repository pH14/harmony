// SPDX-License-Identifier: AGPL-3.0-or-later
//! The x86-64 vendor's value-type vocabulary: the full register record set
//! ([`VcpuState`] and its subrecords), the installed-policy tables
//! ([`CpuidModel`] / [`MsrFilter`]), the injectable events ([`Injection`]),
//! and the retired-conditional-branch work-counter event pin.

mod config;
mod state;

pub use config::{CpuidEntry, CpuidModel, MsrFilter, MsrRange};
pub use state::{DebugRegs, DescriptorTable, Segment, VcpuEvents, VcpuRegs, VcpuSregs, VcpuState};

/// `BR_INST_RETIRED.CONDITIONAL` (event `0xC4`, umask `0x01`), Coffee Lake-S
/// (i9-9900K) — the exact `PERF_TYPE_RAW` event task 07 validated, identical to
/// vmm-core's `work_perf`. The x86 work-counter event pin: each vendor supplies
/// its own retired-branch-class raw event with its backend
/// (`docs/ARCH-BOUNDARY.md` §A).
pub(crate) const RAW_BR_COND: u64 = 0x1c4;

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
