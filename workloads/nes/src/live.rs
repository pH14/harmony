// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::VecDeque,
    error::Error,
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use machine::{MachineError, nes::core_state, quicknes::QuickNesMachine};
use nes_games::arcade::{
    ArcadeGame,
    feed::{self, Batch, Feed, Note, Point, Stats, actions, chord_frames, raw_state, run_noted},
    snapshot_digest,
    trail::Traced,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    eval::{Request, Result, evaluate_watched},
    search::campaign::{
        CampaignAdmission, CampaignCoordinator, CampaignWatch, JobOutcome, JobSpec, WatchedJob,
    },
    witness::EndpointIdentity,
};

const TICK: Duration = Duration::from_millis(100);
const HEARTBEAT: Duration = Duration::from_secs(15);
const LOOKUP_WAIT: Duration = Duration::from_secs(10);
const TRAILS_PER_TICK: usize = 32;
const FINISHES_PER_TICK: usize = 8;
const POINTS_PER_TICK: usize = 2_000;
const HISTORY_POINTS: usize = 40_000;
const STATES_PER_LOOKUP: usize = 12;
const CLIENT_QUEUE: usize = 64;

pub type NativeMachine = Traced<QuickNesMachine>;

#[must_use]
pub fn quicknes_core() -> String {
    format!(
        "quicknes-libretro:{};{};{}",
        machine::quicknes::QUICKNES_REVISION,
        machine::quicknes::QUICKNES_BUILD,
        machine::quicknes::QUICKNES_OPTIONS,
    )
}

pub fn native_game<G: ArcadeGame<Machine = NativeMachine>>(
    rom: &[u8],
    core_path: &Path,
    core_sha256: &str,
) -> G {
    let (machine_rom, path, sha256) = (
        rom.to_vec(),
        core_path.to_path_buf(),
        core_sha256.to_owned(),
    );
    G::arcade(
        rom,
        &quicknes_core(),
        core_sha256,
        Arc::new(move || {
            Ok(Traced::new(
                QuickNesMachine::from_rom_bytes(&machine_rom, &path, &sha256)?,
                G::probe(),
            ))
        }),
    )
}

struct Lookup<V> {
    request: i64,
    ids: Vec<u64>,
    views: Vec<Option<(V, u64)>>,
    reply: Sender<String>,
}

enum Event<V> {
    Root {
        view: V,
        digest: u64,
        snapshot: Vec<u8>,
    },
    Admitted {
        note: Note<V>,
        admissions: Vec<CampaignAdmission>,
        stats: Stats,
    },
    Finished,
    Query {
        request: i64,
        ids: Vec<String>,
        reply: Sender<String>,
    },
    Viewer {
        sender: SyncSender<Arc<str>>,
    },
}

pub struct LiveWatch<G: ArcadeGame> {
    events: Sender<Event<G::View>>,
    lookups: Mutex<Receiver<Lookup<G::View>>>,
    memory: Memory,
}

type Memory = Box<dyn Fn(&[u8]) -> std::result::Result<Vec<u8>, MachineError> + Send + Sync>;

fn native_memory(rom: &[u8], core_path: &Path, core_sha256: &str, bytes: usize) -> Memory {
    let (rom, path, sha256) = (
        rom.to_vec(),
        core_path.to_path_buf(),
        core_sha256.to_owned(),
    );
    Box::new(move |raw| {
        let mut machine = QuickNesMachine::from_rom_bytes(&rom, &path, &sha256)?;
        machine.restore_bytes(raw)?;
        let mut memory = machine.read_wram()?.to_vec();
        if bytes > memory.len() {
            memory.extend(machine.read_save_ram()?);
        }
        Ok(memory)
    })
}

impl<G: ArcadeGame> LiveWatch<G> {
    fn root(&self, campaign: &CampaignCoordinator<'_, G>) -> Result<()>
    where
        G::ArchiveReport: Serialize,
    {
        let archive = campaign.archive();
        let snapshot = archive
            .index_of_id(0)
            .and_then(|index| archive.entry_snapshot(index))
            .ok_or("the campaign has no root snapshot")?;
        let raw = raw_state(G::emulator_state(snapshot))?;
        let view = G::memory_view(&(self.memory)(&raw)?)?;
        let _ = self.events.send(Event::Root {
            view,
            digest: snapshot_digest(core_state(&raw)),
            snapshot: core_state(&raw).to_vec(),
        });
        Ok(())
    }

    fn serve(&self, campaign: &CampaignCoordinator<'_, G>)
    where
        G::ArchiveReport: Serialize,
    {
        let Ok(lookups) = self.lookups.try_lock() else {
            return;
        };
        while let Ok(lookup) = lookups.try_recv() {
            let archive = campaign.archive();
            let mut states = Vec::new();
            let mut missing = Vec::new();
            for (id, view) in lookup.ids.iter().zip(lookup.views) {
                let found = view.and_then(|(observation, digest)| {
                    let index = archive.index_of_id(*id)?;
                    let input = archive.entry_input(index).ok()?.actions;
                    let snapshot = archive
                        .entry_snapshot(index)
                        .and_then(|snapshot| raw_state(G::emulator_state(snapshot)).ok());
                    Some(state_json(
                        *id,
                        observation,
                        &input,
                        digest,
                        snapshot.as_deref().map(core_state),
                    ))
                });
                match found {
                    Some(state) => states.push(state),
                    None => missing.push(json!(id)),
                }
            }
            let _ = lookup.reply.send(
                json!({"type": "states", "request": lookup.request, "states": states, "missing": missing})
                    .to_string(),
            );
        }
    }
}

impl<G: ArcadeGame> CampaignWatch<G> for LiveWatch<G>
where
    G::ArchiveReport: Serialize,
{
    type Note = Option<Note<G::View>>;

    fn run(
        &self,
        job: &JobSpec<G>,
        run: impl FnOnce() -> std::result::Result<JobOutcome<G>, Box<dyn Error>>,
    ) -> WatchedJob<G, Self::Note> {
        let (outcome, note) = run_noted(job, run)?;
        Ok((outcome, Some(note)))
    }

    fn admitted(&self, note: Self::Note, campaign: &mut CampaignCoordinator<'_, G>) {
        let admissions = campaign.take_admissions();
        if let Some(note) = note {
            let _ = self.events.send(Event::Admitted {
                note,
                admissions,
                stats: feed::stats(campaign),
            });
        }
        self.serve(campaign);
    }

    fn start(&self, campaign: &mut CampaignCoordinator<'_, G>) {
        campaign.watch_admissions();
        if let Err(error) = self.root(campaign) {
            eprintln!("live view has no root state: {error}");
        }
    }
}

fn state_json<V: Serialize>(
    id: u64,
    observation: V,
    input: &[machine::nes::ButtonChord],
    digest: u64,
    snapshot: Option<&[u8]>,
) -> Value {
    json!({
        "id": id,
        "observation": observation,
        "actions": actions(input),
        "frames": chord_frames(input),
        "digest": format!("{digest:016x}"),
        "snapshot": snapshot.map(encode_snapshot),
        "branch": null,
        "boot_level": 0,
    })
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
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % bound as u64) as usize
    }

    fn pick(&mut self, len: usize, keep: usize) -> Vec<usize> {
        if len <= keep {
            return (0..len).collect();
        }
        let mut chosen: Vec<usize> = (0..keep).collect();
        for index in keep..len {
            let slot = self.below(index + 1);
            if slot < keep {
                chosen[slot] = index;
            }
        }
        chosen.sort_unstable();
        chosen
    }
}

struct Viewers {
    senders: Vec<SyncSender<Arc<str>>>,
}

impl Viewers {
    fn broadcast(&mut self, message: &Arc<str>) {
        self.senders.retain(|sender| {
            !matches!(
                sender.try_send(Arc::clone(message)),
                Err(TrySendError::Disconnected(_))
            )
        });
    }
}

fn batch_json<V: Serialize>(batch: &Batch<V>, workers: u32) -> Arc<str> {
    let mut message = serde_json::to_value(batch).unwrap_or_default();
    if let Value::Object(fields) = &mut message {
        for (key, value) in [
            ("type", json!("batch")),
            ("active", json!(0)),
            ("workers", json!(workers)),
            ("wasm_bytes", json!(0)),
        ] {
            fields.insert(key.to_owned(), value);
        }
    }
    Arc::from(message.to_string())
}

#[allow(clippy::disallowed_methods)]
fn wall_now() -> Instant {
    Instant::now()
}

fn backlog<V: Copy>(stats: Stats, points: Vec<Point<V>>) -> Batch<V> {
    Batch {
        stats,
        deaths: 0,
        frames: 0,
        points,
        motion: Vec::new(),
        finishes: Vec::new(),
        exits: Vec::new(),
    }
}

fn feed_loop<V: Serialize + Copy + Send + 'static>(
    events: Receiver<Event<V>>,
    lookups: Sender<Lookup<V>>,
    game: String,
    workers: u32,
) {
    let mut feed: Option<Feed<V>> = None;
    let mut ready: Option<Arc<str>> = None;
    let mut viewers = Viewers {
        senders: Vec::new(),
    };
    let mut history: VecDeque<Point<V>> = VecDeque::new();
    let mut stats = Stats::default();
    let mut sampler = Sampler(0x9e37_79b9_7f4a_7c15);
    let mut next_tick = wall_now() + TICK;
    loop {
        let wait = next_tick.saturating_duration_since(wall_now());
        match events.recv_timeout(wait) {
            Ok(Event::Root {
                view,
                digest,
                snapshot,
            }) => {
                feed = Some(Feed::new(view, 0, digest));
                ready = Some(Arc::from(
                    json!({
                        "type": "ready",
                        "game": game,
                        "live": true,
                        "active": 0,
                        "paused": false,
                        "searches": [{"id": 0, "origin": null, "branch": null, "parent": null}],
                        "state": state_json(0, view, &[], digest, Some(&snapshot)),
                    })
                    .to_string(),
                ));
            }
            Ok(Event::Admitted {
                note,
                admissions,
                stats: latest,
            }) => {
                if let Some(feed) = feed.as_mut() {
                    feed.note(note);
                    feed.admit(admissions);
                }
                stats = latest;
            }
            Ok(Event::Query {
                request,
                ids,
                reply,
            }) => {
                let ids: Vec<u64> = ids
                    .iter()
                    .take(STATES_PER_LOOKUP)
                    .filter_map(|id| id.parse().ok())
                    .collect();
                let views = ids
                    .iter()
                    .map(|&id| {
                        feed.as_ref()
                            .and_then(|feed| Some((feed.view(id)?, feed.digest(id)?)))
                    })
                    .collect();
                let _ = lookups.send(Lookup {
                    request,
                    ids,
                    views,
                    reply,
                });
            }
            Ok(Event::Viewer { sender }) => {
                if let Some(ready) = &ready {
                    let _ = sender.try_send(Arc::clone(ready));
                    let history_points: Vec<Point<V>> = history.iter().copied().collect();
                    let _ = sender.try_send(batch_json(&backlog(stats, history_points), workers));
                }
                viewers.senders.push(sender);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Ok(Event::Finished) | Err(RecvTimeoutError::Disconnected) => {
                if let Some(feed) = feed.as_mut() {
                    let batch = feed.take_batch(stats);
                    let mut batch = batch;
                    batch.points.retain(|point| point.retained.is_some());
                    batch.motion.clear();
                    batch.finishes.clear();
                    batch.exits.clear();
                    viewers.broadcast(&batch_json(&batch, workers));
                }
                let done: Arc<str> = Arc::from(
                    json!({"type": "limit", "won": stats.won, "finished": true}).to_string(),
                );
                viewers.broadcast(&done);
                return;
            }
        }
        if wall_now() < next_tick {
            continue;
        }
        next_tick = wall_now() + TICK;
        let Some(feed) = feed.as_mut() else {
            continue;
        };
        let batch = feed.take_batch(stats);
        let (retained, open): (Vec<_>, Vec<_>) = batch
            .points
            .iter()
            .copied()
            .partition(|point| point.retained.is_some());
        let mut points = retained;
        points.extend(
            sampler
                .pick(open.len(), POINTS_PER_TICK)
                .into_iter()
                .map(|index| open[index]),
        );
        for point in &points {
            if history.len() == HISTORY_POINTS {
                history.pop_front();
            }
            history.push_back(*point);
        }
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
        let motion: Vec<Vec<u16>> = kept
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
        let message = batch_json(
            &Batch {
                points,
                motion,
                finishes,
                exits,
                ..batch
            },
            workers,
        );
        viewers.broadcast(&message);
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
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
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

fn site_file(site: &Path, target: &str) -> Option<PathBuf> {
    let path = target.split('?').next()?.trim_start_matches('/');
    let relative = Path::new(if path.is_empty() { "index.html" } else { path });
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    let file = site.join(relative);
    file.is_file().then_some(file)
}

fn handle<V>(
    mut stream: TcpStream,
    events: &Sender<Event<V>>,
    site: Option<&Path>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Ok(());
    };
    let target = target.to_owned();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header == "\r\n" || header == "\n" {
            break;
        }
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
        let (sender, receiver) = mpsc::sync_channel(CLIENT_QUEUE);
        if events.send(Event::Viewer { sender }).is_err() {
            return respond(
                &mut stream,
                "410 Gone",
                "text/plain",
                b"the campaign has ended",
            );
        }
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nAccess-Control-Allow-Origin: *\r\n\r\n")?;
        loop {
            match receiver.recv_timeout(HEARTBEAT) {
                Ok(message) => {
                    stream.write_all(b"data: ")?;
                    stream.write_all(message.as_bytes())?;
                    stream.write_all(b"\n\n")?;
                }
                Err(RecvTimeoutError::Timeout) => stream.write_all(b": heartbeat\n\n")?,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }
    }
    if target.starts_with("/live/states") {
        let request = query(&target, "request")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let ids = query(&target, "ids")
            .map(|ids| ids.split(',').map(str::to_owned).collect())
            .unwrap_or_default();
        let (reply, answer) = mpsc::channel();
        let body = if events
            .send(Event::Query {
                request,
                ids,
                reply,
            })
            .is_ok()
        {
            answer.recv_timeout(LOOKUP_WAIT).ok()
        } else {
            None
        };
        return match body {
            Some(body) => respond(&mut stream, "200 OK", "application/json", body.as_bytes()),
            None => respond(
                &mut stream,
                "503 Service Unavailable",
                "text/plain",
                b"no state lookup",
            ),
        };
    }
    match site.and_then(|site| site_file(site, &target)) {
        Some(file) => respond(
            &mut stream,
            "200 OK",
            content_type(&file),
            &fs::read(&file)?,
        ),
        None => respond(&mut stream, "404 Not Found", "text/plain", b"not found"),
    }
}

fn serve<V: Send + 'static>(
    listener: TcpListener,
    events: Sender<Event<V>>,
    site: Option<PathBuf>,
) {
    for stream in listener.incoming().flatten() {
        let events = events.clone();
        let site = site.clone();
        thread::spawn(move || {
            let _ = handle(stream, &events, site.as_deref());
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
    G::Snapshot: EndpointIdentity,
{
    let address = request
        .live
        .as_deref()
        .ok_or("live evaluation needs a listen address")?;
    let listener = TcpListener::bind(address)?;
    eprintln!("live view listening on {}", listener.local_addr()?);
    let (events, received) = mpsc::channel();
    let (lookups, pending) = mpsc::channel();
    let watch = LiveWatch::<G> {
        events: events.clone(),
        lookups: Mutex::new(pending),
        memory: native_memory(rom, &request.core, &request.core_sha256, G::MEMORY_BYTES),
    };
    let game: G = native_game(rom, &request.core, &request.core_sha256);
    let (game_id, workers) = (id.to_owned(), request.workers);
    let feed = thread::spawn(move || feed_loop(received, lookups, game_id, workers));
    let site = request.live_site.clone();
    thread::spawn(move || serve(listener, events, site));
    let result = evaluate_watched(game, G::run(), request, out, started, &watch);
    let _ = watch.events.send(Event::Finished);
    let _ = feed.join();
    result
}
