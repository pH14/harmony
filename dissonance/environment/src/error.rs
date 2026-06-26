// SPDX-License-Identifier: AGPL-3.0-or-later
//! The single error type for every fallible decode/parse path: [`EnvError`].

use thiserror::Error;

/// A failure decoding catalog or reproducer bytes ([`Answer::decode`],
/// [`FaultPolicy::from_bytes`], [`EnvSpec::decode`]) or constructing a
/// [`FaultPolicy`].
///
/// Every decode is **strict and total**: arbitrary or mutated bytes can only
/// produce an `Err`, never a panic (conventions rule 4). Off-version input is
/// reported as [`EnvError::BadVersion`] so the backend can map it to a distinct
/// control-plane error; every other defect (bad magic, truncation, trailing
/// bytes, a non-canonical or out-of-range field, an impossible enum tag, a
/// zero probability denominator) is [`EnvError::Malformed`].
///
/// [`Answer::decode`]: crate::Answer::decode
/// [`FaultPolicy::from_bytes`]: crate::FaultPolicy::from_bytes
/// [`EnvSpec::decode`]: crate::EnvSpec::decode
/// [`FaultPolicy`]: crate::FaultPolicy
#[derive(Clone, Copy, PartialEq, Eq, Debug, Error)]
pub enum EnvError {
    /// The blob's declared format version is one this build does not decode.
    /// Carries the offending version.
    #[error("unsupported blob version {0}")]
    BadVersion(u16),
    /// The bytes are not a valid, canonical encoding of the expected value.
    #[error("malformed environment blob")]
    Malformed,
}
