// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared proptest strategies that build arbitrary **in-bounds**
//! `Request` / `Reply` / `ControlError` values (small payloads, so a generated
//! frame body never approaches `MAX_FRAME_LEN`). Used by the round-trip,
//! streaming, and adversarial property tests.

#![allow(dead_code)] // each test binary uses a subset of these helpers.

use control_proto::{
    Answer, CapFlags, Caps, ControlError, CoverageGeometry, CrashInfo, CrashKind, DecisionId,
    Environment, EventRef, HashScope, HostFault, Moment, ProtocolError, RegsView, Reply, Request,
    SnapId, StopConditions, StopMask, StopReason, VTime,
};
use proptest::prelude::*;

/// Small byte blobs keep generated frame bodies well under `MAX_FRAME_LEN`.
const MAX_BLOB: usize = 64;

fn arb_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..MAX_BLOB)
}

pub fn arb_caps() -> impl Strategy<Value = Caps> {
    (
        any::<u16>(),
        any::<u16>(),
        any::<u16>(),
        any::<u32>(),
        any::<u8>(),
        any::<u32>(),
    )
        .prop_map(|(pv, lo, hi, map_bytes, producer, flags)| Caps {
            protocol_version: pv,
            env_version_min: lo,
            env_version_max: hi,
            coverage: CoverageGeometry {
                map_bytes,
                producer,
            },
            flags: CapFlags(flags),
        })
}

pub fn arb_environment() -> impl Strategy<Value = Environment> {
    (any::<u16>(), arb_bytes()).prop_map(|(blob_version, bytes)| Environment {
        blob_version,
        bytes,
    })
}

fn arb_hash_scope() -> impl Strategy<Value = HashScope> {
    prop_oneof![
        Just(HashScope::Whole),
        Just(HashScope::Disk),
        (any::<u64>(), any::<u64>()).prop_map(|(base, len)| HashScope::Region { base, len }),
    ]
}

fn arb_stop_conditions() -> impl Strategy<Value = StopConditions> {
    (proptest::option::of(any::<u64>()), any::<u32>()).prop_map(|(deadline, on)| StopConditions {
        deadline: deadline.map(VTime),
        on: StopMask(on),
    })
}

pub fn arb_request() -> impl Strategy<Value = Request> {
    prop_oneof![
        arb_caps().prop_map(Request::Hello),
        Just(Request::Snapshot),
        any::<u64>().prop_map(|s| Request::Drop(SnapId(s))),
        (any::<u64>(), arb_environment()).prop_map(|(snap, env)| Request::Branch {
            snap: SnapId(snap),
            env
        }),
        any::<u64>().prop_map(|s| Request::Replay(SnapId(s))),
        (arb_stop_conditions(), proptest::option::of(arb_bytes())).prop_map(|(until, resolve)| {
            Request::Run {
                until,
                resolve: resolve.map(Answer),
            }
        }),
        arb_hash_scope().prop_map(|scope| Request::Hash { scope }),
        (arb_bytes(), any::<u64>()).prop_map(|(fault, at)| Request::Perturb {
            fault: HostFault(fault),
            at: Moment(at),
        }),
        any::<u32>().prop_map(|offset| Request::SdkEvents { offset }),
        (any::<u64>(), any::<u32>()).prop_map(|(gpa, len)| Request::Read { gpa, len }),
        Just(Request::Regs),
        ("[ -~]{0,64}", any::<u64>()).prop_map(|(cmd, deadline)| Request::Exec {
            cmd,
            deadline: VTime(deadline),
        }),
        Just(Request::RecordedEnv),
    ]
}

pub fn arb_regs_view() -> impl Strategy<Value = RegsView> {
    (
        any::<u16>(),
        prop::array::uniform16(any::<u64>()),
        any::<u64>(),
        any::<u64>(),
        prop::array::uniform6(any::<u16>()),
        (any::<u64>(), any::<u64>(), any::<u64>()),
        (any::<u64>(), any::<u64>()),
    )
        .prop_map(
            |(version, gpr, rip, rflags, seg, (cr0, cr3, cr4), (moment, vtime))| RegsView {
                version,
                gpr,
                rip,
                rflags,
                seg,
                cr0,
                cr3,
                cr4,
                moment: Moment(moment),
                vtime,
            },
        )
}

fn arb_crash_kind() -> impl Strategy<Value = CrashKind> {
    prop_oneof![
        Just(CrashKind::Panic),
        Just(CrashKind::TripleFault),
        Just(CrashKind::Shutdown),
    ]
}

fn arb_stop_reason() -> impl Strategy<Value = StopReason> {
    prop_oneof![
        any::<u64>().prop_map(|v| StopReason::Deadline { vtime: VTime(v) }),
        any::<u64>().prop_map(|v| StopReason::Quiescent { vtime: VTime(v) }),
        (any::<u64>(), arb_crash_kind(), arb_bytes()).prop_map(|(v, kind, detail)| {
            StopReason::Crash {
                vtime: VTime(v),
                info: CrashInfo { kind, detail },
            }
        }),
        (any::<u64>(), any::<u64>(), arb_bytes()).prop_map(|(v, id, ctx)| StopReason::Decision {
            vtime: VTime(v),
            id: DecisionId(id),
            ctx,
        }),
        any::<u64>().prop_map(|v| StopReason::SnapshotPoint { vtime: VTime(v) }),
        (any::<u64>(), any::<u32>(), arb_bytes()).prop_map(|(v, id, data)| StopReason::Assertion {
            vtime: VTime(v),
            ev: EventRef { id, data },
        }),
    ]
}

fn arb_protocol_error() -> impl Strategy<Value = ProtocolError> {
    prop_oneof![
        Just(ProtocolError::ShortFrame),
        Just(ProtocolError::BadMagic),
        Just(ProtocolError::BadVersion),
        Just(ProtocolError::BadLength),
    ]
}

fn arb_control_error() -> impl Strategy<Value = ControlError> {
    prop_oneof![
        any::<u64>().prop_map(|s| ControlError::UnknownSnapshot(SnapId(s))),
        Just(ControlError::RestoreFailed),
        Just(ControlError::SnapshotWhileArmed),
        Just(ControlError::NotQuiescent),
        any::<u16>().prop_map(ControlError::BadEnvVersion),
        Just(ControlError::MalformedEnvironment),
        Just(ControlError::ResolveWithoutDecision),
        Just(ControlError::MalformedAnswer),
        Just(ControlError::Unsupported),
        (any::<u64>(), any::<u64>())
            .prop_map(|(gpa, ram_len)| ControlError::PerturbOutOfRange { gpa, ram_len }),
        (any::<u64>(), any::<u64>())
            .prop_map(|(at, floor)| ControlError::PerturbPastMoment { at, floor }),
        any::<u64>().prop_map(|at| ControlError::PerturbMomentTaken { at }),
        (any::<u64>(), any::<u64>())
            .prop_map(|(moment, vtime)| ControlError::ScheduleUnsatisfiable { moment, vtime }),
        Just(ControlError::NotSynchronized),
        any::<u8>().prop_map(|vector| ControlError::PerturbReservedVector { vector }),
        (any::<u64>(), any::<u32>(), any::<u64>())
            .prop_map(|(gpa, len, ram_len)| ControlError::ReadOutOfRange { gpa, len, ram_len }),
        (any::<u32>(), any::<u32>()).prop_map(|(len, cap)| ControlError::ReadTooLarge { len, cap }),
        Just(ControlError::Tainted),
        arb_protocol_error().prop_map(ControlError::Protocol),
    ]
}

fn arb_reply() -> impl Strategy<Value = Reply> {
    prop_oneof![
        arb_caps().prop_map(Reply::Hello),
        any::<u64>().prop_map(|s| Reply::SnapId(SnapId(s))),
        Just(Reply::Unit),
        arb_stop_reason().prop_map(Reply::Stop),
        proptest::array::uniform32(any::<u8>()).prop_map(Reply::Hash),
        arb_bytes().prop_map(Reply::Bytes),
        arb_regs_view().prop_map(Reply::Regs),
        (arb_bytes(), any::<bool>()).prop_map(|(output, ok)| Reply::ExecResult { output, ok }),
        (any::<u64>(), any::<bool>()).prop_map(|(id, tainted)| Reply::Snapshot {
            id: SnapId(id),
            tainted
        }),
        arb_environment().prop_map(Reply::Recorded),
    ]
}

pub fn arb_reply_result() -> impl Strategy<Value = Result<Reply, ControlError>> {
    prop_oneof![arb_reply().prop_map(Ok), arb_control_error().prop_map(Err)]
}
