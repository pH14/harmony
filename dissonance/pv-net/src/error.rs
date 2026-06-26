// SPDX-License-Identifier: AGPL-3.0-or-later
//! The single error type for the switch's restore path: [`NetError`].

use thiserror::Error;

/// A failure decoding a [`Switch::save_state`](crate::Switch::save_state) blob in
/// [`Switch::restore_state`](crate::Switch::restore_state).
///
/// Restore is **strict and total**: any malformed blob — bad magic/version, a
/// truncated buffer, a length field that runs past end-of-buffer, trailing bytes,
/// a non-canonical (unsorted/duplicate) section, or a value that violates a state
/// invariant (e.g. a pending `seq >= next_seq`) — yields [`NetError::Malformed`]
/// and never panics (conventions rule 4). A clean restore is the only `Ok`.
#[derive(Clone, PartialEq, Eq, Debug, Error)]
pub enum NetError {
    /// The blob is not a valid, canonical switch-state snapshot.
    #[error("malformed pv-net switch-state blob")]
    Malformed,
}
