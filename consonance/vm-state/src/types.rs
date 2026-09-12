// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

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
    pub present_dpl_s: u8,
    pub flags: u8,
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
    pub gdt_base: u64,
    pub gdt_limit: u16,
    pub idt_base: u64,
    pub idt_limit: u16,
    pub cr0: u64,
    pub cr2: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub cr8: u64,
    pub efer: u64,
    pub apic_base: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Xcrs {
    pub xcr0: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DebugRegs {
    pub db: [u64; 4],
    pub dr6: u64,
    pub dr7: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct VcpuEvents {
    pub exception_pending: bool,
    pub exception_vector: u8,
    pub exception_error_code: u32,
    pub nmi_pending: bool,
    pub smi_pending: bool,
    pub interrupt_shadow: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MpState {
    #[default]
    Runnable,
    Halted,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MsrBlock(pub BTreeMap<u32, u64>);

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct XsaveImage(pub Vec<u8>);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct VtimeState {
    pub guest_hz: u64,
    pub guest_base: u64,
    pub snapshot_vns: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TimerEntry {
    pub deadline_vns: u64,
    pub seq: u64,
    pub token: u64,
    pub period_vns: u64,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct TimerQueueState {
    pub entries: Vec<TimerEntry>,
    pub next_seq: u64,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DeviceBlob(pub Vec<u8>);
