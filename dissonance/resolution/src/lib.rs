// SPDX-License-Identifier: AGPL-3.0-or-later
//! # resolution — the moment-addressed session client, REPL, and transcript
//!
//! `resolution` is **dissonance**'s epoch-loop agent surface (`docs/RESOLUTION.md`):
//! the layer where an agent — usually an LLM, sometimes a human through one —
//! investigates a moment of a deterministic execution. It is **API-first**: a
//! session client over the task-58 control-transport socket, a thin human/agent
//! REPL over that client, and a `MomentRef`-stamped transcript that makes every
//! investigation a replayable artifact.
//!
//! The universal handle is the [`MomentRef`] — a genesis-complete reproducer
//! ([`EnvSpec`]) plus an absolute [`Moment`] — a copyable, versioned, textual
//! coordinate a user pastes out of a finding to get a live fork at exactly that
//! instant. A [`Session`] [`materialize`](Session::materialize)s one
//! (`branch(genesis, env)` + `run(until = moment)`) into a
//! [`MaterializedSession`], then drives the verb surface: **observation**
//! ([`read`](MaterializedSession::read) / [`regs`](MaterializedSession::regs) /
//! [`hash`](MaterializedSession::hash) — never recorded, hash-invariant),
//! **navigation** ([`run`](MaterializedSession::run), re-materialize),
//! **improvisation** ([`exec`](MaterializedSession::exec) — off the record,
//! taints the timeline), and the **counterfactual**
//! ([`MomentRef::vary`] — replay-with-one-change, a pure edit of the native
//! `BTreeMap<Moment, Action>`). The two result categories are kept strictly
//! apart: a guest outcome is a [`StopReason`] (data), a control failure is a
//! [`SessionError`].
//!
//! ## The mock and the wire
//!
//! The whole laptop gate runs against the in-crate [`MockServer`] — a scripted,
//! deterministic guest reached over the [`Server`] seam (the task-58 loopback
//! pattern, owned here). The verbs `control-proto` already carries use its real
//! wire types; the three tasks 80/81 add — `read` / `regs` / `exec` — are not
//! merged on this branch, so this crate defines their views ([`RegsView`],
//! [`ExecResult`]) and the [`Tainted`](SessionError::Tainted) guard locally
//! (conventions rule 2), matching those specs' wire contract. The live box
//! connection is a second [`Server`] implementor handed to the foreman.
//!
//! ## Module layout
//!
//! `mref` ([`MomentRef`], its textual codec, `vary`) · `server` (the
//! [`Server`] seam + the task-80/81 views) · `session` ([`Session`] /
//! [`MaterializedSession`]) · `mock` ([`MockServer`]) · `transcript` (the
//! JSONL [`Record`] + the one renderer) · `repl` (the line protocol +
//! [`Shell`]) · `error` ([`SessionError`]).

mod error;
mod mock;
mod mref;
mod repl;
mod server;
mod session;
mod transcript;

pub use error::SessionError;
pub use mock::MockServer;
pub use mref::{MRefParseError, MomentRef, OverrideEdit};
pub use repl::{Command, CommandParseError, DispatchOutput, Shell};
pub use server::{ExecResult, RegsView, Server, Snapshot};
pub use session::{MaterializedSession, Session, client_caps};
pub use transcript::{Outcome, Record, from_jsonl, render_line, render_transcript, to_jsonl};

// The wire/reproducer types that appear in this crate's public API, re-exported
// so a consumer need not also name `environment` / `control-proto` directly.
pub use control_proto::{HashScope, SnapId, StopReason};
pub use environment::{Action, EnvSpec, HostFault, Moment};

/// The maximum bytes one [`read`](MaterializedSession::read) may request. A
/// larger `len` is rejected before any allocation, so an untrusted count can
/// never force an unbounded buffer (conventions rule 4).
pub const READ_CAP: u32 = 1 << 16; // 64 KiB

/// The [`MockServer`]'s default scripted guest RAM size — the ceiling `read`
/// range-checks against.
pub const DEFAULT_RAM_BYTES: u64 = 1 << 30; // 1 GiB

/// The default V-time budget an [`exec`](MaterializedSession::exec) adds to the
/// current moment for its deadline (the improvisation runs until a completion
/// sentinel or this deadline).
pub const EXEC_BUDGET: u64 = 1_000_000;

/// Lower-case hex encoding — the transcript's byte representation for read
/// bytes and digests, and the `MomentRef` env blob.
pub(crate) fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // Writing two hex nibbles per byte into a String is infallible.
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Decode canonical lower-case hex to bytes. Total: an odd length or a non-hex
/// (incl. upper-case) digit yields `None`, never a panic. Rejecting upper-case
/// keeps the encoding canonical (one text per byte string).
pub(crate) fn from_hex(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let nib = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        }
    };
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        out.push((nib(pair[0])? << 4) | nib(pair[1])?);
    }
    Some(out)
}
