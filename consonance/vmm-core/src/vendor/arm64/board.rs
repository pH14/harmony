// SPDX-License-Identifier: AGPL-3.0-or-later
//! The arm64 machine memory map (`tasks/112` M3).
//!
//! One fixed board layout, chosen to match QEMU's `-M virt` (GICv3) so the M3
//! TCG smoke can boot the `Image`+DTB artifacts this vendor produces on QEMU's
//! own emulated machine. RAM sits high (`0x4000_0000`) and every device frame
//! sits **below** it, so — unlike x86's xAPIC page inside the RAM image — no
//! device address falls inside guest RAM and `Vendor::mmio_holes` stays empty
//! (the RAM memslot never needs splitting). The DTB describes exactly these
//! addresses; [`super::dispatch`] routes accesses to them.
//!
//! designed-not-frozen (AA-3 / AA-6): the exact addresses are a composition
//! choice, not a measured constant; the reserved pvclock GPA is the `hm-rk5`
//! seam (reserved here, its protocol implemented there).

/// A 4 KiB page.
pub(crate) const PAGE: u64 = 0x1000;

/// Guest RAM base (QEMU virt: 1 GiB). 2 MiB-aligned, as the arm64 boot
/// protocol requires for the `Image` load base.
pub const RAM_BASE: u64 = 0x4000_0000;

/// GICv3 distributor MMIO frame `(base, len)` — 64 KiB (`arm,gic-v3` reg[0]).
pub const GICD: (u64, u64) = (0x0800_0000, 0x0001_0000);

/// GICv3 redistributor MMIO frame `(base, len)` — one redistributor pair
/// (RD + SGI frame), 128 KiB (`arm,gic-v3` reg[1]).
pub const GICR: (u64, u64) = (0x080A_0000, 0x0002_0000);

/// PL011 UART MMIO frame `(base, len)` — 4 KiB (QEMU virt UART0).
pub const PL011: (u64, u64) = (0x0900_0000, 0x0000_1000);

/// The PL011's SPI interrupt line (QEMU virt UART0 = SPI 1 ⇒ GIC INTID 33).
pub const PL011_SPI: u32 = 1;

/// The reserved-MMIO GPA of the **hypercall doorbell** (`docs/ARCH-BOUNDARY.md`
/// §4: on arm64 the doorbell is a reserved-MMIO store surfacing as
/// `KVM_EXIT_MMIO`, not a port `OUT`). A one-page hole below RAM, recognized
/// by [`super::dispatch`]'s MMIO routing and handled as the doorbell.
pub const DOORBELL: (u64, u64) = (0x0A00_0000, 0x0000_1000);

/// The generic timer's virtual-timer PPI INTID (PPI 11 ⇒ GIC INTID 27) — the
/// `arm,armv8-timer` virtual-timer interrupt, the fabric's `timer_intid`.
pub const VIRT_TIMER_INTID: u32 = 27;

/// The implemented SPI count the distributor advertises (a multiple of 32; the
/// PL011's SPI 1 is well inside it). Governs `GICD_TYPER.ITLinesNumber`.
pub const IMPL_SPIS: u32 = 64;

/// The generic-timer input frequency in Hz the DTB fixes (`CNTFRQ`). A
/// documented composition constant, like x86's `LAPIC_TIMER_HZ` — **not** a
/// measured quantity (the V-time↔tick mapping is the clock page's, `hm-rk5`).
pub const CNTFRQ_HZ: u64 = 62_500_000;

/// Round `x` up to the next multiple of `align` (a power of two). Saturating,
/// so a pathological input can never wrap (rule #4).
pub(crate) const fn align_up(x: u64, align: u64) -> u64 {
    let mask = align - 1;
    x.saturating_add(mask) & !mask
}

/// The board's canonical GICv3 configuration — the one source of truth for the
/// distributor bound, the timer frequency, and the virtual-timer INTID, so a
/// wired fabric and the DTB never disagree. (Infallible for these fixed board
/// constants — [`IMPL_SPIS`] is a multiple of 32 in range, [`CNTFRQ_HZ`] is
/// non-zero, and [`VIRT_TIMER_INTID`] is a PPI — so `expect` here is
/// statically justified.)
pub fn gic_config() -> gicv3::GicConfig {
    gicv3::GicConfig {
        impl_spis: IMPL_SPIS,
        timer_hz: CNTFRQ_HZ,
        timer_intid: VIRT_TIMER_INTID,
    }
}

/// A fresh, reset GICv3 for the board (the fabric a test/mock composition wires
/// via [`Vmm::wire_gic`](crate::vmm::Vmm)). Infallible per [`gic_config`].
pub fn new_gic() -> gicv3::Gicv3 {
    gicv3::Gicv3::new(gic_config()).expect("board GIC config is statically valid")
}
