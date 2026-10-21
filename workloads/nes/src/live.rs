// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::BTreeSet,
    error::Error,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use machine::{
    MachineError,
    nes::{ButtonChord, core_state},
    quicknes::QuickNesMachine,
};
use nes_games::{
    arcade::{
        ArcadeGame,
        feed::{self, Batch, Feed, Note, Point, State, Stats, raw_state, run_noted},
        snapshot_digest,
        trail::Traced,
    },
    nes_backend::native_identity_sha256,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    eval::{Request, Result, evaluate_watched},
    search::{
        campaign::{
            CampaignCoordinator, CampaignView, CampaignWatch, JobOutcome, JobSpec, WatchedJob,
        },
        draw::draw_mixture_identifier,
    },
    witness::EndpointIdentity,
};

const TICK: Duration = Duration::from_millis(100);
const HEARTBEAT: Duration = Duration::from_secs(15);
const LOOKUP_WAIT: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REQUEST_BYTES: u64 = 65_536;
const MAX_CONNECTIONS: usize = 64;
const TRAILS_PER_TICK: usize = 256;
const FINISHES_PER_TICK: usize = 32;
const POINTS_PER_TICK: usize = 2_000;
const GLANCE_STATES: usize = 1_000;
const GLANCE_NEWEST: usize = 250;
const STATES_PER_LOOKUP: usize = 12;
const CLIENT_QUEUE: usize = 64;

pub type NativeMachine = Traced<QuickNesMachine>;

pub fn native_game<G: ArcadeGame<Machine = NativeMachine>>(
    rom: &[u8],
    core_path: &Path,
    core_sha256: &str,
    execution_sha256: &str,
) -> G {
    let (machine_rom, path, sha256, execution) = (
        rom.to_vec(),
        core_path.to_path_buf(),
        core_sha256.to_owned(),
        execution_sha256.to_owned(),
    );
    G::arcade(
        rom,
        &machine::quicknes::core_identity(),
        &native_identity_sha256(core_sha256, execution_sha256),
        Arc::new(move || {
            Ok(Traced::new(
                QuickNesMachine::from_rom_bytes_with_identity(
                    &machine_rom,
                    &path,
                    &sha256,
                    &execution,
                )?,
                G::probe(),
            ))
        }),
    )
}

pub type MemoryView<V> = fn(&[u8]) -> std::result::Result<V, Box<dyn Error>>;

type ReadView<V> = Box<dyn FnMut(&[u8]) -> Option<V>>;
type Observe<V> = Arc<dyn Fn() -> Option<ReadView<V>> + Send + Sync>;

fn observe_packed<R: AsRef<[u8]>, V>(
    packed: &[(u64, Vec<u8>)],
    create: &Observe<V>,
    mut decode: impl FnMut(&[u8]) -> Option<R>,
) -> Vec<Option<V>> {
    if packed.is_empty() {
        return Vec::new();
    }
    let Some(mut observe) = create() else {
        return packed.iter().map(|_| None).collect();
    };
    packed
        .iter()
        .map(|(_, bytes)| {
            let raw = decode(bytes)?;
            observe(raw.as_ref())
        })
        .collect()
}

struct Shared<V> {
    feed: Option<Feed<V>>,
    stats: Stats,
}

type SharedFeed<V> = Arc<Mutex<Shared<V>>>;

fn lock<V>(shared: &Mutex<Shared<V>>) -> MutexGuard<'_, Shared<V>> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Found<V> {
    id: u64,
    observation: Option<V>,
    digest: Option<u64>,
    frames_at: Option<u64>,
    input: Vec<ButtonChord>,
    packed: Option<Vec<u8>>,
}

struct Lookup<V> {
    ids: Vec<u64>,
    reply: Sender<Vec<Found<V>>>,
}

enum Event {
    Ready(Arc<str>),
    Viewer(SyncSender<Arc<str>>),
    Glance(Vec<(u64, Vec<u8>)>),
    Finished,
}

pub struct LiveWatch<G: ArcadeGame> {
    workload: Arc<G>,
    shared: SharedFeed<G::View>,
    events: Sender<Event>,
    lookups: Mutex<Option<Receiver<Lookup<G::View>>>>,
    pending_lookups: Arc<AtomicUsize>,
    observe: Observe<G::View>,
    game: String,
    viewers: Arc<AtomicUsize>,
    glance: Arc<AtomicBool>,
    recording: AtomicBool,
    details: Arc<AtomicUsize>,
    cleared: AtomicUsize,
}

fn native_observer<V: 'static>(
    rom: &[u8],
    request: &Request,
    execution_sha256: &str,
    bytes: usize,
    view: MemoryView<V>,
) -> Observe<V> {
    let (rom, path, sha256, execution) = (
        rom.to_vec(),
        request.core.clone(),
        request.core_sha256.clone(),
        execution_sha256.to_owned(),
    );
    let read = move |machine: &mut QuickNesMachine,
                     raw: &[u8]|
          -> std::result::Result<Vec<u8>, MachineError> {
        machine.restore_bytes(raw)?;
        let mut memory = machine.read_wram()?.to_vec();
        if bytes > memory.len() {
            memory.extend(machine.read_save_ram()?);
        }
        Ok(memory)
    };
    Arc::new(move || {
        let mut machine =
            QuickNesMachine::from_rom_bytes_with_identity(&rom, &path, &sha256, &execution).ok()?;
        Some(Box::new(move |raw: &[u8]| view(&read(&mut machine, raw).ok()?).ok()) as ReadView<V>)
    })
}

fn glance<G: ArcadeGame>(campaign: &impl CampaignView<G>) -> Vec<(u64, Vec<u8>)> {
    let archive = campaign.archive();
    let entries = archive.live_entry_count();
    let indices = glance_indices(entries, &mut Sampler(campaign.executions() | 1));
    indices
        .into_iter()
        .filter_map(|index| {
            Some((
                archive.stable_id(index)?,
                G::emulator_state(archive.entry_snapshot(index)?).to_vec(),
            ))
        })
        .collect()
}

fn glance_indices(entries: usize, sampler: &mut Sampler) -> Vec<usize> {
    let newest = entries.saturating_sub(GLANCE_NEWEST);
    let mut indices = sampler.pick(newest, GLANCE_STATES - GLANCE_NEWEST);
    indices.extend(newest..entries);
    indices
}

fn live_state<G: ArcadeGame>(
    workload: &G,
    id: u64,
    view: G::View,
    input: &[ButtonChord],
    frames: Option<u64>,
    genesis: &mut Option<usize>,
) -> Result<State<G::View>> {
    feed::archived_state(id, view, &[], input, frames, |input| {
        let count = match *genesis {
            Some(count) => count,
            None => {
                let count = feed::genesis_actions(workload)?;
                *genesis = Some(count);
                count
            }
        };
        feed::played(workload, None, count, input)
    })
}

fn resident_root<A, K, M, S>(
    archive: &crate::search::archive::Archive<A, K, M, S>,
) -> Result<(u64, &S)>
where
    A: Clone + std::fmt::Debug + Eq + Ord + Serialize + serde::de::DeserializeOwned,
    K: crate::search::archive::ArchiveKey,
    M: Clone + Copy + std::fmt::Debug + Eq + Serialize + serde::de::DeserializeOwned,
    S: Clone,
{
    let resident = |index| Some((archive.stable_id(index)?, archive.entry_snapshot(index)?));
    archive
        .index_of_id(0)
        .and_then(resident)
        .or_else(|| (0..archive.live_entry_count()).find_map(resident))
        .ok_or_else(|| "the live view found no archived state to start from".into())
}

impl<G: ArcadeGame> LiveWatch<G> {
    fn root(&self, campaign: &impl CampaignView<G>) -> Result<()> {
        let archive = campaign.archive();
        let (id, snapshot) = resident_root(archive)?;
        let frames = G::snapshot_frame_count(snapshot);
        let raw = raw_state(G::emulator_state(snapshot))?;
        let view = (self.observe)()
            .and_then(|mut observe| observe(&raw))
            .ok_or("the live view could not read the first archived state")?;
        let snapshot = core_state(&raw);
        let digest = snapshot_digest(snapshot);
        let index = archive
            .index_of_id(id)
            .ok_or("the live root metadata disappeared")?;
        let input = archive.entry_input(index)?.actions;
        let root = live_state(
            self.workload.as_ref(),
            id,
            view,
            &input,
            Some(frames),
            &mut None,
        )?;
        {
            let mut shared = lock(&self.shared);
            shared.feed = Some(Feed::new(id, view, frames, digest));
            shared.stats = feed::stats(campaign);
        }
        let ready = json!({
            "type": "ready",
            "game": self.game,
            "live": true,
            "active": 0,
            "paused": false,
            "searches": [{"id": 0, "origin": null, "branch": null, "parent": null}],
            "state": state_json(&root, digest, Some(snapshot)),
        });
        let _ = self.events.send(Event::Ready(Arc::from(ready.to_string())));
        Ok(())
    }

    fn close_lookups(&self) {
        let lookups = self
            .lookups
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(lookups) = lookups {
            for lookup in lookups.try_iter() {
                self.pending_lookups.fetch_sub(1, Ordering::Relaxed);
                let _ = lookup.reply.send(Vec::new());
            }
        }
    }

    fn serve(&self, campaign: &impl CampaignView<G>) {
        if self.pending_lookups.load(Ordering::Relaxed) == 0 {
            return;
        }
        let Ok(lookups) = self.lookups.try_lock() else {
            return;
        };
        let Some(lookups) = lookups.as_ref() else {
            return;
        };
        for lookup in lookups.try_iter().take(1) {
            self.pending_lookups.fetch_sub(1, Ordering::Relaxed);
            let archive = campaign.archive();
            let shared = lock(&self.shared);
            let found = lookup
                .ids
                .iter()
                .filter_map(|&id| {
                    let feed = shared.feed.as_ref();
                    let index = archive.index_of_id(id)?;
                    Some(Found {
                        id,
                        observation: feed.and_then(|feed| feed.view(id)),
                        digest: feed.and_then(|feed| feed.digest(id)),
                        frames_at: feed
                            .and_then(|feed| feed.frames_at(id))
                            .or_else(|| archive.entry_snapshot(index).map(G::snapshot_frame_count)),
                        input: archive.entry_input(index).ok()?.actions,
                        packed: archive
                            .entry_snapshot(index)
                            .map(|snapshot| G::emulator_state(snapshot).to_vec()),
                    })
                })
                .collect();
            drop(shared);
            let _ = lookup.reply.send(found);
        }
    }
}

impl<G: ArcadeGame + Send> CampaignWatch<G> for LiveWatch<G>
where
    G::ArchiveReport: Serialize,
{
    type Note = Note<G::View>;

    fn run(
        &self,
        job: &JobSpec<G>,
        run: impl FnOnce() -> std::result::Result<JobOutcome<G>, Box<dyn Error>>,
    ) -> WatchedJob<G, Self::Note> {
        let watched = self.viewers.load(Ordering::Relaxed) > 0;
        if watched && take_detail(&self.details) {
            let (outcome, note) = run_noted(job, run)?;
            take_progress(&self.cleared, note.next_level);
            return Ok((outcome, note));
        }
        let outcome = run()?;
        let cleared = if watched {
            outcome
                .result
                .actions
                .iter()
                .flat_map(|action| action.observations.last())
                .map(G::cleared)
                .max()
                .unwrap_or(0)
        } else {
            0
        };
        let progress = watched && take_progress(&self.cleared, cleared);
        let note = Note {
            reservation: outcome.reservation,
            steps: if progress {
                feed::steps::<G>(&outcome.result)?
            } else {
                Vec::new()
            },
            trail: None,
            next_level: cleared,
            execution_work: outcome.execution_work,
            death: outcome.result.actions.last().is_some_and(|action| {
                action.outcome.disposition == crate::search::rollout::ExecutionDisposition::Terminal
                    && !action.outcome.objective_reached
            }),
        };
        Ok((outcome, note))
    }

    fn prepare_admission(&self, note: &Self::Note, campaign: &mut CampaignCoordinator<'_, G>) {
        let watched = self.viewers.load(Ordering::Relaxed) > 0;
        if watched != self.recording.load(Ordering::Relaxed) {
            self.recording.store(watched, Ordering::Relaxed);
            if watched {
                campaign.watch_selected_admissions();
            } else {
                campaign.unwatch_admissions();
                if let Some(feed) = lock(&self.shared).feed.as_mut() {
                    feed.forget_pending();
                }
            }
        }
        if watched && note.needs_admission() {
            campaign.request_admission(note.reservation);
        }
    }

    fn admitted(&self, note: Self::Note, campaign: &mut CampaignCoordinator<'_, G>) {
        let watched = self.recording.load(Ordering::Relaxed);
        if watched {
            let admissions = campaign.take_admissions();
            {
                let mut shared = lock(&self.shared);
                if let Some(feed) = shared.feed.as_mut() {
                    feed.note(note);
                    feed.admit(admissions);
                    feed.forget_dropped(campaign);
                }
                shared.stats = feed::stats(campaign);
            }
            if self.glance.swap(false, Ordering::AcqRel) {
                let _ = self.events.send(Event::Glance(glance(campaign)));
            }
        } else if campaign.finished() {
            lock(&self.shared).stats = feed::stats(campaign);
        }
        self.serve(campaign);
    }

    fn start(&self, campaign: &mut CampaignCoordinator<'_, G>) -> Result<()> {
        self.root(campaign)
    }
}

fn with_fields(mut value: Value, fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    if let Value::Object(object) = &mut value {
        for (key, field) in fields {
            object.insert(key.to_owned(), field);
        }
    }
    value
}

fn state_json<V: Serialize>(state: &State<V>, digest: u64, snapshot: Option<&[u8]>) -> Value {
    with_fields(
        serde_json::to_value(state).unwrap_or_default(),
        [
            ("digest", json!(format!("{digest:016x}"))),
            ("snapshot", json!(snapshot.map(encode_snapshot))),
            ("branch", Value::Null),
            ("boot_level", json!(0)),
        ],
    )
}

fn encode_snapshot(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |word, (index, &byte)| {
                word | u32::from(byte) << (16 - 8 * index)
            });
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(char::from(
                    ALPHABET[(word >> (18 - 6 * index) & 63) as usize],
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

struct Sampler(u64);

impl Sampler {
    fn below(&mut self, bound: usize) -> usize {
        Self::below_with(bound, || {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        })
    }

    fn below_with(bound: usize, mut next: impl FnMut() -> u64) -> usize {
        let bound = bound as u64;
        let limit = u64::MAX - u64::MAX % bound;
        loop {
            let value = next();
            if value != 0 && value - 1 < limit {
                return ((value - 1) % bound) as usize;
            }
        }
    }

    fn pick(&mut self, len: usize, keep: usize) -> Vec<usize> {
        if len <= keep {
            return (0..len).collect();
        }
        Self::pick_with(len, keep, |bound| self.below(bound))
    }

    fn pick_with(len: usize, keep: usize, mut below: impl FnMut(usize) -> usize) -> Vec<usize> {
        let mut chosen = BTreeSet::new();
        for index in len.saturating_sub(keep)..len {
            if !chosen.insert(below(index + 1)) {
                chosen.insert(index);
            }
        }
        chosen.into_iter().collect()
    }
}

struct Terminal {
    ready: Option<Arc<str>>,
    batch: Option<Arc<str>>,
    limit: Arc<str>,
}

impl Terminal {
    fn write(&self, stream: &mut impl Write, include_ready: bool) -> std::io::Result<()> {
        if include_ready && let Some(ready) = &self.ready {
            write_event(stream, ready)?;
        }
        if let Some(batch) = &self.batch {
            write_event(stream, batch)?;
        }
        write_event(stream, &self.limit)
    }
}

#[derive(Default)]
struct Streams {
    active: Mutex<usize>,
    changed: Condvar,
}

impl Streams {
    fn enter(&self) -> StreamGuard<'_> {
        *self.active.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        StreamGuard(self)
    }

    fn wait(&self, timeout: Duration) -> bool {
        let active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        let (active, _) = self
            .changed
            .wait_timeout_while(active, timeout, |active| *active > 0)
            .unwrap_or_else(PoisonError::into_inner);
        *active == 0
    }
}

struct StreamGuard<'a>(&'a Streams);

impl Drop for StreamGuard<'_> {
    fn drop(&mut self) {
        *self.0.active.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
        self.0.changed.notify_all();
    }
}

fn write_event(stream: &mut impl Write, message: &str) -> std::io::Result<()> {
    stream.write_all(b"data: ")?;
    stream.write_all(message.as_bytes())?;
    stream.write_all(b"\n\n")
}

fn event_header(stream: &mut impl Write) -> std::io::Result<()> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\n",
    )
}

fn write_events(
    stream: &mut impl Write,
    receiver: &Receiver<Arc<str>>,
    terminal: &OnceLock<Terminal>,
) -> std::io::Result<()> {
    let mut started = false;
    loop {
        if let Some(terminal) = terminal.get() {
            return terminal.write(stream, !started);
        }
        let received = receiver.recv_timeout(HEARTBEAT);
        if let Some(terminal) = terminal.get() {
            return terminal.write(stream, !started);
        }
        match received {
            Ok(message) => {
                write_event(stream, &message)?;
                started = true;
            }
            Err(RecvTimeoutError::Timeout) => stream.write_all(b": heartbeat\n\n")?,
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

struct Viewers {
    senders: Vec<SyncSender<Arc<str>>>,
    waiting: Vec<SyncSender<Arc<str>>>,
    count: Arc<AtomicUsize>,
}

fn delivered(sender: &SyncSender<Arc<str>>, message: &Arc<str>) -> bool {
    !matches!(
        sender.try_send(Arc::clone(message)),
        Err(TrySendError::Disconnected(_))
    )
}

impl Viewers {
    fn broadcast(&mut self, message: &Arc<str>) {
        self.senders.retain(|sender| delivered(sender, message));
        self.counted();
    }

    fn counted(&self) {
        self.count
            .store(self.senders.len() + self.waiting.len(), Ordering::Relaxed);
    }
}

fn batch_json<V: Serialize>(batch: &Batch<V>, workers: u32) -> Arc<str> {
    let message = with_fields(
        serde_json::to_value(batch).unwrap_or_default(),
        [
            ("type", json!("batch")),
            ("active", json!(0)),
            ("workers", json!(workers)),
            ("wasm_bytes", json!(0)),
        ],
    );
    Arc::from(message.to_string())
}

#[allow(clippy::disallowed_methods)]
fn wall_now() -> Instant {
    Instant::now()
}

fn glance_json<V: Serialize>(
    stats: Stats,
    packed: &[(u64, Vec<u8>)],
    observe: &Observe<V>,
    workers: u32,
) -> Arc<str> {
    let points = packed
        .iter()
        .zip(observe_packed(packed, observe, |bytes| {
            raw_state(bytes).ok()
        }))
        .filter_map(|((id, _), observation)| {
            Some(Point {
                observation: observation?,
                retained: Some(*id),
                frame: 0,
                priority: false,
            })
        })
        .collect();
    batch_json(
        &Batch {
            stats,
            deaths: 0,
            sampled_jobs: 0,
            frames: 0,
            points,
            motion: Vec::new(),
            finishes: Vec::new(),
            exits: Vec::new(),
        },
        workers,
    )
}

fn sampled<V: Copy>(batch: Batch<V>, sampler: &mut Sampler) -> Batch<V> {
    let (mut points, open): (Vec<_>, Vec<_>) = batch
        .points
        .iter()
        .copied()
        .partition(|point| point.retained.is_some() || point.priority);
    points.extend(
        sampler
            .pick(open.len(), POINTS_PER_TICK)
            .into_iter()
            .map(|index| open[index]),
    );
    let finished: Vec<usize> = sampler
        .pick(batch.finishes.len(), FINISHES_PER_TICK)
        .into_iter()
        .map(|index| batch.finishes[index])
        .collect();
    let others: Vec<usize> = (0..batch.motion.len())
        .filter(|index| !batch.finishes.contains(index))
        .collect();
    let mut kept: Vec<usize> = sampler
        .pick(others.len(), TRAILS_PER_TICK.saturating_sub(finished.len()))
        .into_iter()
        .map(|index| others[index])
        .collect();
    kept.extend(&finished);
    kept.sort_unstable();
    let motion = kept
        .iter()
        .map(|&index| batch.motion[index].clone())
        .collect();
    let mut finishes = Vec::new();
    let mut exits = Vec::new();
    for (position, index) in batch.finishes.iter().enumerate() {
        if let Ok(slot) = kept.binary_search(index)
            && finished.contains(index)
        {
            finishes.push(slot);
            exits.push(batch.exits[position]);
        }
    }
    Batch {
        points,
        motion,
        finishes,
        exits,
        ..batch
    }
}

fn take_batch<V: Copy>(shared: &Mutex<Shared<V>>) -> Option<Batch<V>> {
    let mut shared = lock(shared);
    let stats = shared.stats;
    shared.feed.as_mut().map(|feed| feed.take_batch(stats))
}

fn take_progress(cleared: &AtomicUsize, next: u8) -> bool {
    let next = usize::from(next);
    next > cleared.load(Ordering::Relaxed) && next > cleared.fetch_max(next, Ordering::Relaxed)
}

fn take_detail(details: &AtomicUsize) -> bool {
    details
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
            remaining.checked_sub(1)
        })
        .is_ok()
}

struct Feeding<'a, V> {
    events: &'a Receiver<Event>,
    shared: &'a Mutex<Shared<V>>,
    observe: &'a Observe<V>,
    glance: &'a AtomicBool,
    details: &'a AtomicUsize,
    workers: u32,
    terminal: &'a OnceLock<Terminal>,
}

fn feed_loop<V: Serialize + Copy>(feeding: &Feeding<'_, V>, mut viewers: Viewers) {
    let Feeding {
        events,
        shared,
        observe,
        glance,
        details,
        workers,
        terminal,
    } = *feeding;
    let mut ready: Option<Arc<str>> = None;
    let mut sampler = Sampler(0x9e37_79b9_7f4a_7c15);
    let mut next_tick = wall_now() + TICK;
    loop {
        let wait = next_tick.saturating_duration_since(wall_now());
        match events.recv_timeout(wait) {
            Ok(Event::Ready(message)) => {
                viewers.waiting.retain(|sender| delivered(sender, &message));
                if !viewers.waiting.is_empty() {
                    glance.store(true, Ordering::Release);
                }
                viewers.counted();
                ready = Some(message);
            }
            Ok(Event::Viewer(sender)) => {
                if let Some(ready) = &ready
                    && delivered(&sender, ready)
                {
                    glance.store(true, Ordering::Release);
                }
                viewers.waiting.push(sender);
                viewers.counted();
            }
            Ok(Event::Glance(packed)) => {
                if !viewers.waiting.is_empty() {
                    let stats = lock(shared).stats;
                    let message = glance_json(stats, &packed, observe, workers);
                    for sender in std::mem::take(&mut viewers.waiting) {
                        if delivered(&sender, &message) {
                            viewers.senders.push(sender);
                        }
                    }
                    viewers.counted();
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Ok(Event::Finished) | Err(RecvTimeoutError::Disconnected) => {
                let mut shared = lock(shared);
                shared.stats.stopped = true;
                let stats = shared.stats;
                let batch = shared
                    .feed
                    .as_mut()
                    .map(|feed| batch_json(&feed.take_terminal_batch(stats), workers));
                drop(shared);
                let _ = terminal.set(Terminal {
                    ready,
                    batch,
                    limit: Arc::from(
                        json!({"type": "limit", "won": stats.won, "finished": true}).to_string(),
                    ),
                });
                viewers.count.store(0, Ordering::Relaxed);
                return;
            }
        }
        if wall_now() < next_tick {
            continue;
        }
        next_tick = wall_now() + TICK;
        let Some(batch) = take_batch(shared) else {
            continue;
        };
        details.store(TRAILS_PER_TICK, Ordering::Relaxed);
        if !viewers.senders.is_empty() {
            viewers.broadcast(&batch_json(&sampled(batch, &mut sampler), workers));
        }
    }
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript",
        Some("css") => "text/css",
        Some("json") => "application/json",
        Some("wasm") => "application/wasm",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn respond(stream: &mut TcpStream, status: &str, kind: &str, body: &[u8]) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

fn query<'a>(target: &'a str, key: &str) -> Option<&'a str> {
    target
        .split_once('?')?
        .1
        .split('&')
        .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
}

fn site_path(target: &str) -> Option<PathBuf> {
    let path = target.split('?').next()?.trim_start_matches('/');
    let relative = Path::new(if path.is_empty() { "index.html" } else { path });
    relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
        .then(|| relative.to_path_buf())
}

fn site_file(site: &Path, target: &str) -> Option<PathBuf> {
    let file = site.join(site_path(target)?);
    file.is_file().then_some(file)
}

struct Server<G: ArcadeGame> {
    events: Sender<Event>,
    lookups: Sender<Lookup<G::View>>,
    pending_lookups: Arc<AtomicUsize>,
    site: Option<PathBuf>,
    workload: Arc<G>,
    observe: Observe<G::View>,
    terminal: Arc<OnceLock<Terminal>>,
    streams: Arc<Streams>,
}

impl<G: ArcadeGame> Server<G> {
    fn lookup(&self, ids: Vec<u64>) -> Option<Vec<Found<G::View>>> {
        let (reply, answer) = mpsc::channel();
        self.pending_lookups.fetch_add(1, Ordering::Relaxed);
        if self.lookups.send(Lookup { ids, reply }).is_err() {
            self.pending_lookups.fetch_sub(1, Ordering::Relaxed);
            return self.terminal.get().map(|_| Vec::new());
        }
        match answer.recv_timeout(LOOKUP_WAIT) {
            Ok(found) => Some(found),
            Err(_) => self.terminal.get().map(|_| Vec::new()),
        }
    }

    fn states(&self, target: &str) -> Option<String> {
        let request: i64 = query(target, "request")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let ids: Vec<u64> = query(target, "ids")
            .unwrap_or_default()
            .split(',')
            .take(STATES_PER_LOOKUP)
            .filter_map(|id| id.parse().ok())
            .collect();
        let mut found = if self.terminal.get().is_some() {
            Vec::new()
        } else {
            self.lookup(ids.clone())?
        };
        let raw: Vec<_> = found
            .iter()
            .map(|found| {
                found
                    .packed
                    .as_deref()
                    .and_then(|packed| raw_state(packed).ok())
            })
            .collect();
        let unseen: Vec<usize> = (0..found.len())
            .filter(|&index| found[index].observation.is_none() && raw[index].is_some())
            .collect();
        if !unseen.is_empty()
            && let Some(mut observe) = (self.observe)()
        {
            for index in unseen {
                found[index].observation = raw[index].as_deref().and_then(&mut observe);
            }
        }
        let mut genesis = None;
        let mut states = Vec::new();
        let mut answered = Vec::new();
        for (found, raw) in found.into_iter().zip(raw) {
            let snapshot = raw.as_deref().map(core_state);
            let (Some(observation), Some(digest)) = (
                found.observation,
                found.digest.or(snapshot.map(snapshot_digest)),
            ) else {
                continue;
            };
            let state = live_state(
                self.workload.as_ref(),
                found.id,
                observation,
                &found.input,
                found.frames_at,
                &mut genesis,
            );
            if let Ok(state) = state {
                states.push(state_json(&state, digest, snapshot));
                answered.push(found.id);
            }
        }
        let missing: Vec<u64> = ids
            .into_iter()
            .filter(|id| !answered.contains(id))
            .collect();
        Some(
            json!({"type": "states", "request": request, "states": states, "missing": missing})
                .to_string(),
        )
    }

    fn handle(&self, mut stream: TcpStream) -> std::io::Result<()> {
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        let mut reader = BufReader::new(stream.try_clone()?.take(MAX_REQUEST_BYTES));
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let mut parts = line.split_whitespace();
        let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
            return Ok(());
        };
        let target = target.to_owned();
        let (mut host, mut origin) = (None, None);
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header)? == 0 || header == "\r\n" || header == "\n" {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                let value = Some(value.trim().to_owned());
                if name.eq_ignore_ascii_case("host") {
                    host = value;
                } else if name.eq_ignore_ascii_case("origin") {
                    origin = value;
                }
            }
        }
        if !local_request(host.as_deref(), origin.as_deref()) {
            return respond(&mut stream, "403 Forbidden", "text/plain", b"forbidden");
        }
        if method != "GET" {
            return respond(
                &mut stream,
                "405 Method Not Allowed",
                "text/plain",
                b"GET only",
            );
        }
        if target.starts_with("/live/events") {
            let _active = self.streams.enter();
            if let Some(terminal) = self.terminal.get() {
                event_header(&mut stream)?;
                return terminal.write(&mut stream, true);
            }
            let (sender, receiver) = mpsc::sync_channel(CLIENT_QUEUE);
            if self.events.send(Event::Viewer(sender)).is_err() {
                if let Some(terminal) = self.terminal.get() {
                    event_header(&mut stream)?;
                    return terminal.write(&mut stream, true);
                }
                return respond(
                    &mut stream,
                    "410 Gone",
                    "text/plain",
                    b"the campaign has ended",
                );
            }
            event_header(&mut stream)?;
            return write_events(&mut stream, &receiver, &self.terminal);
        }
        if target.starts_with("/live/states") {
            let _active = self.streams.enter();
            return match self.states(&target) {
                Some(body) => respond(&mut stream, "200 OK", "application/json", body.as_bytes()),
                None => respond(
                    &mut stream,
                    "503 Service Unavailable",
                    "text/plain",
                    b"no state lookup",
                ),
            };
        }
        match self
            .site
            .as_deref()
            .and_then(|site| site_file(site, &target))
        {
            Some(file) => respond(
                &mut stream,
                "200 OK",
                content_type(&file),
                &fs::read(&file)?,
            ),
            None => respond(&mut stream, "404 Not Found", "text/plain", b"not found"),
        }
    }
}

fn loopback_authority(authority: &str) -> bool {
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => authority,
    };
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

fn local_request(host: Option<&str>, origin: Option<&str>) -> bool {
    host.is_some_and(loopback_authority)
        && origin.is_none_or(|origin| {
            origin
                .strip_prefix("http://")
                .is_some_and(loopback_authority)
        })
}

fn process_start() -> Option<u64> {
    let stat = fs::read_to_string("/proc/self/stat").ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

fn listen<G: ArcadeGame + Send>(listener: &TcpListener, server: &Arc<Server<G>>) {
    let open = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming().flatten() {
        if open.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            open.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        let (server, open) = (Arc::clone(server), Arc::clone(&open));
        thread::spawn(move || {
            let _ = server.handle(stream);
            open.fetch_sub(1, Ordering::AcqRel);
        });
    }
}

pub fn evaluate_live<G>(
    rom: &[u8],
    request: &Request,
    out: &Path,
    started: Instant,
    id: &str,
) -> Result<()>
where
    G: ArcadeGame<Machine = NativeMachine> + Send,
    G::ArchiveReport: Serialize + serde::de::DeserializeOwned,
    G::Milestones: 'static,
    G::Observations: 'static,
    G::Snapshot: EndpointIdentity + 'static,
{
    evaluate_live_with::<G>(
        rom,
        request,
        out,
        id,
        |game, run, effective, execution, watch| {
            evaluate_watched(game, run, effective, execution, out, started, watch)
        },
    )
}

fn evaluate_live_with<G>(
    rom: &[u8],
    request: &Request,
    out: &Path,
    id: &str,
    evaluate: impl FnOnce(&G, G::Run, &Request, &str, &LiveWatch<G>) -> Result<()>,
) -> Result<()>
where
    G: ArcadeGame<Machine = NativeMachine> + Send,
    G::ArchiveReport: Serialize + serde::de::DeserializeOwned,
    G::Milestones: 'static,
    G::Observations: 'static,
    G::Snapshot: EndpointIdentity + 'static,
{
    let address = request
        .live
        .as_deref()
        .ok_or("live evaluation needs a listen address")?;
    let request = &Request {
        mixture: draw_mixture_identifier(G::mixture()),
        ..request.clone()
    };
    let execution_sha256 =
        machine::quicknes::prepared_execution_sha256(&request.core, &request.core_sha256)?;
    let watched = Watched {
        rom,
        request,
        execution_sha256: &execution_sha256,
        out,
        id,
        address,
        view: G::memory_view,
    };
    watched.evaluate(
        native_game::<G>(rom, &request.core, &request.core_sha256, &execution_sha256),
        G::run(),
        |game, run, watch| evaluate(game, run, request, &execution_sha256, watch),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn evaluate_watching<G>(
    game: G,
    run: G::Run,
    rom: &[u8],
    request: &Request,
    execution_sha256: &str,
    out: &Path,
    started: Instant,
    id: &str,
    view: MemoryView<G::View>,
) -> Result<()>
where
    G: ArcadeGame + Send,
    G::ArchiveReport: Serialize + serde::de::DeserializeOwned,
    G::Milestones: 'static,
    G::Observations: 'static,
    G::Snapshot: EndpointIdentity + 'static,
{
    let watched = Watched {
        rom,
        request,
        execution_sha256,
        out,
        id,
        address: "127.0.0.1:0",
        view,
    };
    watched.evaluate(game, run, |game, run, watch| {
        evaluate_watched(game, run, request, execution_sha256, out, started, watch)
    })
}

struct Watched<'a, V> {
    rom: &'a [u8],
    request: &'a Request,
    execution_sha256: &'a str,
    out: &'a Path,
    id: &'a str,
    address: &'a str,
    view: MemoryView<V>,
}

impl<V: Serialize + Copy + Send + 'static> Watched<'_, V> {
    fn evaluate<G>(
        &self,
        game: G,
        run: G::Run,
        evaluate: impl FnOnce(&G, G::Run, &LiveWatch<G>) -> Result<()>,
    ) -> Result<()>
    where
        G: ArcadeGame<View = V> + Send,
        G::ArchiveReport: Serialize + serde::de::DeserializeOwned,
        G::Milestones: 'static,
        G::Observations: 'static,
        G::Snapshot: EndpointIdentity + 'static,
    {
        let game = Arc::new(game);
        let listener = TcpListener::bind(self.address)?;
        let local = listener.local_addr()?;
        if !local.ip().is_loopback() {
            return Err(format!(
                "the live view listens only on a loopback address, and {local} is not one; forward the port over SSH to watch from another machine"
            )
            .into());
        }
        eprintln!("live view listening on {local}");
        let record = self.out.join("live.json");
        fs::write(
            &record,
            json!({
                "address": local.to_string(),
                "game": self.id,
                "pid": std::process::id(),
                "process_start": process_start(),
            })
            .to_string(),
        )?;
        let observe = native_observer(
            self.rom,
            self.request,
            self.execution_sha256,
            G::MEMORY_BYTES,
            self.view,
        );
        let shared: SharedFeed<V> = Arc::new(Mutex::new(Shared {
            feed: None,
            stats: Stats::default(),
        }));
        let (events, received) = mpsc::channel();
        let (lookups, pending) = mpsc::channel();
        let pending_lookups = Arc::new(AtomicUsize::new(0));
        let viewers = Arc::new(AtomicUsize::new(0));
        let glance = Arc::new(AtomicBool::new(false));
        let details = Arc::new(AtomicUsize::new(TRAILS_PER_TICK));
        let watch = LiveWatch::<G> {
            workload: Arc::clone(&game),
            shared: Arc::clone(&shared),
            events: events.clone(),
            lookups: Mutex::new(Some(pending)),
            pending_lookups: Arc::clone(&pending_lookups),
            observe: Arc::clone(&observe),
            game: self.id.to_owned(),
            viewers: Arc::clone(&viewers),
            glance: Arc::clone(&glance),
            recording: AtomicBool::new(false),
            details: Arc::clone(&details),
            cleared: AtomicUsize::new(0),
        };
        let workers = self.request.workers;
        let terminal = Arc::new(OnceLock::new());
        let streams = Arc::new(Streams::default());
        let feed = {
            let terminal = Arc::clone(&terminal);
            let observe = Arc::clone(&observe);
            thread::spawn(move || {
                feed_loop(
                    &Feeding {
                        events: &received,
                        shared: &shared,
                        observe: &observe,
                        glance: &glance,
                        details: &details,
                        workers,
                        terminal: &terminal,
                    },
                    Viewers {
                        senders: Vec::new(),
                        waiting: Vec::new(),
                        count: viewers,
                    },
                );
            })
        };
        let server = Arc::new(Server {
            events,
            lookups,
            pending_lookups,
            site: self.request.live_site.clone(),
            workload: Arc::clone(&game),
            observe,
            terminal,
            streams: Arc::clone(&streams),
        });
        thread::spawn(move || listen(&listener, &server));
        let result = evaluate(game.as_ref(), run, &watch);
        let _ = watch.events.send(Event::Finished);
        let _ = feed.join();
        watch.close_lookups();
        streams.wait(IO_TIMEOUT);
        let _ = fs::remove_file(record);
        result
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        Batch, Point, Sampler, Stats, encode_snapshot, glance_indices, local_request, sampled,
        site_path, take_detail, take_progress,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn glance_decodes_one_state_at_a_time_with_one_observer() {
        use std::sync::Arc;

        struct Decoded {
            bytes: Vec<u8>,
            live: Arc<AtomicUsize>,
        }
        impl AsRef<[u8]> for Decoded {
            fn as_ref(&self) -> &[u8] {
                &self.bytes
            }
        }
        impl Drop for Decoded {
            fn drop(&mut self) {
                assert_eq!(self.live.fetch_sub(1, Ordering::SeqCst), 1);
            }
        }

        let live = Arc::new(AtomicUsize::new(0));
        let factories = Arc::new(AtomicUsize::new(0));
        let factory_count = Arc::clone(&factories);
        let observed_live = Arc::clone(&live);
        let observe: super::Observe<u8> = Arc::new(move || {
            assert_eq!(factory_count.fetch_add(1, Ordering::SeqCst), 0);
            let live = Arc::clone(&observed_live);
            Some(Box::new(move |raw| {
                assert_eq!(live.load(Ordering::SeqCst), 1);
                raw.first().copied()
            }))
        });
        let packed = [
            (41, lz4_flex::block::compress_prepend_size(&[11])),
            (42, vec![1]),
            (43, lz4_flex::block::compress_prepend_size(&[22])),
        ];
        let mut decodes = 0;
        let result = super::observe_packed(&packed, &observe, |bytes| {
            assert_eq!(live.load(Ordering::SeqCst), 0);
            decodes += 1;
            let bytes = super::raw_state(bytes).ok()?;
            assert_eq!(live.fetch_add(1, Ordering::SeqCst), 0);
            Some(Decoded {
                bytes,
                live: Arc::clone(&live),
            })
        });
        assert_eq!(result, [Some(11), None, Some(22)]);
        assert_eq!(decodes, 3);
        assert_eq!(factories.load(Ordering::SeqCst), 1);
        assert_eq!(live.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn glance_keeps_original_ids_and_order_when_decode_or_observation_fails() {
        use std::sync::Arc;

        let observe: super::Observe<u8> = Arc::new(|| {
            Some(Box::new(|raw| {
                raw.first().copied().filter(|value| *value != 33)
            }))
        });
        let packed = [
            (91, lz4_flex::block::compress_prepend_size(&[11])),
            (12, vec![1]),
            (83, lz4_flex::block::compress_prepend_size(&[33])),
            (44, lz4_flex::block::compress_prepend_size(&[22])),
        ];
        let json: serde_json::Value = serde_json::from_str(&super::glance_json(
            Stats::default(),
            &packed,
            &observe,
            128,
        ))
        .unwrap();
        assert_eq!(json["workers"], 128);
        assert_eq!(json["type"], "batch");
        assert_eq!(
            json["points"],
            serde_json::json!([
                {"observation": 11, "retained": 91, "frame": 0},
                {"observation": 22, "retained": 44, "frame": 0}
            ])
        );
        assert!(json["motion"].as_array().unwrap().is_empty());
        let unavailable: super::Observe<u8> = Arc::new(|| None);
        assert_eq!(
            super::observe_packed(&packed, &unavailable, |_| -> Option<Vec<u8>> {
                panic!("an unavailable observer must not decode snapshots")
            }),
            [None, None, None, None]
        );
        let unused: super::Observe<u8> =
            Arc::new(|| panic!("an empty glance must not construct an observer"));
        assert!(
            super::observe_packed(&[], &unused, |_| -> Option<Vec<u8>> {
                panic!("an empty glance must not decode snapshots")
            })
            .is_empty()
        );
    }

    #[derive(
        Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
    )]
    struct ResidentKey(u8);

    impl crate::search::archive::ArchiveKey for ResidentKey {
        type Place = u8;
        type Progress = u8;
        type Identity = ();
        type Lineage = ();

        fn place(self) -> Self::Place {
            self.0
        }
        fn progress(self) -> Self::Progress {
            self.0
        }
        fn identity(self) -> Self::Identity {}
        fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
            self
        }
        fn record(_lineage: &mut Self::Lineage, _key: Self) {}
    }

    #[test]
    fn live_nonzero_ready_and_lookup_share_the_exact_configured_workload() {
        use nes_games::{
            arcade::ArcadeGame,
            famidash::{
                campaign::FamidashGame,
                levels::{Side, Stage},
            },
        };
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        type Game = FamidashGame<super::NativeMachine>;
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        let configured = Arc::new(Game::with_targets(
            b"test-rom",
            "test-core",
            "test-sha",
            Stage {
                side: Side::Main,
                level: 7,
                goal: 9,
            },
            Box::new(move || {
                observed.fetch_add(1, Ordering::SeqCst);
                Err("configured level7 factory".into())
            }),
        ));
        let view = Game::memory_view(&[0_u8; machine::nes::WRAM_SIZE]).unwrap();
        let input = [machine::nes::ButtonChord::new(1, 4)];
        let ready =
            super::live_state(configured.as_ref(), 7, view, &input, Some(4), &mut None).unwrap();
        let feed = nes_games::arcade::feed::Feed::new(7, view, ready.frames, 81);
        let lookup = super::live_state(
            configured.as_ref(),
            7,
            feed.view(7).unwrap(),
            &input,
            feed.frames_at(7),
            &mut None,
        )
        .unwrap();
        let ready_json = super::state_json(&ready, 81, None);
        assert_eq!(
            ready_json,
            super::state_json(&lookup, feed.digest(7).unwrap(), None)
        );
        assert_eq!(ready_json["id"], 7);
        assert_eq!(ready_json["frames"], 4);
        assert_eq!(ready_json["actions"][0]["frames"], 4);
        assert!(feed.view(0).is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let error = super::live_state(configured.as_ref(), 7, view, &input, Some(11), &mut None)
            .err()
            .unwrap();
        assert!(error.to_string().contains("configured level7 factory"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn live_resident_root_skips_evicted_genesis_metadata_and_rejects_missing_snapshots() {
        use crate::search::archive::{Archive, ArchiveCandidate};
        let mut archive = Archive::<u8, ResidentKey, (), u8>::new(|_| 1);
        archive.max_entries = 1;
        let first = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: vec![0],
                    key: ResidentKey(0),
                    milestones: (),
                },
                11,
            )
            .unwrap()
            .unwrap();
        assert_eq!(archive.stable_id(first), Some(0));
        assert_eq!(super::resident_root(&archive).unwrap(), (0, &11));
        let second = archive
            .insert(
                None,
                1,
                ArchiveCandidate {
                    suffix: vec![1],
                    key: ResidentKey(1),
                    milestones: (),
                },
                22,
            )
            .unwrap()
            .unwrap();
        assert_eq!(archive.index_of_id(0), Some(first));
        assert!(archive.entry_snapshot(first).is_none());
        assert_eq!(archive.entry_drops(), 1);
        assert_eq!(
            super::resident_root(&archive).unwrap(),
            (archive.stable_id(second).unwrap(), &22)
        );
        let bytes = postcard::to_allocvec(&archive).unwrap();
        let absent: Archive<u8, ResidentKey, (), u8> = postcard::from_bytes(&bytes).unwrap();
        assert!(absent.live_entry_count() > 0);
        assert!((0..absent.live_entry_count()).all(|index| absent.entry_snapshot(index).is_none()));
        assert_eq!(
            super::resident_root(&absent).unwrap_err().to_string(),
            "the live view found no archived state to start from"
        );
    }

    fn terminal_fixture() -> super::Terminal {
        super::Terminal {
            ready: Some(std::sync::Arc::from(r#"{"type":"ready"}"#)),
            batch: Some(std::sync::Arc::from(r#"{"type":"batch","points":[43]}"#)),
            limit: std::sync::Arc::from(r#"{"type":"limit","won":true,"finished":true}"#),
        }
    }

    #[test]
    fn terminal_bypasses_a_full_ordinary_queue_and_sends_points_before_completion() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(super::CLIENT_QUEUE);
        for _ in 0..super::CLIENT_QUEUE {
            sender.send(std::sync::Arc::from("stale")).unwrap();
        }
        assert!(matches!(
            sender.try_send(std::sync::Arc::from("overflow")),
            Err(std::sync::mpsc::TrySendError::Full(_))
        ));
        let terminal = std::sync::OnceLock::new();
        assert!(terminal.set(terminal_fixture()).is_ok());
        let mut output = Vec::new();
        super::write_events(&mut output, &receiver, &terminal).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(!text.contains("stale"));
        assert!(text.find("ready").unwrap() < text.find("batch").unwrap());
        assert!(text.find("batch").unwrap() < text.find("limit").unwrap());
    }

    #[test]
    fn a_waiting_viewer_gets_previously_drained_progress_at_finish() {
        use nes_games::arcade::feed::{Feed, Note, Step};
        use std::sync::{Arc, Mutex, OnceLock, atomic::AtomicBool, mpsc};
        let mut feed = Feed::new(0, 0_u8, 0, 0);
        feed.note(Note {
            reservation: 1,
            steps: vec![Step {
                view: 43,
                frame: 77,
                digest: None,
                objective: true,
                cleared: 43,
            }],
            trail: None,
            next_level: 43,
            execution_work: 77,
            death: false,
        });
        feed.admit(vec![crate::search::campaign::CampaignAdmission {
            reservation: 1,
            decisions: vec![crate::search::campaign::CampaignAdmissionDecision::Objective],
        }]);
        assert_eq!(feed.take_batch(Stats::default()).points.len(), 1);
        let shared = Mutex::new(super::Shared {
            feed: Some(feed),
            stats: Stats {
                executions: 99,
                stopped: false,
                won: true,
                ..Stats::default()
            },
        });
        let (events, received) = mpsc::channel();
        let (viewer, receiver) = mpsc::sync_channel(1);
        viewer.send(Arc::from("stale")).unwrap();
        events
            .send(super::Event::Ready(Arc::from(r#"{"type":"ready"}"#)))
            .unwrap();
        events.send(super::Event::Viewer(viewer)).unwrap();
        events.send(super::Event::Finished).unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let terminal = OnceLock::new();
        let observe: super::Observe<u8> = Arc::new(|| None);
        super::feed_loop(
            &super::Feeding {
                events: &received,
                shared: &shared,
                observe: &observe,
                glance: &AtomicBool::new(false),
                details: &AtomicUsize::new(0),
                workers: 128,
                terminal: &terminal,
            },
            super::Viewers {
                senders: Vec::new(),
                waiting: Vec::new(),
                count: Arc::clone(&count),
            },
        );
        assert_eq!(count.load(Ordering::Relaxed), 0);
        let snapshot = terminal.get().unwrap();
        let batch: serde_json::Value =
            serde_json::from_str(snapshot.batch.as_ref().unwrap()).unwrap();
        assert_eq!(batch["executions"], 99);
        assert_eq!(batch["frames"], 77);
        assert!(batch["stopped"].as_bool().unwrap());
        assert!(batch["won"].as_bool().unwrap());
        assert!(
            batch["points"]
                .as_array()
                .unwrap()
                .iter()
                .all(|point| point["observation"] == 43)
        );
        let mut output = Vec::new();
        super::write_events(&mut output, &receiver, &terminal).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("43") && text.contains("limit"));
        assert!(!text.contains("stale"));
    }

    #[test]
    fn late_event_requests_replay_terminal_after_the_feed_receiver_exits() {
        use std::{
            io::{Read, Write},
            net::{TcpListener, TcpStream},
            sync::{Arc, OnceLock, mpsc},
        };
        type Game = nes_games::famidash::campaign::FamidashGame<super::NativeMachine>;
        let (events, received) = mpsc::channel();
        drop(received);
        let (lookups, _) = mpsc::channel();
        let terminal = Arc::new(OnceLock::new());
        assert!(terminal.set(terminal_fixture()).is_ok());
        let server = super::Server::<Game> {
            events,
            lookups,
            pending_lookups: Arc::new(AtomicUsize::new(0)),
            site: None,
            workload: Arc::new(Game::with_targets(
                b"test-rom",
                "test-core",
                "test-core-sha",
                nes_games::arcade::famidash::group_stage(0),
                Box::new(|| Err("this terminal-only test must not execute a target".into())),
            )),
            observe: Arc::new(|| None),
            terminal,
            streams: Arc::new(super::Streams::default()),
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            server.handle(stream).unwrap();
        });
        client
            .write_all(b"GET /live/events HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut reply = String::new();
        client.read_to_string(&mut reply).unwrap();
        handle.join().unwrap();
        assert!(reply.starts_with("HTTP/1.1 200 OK"));
        assert!(reply.find("ready").unwrap() < reply.find("batch").unwrap());
        assert!(reply.find("batch").unwrap() < reply.find("limit").unwrap());
    }

    type LookupWorkload = nes_games::famidash::campaign::FamidashGame<super::NativeMachine>;

    fn state_lookup_fixture() -> (
        std::sync::Arc<super::Server<LookupWorkload>>,
        super::LiveWatch<LookupWorkload>,
    ) {
        use std::sync::{Arc, Mutex, OnceLock, atomic::AtomicBool, mpsc};
        let workload = Arc::new(LookupWorkload::with_targets(
            b"lookup-test-rom",
            "lookup-test-core",
            "lookup-test-sha",
            nes_games::arcade::famidash::group_stage(0),
            Box::new(|| Err("lookup fixture must not execute a target".into())),
        ));
        let (events, _) = mpsc::channel();
        let (lookups, receiver) = mpsc::channel();
        let pending_lookups = Arc::new(AtomicUsize::new(0));
        let observe: super::Observe<_> = Arc::new(|| None);
        let server = Arc::new(super::Server {
            events: events.clone(),
            lookups,
            pending_lookups: Arc::clone(&pending_lookups),
            site: None,
            workload: Arc::clone(&workload),
            observe: Arc::clone(&observe),
            terminal: Arc::new(OnceLock::new()),
            streams: Arc::new(super::Streams::default()),
        });
        let watch = super::LiveWatch {
            workload,
            shared: Arc::new(Mutex::new(super::Shared {
                feed: None,
                stats: super::Stats::default(),
            })),
            events,
            lookups: Mutex::new(Some(receiver)),
            pending_lookups,
            observe,
            game: "famidash".into(),
            viewers: Arc::new(AtomicUsize::new(0)),
            glance: Arc::new(AtomicBool::new(false)),
            recording: AtomicBool::new(false),
            details: Arc::new(AtomicUsize::new(0)),
            cleared: AtomicUsize::new(0),
        };
        (server, watch)
    }

    fn state_lookup_http(
        server: std::sync::Arc<super::Server<LookupWorkload>>,
    ) -> (std::net::TcpStream, std::thread::JoinHandle<()>) {
        use std::{
            io::Write,
            net::{TcpListener, TcpStream},
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        client
            .write_all(b"GET /live/states?ids=7,8&request=42 HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let thread = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            server.handle(stream).unwrap();
        });
        (client, thread)
    }

    fn state_lookup_response(
        mut client: std::net::TcpStream,
        thread: std::thread::JoinHandle<()>,
    ) -> (String, String) {
        use std::io::Read;
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        thread.join().unwrap();
        let (header, body) = response.split_once("\r\n\r\n").unwrap();
        (header.to_owned(), body.to_owned())
    }

    #[test]
    fn terminal_state_lookup_drains_a_queued_http_request_before_shutdown() {
        let (server, watch) = state_lookup_fixture();
        let (client, thread) = state_lookup_http(std::sync::Arc::clone(&server));
        let lookup = watch
            .lookups
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        server.lookups.send(lookup).unwrap();
        assert!(!server.streams.wait(std::time::Duration::ZERO));
        assert!(server.terminal.set(terminal_fixture()).is_ok());
        watch.close_lookups();
        let (header, body) = state_lookup_response(client, thread);
        assert!(header.starts_with("HTTP/1.1 200 OK"));
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"type":"states","request":42,"states":[],"missing":[7,8]})
        );
        assert!(server.streams.wait(std::time::Duration::ZERO));
        assert!(watch.lookups.lock().unwrap().is_none());
    }

    #[test]
    fn terminal_state_lookup_after_close_and_late_enqueue_return_missing() {
        let (server, watch) = state_lookup_fixture();
        assert!(server.terminal.set(terminal_fixture()).is_ok());
        watch.close_lookups();
        assert!(server.lookup(vec![7, 8]).unwrap().is_empty());
        let (client, thread) = state_lookup_http(std::sync::Arc::clone(&server));
        let (header, body) = state_lookup_response(client, thread);
        assert!(header.starts_with("HTTP/1.1 200 OK"));
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["missing"], serde_json::json!([7, 8]));
        assert_eq!(body["request"], 42);
        assert_eq!(server.pending_lookups.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn terminal_state_lookup_preserves_an_already_served_found_reply() {
        use nes_games::arcade::ArcadeGame;
        let (server, watch) = state_lookup_fixture();
        let (client, thread) = state_lookup_http(std::sync::Arc::clone(&server));
        let lookup = watch
            .lookups
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        watch.pending_lookups.fetch_sub(1, Ordering::Relaxed);
        lookup
            .reply
            .send(vec![super::Found {
                id: 7,
                observation: Some(
                    LookupWorkload::memory_view(&[0; machine::nes::WRAM_SIZE]).unwrap(),
                ),
                digest: Some(81),
                frames_at: Some(4),
                input: vec![machine::nes::ButtonChord::new(1, 4)],
                packed: None,
            }])
            .unwrap_or_else(|_| panic!("queued lookup receiver disappeared"));
        assert!(server.terminal.set(terminal_fixture()).is_ok());
        watch.close_lookups();
        let (header, body) = state_lookup_response(client, thread);
        assert!(header.starts_with("HTTP/1.1 200 OK"));
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["states"][0]["id"], 7);
        assert_eq!(body["states"][0]["frames"], 4);
        assert_eq!(body["states"][0]["actions"][0]["frames"], 4);
        assert_eq!(body["missing"], serde_json::json!([8]));
    }

    #[test]
    fn terminal_state_lookup_keeps_nonterminal_disconnect_an_http_error() {
        let (server, watch) = state_lookup_fixture();
        watch.close_lookups();
        let (client, thread) = state_lookup_http(server);
        let (header, body) = state_lookup_response(client, thread);
        assert!(header.starts_with("HTTP/1.1 503 Service Unavailable"));
        assert_eq!(body, "no state lookup");
    }

    #[test]
    fn shutdown_drain_is_immediate_when_empty_and_bounded_for_a_nonreader() {
        let streams = super::Streams::default();
        assert!(streams.wait(std::time::Duration::ZERO));
        let active = streams.enter();
        assert!(!streams.wait(std::time::Duration::from_millis(1)));
        drop(active);
        assert!(streams.wait(std::time::Duration::ZERO));
    }

    #[test]
    fn shutdown_drain_wakes_when_a_fast_reader_finishes() {
        use std::{
            sync::{Arc, mpsc},
            time::Duration,
        };
        let streams = Arc::new(super::Streams::default());
        let (ready, entered) = mpsc::channel();
        let (release, finished) = mpsc::channel();
        let reader_streams = Arc::clone(&streams);
        let reader = std::thread::spawn(move || {
            let _active = reader_streams.enter();
            ready.send(()).unwrap();
            finished.recv().unwrap();
        });
        entered.recv().unwrap();
        release.send(()).unwrap();
        assert!(streams.wait(Duration::from_secs(1)));
        reader.join().unwrap();
        assert!(streams.wait(Duration::ZERO));
    }

    #[test]
    fn unsampled_completion_points_survive_output_downsampling() {
        let mut points = vec![
            Point {
                observation: 0_u8,
                retained: None,
                frame: 0,
                priority: false,
            };
            10_000
        ];
        points.push(Point {
            observation: 43,
            retained: None,
            frame: 100,
            priority: true,
        });
        let batch = sampled(
            Batch {
                stats: Stats::default(),
                deaths: 0,
                sampled_jobs: 1,
                frames: 100,
                points,
                motion: Vec::new(),
                finishes: Vec::new(),
                exits: Vec::new(),
            },
            &mut Sampler(123),
        );
        assert!(batch.points.len() < 10_001);
        assert!(batch.points.iter().any(|point| point.observation == 43));
        assert!(
            serde_json::to_value(&batch).unwrap()["points"]
                .as_array()
                .unwrap()
                .iter()
                .all(|point| point.get("priority").is_none())
        );
    }

    #[test]
    fn level_advancement_is_observed_when_detail_credits_are_exhausted() {
        let details = AtomicUsize::new(0);
        let cleared = AtomicUsize::new(0);
        assert!(!take_detail(&details));
        assert!(take_progress(&cleared, 1));
        assert!(!take_progress(&cleared, 1));
        assert!(!take_progress(&cleared, 0));
        assert!(!take_detail(&details));
        assert!(take_progress(&cleared, 2));
        assert!(take_progress(&cleared, 43));
        assert!(!take_progress(&cleared, 42));
    }

    #[test]
    fn detail_budget_is_shared_by_all_workers_and_refilled_by_the_feed() {
        let details = AtomicUsize::new(256);
        let recorded = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..128 {
                scope.spawn(|| {
                    for _ in 0..64 {
                        if take_detail(&details) {
                            recorded.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
        });
        assert_eq!(recorded.load(Ordering::Relaxed), 256);
        assert_eq!(details.load(Ordering::Relaxed), 0);
        assert!(!take_detail(&details));
        details.store(2, Ordering::Relaxed);
        assert!(take_detail(&details));
        assert!(take_detail(&details));
        assert!(!take_detail(&details));
    }

    #[test]
    fn site_paths_stay_inside_the_site() {
        assert_eq!(site_path("/"), Some(PathBuf::from("index.html")));
        assert_eq!(
            site_path("/?game=smb&live="),
            Some(PathBuf::from("index.html"))
        );
        assert_eq!(
            site_path("/assets/main.js?v=1"),
            Some(Path::new("assets").join("main.js"))
        );
        for target in ["/../secret", "/assets/../../secret", "/./index.html"] {
            assert_eq!(site_path(target), None, "{target}");
        }
    }

    #[test]
    fn only_loopback_hosts_and_origins_reach_the_server() {
        for host in [
            "127.0.0.1:8790",
            "localhost:41000",
            "[::1]:8790",
            "localhost",
        ] {
            assert!(local_request(Some(host), None), "{host}");
        }
        assert!(local_request(
            Some("127.0.0.1:8790"),
            Some("http://localhost:8790")
        ));
        for (host, origin) in [
            (None, None),
            (Some("evil.example:8790"), None),
            (Some("127.0.0.1.evil.example"), None),
            (Some("127.0.0.1:8790"), Some("https://evil.example")),
            (Some("127.0.0.1:8790"), Some("http://evil.example:8790")),
            (Some("127.0.0.1:8790"), Some("null")),
        ] {
            assert!(!local_request(host, origin), "{host:?} {origin:?}");
        }
    }

    #[test]
    fn snapshots_encode_as_padded_base_sixty_four() {
        assert_eq!(encode_snapshot(b""), "");
        assert_eq!(encode_snapshot(b"f"), "Zg==");
        assert_eq!(encode_snapshot(b"fo"), "Zm8=");
        assert_eq!(encode_snapshot(b"foo"), "Zm9v");
        assert_eq!(encode_snapshot(&[0xff, 0xfe, 0xfd, 0xfc]), "//79/A==");
    }

    #[test]
    fn archive_glances_preserve_newest_states_with_bounded_samples() {
        for entries in [0, 1, 249, 250, 251, 999, 1_000, 1_001, usize::MAX] {
            let indices = glance_indices(entries, &mut Sampler(7));
            assert_eq!(indices.len(), entries.min(super::GLANCE_STATES));
            assert!(indices.windows(2).all(|pair| pair[0] < pair[1]));
            assert!(indices.iter().all(|index| *index < entries));
            let newest = entries.saturating_sub(super::GLANCE_NEWEST);
            assert!(indices.ends_with(&(newest..entries).collect::<Vec<_>>()));
        }
    }

    #[test]
    fn visual_range_reduction_rejects_the_incomplete_tail_without_overflow() {
        let mut draws = [u64::MAX, 1].into_iter();
        assert_eq!(Sampler::below_with(2, || draws.next().unwrap()), 0);
        assert!(draws.next().is_none());
        assert_eq!(Sampler::below_with(1, || u64::MAX), 0);
        assert_eq!(
            Sampler::below_with(usize::MAX, || usize::MAX as u64),
            usize::MAX - 1
        );
        let bound = usize::MAX - 1;
        assert_eq!(Sampler::below_with(bound, || bound as u64), bound - 1);
    }

    #[test]
    fn sampling_work_is_bounded_by_the_requested_count() {
        for len in [0, 1, 10, 1_000, 1_000_000_000, usize::MAX] {
            for keep in [0, 1, 7, 750] {
                let mut calls = 0;
                let picked = Sampler::pick_with(len, keep, |bound| {
                    calls += 1;
                    bound - 1
                });
                assert_eq!(calls, len.min(keep));
                assert_eq!(picked.len(), len.min(keep));
                assert!(picked.windows(2).all(|pair| pair[0] < pair[1]));
                assert!(picked.iter().all(|index| *index < len));
            }
        }
    }

    #[test]
    fn floyd_sampling_gives_each_small_subset_the_same_number_of_draws() {
        let mut counts = std::collections::BTreeMap::new();
        for first in 0..3 {
            for second in 0..4 {
                let mut draws = [first, second].into_iter();
                let picked = Sampler::pick_with(4, 2, |_| draws.next().unwrap());
                assert!(draws.next().is_none());
                *counts.entry(picked).or_insert(0) += 1;
            }
        }
        assert_eq!(counts.len(), 6);
        assert!(counts.values().all(|count| *count == 2));
    }

    #[test]
    fn the_sampler_keeps_sorted_distinct_indices() {
        let mut sampler = Sampler(7);
        assert_eq!(sampler.pick(3, 5), vec![0, 1, 2]);
        let picked = sampler.pick(1_000, 32);
        assert_eq!(picked.len(), 32);
        assert!(picked.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(picked.iter().all(|&index| index < 1_000));
    }
}
