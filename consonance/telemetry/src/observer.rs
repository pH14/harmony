// SPDX-License-Identifier: AGPL-3.0-or-later
//! The [`Observer`] tap and its sinks.
//!
//! An `Observer` is the **one new seam** of this task: `vmm-core` calls
//! [`Observer::emit`] after each serviced exit, handing it an already-built
//! [`Event`]. The contract is **read-only** — `emit` takes `&Event` and returns
//! `()`, so an observer can neither draw entropy, advance `work`, nor mutate any
//! guest/VMM state. Determinism is therefore preserved *by construction*:
//! attaching any observer cannot change the run (see `docs/INTEGRATION.md` §8).
//!
//! [`NullObserver`] is the **default** (a no-op), so M1/M2/corpus/Linux goldens
//! stay byte-identical unless an operator opts a real sink in. [`NdjsonRecorder`]
//! is the **lossless** persisted record (the replay source of truth);
//! [`crate::LiveSink`] is the **lossy, never-blocking** live lane.

use std::io::{self, Write};

use crate::event::{Event, to_ndjson};

/// A read-only telemetry tap. `vmm-core` calls [`Observer::emit`] after each
/// exit is fully serviced, at the quiescent point where `work` is already read
/// for V-time.
///
/// **Read-only contract.** `emit` receives an already-built [`Event`] by shared
/// reference and returns `()`. An implementation must never draw entropy,
/// advance `work`, or mutate any state that feeds `state_hash`/`observable_digest`
/// — it has no `&mut` access to any of it. This is what makes the tap
/// determinism-safe: attaching it cannot perturb the run.
pub trait Observer {
    /// Observe one serviced exit. Must not panic and must not mutate guest/VMM
    /// state (the signature already forbids the latter).
    fn emit(&mut self, ev: &Event);
}

/// The default observer: `emit` is a no-op. Zero-sized, so the default tap costs
/// nothing and — being a no-op — guarantees byte-identical runs.
#[derive(Clone, Copy, Debug, Default)]
pub struct NullObserver;

impl Observer for NullObserver {
    #[inline]
    fn emit(&mut self, _ev: &Event) {
        // Intentionally empty: the default tap observes nothing.
    }
}

/// A **lossless** sink: writes one `serde_json` NDJSON line per event to an
/// arbitrary [`Write`]. This is the persisted recording and the **replay source
/// of truth** — a box run captures its event stream here, and the Mac console
/// replays the file identically.
///
/// Because [`Observer::emit`] returns `()`, a write error cannot propagate; the
/// recorder stashes the **first** error (queryable via [`NdjsonRecorder::error`]
/// / [`NdjsonRecorder::take_error`]) and stops writing, rather than panicking.
/// Callers that need durability check the error after the run and flush.
#[derive(Debug)]
pub struct NdjsonRecorder<W: Write> {
    writer: W,
    first_error: Option<io::Error>,
}

impl<W: Write> NdjsonRecorder<W> {
    /// Wraps a writer (a file, a socket, a `Vec<u8>` in tests).
    pub fn new(writer: W) -> NdjsonRecorder<W> {
        NdjsonRecorder {
            writer,
            first_error: None,
        }
    }

    /// Flushes the underlying writer.
    ///
    /// # Errors
    ///
    /// Propagates the underlying [`Write::flush`] error.
    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }

    /// Borrows the first write/encode error seen, if any (`None` ⇒ lossless so
    /// far).
    pub fn error(&self) -> Option<&io::Error> {
        self.first_error.as_ref()
    }

    /// Takes the first error, clearing it.
    pub fn take_error(&mut self) -> Option<io::Error> {
        self.first_error.take()
    }

    /// Consumes the recorder, returning the wrapped writer.
    pub fn into_inner(self) -> W {
        self.writer
    }

    /// Writes one framed NDJSON line, recording the first error and then going
    /// quiet. Factored out so `emit` stays panic-free.
    fn write_line(&mut self, ev: &Event) -> io::Result<()> {
        // `to_ndjson` is infallible for a well-formed Event; map a wire error to
        // io so a single early-return covers both failure modes.
        let line = to_ndjson(ev).map_err(io::Error::other)?;
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")
    }
}

impl<W: Write> Observer for NdjsonRecorder<W> {
    fn emit(&mut self, ev: &Event) {
        if self.first_error.is_some() {
            // Already failed once; stay quiet rather than spam a dead writer.
            return;
        }
        if let Err(e) = self.write_line(ev) {
            self.first_error = Some(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventKind;

    fn sample(seq: u64) -> Event {
        Event::new(
            seq,
            seq * 2,
            seq,
            EventKind::Console {
                text: format!("line {seq}\n"),
            },
        )
    }

    #[test]
    fn null_observer_is_a_zero_sized_no_op() {
        assert_eq!(std::mem::size_of::<NullObserver>(), 0);
        let mut obs = NullObserver;
        // Emitting many events changes nothing observable and never panics.
        for i in 0..1000 {
            obs.emit(&sample(i));
        }
    }

    #[test]
    fn recorder_writes_one_line_per_event() {
        let mut rec = NdjsonRecorder::new(Vec::<u8>::new());
        for i in 0..3 {
            rec.emit(&sample(i));
        }
        assert!(rec.error().is_none());
        let buf = rec.into_inner();
        let text = String::from_utf8(buf).expect("utf8");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        for (i, line) in lines.iter().enumerate() {
            let ev = crate::event::from_ndjson(line).expect("decode");
            assert_eq!(ev.seq, i as u64);
        }
    }

    /// A writer that fails after N bytes, to prove `emit` swallows the error.
    struct Failing {
        budget: usize,
    }
    impl Write for Failing {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.budget == 0 {
                return Err(io::Error::other("disk full"));
            }
            let n = buf.len().min(self.budget);
            self.budget -= n;
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn recorder_records_first_error_and_does_not_panic() {
        let mut rec = NdjsonRecorder::new(Failing { budget: 5 });
        rec.emit(&sample(0)); // first line exceeds the 5-byte budget → error
        rec.emit(&sample(1)); // stays quiet
        assert!(rec.error().is_some());
        assert!(rec.take_error().is_some());
        assert!(rec.error().is_none());
    }

    /// A writer that stages writes and only commits them to a shared buffer on
    /// `flush`, so a `flush` that does nothing is observable as missing bytes.
    struct DeferredWriter {
        staged: Vec<u8>,
        committed: std::rc::Rc<std::cell::RefCell<Vec<u8>>>,
    }
    impl Write for DeferredWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.staged.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.committed.borrow_mut().extend_from_slice(&self.staged);
            self.staged.clear();
            Ok(())
        }
    }

    #[test]
    fn flush_forwards_to_the_underlying_writer() {
        let committed = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let writer = DeferredWriter {
            staged: Vec::new(),
            committed: std::rc::Rc::clone(&committed),
        };
        let mut rec = NdjsonRecorder::new(writer);

        rec.emit(&sample(0)); // emit writes but does not flush…
        assert!(
            committed.borrow().is_empty(),
            "emit must not flush on its own"
        );

        rec.flush().expect("flush");
        // …so the bytes only become visible if `flush` actually forwards.
        assert!(
            !committed.borrow().is_empty(),
            "flush must push staged bytes through the writer"
        );
    }
}
