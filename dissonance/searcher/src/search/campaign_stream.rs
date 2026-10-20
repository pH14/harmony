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
use crate::search::telemetry::{Moment, StreamTelemetry, nanos_since, now, thread_schedstat};

const COMMAND_CAPACITY: usize = 16;
const OUTPUT_CAPACITY: usize = 1;
const BATCH_RECORD_LIMIT: usize = 16;
const PARTIAL_BATCH_AGE_NS: u64 = 1_000_000;

struct Command<R> {
    records: Vec<R>,
    buffer: Vec<u8>,
}

struct Chunk<R> {
    buffer: Vec<u8>,
    records: u64,
    recycled_records: Vec<R>,
}

struct Encoded {
    digest: String,
    telemetry: StreamTelemetry,
}

struct Background<R> {
    commands: Option<SyncSender<Command<R>>>,
    output: Option<Receiver<Chunk<R>>>,
    worker: Option<JoinHandle<Result<Encoded, String>>>,
    recycled: Vec<Vec<u8>>,
    recycled_records: Vec<Vec<R>>,
    pending: u64,
    telemetry: StreamTelemetry,
}

impl<R: Serialize + Send + 'static> Background<R> {
    fn start(
        mut hasher: Sha256,
        placement: Option<ThreadPlacement>,
    ) -> Result<Self, Box<dyn Error>> {
        let (commands, pending) = mpsc::sync_channel::<Command<R>>(COMMAND_CAPACITY);
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
                    let mut records = 0;
                    let mut failure = None;
                    for record in command.records.drain(..) {
                        let offset = command.buffer.len();
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let result = serde_json::to_writer(&mut command.buffer, &record)
                                .map_err(|error| error.to_string());
                            drop(record);
                            result
                        }));
                        match result {
                            Ok(Ok(())) => {
                                command.buffer.push(b'\n');
                                records += 1;
                            }
                            other => {
                                command.buffer.truncate(offset);
                                failure = Some(match other {
                                    Ok(Err(error)) => error,
                                    Err(_) => "campaign stream encoder panicked".to_owned(),
                                    Ok(Ok(())) => unreachable!(),
                                });
                                break;
                            }
                        }
                    }
                    hasher.update(&command.buffer);
                    telemetry.encode_ns = telemetry.encode_ns.saturating_add(nanos_since(started));
                    telemetry.background_records += records;
                    telemetry.background_batches += 1;
                    telemetry.largest_batch_records = telemetry.largest_batch_records.max(records);
                    telemetry.background_bytes += command.buffer.len() as u64;
                    telemetry.largest_buffer_capacity_bytes = telemetry
                        .largest_buffer_capacity_bytes
                        .max(command.buffer.capacity() as u64);
                    completed
                        .send(Chunk {
                            buffer: command.buffer,
                            records,
                            recycled_records: command.records,
                        })
                        .map_err(|_| "campaign stream consumer disconnected".to_owned())?;
                    if let Some(error) = failure {
                        return Err(error);
                    }
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
            recycled_records: Vec::new(),
            pending: 0,
            telemetry: StreamTelemetry {
                command_capacity: COMMAND_CAPACITY,
                output_capacity: OUTPUT_CAPACITY,
                batch_record_limit: BATCH_RECORD_LIMIT,
                partial_batch_age_ns: PARTIAL_BATCH_AGE_NS,
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

    fn consume(&mut self, chunk: Chunk<R>, sink: &mut dyn Write) -> Result<(), Box<dyn Error>> {
        self.pending -= chunk.records;
        let buffer = chunk.buffer;
        self.recycled_records.push(chunk.recycled_records);
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

    fn enqueue(&mut self, records: Vec<R>, sink: &mut dyn Write) -> Result<(), Box<dyn Error>> {
        let count = records.len() as u64;
        let mut command = Command {
            records,
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
                    self.pending += count;
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
        encoded.telemetry.batch_record_limit = BATCH_RECORD_LIMIT;
        encoded.telemetry.partial_batch_age_ns = PARTIAL_BATCH_AGE_NS;
        encoded.telemetry.records_in_flight_peak = self.telemetry.records_in_flight_peak;
        encoded.telemetry.producer_backpressure_events =
            self.telemetry.producer_backpressure_events;
        encoded.telemetry.producer_wait_ns = self.telemetry.producer_wait_ns;
        encoded.telemetry.sink_ns = self.telemetry.sink_ns;
        encoded.telemetry.sink_bytes = self.telemetry.sink_bytes;
        Ok(encoded)
    }
}

impl<R> Drop for Background<R> {
    fn drop(&mut self) {
        self.commands.take();
        self.output.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(super) struct StreamWriter<'a, R: Serialize + Send + 'static> {
    sink: &'a mut dyn Write,
    hasher: Option<Sha256>,
    line: Vec<u8>,
    background: Option<Background<R>>,
    failed: bool,
    staged: Vec<R>,
    staged_since: Option<Moment>,
}

impl<'a, R: Serialize + Send + 'static> StreamWriter<'a, R> {
    pub(super) fn new(sink: &'a mut dyn Write) -> Self {
        Self {
            sink,
            hasher: Some(Sha256::new()),
            line: Vec::new(),
            background: None,
            failed: false,
            staged: Vec::new(),
            staged_since: None,
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
            self.staged.reserve_exact(BATCH_RECORD_LIMIT);
        }
        Ok(())
    }

    pub(super) fn write_header<T: Serialize>(&mut self, value: &T) -> Result<(), Box<dyn Error>> {
        if self.background.is_some() {
            return Err("campaign stream header must precede background encoding".into());
        }
        self.write_inline(value)
    }

    pub(super) fn write_line(&mut self, value: R) -> Result<(), Box<dyn Error>> {
        if self.background.is_some() {
            if self.staged.is_empty() {
                self.staged_since = Some(now());
            }
            self.staged.push(value);
            if self.staged.len() == BATCH_RECORD_LIMIT {
                return self.flush_staged();
            }
            return Ok(());
        }
        self.write_inline(&value)
    }

    fn write_inline<T: Serialize>(&mut self, value: &T) -> Result<(), Box<dyn Error>> {
        self.line.clear();
        serde_json::to_writer(&mut self.line, value)?;
        self.line.push(b'\n');
        self.sink.write_all(&self.line)?;
        self.hasher
            .as_mut()
            .ok_or("campaign stream has no hash state")?
            .update(&self.line);
        Ok(())
    }

    fn flush_staged(&mut self) -> Result<(), Box<dyn Error>> {
        if self.staged.is_empty() {
            return Ok(());
        }
        self.staged_since = None;
        let background = self
            .background
            .as_mut()
            .ok_or("campaign stream has no encoder")?;
        let records = std::mem::take(&mut self.staged);
        let result = background.enqueue(records, self.sink);
        self.staged = background
            .recycled_records
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(BATCH_RECORD_LIMIT));
        self.failed |= result.is_err();
        result
    }

    pub(super) fn drain_ready(&mut self) -> Result<(), Box<dyn Error>> {
        if self
            .staged_since
            .is_some_and(|started| nanos_since(started) >= PARTIAL_BATCH_AGE_NS)
        {
            self.flush_staged()?;
        }
        if let Some(background) = self.background.as_mut() {
            let result = background.drain(self.sink);
            self.failed |= result.is_err();
            return result;
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<(String, Option<StreamTelemetry>), Box<dyn Error>> {
        self.flush_staged()?;
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

impl<R: Serialize + Send + 'static> Drop for StreamWriter<'_, R> {
    fn drop(&mut self) {
        if !self.failed {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || -> Result<(), Box<dyn Error>> {
                    self.flush_staged()?;
                    if let Some(background) = self.background.take() {
                        background.finish(self.sink)?;
                        self.sink.flush()?;
                    }
                    Ok(())
                },
            ));
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
            .write_header(&serde_json::json!({"header": "prefix"}))
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
            .write_header(&serde_json::json!({"header": "prefix"}))
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
        assert_eq!(telemetry.background_batches, 63);
        assert_eq!(telemetry.largest_batch_records, BATCH_RECORD_LIMIT as u64);
        assert_eq!(telemetry.batch_record_limit, BATCH_RECORD_LIMIT);
        assert_eq!(telemetry.sink_bytes, telemetry.background_bytes);
        assert!(telemetry.sink_bytes < inline.len() as u64);
        assert!(
            telemetry.records_in_flight_peak
                <= ((COMMAND_CAPACITY + OUTPUT_CAPACITY + 1) * BATCH_RECORD_LIMIT) as u64
        );
    }

    struct LocalHeader(Rc<String>);

    impl Serialize for LocalHeader {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            self.0.as_str().serialize(serializer)
        }
    }

    #[test]
    fn heterogeneous_borrowed_non_send_headers_stay_inline() {
        let mut sink = Vec::new();
        let mut writer = StreamWriter::new(&mut sink);
        let header = LocalHeader(Rc::new("header".to_owned()));
        writer.write_header(&header).unwrap();
        assert_eq!(Rc::strong_count(&header.0), 1);
        writer.enable_background(None).unwrap();
        assert!(writer.write_header(&header).is_err());
        writer.write_line(17_u64).unwrap();
        let (digest, _) = writer.finish().unwrap();
        assert_eq!(sink, b"\"header\"\n17\n");
        assert_eq!(digest, format!("{:x}", Sha256::digest(&sink)));
    }

    #[test]
    fn typed_record_storage_returns_empty_with_its_original_allocation() {
        let mut sink = Vec::new();
        let mut background = Background::start(Sha256::new(), None).unwrap();
        let mut records = Vec::with_capacity(BATCH_RECORD_LIMIT);
        records.extend(0_u64..BATCH_RECORD_LIMIT as u64);
        let allocation = records.as_ptr();
        let capacity = records.capacity();
        background.enqueue(records, &mut sink).unwrap();
        if background.recycled_records.is_empty() {
            let chunk = background.output.as_ref().unwrap().recv().unwrap();
            background.consume(chunk, &mut sink).unwrap();
        }
        let mut recycled = background.recycled_records.pop().unwrap();
        assert!(recycled.is_empty());
        assert_eq!(recycled.as_ptr(), allocation);
        assert_eq!(recycled.capacity(), capacity);
        recycled.extend(100_u64..100 + BATCH_RECORD_LIMIT as u64);
        assert_eq!(recycled.as_ptr(), allocation);
        background.enqueue(recycled, &mut sink).unwrap();
        let encoded = background.finish(&mut sink).unwrap();
        assert_eq!(
            encoded.telemetry.background_records,
            2 * BATCH_RECORD_LIMIT as u64
        );
        let expected = (0_u64..BATCH_RECORD_LIMIT as u64)
            .chain(100_u64..100 + BATCH_RECORD_LIMIT as u64)
            .map(|value| format!("{value}\n"))
            .collect::<String>();
        assert_eq!(sink, expected.as_bytes());
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

    #[derive(Serialize)]
    #[serde(untagged)]
    enum Record<T> {
        Number(usize),
        Value(T),
    }

    #[test]
    fn an_encoder_error_preserves_the_successful_prefix_and_drops_later_records() {
        let mut sink = Vec::new();
        let mut writer = StreamWriter::new(&mut sink);
        writer.enable_background(None).unwrap();
        writer.write_line(Record::Number(1)).unwrap();
        let rejected = writer.write_line(Record::Value(Invalid)).is_err();
        if rejected || writer.write_line(Record::Number(2)).is_err() {
            drop(writer);
        } else {
            assert!(writer.finish().is_err());
        }
        assert_eq!(sink, b"1\n");
    }

    struct PartialInvalid;
    impl Serialize for PartialInvalid {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeSeq;
            let mut sequence = serializer.serialize_seq(Some(2))?;
            sequence.serialize_element(&123)?;
            Err(S::Error::custom("partial record refused"))
        }
    }

    fn in_batch_failure<T: Serialize + Send + 'static>(value: T, expected: &str) {
        let mut sink = Vec::new();
        let mut writer = StreamWriter::new(&mut sink);
        writer.enable_background(None).unwrap();
        writer.write_line(Record::Number(1)).unwrap();
        writer.write_line(Record::Number(2)).unwrap();
        writer.write_line(Record::Value(value)).unwrap();
        writer.write_line(Record::Number(4)).unwrap();
        assert!(writer.finish().unwrap_err().to_string().contains(expected));
        assert_eq!(sink, b"1\n2\n");
    }

    struct PanicDrop;

    impl Serialize for PanicDrop {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_u8(3)
        }
    }

    impl Drop for PanicDrop {
        fn drop(&mut self) {
            panic!("record drop panic fixture");
        }
    }

    #[test]
    fn an_in_batch_partial_serialization_error_or_panic_keeps_only_complete_prefix_records() {
        in_batch_failure(PartialInvalid, "partial record refused");
        in_batch_failure(Panicking, "panicked");
        in_batch_failure(PanicDrop, "panicked");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_partial_batch_is_submitted_by_age_and_finish_preserves_the_remainder() {
        let mut sink = Vec::new();
        let mut writer = StreamWriter::new(&mut sink);
        writer.enable_background(None).unwrap();
        writer.write_line(1).unwrap();
        assert_eq!(writer.staged.len(), 1);
        writer.staged_since = Some(now() - std::time::Duration::from_millis(2));
        writer.drain_ready().unwrap();
        assert!(writer.staged.is_empty());
        writer.write_line(2).unwrap();
        let (_, telemetry) = writer.finish().unwrap();
        let telemetry = telemetry.unwrap();
        assert_eq!(telemetry.background_batches, 2);
        assert_eq!(telemetry.background_records, 2);
        assert_eq!(telemetry.largest_batch_records, 1);
        assert_eq!(sink, b"1\n2\n");
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
        writer.write_line(0_u8).unwrap();
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
            writer.write_line(0_u8).unwrap();
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

    struct WaitingRecord {
        value: usize,
        wait: Option<Receiver<()>>,
    }

    impl Serialize for WaitingRecord {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            if let Some(wait) = &self.wait {
                wait.recv().unwrap();
            }
            self.value.serialize(serializer)
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
                records: vec![WaitingRecord {
                    value: 0,
                    wait: Some(blocked),
                }],
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
                    records: vec![WaitingRecord {
                        value: index,
                        wait: None,
                    }],
                    buffer: Vec::new(),
                })
                .unwrap_or_else(|_| panic!("encoder stopped"));
            background.pending += 1;
        }
        assert!(matches!(
            background.commands.as_ref().unwrap().try_send(Command {
                records: vec![WaitingRecord {
                    value: usize::MAX,
                    wait: None
                }],
                buffer: Vec::new()
            }),
            Err(TrySendError::Full(_))
        ));
        release.send(()).unwrap();
        writer
            .write_line(WaitingRecord {
                value: COMMAND_CAPACITY + 1,
                wait: None,
            })
            .unwrap();
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
