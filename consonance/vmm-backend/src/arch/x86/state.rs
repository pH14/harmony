// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use crate::types::MpState;

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct VcpuState {
    pub regs: VcpuRegs,
    pub sregs: VcpuSregs,
    pub xcr0: u64,
    pub debugregs: DebugRegs,
    pub events: VcpuEvents,
    pub mp_state: MpState,
    pub msrs: BTreeMap<u32, u64>,
    pub xsave: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct VcpuRegs {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rsp: u64,
    pub rbp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rip: u64,
    pub rflags: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Segment {
    pub base: u64,
    pub limit: u32,
    pub selector: u16,
    pub type_: u8,
    pub present: u8,
    pub dpl: u8,
    pub db: u8,
    pub s: u8,
    pub l: u8,
    pub g: u8,
    pub avl: u8,
    pub unusable: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DescriptorTable {
    pub base: u64,
    pub limit: u16,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct VcpuSregs {
    pub cs: Segment,
    pub ds: Segment,
    pub es: Segment,
    pub fs: Segment,
    pub gs: Segment,
    pub ss: Segment,
    pub tr: Segment,
    pub ldt: Segment,
    pub gdt: DescriptorTable,
    pub idt: DescriptorTable,
    pub cr0: u64,
    pub cr2: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub cr8: u64,
    pub efer: u64,
    pub apic_base: u64,
    pub flags: u64,
    pub pdptrs: [u64; 4],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DebugRegs {
    pub db: [u64; 4],
    pub dr6: u64,
    pub dr7: u64,
    pub flags: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct VcpuEvents {
    pub exception_injected: u8,
    pub exception_nr: u8,
    pub exception_has_error_code: u8,
    pub exception_pending: u8,
    pub exception_error_code: u32,
    pub exception_has_payload: u8,
    pub exception_payload: u64,
    pub interrupt_injected: u8,
    pub interrupt_nr: u8,
    pub interrupt_soft: u8,
    pub interrupt_shadow: u8,
    pub nmi_injected: u8,
    pub nmi_pending: u8,
    pub nmi_masked: u8,
    pub sipi_vector: u32,
    pub flags: u32,
    pub smi_smm: u8,
    pub smi_pending: u8,
    pub smi_inside_nmi: u8,
    pub smi_latched_init: u8,
    pub triple_fault_pending: u8,
}

const RFLAGS_RF: u64 = 1 << 16;

pub fn canonicalize_regs(regs: &mut VcpuRegs) {
    regs.rflags &= !RFLAGS_RF;
}

pub fn canonicalize_sregs(sregs: &mut VcpuSregs) {
    for seg in [
        &mut sregs.cs,
        &mut sregs.ds,
        &mut sregs.es,
        &mut sregs.fs,
        &mut sregs.gs,
        &mut sregs.ss,
        &mut sregs.tr,
        &mut sregs.ldt,
    ] {
        if seg.unusable != 0 {
            *seg = Segment {
                base: seg.base,
                selector: seg.selector,
                unusable: 1,
                ..Segment::default()
            };
        }
    }
}

const XSTATE_BV: usize = 512;
const XCOMP_BV: usize = 520;
const X87_CONTROL: std::ops::Range<usize> = 0..24;
const X87_ST: std::ops::Range<usize> = 32..160;
const SSE_MXCSR: std::ops::Range<usize> = 24..28;
const MXCSR_MASK: std::ops::Range<usize> = 28..32;
const MXCSR_MASK_PINNED: [u8; 4] = 0x0000FFFFu32.to_le_bytes();
const SSE_XMM: std::ops::Range<usize> = 160..416;
const X87_INIT_FCW: [u8; 2] = 0x037Fu16.to_le_bytes();
const SSE_INIT_MXCSR: [u8; 4] = 0x1F80u32.to_le_bytes();
const LEGACY_TAIL: std::ops::Range<usize> = 416..512;

pub fn canonicalize_xsave(image: &mut [u8]) {
    if image.len() < XCOMP_BV + 8 || image[XCOMP_BV..XCOMP_BV + 8] != [0u8; 8] {
        return;
    }

    image[MXCSR_MASK].copy_from_slice(&MXCSR_MASK_PINNED);
    image[LEGACY_TAIL].fill(0);

    let is_zero = |r: std::ops::Range<usize>, image: &[u8]| image[r].iter().all(|&b| b == 0);
    let x87_init = |image: &[u8]| {
        image[X87_CONTROL.start..X87_CONTROL.start + 2] == X87_INIT_FCW
            && is_zero(X87_CONTROL.start + 2..X87_CONTROL.end, image)
            && is_zero(X87_ST, image)
    };
    let sse_init = |image: &[u8]| image[SSE_MXCSR] == SSE_INIT_MXCSR && is_zero(SSE_XMM, image);

    let mut bv = u64::from_le_bytes(image[XSTATE_BV..XSTATE_BV + 8].try_into().expect("8 bytes"));
    if bv & 1 == 0 {
        image[X87_CONTROL.start..X87_CONTROL.start + 2].copy_from_slice(&X87_INIT_FCW);
        image[X87_CONTROL.start + 2..X87_CONTROL.end].fill(0);
        image[X87_ST].fill(0);
    } else if x87_init(image) {
        bv &= !1;
    }
    if bv & 2 == 0 {
        image[SSE_MXCSR].copy_from_slice(&SSE_INIT_MXCSR);
        image[SSE_XMM].fill(0);
    } else if sse_init(image) {
        bv &= !2;
    }
    image[XSTATE_BV..XSTATE_BV + 8].copy_from_slice(&bv.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_image(xstate_bv: u64) -> Vec<u8> {
        let mut image = vec![0u8; 4096];
        image[0..2].copy_from_slice(&X87_INIT_FCW);
        image[SSE_MXCSR].copy_from_slice(&SSE_INIT_MXCSR);
        image[28..32].copy_from_slice(&0xFFFFu32.to_le_bytes());
        image[XSTATE_BV..XSTATE_BV + 8].copy_from_slice(&xstate_bv.to_le_bytes());
        image
    }

    #[test]
    fn init_state_encodings_collapse_to_one_image() {
        let mut a = init_image(0x3);
        let mut b = init_image(0x2);
        canonicalize_xsave(&mut a);
        canonicalize_xsave(&mut b);
        assert_eq!(a, b);
        assert_eq!(a[XSTATE_BV..XSTATE_BV + 8], 0u64.to_le_bytes());
    }

    #[test]
    fn live_state_is_untouched() {
        let mut image = init_image(0x3);
        image[0..2].copy_from_slice(&0x027Fu16.to_le_bytes());
        image[SSE_XMM.start] = 0x5A;
        let before = image.clone();
        canonicalize_xsave(&mut image);
        assert_eq!(image, before);
    }

    #[test]
    fn ignored_area_bytes_become_the_init_values() {
        let mut image = init_image(0x0);
        image[X87_ST.start] = 0xEE;
        image[SSE_XMM.start + 7] = 0xEE;
        canonicalize_xsave(&mut image);
        assert_eq!(image, init_image(0x0));
    }

    #[test]
    fn mxcsr_mask_is_pinned_to_the_contract_value() {
        let mut image = init_image(0x2);
        image[MXCSR_MASK].copy_from_slice(&0x0002FFFFu32.to_le_bytes());
        canonicalize_xsave(&mut image);
        assert_eq!(image[MXCSR_MASK], MXCSR_MASK_PINNED);
    }

    #[test]
    fn legacy_tail_host_template_is_zeroed() {
        let mut a = init_image(0x2);
        let mut b = init_image(0x2);
        a[464..472].copy_from_slice(&0x7u64.to_le_bytes());
        b[464..472].copy_from_slice(&0x600e7u64.to_le_bytes());
        canonicalize_xsave(&mut a);
        canonicalize_xsave(&mut b);
        assert_eq!(a, b);
        assert!(a[LEGACY_TAIL].iter().all(|&x| x == 0));
    }

    #[test]
    fn rf_exit_residue_collapses_across_vendors() {
        let mut intel = VcpuRegs {
            rflags: 0x10282,
            ..VcpuRegs::default()
        };
        let mut amd = VcpuRegs {
            rflags: 0x282,
            ..VcpuRegs::default()
        };
        canonicalize_regs(&mut intel);
        canonicalize_regs(&mut amd);
        assert_eq!(intel, amd);
        assert_eq!(intel.rflags, 0x282);
    }

    #[test]
    fn regs_other_than_rf_are_untouched() {
        let mut regs = VcpuRegs {
            rax: 0x1234,
            rsp: 0xffff_ffff_8260_3e98,
            rip: 0xffff_ffff_8125_6a62,
            rflags: 0x10ac6,
            ..VcpuRegs::default()
        };
        canonicalize_regs(&mut regs);
        assert_eq!(regs.rax, 0x1234);
        assert_eq!(regs.rsp, 0xffff_ffff_8260_3e98);
        assert_eq!(regs.rip, 0xffff_ffff_8125_6a62);
        assert_eq!(regs.rflags, 0xac6);
    }

    #[test]
    fn unusable_segment_residue_collapses_to_the_zeroed_form() {
        let intel = Segment {
            base: 726582208,
            selector: 0x23,
            limit: 0xFFFF_FFFF,
            type_: 1,
            db: 1,
            g: 1,
            unusable: 1,
            ..Segment::default()
        };
        let amd = Segment {
            base: 726582208,
            selector: 0x23,
            unusable: 1,
            ..Segment::default()
        };
        let mut a = VcpuSregs {
            fs: intel,
            ..VcpuSregs::default()
        };
        let mut b = VcpuSregs {
            fs: amd,
            ..VcpuSregs::default()
        };
        canonicalize_sregs(&mut a);
        canonicalize_sregs(&mut b);
        assert_eq!(a, b);
        assert_eq!(a.fs.base, 726582208);
        assert_eq!(a.fs.selector, 0x23);
        assert_eq!(a.fs.unusable, 1);
    }

    #[test]
    fn usable_segments_are_untouched() {
        let cs = Segment {
            limit: 0xFFFF_FFFF,
            selector: 0x10,
            type_: 11,
            present: 1,
            s: 1,
            l: 1,
            g: 1,
            ..Segment::default()
        };
        let mut sregs = VcpuSregs {
            cs,
            ..VcpuSregs::default()
        };
        let before = sregs;
        canonicalize_sregs(&mut sregs);
        assert_eq!(sregs, before);
    }

    #[test]
    fn compacted_images_are_untouched() {
        let mut image = init_image(0x3);
        image[XCOMP_BV..XCOMP_BV + 8].copy_from_slice(&(1u64 << 63 | 0x3).to_le_bytes());
        let before = image.clone();
        canonicalize_xsave(&mut image);
        assert_eq!(image, before);
    }

    #[test]
    fn xsave_header_length_boundary_is_fail_closed_and_inclusive() {
        let mut short = vec![0xa5; XCOMP_BV + 7];
        let before = short.clone();
        canonicalize_xsave(&mut short);
        assert_eq!(short, before);

        let mut exact = init_image(0x2);
        exact.truncate(XCOMP_BV + 8);
        exact[MXCSR_MASK].copy_from_slice(&0x0002_FFFFu32.to_le_bytes());
        exact[LEGACY_TAIL].fill(0xa5);
        canonicalize_xsave(&mut exact);
        assert_eq!(exact[MXCSR_MASK], MXCSR_MASK_PINNED);
        assert!(exact[LEGACY_TAIL].iter().all(|&byte| byte == 0));
    }
}
