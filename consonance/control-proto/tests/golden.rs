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
    EventRef, HashScope, HostFault, Moment, PROTO_VERSION, ProtocolError, Reply, Reproducer,
    Request, SnapId, StopConditions, StopMask, StopReason, class_bit, decode_reply, decode_request,
    encode_reply, encode_request,
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
            env: Reproducer {
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
                deadline: Some(Moment(0x100)),
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

#[test]
fn req_read() {
    check_req(
        11,
        Request::Read {
            gpa: 0x1000,
            len: 0x40,
        },
        &[
            0x0A, // REQ_READ
            0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // gpa = 0x1000
            0x40, 0x00, 0x00, 0x00, // len = 0x40
        ],
    );
}

#[test]
fn req_regs() {
    check_req(12, Request::Regs, &[0x0B]);
}

// -------------------------------- replies ----------------------------------

#[test]
fn reply_hello() {
    let mut body = vec![0x00, 0x01]; // RESULT_OK, REPLY_HELLO
    body.extend_from_slice(&CAPS_BYTES);
    check_reply(10, Ok(Reply::Hello(sample_caps())), &body);
}

/// The seal-bound snapshot reply (task 127), **untainted**: the one reply to
/// `Request::Snapshot` carries the handle, the synchronized seal `Moment`, the
/// included SDK-event count (the cut), and the taint byte — all from the same
/// stopped server state. (The pre-127 bare-handle `SnapId` reply, wire tag 2,
/// is retired; see `retired_snapid_tag_is_rejected` in `adversarial.rs`.)
#[test]
fn reply_snapshot_untainted_carries_the_cut() {
    check_reply(
        11,
        Ok(Reply::Snapshot {
            id: SnapId(9),
            at: Moment(0x1234),
            sdk_events: 3,
            tainted: false,
        }),
        &[
            0x00, 0x0A, // RESULT_OK, REPLY_SNAPSHOT
            0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // id = 9
            0x34, 0x12, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // at = 0x1234
            0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // sdk_events = 3
            0x00, // tainted = false
        ],
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

#[test]
fn reply_bytes() {
    check_reply(
        14,
        Ok(Reply::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF])),
        &[
            0x00, 0x07, // RESULT_OK, REPLY_BYTES
            0x04, 0x00, 0x00, 0x00, // bytes len = 4
            0xDE, 0xAD, 0xBE, 0xEF, // bytes
        ],
    );
}

#[test]
fn reply_regs() {
    use control_proto::{Moment, RegsView};
    // Distinctive per-field values so any layout drift (field order, width, or a
    // dropped/added field) fails against the hand-written bytes.
    let view = RegsView {
        version: RegsView::VERSION,
        gpr: [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD,
            0xEE, 0xFF,
        ],
        rip: 0x1234,
        rflags: 0x2,
        seg: [0x10, 0x18, 0x20, 0x28, 0x30, 0x38],
        cr0: 0x8000_0011,
        cr3: 0x3000,
        cr4: 0x20,
        moment: Moment(0x64),
        vtime: 0x64,
    };
    // Build the expected body field-by-field (fixed order, little-endian) — the
    // wire layout pinned independently of the codec.
    let mut body = vec![0x00, 0x08]; // RESULT_OK, REPLY_REGS
    body.extend_from_slice(&1u16.to_le_bytes()); // version = 1
    for g in view.gpr {
        body.extend_from_slice(&g.to_le_bytes());
    }
    body.extend_from_slice(&view.rip.to_le_bytes());
    body.extend_from_slice(&view.rflags.to_le_bytes());
    for s in view.seg {
        body.extend_from_slice(&s.to_le_bytes());
    }
    body.extend_from_slice(&view.cr0.to_le_bytes());
    body.extend_from_slice(&view.cr3.to_le_bytes());
    body.extend_from_slice(&view.cr4.to_le_bytes());
    body.extend_from_slice(&view.moment.0.to_le_bytes());
    body.extend_from_slice(&view.vtime.to_le_bytes());
    // 2 tag + 2 version + 16*8 gpr + 8 rip + 8 rflags + 6*2 seg + 3*8 cr + 8 moment + 8 vtime.
    assert_eq!(body.len(), 2 + 2 + 128 + 8 + 8 + 12 + 24 + 8 + 8);
    check_reply(15, Ok(Reply::Regs(view)), &body);
}

// --------------------------- StopReason variants ---------------------------

#[test]
fn reply_stop_deadline() {
    check_reply(
        20,
        Ok(Reply::Stop(StopReason::Deadline {
            vtime: Moment(0x2A),
        })),
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
        Ok(Reply::Stop(StopReason::Quiescent {
            vtime: Moment(0x2A),
        })),
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
            vtime: Moment(5),
            info: CrashInfo {
                kind: CrashKind::UnrecoverableFault,
                detail: vec![0xEE],
            },
        })),
        &[
            0x00, 0x04, // RESULT_OK, REPLY_STOP
            0x03, // SR_CRASH
            0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // vtime = 5
            0x01, // CK_UNRECOVERABLE_FAULT
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
            vtime: Moment(0x10),
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
            vtime: Moment(0x10),
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
            vtime: Moment(0x10),
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
    check_reply(42, Err(ControlError::Unsupported), &[0x01, 0x0A]);
}

#[test]
fn err_perturb_out_of_range() {
    // RESULT_ERR (0x01), CE_PERTURB_OUT_OF_RANGE (0x0B), gpa (u64 LE), ram_len (u64 LE).
    check_reply(
        43,
        Err(ControlError::PerturbOutOfRange {
            gpa: 0x1234,
            ram_len: 0x1000,
        }),
        &[
            0x01, 0x0B, 0x34, 0x12, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ],
    );
}

#[test]
fn err_perturb_past_moment() {
    // RESULT_ERR (0x01), CE_PERTURB_PAST_MOMENT (0x0C), at (u64 LE), floor (u64 LE).
    check_reply(
        44,
        Err(ControlError::PerturbPastMoment {
            at: 0x2C,
            floor: 0x64,
        }),
        &[
            0x01, 0x0C, 0x2C, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ],
    );
}

#[test]
fn err_perturb_moment_taken() {
    // RESULT_ERR (0x01), CE_PERTURB_MOMENT_TAKEN (0x0D), at (u64 LE).
    check_reply(
        45,
        Err(ControlError::PerturbMomentTaken { at: 0x1F4 }),
        &[0x01, 0x0D, 0xF4, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
}

#[test]
fn err_schedule_unsatisfiable() {
    // RESULT_ERR (0x01), CE_SCHEDULE_UNSATISFIABLE (0x0E), moment (u64 LE), vtime (u64 LE).
    check_reply(
        46,
        Err(ControlError::ScheduleUnsatisfiable {
            moment: 0x64,
            vtime: 0xC8,
        }),
        &[
            0x01, 0x0E, 0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC8, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ],
    );
}

#[test]
fn err_schedule_moment_unreachable() {
    // RESULT_ERR (0x01), CE_SCHEDULE_MOMENT_UNREACHABLE (0x14), moment (u64 LE),
    // landing (u64 LE) — the arm-site refusal's prospective (unreached) landing.
    check_reply(
        61,
        Err(ControlError::ScheduleMomentUnreachable {
            moment: 0x64,
            landing: 0xC8,
        }),
        &[
            0x01, 0x14, 0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC8, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ],
    );
}

#[test]
fn err_not_synchronized() {
    // RESULT_ERR (0x01), CE_NOT_SYNCHRONIZED (0x0F), no payload.
    check_reply(47, Err(ControlError::NotSynchronized), &[0x01, 0x0F]);
}

#[test]
fn err_perturb_reserved_vector() {
    // RESULT_ERR (0x01), CE_PERTURB_RESERVED_VECTOR (0x10), vector (u8).
    check_reply(
        48,
        Err(ControlError::PerturbReservedVector { vector: 7 }),
        &[0x01, 0x10, 0x07],
    );
}

#[test]
fn err_read_out_of_range() {
    // RESULT_ERR (0x01), CE_READ_OUT_OF_RANGE (0x11), gpa (u64 LE), len (u32 LE), ram_len (u64 LE).
    check_reply(
        49,
        Err(ControlError::ReadOutOfRange {
            gpa: 0x3FF0,
            len: 0x40,
            ram_len: 0x4000,
        }),
        &[
            0x01, 0x11, // RESULT_ERR, CE_READ_OUT_OF_RANGE
            0xF0, 0x3F, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // gpa = 0x3FF0
            0x40, 0x00, 0x00, 0x00, // len = 0x40
            0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // ram_len = 0x4000
        ],
    );
}

#[test]
fn err_read_too_large() {
    // RESULT_ERR (0x01), CE_READ_TOO_LARGE (0x12), len (u32 LE), cap (u32 LE).
    check_reply(
        50,
        Err(ControlError::ReadTooLarge {
            len: 0x2_0000,
            cap: control_proto::READ_CAP,
        }),
        &[
            0x01, 0x12, // RESULT_ERR, CE_READ_TOO_LARGE
            0x00, 0x00, 0x02, 0x00, // len = 0x20000
            0x00, 0x00, 0x01, 0x00, // cap = 0x10000 (READ_CAP = 64 KiB)
        ],
    );
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

// ------------------------- task 81: improvisations -------------------------

#[test]
fn req_exec() {
    // REQ_EXEC (0x0C), cmd (u32-len-prefixed UTF-8), deadline (u64 LE).
    check_req(
        13,
        Request::Exec {
            cmd: "ls /".to_string(),
            deadline: Moment(0x64),
        },
        &[
            0x0C, // REQ_EXEC
            0x04, 0x00, 0x00, 0x00, // cmd len = 4
            b'l', b's', b' ', b'/', // "ls /"
            0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // deadline = 0x64
        ],
    );
}

#[test]
fn req_recorded_env() {
    check_req(14, Request::RecordedEnv, &[0x0D]);
}

#[test]
fn reply_exec_result() {
    // RESULT_OK (0x00), REPLY_EXEC_RESULT (0x09), output (u32-len blob), ok (u8).
    check_reply(
        60,
        Ok(Reply::ExecResult {
            output: vec![0x6F, 0x6B], // "ok"
            ok: true,
        }),
        &[
            0x00, 0x09, // RESULT_OK, REPLY_EXEC_RESULT
            0x02, 0x00, 0x00, 0x00, // output len = 2
            0x6F, 0x6B, // "ok"
            0x01, // ok = true
        ],
    );
}

/// The seal-bound snapshot reply, **tainted** (task 81 × task 127): the taint
/// byte rides the same cut-carrying shape — a tainted seal still binds its
/// exact evidence cut.
#[test]
fn reply_snapshot_tainted_carries_the_cut() {
    // RESULT_OK (0x00), REPLY_SNAPSHOT (0x0A), id (u64 LE), at (u64 LE),
    // sdk_events (u64 LE), tainted (u8).
    check_reply(
        61,
        Ok(Reply::Snapshot {
            id: SnapId(9),
            at: Moment(0x64),
            sdk_events: 2,
            tainted: true,
        }),
        &[
            0x00, 0x0A, // RESULT_OK, REPLY_SNAPSHOT
            0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // id = 9
            0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // at = 0x64
            0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // sdk_events = 2
            0x01, // tainted = true
        ],
    );
}

#[test]
fn reply_recorded() {
    // RESULT_OK (0x00), REPLY_RECORDED (0x0B), Reproducer (blob_version u16, bytes).
    check_reply(
        62,
        Ok(Reply::Recorded(Reproducer {
            blob_version: 3,
            bytes: vec![0xCA, 0xFE],
        })),
        &[
            0x00, 0x0B, // RESULT_OK, REPLY_RECORDED
            0x03, 0x00, // blob_version = 3
            0x02, 0x00, 0x00, 0x00, // bytes len = 2
            0xCA, 0xFE, // bytes
        ],
    );
}

#[test]
fn err_tainted() {
    // RESULT_ERR (0x01), CE_TAINTED (0x13), no payload.
    check_reply(63, Err(ControlError::Tainted), &[0x01, 0x13]);
}

/// The `class_bit` constants are a hand-maintained mirror of
/// `environment::DecisionClass` (the lib stays schema-blind — conventions rule 2,
/// so it never imports the enum). This test — the only place both are in scope —
/// pins the mirror against the real enum: a renumbering on either side, or a new
/// class added to only one, fails here rather than silently desyncing the
/// armed-class `StopMask` from the backend's decision classes.
#[test]
fn class_bit_mirrors_decision_class() {
    use environment::DecisionClass as D;
    assert_eq!(class_bit::ENTROPY, D::Entropy as u16);
    assert_eq!(class_bit::PAYLOAD, D::Payload as u16);
    assert_eq!(class_bit::SCHEDULER, D::Scheduler as u16);
    assert_eq!(class_bit::NET_SEND, D::NetFlow as u16);
    assert_eq!(class_bit::BLOCK_IO, D::BlockIo as u16);
    assert_eq!(class_bit::PROCESS, D::Process as u16);
    assert_eq!(class_bit::BUGGIFY, D::Buggify as u16);
    // And the task-73 addition is pinned to its literal, so the enum and the
    // mirror moving together (to the wrong shared value) is still caught.
    assert_eq!(class_bit::BUGGIFY, 7);
}
