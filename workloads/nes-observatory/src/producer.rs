// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use nes_workload::metroid::{
    archive::MetroidArchiveKey, campaign::MetroidGame, target::MetroidObservations,
};
use searcher::search::{
    campaign::{CampaignJobResult, CampaignObserver},
    rollout::ExecutionDisposition,
};

use crate::{
    contract::{
        BATCH_BYTES, BATCH_EVENTS, Event, MAX_DETAIL_PER_ADMISSION, QUEUE_EVENTS, RunIdentity,
        SPOOL_BYTES,
    },
    store::Store,
};

pub fn random_id() -> std::io::Result<String> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[allow(clippy::disallowed_methods)]
fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| {
            u64::try_from(value.as_millis()).unwrap_or(u64::MAX)
        })
}

fn area_cell(area: u8, x: u8, y: u8) -> usize {
    usize::from(area) * 1024 + usize::from(y) * 32 + usize::from(x)
}

fn valid(observation: &MetroidObservations) -> bool {
    let state = observation.decoded;
    !observation.dead
        && state.in_play()
        && !state.ending
        && state.door == 0
        && state.health > 0
        && state.health < 8000
        && state.map_x < 32
        && state.map_y < 32
}

fn disposition(value: ExecutionDisposition) -> &'static str {
    match value {
        ExecutionDisposition::Runnable => "runnable",
        ExecutionDisposition::Terminal => "terminal",
        ExecutionDisposition::Failed => "failed",
    }
}

fn spool_bytes(directory: &Path) -> u64 {
    fs::read_dir(directory)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .sum()
}

fn write_batch(
    directory: &Path,
    session: &str,
    sequence: u64,
    bytes: &[u8],
) -> std::io::Result<bool> {
    if spool_bytes(directory).saturating_add(bytes.len() as u64) > SPOOL_BYTES {
        return Ok(false);
    }
    let name = format!("b-{session}-{sequence:016}");
    let temporary = directory.join(format!("{name}.tmp"));
    let final_path = directory.join(format!("{name}.jsonl"));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(temporary, final_path)?;
    Ok(true)
}

fn write_loss(directory: &Path, amount: u64) {
    let path = directory.join("loss-count");
    let old = fs::read_to_string(&path)
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(0);
    let _ = fs::write(path, old.saturating_add(amount).to_string());
}

#[allow(clippy::too_many_arguments)]
fn writer_loop(
    receiver: Receiver<Event>,
    directory: PathBuf,
    session: String,
    lost_queue: Arc<AtomicU64>,
    lost_spool: Arc<AtomicU64>,
    next_id: Arc<AtomicU64>,
    identity: RunIdentity,
    started_unix_ms: u64,
) {
    let mut bytes = Vec::with_capacity(BATCH_BYTES);
    let mut rows = 0_usize;
    let mut sequence = 0_u64;
    loop {
        let received = receiver.recv_timeout(Duration::from_millis(500));
        let disconnected = matches!(&received, Err(RecvTimeoutError::Disconnected));
        let timed_out = matches!(&received, Err(RecvTimeoutError::Timeout));
        if let Ok(event) = received
            && let Ok(mut row) = serde_json::to_vec(&event)
        {
            row.push(b'\n');
            if bytes.len().saturating_add(row.len()) > BATCH_BYTES && !bytes.is_empty() {
                sequence = sequence.saturating_add(1);
                match write_batch(&directory, &session, sequence, &bytes) {
                    Ok(true) => {}
                    _ => {
                        lost_spool.fetch_add(rows as u64, Ordering::Relaxed);
                        write_loss(&directory, rows as u64);
                    }
                }
                bytes.clear();
                rows = 0;
            }
            if row.len() <= BATCH_BYTES {
                bytes.extend_from_slice(&row);
                rows += 1;
            } else {
                lost_spool.fetch_add(1, Ordering::Relaxed);
                write_loss(&directory, 1);
            }
        }
        if rows > 0 && (rows >= BATCH_EVENTS || timed_out || disconnected) {
            let lost = lost_queue.swap(0, Ordering::Relaxed);
            if lost > 0 {
                let mut marker = Event::new("loss");
                marker.run_id = identity.run_id.clone();
                marker.session_id = identity.session_id.clone();
                marker.started_unix_ms = started_unix_ms;
                marker.event_id = next_id.fetch_add(1, Ordering::Relaxed);
                marker.amount = u32::try_from(lost).unwrap_or(u32::MAX);
                marker.payload = "producer_queue_overflow".to_owned();
                if let Ok(mut row) = serde_json::to_vec(&marker) {
                    row.push(b'\n');
                    if bytes.len().saturating_add(row.len()) <= BATCH_BYTES {
                        bytes.extend_from_slice(&row);
                        rows += 1;
                    } else {
                        lost_queue.fetch_add(lost, Ordering::Relaxed);
                    }
                }
            }
            sequence = sequence.saturating_add(1);
            match write_batch(&directory, &session, sequence, &bytes) {
                Ok(true) => {}
                _ => {
                    lost_spool.fetch_add(rows as u64, Ordering::Relaxed);
                    write_loss(&directory, rows as u64);
                }
            }
            bytes.clear();
            rows = 0;
        }
        if disconnected {
            break;
        }
    }
}

#[allow(clippy::disallowed_methods)]
fn upload_loop(directory: PathBuf, store: Store, stop: Arc<AtomicBool>) {
    let mut stopping = None::<Instant>;
    loop {
        let mut files = fs::read_dir(&directory)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "jsonl")
            })
            .collect::<Vec<_>>();
        files.sort();
        let mut uploaded = false;
        for path in files {
            let Some(token) = path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            if store.insert(&bytes, token).is_ok() {
                let _ = fs::remove_file(&path);
                uploaded = true;
            } else {
                break;
            }
        }
        if stop.load(Ordering::Relaxed) {
            let since = stopping.get_or_insert_with(Instant::now);
            if directory_empty(&directory) || since.elapsed() >= Duration::from_secs(5) {
                break;
            }
        }
        if !uploaded {
            thread::sleep(Duration::from_millis(500));
        }
    }
}

fn directory_empty(directory: &Path) -> bool {
    fs::read_dir(directory)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .all(|entry| {
            entry
                .path()
                .extension()
                .is_none_or(|extension| extension != "jsonl")
        })
}

pub fn export_spool(directory: &Path, store: &Store) -> crate::store::Result<usize> {
    let mut files = fs::read_dir(directory)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .collect::<Vec<_>>();
    files.sort();
    let mut count = 0;
    for path in files {
        let token = path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or("invalid batch name")?;
        store.insert(&fs::read(&path)?, token)?;
        fs::remove_file(path)?;
        count += 1;
    }
    Ok(count)
}

pub struct Producer {
    identity: RunIdentity,
    started_unix_ms: u64,
    next_id: Arc<AtomicU64>,
    sender: Option<SyncSender<Event>>,
    lost_queue: Arc<AtomicU64>,
    lost_spool: Arc<AtomicU64>,
    seen: Box<[u8; 32768]>,
    pending: BTreeMap<u64, u64>,
    last_watermark: u64,
    last_checkpoint_ms: u64,
    started_instant: Instant,
    writer: Option<JoinHandle<()>>,
    uploader: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

impl Producer {
    #[allow(clippy::disallowed_methods)]
    pub fn start(
        identity: RunIdentity,
        spool_root: &Path,
        store: Store,
    ) -> crate::store::Result<Self> {
        let directory = spool_root.join(&identity.run_id).join(&identity.session_id);
        fs::create_dir_all(&directory)?;
        fs::write(
            directory.join("identity.json"),
            serde_json::to_vec_pretty(&identity)?,
        )?;
        let (sender, receiver) = mpsc::sync_channel(QUEUE_EVENTS);
        let next_id = Arc::new(AtomicU64::new(1));
        let lost_queue = Arc::new(AtomicU64::new(0));
        let lost_spool = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let started_unix_ms = unix_millis();
        let writer = {
            let directory = directory.clone();
            let session = identity.session_id.clone();
            let next_id = Arc::clone(&next_id);
            let lost_queue = Arc::clone(&lost_queue);
            let lost_spool = Arc::clone(&lost_spool);
            let identity = identity.clone();
            thread::spawn(move || {
                writer_loop(
                    receiver,
                    directory,
                    session,
                    lost_queue,
                    lost_spool,
                    next_id,
                    identity,
                    started_unix_ms,
                );
            })
        };
        let uploader = {
            let stop = Arc::clone(&stop);
            thread::spawn(move || upload_loop(directory, store, stop))
        };
        let mut producer = Self {
            identity,
            started_unix_ms,
            next_id,
            sender: Some(sender),
            lost_queue,
            lost_spool,
            seen: Box::new([0_u8; 32768]),
            pending: BTreeMap::new(),
            last_watermark: 0,
            last_checkpoint_ms: 0,
            started_instant: Instant::now(),
            writer: Some(writer),
            uploader: Some(uploader),
            stop,
        };
        let mut start = Event::new("run_start");
        start.payload = serde_json::to_string(&producer.identity)?;
        producer.emit(start);
        Ok(producer)
    }

    fn emit(&mut self, mut event: Event) {
        event.run_id = self.identity.run_id.clone();
        event.session_id = self.identity.session_id.clone();
        event.started_unix_ms = self.started_unix_ms;
        event.event_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if let Some(sender) = &self.sender {
            match sender.try_send(event) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                    self.lost_queue.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    fn checkpoint(&mut self, at_ms: u64) {
        let mut checkpoint = Event::new("territory_checkpoint");
        checkpoint.event_ms = at_ms;
        checkpoint.amount = 0;
        let mut payload = String::with_capacity(1280);
        for area in 16_usize..=20 {
            for byte in &self.seen[area * 128..(area + 1) * 128] {
                payload.push_str(&format!("{byte:02x}"));
            }
        }
        checkpoint.payload = payload;
        self.emit(checkpoint);
        self.last_checkpoint_ms = at_ms;
    }

    #[allow(clippy::disallowed_methods)]
    pub fn finish(mut self, status: &str) -> (u64, u64) {
        let mut end = Event::new("run_end");
        end.event_ms =
            u64::try_from(self.started_instant.elapsed().as_millis()).unwrap_or(u64::MAX);
        end.payload = status.to_owned();
        self.emit(end);
        self.sender.take();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(uploader) = self.uploader.take() {
            let _ = uploader.join();
        }
        (
            self.lost_queue.load(Ordering::Relaxed),
            self.lost_spool.load(Ordering::Relaxed),
        )
    }
}

impl CampaignObserver<MetroidGame> for Producer {
    fn bootstrap(&mut self, key: MetroidArchiveKey) {
        if key.map_x >= 32 || key.map_y >= 32 {
            return;
        }
        let index = area_cell(key.area, key.map_x, key.map_y);
        self.seen[index / 8] |= 1_u8 << (index % 8);
        for kind in ["discovery", "presence"] {
            let mut event = Event::new(kind);
            event.area = key.area;
            event.map_x = key.map_x;
            event.map_y = key.map_y;
            event.x = key.x;
            event.y = key.y;
            self.emit(event);
        }
        self.checkpoint(0);
    }

    fn selection(
        &mut self,
        selection_id: u64,
        reservation: Option<u64>,
        parent_id: u64,
        key: MetroidArchiveKey,
        elapsed_millis: u64,
    ) {
        let mut event = Event::new(if reservation.is_some() {
            "selection"
        } else {
            "skip"
        });
        event.event_ms = elapsed_millis;
        event.selection_ms = elapsed_millis;
        event.selection_id = selection_id;
        event.reservation = reservation.unwrap_or(u64::MAX);
        event.parent_id = parent_id;
        event.area = key.area;
        event.map_x = key.map_x;
        event.map_y = key.map_y;
        event.x = key.x;
        event.y = key.y;
        event.health = key.health;
        event.missiles = key.missiles;
        self.emit(event);
        if let Some(reservation) = reservation {
            self.pending.insert(reservation, elapsed_millis);
        }
    }

    fn admission(
        &mut self,
        reservation: u64,
        sequence: u64,
        parent_id: u64,
        key: MetroidArchiveKey,
        selection_millis: u64,
        admission_millis: u64,
        execution_work: u64,
        result: &CampaignJobResult<MetroidGame>,
    ) {
        let mut work = Event::new("work");
        work.event_ms = admission_millis;
        work.selection_ms = selection_millis;
        work.reservation = reservation;
        work.admission_sequence = sequence;
        work.parent_id = parent_id;
        work.area = key.area;
        work.map_x = key.map_x;
        work.map_y = key.map_y;
        work.execution_work = execution_work;
        work.outcome = if result.preparation_failure.is_some() {
            "failed"
        } else {
            "completed"
        }
        .to_owned();
        self.emit(work);
        self.pending.remove(&reservation);
        let finalized = self
            .pending
            .values()
            .min()
            .map_or(admission_millis, |earliest| earliest.saturating_sub(1));
        if finalized > self.last_watermark
            && (finalized.saturating_sub(self.last_watermark) >= 500 || self.pending.is_empty())
        {
            let mut watermark = Event::new("watermark");
            watermark.event_ms = admission_millis;
            watermark.selection_ms = finalized;
            watermark.amount = 0;
            self.emit(watermark);
            self.last_watermark = finalized;
        }

        let mut cells = BTreeMap::<(u8, u8, u8), u32>::new();
        let mut eligible = 0_usize;
        for action in &result.actions {
            for observation in &action.observations {
                if !valid(observation) {
                    continue;
                }
                eligible += 1;
                let state = observation.decoded;
                let cell = (state.area, state.map_x, state.map_y);
                let count = cells.entry(cell).or_default();
                *count = count.saturating_add(1);
                let index = area_cell(state.area, state.map_x, state.map_y);
                let byte = index / 8;
                let mask = 1_u8 << (index % 8);
                if self.seen[byte] & mask == 0 {
                    self.seen[byte] |= mask;
                    let mut discovery = Event::new("discovery");
                    discovery.event_ms = admission_millis;
                    discovery.selection_ms = selection_millis;
                    discovery.reservation = reservation;
                    discovery.admission_sequence = sequence;
                    discovery.parent_id = parent_id;
                    discovery.area = state.area;
                    discovery.map_x = state.map_x;
                    discovery.map_y = state.map_y;
                    discovery.x = state.x;
                    discovery.y = state.y;
                    self.emit(discovery);
                }
            }
        }
        if admission_millis.saturating_sub(self.last_checkpoint_ms) >= 60_000 {
            self.checkpoint(admission_millis);
        }
        for ((area, map_x, map_y), amount) in cells {
            let mut presence = Event::new("presence");
            presence.event_ms = admission_millis;
            presence.selection_ms = selection_millis;
            presence.reservation = reservation;
            presence.admission_sequence = sequence;
            presence.parent_id = parent_id;
            presence.area = area;
            presence.map_x = map_x;
            presence.map_y = map_y;
            presence.amount = amount;
            self.emit(presence);
        }
        let stride = eligible.div_ceil(MAX_DETAIL_PER_ADMISSION).max(1);
        let mut ordinal = 0_usize;
        for (action_index, action) in result.actions.iter().enumerate() {
            for observation in &action.observations {
                if !valid(observation) {
                    continue;
                }
                let sample =
                    ordinal.is_multiple_of(stride) && ordinal / stride < MAX_DETAIL_PER_ADMISSION;
                ordinal += 1;
                if !sample {
                    continue;
                }
                let state = observation.decoded;
                let mut detail = Event::new("observation");
                detail.event_ms = admission_millis;
                detail.selection_ms = selection_millis;
                detail.reservation = reservation;
                detail.admission_sequence = sequence;
                detail.parent_id = parent_id;
                detail.area = state.area;
                detail.map_x = state.map_x;
                detail.map_y = state.map_y;
                detail.x = state.x;
                detail.y = state.y;
                detail.health = state.health;
                detail.missiles = state.missiles;
                detail.equipment = state.equipment;
                detail.action_index = u32::try_from(action_index).unwrap_or(u32::MAX);
                detail.frame_count = observation.frame_count;
                detail.outcome = disposition(action.outcome.disposition).to_owned();
                detail.sampled = u8::from(eligible > MAX_DETAIL_PER_ADMISSION);
                self.emit(detail);
            }
        }
    }
}
