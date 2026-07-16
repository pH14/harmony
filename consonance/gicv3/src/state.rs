// SPDX-License-Identifier: AGPL-3.0-or-later
//! Constants + the [`GicState`] snapshot record.

/// The architectural maximum ordinary INTID (SPIs end at 1019; `1020..1024`
/// are special INTIDs, not modeled).
pub const GIC_MAX_INTID: u32 = 1019;

/// The number of banked SGI+PPI INTIDs (`0..32`), owned by the redistributor's
/// SGI frame.
pub const SGI_PPI_COUNT: u32 = 32;

/// The number of `u32` bitmap words covering the architectural INTID space
/// (`32 × 32 = 1024 ≥ 1020`).
pub(crate) const BITMAP_WORDS: usize = 32;

/// The architectural priority-byte count (one per ordinary INTID).
pub(crate) const PRIORITY_BYTES: usize = 1020;

/// The distributor register frame size (64 KiB).
pub const GICD_FRAME_SIZE: u64 = 0x1_0000;

/// The redistributor frame pair size (RD frame + SGI frame, 64 KiB each).
pub const GICR_FRAME_SIZE: u64 = 0x2_0000;

/// `CNTV_CTL_EL0.ENABLE` — the virtual timer is enabled.
pub const CNTV_CTL_ENABLE: u64 = 1;

/// `CNTV_CTL_EL0.IMASK` — the virtual timer's interrupt output is masked.
pub const CNTV_CTL_IMASK: u64 = 1 << 1;

/// The [`GicState`] layout version. v1: the skeleton model (`tasks/112` M2) —
/// register files, PMR, and the one-shot-latched virtual timer.
pub const GIC_STATE_VERSION: u32 = 1;

/// The complete GICv3 model snapshot. Plain data, deterministic field order,
/// no map, no float (rule #4); the firing **deadline is derived, never
/// stored**, so absolute V-time deadlines survive restore by recomputation.
///
/// Every register file is architecturally sized (the full 1020-INTID space);
/// entries beyond the configured implementation limit must be zero —
/// [`Gicv3::restore`](crate::Gicv3::restore) validates exactly that, so a
/// snapshot can never smuggle state past the distributor bound.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GicState {
    /// The layout version ([`GIC_STATE_VERSION`]).
    pub version: u32,
    /// Configured SPI count (`GICD_TYPER.ITLinesNumber` input; a multiple of
    /// 32, `0..=960`).
    pub impl_spis: u32,
    /// The generic-timer counter frequency in Hz (the DTB's `CNTFRQ`; fixed by
    /// the composition root, like x86's `LAPIC_TIMER_HZ`).
    pub timer_hz: u64,
    /// The virtual timer's PPI INTID (`16..32`; conventionally 27).
    pub timer_intid: u32,
    /// `GICD_CTLR` (Group-1 forwarding enable in bit 1; ARE modeled always-on).
    pub gicd_ctlr: u32,
    /// Group membership, one bit per INTID (`1` = Group 1, the deliverable
    /// IRQ group; reset `0` = Group 0, not deliverable — the guest programs
    /// `IGROUPR` before expecting delivery, as Linux does).
    pub group: [u32; BITMAP_WORDS],
    /// Enable bits, one per INTID.
    pub enable: [u32; BITMAP_WORDS],
    /// Pending bits, one per INTID.
    pub pending: [u32; BITMAP_WORDS],
    /// Active bits, one per INTID.
    pub active: [u32; BITMAP_WORDS],
    /// Priority bytes, one per INTID (lower value = higher priority).
    pub priority: [u8; PRIORITY_BYTES],
    /// The CPU interface's priority mask (`ICC_PMR_EL1`; reset `0` masks
    /// everything — the guest raises it before expecting delivery).
    pub pmr: u8,
    /// `CNTV_CTL_EL0` (`ENABLE` | `IMASK`).
    pub cntv_ctl: u64,
    /// `CNTV_CVAL_EL0` — the absolute compare value in timer ticks.
    pub cntv_cval: u64,
    /// Whether the current arming has already latched its pending edge
    /// (cleared by reprogramming `CVAL`/`CTL`).
    pub timer_fired: bool,
}
