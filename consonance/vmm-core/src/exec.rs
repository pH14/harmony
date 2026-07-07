// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `exec` improvisation's **sentinel state machine** — the pure, portable
//! logic that turns "run a command at the serial shell" into an injected byte
//! stream plus a completion detector. Task 81.
//!
//! `exec` is an **improvisation** (`docs/RESOLUTION.md` §Improvisations): a
//! one-off command run inside a *forked* guest, **never recorded into any
//! `Environment`** and carrying **no determinism guarantee**. The transport is
//! deliberately crude — raw bytes on the guest's 8250 serial input, as if typed
//! at a root shell — so this module owns none of task 61's deterministic
//! guest-plane machinery. What it owns is the small, testable protocol on top of
//! the shell: *what bytes to type*, and *how to know the command finished and with
//! what status*. The airtight part of the task is the **taint guard**
//! ([`crate::control`]), not this channel; this stays simple on purpose.
//!
//! ## The sentinel scheme
//!
//! A serial shell echoes what is typed and then runs it, interleaving the echo,
//! the command's own output, and the next prompt on one byte stream. To detect
//! completion without a guest agent, [`ExecSession`] injects, after the command,
//! an `echo` of a **unique marker wrapping the shell's `$?`**:
//!
//! ```text
//! <cmd>\n
//! echo <M>:$?:<M>\n
//! ```
//!
//! The shell first **echoes the typed line** — so the bytes `<M>:$?:<M>` appear on
//! the wire with `$?` **literal** (the two ASCII bytes `$` `?`, unexpanded). Then
//! the command runs, and finally the *executed* `echo` emits `<M>:<digits>:<M>`
//! with the real exit status. The detector therefore scans for
//! `<M>` `:` `<one-or-more ASCII digits>` `:` `<M>` — a pattern the literal echo
//! (`<M>:$?:<M>`) **cannot** match, because `$?` are not digits. This is what lets
//! a single marker disambiguate the echo from the result without splitting the
//! token or coordinating with the guest.
//!
//! The marker is `\x01HXEC-<nonce>-\x01` — bracketed by SOH (`0x01`) control bytes
//! that a normal command's textual output is very unlikely to contain, and salted
//! with a per-call `nonce` so two different `exec`s cannot alias. The `nonce` is a
//! caller-supplied counter, **not** wall-clock or `rand` (conventions rule 4);
//! because `exec` is off the record, its exact value never needs to be
//! reproducible — only unique-enough within a session.
//!
//! ## Failure modes (documented, by ruling out of scope to *fix*)
//!
//! - **Deadline before the sentinel.** If V-time reaches the run deadline first,
//!   [`ExecSession::finish_timeout`] closes the session `ok = false`; `output` is
//!   whatever was captured. A long-running or hung command, or a guest with no
//!   cooperating shell, ends here.
//! - **Marker collision.** If the command's *own* output contains the exact
//!   `<M>:<digits>:<M>` pattern, the detector stops early on it. The SOH-bracketed,
//!   nonce-salted marker makes this astronomically unlikely for textual output but
//!   is not impossible for arbitrary binary output — acceptable for a crude,
//!   off-record channel.
//! - **Output cap.** Captured output is bounded at [`MAX_CAPTURE`]; past that,
//!   bytes are dropped (and the session still completes on the sentinel if it
//!   arrives). This keeps a runaway command from growing an unbounded buffer —
//!   library code must never OOM on untrusted output (conventions rule 4).
//! - **Non-echoing / cooked-mode shells.** The scheme assumes the shell echoes the
//!   executed `echo`'s output onto the same serial line. A shell configured
//!   otherwise would time out. The box guest image (`guest/linux/`) provides a
//!   root shell on the serial console for exactly this reason.

/// The marker's fixed prefix, between two SOH (`0x01`) bytes. `HXEC` == "harmony
/// exec"; SOH brackets keep it out of ordinary textual output.
const MARKER_TAG: &[u8] = b"HXEC-";

/// The upper bound on captured serial output for one `exec` (1 MiB). Past this,
/// further output bytes are dropped — the sentinel is still detected if it
/// arrives — so an unbounded or hung command cannot grow the buffer without limit
/// (conventions rule 4: no OOM on untrusted input).
pub const MAX_CAPTURE: usize = 1 << 20;

/// The terminal state of an [`ExecSession`]: either the completion sentinel was
/// seen (with the parsed shell exit status) or the run deadline was reached first.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Done {
    /// The sentinel matched; the shell reported this exit status.
    Sentinel {
        /// The parsed `$?` value (the shell exit status).
        status: u64,
        /// Byte offset in the capture where the sentinel line began — output is
        /// reported up to here (the sentinel itself is stripped).
        cut: usize,
    },
    /// The deadline was reached before any sentinel; the command did not complete.
    Timeout,
}

/// The result of a completed [`ExecSession`]: the captured serial output (up to the
/// sentinel, or all of it on a timeout), whether the command completed cleanly, and
/// the shell exit status when known.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ExecOutcome {
    /// The serial output captured while the command ran (crude — may include the
    /// shell's echo of the injected line and the trailing prompt).
    pub output: Vec<u8>,
    /// Whether the command reached its completion sentinel before the deadline.
    pub ok: bool,
    /// The shell exit status (`$?`) parsed from the sentinel, or `None` on a
    /// timeout (no sentinel was seen).
    pub status: Option<u64>,
}

/// The pure sentinel state machine driving one `exec` improvisation. Build it with
/// [`new`](ExecSession::new), inject [`input`](ExecSession::input) on the guest
/// serial RX, then feed captured serial output with [`feed`](ExecSession::feed)
/// after each VM step until [`is_done`](ExecSession::is_done); close a run that hits
/// its deadline with [`finish_timeout`](ExecSession::finish_timeout). Portable and
/// side-effect-free — the real serial wiring lives in [`crate::vmm`], and this is
/// unit-tested against a scripted mock serial.
pub struct ExecSession {
    /// The full marker: `\x01HXEC-<nonce>-\x01`.
    marker: Vec<u8>,
    /// The bytes to type on the serial input (the command + the sentinel `echo`).
    input: Vec<u8>,
    /// Accumulated serial output, scanned for the sentinel.
    capture: Vec<u8>,
    /// Set once the sentinel matches or the deadline is reached.
    done: Option<Done>,
    /// `true` once [`MAX_CAPTURE`] was hit and bytes were dropped.
    truncated: bool,
}

impl ExecSession {
    /// Build a session for `cmd`, salting the marker with `nonce` (a session
    /// counter — unique-enough, never wall-clock/`rand`). The injected line is
    /// `"<cmd>\necho <M>:$?:<M>\n"`; see the module docs for the scheme.
    ///
    /// A `\n` inside `cmd` is passed through verbatim (the shell runs each line);
    /// the sentinel `echo` still lands after the whole command, so multi-line
    /// commands work. The crude channel does no quoting or escaping — the caller
    /// owns what it injects.
    pub fn new(cmd: &str, nonce: u64) -> ExecSession {
        let mut marker = Vec::with_capacity(MARKER_TAG.len() + 20);
        marker.push(0x01);
        marker.extend_from_slice(MARKER_TAG);
        marker.extend_from_slice(nonce.to_string().as_bytes());
        marker.push(b'-');
        marker.push(0x01);

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
        }
    }

    /// The bytes to inject on the guest's serial input (RBR), as if typed at the
    /// shell. Injected once, up front.
    pub fn input(&self) -> &[u8] {
        &self.input
    }

    /// Feed newly-captured serial output. Appends (bounded by [`MAX_CAPTURE`]) and
    /// rescans for the completion sentinel; a match closes the session `ok`. A
    /// no-op once [`is_done`](Self::is_done) (the first terminal state wins).
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
        if let Some((status, cut)) = self.scan() {
            self.done = Some(Done::Sentinel { status, cut });
        }
    }

    /// Close the session because the run reached its V-time deadline before any
    /// sentinel. Idempotent-safe: a no-op if the sentinel already matched (the
    /// clean completion wins over a same-step deadline).
    pub fn finish_timeout(&mut self) {
        if self.done.is_none() {
            self.done = Some(Done::Timeout);
        }
    }

    /// Whether the session has reached a terminal state (sentinel or timeout).
    pub fn is_done(&self) -> bool {
        self.done.is_some()
    }

    /// Whether captured output hit [`MAX_CAPTURE`] and bytes were dropped.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Consume the session into its [`ExecOutcome`]. If no terminal state was
    /// reached (neither [`feed`](Self::feed) matched nor
    /// [`finish_timeout`](Self::finish_timeout) was called), it is treated as a
    /// timeout — the caller always gets an honest, non-panicking result.
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

    /// Scan the capture for the completion sentinel `<M>:<digits>:<M>` and return
    /// `(status, cut)` — the parsed exit status and the byte offset where the
    /// sentinel line begins (so output is reported up to there). Returns `None`
    /// until the *executed* `echo` output appears; the shell's literal echo of the
    /// typed line (`<M>:$?:<M>`) never matches, because `$?` are not digits.
    fn scan(&self) -> Option<(u64, usize)> {
        let m = &self.marker;
        let buf = &self.capture;
        // Every candidate start is an occurrence of the marker. Walk them in order
        // and return the first that is followed by `:<digits>:<M>`.
        let mut from = 0;
        while let Some(rel) = find(&buf[from..], m) {
            let start = from + rel;
            let mut i = start + m.len();
            // Expect ':'
            if buf.get(i) != Some(&b':') {
                from = start + 1;
                continue;
            }
            i += 1;
            // Expect one or more ASCII digits, parsed as the status.
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
                // No digits (this is the literal `$?` echo, or a partial) — skip.
                from = start + 1;
                continue;
            }
            // Expect ':'
            if buf.get(i) != Some(&b':') {
                from = start + 1;
                continue;
            }
            i += 1;
            // Expect the closing marker.
            if buf[i..].starts_with(m) {
                return Some((status, start));
            }
            from = start + 1;
        }
        None
    }
}

/// First index of `needle` in `haystack` (naive; needles here are short markers).
/// `None` if absent or `needle` is empty.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| haystack[i..i + needle.len()].starts_with(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The injected line is `<cmd>\necho <M>:$?:<M>\n`, and the marker is
    /// SOH-bracketed + nonce-salted.
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
        // Marker present twice (open + close), literal `$?` between them.
        assert_eq!(input.iter().filter(|&&b| b == 0x01).count(), 4, "2 markers");
        assert!(text.contains("HXEC-7-"));
        assert!(text.contains(":$?:"), "literal $? in the injected echo");
    }

    /// A cooperating shell echoes the typed line (with literal `$?`) and then the
    /// executed echo with real digits: the detector ignores the first and fires on
    /// the second, reporting the status and the output before it.
    #[test]
    fn sentinel_with_digits_completes_and_the_literal_echo_does_not() {
        let mut s = ExecSession::new("true", 42);
        let marker = String::from_utf8(s.marker.clone()).unwrap();
        // The shell echoes the typed command line verbatim (literal `$?`)...
        let echo = format!("true\necho {marker}:$?:{marker}\n");
        s.feed(echo.as_bytes());
        assert!(
            !s.is_done(),
            "the literal $? echo must NOT complete the session"
        );
        // ...then the command's own output, then the executed echo with a real 0.
        let result = format!("some output\n{marker}:0:{marker}\n");
        s.feed(result.as_bytes());
        assert!(s.is_done());
        let out = s.into_outcome();
        assert!(out.ok);
        assert_eq!(out.status, Some(0));
        // Output is everything before the sentinel line — includes the echo and the
        // command output (crude), but NOT the sentinel itself.
        let text = String::from_utf8_lossy(&out.output);
        assert!(text.contains("some output"));
        assert!(
            !text.contains(":0:"),
            "the sentinel is stripped from output"
        );
    }

    /// A non-zero exit status is parsed.
    #[test]
    fn nonzero_exit_status_is_parsed() {
        let mut s = ExecSession::new("false", 1);
        let marker = String::from_utf8(s.marker.clone()).unwrap();
        s.feed(format!("{marker}:137:{marker}\n").as_bytes());
        let out = s.into_outcome();
        assert!(out.ok);
        assert_eq!(out.status, Some(137));
    }

    /// The sentinel can arrive split across two `feed` chunks (V-time steps): the
    /// scan runs on the whole accumulated buffer, so a marker straddling a chunk
    /// boundary is still found.
    #[test]
    fn sentinel_split_across_feeds_is_detected() {
        let mut s = ExecSession::new("echo hi", 99);
        let marker = String::from_utf8(s.marker.clone()).unwrap();
        let full = format!("hi\n{marker}:0:{marker}\n");
        let (a, b) = full.split_at(full.len() / 2);
        s.feed(a.as_bytes());
        // May or may not be done depending on the split; feed the rest.
        s.feed(b.as_bytes());
        assert!(s.is_done());
        assert_eq!(s.into_outcome().status, Some(0));
    }

    /// Reaching the deadline with no sentinel closes the session `ok = false`,
    /// surfacing whatever was captured.
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

    /// A clean sentinel on the same step as a deadline wins over the timeout (feed
    /// is processed before finish_timeout in the run loop, and the first terminal
    /// state is sticky).
    #[test]
    fn sentinel_wins_over_a_same_step_timeout() {
        let mut s = ExecSession::new("true", 3);
        let marker = String::from_utf8(s.marker.clone()).unwrap();
        s.feed(format!("{marker}:0:{marker}\n").as_bytes());
        s.finish_timeout(); // no-op: already completed cleanly
        assert!(s.into_outcome().ok);
    }

    /// Output past `MAX_CAPTURE` is dropped rather than growing unbounded, and the
    /// session still completes if the sentinel arrives within the cap.
    #[test]
    fn capture_is_bounded() {
        let mut s = ExecSession::new("yes", 8);
        // Feed more than the cap in one shot.
        let big = vec![b'x'; MAX_CAPTURE + 4096];
        s.feed(&big);
        assert!(s.truncated());
        assert!(!s.is_done());
        // The buffer never exceeds the cap.
        assert!(s.capture.len() <= MAX_CAPTURE);
    }

    /// A marker with no digits between the colons (e.g. a corrupted/partial line)
    /// never falsely completes.
    #[test]
    fn marker_without_digits_never_completes() {
        let mut s = ExecSession::new("x", 11);
        let marker = String::from_utf8(s.marker.clone()).unwrap();
        s.feed(format!("{marker}::{marker}").as_bytes()); // empty status field
        assert!(!s.is_done());
        s.feed(format!("{marker}:$?:{marker}").as_bytes()); // literal $?
        assert!(!s.is_done());
    }

    /// `into_outcome` on a session that never reached a terminal state is an honest
    /// timeout, never a panic.
    #[test]
    fn unterminated_session_yields_a_timeout_outcome() {
        let s = ExecSession::new("x", 0);
        let out = s.into_outcome();
        assert!(!out.ok);
        assert_eq!(out.status, None);
    }
}
