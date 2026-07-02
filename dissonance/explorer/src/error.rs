// SPDX-License-Identifier: AGPL-3.0-or-later
//! The single error type for a driver-seam failure: [`MachineError`].

use thiserror::Error;

/// A **backend/transport** failure surfaced from the driver seam — the second
/// of dissonance's two result categories (`docs/DISSONANCE.md`). A VM or
/// transport failure is a `MachineError`; a guest-observable outcome is a
/// [`StopReason`](crate::StopReason). The two are never confused: a
/// `MachineError` aborts the Progression step **loudly** and is never recorded as
/// a [`Bug`](crate::Bug) (only [`StopReason::Crash`](crate::StopReason::Crash)
/// and [`StopReason::Assertion`](crate::StopReason::Assertion) are).
///
/// In production these map from the R2 socket adapter's `ControlError`; the toy
/// machine raises them for injected backend faults and for protocol misuse (a
/// dropped/unknown [`SnapId`](crate::SnapId), a non-quiescent snapshot).
#[derive(Clone, PartialEq, Eq, Debug, Error)]
pub enum MachineError {
    /// The transport/backend failed (socket error, backend crash, injected
    /// fault). Carries an opaque description.
    #[error("machine transport/backend failure: {0}")]
    Transport(String),
    /// A snapshot was requested at a non-quiescent point (snapshots are
    /// quiescent-only). [`Explorer::new`](crate::Explorer::new) returns this if
    /// the initial genesis snapshot cannot be taken.
    #[error("snapshot requested at a non-quiescent point")]
    NotQuiescent,
    /// A [`SnapId`](crate::SnapId) was used that the backend does not know —
    /// never minted, or already dropped (a corpus-GC-after-use bug). Carries the
    /// offending raw handle.
    #[error("unknown or dropped snapshot handle {0}")]
    UnknownSnapshot(u64),
    /// The backend rejected an [`Environment`](crate::Environment) blob it could
    /// not parse (bad version or malformed). Carries the declared blob version.
    #[error("backend rejected environment blob (version {0})")]
    BadEnvironment(u16),
    /// A [`Selector`](crate::Selector) chose an
    /// [`ExemplarRef`](crate::ExemplarRef) the frontier does not hold — engine
    /// misuse by the policy, surfaced loudly rather than papered over. Carries
    /// the offending entry index.
    #[error("selector chose an unknown frontier exemplar {0}")]
    UnknownExemplar(u64),
}
