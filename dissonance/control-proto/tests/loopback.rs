// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 6 — loopback. An in-process `Reply`-returning server stub driven by an
//! in-process client over a `Vec<u8>` pipe exercises every verb; two identical
//! sessions produce byte-identical transcripts.
//!
//! This stands in for the frontier socket server (vmm-core): it only decodes
//! requests, picks a canned reply, and encodes it — the wire codec is the whole
//! point under test. The stub keeps the minimum state needed to make the
//! `resolve`-without-`Decision` path real (`environment::Answer` is opaque here).

use control_proto::{
    Answer, CapFlags, Caps, ControlError, CoverageGeometry, DecisionId, HashScope, HostFault,
    Moment, PROTO_VERSION, RegsView, Reply, Reproducer, Request, SnapId, StopConditions, StopMask,
    StopReason, class_bit, decode_reply, decode_request, encode_reply, encode_request,
};

fn caps() -> Caps {
    Caps {
        protocol_version: PROTO_VERSION,
        env_version_min: 1,
        env_version_max: 1,
        coverage: CoverageGeometry {
            map_bytes: 4096,
            producer: 1,
        },
        flags: CapFlags::GUEST_HAS_SDK,
    }
}

fn conds() -> StopConditions {
    StopConditions {
        deadline: Some(Moment(10_000)),
        on: StopMask::NONE
            .arm(class_bit::BLOCK_IO)
            .arm(class_bit::NET_SEND),
    }
}

/// A minimal backend stub. The only state is the snapshot counter and whether a
/// decision is currently outstanding (single-vCPU ⇒ at most one).
struct StubServer {
    next_snap: u64,
    armed: bool,
    /// Task 81: whether an `exec` improvisation has tainted the timeline. Drives
    /// the taint-carrying `Snapshot` reply and the `RecordedEnv` guard so the
    /// loopback crosses those wire shapes too.
    tainted: bool,
}

impl StubServer {
    fn new() -> Self {
        Self {
            next_snap: 0,
            armed: false,
            tainted: false,
        }
    }

    fn handle(&mut self, req: &Request) -> Result<Reply, ControlError> {
        match req {
            Request::Hello(_) => Ok(Reply::Hello(caps())),
            Request::Snapshot => {
                let id = self.next_snap;
                self.next_snap += 1;
                // A tainted timeline surfaces the taint-carrying reply (task 81);
                // an untainted one keeps the pre-81 taint-free `SnapId`.
                if self.tainted {
                    Ok(Reply::Snapshot {
                        id: SnapId(id),
                        tainted: true,
                    })
                } else {
                    Ok(Reply::SnapId(SnapId(id)))
                }
            }
            Request::Drop(_)
            | Request::Branch { .. }
            | Request::Replay(_)
            | Request::Perturb { .. } => Ok(Reply::Unit),
            Request::Run { resolve, .. } => {
                if resolve.is_some() {
                    // A resolve with no outstanding decision is a loud error,
                    // never silently dropped (it would desync the DecisionId).
                    if !self.armed {
                        return Err(ControlError::ResolveWithoutDecision);
                    }
                    self.armed = false;
                    Ok(Reply::Stop(StopReason::Quiescent { vtime: Moment(500) }))
                } else {
                    // First run surfaces and arms a decision.
                    self.armed = true;
                    Ok(Reply::Stop(StopReason::Decision {
                        vtime: Moment(100),
                        id: DecisionId(1),
                        ctx: vec![0xAB],
                    }))
                }
            }
            Request::Hash { .. } => Ok(Reply::Hash([0x42; 32])),
            Request::SdkEvents { .. } => Ok(Reply::SdkEvents(vec![
                (10, 0x0100_0001, vec![1, 2, 3]),
                (20, 0x0000_0000, vec![]),
            ])),
            Request::Console { offset } => {
                let serial = b"ORDER_READY\nphase\n".as_slice();
                let start = (*offset as usize).min(serial.len());
                Ok(Reply::Console {
                    total: serial.len() as u32,
                    chunk: serial[start..].to_vec(),
                })
            }
            // Observation verbs (task 80): a stubbed guest returns `len` bytes and
            // a fixed register view — the point here is that they cross the wire
            // and round-trip, not that the bytes are a real guest's.
            &Request::Read { len, .. } => Ok(Reply::Bytes(vec![0xAB; len as usize])),
            Request::Regs => Ok(Reply::Regs(RegsView {
                version: RegsView::VERSION,
                gpr: [0; 16],
                rip: 0xDEAD_BEEF,
                rflags: 0x2,
                seg: [0x10, 0x18, 0x18, 0x18, 0, 0],
                cr0: 0x8000_0011,
                cr3: 0x3000,
                cr4: 0x20,
                moment: Moment(500),
                vtime: 500,
            })),
            // Improvisation (task 81): `exec` taints the timeline and returns the
            // crude serial capture; the server refuses nothing.
            Request::Exec { .. } => {
                self.tainted = true;
                Ok(Reply::ExecResult {
                    output: b"root@guest:/# ".to_vec(),
                    ok: true,
                })
            }
            // The reproducer mint is the taint guard's fail-loud site: a tainted
            // timeline is a loud `Tainted`, never a lying `Reproducer`.
            Request::RecordedEnv => {
                if self.tainted {
                    Err(ControlError::Tainted)
                } else {
                    Ok(Reply::Recorded(Reproducer {
                        blob_version: 1,
                        bytes: vec![0x07, 0x08, 0x09],
                    }))
                }
            }
        }
    }
}

/// The client+server+pipe in one place: each `exchange` encodes the request onto
/// the wire, the server decodes/handles/encodes a reply, the client decodes it —
/// appending every byte (both directions) to the transcript.
struct Loopback {
    server: StubServer,
    transcript: Vec<u8>,
    seq: u32,
}

impl Loopback {
    fn new() -> Self {
        Self {
            server: StubServer::new(),
            transcript: Vec::new(),
            seq: 1,
        }
    }

    fn exchange(&mut self, req: Request) -> Result<Reply, ControlError> {
        let seq = self.seq;
        self.seq += 1;

        // client -> server
        let mut c2s = Vec::new();
        encode_request(seq, &req, &mut c2s).unwrap();
        self.transcript.extend_from_slice(&c2s);

        // server decodes, handles, replies
        let (rseq, dreq, consumed) = decode_request(&c2s).unwrap().unwrap();
        assert_eq!(rseq, seq, "server sees the client's seq");
        assert_eq!(consumed, c2s.len());
        assert_eq!(dreq, req, "server decodes the request verbatim");
        let reply = self.server.handle(&dreq);

        // server -> client
        let mut s2c = Vec::new();
        encode_reply(rseq, &reply, &mut s2c).unwrap();
        self.transcript.extend_from_slice(&s2c);

        // client decodes
        let (cseq, dreply, dconsumed) = decode_reply(&s2c).unwrap().unwrap();
        assert_eq!(cseq, seq, "reply echoes the request seq");
        assert_eq!(dconsumed, s2c.len());
        assert_eq!(dreply, reply, "client decodes the reply verbatim");
        dreply
    }
}

/// Drive a fixed session that touches every verb (and both reply categories),
/// returning the full byte transcript and the decoded reply sequence.
fn run_session() -> (Vec<u8>, Vec<Result<Reply, ControlError>>) {
    let mut lb = Loopback::new();
    let mut replies = Vec::new();

    replies.push(lb.exchange(Request::Hello(caps())));

    let snap_reply = lb.exchange(Request::Snapshot);
    let snap = match &snap_reply {
        Ok(Reply::SnapId(s)) => *s,
        other => panic!("expected SnapId, got {other:?}"),
    };
    replies.push(snap_reply);

    replies.push(lb.exchange(Request::Branch {
        snap,
        env: Reproducer {
            blob_version: 1,
            bytes: vec![0x01, 0x02, 0x03],
        },
    }));
    // run -> Decision, then run(resolve) -> Quiescent
    replies.push(lb.exchange(Request::Run {
        until: conds(),
        resolve: None,
    }));
    replies.push(lb.exchange(Request::Run {
        until: conds(),
        resolve: Some(Answer(vec![0xA1])),
    }));
    replies.push(lb.exchange(Request::Replay(snap)));
    replies.push(lb.exchange(Request::Hash {
        scope: HashScope::Whole,
    }));
    // Observation verbs (task 80): read a small region, then the register view.
    replies.push(lb.exchange(Request::Read {
        gpa: 0x1000,
        len: 4,
    }));
    replies.push(lb.exchange(Request::Regs));
    // Improvisation (task 81): the reproducer mints cleanly BEFORE any exec, then
    // `exec` taints the timeline, the mint fails loud `Tainted`, and a snapshot
    // taken there surfaces the taint-carrying reply — every new wire shape crosses.
    replies.push(lb.exchange(Request::RecordedEnv));
    replies.push(lb.exchange(Request::Exec {
        cmd: "ps aux".to_string(),
        deadline: Moment(9_000),
    }));
    replies.push(lb.exchange(Request::RecordedEnv));
    replies.push(lb.exchange(Request::Snapshot));
    // Stage a host-plane fault over the wire (the perturb verb).
    replies.push(lb.exchange(Request::Perturb {
        fault: HostFault(vec![0x02, 0x80]), // opaque environment::HostFault bytes
        at: Moment(1_234),
    }));
    // resolve with no outstanding decision -> loud ControlError
    replies.push(lb.exchange(Request::Run {
        until: conds(),
        resolve: Some(Answer(vec![0xFF])),
    }));
    replies.push(lb.exchange(Request::Drop(snap)));

    (lb.transcript, replies)
}

#[test]
fn loopback_exercises_every_verb_with_expected_replies() {
    let (_, replies) = run_session();
    assert_eq!(
        replies,
        vec![
            Ok(Reply::Hello(caps())),
            Ok(Reply::SnapId(SnapId(0))),
            Ok(Reply::Unit), // Branch
            Ok(Reply::Stop(StopReason::Decision {
                vtime: Moment(100),
                id: DecisionId(1),
                ctx: vec![0xAB],
            })),
            Ok(Reply::Stop(StopReason::Quiescent { vtime: Moment(500) })),
            Ok(Reply::Unit), // Replay
            Ok(Reply::Hash([0x42; 32])),
            Ok(Reply::Bytes(vec![0xAB; 4])),
            Ok(Reply::Regs(RegsView {
                version: RegsView::VERSION,
                gpr: [0; 16],
                rip: 0xDEAD_BEEF,
                rflags: 0x2,
                seg: [0x10, 0x18, 0x18, 0x18, 0, 0],
                cr0: 0x8000_0011,
                cr3: 0x3000,
                cr4: 0x20,
                moment: Moment(500),
                vtime: 500,
            })),
            Ok(Reply::Recorded(Reproducer {
                blob_version: 1,
                bytes: vec![0x07, 0x08, 0x09],
            })), // RecordedEnv (untainted) mints the reproducer
            Ok(Reply::ExecResult {
                output: b"root@guest:/# ".to_vec(),
                ok: true,
            }), // Exec taints the timeline
            Err(ControlError::Tainted), // RecordedEnv now fails loud
            Ok(Reply::Snapshot {
                id: SnapId(1),
                tainted: true,
            }), // a snapshot from the tainted timeline reports it
            Ok(Reply::Unit),            // Perturb
            Err(ControlError::ResolveWithoutDecision),
            Ok(Reply::Unit), // Drop
        ]
    );
}

#[test]
fn two_identical_sessions_produce_byte_identical_transcripts() {
    let (t1, r1) = run_session();
    let (t2, r2) = run_session();
    assert_eq!(t1, t2, "transcripts are byte-identical across runs");
    assert_eq!(r1, r2, "reply sequences match across runs");
    assert!(!t1.is_empty(), "the session actually moved bytes");
}
