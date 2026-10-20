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
        campaign::{CampaignCoordinator, CampaignWatch, JobOutcome, JobSpec, WatchedJob},
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

type Games<G> = Arc<dyn Fn() -> G + Send + Sync>;

pub type MemoryView<V> = fn(&[u8]) -> std::result::Result<V, Box<dyn Error>>;

type Observe<V> = Arc<dyn Fn(&[Vec<u8>]) -> Vec<Option<V>> + Send + Sync>;

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
    shared: SharedFeed<G::View>,
    events: Sender<Event>,
    lookups: Mutex<Receiver<Lookup<G::View>>>,
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
    Arc::new(move |raws| {
        if raws.is_empty() {
            return Vec::new();
        }
        let Ok(mut machine) =
            QuickNesMachine::from_rom_bytes_with_identity(&rom, &path, &sha256, &execution)
        else {
            return raws.iter().map(|_| None).collect();
        };
        raws.iter()
            .map(|raw| view(&read(&mut machine, raw).ok()?).ok())
            .collect()
    })
}

fn glance<G: ArcadeGame>(campaign: &CampaignCoordinator<'_, G>) -> Vec<(u64, Vec<u8>)> {
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

impl<G: ArcadeGame> LiveWatch<G> {
    fn root(&self, campaign: &CampaignCoordinator<'_, G>) -> Result<()>
    where
        G::ArchiveReport: Serialize,
    {
        let archive = campaign.archive();
        let (id, snapshot) = archive
            .index_of_id(0)
            .or((archive.live_entry_count() > 0).then_some(0))
            .and_then(|index| Some((archive.stable_id(index)?, archive.entry_snapshot(index)?)))
            .ok_or("the live view found no archived state to start from")?;
        let raw = raw_state(G::emulator_state(snapshot))?;
        let view = (self.observe)(std::slice::from_ref(&raw))
            .pop()
            .flatten()
            .ok_or("the live view could not read the first archived state")?;
        let snapshot = core_state(&raw);
        let digest = snapshot_digest(snapshot);
        {
            let mut shared = lock(&self.shared);
            shared.feed = Some(Feed::new(view, 0, digest));
            shared.stats = feed::stats(campaign);
        }
        let root = State {
            id,
            observation: view,
            actions: Vec::new(),
            frames: 0,
        };
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

    fn serve(&self, campaign: &CampaignCoordinator<'_, G>)
    where
        G::ArchiveReport: Serialize,
    {
        if self.pending_lookups.load(Ordering::Relaxed) == 0 {
            return;
        }
        let Ok(lookups) = self.lookups.try_lock() else {
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
                        frames_at: feed.and_then(|feed| feed.frames_at(id)),
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

impl<G: ArcadeGame> CampaignWatch<G> for LiveWatch<G>
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
    let (ids, raws): (Vec<u64>, Vec<Vec<u8>>) = packed
        .iter()
        .filter_map(|(id, packed)| Some((*id, raw_state(packed).ok()?)))
        .unzip();
    let points = ids
        .into_iter()
        .zip(observe(&raws))
        .filter_map(|(id, observation)| {
            Some(Point {
                observation: observation?,
                retained: Some(id),
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
    games: Option<Games<G>>,
    observe: Observe<G::View>,
    terminal: Arc<OnceLock<Terminal>>,
    streams: Arc<Streams>,
}

impl<G: ArcadeGame> Server<G> {
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
        let (reply, answer) = mpsc::channel();
        self.pending_lookups.fetch_add(1, Ordering::Relaxed);
        if self
            .lookups
            .send(Lookup {
                ids: ids.clone(),
                reply,
            })
            .is_err()
        {
            self.pending_lookups.fetch_sub(1, Ordering::Relaxed);
            return None;
        }
        let mut found = answer.recv_timeout(LOOKUP_WAIT).ok()?;
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
        let raws: Vec<Vec<u8>> = unseen
            .iter()
            .filter_map(|&index| raw[index].clone())
            .collect();
        for (index, observation) in unseen.into_iter().zip((self.observe)(&raws)) {
            found[index].observation = observation;
        }
        let mut replay: Option<(G, usize)> = None;
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
            let state = feed::archived_state(
                found.id,
                observation,
                &[],
                &found.input,
                found.frames_at.filter(|_| self.games.is_some()),
                |input| {
                    if replay.is_none() {
                        let game = (self.games.as_ref().ok_or("no replay game")?)();
                        let genesis = feed::genesis_actions(&game)?;
                        replay = Some((game, genesis));
                    }
                    let (game, genesis) = replay.as_ref().ok_or("no replay game")?;
                    feed::played(game, None, *genesis, input)
                },
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

fn listen<G: ArcadeGame>(listener: &TcpListener, server: &Arc<Server<G>>) {
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
    G: ArcadeGame<Machine = NativeMachine>,
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
    let (game_rom, core, core_sha256, execution) = (
        rom.to_vec(),
        request.core.clone(),
        request.core_sha256.clone(),
        execution_sha256.clone(),
    );
    let games: Games<G> = Arc::new(move || native_game(&game_rom, &core, &core_sha256, &execution));
    let watched = Watched {
        rom,
        request,
        execution_sha256: &execution_sha256,
        out,
        started,
        id,
        address,
        view: G::memory_view,
    };
    watched.evaluate(games(), G::run(), Some(games))
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
    G: ArcadeGame,
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
        started,
        id,
        address: "127.0.0.1:0",
        view,
    };
    watched.evaluate(game, run, None)
}

struct Watched<'a, V> {
    rom: &'a [u8],
    request: &'a Request,
    execution_sha256: &'a str,
    out: &'a Path,
    started: Instant,
    id: &'a str,
    address: &'a str,
    view: MemoryView<V>,
}

impl<V: Serialize + Copy + Send + 'static> Watched<'_, V> {
    fn evaluate<G>(&self, game: G, run: G::Run, games: Option<Games<G>>) -> Result<()>
    where
        G: ArcadeGame<View = V>,
        G::ArchiveReport: Serialize + serde::de::DeserializeOwned,
        G::Milestones: 'static,
        G::Observations: 'static,
        G::Snapshot: EndpointIdentity + 'static,
    {
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
            shared: Arc::clone(&shared),
            events: events.clone(),
            lookups: Mutex::new(pending),
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
            games,
            observe,
            terminal,
            streams: Arc::clone(&streams),
        });
        thread::spawn(move || listen(&listener, &server));
        let result = evaluate_watched(
            game,
            run,
            self.request,
            self.execution_sha256,
            self.out,
            self.started,
            &watch,
        );
        let _ = watch.events.send(Event::Finished);
        let _ = feed.join();
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
        let mut feed = Feed::new(0_u8, 0, 0);
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
                stopped: true,
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
        let observe: super::Observe<u8> = Arc::new(|_| Vec::new());
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
            games: None,
            observe: Arc::new(|_| Vec::new()),
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
