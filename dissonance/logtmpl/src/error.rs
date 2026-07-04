// SPDX-License-Identifier: AGPL-3.0-or-later
//! The crate's typed error surface. Library code never panics on untrusted
//! input (conventions rule 4); the fallible paths — codebook (de)serialization
//! and version negotiation — return these instead.

use thiserror::Error;

/// Something went wrong (de)serializing the internal template codebook (the
/// snapshot bytes behind [`LogSensor::with_codebook_bytes`](crate::LogSensor::with_codebook_bytes)).
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

    /// The serialized codebook's parse tree references a template id that does
    /// not exist. Loading it would let the next `ingest` index out of bounds and
    /// panic, so a codebook with a dangling reference is refused on load.
    #[error("codebook references template id {id} but only {count} templates exist")]
    DanglingTemplate {
        /// The out-of-range template id found in a leaf.
        id: u64,
        /// How many templates the codebook actually holds.
        count: usize,
    },

    /// The serialized codebook's id alias table is malformed — an alias must
    /// retire a real id to a **strictly-lower** survivor (`survivor < retired`).
    /// A non-descending or out-of-range alias could make canonicalization loop or
    /// index out of bounds, so it is refused on load.
    #[error("codebook has a corrupt alias {retired} -> {survivor} (survivor must be lower)")]
    CorruptAlias {
        /// The retired (higher) id.
        retired: u64,
        /// The survivor it aliases to (must be strictly lower and in range).
        survivor: u64,
    },

    /// A parse-tree leaf's candidate list is not **strictly ascending** by id.
    /// `ingest`'s lowest-id tie-break relies on ascending iteration to keep the
    /// earliest (lowest) candidate, so a non-ascending or duplicated list (e.g.
    /// `[1, 0]`) would silently pick the wrong template. Refused on load.
    #[error("codebook leaf candidate list is not strictly ascending ({previous} >= {next})")]
    NonAscendingLeaf {
        /// The earlier id in the offending adjacent pair.
        previous: u64,
        /// The later id, which must be strictly greater.
        next: u64,
    },

    /// A template id is **retired** (a key in the alias table) yet still appears
    /// as a live candidate in a parse-tree leaf. An honest fold removes a retired
    /// id from its leaf when it aliases it, so this only arises from a corrupt
    /// snapshot — where the retired template could be matched, mutated, and
    /// emitted as its survivor. Refused on load.
    #[error("template id {id} is retired (aliased to {survivor}) but still live in a leaf")]
    RetiredTemplateLive {
        /// The retired id found live in a leaf.
        id: u64,
        /// The survivor it aliases to.
        survivor: u64,
    },

    /// Two **live** templates in the same parse-tree leaf have the identical
    /// shape. The shape-uniqueness invariant forbids this — an honest fold merges
    /// a duplicate into the survivor (or reuses it on the mint path) rather than
    /// keeping two — so it only arises from a corrupt snapshot, where the tie-break
    /// would resolve ambiguously and one template would be unreachable dead
    /// weight. Refused on load.
    #[error("template id {id} duplicates the shape of an earlier live template in its leaf")]
    DuplicateLiveShape {
        /// The later live id whose shape collides with an earlier one in the leaf.
        id: u64,
    },
}

/// The crate result alias.
pub type Result<T> = std::result::Result<T, Error>;
