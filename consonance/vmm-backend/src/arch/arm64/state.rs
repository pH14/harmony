// SPDX-License-Identifier: AGPL-3.0-or-later
//! The live, in-memory arm64 vCPU snapshot the backend produces (`save`) and
//! consumes (`restore`).
//!
//! The counterpart of `arch/x86/state.rs`: vmm-core marshals an
//! [`Arm64VcpuState`] into `vm-state`'s arm64 record set for the codec; per
//! rule #2 this crate does not depend on `vm-state`, so the field set is
//! mirrored plain data kept consistent by review.
//!
//! The vCPU record also carries a substrate-neutral GICv3 record when the
//! backend owns an in-kernel interrupt controller. `vmm-core` moves that record
//! into the existing arm64 device blob, so an in-kernel KVM vGIC and the HVF
//! userspace model have one canonical snapshot representation.

use crate::types::MpState;

/// PSTATE.TCO (Tag Check Override).
///
/// Harmony's portable identity advertises `ID_AA64PFR1_EL1.MTE=0` and the
/// vCPU feature bitmap does not opt into MTE. TCO is therefore outside the
/// guest's architectural state contract. Some KVM hosts still expose the
/// physical CPU's exception-entry value in `KVM_GET_ONE_REG(PSTATE)`, while
/// HVF reports zero. Strip that substrate residue at the backend boundary.
const PSTATE_TCO: u64 = 1 << 25;

/// Canonicalize core state whose feature is absent from the portable identity.
pub(crate) fn canonicalize_core_regs(core: &mut Arm64CoreRegs) {
    core.pstate &= !PSTATE_TCO;
    core.spsr_el1 &= !PSTATE_TCO;
}

/// Whether a decoded snapshot contains non-canonical, unsupported core bits.
pub(crate) fn has_noncanonical_core_regs(core: &Arm64CoreRegs) -> bool {
    (core.pstate | core.spsr_el1) & PSTATE_TCO != 0
}

/// Full guest-visible arm64 vCPU state for snapshot/restore (skeleton subset;
/// full sysreg set `TODO(AA-6)`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Arm64VcpuState {
    /// Core registers (`KVM_GET_ONE_REG` over the `KVM_REG_ARM_CORE` ids).
    pub core: Arm64CoreRegs,
    /// The skeleton EL1 system-register file (`KVM_GET_ONE_REG` over
    /// `KVM_REG_ARM64_SYSREG` ids).
    pub sysregs: Arm64SysregFile,
    /// SIMD/FP architectural state (`Q0..Q31`, `FPCR`, `FPSR`).
    pub simd_fp: Arm64SimdFpState,
    /// Hardware breakpoint/watchpoint and debug trap-control state.
    pub debug: Arm64DebugState,
    /// EL1 host virtual-timer registers and canonical quarantine state.
    pub vtimer: Arm64VtimerState,
    /// Pending IRQ/FIQ levels exposed by the backend.
    pub interrupts: Arm64InterruptState,
    /// Runnable vs halted (`KVM_GET_MP_STATE`; WFI-halted on arm64).
    pub mp_state: MpState,
    /// Canonical architectural GICv3 state when the backend owns the fabric.
    /// Userspace-fabric backends leave this `None` and retain the equivalent
    /// record in their device model.
    pub gic: Option<Arm64GicState>,
}

/// Bitmap words in the canonical GICv3 ordinary-INTID space.
pub const ARM64_GIC_BITMAP_WORDS: usize = 32;

/// Priority bytes in the canonical GICv3 ordinary-INTID space.
pub const ARM64_GIC_PRIORITY_BYTES: usize = 1020;

/// Substrate-neutral GICv3 architectural state.
///
/// `pending` is the software/edge pending latch. `line_level` is separate:
/// KVM's migration ABI explicitly requires both to reproduce a level-triggered
/// input, because neither can be derived from the other.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Arm64GicState {
    /// Canonical record layout version.
    pub version: u32,
    /// Implemented shared peripheral interrupt count.
    pub impl_spis: u32,
    /// Architectural timer frequency fixed by the board contract.
    pub timer_hz: u64,
    /// Virtual-timer PPI INTID.
    pub timer_intid: u32,
    /// Writable Group-1 forwarding bits of `GICD_CTLR`.
    pub gicd_ctlr: u32,
    /// Group membership (`1` is Group 1).
    pub group: [u32; ARM64_GIC_BITMAP_WORDS],
    /// Enable bitmap.
    pub enable: [u32; ARM64_GIC_BITMAP_WORDS],
    /// Software/edge pending latch bitmap.
    pub pending: [u32; ARM64_GIC_BITMAP_WORDS],
    /// Active bitmap.
    pub active: [u32; ARM64_GIC_BITMAP_WORDS],
    /// External input-line levels, distinct from the pending latch.
    pub line_level: [u32; ARM64_GIC_BITMAP_WORDS],
    /// One priority byte per ordinary INTID.
    pub priority: [u8; ARM64_GIC_PRIORITY_BYTES],
    /// `ICC_PMR_EL1`.
    pub pmr: u8,
    /// `ICC_IGRPEN1_EL1` Group-1 CPU-interface enable.
    pub igrpen1: bool,
    /// Canonical virtual-timer control bits.
    pub cntv_ctl: u64,
    /// Canonical virtual-timer compare value.
    pub cntv_cval: u64,
    /// Whether the current timer arming has fired.
    pub timer_fired: bool,
}

impl Default for Arm64GicState {
    fn default() -> Self {
        Self {
            version: 3,
            impl_spis: 0,
            timer_hz: 0,
            timer_intid: 0,
            gicd_ctlr: 0,
            group: [0; ARM64_GIC_BITMAP_WORDS],
            enable: [0; ARM64_GIC_BITMAP_WORDS],
            pending: [0; ARM64_GIC_BITMAP_WORDS],
            active: [0; ARM64_GIC_BITMAP_WORDS],
            line_level: [0; ARM64_GIC_BITMAP_WORDS],
            priority: [0; ARM64_GIC_PRIORITY_BYTES],
            pmr: 0,
            igrpen1: false,
            cntv_ctl: 0,
            cntv_cval: 0,
            timer_fired: false,
        }
    }
}

/// The arm64 core register file (`struct kvm_regs.regs` — `user_pt_regs` —
/// plus the EL1 banked exception registers KVM carries alongside it).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Arm64CoreRegs {
    /// General-purpose registers `x0..x30`.
    pub x: [u64; 31],
    /// The stack pointer selected at the current EL (`SP_EL0` at EL0).
    pub sp: u64,
    /// The program counter.
    pub pc: u64,
    /// The processor state (`PSTATE`/`SPSR` layout: `DAIF`, the mode field,
    /// the condition flags).
    pub pstate: u64,
    /// `SP_EL1` — the banked EL1 stack pointer.
    pub sp_el1: u64,
    /// `ELR_EL1` — the EL1 exception link register.
    pub elr_el1: u64,
    /// `SPSR_EL1` — the EL1 saved processor state.
    pub spsr_el1: u64,
}

/// The skeleton EL1 system-register file: the named subset a trivial vCPU
/// round-trip needs (MMU/translation, vectors, thread pointers, the traps-
/// and-counter control the determinism contract cares about). **Not** the
/// snapshot contract: `TODO(AA-6)` owns which sysregs a snapshot must carry;
/// this file grows only from that measured record set.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[allow(missing_docs)] // the system-register names are self-documenting
pub struct Arm64SysregFile {
    pub sctlr_el1: u64,
    pub ttbr0_el1: u64,
    pub ttbr1_el1: u64,
    pub tcr_el1: u64,
    pub mair_el1: u64,
    pub vbar_el1: u64,
    pub cpacr_el1: u64,
    pub esr_el1: u64,
    pub far_el1: u64,
    pub tpidr_el0: u64,
    pub tpidr_el1: u64,
    /// `CNTKCTL_EL1` — the EL0 counter-access control the paravirt-clock
    /// closure story turns off (`docs/PARAVIRT-CLOCK.md` §4.2); carried so the
    /// closure posture survives a snapshot.
    pub cntkctl_el1: u64,
}

/// SIMD/FP state retained across snapshots.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Arm64SimdFpState {
    /// Vector registers `Q0..Q31` in architectural byte order.
    pub q: [[u8; 16]; 32],
    /// Floating-point control register.
    pub fpcr: u64,
    /// Floating-point status register.
    pub fpsr: u64,
}

/// Debug register file and the two HVF trap controls.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Arm64DebugState {
    /// Breakpoint value registers `DBGBVR0_EL1..DBGBVR15_EL1`.
    pub breakpoint_value: [u64; 16],
    /// Breakpoint control registers `DBGBCR0_EL1..DBGBCR15_EL1`.
    pub breakpoint_control: [u64; 16],
    /// Watchpoint value registers `DBGWVR0_EL1..DBGWVR15_EL1`.
    pub watchpoint_value: [u64; 16],
    /// Watchpoint control registers `DBGWCR0_EL1..DBGWCR15_EL1`.
    pub watchpoint_control: [u64; 16],
    /// Monitor debug system control register.
    pub mdscr_el1: u64,
    /// Whether guest debug exceptions trap to the backend.
    pub trap_debug_exceptions: bool,
    /// Whether guest debug-register accesses trap to the backend.
    pub trap_debug_reg_accesses: bool,
}

/// Substrate-neutral state of the host-backed architectural virtual timer.
///
/// Harmony's deterministic timer is the userspace exit-count clockevent. The
/// host timer is therefore quarantined on both substrates: HVF masks its
/// automatic exit, while KVM routes it to an unused PPI. `masked` records that
/// shared invariant rather than either substrate's private mechanism.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Arm64VtimerState {
    /// Writable `CNTV_CTL_EL0` bits (`ENABLE | IMASK`; never read-only ISTATUS).
    pub cntv_ctl_el0: u64,
    /// `CNTV_CVAL_EL0`.
    pub cntv_cval_el0: u64,
    /// Whether the host-backed timer is quarantined from deterministic PPI27.
    pub masked: bool,
    /// Canonical host-counter offset. The deterministic composition requires
    /// zero because KVM has no portable counterpart to HVF's private offset.
    pub offset: u64,
}

impl Default for Arm64VtimerState {
    fn default() -> Self {
        Self {
            cntv_ctl_el0: 0,
            cntv_cval_el0: 0,
            masked: true,
            offset: 0,
        }
    }
}

/// Pending interrupt levels retained by the backend API.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Arm64InterruptState {
    /// Pending IRQ level.
    pub irq: bool,
    /// Pending FIQ level.
    pub fiq: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_mte_makes_tco_a_canonical_zero() {
        let canonical = Arm64CoreRegs {
            pstate: 0xc5,
            spsr_el1: 0x6000_0005,
            ..Default::default()
        };
        let mut physical_exception_residue = canonical;
        physical_exception_residue.pstate |= PSTATE_TCO;
        physical_exception_residue.spsr_el1 |= PSTATE_TCO;

        // Planted negative: an identity comparison without canonicalization
        // detects the exact host exception-entry residue seen in M5.
        assert_ne!(physical_exception_residue, canonical);
        assert!(has_noncanonical_core_regs(&physical_exception_residue));

        canonicalize_core_regs(&mut physical_exception_residue);
        assert_eq!(physical_exception_residue, canonical);
        assert!(!has_noncanonical_core_regs(&physical_exception_residue));
    }

    #[test]
    fn canonicalization_preserves_every_supported_pstate_bit() {
        let mut core = Arm64CoreRegs {
            pstate: u64::MAX,
            spsr_el1: u64::MAX,
            ..Default::default()
        };
        canonicalize_core_regs(&mut core);
        assert_eq!(core.pstate, u64::MAX & !PSTATE_TCO);
        assert_eq!(core.spsr_el1, u64::MAX & !PSTATE_TCO);
    }
}
