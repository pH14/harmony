// SPDX-License-Identifier: AGPL-3.0-or-later

pub const GIC_MAX_INTID: u32 = 1019;

pub const SGI_PPI_COUNT: u32 = 32;

pub(crate) const BITMAP_WORDS: usize = 32;

pub(crate) const PRIORITY_BYTES: usize = 1020;

pub const GICD_FRAME_SIZE: u64 = 0x1_0000;

pub const GICR_FRAME_SIZE: u64 = 0x2_0000;

pub const CNTV_CTL_ENABLE: u64 = 1;

pub const CNTV_CTL_IMASK: u64 = 1 << 1;

pub const GIC_STATE_VERSION: u32 = 3;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GicState {
    pub version: u32,
    pub impl_spis: u32,
    pub timer_hz: u64,
    pub timer_intid: u32,
    pub gicd_ctlr: u32,
    pub group: [u32; BITMAP_WORDS],
    pub enable: [u32; BITMAP_WORDS],
    pub pending: [u32; BITMAP_WORDS],
    pub active: [u32; BITMAP_WORDS],
    pub line_level: [u32; BITMAP_WORDS],
    pub priority: [u8; PRIORITY_BYTES],
    pub pmr: u8,
    pub igrpen1: bool,
    pub cntv_ctl: u64,
    pub cntv_cval: u64,
    pub timer_fired: bool,
}
