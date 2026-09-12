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
    Request, Resolution, SnapId, StopConditions, StopMask, StopReason, class_bit, decode_reply,
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
    0x01, 0x00, 0x01, 0x00, 0x03, 0x00, 0x00, 0x10, 0x00, 0x00, 0x02, 0x01, 0x00, 0x00, 0x00,
];

#[test]
fn snapshot_full_frame_is_byte_exact() {
    let mut buf = Vec::new();
    encode_request(0x0102_0304, &Request::Snapshot, &mut buf).expect("encode");
    assert_eq!(
        buf,
        vec![
            0x43, 0x54, 0x4C, 0x31, 0x01, 0x00, 0x04, 0x03, 0x02, 0x01, 0x01, 0x00, 0x00, 0x00,
            0x02,
        ]
    );
}

#[test]
fn req_hello() {
    let mut body = vec![0x01];
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
            0x04, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x02, 0x00, 0x00,
            0x00, 0xDE, 0xAD,
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
            resolve: Some(Resolution {
                vtime: Moment(0x200),
                service: 0x1234,
                id: DecisionId(7),
                answer: Answer(vec![0x01, 0x02]),
            }),
        },
        &[
            0x06, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00,
            0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x34, 0x12, 0x07, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x02,
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
        &[0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
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
            0x07, 0x02, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
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
            0x08, 0x02, 0x00, 0x00, 0x00, 0xAB, 0xCD, 0x42, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00,
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
            0x0A, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00,
        ],
    );
}

#[test]
fn req_regs() {
    check_req(12, Request::Regs, &[0x0B]);
}

#[test]
fn req_sdk_events() {
    check_req(
        15,
        Request::SdkEvents { offset: 0x2A },
        &[0x09, 0x2A, 0x00, 0x00, 0x00],
    );
}

#[test]
fn req_console() {
    check_req(
        16,
        Request::Console { offset: 0x0100 },
        &[0x0E, 0x00, 0x01, 0x00, 0x00],
    );
}

#[test]
fn reply_hello() {
    let mut body = vec![0x00, 0x01];
    body.extend_from_slice(&CAPS_BYTES);
    check_reply(10, Ok(Reply::Hello(sample_caps())), &body);
}

/// The seal-bound snapshot reply, **untainted**: the one reply to
/// `Request::Snapshot` carries the handle, the synchronized seal `Moment`, the
/// included SDK-event count (the cut), and the taint byte — all from the same
/// stopped server state. (The earlier bare-handle `SnapId` reply, wire tag 2,
/// is retired; see `retired_snapid_tag_is_rejected` in `malformed.rs`.)
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
            0x00, 0x0A, 0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x34, 0x12, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
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
    let mut body = vec![0x00, 0x05];
    body.extend_from_slice(&digest);
    check_reply(13, Ok(Reply::Hash(digest)), &body);
}

#[test]
fn reply_bytes() {
    check_reply(
        14,
        Ok(Reply::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF])),
        &[0x00, 0x07, 0x04, 0x00, 0x00, 0x00, 0xDE, 0xAD, 0xBE, 0xEF],
    );
}

#[test]
fn reply_sdk_events() {
    check_reply(
        64,
        Ok(Reply::SdkEvents(vec![
            (0x11, 0x22, vec![0xAA]),
            (0x33, 0x44, vec![]),
        ])),
        &[
            0x00, 0x06, 0x02, 0x00, 0x00, 0x00, 0x11, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x22, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0xAA, 0x33, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ],
    );
}

#[test]
fn reply_console() {
    check_reply(
        65,
        Ok(Reply::Console {
            total: 0x1234,
            chunk: vec![b'h', b'i'],
        }),
        &[
            0x00, 0x0C, 0x34, 0x12, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x68, 0x69,
        ],
    );
}

/// An empty console page — the drained-capture case a client pages until it
/// sees. Pinned separately because "no bytes" is the state a truncating codec
/// bug is most likely to reach by accident.
#[test]
fn reply_console_empty_page() {
    check_reply(
        66,
        Ok(Reply::Console {
            total: 0,
            chunk: vec![],
        }),
        &[0x00, 0x0C, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
}

#[test]
fn reply_regs() {
    use control_proto::{Moment, RegsView};
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
    let mut body = vec![0x00, 0x08];
    body.extend_from_slice(&1u16.to_le_bytes());
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
    assert_eq!(body.len(), 2 + 2 + 128 + 8 + 8 + 12 + 24 + 8 + 8);
    check_reply(15, Ok(Reply::Regs(view)), &body);
}

#[test]
fn reply_stop_deadline() {
    check_reply(
        20,
        Ok(Reply::Stop(StopReason::Deadline {
            vtime: Moment(0x2A),
        })),
        &[
            0x00, 0x04, 0x01, 0x2A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
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
            0x00, 0x04, 0x03, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00,
            0x00, 0x00, 0xEE,
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
            0x00, 0x04, 0x04, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0xAB, 0xCD,
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
            0x00, 0x04, 0x06, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x99, 0x00, 0x00,
            0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
        ],
    );
}

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
    check_reply(
        45,
        Err(ControlError::PerturbMomentTaken { at: 0x1F4 }),
        &[0x01, 0x0D, 0xF4, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
}

#[test]
fn err_schedule_unsatisfiable() {
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
fn err_perturb_reserved_vector() {
    check_reply(
        48,
        Err(ControlError::PerturbReservedVector { vector: 7 }),
        &[0x01, 0x10, 0x07],
    );
}

#[test]
fn err_read_out_of_range() {
    check_reply(
        49,
        Err(ControlError::ReadOutOfRange {
            gpa: 0x3FF0,
            len: 0x40,
            ram_len: 0x4000,
        }),
        &[
            0x01, 0x11, 0xF0, 0x3F, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00,
            0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ],
    );
}

#[test]
fn err_read_too_large() {
    check_reply(
        50,
        Err(ControlError::ReadTooLarge {
            len: 0x8_0000,
            cap: control_proto::READ_CAP,
        }),
        &[0x01, 0x12, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x04, 0x00],
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

#[test]
fn req_exec() {
    check_req(
        13,
        Request::Exec {
            cmd: "ls /".to_string(),
            deadline: Moment(0x64),
        },
        &[
            0x0C, 0x04, 0x00, 0x00, 0x00, b'l', b's', b' ', b'/', 0x64, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00,
        ],
    );
}

#[test]
fn req_recorded_env() {
    check_req(14, Request::RecordedEnv, &[0x0D]);
}

#[test]
fn reply_exec_result() {
    check_reply(
        60,
        Ok(Reply::ExecResult {
            output: vec![0x6F, 0x6B],
            ok: true,
        }),
        &[0x00, 0x09, 0x02, 0x00, 0x00, 0x00, 0x6F, 0x6B, 0x01],
    );
}

/// The seal-bound snapshot reply, **tainted**: the taint
/// byte rides the same cut-carrying shape — a tainted seal still binds its
/// exact evidence cut.
#[test]
fn reply_snapshot_tainted_carries_the_cut() {
    check_reply(
        61,
        Ok(Reply::Snapshot {
            id: SnapId(9),
            at: Moment(0x64),
            sdk_events: 2,
            tainted: true,
        }),
        &[
            0x00, 0x0A, 0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
        ],
    );
}

#[test]
fn reply_recorded() {
    check_reply(
        62,
        Ok(Reply::Recorded(Reproducer {
            blob_version: 3,
            bytes: vec![0xCA, 0xFE],
        })),
        &[0x00, 0x0B, 0x03, 0x00, 0x02, 0x00, 0x00, 0x00, 0xCA, 0xFE],
    );
}

#[test]
fn err_tainted() {
    check_reply(63, Err(ControlError::Tainted), &[0x01, 0x13]);
}

/// The `class_bit` values are a persisted protocol contract. The environment
/// fault catalog used to expose a `DecisionClass` enum that could be mirrored
/// here, but the generic protocol no longer depends on that workload-specific
/// catalog. Pinning the wire values still catches accidental renumbering and
/// keeps armed-class `StopMask` values stable for archived requests.
#[test]
fn class_bit_values_are_pinned() {
    assert_eq!(class_bit::ENTROPY, 1);
    assert_eq!(class_bit::PAYLOAD, 2);
    assert_eq!(class_bit::SCHEDULER, 3);
    assert_eq!(class_bit::NET_SEND, 4);
    assert_eq!(class_bit::BLOCK_IO, 5);
    assert_eq!(class_bit::PROCESS, 6);
    assert_eq!(class_bit::BUGGIFY, 7);
    assert_eq!(class_bit::SNAPSHOT_POINT, 8);
    assert_eq!(class_bit::ASSERTION, 9);
}

/// The body tag of an encoded frame: the first byte after the fixed 14-byte
/// header (`magic·version·seq·len`).
fn body_tag(frame: &[u8]) -> u8 {
    frame[14]
}

#[test]
fn every_request_variant_has_a_pinned_tag() {
    let samples = vec![
        Request::Hello(sample_caps()),
        Request::Snapshot,
        Request::Drop(SnapId(1)),
        Request::Branch {
            snap: SnapId(1),
            env: Reproducer {
                blob_version: 1,
                bytes: vec![0x00],
            },
        },
        Request::Replay(SnapId(1)),
        Request::Run {
            until: StopConditions {
                deadline: None,
                on: StopMask::NONE,
            },
            resolve: None,
        },
        Request::Hash {
            scope: HashScope::Whole,
        },
        Request::Perturb {
            fault: HostFault(vec![0x00]),
            at: Moment(0),
        },
        Request::SdkEvents { offset: 0 },
        Request::Read { gpa: 0, len: 1 },
        Request::Regs,
        Request::Exec {
            cmd: "true".to_string(),
            deadline: Moment(1),
        },
        Request::RecordedEnv,
        Request::Console { offset: 0 },
    ];

    let mut tags = Vec::new();
    for req in &samples {
        let expected: u8 = match req {
            Request::Hello(_) => 0x01,
            Request::Snapshot => 0x02,
            Request::Drop(_) => 0x03,
            Request::Branch { .. } => 0x04,
            Request::Replay(_) => 0x05,
            Request::Run { .. } => 0x06,
            Request::Hash { .. } => 0x07,
            Request::Perturb { .. } => 0x08,
            Request::SdkEvents { .. } => 0x09,
            Request::Read { .. } => 0x0A,
            Request::Regs => 0x0B,
            Request::Exec { .. } => 0x0C,
            Request::RecordedEnv => 0x0D,
            Request::Console { .. } => 0x0E,
        };
        let mut buf = Vec::new();
        encode_request(1, req, &mut buf).expect("encode");
        assert_eq!(body_tag(&buf), expected, "{req:?}: body tag drifted");
        tags.push(expected);
    }

    tags.sort_unstable();
    tags.dedup();
    assert_eq!(
        tags,
        (0x01u8..=0x0E).collect::<Vec<u8>>(),
        "the 14 peer verbs must occupy tags 1..=14, each exactly once"
    );
}

#[test]
fn every_reply_variant_has_a_pinned_tag() {
    use control_proto::RegsView;

    let samples = vec![
        Reply::Hello(sample_caps()),
        Reply::Unit,
        Reply::Stop(StopReason::Quiescent { vtime: Moment(0) }),
        Reply::Hash([0u8; 32]),
        Reply::SdkEvents(vec![]),
        Reply::Console {
            total: 0,
            chunk: vec![],
        },
        Reply::Bytes(vec![]),
        Reply::Regs(RegsView {
            version: RegsView::VERSION,
            gpr: [0; 16],
            rip: 0,
            rflags: 0,
            seg: [0; 6],
            cr0: 0,
            cr3: 0,
            cr4: 0,
            moment: Moment(0),
            vtime: 0,
        }),
        Reply::ExecResult {
            output: vec![],
            ok: true,
        },
        Reply::Snapshot {
            id: SnapId(1),
            at: Moment(0),
            sdk_events: 0,
            tainted: false,
        },
        Reply::Recorded(Reproducer {
            blob_version: 1,
            bytes: vec![],
        }),
    ];

    let mut tags = Vec::new();
    for reply in &samples {
        let expected: u8 = match reply {
            Reply::Hello(_) => 0x01,
            Reply::Unit => 0x03,
            Reply::Stop(_) => 0x04,
            Reply::Hash(_) => 0x05,
            Reply::SdkEvents(_) => 0x06,
            Reply::Bytes(_) => 0x07,
            Reply::Regs(_) => 0x08,
            Reply::ExecResult { .. } => 0x09,
            Reply::Snapshot { .. } => 0x0A,
            Reply::Recorded(_) => 0x0B,
            Reply::Console { .. } => 0x0C,
        };
        let mut buf = Vec::new();
        encode_reply(1, &Ok(reply.clone()), &mut buf).expect("encode");
        assert_eq!(body_tag(&buf), 0x00, "{reply:?}: RESULT_OK byte drifted");
        assert_eq!(buf[15], expected, "{reply:?}: reply tag drifted");
        tags.push(expected);
    }

    tags.sort_unstable();
    tags.dedup();
    let mut expected_tags: Vec<u8> = (0x01u8..=0x0C).collect();
    expected_tags.retain(|t| *t != 0x02);
    assert_eq!(
        tags, expected_tags,
        "every reply must occupy its own tag, and 0x02 must stay retired"
    );
}
