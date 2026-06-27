// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 1 — golden bytes. Hand-written expected frames for every `Request`
//! variant and every `Reply` / `ControlError` (and nested `StopReason`) variant,
//! asserting the exact `[u8]` and pinning the wire format.
//!
//! Each check asserts the full emitted frame equals `header(seq, body) ++ body`,
//! where the per-variant `body` bytes are written out by hand (the encoding
//! contract) and the header is the fixed `magic·version·seq·len` envelope —
//! itself pinned byte-for-byte by [`snapshot_full_frame_is_byte_exact`]. Every
//! golden also round-trips back to the original value.

use control_proto::{
    Answer, CapFlags, Caps, ControlError, CoverageGeometry, CrashInfo, CrashKind, DecisionId,
    Environment, EventRef, HashScope, HostFault, Moment, PROTO_VERSION, ProtocolError, Reply,
    Request, SnapId, StopConditions, StopMask, StopReason, VTime, class_bit, decode_reply,
    decode_request, encode_reply, encode_request,
};

const MAGIC: [u8; 4] = *b"CTL1";

/// Build the expected full frame: `magic · version · seq · len · body`.
fn framed(seq: u32, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&MAGIC);
    v.extend_from_slice(&PROTO_VERSION.to_le_bytes());
    v.extend_from_slice(&seq.to_le_bytes());
    v.extend_from_slice(&(body.len() as u32).to_le_bytes());
    v.extend_from_slice(body);
    v
}

#[track_caller]
fn check_req(seq: u32, req: Request, body: &[u8]) {
    let mut buf = Vec::new();
    encode_request(seq, &req, &mut buf).expect("encode");
    assert_eq!(buf, framed(seq, body), "request frame bytes drifted");
    let (got_seq, got, consumed) = decode_request(&buf).expect("decode").expect("complete");
    assert_eq!(got_seq, seq, "seq echoes");
    assert_eq!(got, req, "request round-trips");
    assert_eq!(consumed, buf.len(), "consumes the whole frame");
}

#[track_caller]
fn check_reply(seq: u32, reply: Result<Reply, ControlError>, body: &[u8]) {
    let mut buf = Vec::new();
    encode_reply(seq, &reply, &mut buf).expect("encode");
    assert_eq!(buf, framed(seq, body), "reply frame bytes drifted");
    let (got_seq, got, consumed) = decode_reply(&buf).expect("decode").expect("complete");
    assert_eq!(got_seq, seq, "seq echoes");
    assert_eq!(got, reply, "reply round-trips");
    assert_eq!(consumed, buf.len(), "consumes the whole frame");
}

/// The capabilities used in the Hello goldens: protocol 1, env range 1..=3, a
/// 4096-byte coverage map from producer 2, the `guest_has_sdk` flag.
fn sample_caps() -> Caps {
    Caps {
        protocol_version: 1,
        env_version_min: 1,
        env_version_max: 3,
        coverage: CoverageGeometry {
            map_bytes: 0x1000,
            producer: 2,
        },
        flags: CapFlags::GUEST_HAS_SDK,
    }
}

/// The exact `Caps` body bytes (15 bytes) shared by the Hello request/reply.
const CAPS_BYTES: [u8; 15] = [
    0x01, 0x00, // protocol_version = 1
    0x01, 0x00, // env_version_min = 1
    0x03, 0x00, // env_version_max = 3
    0x00, 0x10, 0x00, 0x00, // map_bytes = 0x1000
    0x02, // producer = 2
    0x01, 0x00, 0x00, 0x00, // flags = 1 (GUEST_HAS_SDK)
];

#[test]
fn snapshot_full_frame_is_byte_exact() {
    // The one fully-literal frame: pins the complete header envelope (magic,
    // version, seq byte order, length) independently of the `framed` helper.
    let mut buf = Vec::new();
    encode_request(0x0102_0304, &Request::Snapshot, &mut buf).expect("encode");
    assert_eq!(
        buf,
        vec![
            0x43, 0x54, 0x4C, 0x31, // magic "CTL1"
            0x01, 0x00, // version = 1
            0x04, 0x03, 0x02, 0x01, // seq = 0x01020304 (little-endian)
            0x01, 0x00, 0x00, 0x00, // len = 1
            0x02, // body: REQ_SNAPSHOT
        ]
    );
}

// ------------------------------- requests ----------------------------------

#[test]
fn req_hello() {
    let mut body = vec![0x01]; // REQ_HELLO
    body.extend_from_slice(&CAPS_BYTES);
    check_req(1, Request::Hello(sample_caps()), &body);
}

#[test]
fn req_snapshot() {
    check_req(2, Request::Snapshot, &[0x02]);
}

#[test]
fn req_drop() {
    check_req(
        3,
        Request::Drop(SnapId(0xAABB_CCDD)),
        &[0x03, 0xDD, 0xCC, 0xBB, 0xAA, 0x00, 0x00, 0x00, 0x00],
    );
}

#[test]
fn req_branch() {
    check_req(
        4,
        Request::Branch {
            snap: SnapId(7),
            env: Environment {
                blob_version: 2,
                bytes: vec![0xDE, 0xAD],
            },
        },
        &[
            0x04, // REQ_BRANCH
            0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // snap = 7
            0x02, 0x00, // blob_version = 2
            0x02, 0x00, 0x00, 0x00, // env bytes len = 2
            0xDE, 0xAD, // env bytes
        ],
    );
}

#[test]
fn req_replay() {
    check_req(
        5,
        Request::Replay(SnapId(7)),
        &[0x05, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
}

#[test]
fn req_run_with_deadline_and_resolve() {
    check_req(
        6,
        Request::Run {
            until: StopConditions {
                deadline: Some(VTime(0x100)),
                on: StopMask::NONE.arm(class_bit::BLOCK_IO),
            },
            resolve: Some(Answer(vec![0x01, 0x02])),
        },
        &[
            0x06, // REQ_RUN
            0x01, // deadline present
            0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // deadline = 0x100
            0x20, 0x00, 0x00, 0x00, // on = 1<<5 = 0x20 (BlockIo armed)
            0x01, // resolve present
            0x02, 0x00, 0x00, 0x00, // answer len = 2
            0x01, 0x02, // answer bytes
        ],
    );
}

#[test]
fn req_run_without_deadline_or_resolve() {
    check_req(
        7,
        Request::Run {
            until: StopConditions {
                deadline: None,
                on: StopMask::NONE,
            },
            resolve: None,
        },
        &[
            0x06, // REQ_RUN
            0x00, // deadline absent
            0x00, 0x00, 0x00, 0x00, // on = 0
            0x00, // resolve absent
        ],
    );
}

#[test]
fn req_hash_region() {
    check_req(
        8,
        Request::Hash {
            scope: HashScope::Region {
                base: 0x1000,
                len: 0x40,
            },
        },
        &[
            0x07, // REQ_HASH
            0x02, // HS_REGION
            0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // base = 0x1000
            0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // len = 0x40
        ],
    );
}

#[test]
fn req_hash_whole_and_disk() {
    check_req(
        9,
        Request::Hash {
            scope: HashScope::Whole,
        },
        &[0x07, 0x00],
    );
    check_req(
        9,
        Request::Hash {
            scope: HashScope::Disk,
        },
        &[0x07, 0x01],
    );
}

#[test]
fn req_perturb() {
    check_req(
        10,
        Request::Perturb {
            fault: HostFault(vec![0xAB, 0xCD]),
            at: Moment(0x42),
        },
        &[
            0x08, // REQ_PERTURB
            0x02, 0x00, 0x00, 0x00, // fault bytes len = 2
            0xAB, 0xCD, // fault bytes
            0x42, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // at = 0x42
        ],
    );
}

// -------------------------------- replies ----------------------------------

#[test]
fn reply_hello() {
    let mut body = vec![0x00, 0x01]; // RESULT_OK, REPLY_HELLO
    body.extend_from_slice(&CAPS_BYTES);
    check_reply(10, Ok(Reply::Hello(sample_caps())), &body);
}

#[test]
fn reply_snapid() {
    check_reply(
        11,
        Ok(Reply::SnapId(SnapId(9))),
        &[0x00, 0x02, 0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
}

#[test]
fn reply_unit() {
    check_reply(12, Ok(Reply::Unit), &[0x00, 0x03]);
}

#[test]
fn reply_hash() {
    let mut digest = [0u8; 32];
    for (i, b) in digest.iter_mut().enumerate() {
        *b = i as u8;
    }
    let mut body = vec![0x00, 0x05]; // RESULT_OK, REPLY_HASH
    body.extend_from_slice(&digest);
    check_reply(13, Ok(Reply::Hash(digest)), &body);
}

// --------------------------- StopReason variants ---------------------------

#[test]
fn reply_stop_deadline() {
    check_reply(
        20,
        Ok(Reply::Stop(StopReason::Deadline { vtime: VTime(0x2A) })),
        &[
            0x00, 0x04, // RESULT_OK, REPLY_STOP
            0x01, // SR_DEADLINE
            0x2A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // vtime = 0x2A
        ],
    );
}

#[test]
fn reply_stop_quiescent() {
    check_reply(
        21,
        Ok(Reply::Stop(StopReason::Quiescent { vtime: VTime(0x2A) })),
        &[
            0x00, 0x04, 0x02, 0x2A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ],
    );
}

#[test]
fn reply_stop_crash() {
    check_reply(
        22,
        Ok(Reply::Stop(StopReason::Crash {
            vtime: VTime(5),
            info: CrashInfo {
                kind: CrashKind::TripleFault,
                detail: vec![0xEE],
            },
        })),
        &[
            0x00, 0x04, // RESULT_OK, REPLY_STOP
            0x03, // SR_CRASH
            0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // vtime = 5
            0x01, // CK_TRIPLE_FAULT
            0x01, 0x00, 0x00, 0x00, // detail len = 1
            0xEE, // detail
        ],
    );
}

#[test]
fn reply_stop_decision() {
    check_reply(
        23,
        Ok(Reply::Stop(StopReason::Decision {
            vtime: VTime(0x10),
            id: DecisionId(3),
            ctx: vec![0xAB, 0xCD],
        })),
        &[
            0x00, 0x04, // RESULT_OK, REPLY_STOP
            0x04, // SR_DECISION
            0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // vtime = 0x10
            0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // id = 3
            0x02, 0x00, 0x00, 0x00, // ctx len = 2
            0xAB, 0xCD, // ctx
        ],
    );
}

#[test]
fn reply_stop_snapshot_point() {
    check_reply(
        24,
        Ok(Reply::Stop(StopReason::SnapshotPoint {
            vtime: VTime(0x10),
        })),
        &[
            0x00, 0x04, 0x05, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ],
    );
}

#[test]
fn reply_stop_assertion() {
    check_reply(
        25,
        Ok(Reply::Stop(StopReason::Assertion {
            vtime: VTime(0x10),
            ev: EventRef {
                id: 0x99,
                data: vec![0x01],
            },
        })),
        &[
            0x00, 0x04, // RESULT_OK, REPLY_STOP
            0x06, // SR_ASSERTION
            0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // vtime = 0x10
            0x99, 0x00, 0x00, 0x00, // ev.id = 0x99
            0x01, 0x00, 0x00, 0x00, // ev.data len = 1
            0x01, // ev.data
        ],
    );
}

// -------------------------- ControlError variants --------------------------

#[test]
fn err_unknown_snapshot() {
    check_reply(
        30,
        Err(ControlError::UnknownSnapshot(SnapId(4))),
        &[0x01, 0x01, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
}

#[test]
fn err_simple_unit_variants() {
    check_reply(31, Err(ControlError::RestoreFailed), &[0x01, 0x02]);
    check_reply(32, Err(ControlError::SnapshotWhileArmed), &[0x01, 0x03]);
    check_reply(33, Err(ControlError::NotQuiescent), &[0x01, 0x04]);
    check_reply(34, Err(ControlError::MalformedEnvironment), &[0x01, 0x06]);
    check_reply(35, Err(ControlError::ResolveWithoutDecision), &[0x01, 0x07]);
    check_reply(36, Err(ControlError::MalformedAnswer), &[0x01, 0x08]);
}

#[test]
fn err_bad_env_version() {
    check_reply(
        37,
        Err(ControlError::BadEnvVersion(7)),
        &[0x01, 0x05, 0x07, 0x00],
    );
}

#[test]
fn err_protocol() {
    check_reply(
        38,
        Err(ControlError::Protocol(ProtocolError::BadLength)),
        &[0x01, 0x09, 0x03],
    );
    check_reply(
        39,
        Err(ControlError::Protocol(ProtocolError::ShortFrame)),
        &[0x01, 0x09, 0x00],
    );
    check_reply(
        40,
        Err(ControlError::Protocol(ProtocolError::BadMagic)),
        &[0x01, 0x09, 0x01],
    );
    check_reply(
        41,
        Err(ControlError::Protocol(ProtocolError::BadVersion)),
        &[0x01, 0x09, 0x02],
    );
}
