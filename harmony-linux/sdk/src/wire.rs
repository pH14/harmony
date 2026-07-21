// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **SDK event wire convention** (task 73) — the byte-deterministic,
//! versioned payload format every SDK emission rides on the hypercall Event
//! service (`ServiceId::Event`, op 1). This module is the **canonical source of
//! truth**; the host-side link decoder (`dissonance/link`) and the vmm-core
//! run-loop stop-surfacing seam mirror these constants privately (conventions
//! rule 2 — the guest/host protocol pattern, exactly as `hypercall-doorbell`
//! mirrors `hypercall-proto`'s frame magic). A golden test on each side pins the
//! agreement.
//!
//! ## Event-id namespaces
//!
//! Every SDK emission carries a `u32` `event_id` whose **top 8 bits are a
//! namespace** and whose **low 24 bits are a local id** within it. The
//! namespace makes the id spaces of different channels disjoint without a
//! catalog lookup, so an assertion point `5`, a state register `5`, and a
//! buggify point `5` never collide, and the host can decide stop-surfacing from
//! the namespace alone. The SDK owns the namespace allocation so channel plugins
//! (e.g. task 74's OTel bridge) claim ranges without collision.
//!
//! | ns | name        | local id             | payload |
//! |----|-------------|----------------------|---------|
//! | 0  | control     | `0` = catalog decl   | catalog blob |
//! | 1  | assert      | assertion point id   | `[disposition u8][detail_len u16][detail]` |
//! | 2  | state       | register id          | `[op u8][value u64]` |
//! | 3  | buggify     | buggify point id     | `[fired u8]` |
//! | 4  | lifecycle   | `0` = setup_complete | (empty) |
//! | 8..=255 | plugins | plugin-defined       | plugin-defined (OTel is task 74) |
//!
//! All integers are little-endian. Payload builders here are total and never
//! panic; a payload that would exceed one Event frame is reported as an error by
//! the caller, never truncated.

/// The SDK wire format version, carried in the catalog declaration. Bump when the
/// payload layout changes incompatibly (the link decoder rejects an unknown
/// version rather than misreading it).
pub const SDK_WIRE_VERSION: u8 = 1;

/// Bits the namespace occupies at the top of an `event_id`.
pub const NS_SHIFT: u32 = 24;
/// Mask selecting the 24-bit local id of an `event_id`.
pub const LOCAL_MASK: u32 = (1 << NS_SHIFT) - 1;
/// The largest local id an `event_id` can carry (24 bits).
pub const LOCAL_MAX: u32 = LOCAL_MASK;

/// Namespace 0 — control (metadata). Local id 0 is the catalog declaration.
pub const NS_CONTROL: u8 = 0;
/// Namespace 1 — assertion firings (`assert_always`/`sometimes`/`reachable`/`unreachable`).
pub const NS_ASSERT: u8 = 1;
/// Namespace 2 — IJON state registers (`state_set`/`state_max`).
pub const NS_STATE: u8 = 2;
/// Namespace 3 — buggify results.
pub const NS_BUGGIFY: u8 = 3;
/// Namespace 4 — lifecycle (`setup_complete`).
pub const NS_LIFECYCLE: u8 = 4;

/// The catalog-declaration event id (`NS_CONTROL`, local 0).
pub const CATALOG_EVENT_ID: u32 = 0;
/// The `setup_complete` lifecycle event id (`NS_LIFECYCLE`, local 0).
pub const SETUP_COMPLETE_EVENT_ID: u32 = (NS_LIFECYCLE as u32) << NS_SHIFT;

/// Catalog-blob magic, `"SDKC"` little-endian.
pub const CATALOG_MAGIC: u32 = u32::from_le_bytes(*b"SDKC");

/// Assertion disposition: a positive **hit** (a satisfied `sometimes`, a reached
/// `reachable`) — never stops the run.
pub const DISP_HIT: u8 = 0;
/// Assertion disposition: a **violation** (a failed `always`, a reached
/// `unreachable`) — the host surfaces `StopReason::Assertion`.
pub const DISP_VIOLATION: u8 = 1;

/// State-register op: assign (`state_set`).
pub const STATE_SET: u8 = 0;
/// State-register op: keep-the-maximum (`state_max`); the host interprets the
/// max-novelty, the guest only reports the raw value + op (thin-SDK ruling).
pub const STATE_MAX: u8 = 1;

/// Catalog point-kind bytes (in the catalog declaration blob). These name the
/// declared point's role so the never-fired report can be sliced by kind.
pub const KIND_ALWAYS: u8 = 0;
/// `assert_sometimes` point.
pub const KIND_SOMETIMES: u8 = 1;
/// `assert_reachable` point.
pub const KIND_REACHABLE: u8 = 2;
/// `assert_unreachable` point.
pub const KIND_UNREACHABLE: u8 = 3;
/// IJON state register.
pub const KIND_STATE: u8 = 4;
/// Buggify site.
pub const KIND_BUGGIFY: u8 = 5;

/// Compose an `event_id` from a namespace and a 24-bit local id. Callers
/// guarantee `local <= LOCAL_MAX`.
#[inline]
pub const fn event_id(ns: u8, local: u32) -> u32 {
    ((ns as u32) << NS_SHIFT) | (local & LOCAL_MASK)
}

/// Split an `event_id` into `(namespace, local id)`.
#[inline]
pub const fn split(event_id: u32) -> (u8, u32) {
    ((event_id >> NS_SHIFT) as u8, event_id & LOCAL_MASK)
}
