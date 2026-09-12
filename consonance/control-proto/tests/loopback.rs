// SPDX-License-Identifier: AGPL-3.0-or-later

use control_proto::{
    Answer, CapFlags, Caps, ControlError, CoverageGeometry, DecisionId, HashScope, HostFault,
    Moment, PROTO_VERSION, RegsView, Reply, Reproducer, Request, Resolution, SnapId,
    StopConditions, StopMask, StopReason, class_bit, decode_reply, decode_request, encode_reply,
    encode_request,
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

struct StubServer {
    next_snap: u64,
    armed: bool,
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
                Ok(Reply::Snapshot {
                    id: SnapId(id),
                    at: Moment(500),
                    sdk_events: 2,
                    tainted: self.tainted,
                })
            }
            Request::Drop(_)
            | Request::Branch { .. }
            | Request::Replay(_)
            | Request::Perturb { .. } => Ok(Reply::Unit),
            Request::Run { resolve, .. } => {
                if let Some(resolution) = resolve {
                    if !self.armed
                        || resolution.vtime != Moment(100)
                        || resolution.service != 19
                        || resolution.id != DecisionId(1)
                    {
                        return Err(ControlError::ResolveWithoutDecision);
                    }
                    self.armed = false;
                    Ok(Reply::Stop(StopReason::Quiescent { vtime: Moment(500) }))
                } else {
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
            Request::Exec { .. } => {
                self.tainted = true;
                Ok(Reply::ExecResult {
                    output: b"root@guest:/# ".to_vec(),
                    ok: true,
                })
            }
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

        let mut c2s = Vec::new();
        encode_request(seq, &req, &mut c2s).unwrap();
        self.transcript.extend_from_slice(&c2s);

        let (rseq, dreq, consumed) = decode_request(&c2s).unwrap().unwrap();
        assert_eq!(rseq, seq, "server sees the client's seq");
        assert_eq!(consumed, c2s.len());
        assert_eq!(dreq, req, "server decodes the request verbatim");
        let reply = self.server.handle(&dreq);

        let mut s2c = Vec::new();
        encode_reply(rseq, &reply, &mut s2c).unwrap();
        self.transcript.extend_from_slice(&s2c);

        let (cseq, dreply, dconsumed) = decode_reply(&s2c).unwrap().unwrap();
        assert_eq!(cseq, seq, "reply echoes the request seq");
        assert_eq!(dconsumed, s2c.len());
        assert_eq!(dreply, reply, "client decodes the reply verbatim");
        dreply
    }
}

fn run_session() -> (Vec<u8>, Vec<Result<Reply, ControlError>>) {
    let mut lb = Loopback::new();
    let mut replies = Vec::new();

    replies.push(lb.exchange(Request::Hello(caps())));

    let snap_reply = lb.exchange(Request::Snapshot);
    let snap = match &snap_reply {
        Ok(Reply::Snapshot { id, .. }) => *id,
        other => panic!("expected the seal-bound Snapshot reply, got {other:?}"),
    };
    replies.push(snap_reply);

    replies.push(lb.exchange(Request::Branch {
        snap,
        env: Reproducer {
            blob_version: 1,
            bytes: vec![0x01, 0x02, 0x03],
        },
    }));
    replies.push(lb.exchange(Request::Run {
        until: conds(),
        resolve: None,
    }));
    replies.push(lb.exchange(Request::Run {
        until: conds(),
        resolve: Some(Resolution {
            vtime: Moment(100),
            service: 19,
            id: DecisionId(1),
            answer: Answer(vec![0xA1]),
        }),
    }));
    replies.push(lb.exchange(Request::Replay(snap)));
    replies.push(lb.exchange(Request::Hash {
        scope: HashScope::Whole,
    }));
    replies.push(lb.exchange(Request::Read {
        gpa: 0x1000,
        len: 4,
    }));
    replies.push(lb.exchange(Request::Regs));
    replies.push(lb.exchange(Request::RecordedEnv));
    replies.push(lb.exchange(Request::Exec {
        cmd: "ps aux".to_string(),
        deadline: Moment(9_000),
    }));
    replies.push(lb.exchange(Request::RecordedEnv));
    replies.push(lb.exchange(Request::Snapshot));
    replies.push(lb.exchange(Request::Perturb {
        fault: HostFault(vec![0x02, 0x80]),
        at: Moment(1_234),
    }));
    replies.push(lb.exchange(Request::Run {
        until: conds(),
        resolve: Some(Resolution {
            vtime: Moment(0),
            service: 0,
            id: DecisionId(0),
            answer: Answer(vec![0xFF]),
        }),
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
            Ok(Reply::Snapshot {
                id: SnapId(0),
                at: Moment(500),
                sdk_events: 2,
                tainted: false,
            }),
            Ok(Reply::Unit),
            Ok(Reply::Stop(StopReason::Decision {
                vtime: Moment(100),
                id: DecisionId(1),
                ctx: vec![0xAB],
            })),
            Ok(Reply::Stop(StopReason::Quiescent { vtime: Moment(500) })),
            Ok(Reply::Unit),
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
            })),
            Ok(Reply::ExecResult {
                output: b"root@guest:/# ".to_vec(),
                ok: true,
            }),
            Err(ControlError::Tainted),
            Ok(Reply::Snapshot {
                id: SnapId(1),
                at: Moment(500),
                sdk_events: 2,
                tainted: true,
            }),
            Ok(Reply::Unit),
            Err(ControlError::ResolveWithoutDecision),
            Ok(Reply::Unit),
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
