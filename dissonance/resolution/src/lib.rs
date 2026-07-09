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

/// The maximum decoded length of any **hex field** this crate parses from
/// untrusted text: a pasted [`MomentRef`]'s env blob, a `vary … raw` action, and
/// an exec output in a replayed transcript. Checked *before* the buffer is sized
/// (see `from_hex`), so a multi-gigabyte pasted hex string is rejected cheaply
/// — the same capped-untrusted-length discipline as [`READ_CAP`]. Generous
/// enough for any real reproducer (it also bounds the `control-proto` frame the
/// env blob rides), so it never rejects a legitimate paste.
pub const MAX_HEX_FIELD_BYTES: usize = 16 << 20; // 16 MiB

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

/// Decode canonical lower-case hex to bytes, refusing anything decoding to more
/// than `max_bytes`. Total: an odd length, a non-hex (incl. upper-case) digit, or
/// an over-`max_bytes` length yields `None`, never a panic. Rejecting upper-case
/// keeps the encoding canonical (one text per byte string).
///
/// **Capped before allocating.** The `max_bytes` check happens *before* the
/// `Vec::with_capacity`, so a pasted multi-gigabyte hex field (this decodes
/// untrusted text — an `open`ed `MomentRef`, a `vary … raw` action, an exec
/// output in a replayed transcript) is rejected cheaply rather than sizing a
/// buffer to it (the [`READ_CAP`] discipline, conventions rule 4).
pub(crate) fn from_hex(s: &str, max_bytes: usize) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let decoded_len = bytes.len() / 2;
    if decoded_len > max_bytes {
        // Reject BEFORE allocating — never size a buffer to an untrusted length.
        return None;
    }
    let nib = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        }
    };
    let mut out = Vec::with_capacity(decoded_len);
    for pair in bytes.chunks_exact(2) {
        out.push((nib(pair[0])? << 4) | nib(pair[1])?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_hex_caps_decoded_length_before_allocating() {
        // Over-cap → rejected cheaply, before any `Vec::with_capacity`.
        assert_eq!(from_hex("00000000", 3), None); // decodes to 4 bytes > cap 3
        // At / under the cap decodes.
        assert_eq!(from_hex("00ff", 2), Some(vec![0x00, 0xff]));
        assert_eq!(from_hex("00ff", usize::MAX), Some(vec![0x00, 0xff]));
        // The other rejections still hold (odd length, non-/upper-case hex).
        assert_eq!(from_hex("0", usize::MAX), None);
        assert_eq!(from_hex("zz", usize::MAX), None);
        assert_eq!(from_hex("00FF", usize::MAX), None); // upper-case not canonical
    }
}
