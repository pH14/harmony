// SPDX-License-Identifier: AGPL-3.0-or-later

use vmm_backend::{MpState, Segment, VcpuRegs, VcpuSregs, VcpuState};

const CR0_PE: u64 = 1;
const CR0_NE: u64 = 1 << 5;

const FLAT_LIMIT: u32 = 0xFFFF_FFFF;

fn task_register() -> Segment {
    Segment {
        base: 0,
        limit: 0xFFFF,
        selector: 0,
        type_: 0xB,
        present: 1,
        dpl: 0,
        db: 0,
        s: 0,
        l: 0,
        g: 0,
        avl: 0,
        unusable: 0,
    }
}

const CR0_PG: u64 = 1 << 31;
const CR4_PAE: u64 = 1 << 5;
const EFER_LME: u64 = 1 << 8;
const EFER_LMA: u64 = 1 << 10;

const BOOT_CS_SELECTOR: u16 = 0x10;
const BOOT_DS_SELECTOR: u16 = 0x18;

fn long_code_segment() -> Segment {
    Segment {
        base: 0,
        limit: FLAT_LIMIT,
        selector: BOOT_CS_SELECTOR,
        type_: 0xB,
        present: 1,
        dpl: 0,
        db: 0,
        s: 1,
        l: 1,
        g: 1,
        avl: 0,
        unusable: 0,
    }
}

fn long_data_segment() -> Segment {
    Segment {
        base: 0,
        limit: FLAT_LIMIT,
        selector: BOOT_DS_SELECTOR,
        type_: 0x3,
        present: 1,
        dpl: 0,
        db: 1,
        s: 1,
        l: 0,
        g: 1,
        avl: 0,
        unusable: 0,
    }
}

pub fn long_mode_entry(
    entry_rip: u64,
    boot_params_gpa: u64,
    page_table_root: u64,
    gdt_gpa: u64,
) -> VcpuState {
    let data = long_data_segment();
    let sregs = VcpuSregs {
        cs: long_code_segment(),
        ds: data,
        es: data,
        fs: data,
        gs: data,
        ss: data,
        tr: task_register(),
        ldt: Segment {
            unusable: 1,
            ..Segment::default()
        },
        gdt: vmm_backend::DescriptorTable {
            base: gdt_gpa,
            limit: 0x1F,
        },
        idt: vmm_backend::DescriptorTable { base: 0, limit: 0 },
        cr0: CR0_PE | CR0_NE | CR0_PG,
        cr2: 0,
        cr3: page_table_root,
        cr4: CR4_PAE,
        cr8: 0,
        efer: EFER_LME | EFER_LMA,
        apic_base: 0xFEE0_0900,
        flags: 0,
        pdptrs: [0; 4],
    };

    let regs = VcpuRegs {
        rsi: boot_params_gpa,
        rip: entry_rip,
        rflags: 0x0000_0002,
        ..VcpuRegs::default()
    };

    VcpuState {
        regs,
        sregs,
        xcr0: 1,
        debugregs: vmm_backend::DebugRegs {
            db: [0; 4],
            dr6: 0xFFFF_0FF0,
            dr7: 0x0000_0400,
            flags: 0,
        },
        events: vmm_backend::VcpuEvents::default(),
        mp_state: MpState::Runnable,
        msrs: Default::default(),
        xsave: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_register_bits_match_the_architecture() {
        assert_eq!(CR0_NE, 0x20, "CR0.NE is bit 5");
        assert_eq!(CR0_PG, 0x8000_0000, "CR0.PG is bit 31");
        assert_eq!(CR4_PAE, 0x20, "CR4.PAE is bit 5");
        assert_eq!(EFER_LME, 0x100, "EFER.LME is bit 8");
        assert_eq!(EFER_LMA, 0x400, "EFER.LMA is bit 10");
    }

    #[test]
    fn long_mode_entry_matches_64bit_boot_protocol() {
        let st = long_mode_entry(0x10_0200, 0x7000, 0x1000, 0x6000);
        assert_eq!(st.regs.rip, 0x10_0200);
        assert_eq!(st.regs.rsi, 0x7000);
        assert_eq!(st.regs.rax, 0);
        assert_eq!(st.regs.rbx, 0);
        assert_eq!(st.regs.rflags, 0x2);
        assert_ne!(st.sregs.cr0 & (1 << 31), 0, "PG set");
        assert_ne!(st.sregs.cr0 & 1, 0, "PE set");
        assert_ne!(st.sregs.cr4 & (1 << 5), 0, "PAE set");
        assert_ne!(st.sregs.efer & (1 << 8), 0, "LME set");
        assert_ne!(st.sregs.efer & (1 << 10), 0, "LMA set");
        assert_eq!(st.sregs.cr3, 0x1000);
        assert_eq!(st.sregs.cs.selector, 0x10);
        assert_eq!(st.sregs.cs.l, 1);
        assert_eq!(st.sregs.cs.db, 0);
        assert_eq!(st.sregs.cs.type_ & 0x8, 0x8, "code segment");
        for seg in [
            st.sregs.ds,
            st.sregs.es,
            st.sregs.ss,
            st.sregs.fs,
            st.sregs.gs,
        ] {
            assert_eq!(seg.selector, 0x18);
            assert_eq!(seg.type_ & 0x8, 0, "data segment");
        }
        assert_eq!(st.sregs.gdt.base, 0x6000);
        assert_eq!(st.sregs.gdt.limit, 0x1F);
        assert_eq!(st.sregs.tr.present, 1);
        assert_eq!(st.sregs.ldt.unusable, 1);
    }
}
