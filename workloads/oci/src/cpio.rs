// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared deterministic cpio writer used by the OCI staging bundle.
//!
//! The staged container bundle is injected into the guest by appending a
//! second initramfs segment to the stock guest initramfs: the kernel accepts
//! concatenated cpio archives (and concatenated gzip members decompress to
//! their concatenation), and later entries override earlier ones. Entries are
//! written in sorted order with zeroed mtimes so the same bundle always
//! produces the same bytes — the segment participates in the run digest.

pub use guest_image::{CpioError, Writer};
