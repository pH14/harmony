// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `exec` improvisation's **sentinel state machine** — the pure, portable
//! logic that turns "run a command at the serial shell" into an injected byte
//! stream plus a completion detector. Task 81.
//!
//! `exec` is an **improvisation** (`docs/PROTOCOL.md`): a
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
//! `<M>` `:` `<one-or-more ASCII digits, at most 20>` `:` `<M>` — a pattern the
//! literal echo (`<M>:$?:<M>`) **cannot** match, because `$?` are not digits. **This
//! digits-vs-literal-`$?` rule is the load-bearing disambiguation** — not the
//! marker's exact bytes.
//!
//! The marker is `HXEC-<nonce>-` — plain printable ASCII (`HXEC` == "harmony
//! exec"), salted with a per-call `nonce` so two different `exec`s cannot alias.
//! **It must stay printable.** The exec-capable image (`consonance/harmony-linux/linux/exec-init.sh`)
//! hands the console to an *interactive* `busybox ash` with line editing
//! (`CONFIG_FEATURE_EDITING=y`, on by defconfig), and a line editor treats control
//! bytes as editing keystrokes rather than input — e.g. `^A` (SOH, `0x01`) is
//! *cursor-to-start*, which rearranges the injected line so the sentinel never
//! reaches the wire (empirically reproduced against busybox ash 1.37: every `exec`
//! timed out). A printable marker survives the line editor untouched; the nonce
//! salt already carries the collision-avoidance the (removed) SOH brackets were
//! meant to add. The `nonce` is a caller-supplied counter, **not** wall-clock or
//! `rand` (conventions rule 4). The control snapshot retains the nonce and parser
//! so continuation uses the same marker without injecting the command again.
//!
//! ## Failure modes (documented, by ruling out of scope to *fix*)
//!
//! - **Deadline before the sentinel.** The durable control owner retains the
//!   pending parser and output so a later run can continue. A terminal guest
//!   without a sentinel is aborted with unknown status. The standalone parser's
//!   [`ExecSession::finish_timeout`] remains available for an explicit abort.
//! - **Marker collision.** If the command's *own* output contains the exact
//!   `<M>:<digits>:<M>` pattern, the detector stops early on it. The distinctive
//!   `HXEC-` tag plus the per-call `nonce` salt makes this astronomically unlikely
//!   for textual output but is not impossible for arbitrary binary output —
//!   acceptable for a crude, off-record channel.
//! - **Output cap.** Captured output is bounded at [`MAX_CAPTURE`]; past that,
//!   bytes are dropped (and the session still completes on the sentinel if it
//!   arrives). This keeps a runaway command from growing an unbounded buffer —
//!   library code must never OOM on untrusted output (conventions rule 4).
//! - **Non-echoing / cooked-mode shells.** The scheme assumes the shell echoes the
//!   executed `echo`'s output onto the same serial line. A shell configured
//!   otherwise would time out. The box guest image (`consonance/harmony-linux/linux/`) provides a
//!   root shell on the serial console for exactly this reason.

/// The marker's fixed, **plain-printable** prefix (`HXEC` == "harmony exec"). Kept
/// printable so an interactive line-editing shell (busybox ash,
/// `CONFIG_FEATURE_EDITING=y`) does not eat it as editing keystrokes — see the
/// module docs. The per-call `nonce` (appended, then a trailing `-`) does the
/// collision-avoidance the marker's bytes must not.
const MARKER_TAG: &[u8] = b"HXEC-";

/// The upper bound on captured serial output for one `exec` (1 MiB). Past this,
/// further output bytes are dropped — the sentinel is still detected if it
/// arrives — so an unbounded or hung command cannot grow the buffer without limit
/// (conventions rule 4: no OOM on untrusted input).
pub const MAX_CAPTURE: usize = 1 << 20;

/// The largest decimal exit status accepted by the sentinel parser. This is
/// also part of the scanner's bounded-overlap proof: a malformed longer field
/// is rejected instead of being saturated into a valid status.
const MAX_STATUS_DIGITS: usize = 20;

/// Feed input to the scanner in bounded pieces, so a single serial read cannot
/// temporarily grow the overlap buffer without limit.
const SCAN_CHUNK: usize = 4096;

pub(crate) mod retained;

/// The terminal state of an [`ExecSession`]: either the completion sentinel was
/// seen (with the parsed shell exit status) or the run deadline was reached first.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Done {
    /// The sentinel matched; the shell reported this exit status.
    Sentinel {
        /// The parsed `$?` value (the shell exit status).
        status: u64,
        /// Byte offset in the retained capture where the sentinel line began —
        /// output is reported up to here (the sentinel itself is stripped).
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
    /// The full, plain-printable marker: `HXEC-<nonce>-`.
    marker: Vec<u8>,
    /// The bytes to type on the serial input (the command + the sentinel `echo`).
    input: Vec<u8>,
    /// Retained prefix of serial output, bounded by [`MAX_CAPTURE`].
    capture: Vec<u8>,
    /// Set once the sentinel matches or the deadline is reached.
    done: Option<Done>,
    /// Whether the retained output was truncated before the terminal sentinel.
    /// While pending this becomes true once bytes beyond [`MAX_CAPTURE`] arrive;
    /// on completion it is normalized to whether the sentinel began after the
    /// capture cap.
    truncated: bool,
    /// Small streaming overlap passed to the sentinel scanner. It contains the
    /// newest bytes that have not yet been proven unable to start a complete
    /// sentinel; it is independent of the retained output prefix.
    scan_buffer: Vec<u8>,
    /// Absolute serial-stream offset corresponding to `scan_buffer[0]`.
    scan_base: usize,
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
        // Plain-printable marker `HXEC-<nonce>-` — NO control bytes (an interactive
        // line-editing shell would eat them; see the module docs).
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
            scan_buffer: Vec::new(),
            scan_base: 0,
        }
    }

    /// The bytes to inject on the guest's serial input (RBR), as if typed at the
    /// shell. Injected once, up front.
    pub fn input(&self) -> &[u8] {
        &self.input
    }

    /// Feed newly-captured serial output. The retained output is bounded by
    /// [`MAX_CAPTURE`], while every byte is passed through the separate bounded
    /// sentinel scanner; a match closes the session `ok`. A no-op once
    /// [`is_done`](Self::is_done) (the first terminal state wins).
    pub fn feed(&mut self, bytes: &[u8]) {
        if self.done.is_some() {
            return;
        }
        for chunk in bytes.chunks(SCAN_CHUNK) {
            let room = MAX_CAPTURE.saturating_sub(self.capture.len());
            if chunk.len() > room {
                self.capture.extend_from_slice(&chunk[..room]);
                self.truncated = true;
            } else {
                self.capture.extend_from_slice(chunk);
            }

            // The scanner sees the complete stream, including bytes beyond the
            // retained output cap. Its buffer is trimmed only after this scan so
            // a sentinel beginning at the cap boundary remains discoverable.
            self.scan_buffer.extend_from_slice(chunk);
            if let Some((status, relative_cut)) = self.scan() {
                let absolute_cut = self.scan_base.saturating_add(relative_cut);
                let cut = absolute_cut.min(self.capture.len());
                self.capture.truncate(cut);
                self.truncated = absolute_cut > MAX_CAPTURE;
                self.done = Some(Done::Sentinel { status, cut });
                // A completed session must not retain parser state that depends
                // on how its input was partitioned. The absolute base is clipped
                // below, so cap+1 is enough to preserve the truncation distinction.
                self.scan_buffer.clear();
                self.scan_base = 0;
                return;
            }
            self.trim_scan_buffer();
        }
    }

    /// The maximum byte length of a complete sentinel `<M>:<digits>:<M>`: two
    /// markers, the two `:` separators, and up to [`MAX_STATUS_DIGITS`] digits.
    /// A sentinel is never longer than this, so retaining this many bytes of
    /// overlap can never miss one split across feeds.
    fn sentinel_max_len(&self) -> usize {
        2 * self.marker.len() + 2 + MAX_STATUS_DIGITS
    }

    /// Drop bytes that cannot begin a future sentinel while retaining enough
    /// suffix for a marker/status/marker sequence split across feed chunks.
    fn trim_scan_buffer(&mut self) {
        let keep = self.sentinel_max_len().saturating_sub(1);
        if self.scan_buffer.len() > keep {
            let discard = self.scan_buffer.len() - keep;
            self.scan_buffer.copy_within(discard.., 0);
            self.scan_buffer.truncate(keep);
            self.scan_base = self
                .scan_base
                .saturating_add(discard)
                .min(MAX_CAPTURE.saturating_add(1));
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

    /// Scan the overlap buffer for the completion sentinel `<M>:<digits>:<M>`
    /// and return `(status, cut)` — the parsed exit status and the byte offset
    /// where the sentinel line begins. Returns `None` until the *executed*
    /// `echo` output appears; the shell's literal echo of the typed line
    /// (`<M>:$?:<M>`) never matches, because `$?` are not digits. The returned
    /// offset is relative to `scan_buffer`; [`feed`](Self::feed) converts it to
    /// the absolute stream offset before clipping it to the retained capture.
    fn scan(&self) -> Option<(u64, usize)> {
        let m = &self.marker;
        let buf = &self.scan_buffer;
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
            let mut digits = 0;
            let mut invalid = false;
            while let Some(&c) = buf.get(i) {
                if c.is_ascii_digit() {
                    if digits == MAX_STATUS_DIGITS {
                        // A 21st digit is malformed. In particular, do not
                        // saturate it into a completion with status u64::MAX.
                        invalid = true;
                        break;
                    }
                    let Some(next) = status
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(u64::from(c - b'0')))
                    else {
                        invalid = true;
                        break;
                    };
                    status = next;
                    digits += 1;
                    i += 1;
                } else {
                    break;
                }
            }
            if invalid || i == digit_start {
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

    /// The injected line is `<cmd>\necho <M>:$?:<M>\n`, and the marker is a
    /// **plain-printable** nonce-salted token (no control bytes — an interactive
    /// line-editing shell would eat them; see the module docs).
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
        // Every injected byte is printable ASCII or a newline — NO control bytes,
        // so an interactive line-editing shell relays the line intact.
        assert!(
            input
                .iter()
                .all(|&b| b == b'\n' || (0x20..0x7f).contains(&b)),
            "injected bytes must be printable (+\\n): {input:?}"
        );
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

    /// The bounded-overlap scanner does not miss a sentinel after many chatty
    /// feeds — including an early **literal-`$?` echo** (a historical
    /// non-matching marker occurrence) followed by a real sentinel byte-by-byte
    /// much later.
    #[test]
    fn resume_scan_finds_the_sentinel_after_lots_of_chatty_output() {
        let mut s = ExecSession::new("busy", 5);
        let marker = String::from_utf8(s.marker.clone()).unwrap();
        // The shell's echo of the typed line (literal `$?`) — must NOT complete.
        s.feed(format!("busy\necho {marker}:$?:{marker}\n").as_bytes());
        assert!(!s.is_done());
        // A long run of chatty output, one byte per feed (many V-time steps).
        for _ in 0..4096 {
            s.feed(b"x");
        }
        assert!(!s.is_done());
        // The real executed echo, dribbled in byte-by-byte across the boundary.
        for b in format!("{marker}:42:{marker}\n").into_bytes() {
            s.feed(&[b]);
        }
        assert!(s.is_done());
        let out = s.into_outcome();
        assert!(out.ok);
        assert_eq!(out.status, Some(42));
        // Output is everything before the sentinel (the echo + the 4096 x's), never
        // the sentinel itself.
        assert!(out.output.ends_with(b"x"));
        assert!(!String::from_utf8_lossy(&out.output).contains(":42:"));
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
    #[cfg_attr(
        miri,
        ignore = "feeds >MAX_CAPTURE (1 MiB); the bounded scanner is safe but this large allocation is covered natively, while its overlap path stays Miri-run via the small-buffer exec tests"
    )]
    fn capture_is_bounded() {
        let mut s = ExecSession::new("yes", 8);
        // Feed more than the cap in one shot.
        let big = vec![b'x'; MAX_CAPTURE + 4096];
        s.feed(&big);
        assert!(s.truncated());
        assert!(!s.is_done());
        // Neither the retained output nor the streaming overlap grows with the
        // input. The latter is allowed one bounded chunk while it is being fed.
        assert!(s.capture.len() <= MAX_CAPTURE);
        assert!(s.scan_buffer.len() <= s.sentinel_max_len().saturating_sub(1) + SCAN_CHUNK);
    }

    #[test]
    fn nonzero_status_is_found_after_the_capture_cap() {
        let mut s = ExecSession::new("false", 23);
        let marker = String::from_utf8(s.marker.clone()).unwrap();
        let sentinel = format!("{marker}:137:{marker}\n");
        let mut stream = vec![b'x'; MAX_CAPTURE + 128];
        stream.extend_from_slice(sentinel.as_bytes());

        s.feed(&stream);
        assert!(s.is_done());
        assert!(s.truncated());
        assert!(s.scan_buffer.is_empty());
        assert_eq!(s.scan_base, 0);
        let out = s.into_outcome();
        assert!(out.ok);
        assert_eq!(out.status, Some(137));
        assert_eq!(out.output.len(), MAX_CAPTURE);
    }

    #[test]
    fn sentinel_start_before_at_and_after_capture_cap_is_detected() {
        let starts = [
            MAX_CAPTURE - 1, // the sentinel crosses the retained-output boundary
            MAX_CAPTURE,     // the marker starts exactly after retained output
            MAX_CAPTURE + 1, // the marker starts after the retained-output boundary
        ];

        for (nonce, start) in starts.into_iter().enumerate() {
            let mut s = ExecSession::new("x", nonce as u64 + 30);
            let marker = String::from_utf8(s.marker.clone()).unwrap();
            let sentinel = format!("{marker}:9:{marker}\n");
            s.feed(&vec![b'x'; start]);
            for byte in sentinel.as_bytes() {
                s.feed(std::slice::from_ref(byte));
            }

            assert!(s.is_done(), "sentinel starting at {start} was missed");
            assert_eq!(s.truncated(), start > MAX_CAPTURE);
            assert!(s.scan_buffer.is_empty());
            assert_eq!(s.scan_base, 0);
            let out = s.into_outcome();
            assert!(out.ok);
            assert_eq!(out.status, Some(9));
            assert_eq!(out.output.len(), start.min(MAX_CAPTURE));
        }
    }

    fn run_with_chunks(stream: &[u8], chunk_size: usize) -> (ExecOutcome, bool) {
        let mut s = ExecSession::new("chunked", 31);
        for chunk in stream.chunks(chunk_size) {
            s.feed(chunk);
        }
        let truncated = s.truncated();
        (s.into_outcome(), truncated)
    }

    #[test]
    fn feed_chunkings_produce_the_same_output_and_status() {
        let marker = String::from_utf8(ExecSession::new("x", 31).marker).unwrap();
        let mut stream = vec![b'o'; MAX_CAPTURE - 2];
        stream.extend_from_slice(format!("{marker}:201:{marker}\n").as_bytes());
        stream.extend_from_slice(&[b't'; 128]);

        let (whole, whole_truncated) = run_with_chunks(&stream, stream.len());
        let (split, split_truncated) = run_with_chunks(&stream, 17);
        assert_eq!(whole, split);
        assert_eq!(whole_truncated, split_truncated);
        assert!(
            !whole_truncated,
            "output before the sentinel still fits the cap"
        );
    }

    #[test]
    fn overlong_status_and_incomplete_or_wrong_nonce_stay_pending() {
        let mut s = ExecSession::new("x", 44);
        let marker = String::from_utf8(s.marker.clone()).unwrap();
        s.feed(format!("{marker}:123456789012345678901:{marker}").as_bytes());
        assert!(!s.is_done(), "more than 20 digits must not complete");

        let wrong = "HXEC-45-:7:HXEC-45-";
        s.feed(wrong.as_bytes());
        assert!(!s.is_done(), "a different nonce must not complete");

        let incomplete = format!("{marker}:7:{}", &marker[..marker.len() - 1]);
        s.feed(incomplete.as_bytes());
        assert!(
            !s.is_done(),
            "an incomplete closing marker must remain pending"
        );
        assert!(s.scan_buffer.len() <= s.sentinel_max_len().saturating_sub(1));
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
