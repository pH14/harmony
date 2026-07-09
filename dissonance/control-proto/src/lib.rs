// SPDX-License-Identifier: AGPL-3.0-or-later
//! # control-proto — the out-of-band control-plane wire protocol
//!
//! `control-proto` is **dissonance**'s R2 control plane: the versioned,
//! length-delimited request/response protocol the explorer uses to drive a VM as
//! a black box — `snapshot` / `branch` / `replay` / `run` / `hash` — over a unix
//! `SOCK_STREAM`. It is the out-of-band twin of the in-band guest↔host hypercall
//! plane (`hypercall-proto`). This crate is the **protocol layer only**: the wire
//! [types](mod@types) and the [codec](mod@codec). The socket itself, the
//! verb→backend binding, and the stage-and-re-enter run suspension are frontier
//! (vmm-core), built later against these types.
//!
//! Two design rules from `docs/DISSONANCE.md` are load-bearing here:
//!
//! - **No bare `restore`.** Every restore is [`Replay`](Request::Replay)
//!   (verbatim — the determinism-gate / repro path) or [`Branch`](Request::Branch)
//!   (reseed with a new [`Environment`] — the explore path). The choice is
//!   explicit at every call site.
//! - **Schema-blind to `Environment`.** R2 ferries the modulation unit as an
//!   opaque, versioned blob ([`Environment`]) and a per-decision answer as opaque
//!   [`Answer`]. It never parses them — their structure is task 24's contract.
//!   This is what lets R2 be coded ahead of the fault model.
//!
//! Two result categories are kept strictly apart (fail-loud): a guest-observable
//! outcome is a [`StopReason`] (data); a VM/transport failure is a
//! [`ControlError`] (a loud protocol error). Neither is ever reported as the
//! other. The encoding is **bit-deterministic and versioned from day one**, and
//! the [decoder](decode_request) is a `docs/CODE-QUALITY.md` Tier-1 fuzz target:
//! it never panics, never reads out of bounds, and rejects an over-cap frame
//! length before buffering its body. Nothing here observes wall-clock time, host
//! entropy, `HashMap`/`HashSet` iteration order, or floating point.
//!
//! ## Module layout
//!
//! [`mod@types`] (the plain wire data: carried units, handles, verbs, run-control,
//! and outcomes) · [`mod@error`] ([`ControlError`] / [`ProtocolError`]) ·
//! [`mod@codec`] (the strict, canonical little-endian framing + a forward-only
//! bounds-checked reader).

mod codec;
mod error;
mod types;

pub use codec::{decode_reply, decode_request, encode_reply, encode_request};
pub use error::{ControlError, ProtocolError};
pub use types::{
    Answer, CapFlags, Caps, CoverageGeometry, CrashInfo, CrashKind, DecisionId, Environment,
    EventRef, HashScope, HostFault, Moment, RegsView, Reply, Request, SnapId, StopConditions,
    StopMask, StopReason, VTime, class_bit,
};

/// The wire-format version carried in every frame header. Bumps only when the
/// *framing* layout changes (distinct from the negotiated
/// [`APP_PROTOCOL_VERSION`] / [`Caps::protocol_version`] and from an
/// [`Environment::blob_version`], which the codec never validates). A frame whose
/// header version differs is rejected with [`ProtocolError::BadVersion`].
pub const PROTO_VERSION: u16 = 1;

/// The current **negotiated application-protocol version** — the single source of
/// truth both peers advertise in [`Caps::protocol_version`] and compare at
/// `hello`. Distinct from the framing [`PROTO_VERSION`]: it names the *verb + reply
/// vocabulary and semantics*, not the frame layout.
///
/// **Bump procedure.** Increment this whenever the wire vocabulary changes — a new
/// verb, a new [`Reply`], or (as here) a new [`ControlError`] tag — even when the
/// change is byte-*additive* to the codec. An additive tag decodes fine on a peer
/// that already knows it, but a peer that negotiated the *old* version would pass
/// `hello` and then hit a mid-session `ProtocolError::ShortFrame` on the first new
/// tag; bumping the negotiated version makes such a peer reject **at `hello`**
/// instead (the `caps.protocol_version` mismatch is detectable there — see the
/// negotiation gate). Bumped to **2** by PR #51 (task 59): the host-plane
/// enforcement path added the `PerturbOutOfRange` / `PerturbPastMoment` /
/// `PerturbMomentTaken` / `ScheduleUnsatisfiable` / `NotSynchronized` /
/// `PerturbReservedVector` reply tags. Bumped to **3** by task 73: the
/// `SdkEvents` verb + `SdkEvents` reply carry the link-tier event capture. Bumped
/// to **4** by task 73 round-7: the SDK stops (`Assertion` / `SnapshotPoint`) are
/// now gated on their new `StopMask` class bits (8 / 9) rather than surfacing
/// unconditionally — a semantic change to the `Run` stop set. Bumped to **5** by
/// task 80: the observation verbs `read` / `regs` add the `Read` / `Regs`
/// requests, the `Bytes` / `Regs` reply tags, and the `ReadOutOfRange` /
/// `ReadTooLarge` error tags — additive, but a peer that negotiated an older
/// version must reject at `hello` rather than hit a mid-session `ShortFrame` on a
/// tag it does not know. Bumped to **6** by task 81: the improvisation surface —
/// the `Exec` + `RecordedEnv` verbs, the `ExecResult` / `Snapshot` (taint-carrying)
/// / `Recorded` reply tags, and the `ControlError::Tainted` reply tag. Bumped to
/// **7** by task 69 M2: the `Console` scrape verb (`Request::Console` /
/// `Reply::Console`, the socket console-capture tier) extends the wire vocabulary,
/// so a v≤6 peer must reject at `Hello` rather than fail mid-session on an unknown
/// tag.
pub const APP_PROTOCOL_VERSION: u16 = 7;

/// The maximum bytes one [`Read`](Request::Read) may request. A larger `len` is a
/// loud [`ReadTooLarge`](ControlError::ReadTooLarge), rejected **before any
/// allocation**, so an untrusted count can never force an unbounded buffer
/// (conventions rule 4). 64 KiB — generous for any probe region a resolution
/// session reads, and far below [`MAX_FRAME_LEN`] so a full `Bytes` reply always
/// frames. Both peers agree on this number; a server picking a smaller effective
/// cap still rejects loudly, never truncates.
pub const READ_CAP: u32 = 1 << 16; // 64 KiB

/// Maximum on-wire frame *body* length. Generous for [`Environment`] blobs and
/// hashes, but bounded so untrusted transport can never force unbounded
/// buffering: [`decode_request`] / [`decode_reply`] return
/// [`ProtocolError::BadLength`] the moment a header's length field exceeds this —
/// before the body is buffered — and [`encode_request`] / [`encode_reply`] refuse
/// to emit a body larger than this.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024; // 16 MiB

#[cfg(test)]
mod tests {
    use super::*;

    /// `MAX_FRAME_LEN` and `PROTO_VERSION` are part of the wire contract — both
    /// peers must agree on the exact numbers. Pin them with bare literals (no
    /// arithmetic to mutate), so the values can never drift silently.
    #[test]
    fn wire_constants_are_pinned() {
        assert_eq!(MAX_FRAME_LEN, 16_777_216); // == 16 * 1024 * 1024 (16 MiB)
        assert_eq!(PROTO_VERSION, 1);
        assert_eq!(APP_PROTOCOL_VERSION, 7); // task 69 M2: Console scrape verb added to the wire vocabulary
        assert_eq!(READ_CAP, 65_536); // == 1 << 16 (64 KiB)
    }
}
