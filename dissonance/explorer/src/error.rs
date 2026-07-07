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
    /// The backend **rejected a well-formed proposal as inadmissible** — it
    /// staged/decoded cleanly but names a fault the backend refuses to apply: an
    /// out-of-range `CorruptMemory` gpa, a `Moment` behind the restore point or
    /// already carrying a fault, or an out-of-scope fault this backend does not
    /// service. This is a **recoverable** rejection, *categorically distinct from
    /// a [`Transport`](Self::Transport) failure*: the machine is intact and the
    /// rejection is side-effect-free (task-59 stage-time validation), so a driver
    /// that proposes envs (the explorer, the benchmark campaign) should **discard
    /// this proposal and continue** rather than abort — exactly as a fuzzer drops
    /// an inadmissible mutant. It must NEVER be conflated with `Transport`, or a
    /// caller that skips it would mask a real backend death or a determinism
    /// divergence. Carries the wire reason for diagnostics. (task-69 M2)
    #[error("backend rejected an inadmissible proposal: {0}")]
    Inadmissible(String),
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
    /// The engine refused to seal/materialize at a `Moment` the injected
    /// task-63 `sealable` predicate rejects (the GO grid-restricted /
    /// RESTRICTED seam, task 68): such an exemplar should never have been
    /// admitted — the Archive keys on the same predicate. Carries the
    /// offending moment.
    #[error("moment {0} is not sealable under the task-63 predicate")]
    NotSealable(u64),
    /// A materialization replay stopped at a different `Moment` than the
    /// exemplar's keyed `at`. Under the task-63 GO (grid-restricted) ruling,
    /// `at` is a synchronized boundary of the exemplar's own recorded
    /// trajectory, so an identical replay must stop exactly there — anything
    /// else is a determinism/keying violation to escalate, never to seal.
    #[error("materialization of exemplar {exemplar} landed at {landed}, not its keyed moment {at}")]
    MaterializeDivergence {
        /// The raw [`ExemplarRef`](crate::ExemplarRef) being materialized.
        exemplar: u64,
        /// The exemplar's keyed moment.
        at: u64,
        /// The moment the replay actually stopped at.
        landed: u64,
    },
}
