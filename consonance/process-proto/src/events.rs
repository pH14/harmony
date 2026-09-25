// SPDX-License-Identifier: AGPL-3.0-or-later

use core::fmt;

pub const EVENT_CMD_KILL: u64 = 1;
pub const EVENT_CMD_PARK: u64 = 2;
pub const EVENT_CMD_PARK_STATUS: u64 = 3;
pub const EVENT_CMD_COVERAGE_STATUS: u64 = 4;
pub const EVENT_REPORT_HELLO: u64 = 0x4841_524d_4f4e_5945;
pub const EVENT_PROTOCOL_VERSION: u64 = 5;
pub const EVENT_CONTROL_FRAME_SIZE: usize = 40;
pub const EVENT_REPORT_SIZE: usize = 16;
pub const EVENT_RARITY_LIMIT: u8 = 64;
pub const EVENT_PARK_EDGE_LIMIT: u32 = 1 << 24;
pub const EVENT_PARK_STATUS_ARMED: u64 = 1;
pub const EVENT_PARK_STATUS_HELD: u64 = 2;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ParkTarget {
    pub start: u64,
    pub end: u64,
}

impl ParkTarget {
    #[must_use]
    pub fn new(start: u64, end: u64) -> Option<Self> {
        (start < end).then_some(Self { start, end })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    ArmKill {
        rarity: u8,
        start: u64,
    },
    DisarmKill,
    ArmPark {
        edges: u32,
        hold_nanos: u64,
        target: Option<ParkTarget>,
    },
    DisarmPark,
    ParkStatus,
    CoverageStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reply {
    Echo(Command),
    ParkStatus { fires: u64, armed: bool, held: bool },
    CoverageStatus { crossings: u64, digest: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KillReport {
    pub rarity: u8,
    pub site: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Report {
    Hello,
    Kill(KillReport),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    WrongSize,
    UnknownCommand,
    MismatchedReply,
    InvalidParkStatus,
    InvalidRarity,
    InvalidVersion,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::WrongSize => "event frame has the wrong size",
            Self::UnknownCommand => "event frame has an unknown command",
            Self::MismatchedReply => "event reply does not acknowledge the request",
            Self::InvalidParkStatus => "event reply has invalid park status flags",
            Self::InvalidRarity => "event report has an invalid rarity",
            Self::InvalidVersion => "event hello has an unsupported protocol version",
        };
        f.write_str(message)
    }
}

impl std::error::Error for ProtocolError {}

#[must_use]
pub fn encode_command(command: Command) -> [u8; EVENT_CONTROL_FRAME_SIZE] {
    let mut frame = [0_u8; EVENT_CONTROL_FRAME_SIZE];
    let (kind, first, second, target) = match command {
        Command::ArmKill { rarity, .. } => (EVENT_CMD_KILL, u64::from(rarity), 1, None),
        Command::DisarmKill => (EVENT_CMD_KILL, 0, 0, None),
        Command::ArmPark {
            edges,
            hold_nanos,
            target,
        } => (EVENT_CMD_PARK, u64::from(edges), hold_nanos, target),
        Command::DisarmPark => (EVENT_CMD_PARK, 0, 0, None),
        Command::ParkStatus => (EVENT_CMD_PARK_STATUS, 0, 0, None),
        Command::CoverageStatus => (EVENT_CMD_COVERAGE_STATUS, 0, 0, None),
    };
    let (target_start, target_end) = target.map_or((0, 0), |target| (target.start, target.end));
    frame[..8].copy_from_slice(&kind.to_le_bytes());
    frame[8..16].copy_from_slice(&first.to_le_bytes());
    frame[16..24].copy_from_slice(&second.to_le_bytes());
    frame[24..32].copy_from_slice(&target_start.to_le_bytes());
    frame[32..].copy_from_slice(&target_end.to_le_bytes());
    frame
}

pub fn decode_reply(expected: Command, frame: &[u8]) -> Result<Reply, ProtocolError> {
    if frame.len() != EVENT_CONTROL_FRAME_SIZE {
        return Err(ProtocolError::WrongSize);
    }
    let kind = read_u64(frame, 0);
    let first = read_u64(frame, 8);
    let second = read_u64(frame, 16);
    if matches!(expected, Command::ParkStatus) {
        if kind != EVENT_CMD_PARK_STATUS {
            return Err(ProtocolError::MismatchedReply);
        }
        if second & !(EVENT_PARK_STATUS_ARMED | EVENT_PARK_STATUS_HELD) != 0 {
            return Err(ProtocolError::InvalidParkStatus);
        }
        return Ok(Reply::ParkStatus {
            fires: first,
            armed: second & EVENT_PARK_STATUS_ARMED != 0,
            held: second & EVENT_PARK_STATUS_HELD != 0,
        });
    }
    if matches!(expected, Command::CoverageStatus) {
        if kind != EVENT_CMD_COVERAGE_STATUS {
            return Err(ProtocolError::MismatchedReply);
        }
        return Ok(Reply::CoverageStatus {
            crossings: first,
            digest: second,
        });
    }
    if frame != encode_command(expected) {
        return Err(if kind == EVENT_CMD_KILL || kind == EVENT_CMD_PARK {
            ProtocolError::MismatchedReply
        } else {
            ProtocolError::UnknownCommand
        });
    }
    Ok(Reply::Echo(expected))
}

#[must_use]
pub fn encode_kill_report(report: KillReport) -> [u8; EVENT_REPORT_SIZE] {
    let mut frame = [0_u8; EVENT_REPORT_SIZE];
    frame[..8].copy_from_slice(&u64::from(report.rarity).to_le_bytes());
    frame[8..].copy_from_slice(&report.site.to_le_bytes());
    frame
}

#[must_use]
pub fn encode_hello() -> [u8; EVENT_REPORT_SIZE] {
    let mut frame = [0_u8; EVENT_REPORT_SIZE];
    frame[..8].copy_from_slice(&EVENT_REPORT_HELLO.to_le_bytes());
    frame[8..].copy_from_slice(&EVENT_PROTOCOL_VERSION.to_le_bytes());
    frame
}

pub fn decode_report(frame: &[u8]) -> Result<Report, ProtocolError> {
    if frame.len() != EVENT_REPORT_SIZE {
        return Err(ProtocolError::WrongSize);
    }
    if read_u64(frame, 0) == EVENT_REPORT_HELLO {
        if read_u64(frame, 8) != EVENT_PROTOCOL_VERSION {
            return Err(ProtocolError::InvalidVersion);
        }
        return Ok(Report::Hello);
    }
    decode_kill_report(frame).map(Report::Kill)
}

pub fn decode_kill_report(frame: &[u8]) -> Result<KillReport, ProtocolError> {
    if frame.len() != EVENT_REPORT_SIZE {
        return Err(ProtocolError::WrongSize);
    }
    let rarity = read_u64(frame, 0);
    let rarity = u8::try_from(rarity).map_err(|_| ProtocolError::InvalidRarity)?;
    if rarity >= EVENT_RARITY_LIMIT {
        return Err(ProtocolError::InvalidRarity);
    }
    Ok(KillReport {
        rarity,
        site: read_u64(frame, 8),
    })
}

fn read_u64(frame: &[u8], offset: usize) -> u64 {
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&frame[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(values: [u64; 5]) -> [u8; EVENT_CONTROL_FRAME_SIZE] {
        let mut frame = [0_u8; EVENT_CONTROL_FRAME_SIZE];
        for (chunk, value) in frame.chunks_exact_mut(8).zip(values) {
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        frame
    }

    #[test]
    fn control_frames_are_five_little_endian_words() {
        assert_eq!(
            encode_command(Command::ArmKill {
                rarity: 7,
                start: 0,
            }),
            words([1, 7, 1, 0, 0])
        );
        assert_eq!(
            encode_command(Command::ArmPark {
                edges: 2,
                hold_nanos: 9,
                target: None,
            }),
            words([2, 2, 9, 0, 0])
        );
        assert_eq!(
            encode_command(Command::ArmPark {
                edges: 1,
                hold_nanos: 9,
                target: ParkTarget::new(0x40, 0x80),
            }),
            words([2, 1, 9, 0x40, 0x80])
        );
        assert_eq!(ParkTarget::new(5, 5), None);
    }

    #[test]
    fn only_an_exact_echo_acknowledges_an_arm() {
        let command = Command::ArmKill {
            rarity: 4,
            start: 17,
        };
        let frame = encode_command(command);
        assert_eq!(decode_reply(command, &frame), Ok(Reply::Echo(command)));
        let mut wrong = frame;
        wrong[16] = 0;
        assert_eq!(
            decode_reply(command, &wrong),
            Err(ProtocolError::MismatchedReply)
        );
        let park = Command::ArmPark {
            edges: 4,
            hold_nanos: 17,
            target: ParkTarget::new(8, 16),
        };
        let mut wrong = encode_command(park);
        wrong[16] = 18;
        assert_eq!(
            decode_reply(park, &wrong),
            Err(ProtocolError::MismatchedReply)
        );
        let mut wrong = encode_command(park);
        wrong[32] = 17;
        assert_eq!(
            decode_reply(park, &wrong),
            Err(ProtocolError::MismatchedReply)
        );
    }

    #[test]
    fn park_status_carries_runtime_fires_and_armed_and_held_flags() {
        let mut frame = encode_command(Command::ParkStatus);
        frame[8..16].copy_from_slice(&3_u64.to_le_bytes());
        for (flags, armed, held) in [
            (0, false, false),
            (EVENT_PARK_STATUS_ARMED, true, false),
            (EVENT_PARK_STATUS_HELD, false, true),
            (EVENT_PARK_STATUS_ARMED | EVENT_PARK_STATUS_HELD, true, true),
        ] {
            frame[16..24].copy_from_slice(&flags.to_le_bytes());
            assert_eq!(
                decode_reply(Command::ParkStatus, &frame),
                Ok(Reply::ParkStatus {
                    fires: 3,
                    armed,
                    held
                })
            );
        }
        frame[16..24].copy_from_slice(&4_u64.to_le_bytes());
        assert_eq!(
            decode_reply(Command::ParkStatus, &frame),
            Err(ProtocolError::InvalidParkStatus)
        );
    }

    #[test]
    fn coverage_status_carries_bucket_crossings_and_digest() {
        let mut frame = encode_command(Command::CoverageStatus);
        frame[8..16].copy_from_slice(&5_u64.to_le_bytes());
        frame[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(
            decode_reply(Command::CoverageStatus, &frame),
            Ok(Reply::CoverageStatus {
                crossings: 5,
                digest: u64::MAX
            })
        );
        assert_eq!(
            decode_reply(
                Command::CoverageStatus,
                &encode_command(Command::ParkStatus)
            ),
            Err(ProtocolError::MismatchedReply)
        );
    }

    #[test]
    fn kill_reports_require_a_valid_rarity() {
        let report = KillReport {
            rarity: 0,
            site: 0xfeed,
        };
        let frame = encode_kill_report(report);
        assert_eq!(decode_kill_report(&frame), Ok(report));
        let mut invalid = frame;
        invalid[..8].copy_from_slice(&256_u64.to_le_bytes());
        assert_eq!(
            decode_kill_report(&invalid),
            Err(ProtocolError::InvalidRarity)
        );
        let mut width = frame;
        width[..8].copy_from_slice(&64_u64.to_le_bytes());
        assert_eq!(
            decode_kill_report(&width),
            Err(ProtocolError::InvalidRarity)
        );
    }

    #[test]
    fn hello_reports_are_distinct_from_kill_reports() {
        let frame = encode_hello();
        assert_eq!(decode_report(&frame), Ok(Report::Hello));
        let mut invalid = frame;
        invalid[8..16].copy_from_slice(&1_u64.to_le_bytes());
        assert_eq!(decode_report(&invalid), Err(ProtocolError::InvalidVersion));
    }

    #[test]
    fn protocol_errors_describe_the_failed_contract() {
        let cases = [
            (ProtocolError::WrongSize, "event frame has the wrong size"),
            (
                ProtocolError::UnknownCommand,
                "event frame has an unknown command",
            ),
            (
                ProtocolError::MismatchedReply,
                "event reply does not acknowledge the request",
            ),
            (
                ProtocolError::InvalidParkStatus,
                "event reply has invalid park status flags",
            ),
            (
                ProtocolError::InvalidRarity,
                "event report has an invalid rarity",
            ),
            (
                ProtocolError::InvalidVersion,
                "event hello has an unsupported protocol version",
            ),
        ];
        for (error, message) in cases {
            assert_eq!(error.to_string(), message);
        }
    }
}
