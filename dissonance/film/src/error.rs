// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`FilmError`] — the projector's fail-loud error, layering the sub-errors of
//! the pass.
//!
//! Film keeps the two result categories `docs/RESOLUTION.md` rules: a
//! guest-observable landing is a [`StopReason`](control_proto::StopReason) (data
//! — surfaced via [`ShortRun`](FilmError::ShortRun) when a run does not reach the
//! frame it was asked for), and a control failure is a
//! [`SessionError`](resolution::SessionError). A **billboard header mismatch is a
//! hard error** ([`Header`](FilmError::Header)), never a silently misaligned
//! frame (task 87 §projector).

use thiserror::Error;

use crate::billboard::HeaderError;
use environment::Moment;
use resolution::SessionError;

/// Why filming a clip failed. A dropped session is *recovered* (re-materialize at
/// the failed frame — see the projector), not surfaced here; every variant below
/// is a genuine, unrecoverable failure.
#[derive(Clone, PartialEq, Eq, Debug, Error)]
pub enum FilmError {
    /// A control-transport failure that filming could not recover (a non-drop
    /// [`SessionError`], or drop retries exhausted).
    #[error("session error while filming: {0}")]
    Session(#[from] SessionError),

    /// A billboard header failed to parse or verify — the hard error: the frame
    /// the guest stamped did not match the frame-clock `Moment`, or the buffer
    /// was corrupt. Never rendered as a misaligned frame.
    #[error("billboard header error at frame {frame} (moment {moment}): {source}")]
    Header {
        /// The frame the projector was filming.
        frame: u32,
        /// The `Moment` it was positioned at.
        moment: Moment,
        /// The underlying header error.
        #[source]
        source: HeaderError,
    },

    /// A `run` landed **before** the frame's `Moment` (the guest crashed or
    /// quiesced first), so the reproducer's recorded frame is unreachable. The
    /// landing `StopReason` is not swallowed — its `Moment` is reported here.
    #[error(
        "run for frame {frame} stopped at moment {landed} before the target {target} ({stop_kind})"
    )]
    ShortRun {
        /// The frame being filmed.
        frame: u32,
        /// The `Moment` the run actually landed at.
        landed: Moment,
        /// The frame's target `Moment`.
        target: Moment,
        /// A short label for the landing `StopReason` (crash / quiescent / …).
        stop_kind: &'static str,
    },

    /// A dropped session could not be recovered within the retry budget.
    #[error("session dropped filming frame {frame}; exhausted {retries} re-materialize retries")]
    SessionDropped {
        /// The frame that kept failing.
        frame: u32,
        /// The number of re-materialize attempts made.
        retries: u32,
    },

    /// The [`FilmPlan`](crate::FilmPlan) handed to [`film`](crate::film) was
    /// invalid. [`FilmPlan::derive`](crate::FilmPlan::derive) never produces such
    /// a plan, but a plan reached another way (deserialized, or built field-by-
    /// field) is re-validated at entry so an untrusted plan fails loudly instead
    /// of, e.g., hanging a zero-`read_cap` chunker (rule 4).
    #[error("invalid film plan: {reason}")]
    InvalidPlan {
        /// Why the plan was rejected.
        reason: &'static str,
    },
}
