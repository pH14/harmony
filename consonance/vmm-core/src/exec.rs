// SPDX-License-Identifier: AGPL-3.0-or-later

const MARKER_TAG: &[u8] = b"HXEC-";

pub const MAX_CAPTURE: usize = 1 << 20;

#[derive(Clone, PartialEq, Eq, Debug)]
enum Done {
    Sentinel { status: u64, cut: usize },
    Timeout,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ExecOutcome {
    pub output: Vec<u8>,
    pub ok: bool,
    pub status: Option<u64>,
}

pub struct ExecSession {
    marker: Vec<u8>,
    input: Vec<u8>,
    capture: Vec<u8>,
    done: Option<Done>,
    truncated: bool,
    scan_from: usize,
}

impl ExecSession {
    pub fn new(cmd: &str, nonce: u64) -> ExecSession {
        let mut marker = Vec::with_capacity(MARKER_TAG.len() + 20);
        marker.extend_from_slice(MARKER_TAG);
        marker.extend_from_slice(nonce.to_string().as_bytes());
        marker.push(b'-');

        let mut input = Vec::with_capacity(cmd.len() + 2 * marker.len() + 16);
        input.extend_from_slice(cmd.as_bytes());
        input.push(b'\n');
        input.extend_from_slice(b"echo ");
        input.extend_from_slice(&marker);
        input.push(b':');
        input.extend_from_slice(b"$?");
        input.push(b':');
        input.extend_from_slice(&marker);
        input.push(b'\n');

        ExecSession {
            marker,
            input,
            capture: Vec::new(),
            done: None,
            truncated: false,
            scan_from: 0,
        }
    }

    pub fn input(&self) -> &[u8] {
        &self.input
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        if self.done.is_some() {
            return;
        }
        let room = MAX_CAPTURE.saturating_sub(self.capture.len());
        if bytes.len() > room {
            self.capture.extend_from_slice(&bytes[..room]);
            self.truncated = true;
        } else {
            self.capture.extend_from_slice(bytes);
        }
        if let Some((status, cut)) = self.scan(self.scan_from) {
            self.done = Some(Done::Sentinel { status, cut });
            return;
        }
        self.scan_from = self.capture.len().saturating_sub(self.sentinel_max_len());
    }

    fn sentinel_max_len(&self) -> usize {
        2 * self.marker.len() + 2 + 20
    }

    pub fn finish_timeout(&mut self) {
        if self.done.is_none() {
            self.done = Some(Done::Timeout);
        }
    }

    pub fn is_done(&self) -> bool {
        self.done.is_some()
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn into_outcome(self) -> ExecOutcome {
        match self.done {
            Some(Done::Sentinel { status, cut }) => ExecOutcome {
                output: self.capture[..cut].to_vec(),
                ok: true,
                status: Some(status),
            },
            Some(Done::Timeout) | None => ExecOutcome {
                output: self.capture,
                ok: false,
                status: None,
            },
        }
    }

    fn scan(&self, start: usize) -> Option<(u64, usize)> {
        let m = &self.marker;
        let buf = &self.capture;
        let mut from = start.min(buf.len());
        while let Some(rel) = find(&buf[from..], m) {
            let start = from + rel;
            let mut i = start + m.len();
            if buf.get(i) != Some(&b':') {
                from = start + 1;
                continue;
            }
            i += 1;
            let digit_start = i;
            let mut status: u64 = 0;
            while let Some(&c) = buf.get(i) {
                if c.is_ascii_digit() {
                    status = status
                        .saturating_mul(10)
                        .saturating_add(u64::from(c - b'0'));
                    i += 1;
                } else {
                    break;
                }
            }
            if i == digit_start {
                from = start + 1;
                continue;
            }
            if buf.get(i) != Some(&b':') {
                from = start + 1;
                continue;
            }
            i += 1;
            if buf[i..].starts_with(m) {
                return Some((status, start));
            }
            from = start + 1;
        }
        None
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| haystack[i..i + needle.len()].starts_with(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injection_wraps_the_command_with_a_sentinel_echo() {
        let s = ExecSession::new("ls /", 7);
        let input = s.input();
        let text = String::from_utf8_lossy(input);
        assert!(
            text.starts_with("ls /\necho "),
            "cmd then the echo: {text:?}"
        );
        assert!(text.ends_with('\n'));
        assert!(text.contains("HXEC-7-"));
        assert!(text.contains(":$?:"), "literal $? in the injected echo");
        assert!(
            input
                .iter()
                .all(|&b| b == b'\n' || (0x20..0x7f).contains(&b)),
            "injected bytes must be printable (+\\n): {input:?}"
        );
    }

    #[test]
    fn sentinel_with_digits_completes_and_the_literal_echo_does_not() {
        let mut s = ExecSession::new("true", 42);
        let marker = String::from_utf8(s.marker.clone()).unwrap();
        let echo = format!("true\necho {marker}:$?:{marker}\n");
        s.feed(echo.as_bytes());
        assert!(
            !s.is_done(),
            "the literal $? echo must NOT complete the session"
        );
        let result = format!("some output\n{marker}:0:{marker}\n");
        s.feed(result.as_bytes());
        assert!(s.is_done());
        let out = s.into_outcome();
        assert!(out.ok);
        assert_eq!(out.status, Some(0));
        let text = String::from_utf8_lossy(&out.output);
        assert!(text.contains("some output"));
        assert!(
            !text.contains(":0:"),
            "the sentinel is stripped from output"
        );
    }

    #[test]
    fn nonzero_exit_status_is_parsed() {
        let mut s = ExecSession::new("false", 1);
        let marker = String::from_utf8(s.marker.clone()).unwrap();
        s.feed(format!("{marker}:137:{marker}\n").as_bytes());
        let out = s.into_outcome();
        assert!(out.ok);
        assert_eq!(out.status, Some(137));
    }

    #[test]
    fn resume_scan_finds_the_sentinel_after_lots_of_chatty_output() {
        let mut s = ExecSession::new("busy", 5);
        let marker = String::from_utf8(s.marker.clone()).unwrap();
        s.feed(format!("busy\necho {marker}:$?:{marker}\n").as_bytes());
        assert!(!s.is_done());
        for _ in 0..4096 {
            s.feed(b"x");
        }
        assert!(!s.is_done());
        for b in format!("{marker}:42:{marker}\n").into_bytes() {
            s.feed(&[b]);
        }
        assert!(s.is_done());
        let out = s.into_outcome();
        assert!(out.ok);
        assert_eq!(out.status, Some(42));
        assert!(out.output.ends_with(b"x"));
        assert!(!String::from_utf8_lossy(&out.output).contains(":42:"));
    }

    #[test]
    fn sentinel_split_across_feeds_is_detected() {
        let mut s = ExecSession::new("echo hi", 99);
        let marker = String::from_utf8(s.marker.clone()).unwrap();
        let full = format!("hi\n{marker}:0:{marker}\n");
        let (a, b) = full.split_at(full.len() / 2);
        s.feed(a.as_bytes());
        s.feed(b.as_bytes());
        assert!(s.is_done());
        assert_eq!(s.into_outcome().status, Some(0));
    }

    #[test]
    fn timeout_without_sentinel_is_not_ok() {
        let mut s = ExecSession::new("sleep 999", 5);
        s.feed(b"partial output, no sentinel yet");
        assert!(!s.is_done());
        s.finish_timeout();
        assert!(s.is_done());
        let out = s.into_outcome();
        assert!(!out.ok);
        assert_eq!(out.status, None);
        assert_eq!(out.output, b"partial output, no sentinel yet");
    }

    #[test]
    fn sentinel_wins_over_a_same_step_timeout() {
        let mut s = ExecSession::new("true", 3);
        let marker = String::from_utf8(s.marker.clone()).unwrap();
        s.feed(format!("{marker}:0:{marker}\n").as_bytes());
        s.finish_timeout();
        assert!(s.into_outcome().ok);
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "feeds >MAX_CAPTURE (1 MiB): the sentinel rescan over the capped buffer is a byte-wise interpreted scan (~9 min); pure safe code — the cap arithmetic is covered natively and the scan path stays Miri-run via the small-buffer exec tests"
    )]
    fn capture_is_bounded() {
        let mut s = ExecSession::new("yes", 8);
        let big = vec![b'x'; MAX_CAPTURE + 4096];
        s.feed(&big);
        assert!(s.truncated());
        assert!(!s.is_done());
        assert!(s.capture.len() <= MAX_CAPTURE);
    }

    #[test]
    fn marker_without_digits_never_completes() {
        let mut s = ExecSession::new("x", 11);
        let marker = String::from_utf8(s.marker.clone()).unwrap();
        s.feed(format!("{marker}::{marker}").as_bytes());
        assert!(!s.is_done());
        s.feed(format!("{marker}:$?:{marker}").as_bytes());
        assert!(!s.is_done());
    }

    #[test]
    fn unterminated_session_yields_a_timeout_outcome() {
        let s = ExecSession::new("x", 0);
        let out = s.into_outcome();
        assert!(!out.ok);
        assert_eq!(out.status, None);
    }
}
