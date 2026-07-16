// SPDX-License-Identifier: AGPL-3.0-or-later
//! The single error type for the model: [`GicError`].

use thiserror::Error;

/// Every failure mode of the GICv3 model. Deliberately small: deny-ignore
/// means a read-only/unmodeled in-range register write is *dropped*, not an
/// error, so only structurally malformed inputs error (rule #4: total over
/// untrusted input, never a panic).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Error)]
pub enum GicError {
    /// A register access outside the frame, not 4-byte aligned, or not the
    /// modeled 32-bit width.
    #[error("malformed GIC MMIO access at offset {0:#07x}: out of frame, unaligned, or not 32-bit")]
    BadOffset(u64),
    /// An INTID outside the implemented, distributor-bounded identity space
    /// (SGI/PPI/SPI up to the configured limit; special INTIDs and
    /// extended-SPI/LPI spaces are not modeled).
    #[error("INTID {0} is outside the implemented GICv3 identity space")]
    BadIntId(u32),
    /// A [`GicConfig`](crate::GicConfig) or restored
    /// [`GicState`](crate::GicState) failed an internal consistency check.
    #[error("GICv3 config/snapshot failed an internal consistency check")]
    InvalidState,
}
