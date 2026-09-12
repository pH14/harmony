// SPDX-License-Identifier: AGPL-3.0-or-later

use thiserror::Error;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Error)]
pub enum GicError {
    #[error("malformed GIC MMIO access at offset {0:#07x}: out of frame, unaligned, or not 32-bit")]
    BadOffset(u64),
    #[error("INTID {0} is outside the implemented GICv3 identity space")]
    BadIntId(u32),
    #[error("GICv3 config/snapshot failed an internal consistency check")]
    InvalidState,
}
