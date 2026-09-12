// SPDX-License-Identifier: AGPL-3.0-or-later

use thiserror::Error;

#[derive(Clone, PartialEq, Eq, Debug, Error)]
pub enum VmStateError {
    #[error("bad magic: {0:#010x}")]
    BadMagic(u32),
    #[error("unsupported version: {0}")]
    UnsupportedVersion(u16),
    #[error("unsupported arch tag: {0}")]
    UnsupportedArch(u16),
    #[error("truncated blob")]
    Truncated,
    #[error("trailing bytes after final section")]
    TrailingBytes,
    #[error("unknown section tag: {0}")]
    UnknownTag(u16),
    #[error("duplicate section tag: {0}")]
    DuplicateTag(u16),
    #[error("section tag out of order: {0}")]
    SectionOrder(u16),
    #[error("missing required section: {0}")]
    MissingSection(u16),
    #[error("invalid field value")]
    InvalidField,
}
