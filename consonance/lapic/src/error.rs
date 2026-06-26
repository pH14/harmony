// SPDX-License-Identifier: AGPL-3.0-or-later
//! The crate's single error type, [`LapicError`].

/// Errors returned by the [`Lapic`](crate::Lapic) state machine.
///
/// The set is deliberately small: per the CPU/MSR contract's `deny-ignore-write`
/// rule (`docs/CPU-MSR-CONTRACT.md` §5), a guest write to a read-only or
/// reserved-but-in-range register is *dropped*, not an error — so the only way an
/// MMIO access produces an error is a structurally malformed offset
/// ([`LapicError::BadOffset`]). The other two variants guard the [`raise`] and
/// [`restore`] entry points.
///
/// [`raise`]: crate::Lapic::raise
/// [`restore`]: crate::Lapic::restore
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum LapicError {
    /// The MMIO offset is not 16-byte aligned or lies outside `0x000..=0xFF0`.
    ///
    /// A *write* to a read-only or reserved register that is in range and aligned
    /// is **not** this error — it is silently dropped (deny-ignore-write). Only a
    /// malformed offset reaches here.
    #[error(
        "malformed APIC MMIO offset {0:#06x}: not 16-byte aligned or out of range 0x000..=0xFF0"
    )]
    BadOffset(u32),

    /// A vector below 16 was passed to [`raise`](crate::Lapic::raise); vectors
    /// `0..=15` are reserved by the architecture and can never be delivered.
    #[error("interrupt vector {0} is reserved (vectors 0..=15 are architecturally reserved)")]
    ReservedVector(u8),

    /// A [`LapicState`](crate::LapicState) failed a consistency check during
    /// [`restore`](crate::Lapic::restore): `timer_hz == 0`, an unsupported
    /// snapshot `version`, or an armed timer with a zero initial count.
    #[error("LAPIC snapshot failed an internal consistency check")]
    InvalidState,
}
