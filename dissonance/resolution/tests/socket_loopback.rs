// SPDX-License-Identifier: AGPL-3.0-or-later
//! **The socket loopback gate (task 107).** [`SocketServer`] — the production
//! [`Server`] — driven against a frame-speaking server over real `control-proto`
//! bytes: the whole verb surface, the trust boundary, and the hash-neutrality of
//! the observation verbs.
//!
//! ## The two loopbacks, and what runs under Miri
//!
//! The server on the far end is [`FrameServer`]: it decodes real request frames,
//! dispatches them to resolution's own scripted [`MockServer`], and encodes real
//! reply frames. It is reached two ways:
//!
//! - **[`Pipe`] — in-memory, single-threaded, no syscalls.** The stream services
//!   each request inline on `write`, so a `SocketServer` over it exercises the
//!   identical `encode → frame → decode` path with nothing but memory
//!   underneath. **Every test in this file but one runs on it**, so the whole
//!   verb surface and every error path is Miri-reachable.
//! - **A real `UnixStream::pair()`**, server on a spawned thread — the shape the
//!   box gate actually runs. Exactly one test
//!   ([`socket_server_speaks_the_wire_over_a_real_unix_socketpair`]) uses it, and
//!   it is `#[cfg_attr(miri, ignore)]`d because Miri cannot execute socket
//!   syscalls (vmm-core's `serve_speaks_frames_over_an_in_memory_stream`
//!   precedent). Its coverage is not lost under the interpreter: the same verbs
//!   over the same codec are the `Pipe` tests above.
//!
//! The adapter against vmm-core's **real** control server (a live `ControlServer`
//! over a socketpair, portable `MockBackend` guest) is campaign-runner's
//! `tests/resolution_loopback.rs` — this crate cannot depend on vmm-core.

use std::io::{Read, Write};

use control_proto::{
    ControlError, HashScope, Moment as WireMoment, Reply, Reproducer, Request, SnapId,
    StopConditions, StopMask, StopReason,
};
use environment::{EnvCodec, EnvSpec, FaultPolicy};
use proptest::prelude::*;
use resolution::{
    MockServer, MomentRef, READ_CAP, Server, Session, SessionError, SocketServer, client_caps,
};

// ---------------------------------------------------------------------------
// The far end: a frame-speaking server over resolution's scripted MockServer.
// ---------------------------------------------------------------------------

/// How the [`FrameServer`] should misbehave — the hostile/broken peers the
/// adapter's trust boundary exists for. Every one of these must produce a typed
/// [`SessionError`], never a panic and never a silent truncation.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Misbehavior {
    /// A well-behaved server.
    #[default]
    None,
    /// Answer every verb with `Reply::Unit`, whatever was asked — including the
    /// `hello` that opens the session.
    WrongReplyKind,
    /// Negotiate honestly, then answer every *later* verb with `Reply::Unit` —
    /// the reply/verb desync that only shows up mid-session.
    WrongReplyKindAfterHello,
    /// Answer a `read(len)` with `len - 1` bytes — the truncated success the
    /// wire contract forbids.
    ShortBytes,
    /// Stamp the reply with a sequence number that does not echo the request's.
    SeqSkew,
    /// Write bytes that are not a frame at all.
    Garbage,
    /// Say nothing (the client sees EOF where a reply should be) — a peer that
    /// died mid-verb.
    Hangup,
    /// Write the first half of a well-formed reply frame, then hang up.
    TornFrame,
}

/// A control-transport server that speaks real frames: decode a [`Request`],
/// service it against the scripted [`MockServer`], encode the [`Reply`].
struct FrameServer {
    guest: MockServer,
    misbehave: Misbehavior,
    /// The scripted SDK-event capture the `SdkEvents` verb pages out.
    events: Vec<(u64, u32, Vec<u8>)>,
    /// How many events one `SdkEvents` page carries — the real server bounds a
    /// page by the frame limit; a small number here proves the client *pages*
    /// rather than taking the first page for the whole capture.
    page: usize,
    /// Every request the server saw, in order — the wire-level record the
    /// hash-neutrality and no-traffic assertions are made against.
    log: Vec<Request>,
}

impl FrameServer {
    fn new(guest: MockServer) -> Self {
        FrameServer {
            guest,
            misbehave: Misbehavior::None,
            events: Vec::new(),
            page: 8,
            log: Vec::new(),
        }
    }

    fn misbehaving(guest: MockServer, misbehave: Misbehavior) -> Self {
        FrameServer {
            misbehave,
            ..Self::new(guest)
        }
    }

    /// Service one decoded request. The nested result mirrors the wire: an inner
    /// `Err(ControlError)` is a reply frame, not a dead session.
    fn handle(&mut self, req: &Request) -> Result<Reply, ControlError> {
        self.log.push(req.clone());
        let short_bytes = self.misbehave == Misbehavior::ShortBytes;
        match req {
            Request::Hello(caps) => self.guest.hello(*caps).map(Reply::Hello),
            Request::Snapshot => self.guest.snapshot().map(|s| {
                // The wire keeps the taint-free reply for an untainted capture.
                if s.tainted {
                    Reply::Snapshot {
                        id: s.id,
                        tainted: true,
                    }
                } else {
                    Reply::SnapId(s.id)
                }
            }),
            Request::Drop(snap) => self.guest.drop_snap(*snap).map(|()| Reply::Unit),
            Request::Branch { snap, env } => self.guest.branch(*snap, env).map(|()| Reply::Unit),
            Request::Replay(snap) => self.guest.replay(*snap).map(|()| Reply::Unit),
            Request::Run { until, .. } => self.guest.run(*until).map(Reply::Stop),
            Request::Hash { scope } => self.guest.hash(*scope).map(Reply::Hash),
            Request::Read { gpa, len } => self.guest.read(*gpa, *len).map(|b| {
                if short_bytes {
                    Reply::Bytes(b[..b.len().saturating_sub(1)].to_vec())
                } else {
                    Reply::Bytes(b)
                }
            }),
            Request::Regs => self.guest.regs().map(|r| {
                Reply::Regs(control_proto::RegsView {
                    version: r.version,
                    gpr: r.gpr,
                    rip: r.rip,
                    rflags: r.rflags,
                    seg: r.seg,
                    cr0: r.cr0,
                    cr3: r.cr3,
                    cr4: r.cr4,
                    moment: WireMoment(r.moment),
                    vtime: r.vtime,
                })
            }),
            Request::Exec { cmd, deadline } => {
                self.guest.exec(cmd, *deadline).map(|e| Reply::ExecResult {
                    output: e.output,
                    ok: e.ok,
                })
            }
            Request::RecordedEnv => self.guest.recorded_env().map(|spec| {
                Reply::Recorded(Reproducer {
                    blob_version: EnvSpec::BLOB_VERSION,
                    bytes: spec.encode(),
                })
            }),
            // Paged, exactly like the real server: a page starts at `offset` and
            // is bounded, so the client must keep asking until an empty page.
            Request::SdkEvents { offset } => {
                let start = (*offset as usize).min(self.events.len());
                let end = start.saturating_add(self.page).min(self.events.len());
                Ok(Reply::SdkEvents(self.events[start..end].to_vec()))
            }
            // Not part of resolution's seam.
            Request::Console { .. } | Request::Perturb { .. } => {
                Err(SessionError::Control(ControlError::Unsupported))
            }
        }
        // The guest is a `Server`, so it speaks the client's error type; the wire
        // speaks `ControlError`. Map back — the inverse of the adapter's own map,
        // so a round trip through the socket must land on the identical variant.
        .map_err(session_error_to_wire)
    }
}

/// The server-side inverse of the adapter's wire→client error map.
fn session_error_to_wire(e: SessionError) -> ControlError {
    match e {
        SessionError::Control(c) => c,
        SessionError::Tainted => ControlError::Tainted,
        SessionError::ReadOutOfRange { gpa, len, ram_len } => {
            ControlError::ReadOutOfRange { gpa, len, ram_len }
        }
        SessionError::ReadTooLarge { len, cap } => ControlError::ReadTooLarge { len, cap },
        // A real server raises none of these (they are client-local categories).
        SessionError::NothingOpen | SessionError::Negotiation(_) | SessionError::Transport(_) => {
            ControlError::Unsupported
        }
    }
}

/// An in-memory duplex that services each request **inline** on `write`: no
/// socket, no thread, no syscall — so the whole wire path runs under Miri.
struct Pipe {
    srv: FrameServer,
    /// Request bytes the client has written but that do not yet form a frame.
    inbuf: Vec<u8>,
    /// Reply bytes waiting for the client to read.
    outbox: Vec<u8>,
    /// How far into `outbox` the client has read.
    taken: usize,
}

impl Pipe {
    fn new(srv: FrameServer) -> Self {
        Pipe {
            srv,
            inbuf: Vec::new(),
            outbox: Vec::new(),
            taken: 0,
        }
    }
}

impl Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inbuf.extend_from_slice(buf);
        // Service every complete request frame the client has now written.
        while let Some((seq, req, consumed)) =
            control_proto::decode_request(&self.inbuf).expect("the client emits valid frames")
        {
            self.inbuf.drain(..consumed);
            let reply = self.srv.handle(&req);
            let mangle = match self.srv.misbehave {
                Misbehavior::WrongReplyKind => true,
                Misbehavior::WrongReplyKindAfterHello => !matches!(req, Request::Hello(_)),
                _ => false,
            };
            let reply = if mangle {
                reply.map(|_| Reply::Unit)
            } else {
                reply
            };
            match self.srv.misbehave {
                Misbehavior::Hangup => {} // say nothing: the client reads EOF
                Misbehavior::Garbage => {
                    self.outbox.extend_from_slice(b"not a control frame at all")
                }
                Misbehavior::SeqSkew => {
                    control_proto::encode_reply(seq.wrapping_add(1), &reply, &mut self.outbox)
                        .expect("encode reply");
                }
                Misbehavior::TornFrame => {
                    let mut frame = Vec::new();
                    control_proto::encode_reply(seq, &reply, &mut frame).expect("encode reply");
                    let half = frame.len() / 2;
                    self.outbox.extend_from_slice(&frame[..half]);
                }
                _ => control_proto::encode_reply(seq, &reply, &mut self.outbox)
                    .expect("encode reply"),
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let available = &self.outbox[self.taken..];
        // Nothing left to hand over: EOF — the peer has closed (or never spoke).
        let n = available.len().min(buf.len());
        buf[..n].copy_from_slice(&available[..n]);
        self.taken += n;
        Ok(n)
    }
}

/// A stream that answers every request with `bytes` (then EOF) — the hostile
/// peer of the totality proptest. Writes are swallowed.
struct RawReplyStream {
    bytes: Vec<u8>,
    taken: usize,
}

impl Write for RawReplyStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Read for RawReplyStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let available = &self.bytes[self.taken.min(self.bytes.len())..];
        let n = available.len().min(buf.len());
        buf[..n].copy_from_slice(&available[..n]);
        self.taken += n;
        Ok(n)
    }
}

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

/// The env the scripted guest boots under.
fn boot_env() -> EnvSpec {
    EnvCodec::seeded(0xB0_07, FaultPolicy::none())
}

/// A distinct env to branch a timeline with (a different world than the boot).
fn branch_env() -> EnvSpec {
    EnvCodec::seeded(0xC0_FF_EE, FaultPolicy::none())
}

fn wire(spec: &EnvSpec) -> Reproducer {
    Reproducer {
        blob_version: EnvSpec::BLOB_VERSION,
        bytes: spec.encode(),
    }
}

/// A `SocketServer` over an in-memory `Pipe` to a well-behaved frame server.
fn loopback() -> SocketServer<Pipe> {
    SocketServer::new(Pipe::new(FrameServer::new(MockServer::boot(boot_env()))))
}

fn misbehaving(m: Misbehavior) -> SocketServer<Pipe> {
    SocketServer::new(Pipe::new(FrameServer::misbehaving(
        MockServer::boot(boot_env()),
        m,
    )))
}

/// Negotiate + snapshot genesis + branch a timeline onto it — the shape every
/// verb test starts from.
fn opened(adapter: &mut SocketServer<Pipe>) -> SnapId {
    adapter.hello(client_caps()).expect("hello");
    let genesis = adapter.snapshot().expect("genesis snapshot").id;
    adapter
        .branch(genesis, &wire(&branch_env()))
        .expect("branch");
    genesis
}

fn run_to(adapter: &mut SocketServer<Pipe>, moment: u64) -> StopReason {
    adapter
        .run(StopConditions {
            deadline: Some(WireMoment(moment)),
            on: StopMask::NONE,
        })
        .expect("run")
}

// ---------------------------------------------------------------------------
// The verb surface, over real frames.
// ---------------------------------------------------------------------------

/// Every verb round-trips through the codec and lands on the same observable a
/// caller would get by driving the scripted guest **in process** — the seam's
/// whole claim: a `Session` cannot tell the two `Server`s apart.
#[test]
fn every_verb_over_the_wire_matches_the_in_process_server() {
    let mut wired = loopback();
    let mut direct = MockServer::boot(boot_env());

    // hello
    let wired_caps = wired.hello(client_caps()).expect("wire hello");
    let direct_caps = direct.hello(client_caps()).expect("direct hello");
    assert_eq!(wired_caps, direct_caps);
    assert_eq!(wired.server_caps(), Some(wired_caps));

    // snapshot (untainted → the taint-free wire reply)
    let w_genesis = wired.snapshot().expect("wire snapshot");
    let d_genesis = direct.snapshot().expect("direct snapshot");
    assert_eq!(w_genesis, d_genesis);
    assert!(!w_genesis.tainted);

    // branch + run
    let env = wire(&branch_env());
    wired.branch(w_genesis.id, &env).expect("wire branch");
    direct.branch(d_genesis.id, &env).expect("direct branch");
    let until = StopConditions {
        deadline: Some(WireMoment(4_242)),
        on: StopMask::NONE,
    };
    assert_eq!(
        wired.run(until).expect("wire run"),
        direct.run(until).expect("direct run")
    );

    // hash / read / regs — the observation verbs
    assert_eq!(
        wired.hash(HashScope::Whole).expect("wire hash"),
        direct.hash(HashScope::Whole).expect("direct hash")
    );
    assert_eq!(
        wired.read(0x1000, 64).expect("wire read"),
        direct.read(0x1000, 64).expect("direct read")
    );
    assert_eq!(wired.read(0x1000, 64).expect("wire read").len(), 64);
    assert_eq!(
        wired.regs().expect("wire regs"),
        direct.regs().expect("direct regs")
    );

    // recorded_env — the reproducer mint, on an untainted timeline
    assert_eq!(
        wired.recorded_env().expect("wire recorded_env"),
        direct.recorded_env().expect("direct recorded_env")
    );

    // replay + snapshot + drop
    let w_mid = wired.snapshot().expect("wire mid snapshot");
    let d_mid = direct.snapshot().expect("direct mid snapshot");
    assert_eq!(w_mid, d_mid);
    wired.replay(w_mid.id).expect("wire replay");
    direct.replay(d_mid.id).expect("direct replay");
    assert_eq!(
        wired.hash(HashScope::Whole).expect("wire hash"),
        direct.hash(HashScope::Whole).expect("direct hash")
    );
    wired.drop_snap(w_mid.id).expect("wire drop");
    direct.drop_snap(d_mid.id).expect("direct drop");

    // exec — the improvisation, and the taint it leaves
    let deadline = WireMoment(9_999_999);
    let w_exec = wired.exec("uname -a", deadline).expect("wire exec");
    let d_exec = direct.exec("uname -a", deadline).expect("direct exec");
    assert_eq!(w_exec, d_exec);
    assert!(w_exec.tainted, "an exec taints its timeline by ruling");
    // The taint now rides the wire's taint-carrying snapshot reply…
    let w_tainted = wired
        .snapshot()
        .expect("wire snapshot of a tainted timeline");
    assert!(w_tainted.tainted);
    // …and the mint refuses, as the taint guard's typed error.
    assert_eq!(wired.recorded_env(), Err(SessionError::Tainted));
}

/// A full [`Session`] over the socket: negotiate, materialize a [`MomentRef`],
/// and drive the observation verbs — the stack the film gate runs.
#[test]
fn a_session_over_the_socket_materializes_and_observes() {
    let mut session = Session::connect(loopback()).expect("connect over the wire");
    let mref = MomentRef::new(branch_env(), 5_000);

    let mut mat = session.materialize(&mref).expect("materialize");
    assert_eq!(mat.moment(), 5_000);
    assert!(matches!(mat.stop(), StopReason::Deadline { .. }));
    let regs = mat.regs().expect("regs");
    assert_eq!(regs.moment, 5_000, "the view is of the landed moment");
    assert_eq!(regs.version, resolution::RegsView::VERSION);
    let bytes = mat.read(0x2000, 128).expect("read");
    assert_eq!(bytes.len(), 128);
    let hash = mat.hash().expect("hash");

    // Determinism through the socket: the same address materializes to the same
    // state, bit for bit.
    let mut again = session.materialize(&mref).expect("re-materialize");
    assert_eq!(again.hash().expect("hash"), hash);
    assert_eq!(again.read(0x2000, 128).expect("read"), bytes);
    assert_eq!(again.regs().expect("regs"), regs);
}

// ---------------------------------------------------------------------------
// Hash-neutrality: observation stays observation (deliverable 4).
// ---------------------------------------------------------------------------

/// **The observation verbs are inert.** `read`/`regs`/`hash` over the wire touch
/// neither the position, the state hash, nor the recorded reproducer — proven
/// twice over: the observed state is identical before and after an arbitrary
/// burst of them, and the reproducer the timeline mints is unchanged.
///
/// This is the socket half of the PR-51/task-80 observation-inertness line: the
/// server-side proof (a `state_hash` identical across interleaved `Read`/`Regs`)
/// is vmm-core's; this proves the *client* adds no contact of its own — no
/// hidden `run`, no re-branch, nothing that would touch a draw stream.
#[test]
fn observation_verbs_over_the_wire_are_hash_neutral() {
    let mut adapter = loopback();
    opened(&mut adapter);
    run_to(&mut adapter, 7_777);

    let before_hash = adapter.hash(HashScope::Whole).expect("hash");
    let before_regs = adapter.regs().expect("regs");
    let before_env = adapter.recorded_env().expect("recorded_env");

    // An arbitrary burst of observations, interleaved.
    for i in 0..16u64 {
        let _ = adapter.read(i * 0x100, 32).expect("read");
        let _ = adapter.regs().expect("regs");
        let _ = adapter.hash(HashScope::Whole).expect("hash");
    }

    assert_eq!(
        adapter.hash(HashScope::Whole).expect("hash"),
        before_hash,
        "observation moved the state hash"
    );
    assert_eq!(
        adapter.regs().expect("regs"),
        before_regs,
        "observation moved the position"
    );
    assert_eq!(
        adapter.recorded_env().expect("recorded_env"),
        before_env,
        "observation was recorded into the reproducer"
    );
}

/// The wire-level twin of the above: an observation emits **only** its own
/// request. The client sends no hidden `run`, no re-`branch`, nothing that could
/// advance a timeline or pull a draw — so the server's request log with the
/// observations filtered out is exactly the log of the navigation alone.
#[test]
fn an_observation_emits_only_its_own_request_frame() {
    let mut adapter = loopback();
    let genesis = opened(&mut adapter);
    run_to(&mut adapter, 1_234);
    let _ = adapter.read(0, 16).expect("read");
    let _ = adapter.regs().expect("regs");
    let _ = adapter.hash(HashScope::Whole).expect("hash");

    let log = &adapter.into_inner().srv.log;
    let navigation: Vec<&Request> = log
        .iter()
        .filter(|r| {
            !matches!(
                r,
                Request::Read { .. } | Request::Regs | Request::Hash { .. }
            )
        })
        .collect();
    assert_eq!(
        navigation,
        vec![
            &Request::Hello(client_caps()),
            &Request::Snapshot,
            &Request::Branch {
                snap: genesis,
                env: wire(&branch_env()),
            },
            &Request::Run {
                until: StopConditions {
                    deadline: Some(WireMoment(1_234)),
                    on: StopMask::NONE,
                },
                resolve: None,
            },
        ],
        "the observations added exactly nothing to the navigation stream"
    );
    // And each observation is one frame, not a re-position followed by a read.
    assert_eq!(log.iter().filter(|r| matches!(r, Request::Regs)).count(), 1);
    assert_eq!(
        log.iter()
            .filter(|r| matches!(r, Request::Read { .. }))
            .count(),
        1
    );
}

// ---------------------------------------------------------------------------
// Rooting: `connect_rooted` (the honest replacement for a lying `snapshot()`).
// ---------------------------------------------------------------------------

/// `connect_rooted` branches off the snapshot it is **given** and takes none of
/// its own — so a caller that scraped absolute `Moment`s from a run rooted at a
/// pre-taken base (the film gate) materializes from that same base.
#[test]
fn connect_rooted_roots_at_the_given_snapshot_and_takes_none_of_its_own() {
    let mut adapter = loopback();
    let base = opened(&mut adapter);
    run_to(&mut adapter, 2_000);

    let mut session = Session::connect_rooted(&mut adapter, base).expect("connect_rooted");
    let mat = session
        .materialize(&MomentRef::new(branch_env(), 3_000))
        .expect("materialize");
    assert_eq!(mat.moment(), 3_000);
    drop(session);

    let log = &adapter.into_inner().srv.log;
    // Exactly one `Snapshot` frame ever crossed the wire — the caller's own. The
    // session took none, and `hello` was not sent twice.
    assert_eq!(
        log.iter()
            .filter(|r| matches!(r, Request::Snapshot))
            .count(),
        1,
        "connect_rooted must not snapshot"
    );
    assert_eq!(
        log.iter()
            .filter(|r| matches!(r, Request::Hello(_)))
            .count(),
        1,
        "hello is negotiated once per stream"
    );
    // The materialize branched off the base the caller handed in.
    assert!(
        log.iter()
            .any(|r| matches!(r, Request::Branch { snap, .. } if *snap == base)),
        "the session branched off the given root"
    );
}

/// `hello` is negotiated **once per stream**: a second call answers from the
/// cache without a frame, so a raw pre-pass and a `Session` over the same
/// adapter share one session — and offering *different* caps afterwards is a
/// loud negotiation error, never a stale answer.
#[test]
fn hello_negotiates_once_per_stream() {
    let mut adapter = loopback();
    let first = adapter.hello(client_caps()).expect("hello");
    let second = adapter.hello(client_caps()).expect("second hello");
    assert_eq!(first, second);

    let mut odd = client_caps();
    odd.protocol_version = client_caps().protocol_version.wrapping_add(1);
    let err = adapter.hello(odd).expect_err("renegotiation must be loud");
    assert_eq!(err.category(), "negotiation");

    assert_eq!(
        adapter
            .into_inner()
            .srv
            .log
            .iter()
            .filter(|r| matches!(r, Request::Hello(_)))
            .count(),
        1,
        "exactly one hello frame crossed the wire"
    );
}

// ---------------------------------------------------------------------------
// SDK events: paged (the bug the test-local adapter carried).
// ---------------------------------------------------------------------------

/// The SDK-event capture is **paged**: the client keeps asking until an empty
/// page, so a capture longer than one page arrives whole. A single `offset: 0`
/// call would silently return the first page and call it the capture — the film
/// gate's frame clock would then simply stop early.
#[test]
fn sdk_events_pages_until_the_capture_is_drained() {
    let events: Vec<(u64, u32, Vec<u8>)> = (0..21u64)
        .map(|i| (i * 100, i as u32, vec![i as u8; 4]))
        .collect();
    let mut srv = FrameServer::new(MockServer::boot(boot_env()));
    srv.events = events.clone();
    srv.page = 4;
    let mut adapter = SocketServer::new(Pipe::new(srv));
    adapter.hello(client_caps()).expect("hello");

    assert_eq!(
        adapter.sdk_events().expect("sdk_events"),
        events,
        "the capture arrives whole, in order — not just its first page"
    );

    let pages = adapter
        .into_inner()
        .srv
        .log
        .iter()
        .filter(|r| matches!(r, Request::SdkEvents { .. }))
        .count();
    // 21 events over 4-event pages: five full pages, a sixth carrying the last
    // one, then the empty page that ends the drain.
    assert_eq!(pages, 7, "the client paged the capture out");
}

/// An empty capture (a guest with no SDK) drains in one page and is not an error.
#[test]
fn sdk_events_on_a_guest_with_no_sdk_is_empty() {
    let mut adapter = loopback();
    adapter.hello(client_caps()).expect("hello");
    assert_eq!(adapter.sdk_events().expect("sdk_events"), vec![]);
}

// ---------------------------------------------------------------------------
// The trust boundary: every hostile/broken peer is a typed error, never a panic.
// ---------------------------------------------------------------------------

/// An over-cap `read` is refused **before any wire traffic** — an untrusted
/// length never reaches the far end, let alone sizes a buffer there.
#[test]
fn an_over_cap_read_is_refused_before_any_wire_traffic() {
    let mut adapter = loopback();
    adapter.hello(client_caps()).expect("hello");
    assert_eq!(
        adapter.read(0, READ_CAP + 1),
        Err(SessionError::ReadTooLarge {
            len: READ_CAP + 1,
            cap: READ_CAP,
        })
    );
    assert_eq!(
        adapter.read(0, u32::MAX),
        Err(SessionError::ReadTooLarge {
            len: u32::MAX,
            cap: READ_CAP,
        })
    );
    // Nothing but the `hello` ever reached the far end: the untrusted length was
    // refused on this side of the wire, so it never sized a buffer on that side.
    let log = adapter.into_inner().srv.log;
    assert_eq!(log, vec![Request::Hello(client_caps())]);
}

/// A `read` past guest RAM comes back as the client's **own** typed variant —
/// the same error `MockServer` raises in-process, so a consumer's match arms do
/// not depend on which `Server` it holds. Never a short read.
#[test]
fn an_out_of_range_read_arrives_as_the_typed_range_error() {
    let mut adapter = loopback();
    opened(&mut adapter);
    let ram = resolution::DEFAULT_RAM_BYTES;
    assert_eq!(
        adapter.read(ram - 4, 64),
        Err(SessionError::ReadOutOfRange {
            gpa: ram - 4,
            len: 64,
            ram_len: ram,
        })
    );
    // The wire's `u64` end-of-range arithmetic cannot be wrapped into a success.
    assert!(matches!(
        adapter.read(u64::MAX, 8),
        Err(SessionError::ReadOutOfRange { .. })
    ));
}

/// A server that answers a `read(len)` with fewer than `len` bytes is **broken**,
/// and the client says so: the wire contract is "exactly `len`, never a truncated
/// success", so the short reply is a loud transport failure rather than bytes the
/// caller would go on to decode.
#[test]
fn a_short_bytes_reply_is_refused_never_a_truncated_read() {
    let mut adapter = misbehaving(Misbehavior::ShortBytes);
    adapter.hello(client_caps()).expect("hello");
    let err = adapter.read(0, 64).expect_err("a short read must be loud");
    assert_eq!(err.category(), "transport");
    assert!(
        format!("{err}").contains("never a truncated success"),
        "the message names the contract it broke: {err}"
    );
}

/// A reply that does not answer the verb that was sent aborts the verb loudly —
/// the session is out of step with the server, and guessing would mis-attribute
/// state to the wrong request.
#[test]
fn an_unexpected_reply_kind_is_refused() {
    // Even `hello` — the reply is `Unit`, not `Hello`.
    let mut adapter = misbehaving(Misbehavior::WrongReplyKind);
    let err = adapter.hello(client_caps()).expect_err("wrong reply kind");
    assert_eq!(err.category(), "transport");

    // And mid-session, where a server that negotiated honestly then desyncs: the
    // verb that got the wrong reply fails; it does not silently take `Unit` for
    // an answer.
    let mut adapter = misbehaving(Misbehavior::WrongReplyKindAfterHello);
    adapter.hello(client_caps()).expect("hello is honest");
    assert_eq!(
        adapter.snapshot().map(|_| ()).unwrap_err().category(),
        "transport"
    );
    assert_eq!(
        adapter.read(0, 8).map(|_| ()).unwrap_err().category(),
        "transport"
    );
    assert_eq!(
        adapter.regs().map(|_| ()).unwrap_err().category(),
        "transport"
    );
}

/// A reply whose sequence number does not echo the request's is refused — a
/// client that accepted it would pair a reply with the wrong verb.
#[test]
fn a_reply_that_does_not_echo_the_request_seq_is_refused() {
    let mut adapter = misbehaving(Misbehavior::SeqSkew);
    let err = adapter.hello(client_caps()).expect_err("seq skew");
    assert_eq!(err.category(), "transport");
    assert!(format!("{err}").contains("does not echo"), "{err}");
}

/// A peer that dies mid-verb (EOF where a reply should be) is a typed transport
/// failure — the film projector's recoverable "dropped session", not a hang and
/// not a panic.
#[test]
fn a_disconnect_mid_verb_is_a_transport_error() {
    let mut adapter = misbehaving(Misbehavior::Hangup);
    let err = adapter.hello(client_caps()).expect_err("hangup");
    assert_eq!(err.category(), "transport");
    assert!(format!("{err}").contains("closed the stream"), "{err}");
}

/// Half a frame followed by EOF is the same story — the client never blocks
/// forever on the missing half and never decodes a partial body.
#[test]
fn a_torn_frame_is_a_transport_error() {
    let mut adapter = misbehaving(Misbehavior::TornFrame);
    assert_eq!(
        adapter.hello(client_caps()).unwrap_err().category(),
        "transport"
    );
}

/// Bytes that are not a frame at all are a typed framing failure — the codec is
/// the trust boundary and it refuses them without ever indexing past the buffer.
#[test]
fn garbage_on_the_wire_is_a_framing_error() {
    let mut adapter = misbehaving(Misbehavior::Garbage);
    let err = adapter.hello(client_caps()).expect_err("garbage");
    assert_eq!(err.category(), "transport");
    assert!(format!("{err}").contains("framing"), "{err}");
}

/// A reproducer blob in a schema this client does not speak is refused rather
/// than decoded on a guess.
#[test]
fn a_recorded_env_in_an_unknown_blob_schema_is_refused() {
    // The frame server is honest; the *blob* is from the future.
    struct FutureBlob {
        out: Vec<u8>,
        taken: usize,
    }
    impl Write for FutureBlob {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let (seq, _req, _n) = control_proto::decode_request(buf).unwrap().unwrap();
            let reply: Result<Reply, ControlError> = Ok(Reply::Recorded(Reproducer {
                blob_version: EnvSpec::BLOB_VERSION + 1,
                bytes: vec![0xFF; 8],
            }));
            control_proto::encode_reply(seq, &reply, &mut self.out).unwrap();
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Read for FutureBlob {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let available = &self.out[self.taken..];
            let n = available.len().min(buf.len());
            buf[..n].copy_from_slice(&available[..n]);
            self.taken += n;
            Ok(n)
        }
    }
    let mut adapter = SocketServer::new(FutureBlob {
        out: Vec::new(),
        taken: 0,
    });
    let err = adapter.recorded_env().expect_err("an unknown blob schema");
    assert_eq!(err.category(), "transport");
    assert!(format!("{err}").contains("blob version"), "{err}");
}

proptest! {
    // `failure_persistence: None` so this runs **under Miri**: proptest's default
    // regression file is resolved against the cwd, and `getcwd` is unavailable
    // under Miri's isolation (the nightly job runs `-Zmiri-permissive-provenance`
    // only — no `-Zmiri-disable-isolation`). Losing the regression file costs a
    // dev convenience; running a totality fuzz under the interpreter — which
    // catches an out-of-bounds read that returns *plausible* bytes, as a value
    // assertion cannot — is worth more. Cases are cut under Miri (10–100× slower).
    #![proptest_config(ProptestConfig {
        failure_persistence: None,
        ..ProptestConfig::with_cases(if cfg!(miri) { 16 } else { 256 })
    })]

    /// **Totality against a hostile peer** (conventions rule 4): whatever bytes
    /// come back — truncated frames, over-long length fields, junk — the verb
    /// returns, and it never panics, never hangs, and never reads out of bounds.
    /// The one thing it may not do is the one thing a fuzz target checks for.
    #[test]
    fn no_reply_bytes_can_panic_the_client(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let mut adapter = SocketServer::new(RawReplyStream { bytes: bytes.clone(), taken: 0 });
        let _ = adapter.hello(client_caps());
        let mut adapter = SocketServer::new(RawReplyStream { bytes: bytes.clone(), taken: 0 });
        let _ = adapter.snapshot();
        let mut adapter = SocketServer::new(RawReplyStream { bytes: bytes.clone(), taken: 0 });
        let _ = adapter.read(0, 64);
        let mut adapter = SocketServer::new(RawReplyStream { bytes: bytes.clone(), taken: 0 });
        let _ = adapter.regs();
        let mut adapter = SocketServer::new(RawReplyStream { bytes: bytes.clone(), taken: 0 });
        let _ = adapter.recorded_env();
        let mut adapter = SocketServer::new(RawReplyStream { bytes, taken: 0 });
        let _ = adapter.run(StopConditions { deadline: Some(WireMoment(1)), on: StopMask::NONE });
    }
}

// ---------------------------------------------------------------------------
// The real socket (the box gate's shape).
// ---------------------------------------------------------------------------

/// The same adapter over a **real** `UnixStream` socketpair, server on a spawned
/// thread — the composition the box gate runs (and the one the in-memory `Pipe`
/// above stands in for everywhere else).
///
/// Miri cannot execute socket syscalls, so this one test is skipped under the
/// interpreter (vmm-core's `serve_speaks_frames_over_an_in_memory_stream`
/// precedent). Nothing is lost there: the verb surface, the codec path, and every
/// error path above run on `Pipe`, which is pure memory — in particular
/// [`every_verb_over_the_wire_matches_the_in_process_server`] is this test's
/// Miri-run sibling.
#[test]
#[cfg_attr(
    miri,
    ignore = "drives a real UnixStream socketpair across threads; Miri cannot execute the socket \
              syscalls. Miri-run sibling: every_verb_over_the_wire_matches_the_in_process_server \
              (the same verbs, the same codec, over an in-memory Pipe)"
)]
fn socket_server_speaks_the_wire_over_a_real_unix_socketpair() {
    use std::os::unix::net::UnixStream;

    let (client_end, mut server_end) = UnixStream::pair().expect("socketpair");
    let server = std::thread::spawn(move || {
        let mut srv = FrameServer::new(MockServer::boot(boot_env()));
        let mut inbuf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            while let Some((seq, req, consumed)) =
                control_proto::decode_request(&inbuf).expect("client frames decode")
            {
                inbuf.drain(..consumed);
                let reply = srv.handle(&req);
                let mut out = Vec::new();
                control_proto::encode_reply(seq, &reply, &mut out).expect("encode reply");
                server_end.write_all(&out).expect("write reply");
                server_end.flush().expect("flush");
            }
            let n = server_end.read(&mut chunk).expect("read request");
            if n == 0 {
                return; // the client hung up
            }
            inbuf.extend_from_slice(&chunk[..n]);
        }
    });

    // A session over the real socket: connect, materialize, observe.
    let mut session = Session::connect(SocketServer::new(client_end)).expect("connect");
    let mref = MomentRef::new(branch_env(), 6_000);
    let mut mat = session.materialize(&mref).expect("materialize");
    assert_eq!(mat.moment(), 6_000);
    let hash = mat.hash().expect("hash");
    let bytes = mat.read(0x3000, 256).expect("read");
    assert_eq!(bytes.len(), 256);
    assert_eq!(mat.regs().expect("regs").moment, 6_000);

    // Determinism over a real socket: the same address, the same state.
    let mut again = session.materialize(&mref).expect("re-materialize");
    assert_eq!(again.hash().expect("hash"), hash);
    assert_eq!(again.read(0x3000, 256).expect("read"), bytes);

    // The observation verbs left the state where they found it.
    assert_eq!(again.hash().expect("hash"), hash);

    drop(session); // closes the client end → the server loop sees EOF and returns
    server.join().expect("the server thread finished cleanly");
}
