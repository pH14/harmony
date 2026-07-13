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
///   [`Reproducer::blob_version`](crate::Reproducer::blob_version), neither of
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
    /// server answers the non-`Whole` hash scopes, a `branch` env carrying a
    /// still-unenforceable guest override / standing fault / non-`none` policy, a
    /// `perturb` of an out-of-scope `SkewTime`/`SetClockRate`, and any verb sent
    /// before `hello` has negotiated a session with this. Loud and distinct from a framing
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
    #[error(
        "perturb CorruptMemory gpa {gpa:#x} + 8 is out of range (guest RAM is {ram_len} bytes)"
    )]
    PerturbOutOfRange {
        /// The offending guest-physical address.
        gpa: u64,
        /// The guest RAM size in bytes.
        ram_len: u64,
    },
    /// A `perturb` (or a `branch` env host fault) names a `Moment` **behind the
    /// current point** (`at < effective_vns`, or, for a branch env, behind the
    /// restored snapshot's V-time). Rejected loud at stage time (task 59): the
    /// fault could only apply *later* than its recorded `Moment`, so the emitted
    /// reproducer would replay it at the wrong count — a reproducer that does not
    /// reproduce. `at == effective_vns` is fine (it applies immediately and
    /// truthfully).
    #[error("perturb Moment {at} is behind the current V-time {floor}")]
    PerturbPastMoment {
        /// The rejected `Moment`.
        at: u64,
        /// The current effective V-time (the earliest still-stageable `Moment`).
        floor: u64,
    },
    /// A `perturb` stages a fault at a `Moment` that **already carries one**.
    /// Task 45's `EnvSpec` override map is `BTreeMap<Moment, Action>` — **one
    /// action per `Moment`** — so a second same-`Moment` stage cannot be recorded
    /// without losing the first; rather than emit a non-reproducing reproducer, the
    /// frontier **loudly rejects** it. (The one-fault-per-`Moment` rule is the
    /// integrator's final ruling — spec amendment PR #54.)
    #[error("perturb Moment {at} already carries a staged fault (one fault per Moment)")]
    PerturbMomentTaken {
        /// The already-occupied `Moment`.
        at: u64,
    },
    /// A `run` reached its V-time deadline having **overshot a staged `Moment`**
    /// without applying it (`moment <= vtime`, but the deadline was below it so it
    /// was never armed). The guest has now executed past that `Moment`, so the
    /// fault can never be applied at its recorded count — the schedule is
    /// unsatisfiable. Failed loud (task 59) rather than let a later `run` apply it
    /// from the past while recording the earlier `Moment` (a reproducer that does
    /// not reproduce). The caller must rewind (`branch`/`replay`, which clears the
    /// schedule) before continuing.
    #[error("run overshot staged Moment {moment} (now at V-time {vtime}); schedule unsatisfiable")]
    ScheduleUnsatisfiable {
        /// The staged `Moment` the run executed past without applying.
        moment: u64,
        /// The effective V-time the run reached (already beyond `moment`).
        vtime: u64,
    },
    /// A `perturb` arrived at a **non-V-time-synchronized point** — the VM's last
    /// stop was a terminal (HLT / shutdown / debug) or another non-intercept exit,
    /// so its effective V-time is only a *lower bound* on the true retired count,
    /// not the exact position. Staging a fault against a lower-bound floor could
    /// record it at a `Moment` the guest has already executed past — a reproducer
    /// that does not reproduce. The client must first reach a synchronized point
    /// (rewind via `branch`/`replay`, which restores onto a V-time intercept) before
    /// staging (task 59; PR #51 round-7 — the exact-`effective_vns` family).
    #[error("perturb at a non-synchronized point (effective V-time is a lower bound)")]
    NotSynchronized,
    /// A `perturb` (or a `branch` env host fault) stages an `InjectInterrupt` with an
    /// **architecturally reserved vector** (`0..=15`), which the LAPIC cannot raise.
    /// A stage-time-decidable property of the request, rejected loudly here (like
    /// [`PerturbOutOfRange`](Self::PerturbOutOfRange) for a gpa) rather than exploding
    /// as a session-fatal apply-time failure (task 59; PR #51 round-8).
    #[error("perturb InjectInterrupt vector {vector} is architecturally reserved (< 16)")]
    PerturbReservedVector {
        /// The reserved vector.
        vector: u8,
    },
    /// A [`Read`](crate::Request::Read) named a `[gpa, gpa+len)` range that runs
    /// past guest RAM. Rejected **loudly** at the observation boundary (task 80:
    /// "out-of-range → error, never a truncated success"): a short read would
    /// hand the client bytes it did not ask for (or zero-fill), silently corrupting
    /// whatever it decodes from them.
    #[error("read [{gpa:#x}, {gpa:#x}+{len}) is out of range (guest RAM is {ram_len} bytes)")]
    ReadOutOfRange {
        /// The requested guest-physical base.
        gpa: u64,
        /// The requested length in bytes.
        len: u32,
        /// The guest RAM size in bytes.
        ram_len: u64,
    },
    /// A [`Read`](crate::Request::Read) asked for more than
    /// [`READ_CAP`](crate::READ_CAP) bytes. Rejected **before any allocation**, so
    /// an untrusted `len` can never force an unbounded buffer (conventions rule 4)
    /// — the same discipline the codec applies to a frame length.
    #[error("read len {len} exceeds the per-call cap of {cap} bytes")]
    ReadTooLarge {
        /// The requested length.
        len: u32,
        /// The per-call cap ([`READ_CAP`](crate::READ_CAP)).
        cap: u32,
    },
    /// The **taint guard** fired (task 81): the current timeline was tainted by an
    /// [`Exec`](crate::Request::Exec) improvisation, and the request would have
    /// minted a reproducer from it ([`RecordedEnv`](crate::Request::RecordedEnv) or
    /// equivalent). An improvised timeline is off the record by ruling
    /// (`docs/RESOLUTION.md` §Improvisations) — its execution carries no determinism
    /// guarantee, so there is no honest [`Reproducer`](crate::Reproducer) that
    /// replays it. Refused **loudly** rather than handing back a reproducer that does
    /// not reproduce; the caller must rewind to an untainted ancestor
    /// (`branch`/`replay`) to reach recordable state again. Taint never clears
    /// downstream — every snapshot and every `branch`/`replay` from a tainted point
    /// stays tainted; only an untainted ancestor is untainted.
    #[error("timeline is tainted by an exec improvisation; refusing to mint a reproducer")]
    Tainted,
    /// A wire-framing failure surfaced as a reply.
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
}
