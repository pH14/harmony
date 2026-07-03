// SPDX-License-Identifier: AGPL-3.0-or-later
//! The SDK event wire convention, **mirrored** from the guest SDK crate
//! (`guest/sdk/src/wire.rs`, the canonical source of truth). The link tier is the
//! host-side reader of code the guest emits, so — exactly as `vmcall-transport`
//! privately mirrors `hypercall-proto`'s frame magic (conventions rule 2, the
//! guest/host protocol pattern) — these constants restate the guest's format and
//! a golden test (`tests/wire_golden.rs`) pins byte-for-byte agreement with the
//! bytes the SDK actually emits.
//!
//! Event-id layout: the top 8 bits are a namespace, the low 24 bits a local id.

/// The SDK wire format version carried in the catalog declaration.
pub(crate) const SDK_WIRE_VERSION: u8 = 1;

/// Bits the namespace occupies at the top of an `event_id`.
pub(crate) const NS_SHIFT: u32 = 24;
/// Mask selecting the 24-bit local id of an `event_id`.
pub(crate) const LOCAL_MASK: u32 = (1 << NS_SHIFT) - 1;

/// Namespace 0 — control (local 0 = catalog declaration).
pub(crate) const NS_CONTROL: u8 = 0;
/// Namespace 1 — assertion firings.
pub(crate) const NS_ASSERT: u8 = 1;
/// Namespace 2 — IJON state registers.
pub(crate) const NS_STATE: u8 = 2;
/// Namespace 3 — buggify results.
pub(crate) const NS_BUGGIFY: u8 = 3;
/// Namespace 4 — lifecycle.
pub(crate) const NS_LIFECYCLE: u8 = 4;

/// Catalog-declaration event id (`NS_CONTROL`, local 0).
pub(crate) const CATALOG_EVENT_ID: u32 = 0;

/// Catalog-blob magic, `"SDKC"` little-endian.
pub(crate) const CATALOG_MAGIC: u32 = u32::from_le_bytes(*b"SDKC");

/// Assertion disposition: a positive **hit**.
pub(crate) const DISP_HIT: u8 = 0;
/// Assertion disposition: a **violation**.
pub(crate) const DISP_VIOLATION: u8 = 1;

/// State-register op: assign.
pub(crate) const STATE_SET: u8 = 0;
/// State-register op: keep-the-maximum.
pub(crate) const STATE_MAX: u8 = 1;

/// Catalog point-kind bytes.
pub(crate) const KIND_ALWAYS: u8 = 0;
pub(crate) const KIND_SOMETIMES: u8 = 1;
pub(crate) const KIND_REACHABLE: u8 = 2;
pub(crate) const KIND_UNREACHABLE: u8 = 3;
pub(crate) const KIND_STATE: u8 = 4;
pub(crate) const KIND_BUGGIFY: u8 = 5;

/// The lifecycle local id for `setup_complete`.
pub(crate) const LIFECYCLE_SETUP_COMPLETE: u32 = 0;

/// Split an `event_id` into `(namespace, local id)`.
#[inline]
pub(crate) const fn split(event_id: u32) -> (u8, u32) {
    ((event_id >> NS_SHIFT) as u8, event_id & LOCAL_MASK)
}
