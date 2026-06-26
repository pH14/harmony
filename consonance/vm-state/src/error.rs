// SPDX-License-Identifier: AGPL-3.0-or-later
//! The single error type for the codec: [`VmStateError`].

use thiserror::Error;

/// Every failure mode of [`VmState::encode`](crate::VmState::encode) and
/// [`VmState::decode`](crate::VmState::decode).
///
/// Decoding is **strict and total**: every malformed blob yields one of these
/// variants and decoding never panics on arbitrary input (Convention rule #4).
/// There is deliberately no `Default`: an error enum has no meaningful default,
/// and a field-bearing variant set could not derive one anyway.
#[derive(Clone, PartialEq, Eq, Debug, Error)]
pub enum VmStateError {
    /// The header magic was not [`VM_STATE_MAGIC`](crate::VM_STATE_MAGIC).
    #[error("bad magic: {0:#010x}")]
    BadMagic(u32),
    /// The header version is not [`VM_STATE_VERSION`](crate::VM_STATE_VERSION).
    /// Decoding refuses a version it does not understand rather than silently
    /// misreading a future layout.
    #[error("unsupported version: {0}")]
    UnsupportedVersion(u16),
    /// Input ended before a complete header, section header, or section payload
    /// was available (a `len` claimed bytes past end-of-buffer).
    #[error("truncated blob")]
    Truncated,
    /// Bytes remain after the declared section count was consumed.
    #[error("trailing bytes after final section")]
    TrailingBytes,
    /// A section carried a tag not defined in this format version.
    #[error("unknown section tag: {0}")]
    UnknownTag(u16),
    /// The same section tag appeared twice (sections must be unique).
    #[error("duplicate section tag: {0}")]
    DuplicateTag(u16),
    /// A section tag was not strictly greater than the previous one (sections
    /// must be emitted in ascending tag order).
    #[error("section tag out of order: {0}")]
    SectionOrder(u16),
    /// A required v1 section tag was absent. Every v1 tag must be present
    /// exactly once; a decoder that tolerated a missing section would silently
    /// restore that machine state as the field's `Default` (zero).
    #[error("missing required section: {0}")]
    MissingSection(u16),
    /// A snapshot-bearing [`VtimeState`](crate::VtimeState) had `ratio_den != 1`.
    /// Refused at `encode` so an un-restorable-exactly timeline is never written
    /// (INTEGRATION.md §4).
    #[error("fractional vtime ratio (ratio_den != 1) cannot be snapshotted")]
    FractionalRatio,
    /// A field held a value outside its valid range — e.g. a `MpState` byte that
    /// is neither 0 nor 1, a boolean event flag that is neither 0 nor 1, an MSR
    /// list whose indices are not strictly ascending, a timer queue whose
    /// entries are not in canonical `(deadline_vns, seq)` order, or a
    /// fixed-layout section whose length does not match its record size.
    #[error("invalid field value")]
    InvalidField,
}
