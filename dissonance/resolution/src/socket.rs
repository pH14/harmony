// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`SocketServer`] — the **production** [`Server`] over a real `control-proto`
//! stream: the second client of the task-58 control server, the one
//! `docs/RESOLUTION.md` promised ("resolution forks and reads *directly* against
//! the control-transport server … the same verb socket the explorer drives").
//!
//! Every [`Server`] verb is one request/reply exchange of `control-proto` frames
//! on a `Read + Write` stream — a connected unix socket, one end of a
//! socketpair, or any in-process duplex. The crate's other implementor,
//! [`MockServer`](crate::MockServer), scripts the same seam in-process; nothing
//! in a [`Session`](crate::Session)'s observable behaviour distinguishes the two,
//! which is what lets the laptop gate run against the mock and the box gate
//! against this.
//!
//! ## Trust boundary (conventions rule 4)
//!
//! The stream is untrusted: a torn connection, a truncated or malformed frame, a
//! reply that does not answer the request, a `Bytes` reply shorter than the
//! `read` that asked for it, or a reproducer blob in an unknown schema are all
//! **typed [`SessionError`]s**, never a panic and never a silent truncation.
//! Lengths off the wire are checked before they are believed: a `read` past
//! [`READ_CAP`] is refused *before any traffic*, and the codec rejects an
//! over-`MAX_FRAME_LEN` header before buffering its body.
//!
//! The same rule governs anything the peer can make us accumulate *across* calls,
//! not just within one: [`sdk_events`](SocketServer::sdk_events) pages until the
//! server says the capture is drained, so it carries an aggregate budget
//! ([`SDK_EVENTS_CAP`] / [`SDK_EVENTS_BYTES_CAP`]) — a server that never signals
//! the end is a typed error, not an OOM kill.
//!
//! ## Error mapping (the seam's two categories, preserved)
//!
//! A guest-observable outcome is a [`StopReason`](control_proto::StopReason)
//! returned `Ok`; every failure is a [`SessionError`]. The wire's
//! [`ControlError`](control_proto::ControlError) maps onto the client's typed
//! variants so that **the same failure looks the same whichever [`Server`] a
//! consumer holds**: `ReadOutOfRange` / `ReadTooLarge` / `Tainted` become
//! [`SessionError::ReadOutOfRange`] / [`SessionError::ReadTooLarge`] /
//! [`SessionError::Tainted`] — exactly what [`MockServer`](crate::MockServer)
//! raises for those conditions — and every other control error rides through
//! verbatim as [`SessionError::Control`]. A transport/framing failure is
//! [`SessionError::Transport`].
//!
//! ## `hello` is negotiated once per stream
//!
//! The wire contract makes `hello` the *first frame of a session* (a conforming
//! server may refuse a second one). So the adapter negotiates **once** per
//! stream and answers later [`hello`](Server::hello) calls from the cached
//! server [`Caps`] without a second frame. That is what lets a raw pre-pass (a
//! scrape, a base snapshot) and a [`Session::connect`](crate::Session::connect)
//! layered over the *same* adapter share one wire session. Offering *different*
//! caps after negotiation is a loud [`SessionError::Negotiation`], never a
//! silently stale answer.

use std::io::{Read, Write};

use control_proto::{
    Caps, ControlError, HashScope, Reply, Reproducer, Request, SnapId, StopConditions, StopReason,
};
use environment::EnvSpec;

use crate::server::{ExecResult, RegsView, Server, Snapshot};
use crate::{READ_CAP, SDK_EVENTS_BYTES_CAP, SDK_EVENTS_CAP, SessionError};

/// The read buffer's chunk size — one `read(2)` per iteration of the
/// reply-assembly loop, exactly the explorer socket adapter's shape.
const CHUNK: usize = 4096;

/// The production [`Server`]: resolution's session verbs over a `control-proto`
/// stream (see the module doc for the trust boundary, the error mapping, and the
/// once-per-stream `hello`).
///
/// `S` is any `Read + Write` duplex — a connected `UnixStream`, a socketpair
/// end, or an in-process pipe. The adapter owns no VM state: it is a pure
/// request/reply translator, so a [`Session`](crate::Session) over it and a
/// `Session` over [`MockServer`](crate::MockServer) differ only in what is on
/// the far end.
pub struct SocketServer<S: Read + Write> {
    stream: S,
    /// The request sequence number; each [`call`](Self::call) increments it and
    /// the reply must echo it.
    seq: u32,
    /// Undecoded reply bytes carried between calls (a stream may deliver a frame
    /// in pieces, or more than one frame in a read).
    inbuf: Vec<u8>,
    /// The request-encode scratch buffer, cleared and reused per call so the hot
    /// verb path does not churn the heap.
    outbuf: Vec<u8>,
    /// The caps exchanged at negotiation: what we offered and what the server
    /// answered. `None` until the first [`hello`](Server::hello).
    negotiated: Option<(Caps, Caps)>,
    /// The aggregate ceiling on one [`sdk_events`](Self::sdk_events) drain —
    /// `(max events, max payload bytes)`. Defaults to
    /// ([`SDK_EVENTS_CAP`], [`SDK_EVENTS_BYTES_CAP`]).
    sdk_budget: (u32, usize),
}

impl<S: Read + Write> SocketServer<S> {
    /// Wrap a connected control-transport stream. Performs no I/O: the session
    /// is negotiated by the first [`hello`](Server::hello) — which
    /// [`Session::connect`](crate::Session::connect) issues, so the usual path is
    /// `Session::connect(SocketServer::new(stream))`.
    pub fn new(stream: S) -> Self {
        SocketServer {
            stream,
            seq: 0,
            inbuf: Vec::new(),
            outbuf: Vec::new(),
            negotiated: None,
            sdk_budget: (SDK_EVENTS_CAP, SDK_EVENTS_BYTES_CAP),
        }
    }

    /// Re-set the aggregate ceiling one [`sdk_events`](Self::sdk_events) drain
    /// may accumulate: at most `max_events` events carrying at most `max_bytes`
    /// of payload in total. Exceeding either is a typed
    /// [`SessionError::Transport`], never an unbounded accumulation.
    ///
    /// The defaults ([`SDK_EVENTS_CAP`] / [`SDK_EVENTS_BYTES_CAP`]) clear any real
    /// capture by orders of magnitude; this exists so a session with an unusual
    /// workload can raise the bound deliberately — and so the guard itself is
    /// cheap to exercise — **not** so it can be removed. There is no way to
    /// express "unbounded", by design.
    pub fn set_sdk_event_budget(&mut self, max_events: u32, max_bytes: usize) {
        self.sdk_budget = (max_events, max_bytes);
    }

    /// The server [`Caps`] negotiated at `hello`, or `None` before the session is
    /// negotiated.
    pub fn server_caps(&self) -> Option<Caps> {
        self.negotiated.map(|(_, server)| server)
    }

    /// Take the stream back, dropping the adapter. The session's negotiation and
    /// its sequence counter go with it — a fresh adapter over the same stream
    /// would re-`hello` and restart the sequence, so this is for tearing a
    /// connection down (or inspecting it), not for handing it to a second
    /// adapter.
    pub fn into_inner(self) -> S {
        self.stream
    }

    /// Drain the server-side **SDK event capture** of the current run: the
    /// `Moment`-stamped `(moment, event_id, bytes)` stream a cooperating guest
    /// SDK emitted (task 73). Not a [`Server`] verb — it observes the *server's*
    /// capture rather than the guest's state — but it is the channel a film/
    /// campaign scrape harvests its frame clock from, so it lives on the adapter
    /// beside the verbs.
    ///
    /// **Paged.** The server bounds each reply to the control frame limit, so
    /// this fetches from the running offset until an empty page — a single
    /// `offset: 0` call would silently truncate a capture longer than one page.
    /// A pure read: it never advances the VM, so a run's `state_hash` is
    /// identical whether or not it is called.
    ///
    /// **Bounded** (conventions rule 4). The empty page that ends the drain is a
    /// signal from an *untrusted peer*: a server that never sends one — broken, or
    /// hostile — would grow the accumulator until the OOM reaper kills the
    /// process, which is not a failure any caller can catch or report. So the
    /// drain carries an aggregate budget on **both** axes (a peer could stay under
    /// an event count while sending unboundedly large payloads, since each page is
    /// frame-limited but the number of pages is not), and busting either is a loud
    /// [`SessionError::Transport`]. The budget is checked **before** each page is
    /// absorbed, so the accumulator never exceeds it. Defaults:
    /// [`SDK_EVENTS_CAP`] / [`SDK_EVENTS_BYTES_CAP`]; see
    /// [`set_sdk_event_budget`](Self::set_sdk_event_budget).
    pub fn sdk_events(&mut self) -> Result<Vec<(u64, u32, Vec<u8>)>, SessionError> {
        let (max_events, max_bytes) = self.sdk_budget;
        let mut all: Vec<(u64, u32, Vec<u8>)> = Vec::new();
        let mut bytes: usize = 0;
        loop {
            // The offset is an event *index* on the wire. `all.len()` is held
            // under `max_events` (a u32) by the budget below, so this cannot
            // overflow the page index.
            let offset = u32::try_from(all.len()).map_err(|_| {
                SessionError::Transport(
                    "sdk-event capture exceeds the u32 page index of the wire verb".to_string(),
                )
            })?;
            let page = match self.call(&Request::SdkEvents { offset })? {
                Reply::SdkEvents(events) => events,
                other => return Err(unexpected("sdk_events", &other)),
            };
            // The end of the capture. A well-behaved server always gets here.
            if page.is_empty() {
                return Ok(all);
            }
            // Budget FIRST, absorb second — so a runaway peer can never make this
            // buffer grow past what the caller sanctioned.
            let events = all.len().saturating_add(page.len());
            if events > max_events as usize {
                return Err(SessionError::Transport(format!(
                    "sdk-event drain exceeded its budget of {max_events} events (the server never \
                     sent an empty page); refusing to keep buffering an unbounded capture"
                )));
            }
            let page_bytes = page
                .iter()
                .fold(0usize, |acc, (_, _, b)| acc.saturating_add(b.len()));
            bytes = bytes.saturating_add(page_bytes);
            if bytes > max_bytes {
                return Err(SessionError::Transport(format!(
                    "sdk-event drain exceeded its budget of {max_bytes} payload bytes (the server \
                     never sent an empty page); refusing to keep buffering an unbounded capture"
                )));
            }
            all.extend(page);
        }
    }

    /// One request/reply exchange. Encodes `req`, writes it, then assembles reply
    /// frames until one decodes. Every failure mode is typed: a write/read error
    /// or an EOF mid-reply is [`SessionError::Transport`], a framing failure is
    /// `Transport`, a reply whose sequence number does not echo the request's is
    /// `Transport` (never matched to the wrong verb), and an error reply is the
    /// mapped [`SessionError`].
    fn call(&mut self, req: &Request) -> Result<Reply, SessionError> {
        self.seq = self.seq.wrapping_add(1);
        self.outbuf.clear();
        control_proto::encode_request(self.seq, req, &mut self.outbuf)
            .map_err(|e| SessionError::Transport(format!("request encode failed: {e}")))?;
        self.stream
            .write_all(&self.outbuf)
            .and_then(|()| self.stream.flush())
            .map_err(|e| SessionError::Transport(format!("socket write failed: {e}")))?;
        let mut chunk = [0u8; CHUNK];
        loop {
            // The codec refuses an over-`MAX_FRAME_LEN` header before its body is
            // buffered, so a hostile length field cannot grow `inbuf` unbounded.
            match control_proto::decode_reply(&self.inbuf)
                .map_err(|e| SessionError::Transport(format!("reply framing error: {e}")))?
            {
                Some((seq, reply, consumed)) => {
                    self.inbuf.drain(..consumed);
                    if seq != self.seq {
                        return Err(SessionError::Transport(format!(
                            "reply seq {seq} does not echo request seq {}",
                            self.seq
                        )));
                    }
                    return reply.map_err(control_error_to_session);
                }
                None => {
                    let n = self
                        .stream
                        .read(&mut chunk)
                        .map_err(|e| SessionError::Transport(format!("socket read failed: {e}")))?;
                    if n == 0 {
                        return Err(SessionError::Transport(
                            "server closed the stream mid-reply".to_string(),
                        ));
                    }
                    self.inbuf.extend_from_slice(&chunk[..n]);
                }
            }
        }
    }
}

/// A reply that does not answer the verb that was sent. Loud
/// [`SessionError::Transport`]: the session is out of step with the server, and
/// guessing would silently mis-attribute state.
fn unexpected(verb: &str, got: &Reply) -> SessionError {
    SessionError::Transport(format!("{verb} answered with an unexpected reply: {got:?}"))
}

/// Map a wire [`ControlError`] onto the client's [`SessionError`]. The three
/// conditions the client models natively — an out-of-range read, an over-cap
/// read, and the task-81 taint guard — become the *same* typed variants
/// [`MockServer`](crate::MockServer) raises, so a consumer's match arms do not
/// depend on which [`Server`] it holds. Everything else rides through verbatim as
/// [`SessionError::Control`] (still visibly server-originated). No wire error is
/// ever turned into a [`StopReason`] — the two result categories never cross.
fn control_error_to_session(err: ControlError) -> SessionError {
    match err {
        ControlError::ReadOutOfRange { gpa, len, ram_len } => {
            SessionError::ReadOutOfRange { gpa, len, ram_len }
        }
        ControlError::ReadTooLarge { len, cap } => SessionError::ReadTooLarge { len, cap },
        ControlError::Tainted => SessionError::Tainted,
        other => SessionError::Control(other),
    }
}

impl<S: Read + Write> Server for SocketServer<S> {
    /// Negotiate the session — **once per stream** (module doc). The first call
    /// exchanges the `hello` frame and caches the server's [`Caps`]; a later call
    /// offering the same caps answers from the cache without a frame. Offering
    /// *different* caps after negotiation cannot be honoured on a session the
    /// wire contract has already fixed, so it is a loud
    /// [`SessionError::Negotiation`].
    fn hello(&mut self, caps: Caps) -> Result<Caps, SessionError> {
        if let Some((offered, server)) = self.negotiated {
            if offered != caps {
                return Err(SessionError::Negotiation(format!(
                    "the session is already negotiated with different caps (offered \
                     protocol_version {}, now {}); open a fresh stream to renegotiate",
                    offered.protocol_version, caps.protocol_version
                )));
            }
            return Ok(server);
        }
        match self.call(&Request::Hello(caps))? {
            Reply::Hello(server_caps) => {
                self.negotiated = Some((caps, server_caps));
                Ok(server_caps)
            }
            other => Err(unexpected("hello", &other)),
        }
    }

    fn snapshot(&mut self) -> Result<Snapshot, SessionError> {
        match self.call(&Request::Snapshot)? {
            // The one seal-bound reply (task 127) carries handle + evidence
            // cut + taint. The investigation session consumes the handle and
            // taint; the cut (the seal Moment + included SDK-event count) is
            // campaign-plane evidence the explorer transports — not yet a
            // resolution concern, so it is deliberately not surfaced here.
            Reply::Snapshot { id, tainted, .. } => Ok(Snapshot { id, tainted }),
            other => Err(unexpected("snapshot", &other)),
        }
    }

    fn drop_snap(&mut self, snap: SnapId) -> Result<(), SessionError> {
        match self.call(&Request::Drop(snap))? {
            Reply::Unit => Ok(()),
            other => Err(unexpected("drop", &other)),
        }
    }

    fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), SessionError> {
        match self.call(&Request::Branch {
            snap,
            env: env.clone(),
        })? {
            Reply::Unit => Ok(()),
            other => Err(unexpected("branch", &other)),
        }
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), SessionError> {
        match self.call(&Request::Replay(snap))? {
            Reply::Unit => Ok(()),
            other => Err(unexpected("replay", &other)),
        }
    }

    /// Advance the VM. The guest-observable [`StopReason`] comes back as `Ok`
    /// data — a crash is not an error (the two-categories rule). `resolve` is
    /// never sent: resolution drives no decision-answering loop (that is the
    /// explorer's seam), and the server refuses a resolve with no outstanding
    /// decision rather than absorbing it.
    fn run(&mut self, until: StopConditions) -> Result<StopReason, SessionError> {
        match self.call(&Request::Run {
            until,
            resolve: None,
        })? {
            Reply::Stop(stop) => Ok(stop),
            other => Err(unexpected("run", &other)),
        }
    }

    fn hash(&mut self, scope: HashScope) -> Result<[u8; 32], SessionError> {
        match self.call(&Request::Hash { scope })? {
            Reply::Hash(h) => Ok(h),
            other => Err(unexpected("hash", &other)),
        }
    }

    /// **Observation.** An over-[`READ_CAP`] `len` is refused **before any wire
    /// traffic** (never ship an untrusted length the far end would have to size a
    /// buffer to), and a `Bytes` reply that does not carry **exactly** `len`
    /// bytes is a loud [`SessionError::Transport`] — the wire contract is "never
    /// a truncated success", so a short (or over-long) reply is a broken server,
    /// not a partial read to be papered over.
    fn read(&mut self, gpa: u64, len: u32) -> Result<Vec<u8>, SessionError> {
        if len > READ_CAP {
            return Err(SessionError::ReadTooLarge { len, cap: READ_CAP });
        }
        match self.call(&Request::Read { gpa, len })? {
            Reply::Bytes(b) => {
                if b.len() != len as usize {
                    return Err(SessionError::Transport(format!(
                        "read({gpa:#x}, {len}) answered {} bytes — the wire contract is exactly \
                         {len}, never a truncated success",
                        b.len()
                    )));
                }
                Ok(b)
            }
            other => Err(unexpected("read", &other)),
        }
    }

    /// **Observation.** The versioned register view at the current [`Moment`].
    /// The view's `version` rides through verbatim: it is an *additive* contract
    /// (a bump adds fields, never reshapes one), so a reader that pins an older
    /// shape keeps reading the prefix it knows and must not reject a newer one.
    fn regs(&mut self) -> Result<RegsView, SessionError> {
        match self.call(&Request::Regs)? {
            Reply::Regs(r) => Ok(RegsView {
                version: r.version,
                gpr: r.gpr,
                rip: r.rip,
                rflags: r.rflags,
                seg: r.seg,
                cr0: r.cr0,
                cr3: r.cr3,
                cr4: r.cr4,
                moment: r.moment.0,
                vtime: r.vtime,
            }),
            other => Err(unexpected("regs", &other)),
        }
    }

    /// **Improvisation.** The wire's `ExecResult` carries the output and whether
    /// the command reached its completion sentinel; the **taint bit rides the
    /// `Snapshot` reply**, not this one. An `exec` taints its timeline by ruling
    /// — unconditionally, from the instant the request is issued — so the
    /// reported taint is `true` on every success, and the caller
    /// ([`MaterializedSession::exec`](crate::MaterializedSession::exec)) has
    /// already marked the timeline before the round trip in case the reply is
    /// lost.
    fn exec(
        &mut self,
        cmd: &str,
        deadline: control_proto::Moment,
    ) -> Result<ExecResult, SessionError> {
        match self.call(&Request::Exec {
            cmd: cmd.to_string(),
            deadline,
        })? {
            Reply::ExecResult { output, ok } => Ok(ExecResult {
                output,
                ok,
                tainted: true,
            }),
            other => Err(unexpected("exec", &other)),
        }
    }

    /// Mint the genesis-complete reproducer for the current point. A tainted
    /// timeline comes back as the wire's taint guard, mapped to
    /// [`SessionError::Tainted`] — never a lying [`Reproducer`]. The reply's blob
    /// is untrusted: a `blob_version` this client does not speak is refused
    /// rather than decoded on a guess, and a well-versioned blob that fails
    /// [`EnvSpec::decode`] is a loud transport failure.
    fn recorded_env(&mut self) -> Result<EnvSpec, SessionError> {
        match self.call(&Request::RecordedEnv)? {
            Reply::Recorded(env) => {
                if env.blob_version != EnvSpec::BLOB_VERSION {
                    return Err(SessionError::Transport(format!(
                        "recorded env carries reproducer blob version {} (this client speaks {})",
                        env.blob_version,
                        EnvSpec::BLOB_VERSION
                    )));
                }
                EnvSpec::decode(&env.bytes).map_err(|e| {
                    SessionError::Transport(format!("recorded env failed to decode: {e:?}"))
                })
            }
            other => Err(unexpected("recorded_env", &other)),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit coverage of the pure, stream-free logic: the wire→client error map.
    //! The verb surface, the trust boundary, and the loopback (this adapter
    //! against a frame-speaking server) are `tests/socket_loopback.rs`; the
    //! adapter against vmm-core's **real** control server is campaign-runner's
    //! `tests/resolution_loopback.rs`.

    use super::*;

    #[test]
    fn wire_read_and_taint_errors_become_the_client_s_own_typed_variants() {
        // The three conditions the client models natively map onto the SAME
        // variants MockServer raises, so a consumer cannot tell the two Servers
        // apart by their errors.
        assert_eq!(
            control_error_to_session(ControlError::ReadOutOfRange {
                gpa: 0x1000,
                len: 8,
                ram_len: 0x800,
            }),
            SessionError::ReadOutOfRange {
                gpa: 0x1000,
                len: 8,
                ram_len: 0x800,
            }
        );
        assert_eq!(
            control_error_to_session(ControlError::ReadTooLarge {
                len: 1 << 20,
                cap: READ_CAP,
            }),
            SessionError::ReadTooLarge {
                len: 1 << 20,
                cap: READ_CAP,
            }
        );
        assert_eq!(
            control_error_to_session(ControlError::Tainted),
            SessionError::Tainted
        );
        // Their categories agree with the mock's, too.
        assert_eq!(
            control_error_to_session(ControlError::Tainted).category(),
            "tainted"
        );
    }

    #[test]
    fn every_other_control_error_rides_through_verbatim() {
        for e in [
            ControlError::UnknownSnapshot(SnapId(3)),
            ControlError::NotQuiescent,
            ControlError::RestoreFailed,
            ControlError::Unsupported,
            ControlError::MalformedEnvironment,
            ControlError::BadEnvVersion(9),
            ControlError::Protocol(control_proto::ProtocolError::ShortFrame),
        ] {
            assert_eq!(
                control_error_to_session(e.clone()),
                SessionError::Control(e.clone()),
                "server-originated control errors stay visibly server-originated"
            );
            assert_eq!(control_error_to_session(e).category(), "control");
        }
    }
}
