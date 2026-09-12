// SPDX-License-Identifier: AGPL-3.0-or-later

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum LapicError {
    #[error(
        "malformed APIC MMIO offset {0:#06x}: not 16-byte aligned or out of range 0x000..=0xFF0"
    )]
    BadOffset(u32),

    #[error("interrupt vector {0} is reserved (vectors 0..=15 are architecturally reserved)")]
    ReservedVector(u8),

    #[error("LAPIC snapshot failed an internal consistency check")]
    InvalidState,
}
