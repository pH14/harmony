// SPDX-License-Identifier: AGPL-3.0-or-later
//! The single error type for every fallible decode/parse path: [`EnvError`].

use thiserror::Error;

/// A failure decoding catalog or reproducer bytes ([`Answer::decode`],
/// [`FaultPolicy::from_bytes`], [`EnvSpec::decode`]), constructing a
/// [`FaultPolicy`], or composing two reproducers ([`EnvCodec::compose`]).
///
/// Every decode is **strict and total**: arbitrary or mutated bytes can only
/// produce an `Err`, never a panic (conventions rule 4). Off-version input is
/// reported as [`EnvError::BadVersion`] so the backend can map it to a distinct
/// control-plane error; every other defect (bad magic, truncation, trailing
/// bytes, a non-canonical or out-of-range field, an impossible enum tag, a
/// zero probability denominator) is [`EnvError::Malformed`]. A composition
/// [`compose`](crate::EnvCodec::compose) cannot prove bit-identical is
/// [`EnvError::UnsupportedComposition`].
///
/// [`Answer::decode`]: crate::Answer::decode
/// [`FaultPolicy::from_bytes`]: crate::FaultPolicy::from_bytes
/// [`EnvSpec::decode`]: crate::EnvSpec::decode
/// [`FaultPolicy`]: crate::FaultPolicy
/// [`EnvCodec::compose`]: crate::EnvCodec::compose
#[derive(Clone, Copy, PartialEq, Eq, Debug, Error)]
pub enum EnvError {
    /// The blob's declared format version is one this build does not decode.
    /// Carries the offending version.
    #[error("unsupported blob version {0}")]
    BadVersion(u16),
    /// The bytes are not a valid, canonical encoding of the expected value.
    #[error("malformed environment blob")]
    Malformed,
    /// A [`compose`](crate::EnvCodec::compose) `Moment` re-key overflowed
    /// `u64::MAX` (a tail override `Moment` shifted by `at` past the axis). Rejected
    /// rather than saturated, so two distinct overrides can never collapse onto one
    /// key (collision-free replay).
    #[error("environment composition offset overflowed the Moment axis")]
    Overflow,
    /// A [`compose`](crate::EnvCodec::compose) was asked for a composition outside
    /// its task-45 scope (one-axis `Moment` override re-keying) and therefore
    /// **fails closed** rather than emit a wrong reproducer. The cases deferred to
    /// task 93 (the compose-model revisit): either input carries a
    /// [`StandingFault`] (its V-time window is a *different axis* than the `Moment`
    /// offset — no one-axis re-key is correct); either input is a pure
    /// [`Seeded`](crate::EnvSpec::Seeded) environment (every decision is
    /// seed-serviced, so splicing it would desync the fresh PRNG stream); or the
    /// `tail`'s seed or policy differs from the `base`'s (one `EnvSpec` cannot
    /// carry a piecewise-seeded stream). `compose` supports the override-only,
    /// same-seed/same-policy case at any `at`.
    ///
    /// [`StandingFault`]: crate::StandingFault
    #[error("unsupported environment composition (deferred to task 93)")]
    UnsupportedComposition,
}
