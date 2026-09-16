// SPDX-License-Identifier: AGPL-3.0-or-later

use thiserror::Error;

use crate::SnapId;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Error)]
pub enum ProtocolError {
    #[error("short or malformed frame body")]
    ShortFrame,
    #[error("bad frame magic")]
    BadMagic,
    #[error("unsupported wire-format version")]
    BadVersion,
    #[error("frame body length exceeds MAX_FRAME_LEN")]
    BadLength,
}

#[derive(Clone, PartialEq, Eq, Debug, Error)]
pub enum ControlError {
    #[error("unknown snapshot {0:?}")]
    UnknownSnapshot(SnapId),
    #[error("restore failed")]
    RestoreFailed,
    #[error("snapshot while a decision is armed")]
    SnapshotWhileArmed,
    #[error("not at a quiescent point")]
    NotQuiescent,
    #[error("snapshot refused: {reason}")]
    SnapshotRefused { reason: String },
    #[error("unsupported environment blob version {0}")]
    BadEnvVersion(u16),
    #[error("malformed environment blob")]
    MalformedEnvironment,
    #[error("resolve with no outstanding decision")]
    ResolveWithoutDecision,
    #[error("malformed or wrong-class resolve answer")]
    MalformedAnswer,
    #[error("verb not supported by this backend")]
    Unsupported,
    #[error(
        "perturb CorruptMemory gpa {gpa:#x} + 8 is out of range (guest RAM is {ram_len} bytes)"
    )]
    PerturbOutOfRange { gpa: u64, ram_len: u64 },
    #[error("perturb Moment {at} is behind the current V-time {floor}")]
    PerturbPastMoment { at: u64, floor: u64 },
    #[error("perturb Moment {at} already carries a staged fault (one fault per Moment)")]
    PerturbMomentTaken { at: u64 },
    #[error("run overshot staged Moment {moment} (now at V-time {vtime}); schedule unsatisfiable")]
    ScheduleUnsatisfiable { moment: u64, vtime: u64 },
    #[error("perturb InjectInterrupt vector {vector} is architecturally reserved (< 16)")]
    PerturbReservedVector { vector: u8 },
    #[error("read [{gpa:#x}, {gpa:#x}+{len}) is out of range (guest RAM is {ram_len} bytes)")]
    ReadOutOfRange { gpa: u64, len: u32, ram_len: u64 },
    #[error("read len {len} exceeds the per-call cap of {cap} bytes")]
    ReadTooLarge { len: u32, cap: u32 },
    #[error("timeline is tainted by an exec improvisation; refusing to mint a reproducer")]
    Tainted,
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
}
