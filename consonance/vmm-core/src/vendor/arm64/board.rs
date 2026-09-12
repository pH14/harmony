// SPDX-License-Identifier: AGPL-3.0-or-later

pub(crate) const PAGE: u64 = 0x1000;

pub const RAM_BASE: u64 = 0x4000_0000;

pub const GICD: (u64, u64) = (0x0800_0000, 0x0001_0000);

pub const GICR: (u64, u64) = (0x080A_0000, 0x0002_0000);

pub const PL011: (u64, u64) = (0x0900_0000, 0x0000_1000);

pub const PL011_SPI: u32 = 1;

pub const DOORBELL: (u64, u64) = (0x0A00_0000, 0x0000_1000);

pub const PVCLOCK: (u64, u64) = (0x0B00_0000, 0x0000_1000);

pub const VIRT_TIMER_INTID: u32 = 27;

pub const PVCLOCK_PPI: u32 = VIRT_TIMER_INTID;

pub const IMPL_SPIS: u32 = 64;

pub const CNTFRQ_HZ: u64 = 62_500_000;

pub(crate) const fn align_up(x: u64, align: u64) -> u64 {
    let mask = align - 1;
    x.saturating_add(mask) & !mask
}

pub fn gic_config() -> gicv3::GicConfig {
    gicv3::GicConfig {
        impl_spis: IMPL_SPIS,
        timer_hz: CNTFRQ_HZ,
        timer_intid: VIRT_TIMER_INTID,
    }
}

pub fn new_gic() -> gicv3::Gicv3 {
    gicv3::Gicv3::new(gic_config()).expect("board GIC config is statically valid")
}
