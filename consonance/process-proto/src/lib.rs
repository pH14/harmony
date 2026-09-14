// SPDX-License-Identifier: AGPL-3.0-or-later

use core::fmt;

pub const PROCESS_CLASS: u16 = 6;
pub const CLASS_PROCESS: u16 = PROCESS_CLASS;
pub const STANDING_NAMESPACE: u16 = 9;

const PAUSE: u8 = 9;
const KILL: u8 = 10;
const RESTART: u8 = 11;
const RUN_HOOK: u8 = 17;
const PARK: u8 = 19;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ProcessAction {
    Pause(u64),
    Kill,
    Restart,
    RunHook(u32),
    Park {
        addr: u64,
        hits: u32,
        hold_nanos: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Window {
    pub class: u16,
    pub target: Vec<u8>,
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ProcessWindow {
    pub node: u16,
    pub action: ProcessAction,
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireError {
    Malformed,
    TargetTooLarge,
    TooManyWindows,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Malformed => "malformed process wire data",
            Self::TargetTooLarge => "process target exceeds u16 length",
            Self::TooManyWindows => "process window count exceeds u32",
        };
        f.write_str(text)
    }
}

impl std::error::Error for WireError {}

impl ProcessAction {
    #[must_use]
    pub fn encode(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(25);
        write_action(&mut out, self);
        out
    }

    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let mut reader = Reader::new(bytes);
        let action = read_action(&mut reader).ok()?;
        reader.at_end().then_some(action)
    }
}

#[must_use]
pub fn encode_target(node: u16, action: &ProcessAction) -> Vec<u8> {
    let mut out = node.to_le_bytes().to_vec();
    write_action(&mut out, *action);
    out
}

#[must_use]
pub fn decode_target(bytes: &[u8]) -> Option<(u16, ProcessAction)> {
    let mut reader = Reader::new(bytes);
    let node = reader.u16().ok()?;
    let action = read_action(&mut reader).ok()?;
    reader.at_end().then_some((node, action))
}

pub use decode_target as decode_process_target;
pub use encode_target as encode_process_target;

pub fn encode_windows(windows: &[Window]) -> Result<Vec<u8>, WireError> {
    let count = u32::try_from(windows.len()).map_err(|_| WireError::TooManyWindows)?;
    let mut out = Vec::new();
    put_u32(&mut out, count);
    for window in windows {
        write_window(&mut out, window)?;
    }
    Ok(out)
}

pub fn decode_windows(bytes: &[u8]) -> Result<Vec<Window>, WireError> {
    let mut reader = Reader::new(bytes);
    let windows = read_windows(&mut reader)?;
    if reader.at_end() {
        Ok(windows)
    } else {
        Err(WireError::Malformed)
    }
}

pub fn encode_standing(moment: u64, windows: &[Window]) -> Result<Vec<u8>, WireError> {
    let mut out = moment.to_le_bytes().to_vec();
    out.extend(encode_windows(windows)?);
    Ok(out)
}

pub fn decode_standing(bytes: &[u8]) -> Result<(u64, Vec<Window>), WireError> {
    let mut reader = Reader::new(bytes);
    let moment = reader.u64()?;
    let windows = read_windows(&mut reader)?;
    if reader.at_end() {
        Ok((moment, windows))
    } else {
        Err(WireError::Malformed)
    }
}

pub fn encode_process_windows(
    moment: u64,
    windows: &[ProcessWindow],
) -> Result<Vec<u8>, WireError> {
    let raw = windows
        .iter()
        .map(|window| Window {
            class: PROCESS_CLASS,
            target: encode_target(window.node, &window.action),
            start: window.start,
            end: window.end,
        })
        .collect::<Vec<_>>();
    encode_standing(moment, &raw)
}

pub fn decode_process_windows(bytes: &[u8]) -> Result<(u64, Vec<ProcessWindow>), WireError> {
    let (moment, windows) = decode_standing(bytes)?;
    let mut process = Vec::new();
    for window in windows {
        if window.class != PROCESS_CLASS {
            continue;
        }
        let Some((node, action)) = decode_target(&window.target) else {
            continue;
        };
        process.push(ProcessWindow {
            node,
            action,
            start: window.start,
            end: window.end,
        });
    }
    Ok((moment, process))
}

fn write_action(out: &mut Vec<u8>, action: ProcessAction) {
    match action {
        ProcessAction::Pause(nanos) => {
            out.push(PAUSE);
            put_u64(out, nanos);
        }
        ProcessAction::Kill => out.push(KILL),
        ProcessAction::Restart => out.push(RESTART),
        ProcessAction::RunHook(id) => {
            out.push(RUN_HOOK);
            put_u32(out, id);
        }
        ProcessAction::Park {
            addr,
            hits,
            hold_nanos,
        } => {
            out.push(PARK);
            put_u64(out, addr);
            put_u32(out, hits);
            put_u64(out, hold_nanos);
        }
    }
}

fn read_action(reader: &mut Reader<'_>) -> Result<ProcessAction, WireError> {
    match reader.u8()? {
        PAUSE => Ok(ProcessAction::Pause(reader.u64()?)),
        KILL => Ok(ProcessAction::Kill),
        RESTART => Ok(ProcessAction::Restart),
        RUN_HOOK => Ok(ProcessAction::RunHook(reader.u32()?)),
        PARK => Ok(ProcessAction::Park {
            addr: reader.u64()?,
            hits: reader.u32()?,
            hold_nanos: reader.u64()?,
        }),
        _ => Err(WireError::Malformed),
    }
}

fn write_window(out: &mut Vec<u8>, window: &Window) -> Result<(), WireError> {
    let length = u16::try_from(window.target.len()).map_err(|_| WireError::TargetTooLarge)?;
    put_u16(out, window.class);
    put_u16(out, length);
    out.extend_from_slice(&window.target);
    put_u64(out, window.start);
    put_u64(out, window.end);
    Ok(())
}

fn read_windows(reader: &mut Reader<'_>) -> Result<Vec<Window>, WireError> {
    let count = reader.u32()?;
    let minimum = usize::try_from(count)
        .ok()
        .and_then(|count| count.checked_mul(20))
        .ok_or(WireError::Malformed)?;
    if minimum > reader.remaining() {
        return Err(WireError::Malformed);
    }
    let mut windows = Vec::new();
    for _ in 0..count {
        let class = reader.u16()?;
        let length = usize::from(reader.u16()?);
        let target = reader.bytes(length)?.to_vec();
        let start = reader.u64()?;
        let end = reader.u64()?;
        windows.push(Window {
            class,
            target,
            start,
            end,
        });
    }
    Ok(windows)
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn at_end(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn bytes(&mut self, length: usize) -> Result<&'a [u8], WireError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(WireError::Malformed)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(WireError::Malformed)?;
        self.offset = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, WireError> {
        let bytes = self.bytes(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, WireError> {
        let bytes = self.bytes(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, WireError> {
        let bytes = self.bytes(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actions() -> [ProcessAction; 5] {
        [
            ProcessAction::Pause(1234),
            ProcessAction::Kill,
            ProcessAction::Restart,
            ProcessAction::RunHook(7),
            ProcessAction::Park {
                addr: 0x4b_0e86,
                hits: 28,
                hold_nanos: 2_000_000,
            },
        ]
    }

    #[test]
    fn process_actions_round_trip_and_keep_the_existing_tags() {
        let expected = [
            vec![9, 0xd2, 0x04, 0, 0, 0, 0, 0, 0],
            vec![10],
            vec![11],
            vec![17, 7, 0, 0, 0],
            vec![
                19, 0x86, 0x0e, 0x4b, 0, 0, 0, 0, 0, 28, 0, 0, 0, 0x80, 0x84, 0x1e, 0, 0, 0, 0, 0,
            ],
        ];
        for (action, expected) in actions().into_iter().zip(expected) {
            assert_eq!(action.encode(), expected);
            assert_eq!(ProcessAction::decode(&expected), Some(action));
        }
    }

    #[test]
    fn process_target_preserves_node_prefix_and_rejects_trailing_data() {
        let bytes = encode_target(3, &ProcessAction::RunHook(7));
        assert_eq!(bytes, [3, 0, 17, 7, 0, 0, 0]);
        assert_eq!(decode_target(&bytes), Some((3, ProcessAction::RunHook(7))));
        assert_eq!(decode_target(&bytes[..bytes.len() - 1]), None);
        let mut extra = bytes;
        extra.push(0);
        assert_eq!(decode_target(&extra), None);
    }

    #[test]
    fn standing_windows_round_trip_and_filter_process_class() {
        let windows = [
            Window {
                class: PROCESS_CLASS,
                target: encode_target(1, &ProcessAction::Pause(20)),
                start: 5,
                end: 10,
            },
            Window {
                class: 4,
                target: vec![1, 2],
                start: 0,
                end: u64::MAX,
            },
        ];
        let bytes = encode_standing(77, &windows).unwrap();
        assert_eq!(decode_standing(&bytes), Ok((77, windows.to_vec())));
        assert_eq!(
            decode_process_windows(&bytes).unwrap(),
            (
                77,
                vec![ProcessWindow {
                    node: 1,
                    action: ProcessAction::Pause(20),
                    start: 5,
                    end: 10,
                }]
            )
        );
    }

    #[test]
    fn malformed_and_trailing_windows_are_rejected() {
        let bytes = encode_standing(
            1,
            &[Window {
                class: PROCESS_CLASS,
                target: vec![1],
                start: 2,
                end: 3,
            }],
        )
        .unwrap();
        for length in 0..bytes.len() {
            assert!(
                decode_standing(&bytes[..length]).is_err(),
                "length {length}"
            );
        }
        let mut extra = bytes;
        extra.push(0);
        assert_eq!(decode_standing(&extra), Err(WireError::Malformed));
        assert_eq!(decode_windows(&[1, 0, 0, 0]), Err(WireError::Malformed));
    }

    #[test]
    fn typed_process_windows_preserve_moment_and_all_actions() {
        let windows: Vec<_> = actions()
            .into_iter()
            .enumerate()
            .map(|(index, action)| ProcessWindow {
                node: u16::try_from(index).unwrap(),
                action,
                start: 42,
                end: 100,
            })
            .collect();
        let encoded = encode_process_windows(99, &windows).unwrap();
        assert_eq!(decode_process_windows(&encoded), Ok((99, windows)));
    }

    #[test]
    fn empty_targets_and_empty_window_lists_are_valid() {
        assert_eq!(decode_windows(&[0, 0, 0, 0]), Ok(Vec::new()));
        let windows = vec![Window {
            class: 4,
            target: Vec::new(),
            start: 0,
            end: 1,
        }];
        assert_eq!(
            decode_windows(&encode_windows(&windows).unwrap()),
            Ok(windows)
        );
    }

    #[test]
    fn wire_errors_describe_the_failed_contract() {
        for (error, expected) in [
            (WireError::Malformed, "malformed process wire data"),
            (
                WireError::TargetTooLarge,
                "process target exceeds u16 length",
            ),
            (
                WireError::TooManyWindows,
                "process window count exceeds u32",
            ),
        ] {
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn a_target_larger_than_u16_is_rejected() {
        let window = Window {
            class: PROCESS_CLASS,
            target: vec![0; usize::from(u16::MAX) + 1],
            start: 0,
            end: 1,
        };
        assert_eq!(encode_windows(&[window]), Err(WireError::TargetTooLarge));
    }
}
