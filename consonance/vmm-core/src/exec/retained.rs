// SPDX-License-Identifier: AGPL-3.0-or-later
//! Durable, control-plane state for one serial `exec` command.
//!
//! [`ExecSession`](super::ExecSession) owns the sentinel parser, while this
//! wrapper owns the part that must survive a control-state snapshot: the
//! absolute serial cursor and the completion moment.  The command text and the
//! injected input are deliberately absent from the codec.  The VMM owns the
//! serial RX queue, and restoring that queue must never cause a decoded command
//! to be injected a second time.

use control_proto::{ExecCompletion, ExecStatus, Moment};

use super::{Done, ExecSession, MAX_CAPTURE};

const MAGIC: &[u8; 8] = b"HCEXEC01";
const COMPLETION_PENDING: u8 = 0;
const COMPLETION_EXITED: u8 = 1;
const COMPLETION_ABORTED: u8 = 2;

/// The parser state carried by the control-state snapshot.
pub(crate) struct RetainedExec {
    id: u64,
    cursor: u64,
    session: ExecSession,
    completion: ExecCompletion,
}

impl RetainedExec {
    /// Construct a command at the current end of the VM's serial output.
    ///
    /// `serial_len` is the absolute output position at which the command's
    /// parser starts.  The command's input is returned exactly once through
    /// [`input`](Self::input); it is intentionally not part of the retained
    /// codec.
    pub(crate) fn new(cmd: &str, id: u64, serial_len: usize) -> Self {
        Self {
            id,
            cursor: serial_len as u64,
            session: ExecSession::new(cmd, id),
            completion: ExecCompletion::Pending,
        }
    }

    /// Input for the initial serial injection.  A decoded retained command has
    /// an empty input buffer, so a restore cannot inject it again.
    pub(crate) fn input(&self) -> &[u8] {
        self.session.input()
    }

    /// Whether the command still needs serial output to reach a terminal state.
    pub(crate) fn is_pending(&self) -> bool {
        matches!(self.completion, ExecCompletion::Pending)
    }

    /// Observe the command without advancing the parser or the VM.
    pub(crate) fn view(&self, at: Moment) -> ExecStatus {
        let output = match self.session.done.as_ref() {
            Some(Done::Sentinel { cut, .. }) => {
                self.session.capture[..(*cut).min(self.session.capture.len())].to_vec()
            }
            Some(Done::Timeout) | None => self.session.capture.clone(),
        };
        ExecStatus {
            id: self.id,
            at,
            completion: self.completion,
            output,
            truncated: self.session.truncated,
        }
    }

    /// Consume only serial bytes after the retained absolute cursor.
    ///
    /// A deadline does not abort a command; callers pass `terminal = true` only
    /// when the VM itself reached a terminal state before the sentinel.  Once a
    /// command is terminal, later feeds are deliberately no-ops.
    pub(crate) fn feed(
        &mut self,
        serial: &[u8],
        at: Moment,
        terminal: bool,
    ) -> Result<(), &'static str> {
        if !self.is_pending() {
            return Ok(());
        }

        let serial_len = u64::try_from(serial.len()).map_err(|_| "exec serial length")?;
        if self.cursor > serial_len {
            return Err("exec serial cursor exceeds output length");
        }
        let start = usize::try_from(self.cursor).map_err(|_| "exec serial cursor")?;
        if start > serial.len() {
            return Err("exec serial cursor exceeds output length");
        }

        self.session.feed(&serial[start..]);
        self.cursor = serial_len;

        if let Some(Done::Sentinel { status, .. }) = self.session.done.as_ref() {
            self.completion = ExecCompletion::Exited {
                status: *status,
                at,
            };
        } else if terminal {
            self.session.finish_timeout();
            self.session.scan_buffer.clear();
            self.session.scan_base = 0;
            self.completion = ExecCompletion::Aborted { at };
        }

        self.validate_at(at, serial.len())
    }

    /// Validate the retained parser against the current VM endpoint.
    pub(crate) fn validate_at(&self, at: Moment, serial_len: usize) -> Result<(), &'static str> {
        self.validate_structure()?;
        let serial_len = u64::try_from(serial_len).map_err(|_| "exec serial length")?;
        if self.cursor > serial_len {
            return Err("exec serial cursor exceeds output length");
        }
        match self.completion {
            ExecCompletion::Pending => {}
            ExecCompletion::Exited { at: completed, .. }
            | ExecCompletion::Aborted { at: completed } => {
                if completed > at {
                    return Err("exec completion is after endpoint");
                }
            }
        }
        Ok(())
    }

    /// Bind pending parser bytes to the VM stream that will supply future bytes.
    pub(crate) fn validate_serial(&self, at: Moment, serial: &[u8]) -> Result<(), &'static str> {
        self.validate_at(at, serial.len())?;
        if self.is_pending() {
            let end = usize::try_from(self.cursor).map_err(|_| "exec serial cursor")?;
            let start = end
                .checked_sub(self.session.scan_buffer.len())
                .ok_or("exec scanner extends before serial output")?;
            if serial[start..end] != self.session.scan_buffer {
                return Err("exec scanner differs from VM serial output");
            }
            if !self.session.truncated {
                let start = end
                    .checked_sub(self.session.capture.len())
                    .ok_or("exec capture extends before serial output")?;
                if serial[start..end] != self.session.capture {
                    return Err("exec capture differs from VM serial output");
                }
            }
        }
        Ok(())
    }

    /// Encode only parser state and the absolute serial cursor.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            MAGIC.len() + 8 * 5 + 2 + self.session.capture.len() + self.session.scan_buffer.len(),
        );
        out.extend_from_slice(MAGIC);
        put_u64(&mut out, self.id);
        put_u64(&mut out, self.cursor);
        put_bytes(&mut out, &self.session.capture);
        put_bytes(&mut out, &self.session.scan_buffer);
        put_u64(&mut out, self.session.scan_base as u64);
        out.push(u8::from(self.session.truncated));
        match self.completion {
            ExecCompletion::Pending => out.push(COMPLETION_PENDING),
            ExecCompletion::Exited { status, at } => {
                out.push(COMPLETION_EXITED);
                put_u64(&mut out, status);
                put_u64(&mut out, at.0);
            }
            ExecCompletion::Aborted { at } => {
                out.push(COMPLETION_ABORTED);
                put_u64(&mut out, at.0);
            }
        }
        out
    }

    /// Strictly decode retained parser state.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        let mut reader = Reader::new(bytes);
        if reader.take(MAGIC.len())? != MAGIC {
            return Err("exec magic");
        }
        let id = reader.u64()?;
        let cursor = reader.u64()?;
        let template = ExecSession::new("", id);
        let capture = reader.bytes(MAX_CAPTURE)?;
        let scan_limit = template.sentinel_max_len().saturating_sub(1);
        let scan_buffer = reader.bytes(scan_limit)?;
        let scan_base = usize::try_from(reader.u64()?).map_err(|_| "exec scan base")?;
        if scan_base > MAX_CAPTURE.saturating_add(1) {
            return Err("exec scan base");
        }
        let truncated = match reader.byte()? {
            0 => false,
            1 => true,
            _ => return Err("exec truncation tag"),
        };
        let completion = match reader.byte()? {
            COMPLETION_PENDING => ExecCompletion::Pending,
            COMPLETION_EXITED => ExecCompletion::Exited {
                status: reader.u64()?,
                at: Moment(reader.u64()?),
            },
            COMPLETION_ABORTED => ExecCompletion::Aborted {
                at: Moment(reader.u64()?),
            },
            _ => return Err("exec completion tag"),
        };
        if !reader.empty() {
            return Err("trailing exec state");
        }

        let mut session = template;
        session.input.clear();
        session.capture = capture;
        session.scan_buffer = scan_buffer;
        session.scan_base = scan_base;
        session.truncated = truncated;
        session.done = match completion {
            ExecCompletion::Pending => None,
            ExecCompletion::Exited { status, .. } => Some(Done::Sentinel {
                status,
                cut: session.capture.len(),
            }),
            ExecCompletion::Aborted { .. } => Some(Done::Timeout),
        };

        let retained = Self {
            id,
            cursor,
            session,
            completion,
        };
        retained.validate_structure()?;
        Ok(retained)
    }

    fn validate_structure(&self) -> Result<(), &'static str> {
        if self.session.capture.len() > MAX_CAPTURE {
            return Err("exec capture exceeds cap");
        }
        if self.session.scan_base > MAX_CAPTURE.saturating_add(1) {
            return Err("exec scan base");
        }
        let keep = self.session.sentinel_max_len().saturating_sub(1);
        if self.session.scan_buffer.len() > keep {
            return Err("exec scanner overlap exceeds bound");
        }

        if self.session.truncated && self.session.capture.len() != MAX_CAPTURE {
            return Err("truncated exec capture is not full");
        }
        if self.cursor < self.session.capture.len() as u64 {
            return Err("exec cursor precedes captured output");
        }
        match (self.completion, self.session.done.as_ref()) {
            (ExecCompletion::Pending, None) => {
                let scanned_end = self.session.scan_base + self.session.scan_buffer.len();
                if scanned_end as u64 > self.cursor {
                    return Err("exec scanner extends beyond serial cursor");
                }
                if self.session.truncated {
                    if self.session.scan_buffer.len() != keep
                        || self.session.scan_base < MAX_CAPTURE + 1 - keep
                    {
                        return Err("truncated exec scanner overlap is incomplete");
                    }
                    let overlap = MAX_CAPTURE.saturating_sub(self.session.scan_base);
                    if overlap > 0
                        && self.session.scan_buffer[..overlap]
                            != self.session.capture[self.session.scan_base..]
                    {
                        return Err("exec scanner overlap does not match capture");
                    }
                } else {
                    let suffix_start = self.session.capture.len().saturating_sub(keep);
                    if self.session.scan_base != suffix_start
                        || self.session.scan_buffer != self.session.capture[suffix_start..]
                    {
                        return Err("exec scanner overlap does not match capture");
                    }
                }
                if self.session.scan().is_some() {
                    return Err("pending exec contains a complete sentinel");
                }
            }
            (
                ExecCompletion::Exited { status, .. },
                Some(Done::Sentinel {
                    status: parsed,
                    cut,
                }),
            ) if status == *parsed && *cut == self.session.capture.len() => {
                if !self.session.scan_buffer.is_empty() || self.session.scan_base != 0 {
                    return Err("completed exec retains scanner state");
                }
            }
            (ExecCompletion::Aborted { .. }, Some(Done::Timeout)) => {
                if !self.session.scan_buffer.is_empty() || self.session.scan_base != 0 {
                    return Err("aborted exec retains scanner state");
                }
            }
            _ => return Err("exec completion disagrees with parser"),
        }
        Ok(())
    }
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    put_u64(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], &'static str> {
        let end = self.offset.checked_add(len).ok_or("truncated exec state")?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or("truncated exec state")?;
        self.offset = end;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, &'static str> {
        Ok(self.take(1)?[0])
    }

    fn u64(&mut self) -> Result<u64, &'static str> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| "exec integer")?,
        ))
    }

    fn bytes(&mut self, max: usize) -> Result<Vec<u8>, &'static str> {
        let len = usize::try_from(self.u64()?).map_err(|_| "exec field length")?;
        if len > max {
            return Err("exec field exceeds bound");
        }
        Ok(self.take(len)?.to_vec())
    }

    fn empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(exec: &RetainedExec) -> String {
        String::from_utf8(exec.session.marker.clone()).expect("marker is printable")
    }

    fn field_offset_for_pending() -> usize {
        // magic, id, cursor, capture length, scan length, scan base, truncation,
        // completion tag.  All variable fields are empty in this fixture.
        MAGIC.len() + 8 + 8 + 8 + 8 + 8 + 1
    }

    #[test]
    fn partial_sentinel_roundtrips_and_continues_with_nonzero_status() {
        let mut warm = RetainedExec::new("false", 7, 0);
        let token = marker(&warm);
        let prefix = format!("output\n{token}:1");
        warm.feed(prefix.as_bytes(), Moment(10), false).unwrap();
        assert!(warm.is_pending());

        let encoded = warm.encode();
        let mut cold = RetainedExec::decode(&encoded).unwrap();
        assert!(
            cold.input().is_empty(),
            "restore cannot reinject the command"
        );

        let full = format!("{prefix}37:{token}\n");
        warm.feed(full.as_bytes(), Moment(20), false).unwrap();
        cold.feed(full.as_bytes(), Moment(20), false).unwrap();
        assert_eq!(warm.view(Moment(20)), cold.view(Moment(20)));
        assert_eq!(
            warm.view(Moment(20)).completion,
            ExecCompletion::Exited {
                status: 137,
                at: Moment(20),
            }
        );
    }

    #[test]
    fn completed_and_aborted_states_restore_without_mutation() {
        let mut exited = RetainedExec::new("true", 8, 0);
        let token = marker(&exited);
        let serial = format!("before {token}:3:{token}\n");
        exited.feed(serial.as_bytes(), Moment(4), false).unwrap();
        let mut exited_cold = RetainedExec::decode(&exited.encode()).unwrap();
        let exited_before = exited_cold.view(Moment(5));
        exited_cold.feed(b"shorter", Moment(6), false).unwrap();
        assert_eq!(exited_cold.view(Moment(6)).output, exited_before.output);
        assert_eq!(
            exited_cold.view(Moment(6)).completion,
            exited_before.completion
        );

        let mut aborted = RetainedExec::new("sleep 1", 9, 0);
        aborted.feed(b"partial", Moment(11), true).unwrap();
        let mut aborted_cold = RetainedExec::decode(&aborted.encode()).unwrap();
        let aborted_before = aborted_cold.view(Moment(12));
        aborted_cold
            .feed(b"partial plus more", Moment(13), false)
            .unwrap();
        assert_eq!(aborted_cold.view(Moment(13)).output, aborted_before.output);
        assert_eq!(
            aborted_cold.view(Moment(13)).completion,
            aborted_before.completion
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "the one-megabyte cap boundary is covered natively; small parser overlap is Miri-safe"
    )]
    fn sentinel_split_across_capture_cap_roundtrips() {
        let mut warm = RetainedExec::new("false", 12, 0);
        let token = marker(&warm);
        let prefix = vec![b'x'; MAX_CAPTURE - 1];
        let sentinel = format!("{token}:9:{token}\n");
        let split = token.len() / 2;
        let mut partial = prefix.clone();
        partial.extend_from_slice(&sentinel.as_bytes()[..split]);
        warm.feed(&partial, Moment(20), false).unwrap();
        assert!(warm.is_pending());
        assert!(warm.session.truncated);
        let mut cold = RetainedExec::decode(&warm.encode()).unwrap();

        let mut complete = partial;
        complete.extend_from_slice(&sentinel.as_bytes()[split..]);
        warm.feed(&complete, Moment(21), false).unwrap();
        cold.feed(&complete, Moment(21), false).unwrap();
        assert_eq!(warm.view(Moment(21)), cold.view(Moment(21)));
        assert_eq!(warm.view(Moment(21)).output.len(), MAX_CAPTURE - 1);
        assert!(!warm.view(Moment(21)).truncated);
    }

    #[test]
    fn strict_decode_rejects_tags_trailing_and_oversized_fields() {
        let valid = RetainedExec::new("x", 4, 0).encode();
        let mut bad_magic = valid.clone();
        bad_magic[0] ^= 1;
        assert!(matches!(
            RetainedExec::decode(&bad_magic),
            Err("exec magic")
        ));

        let mut trailing = valid.clone();
        trailing.push(0);
        assert!(matches!(
            RetainedExec::decode(&trailing),
            Err("trailing exec state")
        ));

        let completion = field_offset_for_pending();
        let mut bad_tag = valid.clone();
        bad_tag[completion] = 9;
        assert!(matches!(
            RetainedExec::decode(&bad_tag),
            Err("exec completion tag")
        ));

        let mut bad_capture_len = valid.clone();
        bad_capture_len[MAGIC.len() + 8 + 8..MAGIC.len() + 8 + 8 + 8]
            .copy_from_slice(&(MAX_CAPTURE as u64 + 1).to_le_bytes());
        assert!(matches!(
            RetainedExec::decode(&bad_capture_len),
            Err("exec field exceeds bound")
        ));

        let mut bad_scan_len = valid.clone();
        bad_scan_len[MAGIC.len() + 8 + 8 + 8..MAGIC.len() + 8 + 8 + 8 + 8]
            .copy_from_slice(&1000u64.to_le_bytes());
        assert!(matches!(
            RetainedExec::decode(&bad_scan_len),
            Err("exec field exceeds bound")
        ));

        let mut bad_base = valid;
        let base = MAGIC.len() + 8 + 8 + 8 + 8;
        bad_base[base..base + 8].copy_from_slice(&(MAX_CAPTURE as u64 + 2).to_le_bytes());
        assert!(matches!(
            RetainedExec::decode(&bad_base),
            Err("exec scan base")
        ));
    }

    #[test]
    fn endpoint_validation_rejects_time_and_cursor_inconsistency() {
        let mut exec = RetainedExec::new("x", 5, 0);
        let token = marker(&exec);
        let serial = format!("{token}:17:{token}");
        exec.feed(serial.as_bytes(), Moment(10), false).unwrap();
        assert_eq!(
            exec.validate_at(Moment(9), serial.len()),
            Err("exec completion is after endpoint")
        );
        assert_eq!(
            exec.validate_at(Moment(10), serial.len() - 1),
            Err("exec serial cursor exceeds output length")
        );
    }

    #[test]
    fn pending_decode_rejects_a_complete_sentinel() {
        let mut bad = RetainedExec::new("x", 6, 0);
        let token = marker(&bad);
        let complete = format!("{token}:0:{token}").into_bytes();
        bad.cursor = complete.len() as u64;
        bad.session.capture = complete.clone();
        bad.session.scan_buffer = complete;
        bad.session.scan_base = 0;
        assert!(matches!(
            RetainedExec::decode(&bad.encode()),
            Err("pending exec contains a complete sentinel")
        ));
    }

    #[test]
    fn truncated_parser_rejects_impossible_overlap_and_terminal_capture() {
        let serial = vec![b'x'; MAX_CAPTURE + 1];
        let mut pending = RetainedExec::new("cmd", 0, 0);
        pending.feed(&serial, Moment(1), false).unwrap();
        assert!(RetainedExec::decode(&pending.encode()).is_ok());
        pending.session.scan_buffer.clear();
        assert!(RetainedExec::decode(&pending.encode()).is_err());
        let mut aborted = RetainedExec::new("cmd", 0, 0);
        aborted.feed(&serial, Moment(1), true).unwrap();
        aborted.session.capture.pop();
        assert!(RetainedExec::decode(&aborted.encode()).is_err());
    }

    #[test]
    fn pending_scanner_cannot_contain_bytes_beyond_serial_cursor() {
        let serial = vec![b'x'; MAX_CAPTURE + 1];
        let mut bad = RetainedExec::new("cmd", 0, 0);
        bad.feed(&serial, Moment(1), false).unwrap();
        bad.cursor = MAX_CAPTURE as u64;
        bad.session.scan_base = MAX_CAPTURE + 1;
        assert!(matches!(
            RetainedExec::decode(&bad.encode()),
            Err("exec scanner extends beyond serial cursor")
        ));
    }

    #[test]
    fn restored_pending_overlap_must_match_the_vm_stream() {
        let serial = vec![b'x'; MAX_CAPTURE + 100];
        let mut command = RetainedExec::new("cmd", 0, 0);
        command.feed(&serial, Moment(1), false).unwrap();
        command.validate_serial(Moment(1), &serial).unwrap();
        command.session.scan_buffer[0] = b'y';
        let decoded = RetainedExec::decode(&command.encode()).unwrap();
        assert_eq!(
            decoded.validate_serial(Moment(1), &serial),
            Err("exec scanner differs from VM serial output")
        );
    }

    #[test]
    fn decoded_state_has_no_command_input() {
        let exec = RetainedExec::new("echo should-run-once", 10, 3);
        assert!(!exec.input().is_empty());
        let decoded = RetainedExec::decode(&exec.encode()).unwrap();
        assert!(decoded.input().is_empty());
    }
}
