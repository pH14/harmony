// SPDX-License-Identifier: AGPL-3.0-or-later
//! The session client's error type — the *control* side of the two-result-
//! categories rule.
//!
//! `docs/DISSONANCE.md`'s "two result categories, fail-loud" law keeps a
//! guest-observable run **outcome** ([`StopReason`](control_proto::StopReason) —
//! data the agent reacts to) strictly apart from a backend/transport **failure**
//! (this [`SessionError`]). The two are never conflated: [`run`](crate::MaterializedSession::run)
//! returns `Ok(StopReason)` for every guest outcome (including a
//! [`Crash`](control_proto::StopReason::Crash)) and `Err(SessionError)` only for
//! a control failure. A [`Tainted`](SessionError::Tainted) guard surfaces
//! verbatim.
//!
//! ## Why [`Tainted`] lives here, not in `control-proto`
//!
//! Tasks 80/81 extend `control-proto` with the `read`/`regs`/`exec` verbs and a
//! `ControlError::Tainted` variant, but those specs are siblings of this one and
//! are not merged on this branch; conventions hard-rule 1 forbids editing
//! `control-proto` from here. So the three not-yet-merged verbs and their errors
//! ([`Tainted`](SessionError::Tainted), the [`read`](crate::MaterializedSession::read)
//! range guards) are modelled at *this* client's boundary (conventions rule 2 —
//! define interfaces locally), matching those specs' wire contract exactly. When
//! 80/81 land, the integrator collapses these onto the real `control-proto`
//! surface; the client's observable behaviour is unchanged.

use thiserror::Error;

/// A control-plane failure at the session client — never a guest-observable
/// outcome (that is a [`StopReason`](control_proto::StopReason)). Every variant
/// is a loud, distinct reason a verb could not be serviced; none is ever
/// produced by panicking on untrusted input (conventions rule 4).
#[derive(Clone, PartialEq, Eq, Debug, Error)]
pub enum SessionError {
    /// A wire [`ControlError`](control_proto::ControlError) from the server,
    /// carried verbatim (unknown snapshot, malformed env, unsupported verb, the
    /// task-59 host-plane stage guards, …). Kept distinct from the client-local
    /// variants below so a reviewer sees exactly which failures originate
    /// server-side.
    #[error("control error: {0}")]
    Control(#[from] control_proto::ControlError),

    /// The task-81 taint guard fired: an operation that would mint or admit a
    /// reproducer (e.g. [`recorded_env`](crate::MaterializedSession::recorded_env))
    /// was attempted on a timeline that an [`exec`](crate::MaterializedSession::exec)
    /// improvisation has tainted. Surfaces verbatim — the improvised timeline is
    /// disposable by ruling and must never become a reproducer that cannot be
    /// regenerated from `(seed, overrides)`.
    ///
    /// Modelled here (not in `control-proto`) until task 81 merges — see the
    /// module docs.
    #[error("timeline is tainted by an exec improvisation; refusing to mint a reproducer")]
    Tainted,

    /// A [`read`](crate::MaterializedSession::read) named a `[gpa, gpa+len)`
    /// range that runs past guest RAM. Rejected loudly (task 80: "out-of-range →
    /// error, never a truncated success"), never a short read.
    #[error("read [{gpa:#x}, {gpa:#x}+{len}) is out of range (guest RAM is {ram_len} bytes)")]
    ReadOutOfRange {
        /// The requested guest-physical base.
        gpa: u64,
        /// The requested length in bytes.
        len: u32,
        /// The guest RAM size in bytes.
        ram_len: u64,
    },

    /// A [`read`](crate::MaterializedSession::read) asked for more than
    /// [`READ_CAP`](crate::READ_CAP) bytes. Rejected before any allocation so an
    /// untrusted `len` can never force an unbounded buffer (conventions rule 4).
    #[error("read len {len} exceeds the per-call cap of {cap} bytes")]
    ReadTooLarge {
        /// The requested length.
        len: u32,
        /// The per-call cap.
        cap: u32,
    },

    /// A verb was issued before a [`MomentRef`](crate::MomentRef) was
    /// materialized — there is no live timeline to act on. (The REPL surfaces
    /// this when a command precedes `open`.)
    #[error("no moment is open; materialize a MomentRef first")]
    NothingOpen,

    /// The `hello` handshake did not negotiate a compatible session (a protocol
    /// or env-version mismatch, or a non-hello reply). Loud, never a silent
    /// downgrade.
    #[error("session negotiation failed: {0}")]
    Negotiation(String),

    /// A transport-level failure (a torn connection, an I/O error, or a reply
    /// that did not match its request). Aborts the operation loudly.
    #[error("transport failure: {0}")]
    Transport(String),
}

impl SessionError {
    /// A short, stable category label for the transcript (`control` / `tainted`
    /// / `read` / `nothing_open` / `negotiation` / `transport`). Distinct from
    /// the `Display` message so a record carries both the taxonomy and the
    /// verbatim text.
    pub fn category(&self) -> &'static str {
        match self {
            Self::Control(_) => "control",
            Self::Tainted => "tainted",
            Self::ReadOutOfRange { .. } | Self::ReadTooLarge { .. } => "read",
            Self::NothingOpen => "nothing_open",
            Self::Negotiation(_) => "negotiation",
            Self::Transport(_) => "transport",
        }
    }
}
