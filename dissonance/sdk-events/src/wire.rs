// SPDX-License-Identifier: AGPL-3.0-or-later
//! The SDK event wire convention, **mirrored** from the guest SDK crate
//! (`harmony-linux/sdk/src/wire.rs`, the canonical source of truth). The link tier is the
//! host-side reader of code the guest emits, so — exactly as `hypercall-doorbell`
//! privately mirrors `hypercall-proto`'s frame magic (conventions rule 2, the
//! guest/host protocol pattern) — these constants restate the guest's format and
//! the decode goldens in `tests/decode.rs` pin byte-for-byte agreement with the
//! bytes the SDK actually emits (`harmony-linux/sdk/tests/loopback.rs` pins the guest
//! side); if the two ever drift, a golden breaks on one side or the other.
//!
//! Event-id layout: the top 8 bits are a namespace, the low 24 bits a local id.

/// The SDK wire format version carried in the catalog declaration.
pub(crate) const SDK_WIRE_VERSION: u8 = 1;

/// The **wire-v2** catalog-declaration version (`hm-bbx.1`): the cooperative
/// production declaration that, unlike v1, carries per-point occurrence/state
/// classification, value shape, and base update operation. Firings still arrive
/// under the same namespaced `event_id`s and payloads as v1 — v2 only enriches the
/// *declaration*, so a v2-declared state point is reducible before it ever fires.
///
/// v2 catalog blob layout (all integers little-endian):
/// ```text
/// [magic u32][version=2 u8][count u32]
///   repeat count:
///     [namespace u8][local u32]
///     [classification u8][value_shape u8][base_op u8][expectation u8]
///     [name_len u16][name bytes]
/// ```
/// The four enumerated bytes use the `V2_*` constants below; `*_NONE` (255) marks
/// an absent shape/op/expectation. This host-side format is decoded by
/// [`crate::decode_binary`] and encoded by [`crate::encode_v2_declaration`]; the
/// canonical guest-side encoder is a future `harmony-linux/sdk` deliverable (out of this
/// task's surface).
pub(crate) const SDK_WIRE_VERSION_V2: u8 = 2;

/// v2 classification byte: a one-shot occurrence.
pub(crate) const V2_CLASS_OCCURRENCE: u8 = 0;
/// v2 classification byte: a state-bearing register.
pub(crate) const V2_CLASS_STATE: u8 = 1;

/// v2 sentinel: an absent value shape / base op / expectation.
pub(crate) const V2_NONE: u8 = 255;

/// v2 expectation byte: must be hit / satisfied at least once.
pub(crate) const V2_EXPECT_MUST_HIT: u8 = 0;
/// v2 expectation byte: must never be hit.
pub(crate) const V2_EXPECT_MUST_NOT_HIT: u8 = 1;

/// Bits the namespace occupies at the top of an `event_id`.
pub(crate) const NS_SHIFT: u32 = 24;
/// Mask selecting the 24-bit local id of an `event_id`.
pub(crate) const LOCAL_MASK: u32 = (1 << NS_SHIFT) - 1;

/// Namespace 0 — control (local 0 = catalog declaration).
pub const NS_CONTROL: u8 = 0;
/// Namespace 1 — assertion firings.
pub const NS_ASSERT: u8 = 1;
/// Namespace 2 — IJON state registers.
pub const NS_STATE: u8 = 2;
/// Namespace 3 — buggify results.
pub const NS_BUGGIFY: u8 = 3;
/// Namespace 4 — lifecycle.
pub const NS_LIFECYCLE: u8 = 4;

/// Catalog-declaration event id (`NS_CONTROL`, local 0).
pub(crate) const CATALOG_EVENT_ID: u32 = 0;

/// Catalog-blob magic, `"SDKC"` little-endian.
pub(crate) const CATALOG_MAGIC: u32 = u32::from_le_bytes(*b"SDKC");

/// Assertion disposition: a positive **hit**.
pub(crate) const DISP_HIT: u8 = 0;
/// Assertion disposition: a **violation**.
pub(crate) const DISP_VIOLATION: u8 = 1;

/// State-register firing op: assign.
pub(crate) const STATE_SET: u8 = 0;
/// State-register firing op: keep-the-maximum.
pub(crate) const STATE_MAX: u8 = 1;
/// State-register firing op: keep-the-minimum. A wire-v2 extension (the canonical
/// v1 guest encoder emits only set/max); a `min`-declared point fires under this
/// byte. Kept numerically equal to [`crate::UpdateOp`]'s `Min` byte.
pub(crate) const STATE_MIN: u8 = 2;
/// State-register firing op: accumulate the observed value into the retained set.
/// A wire-v2 extension, aligned with [`crate::UpdateOp`]'s `Accumulate` byte.
pub(crate) const STATE_ACCUMULATE: u8 = 3;

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
