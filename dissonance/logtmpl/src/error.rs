// SPDX-License-Identifier: AGPL-3.0-or-later
//! The crate's typed error surface. Library code never panics on untrusted
//! input (conventions rule 4); the fallible paths — codebook (de)serialization
//! and version negotiation — return these instead.

use thiserror::Error;

/// Something went wrong (de)serializing a [`Codebook`](crate::Codebook).
#[derive(Debug, Error)]
pub enum Error {
    /// The serialized codebook could not be decoded (malformed JSON, or a shape
    /// that does not match the current schema).
    #[error("codebook decode failed: {0}")]
    Decode(#[from] serde_json::Error),

    /// The serialized codebook carries a version this build does not understand.
    /// Reloading it could silently desync clustering, so it is refused.
    #[error("unsupported codebook version {found} (this build speaks {expected})")]
    Version {
        /// The version stamped in the serialized bytes.
        found: u16,
        /// The version this build serializes and can reload.
        expected: u16,
    },
}

/// The crate result alias.
pub type Result<T> = std::result::Result<T, Error>;
