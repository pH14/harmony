// SPDX-License-Identifier: AGPL-3.0-or-later
//! The live, in-memory vCPU snapshot the backend produces (`save`) and consumes
//! (`restore`).
//!
//! `VcpuState` is the counterpart to task 09's *serialized* `vm_state` blob:
//! vmm-core marshals a `VcpuState` into a `vm_state::VmState` for the codec. Per
//! rule #2 this crate **does not depend on `vm-state`**; the field set
//! deliberately parallels task 09's records and is kept consistent by review.
//!
//! Determinism (rule #4): the MSR set is a [`BTreeMap`] (never a `HashMap`), so
//! equal guest state ⇒ equal `VcpuState`; no floating point; no host-derived
//! fields (`save` must never launder a host TSC or RNG draw in here). Every
//! field's KVM-ioctl provenance is documented inline.

use std::collections::BTreeMap;

use crate::types::MpState;

/// Full guest-visible vCPU state for snapshot/restore. The per-vCPU input to the
/// M2 state hash (`docs/BRINGUP.md` step 6).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct VcpuState {
    /// GPRs, `RIP`, `RFLAGS` (`KVM_GET_REGS`).
    pub regs: VcpuRegs,
    /// Segments, descriptor tables, control regs, `EFER`, `APIC_BASE`
    /// (`KVM_GET_SREGS2`).
    pub sregs: VcpuSregs,
    /// Live `XCR0` (`KVM_GET_XCRS`). The XSAVE *image* lives in [`Self::xsave`];
    /// `XCR0` itself is captured separately or restore diverges (R1).
    pub xcr0: u64,
    /// `DR0..3`, `DR6`, `DR7` (`KVM_GET_DEBUGREGS`).
    pub debugregs: DebugRegs,
    /// Pending exception/NMI/SMI and interrupt-shadow state
    /// (`KVM_GET_VCPU_EVENTS`).
    pub events: VcpuEvents,
    /// Runnable vs halted (`KVM_GET_MP_STATE`).
    pub mp_state: MpState,
    /// The contract's `allow-stateful` MSR set (`KVM_GET_MSRS` over
    /// `MsrFilter::allow_inkernel`). Sorted by index (rule #4): equal guest state
    /// ⇒ equal bytes.
    pub msrs: BTreeMap<u32, u64>,
    /// FPU/XSAVE state image (`KVM_GET_XSAVE2`). Length is host-XSAVE-area sized;
    /// restored verbatim.
    pub xsave: Vec<u8>,
}

/// General-purpose registers, `RIP`, and `RFLAGS` (`KVM_GET_REGS` /
/// `kvm_regs`). Flat little-endian POD.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct VcpuRegs {
    /// `RAX`.
    pub rax: u64,
    /// `RBX`.
    pub rbx: u64,
    /// `RCX`.
    pub rcx: u64,
    /// `RDX`.
    pub rdx: u64,
    /// `RSI`.
    pub rsi: u64,
    /// `RDI`.
    pub rdi: u64,
    /// `RSP`.
    pub rsp: u64,
    /// `RBP`.
    pub rbp: u64,
    /// `R8`.
    pub r8: u64,
    /// `R9`.
    pub r9: u64,
    /// `R10`.
    pub r10: u64,
    /// `R11`.
    pub r11: u64,
    /// `R12`.
    pub r12: u64,
    /// `R13`.
    pub r13: u64,
    /// `R14`.
    pub r14: u64,
    /// `R15`.
    pub r15: u64,
    /// Instruction pointer.
    pub rip: u64,
    /// Flags register.
    pub rflags: u64,
}

/// A segment register descriptor (`kvm_segment`). Flat POD.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Segment {
    /// Segment base address.
    pub base: u64,
    /// Segment limit.
    pub limit: u32,
    /// Selector.
    pub selector: u16,
    /// Segment type field.
    pub type_: u8,
    /// Present bit.
    pub present: u8,
    /// Descriptor privilege level.
    pub dpl: u8,
    /// Default/Big (`D/B`) bit.
    pub db: u8,
    /// Descriptor-type (`S`) bit (code/data vs system).
    pub s: u8,
    /// Long-mode (`L`) bit.
    pub l: u8,
    /// Granularity bit.
    pub g: u8,
    /// Available-for-system-use bit.
    pub avl: u8,
    /// Unusable bit (KVM-specific: segment is not loadable).
    pub unusable: u8,
}

/// A descriptor-table register (`GDTR`/`IDTR`, `kvm_dtable`). Flat POD.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DescriptorTable {
    /// Table base address.
    pub base: u64,
    /// Table limit (byte count - 1).
    pub limit: u16,
}

/// Segments, descriptor tables, control registers, `EFER`, and `APIC_BASE`
/// (`KVM_GET_SREGS2` / `kvm_sregs2`). Flat POD.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct VcpuSregs {
    /// `CS`.
    pub cs: Segment,
    /// `DS`.
    pub ds: Segment,
    /// `ES`.
    pub es: Segment,
    /// `FS`.
    pub fs: Segment,
    /// `GS`.
    pub gs: Segment,
    /// `SS`.
    pub ss: Segment,
    /// Task register.
    pub tr: Segment,
    /// Local descriptor table register.
    pub ldt: Segment,
    /// Global descriptor table register.
    pub gdt: DescriptorTable,
    /// Interrupt descriptor table register.
    pub idt: DescriptorTable,
    /// `CR0`.
    pub cr0: u64,
    /// `CR2`.
    pub cr2: u64,
    /// `CR3`.
    pub cr3: u64,
    /// `CR4`.
    pub cr4: u64,
    /// `CR8` (TPR).
    pub cr8: u64,
    /// `IA32_EFER`.
    pub efer: u64,
    /// `IA32_APIC_BASE`.
    pub apic_base: u64,
    /// `SREGS2` flags (e.g. `KVM_SREGS2_FLAGS_PDPTRS_VALID`).
    pub flags: u64,
    /// PAE page-directory-pointer-table entries (valid only when `flags`
    /// marks them so).
    pub pdptrs: [u64; 4],
}

/// Debug registers (`KVM_GET_DEBUGREGS` / `kvm_debugregs`). Flat POD.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DebugRegs {
    /// `DR0..DR3` (linear breakpoint addresses).
    pub db: [u64; 4],
    /// `DR6` (debug status).
    pub dr6: u64,
    /// `DR7` (debug control).
    pub dr7: u64,
    /// KVM `flags` field (currently always 0).
    pub flags: u64,
}

/// Pending-event and interrupt-shadow state (`KVM_GET_VCPU_EVENTS` /
/// `kvm_vcpu_events`). A representative subset is modeled; fields KVM may add are
/// left default on restore (documented in `IMPLEMENTATION.md`). Flat POD.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct VcpuEvents {
    /// A pending exception is injected.
    pub exception_injected: u8,
    /// The pending exception vector.
    pub exception_nr: u8,
    /// The pending exception carries an error code.
    pub exception_has_error_code: u8,
    /// An exception is **pending** (queued but not yet injected). Distinct from
    /// `exception_injected`; without it `restore(save())` drops a queued fault.
    pub exception_pending: u8,
    /// The pending exception error code.
    pub exception_error_code: u32,
    /// The pending exception carries a payload (`KVM_VCPUEVENT_VALID_PAYLOAD`):
    /// CR2 for `#PF`, DR6 for `#DB`. Saved/restored with `exception_payload`.
    pub exception_has_payload: u8,
    /// The exception payload value (CR2 / DR6 for the faulting exception).
    pub exception_payload: u64,
    /// A maskable interrupt is being injected.
    pub interrupt_injected: u8,
    /// The injected interrupt vector.
    pub interrupt_nr: u8,
    /// The injected interrupt is a software interrupt.
    pub interrupt_soft: u8,
    /// The interrupt shadow (STI / MOV-SS) is active.
    pub interrupt_shadow: u8,
    /// An NMI is being injected.
    pub nmi_injected: u8,
    /// An NMI is pending.
    pub nmi_pending: u8,
    /// NMIs are masked.
    pub nmi_masked: u8,
    /// Pending `SIPI` vector.
    pub sipi_vector: u32,
    /// `kvm_vcpu_events` flags field.
    pub flags: u32,
    /// In system-management mode.
    pub smi_smm: u8,
    /// An SMI is pending.
    pub smi_pending: u8,
    /// Inside an NMI within SMM.
    pub smi_inside_nmi: u8,
    /// A latched `INIT` is pending in SMM.
    pub smi_latched_init: u8,
    /// A triple fault is **pending** (`KVM_VCPUEVENT_VALID_TRIPLE_FAULT`). Without
    /// it a snapshot taken with a queued triple fault restores as if none occurred.
    pub triple_fault_pending: u8,
}

/// `XSTATE_BV` in the XSAVE header (Intel SDM vol. 1, XSAVE area layout).
const XSTATE_BV: usize = 512;
/// `XCOMP_BV` in the XSAVE header; nonzero selects the compacted format.
const XCOMP_BV: usize = 520;
/// x87 control/status/tag/opcode/instruction/operand words in the legacy area.
const X87_CONTROL: std::ops::Range<usize> = 0..24;
/// ST0–ST7 in the legacy area.
const X87_ST: std::ops::Range<usize> = 32..160;
/// MXCSR in the legacy area (SSE component; `MXCSR_MASK` at 28 is a host
/// capability constant, not guest state).
const SSE_MXCSR: std::ops::Range<usize> = 24..28;
/// XMM0–XMM15 in the legacy area.
const SSE_XMM: std::ops::Range<usize> = 160..416;
/// x87 init state: `FCW = 0x037F`, every other control word and ST register 0.
const X87_INIT_FCW: [u8; 2] = 0x037Fu16.to_le_bytes();
/// SSE init state: `MXCSR = 0x1F80`, every XMM register 0.
const SSE_INIT_MXCSR: [u8; 4] = 0x1F80u32.to_le_bytes();

/// Canonicalize the x87 and SSE components of a standard-format XSAVE image to
/// init-compressed form, in place.
///
/// XSAVE's init optimization gives one guest-visible state two encodings: a
/// component can be recorded present (`XSTATE_BV` bit set, area holding the
/// init values) or absent (bit clear, area architecturally ignored), and which
/// one hardware writes varies with host scheduling rather than guest behavior
/// (observed on Xeon Platinum 8573C: the x87 bit flips across same-seed boots
/// while every state byte matches). Determinism rule #4 forbids host-derived
/// bytes in `VcpuState`, so both encodings must collapse to one: a component
/// whose area holds the init values gets its bit cleared, and a component
/// whose bit is clear gets the init values written into its ignored area.
/// Compacted-format images (nonzero `XCOMP_BV`) have a different layout and
/// are left untouched.
pub fn canonicalize_xsave(image: &mut [u8]) {
    if image.len() < XCOMP_BV + 8 || image[XCOMP_BV..XCOMP_BV + 8] != [0u8; 8] {
        return;
    }

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

    /// A standard-format image holding the x87+SSE init values with the given
    /// `XSTATE_BV` — the shape KVM returns for an early-boot guest.
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
        // The measured Xeon 8573C flip: same bytes, XSTATE_BV 0x3 vs 0x2.
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
        // Bit clear ⇒ the area is architecturally ignored; residue there must
        // not reach the state hash.
        let mut image = init_image(0x0);
        image[X87_ST.start] = 0xEE;
        image[SSE_XMM.start + 7] = 0xEE;
        canonicalize_xsave(&mut image);
        assert_eq!(image, init_image(0x0));
    }

    #[test]
    fn mxcsr_mask_survives_canonicalization() {
        let mut image = init_image(0x2);
        canonicalize_xsave(&mut image);
        assert_eq!(image[28..32], 0xFFFFu32.to_le_bytes());
    }

    #[test]
    fn compacted_images_are_untouched() {
        let mut image = init_image(0x3);
        image[XCOMP_BV..XCOMP_BV + 8].copy_from_slice(&(1u64 << 63 | 0x3).to_le_bytes());
        let before = image.clone();
        canonicalize_xsave(&mut image);
        assert_eq!(image, before);
    }
}
