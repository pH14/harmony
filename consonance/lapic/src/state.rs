// SPDX-License-Identifier: AGPL-3.0-or-later

pub const APIC_BASE_DEFAULT: u64 = 0xFEE0_0000;

pub const APIC_MMIO_SIZE: usize = 0x1000;

pub const APIC_VERSION_VALUE: u32 = 0x0005_0014;

pub const LAPIC_STATE_VERSION: u32 = 3;

pub const APIC_ID: u32 = 0x020;
pub const APIC_VERSION: u32 = 0x030;
pub const APIC_TPR: u32 = 0x080;
pub const APIC_PPR: u32 = 0x0A0;
pub const APIC_EOI: u32 = 0x0B0;
pub const APIC_LDR: u32 = 0x0D0;
pub const APIC_DFR: u32 = 0x0E0;
pub const APIC_SVR: u32 = 0x0F0;
pub const APIC_ISR: u32 = 0x100;
pub const APIC_TMR: u32 = 0x180;
pub const APIC_IRR: u32 = 0x200;
pub const APIC_ESR: u32 = 0x280;
pub const APIC_ICR_LOW: u32 = 0x300;
pub const APIC_ICR_HIGH: u32 = 0x310;
pub const APIC_LVT_TIMER: u32 = 0x320;
pub const APIC_LVT_THERMAL: u32 = 0x330;
pub const APIC_LVT_PERFMON: u32 = 0x340;
pub const APIC_LVT_LINT0: u32 = 0x350;
pub const APIC_LVT_LINT1: u32 = 0x360;
pub const APIC_LVT_ERROR: u32 = 0x370;
pub const APIC_TMICT: u32 = 0x380;
pub const APIC_TMCCT: u32 = 0x390;
pub const APIC_TDCR: u32 = 0x3E0;

pub const APIC_MAX_OFFSET: u32 = 0xFF0;

#[derive(Clone, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LapicState {
    pub version: u32,
    pub id: u32,
    pub timer_hz: u64,
    pub tpr: u32,
    pub svr: u32,
    pub ldr: u32,
    pub dfr: u32,
    pub esr: u32,
    pub icr_low: u32,
    pub icr_high: u32,
    pub divide_config: u32,
    pub isr: [u32; 8],
    pub tmr: [u32; 8],
    pub irr: [u32; 8],
    pub lvt: [u32; 6],
    pub initial_count: u32,
    pub count_at_arm: u32,
    pub timer_arm_vns: u64,
    pub timer_running: bool,
    pub timer_pending: bool,
}
