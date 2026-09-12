// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::types::MpState;

const PSTATE_TCO: u64 = 1 << 25;
const PSTATE_BTYPE: u64 = 0b11 << 10;
const PSTATE_UNSUPPORTED: u64 = PSTATE_TCO | PSTATE_BTYPE;

pub(crate) fn canonicalize_core_regs(core: &mut Arm64CoreRegs) {
    core.pstate &= !PSTATE_UNSUPPORTED;
    core.spsr_el1 &= !PSTATE_UNSUPPORTED;
}

pub(crate) fn has_noncanonical_core_regs(core: &Arm64CoreRegs) -> bool {
    (core.pstate | core.spsr_el1) & PSTATE_UNSUPPORTED != 0
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Arm64VcpuState {
    pub core: Arm64CoreRegs,
    pub sysregs: Arm64SysregFile,
    pub simd_fp: Arm64SimdFpState,
    pub debug: Arm64DebugState,
    pub vtimer: Arm64VtimerState,
    pub interrupts: Arm64InterruptState,
    pub mp_state: MpState,
    pub gic: Option<Arm64GicState>,
}

pub const ARM64_GIC_BITMAP_WORDS: usize = 32;

pub const ARM64_GIC_PRIORITY_BYTES: usize = 1020;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Arm64GicState {
    pub version: u32,
    pub impl_spis: u32,
    pub timer_hz: u64,
    pub timer_intid: u32,
    pub gicd_ctlr: u32,
    pub group: [u32; ARM64_GIC_BITMAP_WORDS],
    pub enable: [u32; ARM64_GIC_BITMAP_WORDS],
    pub pending: [u32; ARM64_GIC_BITMAP_WORDS],
    pub active: [u32; ARM64_GIC_BITMAP_WORDS],
    pub line_level: [u32; ARM64_GIC_BITMAP_WORDS],
    pub priority: [u8; ARM64_GIC_PRIORITY_BYTES],
    pub pmr: u8,
    pub igrpen1: bool,
    pub cntv_ctl: u64,
    pub cntv_cval: u64,
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

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Arm64CoreRegs {
    pub x: [u64; 31],
    pub sp: u64,
    pub pc: u64,
    pub pstate: u64,
    pub sp_el1: u64,
    pub elr_el1: u64,
    pub spsr_el1: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
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
    pub cntkctl_el1: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Arm64SimdFpState {
    pub q: [[u8; 16]; 32],
    pub fpcr: u64,
    pub fpsr: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Arm64DebugState {
    pub breakpoint_value: [u64; 16],
    pub breakpoint_control: [u64; 16],
    pub watchpoint_value: [u64; 16],
    pub watchpoint_control: [u64; 16],
    pub mdscr_el1: u64,
    pub trap_debug_exceptions: bool,
    pub trap_debug_reg_accesses: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Arm64VtimerState {
    pub cntv_ctl_el0: u64,
    pub cntv_cval_el0: u64,
    pub masked: bool,
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

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Arm64InterruptState {
    pub irq: bool,
    pub fiq: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pstate_tco_is_bit_25() {
        assert_eq!(PSTATE_TCO, 0x0200_0000);
    }

    #[test]
    fn absent_mte_and_bti_make_tco_and_btype_canonical_zero() {
        let canonical = Arm64CoreRegs {
            pstate: 0xc5,
            spsr_el1: 0x6000_0005,
            ..Default::default()
        };
        let mut physical_exception_residue = canonical;
        physical_exception_residue.pstate |= PSTATE_TCO | PSTATE_BTYPE;
        physical_exception_residue.spsr_el1 |= PSTATE_TCO | PSTATE_BTYPE;

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
        assert_eq!(core.pstate, !PSTATE_UNSUPPORTED);
        assert_eq!(core.spsr_el1, !PSTATE_UNSUPPORTED);
    }
}
