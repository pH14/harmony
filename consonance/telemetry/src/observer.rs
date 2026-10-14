// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io::{self, Write};

use crate::event::{Event, to_ndjson};

pub trait Observer {
    fn emit(&mut self, ev: &Event);
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NullObserver;

impl Observer for NullObserver {
    #[inline]
    fn emit(&mut self, _ev: &Event) {}
}

#[derive(Debug)]
pub struct NdjsonRecorder<W: Write> {
    writer: W,
    first_error: Option<io::Error>,
    failed: bool,
}

impl<W: Write> NdjsonRecorder<W> {
    pub fn new(writer: W) -> NdjsonRecorder<W> {
        NdjsonRecorder {
            writer,
            first_error: None,
            failed: false,
        }
    }

    pub fn flush(&mut self) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::other(
                "recorder is incomplete after a write failure",
            ));
        }
        match self.writer.flush() {
            Ok(()) => Ok(()),
            Err(error) => {
                self.failed = true;
                self.first_error = Some(io::Error::new(error.kind(), error.to_string()));
                Err(error)
            }
        }
    }

    pub fn error(&self) -> Option<&io::Error> {
        self.first_error.as_ref()
    }

    pub fn take_error(&mut self) -> Option<io::Error> {
        self.first_error.take()
    }

    pub fn into_inner(self) -> W {
        self.writer
    }

    fn write_line(&mut self, ev: &Event) -> io::Result<()> {
        let line = to_ndjson(ev).map_err(io::Error::other)?;
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")
    }
}

impl<W: Write> Observer for NdjsonRecorder<W> {
    fn emit(&mut self, ev: &Event) {
        if self.failed {
            return;
        }
        if let Err(e) = self.write_line(ev) {
            self.failed = true;
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
        rec.emit(&sample(0));
        rec.emit(&sample(1));
        assert!(rec.error().is_some());
        assert!(rec.take_error().is_some());
        assert!(rec.error().is_none());
    }

    struct FailsOnceAfterPartialWrite {
        bytes: Vec<u8>,
        writes: usize,
    }

    impl Write for FailsOnceAfterPartialWrite {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            match self.writes {
                1 => {
                    self.bytes.extend_from_slice(&buf[..5]);
                    Ok(5)
                }
                2 => Err(io::Error::other("temporary write failure")),
                _ => {
                    self.bytes.extend_from_slice(buf);
                    Ok(buf.len())
                }
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn taking_a_write_error_does_not_resume_a_partial_record() {
        let writer = FailsOnceAfterPartialWrite {
            bytes: Vec::new(),
            writes: 0,
        };
        let mut rec = NdjsonRecorder::new(writer);
        rec.emit(&sample(0));
        let incomplete = rec.writer.bytes.clone();
        assert_eq!(incomplete.len(), 5);
        assert!(rec.take_error().is_some());
        rec.emit(&sample(1));
        assert_eq!(rec.writer.bytes, incomplete);
        assert!(rec.flush().is_err());
    }

    struct FailsFirstFlush {
        bytes: Vec<u8>,
        flushes: usize,
    }

    impl Write for FailsFirstFlush {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.flushes == 1 {
                Err(io::Error::other("flush failed"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn flush_failure_latches_the_recorder() {
        let writer = FailsFirstFlush {
            bytes: Vec::new(),
            flushes: 0,
        };
        let mut rec = NdjsonRecorder::new(writer);
        rec.emit(&sample(0));
        let written = rec.writer.bytes.clone();
        assert!(rec.flush().is_err());
        assert!(rec.take_error().is_some());
        rec.emit(&sample(1));
        assert_eq!(rec.writer.bytes, written);
        assert!(rec.flush().is_err());
    }

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

        rec.emit(&sample(0));
        assert!(
            committed.borrow().is_empty(),
            "emit must not flush on its own"
        );

        rec.flush().expect("flush");
        assert!(
            !committed.borrow().is_empty(),
            "flush must push staged bytes through the writer"
        );
    }
}
