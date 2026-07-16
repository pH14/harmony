// SPDX-License-Identifier: AGPL-3.0-or-later
//! The PL011 console decoder — the harness's view of the guest's one MMIO window.
//!
//! Under KVM every guest store to the PL011 data register is an MMIO exit, and the
//! harness feeds the byte here. Three things come out: the two window marks (at
//! which the harness samples `BR_RETIRED`), the payload's protocol lines, and the
//! terminal exit sentinel.
//!
//! # The sentinel is the stop condition, and it carries a status
//!
//! `docs/ARM-ALTRA.md` §Evidence integrity #1: *a done-marker is never a success
//! condition.* So [`Event::Exit`] carries the payload's own status code, and the
//! harness stops the vCPU there and records that code. A payload that ran to
//! completion but failed its in-guest self-checks exits nonzero and is a failed
//! sample — reaching the end of the run is not, by itself, evidence of anything.

use oracle_model::{MARK_BEGIN, MARK_END};

/// Something the guest said.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Event {
    /// The counting window opened. Sample the work counter here.
    MarkBegin,
    /// The counting window closed. Sample the work counter here.
    MarkEnd,
    /// A complete protocol line (without its newline).
    Line(String),
    /// The terminal sentinel, carrying the payload's exit status. The harness must
    /// stop the vCPU here and never re-enter.
    Exit(u8),
}

/// Accumulates bytes written to the PL011 data register.
#[derive(Default, Debug)]
pub struct Console {
    line: Vec<u8>,
}

impl Console {
    /// A fresh decoder.
    #[must_use]
    pub fn new() -> Console {
        Console::default()
    }

    /// Feed one byte; returns an event if that byte completed one.
    pub fn push(&mut self, byte: u8) -> Option<Event> {
        match byte {
            MARK_BEGIN => Some(Event::MarkBegin),
            MARK_END => Some(Event::MarkEnd),
            b'\n' => {
                let line = String::from_utf8_lossy(&self.line).trim_end().to_string();
                self.line.clear();
                // Match the prefix without its trailing space: the line was
                // `trim_end`ed above, so `PAYLOAD EXIT ` with an empty argument has
                // already lost that space, and matching a spaced prefix would send
                // the empty-status case down the ordinary-line path instead of the
                // failure path.
                if let Some(code) = line.strip_prefix("PAYLOAD EXIT") {
                    // A malformed status is not "probably fine": treat an
                    // unparseable exit code as a failure status rather than
                    // silently rounding it to success.
                    return Some(Event::Exit(code.trim().parse::<u8>().unwrap_or(u8::MAX)));
                }
                Some(Event::Line(line))
            }
            b'\r' => None,
            b => {
                // A payload that never emits a newline must not grow the buffer
                // without bound: this runs against a guest that may be misbehaving.
                if self.line.len() < 4096 {
                    self.line.push(b);
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(c: &mut Console, s: &[u8]) -> Vec<Event> {
        s.iter().filter_map(|&b| c.push(b)).collect()
    }

    #[test]
    fn marks_are_events_not_line_content() {
        let mut c = Console::new();
        let events = feed(&mut c, &[MARK_BEGIN, MARK_END]);
        assert_eq!(events, vec![Event::MarkBegin, Event::MarkEnd]);
    }

    #[test]
    fn a_window_is_delimited_inside_a_line_stream() {
        let mut c = Console::new();
        let mut bytes = b"WINDOW trips=1000\n".to_vec();
        bytes.push(MARK_BEGIN);
        bytes.push(MARK_END);
        bytes.extend_from_slice(b"OK done\n");
        let events = feed(&mut c, &bytes);
        assert_eq!(
            events,
            vec![
                Event::Line("WINDOW trips=1000".into()),
                Event::MarkBegin,
                Event::MarkEnd,
                Event::Line("OK done".into()),
            ]
        );
    }

    #[test]
    fn the_exit_sentinel_carries_the_status() {
        let mut c = Console::new();
        assert_eq!(feed(&mut c, b"PAYLOAD EXIT 0\n"), vec![Event::Exit(0)]);
        let mut c = Console::new();
        assert_eq!(feed(&mut c, b"PAYLOAD EXIT 1\n"), vec![Event::Exit(1)]);
    }

    #[test]
    fn a_malformed_exit_status_fails_rather_than_passing() {
        // The one direction this must never round: an unreadable status is not 0.
        let mut c = Console::new();
        assert_eq!(feed(&mut c, b"PAYLOAD EXIT \n"), vec![Event::Exit(u8::MAX)]);
        let mut c = Console::new();
        assert_eq!(
            feed(&mut c, b"PAYLOAD EXIT wat\n"),
            vec![Event::Exit(u8::MAX)]
        );
        let mut c = Console::new();
        assert_eq!(
            feed(&mut c, b"PAYLOAD EXIT 999\n"),
            vec![Event::Exit(u8::MAX)]
        );
    }

    #[test]
    fn a_runaway_guest_cannot_grow_the_buffer_without_bound() {
        let mut c = Console::new();
        for _ in 0..100_000 {
            assert!(c.push(b'x').is_none());
        }
        assert!(c.line.len() <= 4096);
    }
}
