// SPDX-License-Identifier: AGPL-3.0-or-later
//! The crate's single error type, [`PitError`].

/// Errors returned by the [`Pit`](crate::Pit) state machine.
///
/// The set is deliberately small. Per the i8254's architecture and the CPU/MSR
/// contract's `emulate-device` disposition, a guest access to an unimplemented or
/// out-of-block port is not this device's concern (the `vmm-core` legacy-platform
/// fallback handles those), and a write to a write-only register (`0x43` command)
/// or a read of it is silently handled, not an error. So the only ways an access
/// produces an error are a structurally wrong port ([`PitError::BadPort`]) and the
/// two guards on [`Pit::new`](crate::Pit::new) / [`Pit::restore`](crate::Pit::restore).
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum PitError {
    /// The port is not one of the four the PIT owns (`0x40`–`0x43`).
    ///
    /// `vmm-core` only ever routes the owned ports here, so this guards misuse; it
    /// is never produced on the real boot path.
    #[error("port {0:#06x} is not a PIT register (the i8254 owns 0x40..=0x43)")]
    BadPort(u16),

    /// A [`PitConfig`](crate::PitConfig) had `freq_hz == 0`; the countdown
    /// arithmetic divides by it.
    #[error("PIT input frequency must be non-zero")]
    ZeroFrequency,

    /// A [`PitState`](crate::PitState) failed a consistency check during
    /// [`restore`](crate::Pit::restore): an unsupported snapshot `version`,
    /// `freq_hz == 0`, or a counter whose access/mode fields are not bit-reachable
    /// through the programming path.
    #[error("PIT snapshot failed an internal consistency check")]
    InvalidState,
}
