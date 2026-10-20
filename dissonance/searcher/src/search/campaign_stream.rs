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
use crate::search::telemetry::{
    Moment, StreamTelemetry, StreamThreadTelemetry, nanos_since, now, thread_schedstat,
};

const SERIALIZERS: usize = 2;
const COMMAND_CAPACITY: usize = 6;
const SERIALIZER_OUTPUT_CAPACITY: usize = 1;
const OUTPUT_CAPACITY: usize = 1;
const BATCH_RECORD_LIMIT: usize = 16;
const PARTIAL_BATCH_AGE_NS: u64 = 1_000_000;

struct Envelope<R, D> {
    record: Option<R>,
    cleanup: Option<D>,
}

fn guarded_drop<T>(value: T) -> Result<(), String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(value)))
        .map_err(|_| "campaign stream payload destruction panicked".to_owned())
}

impl<R, D> Drop for Envelope<R, D> {
    fn drop(&mut self) {
        let _ = guarded_drop(self.cleanup.take());
        let _ = guarded_drop(self.record.take());
    }
}

fn encode_envelope<R: Serialize, D>(
    mut envelope: Envelope<R, D>,
    buffer: &mut Vec<u8>,
) -> Result<(), String> {
    guarded_drop(envelope.cleanup.take())?;
    let record = envelope.record.take();
    let serialized = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        serde_json::to_writer(buffer, &record).map_err(|error| error.to_string())
    }))
    .unwrap_or_else(|_| Err("campaign stream encoder panicked".to_owned()));
    let disposed = guarded_drop(record);
    serialized.and(disposed)
}

struct Command<R, D> {
    sequence: u64,
    records: Vec<Envelope<R, D>>,
    buffer: Vec<u8>,
}

struct Chunk<R, D> {
    sequence: u64,
    buffer: Vec<u8>,
    records: u64,
    submitted: u64,
    recycled_records: Vec<Envelope<R, D>>,
    error: Option<String>,
}

enum SerializerOutput<R, D> {
    Batch(Chunk<R, D>),
    Finished,
}

#[derive(Debug)]
struct Encoded {
    digest: String,
    telemetry: StreamTelemetry,
}

fn place_stream(placement: Option<&ThreadPlacement>, role: PlacedThread) -> Result<(), String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ThreadPlacement::place(placement, role)
    }))
    .unwrap_or_else(|_| Err("campaign stream placement panicked".to_owned()))
}

fn add_stream_counter(total: &mut u64, amount: u64, overflow: &mut bool) {
    match total.checked_add(amount) {
        Some(value) => *total = value,
        None => {
            *total = u64::MAX;
            *overflow = true;
        }
    }
}

fn serializer_work_ns(serializers: &[StreamThreadTelemetry; SERIALIZERS]) -> Option<u64> {
    serializers
        .iter()
        .try_fold(0_u64, |total, value| total.checked_add(value.work_ns))
}

fn aggregate_stream_cpu(
    serializers: &[StreamThreadTelemetry; SERIALIZERS],
    relay: &StreamThreadTelemetry,
    read: fn(&StreamThreadTelemetry) -> Option<u64>,
) -> (Option<u64>, bool) {
    let mut total = 0_u64;
    let mut missing = false;
    for value in serializers.iter().chain(std::iter::once(relay)) {
        if let Some(value) = read(value) {
            let Some(next) = total.checked_add(value) else {
                return (None, true);
            };
            total = next;
        } else {
            missing = true;
        }
    }
    ((!missing).then_some(total), false)
}

fn stream_thread_id() -> Option<u32> {
    std::fs::read_link("/proc/thread-self")
        .ok()
        .and_then(|path| path.file_name()?.to_str()?.parse().ok())
}

fn finish_thread_cpu(telemetry: &mut StreamThreadTelemetry, before: Option<(u64, u64)>) {
    if let Some(((before_cpu, before_wait), (after_cpu, after_wait))) =
        before.zip(thread_schedstat())
    {
        telemetry.cpu_ns = after_cpu.checked_sub(before_cpu);
        telemetry.cpu_wait_ns = after_wait.checked_sub(before_wait);
    }
}

fn serialize_batches<R: Serialize, D>(
    pending: Receiver<Command<R, D>>,
    completed: SyncSender<SerializerOutput<R, D>>,
) -> Result<StreamThreadTelemetry, String> {
    let cpu_started = thread_schedstat();
    let mut telemetry = StreamThreadTelemetry {
        tid: stream_thread_id(),
        ..StreamThreadTelemetry::default()
    };
    loop {
        let waiting = now();
        let command = pending.recv();
        add_stream_counter(
            &mut telemetry.input_wait_ns,
            nanos_since(waiting),
            &mut telemetry.counter_overflow,
        );
        let Ok(mut command) = command else {
            let waiting = now();
            completed
                .send(SerializerOutput::Finished)
                .map_err(|_| "campaign stream relay disconnected".to_owned())?;
            add_stream_counter(
                &mut telemetry.output_wait_ns,
                nanos_since(waiting),
                &mut telemetry.counter_overflow,
            );
            break;
        };
        let started = now();
        command.buffer.clear();
        let submitted = command.records.len() as u64;
        let mut records = 0;
        let mut failure = None;
        for record in command.records.drain(..) {
            let offset = command.buffer.len();
            match encode_envelope(record, &mut command.buffer) {
                Ok(()) => {
                    command.buffer.push(b'\n');
                    records += 1;
                }
                Err(error) => {
                    command.buffer.truncate(offset);
                    failure = Some(error);
                    break;
                }
            }
        }
        add_stream_counter(
            &mut telemetry.work_ns,
            nanos_since(started),
            &mut telemetry.counter_overflow,
        );
        add_stream_counter(&mut telemetry.batches, 1, &mut telemetry.counter_overflow);
        add_stream_counter(
            &mut telemetry.records,
            records,
            &mut telemetry.counter_overflow,
        );
        add_stream_counter(
            &mut telemetry.bytes,
            command.buffer.len() as u64,
            &mut telemetry.counter_overflow,
        );
        let failed = failure.is_some();
        let waiting = now();
        completed
            .send(SerializerOutput::Batch(Chunk {
                sequence: command.sequence,
                buffer: command.buffer,
                records,
                submitted,
                recycled_records: command.records,
                error: failure,
            }))
            .map_err(|_| "campaign stream relay disconnected".to_owned())?;
        add_stream_counter(
            &mut telemetry.output_wait_ns,
            nanos_since(waiting),
            &mut telemetry.counter_overflow,
        );
        if failed {
            break;
        }
    }
    finish_thread_cpu(&mut telemetry, cpu_started);
    Ok(telemetry)
}

fn relay_batches<R, D>(
    mut hasher: Sha256,
    inputs: Vec<Receiver<SerializerOutput<R, D>>>,
    output: SyncSender<Chunk<R, D>>,
) -> Result<Encoded, String> {
    let cpu_started = thread_schedstat();
    let mut telemetry = StreamTelemetry::default();
    telemetry.relay.tid = stream_thread_id();
    let mut expected = 0_u64;
    loop {
        let owner = (expected % SERIALIZERS as u64) as usize;
        let waiting = now();
        let received = inputs[owner].recv().map_err(|_| {
            "campaign stream serializer disconnected before its terminal message".to_owned()
        })?;
        add_stream_counter(
            &mut telemetry.relay.input_wait_ns,
            nanos_since(waiting),
            &mut telemetry.relay.counter_overflow,
        );
        let chunk = match received {
            SerializerOutput::Batch(chunk) => chunk,
            SerializerOutput::Finished => {
                for (index, input) in inputs.iter().enumerate() {
                    if index == owner {
                        continue;
                    }
                    match input.recv() {
                        Ok(SerializerOutput::Finished) => {}
                        _ => {
                            return Err(
                                "campaign stream serializer finished before an ordered batch"
                                    .to_owned(),
                            );
                        }
                    }
                }
                for input in &inputs {
                    if input.recv().is_ok() {
                        return Err(
                            "campaign stream serializer sent output after its terminal message"
                                .to_owned(),
                        );
                    }
                }
                break;
            }
        };
        if chunk.sequence != expected {
            return Err("campaign stream batch sequence differs from ordered relay".to_owned());
        }
        expected = expected
            .checked_add(1)
            .ok_or("campaign stream batch sequence overflow")?;
        let started = now();
        hasher.update(&chunk.buffer);
        add_stream_counter(
            &mut telemetry.relay.work_ns,
            nanos_since(started),
            &mut telemetry.relay.counter_overflow,
        );
        add_stream_counter(
            &mut telemetry.background_records,
            chunk.records,
            &mut telemetry.counter_overflow,
        );
        add_stream_counter(
            &mut telemetry.background_batches,
            1,
            &mut telemetry.counter_overflow,
        );
        telemetry.largest_batch_records = telemetry.largest_batch_records.max(chunk.records);
        add_stream_counter(
            &mut telemetry.background_bytes,
            chunk.buffer.len() as u64,
            &mut telemetry.counter_overflow,
        );
        telemetry.largest_buffer_capacity_bytes = telemetry
            .largest_buffer_capacity_bytes
            .max(chunk.buffer.capacity() as u64);
        let failure = chunk.error.clone();
        let waiting = now();
        output
            .send(chunk)
            .map_err(|_| "campaign stream consumer disconnected".to_owned())?;
        add_stream_counter(
            &mut telemetry.relay.output_wait_ns,
            nanos_since(waiting),
            &mut telemetry.relay.counter_overflow,
        );
        if let Some(error) = failure {
            return Err(error);
        }
    }
    let digest = format!("{:x}", hasher.finalize());
    telemetry.relay.batches = telemetry.background_batches;
    telemetry.relay.records = telemetry.background_records;
    telemetry.relay.bytes = telemetry.background_bytes;
    finish_thread_cpu(&mut telemetry.relay, cpu_started);
    Ok(Encoded { digest, telemetry })
}

struct Background<R, D> {
    commands: Vec<SyncSender<Command<R, D>>>,
    output: Option<Receiver<Chunk<R, D>>>,
    workers: Vec<JoinHandle<Result<StreamThreadTelemetry, String>>>,
    relay: Option<JoinHandle<Result<Encoded, String>>>,
    recycled: Vec<Vec<u8>>,
    recycled_records: Vec<Vec<Envelope<R, D>>>,
    pending: u64,
    next_sequence: u64,
    consumed_sequence: u64,
    telemetry: StreamTelemetry,
    #[cfg(test)]
    backpressure: Option<SyncSender<u64>>,
}

impl<R, D> Background<R, D> {
    fn start(hasher: Sha256, placement: Option<ThreadPlacement>) -> Result<Self, Box<dyn Error>>
    where
        R: Serialize + Send + 'static,
        D: Send + 'static,
    {
        let (completed, output) = mpsc::sync_channel(OUTPUT_CAPACITY);
        let (ready, startup) = mpsc::sync_channel(SERIALIZERS + 1);
        let mut background = Self {
            commands: Vec::with_capacity(SERIALIZERS),
            output: Some(output),
            workers: Vec::with_capacity(SERIALIZERS),
            relay: None,
            recycled: Vec::new(),
            recycled_records: Vec::new(),
            pending: 0,
            next_sequence: 0,
            consumed_sequence: 0,
            #[cfg(test)]
            backpressure: None,
            telemetry: StreamTelemetry {
                command_capacity: COMMAND_CAPACITY * SERIALIZERS,
                serializer_command_capacity: COMMAND_CAPACITY,
                serializer_output_capacity: SERIALIZER_OUTPUT_CAPACITY,
                output_capacity: OUTPUT_CAPACITY,
                batch_record_limit: BATCH_RECORD_LIMIT,
                partial_batch_age_ns: PARTIAL_BATCH_AGE_NS,
                ..StreamTelemetry::default()
            },
        };
        let mut results = Vec::with_capacity(SERIALIZERS);
        for index in 0..SERIALIZERS {
            let (commands, pending) = mpsc::sync_channel(COMMAND_CAPACITY);
            let (serialized, result) = mpsc::sync_channel(SERIALIZER_OUTPUT_CAPACITY);
            let ready = ready.clone();
            let placement = placement.clone();
            let worker = thread::Builder::new()
                .name(format!("campaign-serialize-{index}"))
                .spawn(move || {
                    let placed = place_stream(
                        placement.as_ref(),
                        PlacedThread::StreamSerializer(index as u32),
                    );
                    let _ = ready.send(placed.clone());
                    placed?;
                    serialize_batches(pending, serialized)
                })?;
            background.commands.push(commands);
            background.workers.push(worker);
            results.push(result);
        }
        background.relay = Some(
            thread::Builder::new()
                .name("campaign-stream".into())
                .spawn(move || {
                    let placed = place_stream(placement.as_ref(), PlacedThread::StreamEncoder);
                    let _ = ready.send(placed.clone());
                    placed?;
                    relay_batches(hasher, results, completed)
                })?,
        );
        for _ in 0..SERIALIZERS + 1 {
            match startup.recv() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => return Err(error.into()),
                Err(_) => return Err("campaign stream startup disconnected".into()),
            }
        }
        Ok(background)
    }

    fn joined(&mut self) -> Result<Encoded, Box<dyn Error>> {
        let relay = match self
            .relay
            .take()
            .ok_or("campaign stream relay was already joined")?
            .join()
        {
            Ok(value) => value,
            Err(_) => Err("campaign stream relay panicked".to_owned()),
        };
        let mut serializers = Vec::with_capacity(SERIALIZERS);
        let mut worker_failure = None;
        for worker in self.workers.drain(..) {
            match worker.join() {
                Ok(Ok(value)) => serializers.push(value),
                Ok(Err(error)) => {
                    worker_failure.get_or_insert(error);
                }
                Err(_) => {
                    worker_failure.get_or_insert("campaign stream serializer panicked".to_owned());
                }
            }
        }
        let mut encoded = relay.map_err(|error| -> Box<dyn Error> { error.into() })?;
        if let Some(error) = worker_failure {
            return Err(error.into());
        }
        if serializers.len() != SERIALIZERS {
            return Err("campaign stream serializer accounting is incomplete".into());
        }
        encoded.telemetry.serializers = serializers
            .try_into()
            .map_err(|_| "campaign stream serializer accounting differs")?;
        let encode_ns = serializer_work_ns(&encoded.telemetry.serializers);
        encoded.telemetry.encode_ns = encode_ns.unwrap_or(u64::MAX);
        encoded.telemetry.counter_overflow |= encode_ns.is_none()
            || encoded
                .telemetry
                .serializers
                .iter()
                .any(|value| value.counter_overflow)
            || encoded.telemetry.relay.counter_overflow;
        let (cpu, cpu_overflow) = aggregate_stream_cpu(
            &encoded.telemetry.serializers,
            &encoded.telemetry.relay,
            |value| value.cpu_ns,
        );
        let (wait, wait_overflow) = aggregate_stream_cpu(
            &encoded.telemetry.serializers,
            &encoded.telemetry.relay,
            |value| value.cpu_wait_ns,
        );
        encoded.telemetry.worker_cpu_ns = cpu;
        encoded.telemetry.worker_cpu_wait_ns = wait;
        encoded.telemetry.counter_overflow |= cpu_overflow || wait_overflow;
        Ok(encoded)
    }

    fn shutdown(&mut self) {
        self.commands.clear();
        self.output.take();
        if let Some(relay) = self.relay.take() {
            let _ = relay.join();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }

    fn disconnected(&mut self) -> Box<dyn Error> {
        self.commands.clear();
        self.output.take();
        match self.joined() {
            Ok(_) => "campaign stream relay disconnected before finish".into(),
            Err(error) => error,
        }
    }

    fn consume(&mut self, chunk: Chunk<R, D>, sink: &mut dyn Write) -> Result<(), Box<dyn Error>> {
        if chunk.sequence != self.consumed_sequence {
            self.shutdown();
            return Err("campaign stream output sequence differs".into());
        }
        self.consumed_sequence = self
            .consumed_sequence
            .checked_add(1)
            .ok_or("campaign stream consumed sequence overflow")?;
        self.pending = self
            .pending
            .checked_sub(chunk.submitted)
            .ok_or("campaign stream consumed unsubmitted records")?;
        let started = now();
        let written = sink.write_all(&chunk.buffer);
        add_stream_counter(
            &mut self.telemetry.sink_ns,
            nanos_since(started),
            &mut self.telemetry.counter_overflow,
        );
        if let Err(error) = written {
            self.shutdown();
            return Err(error.into());
        }
        add_stream_counter(
            &mut self.telemetry.sink_bytes,
            chunk.buffer.len() as u64,
            &mut self.telemetry.counter_overflow,
        );
        self.recycled.push(chunk.buffer);
        self.recycled_records.push(chunk.recycled_records);
        if let Some(error) = chunk.error {
            self.shutdown();
            return Err(error.into());
        }
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
                Ok(chunk) => self.consume(chunk, sink)?,
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => return Err(self.disconnected()),
            }
        }
    }

    fn drain_failure(&mut self, sink: &mut dyn Write) -> Box<dyn Error> {
        self.commands.clear();
        loop {
            match self.output.as_ref().and_then(|output| output.recv().ok()) {
                Some(chunk) => {
                    if let Err(error) = self.consume(chunk, sink) {
                        return error;
                    }
                }
                None => return self.disconnected(),
            }
        }
    }

    fn enqueue(
        &mut self,
        records: Vec<Envelope<R, D>>,
        sink: &mut dyn Write,
    ) -> Result<(), Box<dyn Error>> {
        let count = records.len() as u64;
        let next = self
            .next_sequence
            .checked_add(1)
            .ok_or("campaign stream batch sequence overflow")?;
        let owner = (self.next_sequence % SERIALIZERS as u64) as usize;
        let mut command = Command {
            sequence: self.next_sequence,
            records,
            buffer: self.recycled.pop().unwrap_or_default(),
        };
        loop {
            let Some(sender) = self.commands.get(owner) else {
                return Err(self.drain_failure(sink));
            };
            match sender.try_send(command) {
                Ok(()) => {
                    self.next_sequence = next;
                    self.pending = self
                        .pending
                        .checked_add(count)
                        .ok_or("campaign stream pending records overflow")?;
                    self.telemetry.records_in_flight_peak =
                        self.telemetry.records_in_flight_peak.max(self.pending);
                    return self.drain(sink);
                }
                Err(TrySendError::Full(returned)) => {
                    command = returned;
                    #[cfg(test)]
                    if let Some(signal) = &self.backpressure {
                        let _ = signal.try_send(self.pending);
                    }
                    add_stream_counter(
                        &mut self.telemetry.producer_backpressure_events,
                        1,
                        &mut self.telemetry.counter_overflow,
                    );
                    let waiting = now();
                    let received = self.output.as_ref().and_then(|output| output.recv().ok());
                    add_stream_counter(
                        &mut self.telemetry.producer_wait_ns,
                        nanos_since(waiting),
                        &mut self.telemetry.counter_overflow,
                    );
                    match received {
                        Some(chunk) => self.consume(chunk, sink)?,
                        None => return Err(self.disconnected()),
                    }
                }
                Err(TrySendError::Disconnected(returned)) => {
                    drop(returned);
                    return Err(self.drain_failure(sink));
                }
            }
        }
    }

    fn finish(mut self, sink: &mut dyn Write) -> Result<Encoded, Box<dyn Error>> {
        self.commands.clear();
        while let Some(chunk) = self.output.as_ref().and_then(|output| output.recv().ok()) {
            self.consume(chunk, sink)?;
        }
        self.output.take();
        let mut encoded = self.joined()?;
        if self.pending != 0 || self.consumed_sequence != self.next_sequence {
            return Err("campaign stream lost an encoded batch".into());
        }
        encoded.telemetry.command_capacity = self.telemetry.command_capacity;
        encoded.telemetry.serializer_command_capacity = COMMAND_CAPACITY;
        encoded.telemetry.serializer_output_capacity = SERIALIZER_OUTPUT_CAPACITY;
        encoded.telemetry.output_capacity = OUTPUT_CAPACITY;
        encoded.telemetry.batch_record_limit = BATCH_RECORD_LIMIT;
        encoded.telemetry.partial_batch_age_ns = PARTIAL_BATCH_AGE_NS;
        encoded.telemetry.records_in_flight_peak = self.telemetry.records_in_flight_peak;
        encoded.telemetry.producer_backpressure_events =
            self.telemetry.producer_backpressure_events;
        encoded.telemetry.producer_wait_ns = self.telemetry.producer_wait_ns;
        encoded.telemetry.sink_ns = self.telemetry.sink_ns;
        encoded.telemetry.sink_bytes = self.telemetry.sink_bytes;
        encoded.telemetry.counter_overflow |= self.telemetry.counter_overflow;
        Ok(encoded)
    }
}

impl<R, D> Drop for Background<R, D> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(super) struct StreamWriter<'a, R: Serialize + Send + 'static, D = ()> {
    sink: &'a mut dyn Write,
    hasher: Option<Sha256>,
    line: Vec<u8>,
    background: Option<Background<R, D>>,
    failed: bool,
    staged: Vec<Envelope<R, D>>,
    staged_since: Option<Moment>,
}

impl<'a, R: Serialize + Send + 'static> StreamWriter<'a, R> {
    #[cfg(test)]
    pub(super) fn new(sink: &'a mut dyn Write) -> Self {
        Self::new_with_cleanup(sink)
    }
}

impl<'a, R: Serialize + Send + 'static, D> StreamWriter<'a, R, D> {
    pub(super) fn new_with_cleanup(sink: &'a mut dyn Write) -> Self {
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
    ) -> Result<(), Box<dyn Error>>
    where
        D: Send + 'static,
    {
        self.require_healthy()?;
        if self.background.is_none() {
            let hasher = self
                .hasher
                .take()
                .ok_or("campaign stream has no hash state")?;
            match Background::start(hasher, placement) {
                Ok(background) => self.background = Some(background),
                Err(error) => {
                    self.failed = true;
                    return Err(error);
                }
            }
            self.staged.reserve_exact(BATCH_RECORD_LIMIT);
        }
        Ok(())
    }

    pub(super) fn write_header<T: Serialize>(&mut self, value: &T) -> Result<(), Box<dyn Error>> {
        self.require_healthy()?;
        if self.background.is_some() {
            return Err("campaign stream header must precede background encoding".into());
        }
        let result = self.write_inline(value);
        self.failed |= result.is_err();
        result
    }

    pub(super) fn write_line(&mut self, value: R) -> Result<(), Box<dyn Error>> {
        self.write_envelope(Envelope {
            record: Some(value),
            cleanup: None,
        })
    }

    pub(super) fn write_line_with_cleanup(
        &mut self,
        value: R,
        cleanup: D,
    ) -> Result<(), Box<dyn Error>> {
        self.write_envelope(Envelope {
            record: Some(value),
            cleanup: Some(cleanup),
        })
    }

    fn write_envelope(&mut self, envelope: Envelope<R, D>) -> Result<(), Box<dyn Error>> {
        self.require_healthy()?;
        if self.background.is_some() {
            if self.staged.is_empty() {
                self.staged_since = Some(now());
            }
            self.staged.push(envelope);
            if self.staged.len() == BATCH_RECORD_LIMIT {
                return self.flush_staged();
            }
            return Ok(());
        }
        self.line.clear();
        if let Err(error) = encode_envelope(envelope, &mut self.line) {
            self.failed = true;
            return Err(error.into());
        }
        self.line.push(b'\n');
        let result = self.sink.write_all(&self.line);
        self.failed |= result.is_err();
        result?;
        self.hasher
            .as_mut()
            .ok_or("campaign stream has no hash state")?
            .update(&self.line);
        Ok(())
    }

    fn require_healthy(&self) -> Result<(), Box<dyn Error>> {
        if self.failed {
            return Err("campaign stream previously failed".into());
        }
        Ok(())
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
        self.require_healthy()?;
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
        self.require_healthy()?;
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

impl<R: Serialize + Send + 'static, D> Drop for StreamWriter<'_, R, D> {
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
                <= ((SERIALIZERS * (COMMAND_CAPACITY + SERIALIZER_OUTPUT_CAPACITY + 1)
                    + OUTPUT_CAPACITY
                    + 1)
                    * BATCH_RECORD_LIMIT) as u64
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
        let mut background = Background::<u64, ()>::start(Sha256::new(), None).unwrap();
        let mut records = Vec::with_capacity(BATCH_RECORD_LIMIT);
        records.extend((0_u64..BATCH_RECORD_LIMIT as u64).map(|record| Envelope {
            record: Some(record),
            cleanup: None,
        }));
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
        recycled.extend(
            (100_u64..100 + BATCH_RECORD_LIMIT as u64).map(|record| Envelope {
                record: Some(record),
                cleanup: None,
            }),
        );
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
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = Arc::clone(&calls);
        let placement = ThreadPlacement::new(move |role| {
            recorded.lock().unwrap().push(role);
            Ok(())
        });
        let mut sink = Vec::new();
        let mut writer = StreamWriter::new(&mut sink);
        writer.write_line(0_u8).unwrap();
        writer.enable_background(Some(placement)).unwrap();
        writer.enable_background(None).unwrap();
        writer.finish().unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        for role in [
            PlacedThread::StreamEncoder,
            PlacedThread::StreamSerializer(0),
            PlacedThread::StreamSerializer(1),
        ] {
            assert!(calls.contains(&role));
            for panic in [false, true] {
                let placement = ThreadPlacement::new(move |current| {
                    if current == role {
                        if panic {
                            panic!("placement panic fixture");
                        }
                        return Err("encoder placement refused".into());
                    }
                    Ok(())
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
                assert!(writer.finish().is_err());
            }
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
        let mut blocked = Some(blocked);
        let mut accepted = 0_u64;
        loop {
            let command = Command {
                sequence: accepted,
                records: vec![Envelope {
                    record: Some(WaitingRecord {
                        value: accepted as usize,
                        wait: blocked.take(),
                    }),
                    cleanup: None,
                }],
                buffer: Vec::new(),
            };
            match background.commands[(accepted % 2) as usize].try_send(command) {
                Ok(()) => {
                    accepted += 1;
                    background.pending += 1;
                    background.next_sequence += 1;
                }
                Err(TrySendError::Full(_)) => break,
                Err(_) => panic!("serializer stopped"),
            }
        }
        assert!(accepted >= COMMAND_CAPACITY as u64);
        assert!(accepted <= 18);
        release.send(()).unwrap();
        writer
            .write_line(WaitingRecord {
                value: accepted as usize,
                wait: None,
            })
            .unwrap();
        let (_, telemetry) = writer.finish().unwrap();
        assert_eq!(telemetry.unwrap().background_records, accepted + 1);
        let actual: Vec<usize> = sink
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(actual, (0..=accepted as usize).collect::<Vec<_>>());
    }
    #[test]
    fn inline_cleanup_can_borrow_non_send_non_static_data() {
        struct BorrowedCleanup<'a>(&'a std::cell::Cell<usize>);
        impl Drop for BorrowedCleanup<'_> {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }
        let count = std::cell::Cell::new(0);
        let mut sink = Vec::new();
        let mut writer = StreamWriter::<u8, BorrowedCleanup<'_>>::new_with_cleanup(&mut sink);
        writer
            .write_line_with_cleanup(7, BorrowedCleanup(&count))
            .unwrap();
        writer.finish().unwrap();
        assert_eq!(count.get(), 1);
        assert_eq!(sink, b"7\n");
    }

    struct Cleanup {
        count: Arc<AtomicUsize>,
        threads: Arc<std::sync::Mutex<Vec<String>>>,
        panic: bool,
        wait: Option<Receiver<()>>,
    }

    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Some(wait) = &self.wait {
                wait.recv().unwrap();
            }
            self.count.fetch_add(1, Ordering::SeqCst);
            self.threads.lock().unwrap().push(
                std::thread::current()
                    .name()
                    .unwrap_or("unnamed")
                    .to_owned(),
            );
            assert!(!self.panic, "cleanup panic fixture");
        }
    }

    fn cleanup(count: &Arc<AtomicUsize>, panic: bool) -> Cleanup {
        Cleanup {
            count: Arc::clone(count),
            threads: Arc::new(std::sync::Mutex::new(Vec::new())),
            panic,
            wait: None,
        }
    }

    #[test]
    fn cleanup_precedes_serialization_and_runs_once_on_the_encoding_thread() {
        struct CheckedRecord {
            value: usize,
            count: Arc<AtomicUsize>,
        }
        impl Serialize for CheckedRecord {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                assert_eq!(self.count.load(Ordering::SeqCst), 1);
                self.value.serialize(serializer)
            }
        }
        let mut reference = None;
        for background in [false, true] {
            let mut counts = Vec::new();
            let threads = Arc::new(std::sync::Mutex::new(Vec::new()));
            let mut sink = Vec::new();
            let mut writer = StreamWriter::<CheckedRecord, Cleanup>::new_with_cleanup(&mut sink);
            writer.write_header(&"header").unwrap();
            if background {
                writer.enable_background(None).unwrap();
            }
            for value in 0..100 {
                let count = Arc::new(AtomicUsize::new(0));
                counts.push(Arc::clone(&count));
                writer
                    .write_line_with_cleanup(
                        CheckedRecord {
                            value,
                            count: Arc::clone(&count),
                        },
                        Cleanup {
                            count: Arc::clone(&count),
                            threads: Arc::clone(&threads),
                            panic: false,
                            wait: None,
                        },
                    )
                    .unwrap();
            }
            let (digest, _) = writer.finish().unwrap();
            assert!(counts.iter().all(|count| count.load(Ordering::SeqCst) == 1));
            let threads = threads.lock().unwrap();
            assert_eq!(threads.len(), 100);
            if background {
                assert!(
                    threads
                        .iter()
                        .all(|name| name.starts_with("campaign-serialize-"))
                );
            } else {
                assert!(
                    threads
                        .iter()
                        .all(|name| !name.starts_with("campaign-serialize-"))
                );
            }
            if let Some((bytes, expected_digest)) = &reference {
                assert_eq!(&sink, bytes);
                assert_eq!(&digest, expected_digest);
            } else {
                reference = Some((sink, digest));
            }
        }
    }

    #[test]
    fn cleanup_failure_preserves_prefix_and_guards_current_queued_and_staged_tails() {
        let count = Arc::new(AtomicUsize::new(0));
        let mut sink = Vec::new();
        let mut writer = StreamWriter::<usize, Cleanup>::new_with_cleanup(&mut sink);
        writer.enable_background(None).unwrap();
        let (release, blocked) = mpsc::channel();
        writer
            .write_line_with_cleanup(0, cleanup(&count, false))
            .unwrap();
        let mut disposal = cleanup(&count, true);
        disposal.wait = Some(blocked);
        writer.write_line_with_cleanup(1, disposal).unwrap();
        for value in 2..BATCH_RECORD_LIMIT * 3 + 3 {
            writer
                .write_line_with_cleanup(value, cleanup(&count, true))
                .unwrap();
        }
        release.send(()).unwrap();
        assert!(
            writer
                .finish()
                .unwrap_err()
                .to_string()
                .contains("destruction panicked")
        );
        assert_eq!(count.load(Ordering::SeqCst), BATCH_RECORD_LIMIT * 3 + 3);
        assert_eq!(sink, b"0\n");
    }

    #[test]
    fn cleanup_failure_inline_never_serializes_the_current_record() {
        let count = Arc::new(AtomicUsize::new(0));
        let mut sink = Vec::new();
        let mut writer = StreamWriter::<Record<Panicking>, Cleanup>::new_with_cleanup(&mut sink);
        writer
            .write_line_with_cleanup(Record::Number(1), cleanup(&count, false))
            .unwrap();
        assert!(
            writer
                .write_line_with_cleanup(Record::Value(Panicking), cleanup(&count, true))
                .unwrap_err()
                .to_string()
                .contains("destruction panicked")
        );
        drop(writer);
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert_eq!(sink, b"1\n");
    }

    #[test]
    fn failed_inline_cleanup_rejects_later_records_header_drain_and_digest() {
        let count = Arc::new(AtomicUsize::new(0));
        let mut sink = Vec::new();
        let mut writer = StreamWriter::<u8, Cleanup>::new_with_cleanup(&mut sink);
        writer
            .write_line_with_cleanup(1, cleanup(&count, false))
            .unwrap();
        assert!(
            writer
                .write_line_with_cleanup(2, cleanup(&count, true))
                .unwrap_err()
                .to_string()
                .contains("destruction panicked")
        );
        assert!(
            writer
                .write_line_with_cleanup(3, cleanup(&count, true))
                .is_err()
        );
        assert!(writer.write_line(4).is_err());
        assert!(writer.write_header(&"later header").is_err());
        assert!(writer.drain_ready().is_err());
        assert!(writer.enable_background(None).is_err());
        assert!(writer.finish().is_err());
        assert_eq!(count.load(Ordering::SeqCst), 3);
        assert_eq!(sink, b"1\n");
    }

    struct InvalidAndPanicDrop;
    impl Serialize for InvalidAndPanicDrop {
        fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(S::Error::custom("first serialization failure"))
        }
    }
    impl Drop for InvalidAndPanicDrop {
        fn drop(&mut self) {
            panic!("secondary record destruction failure");
        }
    }

    #[test]
    fn serialization_error_wins_over_record_drop_and_tail_cleanup_failures() {
        for background in [false, true] {
            let count = Arc::new(AtomicUsize::new(0));
            let mut sink = Vec::new();
            let mut writer =
                StreamWriter::<Record<InvalidAndPanicDrop>, Cleanup>::new_with_cleanup(&mut sink);
            if background {
                writer.enable_background(None).unwrap();
            }
            writer
                .write_line_with_cleanup(Record::Number(1), cleanup(&count, false))
                .unwrap();
            let failed = writer.write_line_with_cleanup(
                Record::Value(InvalidAndPanicDrop),
                cleanup(&count, false),
            );
            let error = if let Err(error) = failed {
                drop(writer);
                error
            } else {
                writer
                    .write_line_with_cleanup(
                        Record::Value(InvalidAndPanicDrop),
                        cleanup(&count, true),
                    )
                    .unwrap();
                writer.finish().unwrap_err()
            };
            assert!(error.to_string().contains("first serialization failure"));
            assert_eq!(count.load(Ordering::SeqCst), if background { 3 } else { 2 });
            assert_eq!(sink, b"1\n");
        }
    }

    #[test]
    fn cleanup_panics_and_sink_failure_preserve_primary_unwind_and_release_all_payloads() {
        let count = Arc::new(AtomicUsize::new(0));
        let caught = std::panic::catch_unwind({
            let count = Arc::clone(&count);
            move || {
                let mut sink = PanicFlush;
                let mut writer = StreamWriter::<u8, Cleanup>::new_with_cleanup(&mut sink);
                writer.enable_background(None).unwrap();
                for value in 0..3 {
                    writer
                        .write_line_with_cleanup(value, cleanup(&count, true))
                        .unwrap();
                }
                panic!("primary failure with staged cleanup");
            }
        });
        assert_eq!(
            caught.unwrap_err().downcast_ref::<&str>(),
            Some(&"primary failure with staged cleanup")
        );
        assert_eq!(count.load(Ordering::SeqCst), 3);
        let count = Arc::new(AtomicUsize::new(0));
        let mut sink = BrokenSink { fail_flush: false };
        let mut writer = StreamWriter::<usize, Cleanup>::new_with_cleanup(&mut sink);
        writer.enable_background(None).unwrap();
        for value in 0..3 {
            writer
                .write_line_with_cleanup(value, cleanup(&count, false))
                .unwrap();
        }
        assert!(
            writer
                .finish()
                .unwrap_err()
                .to_string()
                .contains("broken stream sink")
        );
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }
    struct ControlledRecord {
        value: usize,
        wait: Option<Receiver<()>>,
        error: Option<&'static str>,
        dropped: Arc<AtomicUsize>,
        signal_drop: Option<mpsc::Sender<()>>,
    }

    impl Serialize for ControlledRecord {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            if let Some(wait) = &self.wait {
                wait.recv().unwrap();
            }
            if let Some(error) = self.error {
                return Err(S::Error::custom(error));
            }
            self.value.serialize(serializer)
        }
    }

    impl Drop for ControlledRecord {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
            if let Some(signal) = &self.signal_drop {
                let _ = signal.send(());
            }
        }
    }

    #[test]
    fn a_higher_serializer_failure_never_wins_over_an_earlier_logical_failure() {
        for lower_fails in [false, true] {
            let (release, blocked) = mpsc::channel();
            let (higher_seen, higher_done) = mpsc::channel();
            let (finished, completion) = mpsc::channel();
            let dropped = Arc::new(AtomicUsize::new(0));
            let owned = Arc::clone(&dropped);
            let worker = thread::spawn(move || {
                let mut sink = Vec::new();
                let mut writer = StreamWriter::new(&mut sink);
                writer.enable_background(None).unwrap();
                let mut blocked = Some(blocked);
                for value in 0..32 {
                    writer
                        .write_line(ControlledRecord {
                            value,
                            wait: if value == 3 { blocked.take() } else { None },
                            error: if value == 3 && lower_fails {
                                Some("earlier logical failure")
                            } else if value == 18 {
                                Some("higher logical failure")
                            } else {
                                None
                            },
                            dropped: Arc::clone(&owned),
                            signal_drop: (value == 18).then(|| higher_seen.clone()),
                        })
                        .unwrap();
                }
                let error = writer.finish().unwrap_err().to_string();
                finished.send(()).unwrap();
                (sink, error)
            });
            higher_done
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            assert!(matches!(completion.try_recv(), Err(TryRecvError::Empty)));
            release.send(()).unwrap();
            let (sink, error) = worker.join().unwrap();
            assert!(error.contains(if lower_fails {
                "earlier logical failure"
            } else {
                "higher logical failure"
            }));
            let prefix = if lower_fails { 3 } else { 18 };
            assert_eq!(
                sink,
                (0..prefix)
                    .flat_map(|value| format!("{value}\n").into_bytes())
                    .collect::<Vec<_>>()
            );
            assert_eq!(dropped.load(Ordering::SeqCst), 32);
        }
    }

    #[test]
    fn a_disconnected_higher_input_drains_the_blocked_lower_prefix_before_returning_error() {
        let (release, blocked) = mpsc::channel();
        let (tail_dropped, discarded_tail) = mpsc::channel();
        let (finished, completion) = mpsc::channel();
        let dropped = Arc::new(AtomicUsize::new(0));
        let owned = Arc::clone(&dropped);
        let worker = thread::spawn(move || {
            let mut sink = Vec::new();
            let mut writer = StreamWriter::new(&mut sink);
            writer.enable_background(None).unwrap();
            let mut blocked = Some(blocked);
            for value in 0..64 {
                if value == 32 {
                    let deadline = now() + std::time::Duration::from_secs(10);
                    while !writer.background.as_ref().unwrap().workers[1].is_finished() {
                        assert!(now() < deadline);
                        thread::yield_now();
                    }
                }
                let result = writer.write_line(ControlledRecord {
                    value,
                    wait: if value == 0 { blocked.take() } else { None },
                    error: (value == 18).then_some("higher closed serializer"),
                    dropped: Arc::clone(&owned),
                    signal_drop: (value == 63).then(|| tail_dropped.clone()),
                });
                if let Err(error) = result {
                    assert_eq!(value, 63);
                    drop(writer);
                    finished.send(()).unwrap();
                    return (sink, error.to_string());
                }
            }
            panic!("closed serializer accepted a later batch");
        });
        discarded_tail
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        assert!(matches!(completion.try_recv(), Err(TryRecvError::Empty)));
        release.send(()).unwrap();
        let (sink, error) = worker.join().unwrap();
        assert!(error.contains("higher closed serializer"));
        assert_eq!(
            sink,
            (0..18)
                .flat_map(|value| format!("{value}\n").into_bytes())
                .collect::<Vec<_>>()
        );
        assert_eq!(dropped.load(Ordering::SeqCst), 64);
    }

    struct FaultRecord {
        value: usize,
        failure: usize,
        kind: u8,
        dropped: Arc<AtomicUsize>,
    }

    impl Serialize for FaultRecord {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            if self.value == self.failure {
                if self.kind == 0 {
                    return Err(S::Error::custom("position serialization failure"));
                }
                assert_ne!(self.kind, 1, "position serialization panic");
            }
            self.value.serialize(serializer)
        }
    }

    impl Drop for FaultRecord {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
            assert!(
                !(self.value == self.failure && self.kind == 2),
                "position record drop panic"
            );
        }
    }

    #[test]
    fn every_record_position_in_both_serializers_preserves_complete_prefix_and_exact_cleanup() {
        for failure in 0..32 {
            for kind in 0..4 {
                let dropped = Arc::new(AtomicUsize::new(0));
                let cleaned = Arc::new(AtomicUsize::new(0));
                let (release, blocked) = mpsc::channel();
                let mut blocked = Some(blocked);
                let mut sink = Vec::new();
                let mut writer = StreamWriter::<FaultRecord, Cleanup>::new_with_cleanup(&mut sink);
                writer.enable_background(None).unwrap();
                for value in 0..32 {
                    let mut disposal = cleanup(&cleaned, kind == 3 && value == failure);
                    if value == 0 {
                        disposal.wait = blocked.take();
                    }
                    writer
                        .write_line_with_cleanup(
                            FaultRecord {
                                value,
                                failure,
                                kind,
                                dropped: Arc::clone(&dropped),
                            },
                            disposal,
                        )
                        .unwrap();
                }
                release.send(()).unwrap();
                assert!(writer.finish().is_err());
                assert_eq!(
                    sink,
                    (0..failure)
                        .flat_map(|value| format!("{value}\n").into_bytes())
                        .collect::<Vec<_>>(),
                    "position {failure} kind {kind}"
                );
                assert_eq!(dropped.load(Ordering::SeqCst), 32);
                assert_eq!(cleaned.load(Ordering::SeqCst), 32);
            }
        }
    }

    fn plain_chunk(sequence: u64) -> Chunk<u8, ()> {
        Chunk {
            sequence,
            buffer: format!("{sequence}\n").into_bytes(),
            records: 1,
            submitted: 1,
            recycled_records: Vec::new(),
            error: None,
        }
    }

    #[test]
    fn relay_rejects_missing_duplicate_out_of_order_and_post_terminal_batches() {
        for sequence in [0, 2] {
            let (send0, recv0) = mpsc::sync_channel(1);
            let (send1, recv1) = mpsc::sync_channel(1);
            let (send, output) = mpsc::sync_channel(1);
            let relay =
                thread::spawn(move || relay_batches(Sha256::new(), vec![recv0, recv1], send));
            send0
                .send(SerializerOutput::Batch(plain_chunk(0)))
                .unwrap_or_else(|_| panic!("relay stopped"));
            send1
                .send(SerializerOutput::Batch(plain_chunk(sequence)))
                .unwrap_or_else(|_| panic!("relay stopped"));
            assert_eq!(output.recv().unwrap().sequence, 0);
            assert!(relay.join().unwrap().unwrap_err().contains("sequence"));
        }
        let (send0, recv0) = mpsc::sync_channel::<SerializerOutput<u8, ()>>(1);
        let (_send1, recv1) = mpsc::sync_channel(1);
        let (send, _output) = mpsc::sync_channel(1);
        drop(send0);
        assert!(
            relay_batches(Sha256::new(), vec![recv0, recv1], send)
                .unwrap_err()
                .contains("disconnected")
        );
        let (send0, recv0) = mpsc::sync_channel(1);
        let (send1, recv1) = mpsc::sync_channel(1);
        let (send, _output) = mpsc::sync_channel(1);
        let relay = thread::spawn(move || relay_batches(Sha256::new(), vec![recv0, recv1], send));
        send0
            .send(SerializerOutput::Finished)
            .unwrap_or_else(|_| panic!("relay stopped"));
        send1
            .send(SerializerOutput::Finished)
            .unwrap_or_else(|_| panic!("relay stopped"));
        send0
            .send(SerializerOutput::Batch(plain_chunk(0)))
            .unwrap_or_else(|_| panic!("relay stopped"));
        drop(send0);
        drop(send1);
        assert!(
            relay
                .join()
                .unwrap()
                .unwrap_err()
                .contains("after its terminal")
        );
    }

    #[test]
    fn batch_sequence_overflow_rejects_before_submitting_and_releases_the_record() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let mut sink = Vec::new();
        let mut writer = StreamWriter::new(&mut sink);
        writer.enable_background(None).unwrap();
        let background = writer.background.as_mut().unwrap();
        background.next_sequence = u64::MAX;
        let error = background
            .enqueue(
                vec![Envelope {
                    record: Some(Dropped(Arc::clone(&dropped))),
                    cleanup: None,
                }],
                &mut Vec::new(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("sequence overflow"));
        assert_eq!(background.pending, 0);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        drop(writer);
    }
    #[test]
    fn stream_counter_and_serializer_work_overflow_are_reported() {
        let mut total = u64::MAX;
        let mut overflow = false;
        add_stream_counter(&mut total, 1, &mut overflow);
        assert_eq!(total, u64::MAX);
        assert!(overflow);
        let mut values = [
            StreamThreadTelemetry::default(),
            StreamThreadTelemetry::default(),
        ];
        values[0].work_ns = u64::MAX;
        values[1].work_ns = 1;
        assert_eq!(serializer_work_ns(&values), None);
        values[0].cpu_ns = Some(2);
        values[1].cpu_ns = Some(3);
        let mut relay = StreamThreadTelemetry {
            cpu_ns: Some(4),
            ..StreamThreadTelemetry::default()
        };
        assert_eq!(
            aggregate_stream_cpu(&values, &relay, |value| value.cpu_ns),
            (Some(9), false)
        );
        relay.cpu_ns = None;
        assert_eq!(
            aggregate_stream_cpu(&values, &relay, |value| value.cpu_ns),
            (None, false)
        );
        values[0].cpu_ns = Some(u64::MAX);
        assert_eq!(
            aggregate_stream_cpu(&values, &relay, |value| value.cpu_ns),
            (None, true)
        );
    }
    #[test]
    fn typed_header_records_and_end_match_inline_across_empty_partial_and_full_batches() {
        #[derive(Serialize)]
        #[serde(tag = "event", rename_all = "snake_case")]
        enum Typed {
            Data { index: usize, text: String },
            End { count: usize },
        }
        for count in [0, 1, 15, 16, 17, 31, 32, 47, 100] {
            let mut reference = None;
            for background in [false, true] {
                let mut sink = Vec::new();
                let mut writer = StreamWriter::new(&mut sink);
                writer
                    .write_header(&serde_json::json!({"header":"λ\n\"","count":count}))
                    .unwrap();
                if background {
                    writer.enable_background(None).unwrap();
                }
                for index in 0..count {
                    writer
                        .write_line(Typed::Data {
                            index,
                            text: "λ\n\"".to_owned(),
                        })
                        .unwrap();
                    if background && index % 7 == 0 {
                        writer.staged_since = Some(now() - std::time::Duration::from_millis(2));
                        writer.drain_ready().unwrap();
                    }
                }
                writer.write_line(Typed::End { count }).unwrap();
                let (digest, telemetry) = writer.finish().unwrap();
                if let Some(telemetry) = telemetry {
                    assert_eq!(
                        telemetry
                            .serializers
                            .iter()
                            .map(|value| value.records)
                            .sum::<u64>(),
                        count as u64 + 1
                    );
                    assert_eq!(telemetry.relay.records, count as u64 + 1);
                    assert_eq!(
                        telemetry
                            .serializers
                            .iter()
                            .map(|value| value.bytes)
                            .sum::<u64>(),
                        telemetry.background_bytes
                    );
                    assert!(!telemetry.counter_overflow);
                    assert!(
                        telemetry
                            .serializers
                            .iter()
                            .all(|value| !value.counter_overflow)
                    );
                }
                if let Some((bytes, expected)) = &reference {
                    assert_eq!(&sink, bytes);
                    assert_eq!(&digest, expected);
                } else {
                    reference = Some((sink, digest));
                }
            }
        }
    }

    #[test]
    fn a_blocked_first_batch_bounds_producer_storage_while_the_other_serializer_progresses() {
        let (release, blocked) = mpsc::channel();
        let (later_serialized, later_done) = mpsc::channel();
        let (full, backpressure) = mpsc::sync_channel(1);
        let (finished, completion) = mpsc::channel();
        let made = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let made_owned = Arc::clone(&made);
        let dropped_owned = Arc::clone(&dropped);
        let worker = thread::spawn(move || {
            let mut sink = Vec::new();
            let mut writer = StreamWriter::new(&mut sink);
            writer.enable_background(None).unwrap();
            writer.background.as_mut().unwrap().backpressure = Some(full);
            let mut blocked = Some(blocked);
            for value in 0..1024 {
                made_owned.fetch_add(1, Ordering::SeqCst);
                writer
                    .write_line(ControlledRecord {
                        value,
                        wait: if value == 0 { blocked.take() } else { None },
                        error: None,
                        dropped: Arc::clone(&dropped_owned),
                        signal_drop: (value == 16).then(|| later_serialized.clone()),
                    })
                    .unwrap();
            }
            let result = writer.finish().unwrap();
            finished.send(()).unwrap();
            (sink, result)
        });
        later_done
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        let accepted = backpressure
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        assert!(accepted > 0 && accepted <= 288);
        assert_eq!(
            made.load(Ordering::SeqCst),
            accepted as usize + BATCH_RECORD_LIMIT
        );
        assert!(matches!(completion.try_recv(), Err(TryRecvError::Empty)));
        release.send(()).unwrap();
        let (sink, (digest, telemetry)) = worker.join().unwrap();
        assert_eq!(dropped.load(Ordering::SeqCst), 1024);
        assert_eq!(
            sink,
            (0..1024)
                .flat_map(|value| format!("{value}\n").into_bytes())
                .collect::<Vec<_>>()
        );
        assert_eq!(digest, format!("{:x}", Sha256::digest(&sink)));
        let telemetry = telemetry.unwrap();
        assert!(telemetry.records_in_flight_peak <= 288);
        assert!(telemetry.producer_backpressure_events > 0);
        assert_eq!(telemetry.background_records, 1024);
    }

    #[test]
    fn sink_failures_after_full_queues_release_all_owned_records_and_cleanup_without_hanging() {
        for fail_flush in [false, true] {
            let (release, blocked) = mpsc::channel();
            let (full, backpressure) = mpsc::sync_channel(1);
            let made = Arc::new(AtomicUsize::new(0));
            let dropped = Arc::new(AtomicUsize::new(0));
            let cleaned = Arc::new(AtomicUsize::new(0));
            let made_owned = Arc::clone(&made);
            let dropped_owned = Arc::clone(&dropped);
            let cleaned_owned = Arc::clone(&cleaned);
            let worker = thread::spawn(move || {
                let mut sink = BrokenSink { fail_flush };
                let mut writer =
                    StreamWriter::<ControlledRecord, Cleanup>::new_with_cleanup(&mut sink);
                writer.enable_background(None).unwrap();
                writer.background.as_mut().unwrap().backpressure = Some(full);
                let mut blocked = Some(blocked);
                for value in 0..1024 {
                    made_owned.fetch_add(1, Ordering::SeqCst);
                    let result = writer.write_line_with_cleanup(
                        ControlledRecord {
                            value,
                            wait: if value == 0 { blocked.take() } else { None },
                            error: None,
                            dropped: Arc::clone(&dropped_owned),
                            signal_drop: None,
                        },
                        cleanup(&cleaned_owned, false),
                    );
                    if let Err(error) = result {
                        drop(writer);
                        return error.to_string();
                    }
                }
                writer.finish().unwrap_err().to_string()
            });
            let accepted = backpressure
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            assert!(accepted > 0 && accepted <= 288);
            assert_eq!(
                made.load(Ordering::SeqCst),
                accepted as usize + BATCH_RECORD_LIMIT
            );
            release.send(()).unwrap();
            let error = worker.join().unwrap();
            assert!(error.contains(if fail_flush {
                "broken stream flush"
            } else {
                "broken stream sink"
            }));
            assert_eq!(made.load(Ordering::SeqCst), dropped.load(Ordering::SeqCst));
            assert_eq!(made.load(Ordering::SeqCst), cleaned.load(Ordering::SeqCst));
        }
    }
}
