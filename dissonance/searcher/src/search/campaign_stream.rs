// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    error::Error,
    io::Write,
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread::{self, JoinHandle},
};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{PlacedThread, ThreadPlacement};
use crate::search::telemetry::{StreamTelemetry, nanos_since, now, thread_schedstat};

const COMMAND_CAPACITY: usize = 16;
const OUTPUT_CAPACITY: usize = 1;

type Encode = Box<dyn FnOnce(&mut Vec<u8>) -> Result<(), String> + Send>;

struct Command {
    encode: Encode,
    buffer: Vec<u8>,
}

struct Encoded {
    digest: String,
    telemetry: StreamTelemetry,
}

struct Background {
    commands: Option<SyncSender<Command>>,
    output: Option<Receiver<Vec<u8>>>,
    worker: Option<JoinHandle<Result<Encoded, String>>>,
    recycled: Vec<Vec<u8>>,
    pending: u64,
    telemetry: StreamTelemetry,
}

impl Background {
    fn start(
        mut hasher: Sha256,
        placement: Option<ThreadPlacement>,
    ) -> Result<Self, Box<dyn Error>> {
        let (commands, pending) = mpsc::sync_channel::<Command>(COMMAND_CAPACITY);
        let (completed, output) = mpsc::sync_channel(OUTPUT_CAPACITY);
        let (ready, startup) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("campaign-stream".into())
            .spawn(move || {
                let placed =
                    ThreadPlacement::place(placement.as_ref(), PlacedThread::StreamEncoder);
                let _ = ready.send(placed.clone());
                placed?;
                let cpu_started = thread_schedstat();
                let mut telemetry = StreamTelemetry::default();
                for mut command in pending {
                    let started = now();
                    command.buffer.clear();
                    (command.encode)(&mut command.buffer)?;
                    command.buffer.push(b'\n');
                    hasher.update(&command.buffer);
                    telemetry.encode_ns = telemetry.encode_ns.saturating_add(nanos_since(started));
                    telemetry.background_records += 1;
                    telemetry.background_bytes += command.buffer.len() as u64;
                    telemetry.largest_buffer_capacity_bytes = telemetry
                        .largest_buffer_capacity_bytes
                        .max(command.buffer.capacity() as u64);
                    completed
                        .send(command.buffer)
                        .map_err(|_| "campaign stream consumer disconnected".to_owned())?;
                }
                if let Some(((before_cpu, before_wait), (after_cpu, after_wait))) =
                    cpu_started.zip(thread_schedstat())
                {
                    telemetry.worker_cpu_ns = Some(after_cpu.saturating_sub(before_cpu));
                    telemetry.worker_cpu_wait_ns = Some(after_wait.saturating_sub(before_wait));
                }
                Ok(Encoded {
                    digest: format!("{:x}", hasher.finalize()),
                    telemetry,
                })
            })?;
        let mut background = Self {
            commands: Some(commands),
            output: Some(output),
            worker: Some(worker),
            recycled: Vec::new(),
            pending: 0,
            telemetry: StreamTelemetry {
                command_capacity: COMMAND_CAPACITY,
                output_capacity: OUTPUT_CAPACITY,
                ..StreamTelemetry::default()
            },
        };
        match startup.recv() {
            Ok(Ok(())) => Ok(background),
            Ok(Err(error)) => Err(error.into()),
            Err(_) => Err(background.disconnected()),
        }
    }

    fn joined(&mut self) -> Result<Encoded, Box<dyn Error>> {
        self.worker
            .take()
            .ok_or("campaign stream worker was already joined")?
            .join()
            .map_err(|_| "campaign stream encoder panicked")?
            .map_err(Into::into)
    }

    fn disconnected(&mut self) -> Box<dyn Error> {
        self.commands.take();
        self.output.take();
        match self.joined() {
            Ok(_) => "campaign stream encoder disconnected before finish".into(),
            Err(error) => error,
        }
    }

    fn consume(&mut self, buffer: Vec<u8>, sink: &mut dyn Write) -> Result<(), Box<dyn Error>> {
        self.pending -= 1;
        let started = now();
        sink.write_all(&buffer)?;
        self.telemetry.sink_ns = self.telemetry.sink_ns.saturating_add(nanos_since(started));
        self.telemetry.sink_bytes += buffer.len() as u64;
        self.recycled.push(buffer);
        Ok(())
    }

    fn drain(&mut self, sink: &mut dyn Write) -> Result<(), Box<dyn Error>> {
        loop {
            match self
                .output
                .as_ref()
                .ok_or("campaign stream output was closed")?
                .try_recv()
            {
                Ok(buffer) => self.consume(buffer, sink)?,
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => return Err(self.disconnected()),
            }
        }
    }

    fn enqueue<T: Serialize + Send + 'static>(
        &mut self,
        value: T,
        sink: &mut dyn Write,
    ) -> Result<(), Box<dyn Error>> {
        let mut command = Command {
            encode: Box::new(move |buffer| {
                serde_json::to_writer(buffer, &value).map_err(|error| error.to_string())
            }),
            buffer: self.recycled.pop().unwrap_or_default(),
        };
        loop {
            match self
                .commands
                .as_ref()
                .ok_or("campaign stream input was closed")?
                .try_send(command)
            {
                Ok(()) => {
                    self.pending += 1;
                    self.telemetry.records_in_flight_peak =
                        self.telemetry.records_in_flight_peak.max(self.pending);
                    return self.drain(sink);
                }
                Err(TrySendError::Full(returned)) => {
                    command = returned;
                    self.telemetry.producer_backpressure_events += 1;
                    let started = now();
                    let received = self
                        .output
                        .as_ref()
                        .ok_or("campaign stream output was closed")?
                        .recv();
                    self.telemetry.producer_wait_ns = self
                        .telemetry
                        .producer_wait_ns
                        .saturating_add(nanos_since(started));
                    match received {
                        Ok(buffer) => self.consume(buffer, sink)?,
                        Err(_) => return Err(self.disconnected()),
                    }
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.drain(sink)?;
                    return Err(self.disconnected());
                }
            }
        }
    }

    fn finish(mut self, sink: &mut dyn Write) -> Result<Encoded, Box<dyn Error>> {
        self.commands.take();
        while let Ok(buffer) = self
            .output
            .as_ref()
            .ok_or("campaign stream output was closed")?
            .recv()
        {
            self.consume(buffer, sink)?;
        }
        self.output.take();
        let mut encoded = self.joined()?;
        if self.pending != 0 {
            return Err("campaign stream lost an encoded record".into());
        }
        encoded.telemetry.command_capacity = self.telemetry.command_capacity;
        encoded.telemetry.output_capacity = self.telemetry.output_capacity;
        encoded.telemetry.records_in_flight_peak = self.telemetry.records_in_flight_peak;
        encoded.telemetry.producer_backpressure_events =
            self.telemetry.producer_backpressure_events;
        encoded.telemetry.producer_wait_ns = self.telemetry.producer_wait_ns;
        encoded.telemetry.sink_ns = self.telemetry.sink_ns;
        encoded.telemetry.sink_bytes = self.telemetry.sink_bytes;
        Ok(encoded)
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        self.commands.take();
        self.output.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(super) struct StreamWriter<'a> {
    sink: &'a mut dyn Write,
    hasher: Option<Sha256>,
    line: Vec<u8>,
    background: Option<Background>,
    failed: bool,
}

impl<'a> StreamWriter<'a> {
    pub(super) fn new(sink: &'a mut dyn Write) -> Self {
        Self {
            sink,
            hasher: Some(Sha256::new()),
            line: Vec::new(),
            background: None,
            failed: false,
        }
    }

    pub(super) fn enable_background(
        &mut self,
        placement: Option<ThreadPlacement>,
    ) -> Result<(), Box<dyn Error>> {
        if self.background.is_none() {
            let hasher = self
                .hasher
                .take()
                .ok_or("campaign stream has no hash state")?;
            self.background = Some(Background::start(hasher, placement)?);
        }
        Ok(())
    }

    pub(super) fn write_line<T: Serialize + Send + 'static>(
        &mut self,
        value: T,
    ) -> Result<(), Box<dyn Error>> {
        if let Some(background) = self.background.as_mut() {
            let result = background.enqueue(value, self.sink);
            self.failed |= result.is_err();
            return result;
        }
        self.line.clear();
        serde_json::to_writer(&mut self.line, &value)?;
        self.line.push(b'\n');
        self.sink.write_all(&self.line)?;
        self.hasher
            .as_mut()
            .ok_or("campaign stream has no hash state")?
            .update(&self.line);
        Ok(())
    }

    pub(super) fn drain_ready(&mut self) -> Result<(), Box<dyn Error>> {
        if let Some(background) = self.background.as_mut() {
            let result = background.drain(self.sink);
            self.failed |= result.is_err();
            return result;
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<(String, Option<StreamTelemetry>), Box<dyn Error>> {
        let mut result = if let Some(background) = self.background.take() {
            let encoded = background.finish(self.sink)?;
            (encoded.digest, Some(encoded.telemetry))
        } else {
            (
                format!(
                    "{:x}",
                    self.hasher
                        .take()
                        .ok_or("campaign stream has no hash state")?
                        .finalize()
                ),
                None,
            )
        };
        let started = now();
        self.sink.flush()?;
        if let Some(telemetry) = result.1.as_mut() {
            telemetry.flush_ns = nanos_since(started);
        }
        Ok(result)
    }
}

impl Drop for StreamWriter<'_> {
    fn drop(&mut self) {
        if let Some(background) = self.background.take() {
            if self.failed {
                drop(background);
            } else {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                    || -> Result<(), Box<dyn Error>> {
                        background.finish(self.sink)?;
                        self.sink.flush()?;
                        Ok(())
                    },
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        io,
        rc::Rc,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use serde::{Serializer, ser::Error as _};

    use super::*;

    #[derive(Clone)]
    struct LocalSink(Rc<RefCell<Vec<u8>>>);

    impl Write for LocalSink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn ordered_background_bytes_and_digest_match_inline_with_a_non_send_sink() {
        let mut inline = Vec::new();
        let mut writer = StreamWriter::new(&mut inline);
        writer
            .write_line(serde_json::json!({"header": "prefix"}))
            .unwrap();
        for index in 0..1_000 {
            writer
                .write_line(serde_json::json!({"index": index, "text": "λ\\\"\n"}))
                .unwrap();
        }
        let (reference, telemetry) = writer.finish().unwrap();
        assert!(telemetry.is_none());
        let bytes = Rc::new(RefCell::new(Vec::new()));
        let mut sink = LocalSink(Rc::clone(&bytes));
        let mut writer = StreamWriter::new(&mut sink);
        writer
            .write_line(serde_json::json!({"header": "prefix"}))
            .unwrap();
        writer.enable_background(None).unwrap();
        for index in 0..1_000 {
            writer
                .write_line(serde_json::json!({"index": index, "text": "λ\\\"\n"}))
                .unwrap();
        }
        let (digest, telemetry) = writer.finish().unwrap();
        let telemetry = telemetry.unwrap();
        assert_eq!(*bytes.borrow(), inline);
        assert_eq!(digest, reference);
        assert_eq!(digest, format!("{:x}", Sha256::digest(&inline)));
        assert_eq!(telemetry.background_records, 1_000);
        assert_eq!(telemetry.sink_bytes, telemetry.background_bytes);
        assert!(telemetry.sink_bytes < inline.len() as u64);
        assert!(
            telemetry.records_in_flight_peak <= (COMMAND_CAPACITY + OUTPUT_CAPACITY + 1) as u64
        );
    }

    struct Invalid;
    impl Serialize for Invalid {
        fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(S::Error::custom("refused record"))
        }
    }

    struct Panicking;
    impl Serialize for Panicking {
        fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            panic!("encoder panic fixture")
        }
    }

    fn rejected<T: Serialize + Send + 'static>(value: T) -> String {
        let mut sink = Vec::new();
        let mut writer = StreamWriter::new(&mut sink);
        writer.enable_background(None).unwrap();
        match writer.write_line(value) {
            Err(error) => error.to_string(),
            Ok(()) => match writer.finish() {
                Err(error) => error.to_string(),
                Ok(_) => panic!("invalid stream was certified"),
            },
        }
    }

    #[test]
    fn serialization_errors_and_worker_panics_never_return_a_valid_digest() {
        assert!(rejected(Invalid).contains("refused record"));
        assert!(rejected(Panicking).contains("panicked"));
    }

    #[test]
    fn an_encoder_error_preserves_the_successful_prefix_and_drops_later_records() {
        let mut sink = Vec::new();
        let mut writer = StreamWriter::new(&mut sink);
        writer.enable_background(None).unwrap();
        writer.write_line(1).unwrap();
        let rejected = writer.write_line(Invalid).is_err();
        if rejected || writer.write_line(2).is_err() {
            drop(writer);
        } else {
            assert!(writer.finish().is_err());
        }
        assert_eq!(sink, b"1\n");
    }

    struct PanicFlush;
    impl Write for PanicFlush {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("secondary sink panic");
        }
    }

    #[test]
    fn cleanup_preserves_the_primary_unwind_when_the_sink_panics() {
        let result = std::panic::catch_unwind(|| {
            let mut sink = PanicFlush;
            let mut writer = StreamWriter::new(&mut sink);
            writer.enable_background(None).unwrap();
            writer.write_line(1).unwrap();
            panic!("primary campaign panic");
        });
        assert_eq!(
            result.unwrap_err().downcast_ref::<&str>(),
            Some(&"primary campaign panic")
        );
    }

    struct BrokenSink {
        fail_flush: bool,
    }
    impl Write for BrokenSink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail_flush {
                Ok(bytes.len())
            } else {
                Err(io::Error::other("broken stream sink"))
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("broken stream flush"))
        }
    }

    #[test]
    fn sink_write_and_flush_errors_fail_closed_without_join_deadlocks() {
        for fail_flush in [false, true] {
            let mut sink = BrokenSink { fail_flush };
            let mut writer = StreamWriter::new(&mut sink);
            writer.enable_background(None).unwrap();
            let mut failed = false;
            for index in 0..100 {
                if let Err(error) = writer.write_line(index) {
                    assert!(error.to_string().contains("broken stream sink"));
                    failed = true;
                    break;
                }
            }
            if failed {
                drop(writer);
            } else {
                let error = writer.finish().unwrap_err();
                assert!(error.to_string().contains(if fail_flush {
                    "broken stream flush"
                } else {
                    "broken stream sink"
                }));
            }
        }
    }

    struct Dropped(Arc<AtomicUsize>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl Serialize for Dropped {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_u8(1)
        }
    }

    #[test]
    fn early_drop_closes_bounded_channels_and_releases_every_record() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let mut sink = Vec::new();
        let mut writer = StreamWriter::new(&mut sink);
        writer.enable_background(None).unwrap();
        for _ in 0..100 {
            writer.write_line(Dropped(Arc::clone(&dropped))).unwrap();
        }
        drop(writer);
        assert_eq!(dropped.load(Ordering::SeqCst), 100);
        assert_eq!(sink, b"1\n".repeat(100));
    }

    #[test]
    fn placement_runs_once_and_startup_errors_and_panics_propagate() {
        let calls = Arc::new(AtomicUsize::new(0));
        let recorded = Arc::clone(&calls);
        let placement = ThreadPlacement::new(move |role| {
            assert_eq!(role, PlacedThread::StreamEncoder);
            recorded.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        let mut sink = Vec::new();
        let mut writer = StreamWriter::new(&mut sink);
        writer.enable_background(Some(placement)).unwrap();
        writer.enable_background(None).unwrap();
        writer.finish().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        for panic in [false, true] {
            let placement = ThreadPlacement::new(move |_| {
                if panic {
                    panic!("placement panic fixture");
                }
                Err("encoder placement refused".into())
            });
            let mut sink = Vec::new();
            let mut writer = StreamWriter::new(&mut sink);
            let error = writer
                .enable_background(Some(placement))
                .unwrap_err()
                .to_string();
            assert!(error.contains(if panic {
                "panicked"
            } else {
                "placement refused"
            }));
        }
    }

    #[test]
    fn full_command_queue_applies_backpressure_and_preserves_order() {
        let mut sink = Vec::new();
        let mut writer = StreamWriter::new(&mut sink);
        writer.enable_background(None).unwrap();
        let background = writer.background.as_mut().unwrap();
        let (release, blocked) = mpsc::channel();
        background
            .commands
            .as_ref()
            .unwrap()
            .send(Command {
                encode: Box::new(move |buffer| {
                    blocked.recv().unwrap();
                    buffer.extend_from_slice(b"0");
                    Ok(())
                }),
                buffer: Vec::new(),
            })
            .unwrap_or_else(|_| panic!("encoder stopped"));
        background.pending += 1;
        for index in 1..=COMMAND_CAPACITY {
            background
                .commands
                .as_ref()
                .unwrap()
                .send(Command {
                    encode: Box::new(move |buffer| {
                        serde_json::to_writer(buffer, &index).map_err(|e| e.to_string())
                    }),
                    buffer: Vec::new(),
                })
                .unwrap_or_else(|_| panic!("encoder stopped"));
            background.pending += 1;
        }
        assert!(matches!(
            background.commands.as_ref().unwrap().try_send(Command {
                encode: Box::new(|_| Ok(())),
                buffer: Vec::new()
            }),
            Err(TrySendError::Full(_))
        ));
        release.send(()).unwrap();
        writer.write_line(COMMAND_CAPACITY + 1).unwrap();
        let (_, telemetry) = writer.finish().unwrap();
        assert_eq!(
            telemetry.unwrap().background_records,
            (COMMAND_CAPACITY + 2) as u64
        );
        let actual: Vec<usize> = sink
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(actual, (0..=COMMAND_CAPACITY + 1).collect::<Vec<_>>());
    }
}
