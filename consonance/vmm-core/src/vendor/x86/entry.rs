// SPDX-License-Identifier: AGPL-3.0-or-later
//! Architectural long-mode entry state for the controlled Linux guest.

use vmm_backend::{MpState, Segment, VcpuRegs, VcpuSregs, VcpuState};

/// `CR0.PE` (protected-mode enable, bit 0). Written `1` (not `1 << 0`, whose shift
/// carries an equivalent `>>` mutant).
const CR0_PE: u64 = 1;
/// `CR0.NE` (numeric-error, bit 5) — required to be 1 by VMX `CR0` fixed bits.
const CR0_NE: u64 = 1 << 5;

/// Segment limit for a flat 4 GiB segment. KVM loads `kvm_segment.limit` straight
/// into the VMCS, and the box treats it as the **already-expanded byte limit**, so
/// it must be `0xFFFF_FFFF` (not the raw 20-bit `0xFFFFF`, which would cap the
/// segment at 1 MiB and fault the very first instruction fetch at `EIP ≈ 0x100043`).
/// Granularity stays `G=1`: VMX entry requires `G=1` whenever limit bits 31:20 are
/// set, and both interpretations (raw vs. `(limit<<12)|0xFFF`) yield 4 GiB here.
const FLAT_LIMIT: u32 = 0xFFFF_FFFF;

/// A usable 32-bit busy-TSS `TR` (VMX requires `TR` usable on entry). Real KVM
/// keeps its own reset `TR`; this is the self-consistent value for the mock path.
fn task_register() -> Segment {
    Segment {
        base: 0,
        limit: 0xFFFF,
        selector: 0,
        type_: 0xB, // 32-bit busy TSS
        present: 1,
        dpl: 0,
        db: 0,
        s: 0, // system
        l: 0,
        g: 0,
        avl: 0,
        unusable: 0,
    }
}

/// `CR0.PG` (paging enable, bit 31) — required for long mode.
const CR0_PG: u64 = 1 << 31;
/// `CR4.PAE` (physical-address-extension, bit 5) — required for long mode.
const CR4_PAE: u64 = 1 << 5;
/// `IA32_EFER.LME` (long-mode-enable, bit 8).
const EFER_LME: u64 = 1 << 8;
/// `IA32_EFER.LMA` (long-mode-active, bit 10) — set by the CPU when paging turns
/// on with `LME`; written here so the restored state is self-consistent.
const EFER_LMA: u64 = 1 << 10;

/// `__BOOT_CS` selector (`0x10`) the 64-bit boot protocol mandates.
const BOOT_CS_SELECTOR: u16 = 0x10;
/// `__BOOT_DS` selector (`0x18`) the 64-bit boot protocol mandates.
const BOOT_DS_SELECTOR: u16 = 0x18;

/// A flat 64-bit code segment for `__BOOT_CS`: base 0, 4 GiB, exec/read, DPL 0,
/// `L=1` (64-bit), `D=0`, `G=1`.
fn long_code_segment() -> Segment {
    Segment {
        base: 0,
        limit: FLAT_LIMIT,
        selector: BOOT_CS_SELECTOR,
        type_: 0xB, // code, execute/read, accessed
        present: 1,
        dpl: 0,
        db: 0, // must be 0 when L=1
        s: 1,  // code/data
        l: 1,  // 64-bit
        g: 1,
        avl: 0,
        unusable: 0,
    }
}

/// A flat data segment for `__BOOT_DS`: base 0, 4 GiB, read/write, DPL 0, `D/B=1`,
/// `G=1`. (Data-segment fields are ignored in 64-bit mode but must be a usable
/// descriptor for VMX entry.)
fn long_data_segment() -> Segment {
    Segment {
        base: 0,
        limit: FLAT_LIMIT,
        selector: BOOT_DS_SELECTOR,
        type_: 0x3, // data, read/write, accessed
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

/// Build the architectural entry state for the **direct 64-bit Linux boot** as a
/// [`vmm_backend::VcpuState`]: long mode active (`CR0.PG|PE`, `CR4.PAE`,
/// `EFER.LME|LMA`), `CR3 = page_table_root` (the loader's identity map), flat
/// `__BOOT_CS`/`__BOOT_DS`, `GDTR.base = gdt_gpa`, `RIP = entry_rip` (the kernel's
/// 64-bit entry = `load_addr + 0x200`), `RSI = boot_params_gpa` (the zero page),
/// `RFLAGS = 0x2` (IF cleared), all other GPRs `0`.
///
/// On real KVM the Linux composition root
/// overlays these fields onto a live `save()` template (keeping KVM's valid
/// `TR`/`LDT`/XSAVE/MSR shape), because [`vmm_backend::Backend::restore`] validates
/// the XSAVE size and MSR key-set a pure builder cannot know.
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
        // The GDT lives in guest RAM at `gdt_gpa`; 4 entries × 8 bytes − 1.
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
        rflags: 0x0000_0002, // reserved bit 1 set, IF=0
        ..VcpuRegs::default()
    };

    VcpuState {
        regs,
        sregs,
        xcr0: 1, // x87 enabled (XCR0[0] must be 1 for KVM_SET_XCRS)
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
        xsave_restore_bv: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_mode_entry_matches_64bit_boot_protocol() {
        let st = long_mode_entry(0x10_0200, 0x7000, 0x1000, 0x6000);
        // RIP = 64-bit entry, RSI = boot_params, other GPRs zero.
        assert_eq!(st.regs.rip, 0x10_0200);
        assert_eq!(st.regs.rsi, 0x7000);
        assert_eq!(st.regs.rax, 0);
        assert_eq!(st.regs.rbx, 0);
        // IF cleared.
        assert_eq!(st.regs.rflags, 0x2);
        // Long mode: CR0.PG|PE, CR4.PAE, EFER.LME|LMA, CR3 = page table root.
        assert_ne!(st.sregs.cr0 & (1 << 31), 0, "PG set");
        assert_ne!(st.sregs.cr0 & 1, 0, "PE set");
        assert_ne!(st.sregs.cr4 & (1 << 5), 0, "PAE set");
        assert_ne!(st.sregs.efer & (1 << 8), 0, "LME set");
        assert_ne!(st.sregs.efer & (1 << 10), 0, "LMA set");
        assert_eq!(st.sregs.cr3, 0x1000);
        // __BOOT_CS: selector 0x10, 64-bit (L=1, D=0).
        assert_eq!(st.sregs.cs.selector, 0x10);
        assert_eq!(st.sregs.cs.l, 1);
        assert_eq!(st.sregs.cs.db, 0);
        assert_eq!(st.sregs.cs.type_ & 0x8, 0x8, "code segment");
        // __BOOT_DS: selector 0x18 on every data segment.
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
        // GDTR points at the loader's boot GDT.
        assert_eq!(st.sregs.gdt.base, 0x6000);
        assert_eq!(st.sregs.gdt.limit, 0x1F);
        // TR usable (VMX entry requirement); LDT unusable.
        assert_eq!(st.sregs.tr.present, 1);
        assert_eq!(st.sregs.ldt.unusable, 1);
    }
}
