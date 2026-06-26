// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fixed-layout POD wire records.
//!
//! Each is a `#[repr(C)]` struct of `zerocopy` little-endian fields. Because the
//! `little_endian::U16/U32/U64` types have alignment 1, every record is
//! alignment-1 with **no padding**, so the `IntoBytes` derive accepts it and the
//! encoded bytes are fully deterministic with no reserved/pad bytes to differ.
//! These private types carry the byte layout; the public structs in
//! [`crate::types`] carry the contract. Conversions between them are total.

use zerocopy::little_endian::{U16, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::types::{DebugRegs, Segment, VcpuEvents, VcpuRegs, VcpuSregs, VtimeState, Xcrs};

/// The 8-byte container header: magic, version, section count.
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(crate) struct HeaderWire {
    pub magic: U32,
    pub version: U16,
    pub section_count: U16,
}

/// `KVM_GET_REGS` — 18 little-endian `u64`s in `kvm_regs` field order.
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(crate) struct RegsWire {
    rax: U64,
    rbx: U64,
    rcx: U64,
    rdx: U64,
    rsi: U64,
    rdi: U64,
    rsp: U64,
    rbp: U64,
    r8: U64,
    r9: U64,
    r10: U64,
    r11: U64,
    r12: U64,
    r13: U64,
    r14: U64,
    r15: U64,
    rip: U64,
    rflags: U64,
}

impl From<&VcpuRegs> for RegsWire {
    fn from(r: &VcpuRegs) -> Self {
        Self {
            rax: r.rax.into(),
            rbx: r.rbx.into(),
            rcx: r.rcx.into(),
            rdx: r.rdx.into(),
            rsi: r.rsi.into(),
            rdi: r.rdi.into(),
            rsp: r.rsp.into(),
            rbp: r.rbp.into(),
            r8: r.r8.into(),
            r9: r.r9.into(),
            r10: r.r10.into(),
            r11: r.r11.into(),
            r12: r.r12.into(),
            r13: r.r13.into(),
            r14: r.r14.into(),
            r15: r.r15.into(),
            rip: r.rip.into(),
            rflags: r.rflags.into(),
        }
    }
}

impl From<&RegsWire> for VcpuRegs {
    fn from(w: &RegsWire) -> Self {
        Self {
            rax: w.rax.get(),
            rbx: w.rbx.get(),
            rcx: w.rcx.get(),
            rdx: w.rdx.get(),
            rsi: w.rsi.get(),
            rdi: w.rdi.get(),
            rsp: w.rsp.get(),
            rbp: w.rbp.get(),
            r8: w.r8.get(),
            r9: w.r9.get(),
            r10: w.r10.get(),
            r11: w.r11.get(),
            r12: w.r12.get(),
            r13: w.r13.get(),
            r14: w.r14.get(),
            r15: w.r15.get(),
            rip: w.rip.get(),
            rflags: w.rflags.get(),
        }
    }
}

/// One segment-descriptor cache entry (17 bytes, no padding).
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(crate) struct SegmentWire {
    base: U64,
    limit: U32,
    selector: U16,
    type_: u8,
    present_dpl_s: u8,
    flags: u8,
}

impl From<&Segment> for SegmentWire {
    fn from(s: &Segment) -> Self {
        Self {
            base: s.base.into(),
            limit: s.limit.into(),
            selector: s.selector.into(),
            type_: s.type_,
            present_dpl_s: s.present_dpl_s,
            flags: s.flags,
        }
    }
}

impl From<&SegmentWire> for Segment {
    fn from(w: &SegmentWire) -> Self {
        Self {
            base: w.base.get(),
            limit: w.limit.get(),
            selector: w.selector.get(),
            type_: w.type_,
            present_dpl_s: w.present_dpl_s,
            flags: w.flags,
        }
    }
}

/// `KVM_GET_SREGS2` — eight segments then the system/control registers.
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(crate) struct SregsWire {
    cs: SegmentWire,
    ds: SegmentWire,
    es: SegmentWire,
    fs: SegmentWire,
    gs: SegmentWire,
    ss: SegmentWire,
    tr: SegmentWire,
    ldt: SegmentWire,
    gdt_base: U64,
    gdt_limit: U16,
    idt_base: U64,
    idt_limit: U16,
    cr0: U64,
    cr2: U64,
    cr3: U64,
    cr4: U64,
    cr8: U64,
    efer: U64,
    apic_base: U64,
}

impl From<&VcpuSregs> for SregsWire {
    fn from(s: &VcpuSregs) -> Self {
        Self {
            cs: (&s.cs).into(),
            ds: (&s.ds).into(),
            es: (&s.es).into(),
            fs: (&s.fs).into(),
            gs: (&s.gs).into(),
            ss: (&s.ss).into(),
            tr: (&s.tr).into(),
            ldt: (&s.ldt).into(),
            gdt_base: s.gdt_base.into(),
            gdt_limit: s.gdt_limit.into(),
            idt_base: s.idt_base.into(),
            idt_limit: s.idt_limit.into(),
            cr0: s.cr0.into(),
            cr2: s.cr2.into(),
            cr3: s.cr3.into(),
            cr4: s.cr4.into(),
            cr8: s.cr8.into(),
            efer: s.efer.into(),
            apic_base: s.apic_base.into(),
        }
    }
}

impl From<&SregsWire> for VcpuSregs {
    fn from(w: &SregsWire) -> Self {
        Self {
            cs: (&w.cs).into(),
            ds: (&w.ds).into(),
            es: (&w.es).into(),
            fs: (&w.fs).into(),
            gs: (&w.gs).into(),
            ss: (&w.ss).into(),
            tr: (&w.tr).into(),
            ldt: (&w.ldt).into(),
            gdt_base: w.gdt_base.get(),
            gdt_limit: w.gdt_limit.get(),
            idt_base: w.idt_base.get(),
            idt_limit: w.idt_limit.get(),
            cr0: w.cr0.get(),
            cr2: w.cr2.get(),
            cr3: w.cr3.get(),
            cr4: w.cr4.get(),
            cr8: w.cr8.get(),
            efer: w.efer.get(),
            apic_base: w.apic_base.get(),
        }
    }
}

/// `KVM_GET_XCRS` — the single captured `XCR0`.
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(crate) struct XcrsWire {
    xcr0: U64,
}

impl From<&Xcrs> for XcrsWire {
    fn from(x: &Xcrs) -> Self {
        Self {
            xcr0: x.xcr0.into(),
        }
    }
}

impl From<&XcrsWire> for Xcrs {
    fn from(w: &XcrsWire) -> Self {
        Self { xcr0: w.xcr0.get() }
    }
}

/// `KVM_GET_DEBUGREGS` — `DR0..DR3`, `DR6`, `DR7`.
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(crate) struct DebugRegsWire {
    db0: U64,
    db1: U64,
    db2: U64,
    db3: U64,
    dr6: U64,
    dr7: U64,
}

impl From<&DebugRegs> for DebugRegsWire {
    fn from(d: &DebugRegs) -> Self {
        Self {
            db0: d.db[0].into(),
            db1: d.db[1].into(),
            db2: d.db[2].into(),
            db3: d.db[3].into(),
            dr6: d.dr6.into(),
            dr7: d.dr7.into(),
        }
    }
}

impl From<&DebugRegsWire> for DebugRegs {
    fn from(w: &DebugRegsWire) -> Self {
        Self {
            db: [w.db0.get(), w.db1.get(), w.db2.get(), w.db3.get()],
            dr6: w.dr6.get(),
            dr7: w.dr7.get(),
        }
    }
}

/// `KVM_GET_VCPU_EVENTS`. Booleans are encoded as `u8` (0/1) and validated on
/// decode — `bool` is not `FromBytes` because not every byte pattern is valid.
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(crate) struct EventsWire {
    exception_pending: u8,
    exception_vector: u8,
    exception_error_code: U32,
    nmi_pending: u8,
    smi_pending: u8,
    interrupt_shadow: u8,
}

impl From<&VcpuEvents> for EventsWire {
    fn from(e: &VcpuEvents) -> Self {
        Self {
            exception_pending: u8::from(e.exception_pending),
            exception_vector: e.exception_vector,
            exception_error_code: e.exception_error_code.into(),
            nmi_pending: u8::from(e.nmi_pending),
            smi_pending: u8::from(e.smi_pending),
            interrupt_shadow: e.interrupt_shadow,
        }
    }
}

impl EventsWire {
    /// Convert to the public type, validating the boolean bytes.
    pub(crate) fn to_events(&self) -> Option<VcpuEvents> {
        Some(VcpuEvents {
            exception_pending: byte_to_bool(self.exception_pending)?,
            exception_vector: self.exception_vector,
            exception_error_code: self.exception_error_code.get(),
            nmi_pending: byte_to_bool(self.nmi_pending)?,
            smi_pending: byte_to_bool(self.smi_pending)?,
            interrupt_shadow: self.interrupt_shadow,
        })
    }
}

fn byte_to_bool(b: u8) -> Option<bool> {
    match b {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// The V-time block — `vtime::VClockConfig` plus `snapshot_vns`.
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(crate) struct VtimeWire {
    ratio_num: U64,
    ratio_den: U64,
    tsc_hz: U64,
    tsc_base: U64,
    snapshot_vns: U64,
}

impl From<&VtimeState> for VtimeWire {
    fn from(v: &VtimeState) -> Self {
        Self {
            ratio_num: v.ratio_num.into(),
            ratio_den: v.ratio_den.into(),
            tsc_hz: v.tsc_hz.into(),
            tsc_base: v.tsc_base.into(),
            snapshot_vns: v.snapshot_vns.into(),
        }
    }
}

impl From<&VtimeWire> for VtimeState {
    fn from(w: &VtimeWire) -> Self {
        Self {
            ratio_num: w.ratio_num.get(),
            ratio_den: w.ratio_den.get(),
            tsc_hz: w.tsc_hz.get(),
            tsc_base: w.tsc_base.get(),
            snapshot_vns: w.snapshot_vns.get(),
        }
    }
}

/// One MSR `(index, value)` pair (12 bytes).
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(crate) struct MsrPairWire {
    pub index: U32,
    pub value: U64,
}

/// One timer-queue entry (32 bytes).
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(crate) struct TimerEntryWire {
    pub deadline_vns: U64,
    pub seq: U64,
    pub token: U64,
    pub period_vns: U64,
}
