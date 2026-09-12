// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::codec::{self, Reader};
use crate::error::EnvError;

pub const STANDING_NAMESPACE: u16 = 9;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct StandingWindow {
    pub class: u16,
    pub target: Vec<u8>,
    pub start: u64,
    pub end: u64,
}

impl StandingWindow {
    #[must_use]
    pub fn contains(&self, moment: u64) -> bool {
        moment >= self.start && moment < self.end
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StandingEntry<'a> {
    pub class: u16,
    pub target: &'a [u8],
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Debug)]
pub struct StandingIter<'a> {
    buf: &'a [u8],
    offset: usize,
    remaining: u32,
}

impl<'a> Iterator for StandingIter<'a> {
    type Item = StandingEntry<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let at = self.offset;
        let class = read_u16(self.buf, at).ok()?;
        let len = usize::from(read_u16(self.buf, at + 2).ok()?);
        let target = self.buf.get(at + 4..at + 4 + len)?;
        let start = read_u64(self.buf, at + 4 + len).ok()?;
        let end = read_u64(self.buf, at + 12 + len).ok()?;
        self.offset = at + 20 + len;
        Some(StandingEntry {
            class,
            target,
            start,
            end,
        })
    }
}

impl ExactSizeIterator for StandingIter<'_> {
    fn len(&self) -> usize {
        self.remaining as usize
    }
}

fn write_entries(w: &mut Vec<u8>, entries: &[StandingWindow]) -> Result<(), EnvError> {
    codec::put_u32(
        w,
        u32::try_from(entries.len()).map_err(|_| EnvError::Malformed)?,
    );
    for entry in entries {
        codec::put_u16(w, entry.class);
        codec::put_u16(
            w,
            u16::try_from(entry.target.len()).map_err(|_| EnvError::Malformed)?,
        );
        w.extend_from_slice(&entry.target);
        codec::put_u64(w, entry.start);
        codec::put_u64(w, entry.end);
    }
    Ok(())
}

fn scan_entries(buf: &[u8], offset: usize, count: u32) -> Result<usize, EnvError> {
    let mut at = offset;
    for _ in 0..count {
        let len = usize::from(read_u16(
            buf,
            at.checked_add(2).ok_or(EnvError::Malformed)?,
        )?);
        at = at.checked_add(4 + len + 16).ok_or(EnvError::Malformed)?;
        if at > buf.len() {
            return Err(EnvError::Malformed);
        }
    }
    if at != buf.len() {
        return Err(EnvError::Malformed);
    }
    Ok(at)
}

fn read_u16(buf: &[u8], at: usize) -> Result<u16, EnvError> {
    let end = at.checked_add(2).ok_or(EnvError::Malformed)?;
    let b = buf.get(at..end).ok_or(EnvError::Malformed)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn read_u64(buf: &[u8], at: usize) -> Result<u64, EnvError> {
    let end = at.checked_add(8).ok_or(EnvError::Malformed)?;
    let b = buf.get(at..end).ok_or(EnvError::Malformed)?;
    Ok(u64::from_le_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}

pub fn encode_standing(moment: u64, entries: &[StandingWindow]) -> Result<Vec<u8>, EnvError> {
    let mut w = moment.to_le_bytes().to_vec();
    write_entries(&mut w, entries)?;
    Ok(w)
}

pub fn parse_standing(buf: &[u8]) -> Result<(u64, StandingIter<'_>), EnvError> {
    let mut r = Reader::new(buf);
    let moment = r.u64()?;
    let count = r.u32()?;
    scan_entries(buf, 12, count)?;
    Ok((
        moment,
        StandingIter {
            buf,
            offset: 12,
            remaining: count,
        },
    ))
}

pub fn encode_windows(entries: &[StandingWindow]) -> Result<Vec<u8>, EnvError> {
    let mut w = Vec::new();
    write_entries(&mut w, entries)?;
    Ok(w)
}

pub fn decode_windows(buf: &[u8]) -> Result<Vec<StandingWindow>, EnvError> {
    let mut r = Reader::new(buf);
    let count = r.u32()?;
    scan_entries(buf, 4, count)?;
    let iter = StandingIter {
        buf,
        offset: 4,
        remaining: count,
    };
    Ok(iter
        .map(|e| StandingWindow {
            class: e.class,
            target: e.target.to_vec(),
            start: e.start,
            end: e.end,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DecisionClass, Fault, Span, process_target};

    fn windows() -> Vec<StandingWindow> {
        vec![
            StandingWindow {
                class: DecisionClass::Process.as_u16(),
                target: process_target(0, &Fault::RunHook(3)),
                start: 10,
                end: 40,
            },
            StandingWindow {
                class: DecisionClass::Process.as_u16(),
                target: process_target(1, &Fault::ProcPause(Span(20))),
                start: 20,
                end: 60,
            },
            StandingWindow {
                class: DecisionClass::NetFlow.as_u16(),
                target: Vec::new(),
                start: 0,
                end: u64::MAX,
            },
        ]
    }

    fn owned(buf: &[u8]) -> (u64, Vec<StandingWindow>) {
        let (moment, iter) = parse_standing(buf).expect("well-formed answer");
        (
            moment,
            iter.map(|e| StandingWindow {
                class: e.class,
                target: e.target.to_vec(),
                start: e.start,
                end: e.end,
            })
            .collect(),
        )
    }

    #[test]
    fn an_answer_round_trips_with_its_moment() {
        let entries = windows();
        let bytes = encode_standing(77, &entries).unwrap();
        assert_eq!(owned(&bytes), (77, entries));
    }

    #[test]
    fn an_empty_answer_round_trips() {
        let bytes = encode_standing(0, &[]).unwrap();
        assert_eq!(owned(&bytes), (0, vec![]));
        assert_eq!(bytes.len(), 12);
    }

    #[test]
    fn a_window_list_round_trips() {
        let entries = windows();
        let bytes = encode_windows(&entries).unwrap();
        assert_eq!(decode_windows(&bytes).unwrap(), entries);
    }

    #[test]
    fn the_frame_layout_is_the_documented_one() {
        let bytes = encode_standing(
            1,
            &[StandingWindow {
                class: 6,
                target: vec![0xAA, 0xBB],
                start: 2,
                end: 3,
            }],
        )
        .unwrap();
        let mut expected = 1_u64.to_le_bytes().to_vec();
        expected.extend(1_u32.to_le_bytes());
        expected.extend(6_u16.to_le_bytes());
        expected.extend(2_u16.to_le_bytes());
        expected.extend([0xAA, 0xBB]);
        expected.extend(2_u64.to_le_bytes());
        expected.extend(3_u64.to_le_bytes());
        assert_eq!(bytes, expected);
    }

    #[test]
    fn truncation_trailing_bytes_and_a_lying_count_are_refused() {
        let good = encode_standing(7, &windows()).unwrap();
        for len in 0..good.len() {
            assert!(parse_standing(&good[..len]).is_err(), "length {len}");
        }
        let mut lying = good.clone();
        lying[8..12].copy_from_slice(&9_u32.to_le_bytes());
        assert!(parse_standing(&lying).is_err());
        let mut extra = good.clone();
        extra.push(0);
        assert!(parse_standing(&extra).is_err());

        let list = encode_windows(&windows()).unwrap();
        for len in 0..list.len() {
            assert!(decode_windows(&list[..len]).is_err(), "length {len}");
        }
    }

    #[test]
    fn a_half_open_window_excludes_its_end() {
        let w = StandingWindow {
            class: 6,
            target: Vec::new(),
            start: 10,
            end: 20,
        };
        assert!(!w.contains(9));
        assert!(w.contains(10));
        assert!(w.contains(19));
        assert!(!w.contains(20));

        let empty = StandingWindow {
            start: 5,
            end: 5,
            ..w
        };
        assert!(!empty.contains(5));
    }
}
