// SPDX-License-Identifier: AGPL-3.0-or-later
//! The two error categories of the control plane, kept strictly apart
//! (`docs/DISSONANCE.md`, "Two result categories, fail-loud"):
//!
//! - [`ProtocolError`] — a **wire-framing** failure: the bytes on the socket are
//!   not a decodable frame (bad magic/version, an over-cap length field, or a
//!   body that does not form a well-formed value). The codec produces these.
//! - [`ControlError`] — a **VM/backend** failure that is *not* a guest-observable
//!   outcome (a guest outcome is a [`StopReason`](crate::StopReason)). It carries
//!   a [`ProtocolError`] only when a framing failure must itself be reported as a
//!   reply.
//!
//! Both are `thiserror` enums; neither is ever produced by panicking on untrusted
//! input (conventions rule 4).

use thiserror::Error;

use crate::SnapId;

/// A wire-framing failure from the [`encode`](crate::encode_request) /
/// [`decode`](crate::decode_request) codec.
///
/// The four variants are the complete framing-error vocabulary:
///
/// - [`ShortFrame`](Self::ShortFrame) — a **complete** frame whose body does not
///   decode to a well-formed value: an unknown discriminant, an inner
///   length/field that runs past the frame body, or trailing bytes inside the
///   declared body. (A frame that is merely *not yet fully received* is not an
///   error — `decode_*` returns `Ok(None)` for that.)
/// - [`BadMagic`](Self::BadMagic) — the frame does not start with the protocol
///   magic.
/// - [`BadVersion`](Self::BadVersion) — the frame header carries a wire-format
///   version this build cannot parse (≠ [`PROTO_VERSION`](crate::PROTO_VERSION)).
///   This is the *framing* version, distinct from the negotiated
///   [`Caps::protocol_version`](crate::Caps::protocol_version) and from an
///   [`Environment::blob_version`](crate::Environment::blob_version), neither of
///   which the codec validates.
/// - [`BadLength`](Self::BadLength) — the header advertises a body longer than
///   [`MAX_FRAME_LEN`](crate::MAX_FRAME_LEN). Reported from the header alone,
///   before any body is buffered, so an untrusted length can never force an
///   unbounded allocation.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Error)]
pub enum ProtocolError {
    /// A complete frame whose body is incomplete or not a well-formed value.
    #[error("short or malformed frame body")]
    ShortFrame,
    /// The frame does not begin with the control-plane magic.
    #[error("bad frame magic")]
    BadMagic,
    /// The frame header's wire-format version is not `PROTO_VERSION`.
    #[error("unsupported wire-format version")]
    BadVersion,
    /// The header's body-length field exceeds `MAX_FRAME_LEN`.
    #[error("frame body length exceeds MAX_FRAME_LEN")]
    BadLength,
}

/// A control-plane failure that is **not** a guest-observable outcome.
///
/// A guest-observable run result is a [`StopReason`](crate::StopReason) (data the
/// explorer reacts to); a `ControlError` is a loud VM/backend/transport failure
/// that must never be reported as a [`StopReason`](crate::StopReason), and vice versa. The codec
/// carries these verbatim inside an error reply; the backend (frontier) produces
/// them.
///
/// Two payload-level failures are kept distinct from a framing
/// [`Protocol`](Self::Protocol) error and from a version error: a frame can
/// decode cleanly yet carry bytes the backend must reject —
/// [`MalformedEnvironment`](Self::MalformedEnvironment) (a [`Branch`] env blob
/// that fails `environment::EnvSpec::decode`) and
/// [`MalformedAnswer`](Self::MalformedAnswer) (a [`Run`] resolve answer that is
/// malformed or wrong-class for the outstanding decision). The backend never
/// misclassifies them or passes untrusted bytes into service code.
///
/// [`Branch`]: crate::Request::Branch
/// [`Run`]: crate::Request::Run
#[derive(Clone, PartialEq, Eq, Debug, Error)]
pub enum ControlError {
    /// A handle that names no live snapshot.
    #[error("unknown snapshot {0:?}")]
    UnknownSnapshot(SnapId),
    /// A `branch`/`replay` failed to restore the named snapshot.
    #[error("restore failed")]
    RestoreFailed,
    /// `snapshot` was requested while a decision was armed (snapshots are
    /// quiescent-only; the armed decision would not be captured).
    #[error("snapshot while a decision is armed")]
    SnapshotWhileArmed,
    /// `snapshot` was requested at a non-quiescent point.
    #[error("not at a quiescent point")]
    NotQuiescent,
    /// A `branch` env blob's `blob_version` is outside the negotiated range.
    #[error("unsupported environment blob version {0}")]
    BadEnvVersion(u16),
    /// A `branch` env blob is well-framed but fails `EnvSpec::decode`.
    #[error("malformed environment blob")]
    MalformedEnvironment,
    /// A `run` carried a `resolve` answer with no outstanding `Decision` to
    /// answer. Never silently dropped — absorbing it would desync the
    /// `DecisionId` counter and break replay.
    #[error("resolve with no outstanding decision")]
    ResolveWithoutDecision,
    /// A `run` resolve answer is malformed or wrong-class for the outstanding
    /// decision.
    #[error("malformed or wrong-class resolve answer")]
    MalformedAnswer,
    /// The verb decoded cleanly but this backend does not service it (yet): the
    /// seed-driven task-58 server answers `perturb` (host-plane enforcement is
    /// task 59) and the non-`Whole` hash scopes with this, and any verb sent
    /// before `hello` has negotiated a session. Loud and distinct from a framing
    /// [`Protocol`](Self::Protocol) error — the frame was well-formed; the
    /// *capability* is absent.
    #[error("verb not supported by this backend")]
    Unsupported,
    /// A `perturb`-staged [`CorruptMemory`] host fault names a guest-physical
    /// address whose 8-byte word falls outside guest RAM (`gpa + 8 > ram_len`).
    /// The frontier (task 59) rejects it **loudly at stage time** rather than
    /// silently clipping or wrapping the write — a corruption at an
    /// unrepresentable address would mint a reproducer that does not reproduce.
    ///
    /// [`CorruptMemory`]: the `environment::HostFault::CorruptMemory` the
    /// `perturb` fault blob decodes to.
    #[error("perturb CorruptMemory gpa {gpa:#x} + 8 is out of range (guest RAM is {ram_len} bytes)")]
    PerturbOutOfRange {
        /// The offending guest-physical address.
        gpa: u64,
        /// The guest RAM size in bytes.
        ram_len: u64,
    },
    /// A wire-framing failure surfaced as a reply.
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
}
