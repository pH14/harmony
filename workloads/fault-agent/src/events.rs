// SPDX-License-Identifier: AGPL-3.0-or-later

pub const EVENT_CMD_KILL: u64 = 1;
pub const EVENT_CMD_PARK: u64 = 2;
pub const EVENT_CMD_PARK_STATUS: u64 = 3;
pub const EVENT_REPORT_HELLO: u64 = 0x4841_524d_4f4e_5945;
pub const EVENT_PROTOCOL_VERSION: u64 = 1;
pub const EVENT_CONTROL_FRAME_SIZE: usize = 24;
pub const EVENT_REPORT_SIZE: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    ArmKill { rarity: u8, start: u64 },
    DisarmKill,
    ArmPark { rarity: u8, hold_nanos: u64 },
    DisarmPark,
    ParkStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reply {
    Echo(Command),
    ParkStatus { fires: u64, armed: bool },
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ProtocolError {
    #[error("event frame has the wrong size")]
    WrongSize,
    #[error("event frame has an unknown command")]
    UnknownCommand,
    #[error("event reply does not acknowledge the request")]
    MismatchedReply,
    #[error("event reply has an invalid armed flag")]
    InvalidArmed,
    #[error("event report has an invalid rarity")]
    InvalidRarity,
    #[error("event hello has an unsupported protocol version")]
    InvalidVersion,
}

#[must_use]
pub fn encode_command(command: Command) -> [u8; EVENT_CONTROL_FRAME_SIZE] {
    let mut frame = [0_u8; EVENT_CONTROL_FRAME_SIZE];
    let (kind, first, second) = match command {
        Command::ArmKill { rarity, .. } => (EVENT_CMD_KILL, u64::from(rarity), 1),
        Command::DisarmKill => (EVENT_CMD_KILL, 0, 0),
        Command::ArmPark { rarity, hold_nanos } => (EVENT_CMD_PARK, u64::from(rarity), hold_nanos),
        Command::DisarmPark => (EVENT_CMD_PARK, 0, 0),
        Command::ParkStatus => (EVENT_CMD_PARK_STATUS, 0, 0),
    };
    frame[..8].copy_from_slice(&kind.to_le_bytes());
    frame[8..16].copy_from_slice(&first.to_le_bytes());
    frame[16..].copy_from_slice(&second.to_le_bytes());
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
        if second > 1 {
            return Err(ProtocolError::InvalidArmed);
        }
        return Ok(Reply::ParkStatus {
            fires: first,
            armed: second != 0,
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
    if rarity >= fault_policy::EVENT_RARITY_LIMIT {
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

    #[test]
    fn control_frames_are_three_little_endian_words() {
        assert_eq!(
            encode_command(Command::ArmKill {
                rarity: 7,
                start: 0,
            }),
            [
                1, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0
            ]
        );
        assert_eq!(
            encode_command(Command::ArmPark {
                rarity: 2,
                hold_nanos: 9,
            }),
            [
                2, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0
            ]
        );
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
    }

    #[test]
    fn park_status_carries_runtime_fires_and_armed_state() {
        let mut frame = encode_command(Command::ParkStatus);
        frame[8..16].copy_from_slice(&3_u64.to_le_bytes());
        frame[16..24].copy_from_slice(&1_u64.to_le_bytes());
        assert_eq!(
            decode_reply(Command::ParkStatus, &frame),
            Ok(Reply::ParkStatus {
                fires: 3,
                armed: true
            })
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
        invalid[8..16].copy_from_slice(&2_u64.to_le_bytes());
        assert_eq!(decode_report(&invalid), Err(ProtocolError::InvalidVersion));
    }
}
