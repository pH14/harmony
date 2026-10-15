// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::VecDeque,
    error::Error,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicUsize, Ordering},
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
use nes_games::arcade::{
    ArcadeGame,
    feed::{self, Batch, Feed, Note, Point, State, Stats, raw_state, run_noted},
    snapshot_digest,
    trail::Traced,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    eval::{Request, Result, evaluate_watched},
    search::campaign::{CampaignCoordinator, CampaignWatch, JobOutcome, JobSpec, WatchedJob},
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
const HISTORY_POINTS: usize = 40_000;
const STATES_PER_LOOKUP: usize = 12;
const CLIENT_QUEUE: usize = 64;

pub type NativeMachine = Traced<QuickNesMachine>;

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
        &machine::quicknes::core_identity(),
        core_sha256,
        Arc::new(move || {
            Ok(Traced::new(
                QuickNesMachine::from_rom_bytes(&machine_rom, &path, &sha256)?,
                G::probe(),
            ))
        }),
    )
}

type Games<G> = Arc<dyn Fn() -> G + Send + Sync>;

type Memory = Box<dyn Fn(&[u8]) -> std::result::Result<Vec<u8>, MachineError> + Send + Sync>;

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
    observation: V,
    digest: u64,
    frames_at: Option<u64>,
    input: Vec<ButtonChord>,
    snapshot: Option<Vec<u8>>,
}

struct Lookup<V> {
    ids: Vec<u64>,
    reply: Sender<Vec<Found<V>>>,
}

enum Event {
    Ready(Arc<str>),
    Viewer(SyncSender<Arc<str>>),
    Finished,
}

pub struct LiveWatch<G: ArcadeGame> {
    shared: SharedFeed<G::View>,
    events: Sender<Event>,
    lookups: Mutex<Receiver<Lookup<G::View>>>,
    memory: Memory,
    game: String,
}

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
            .ok_or("the live view needs a campaign that starts at genesis")?;
        let raw = raw_state(G::emulator_state(snapshot))?;
        let view = G::memory_view(&(self.memory)(&raw)?)?;
        let snapshot = core_state(&raw);
        let digest = snapshot_digest(snapshot);
        {
            let mut shared = lock(&self.shared);
            shared.feed = Some(Feed::new(view, 0, digest));
            shared.stats = feed::stats(campaign);
        }
        let root = State {
            id: 0,
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
        let Ok(lookups) = self.lookups.try_lock() else {
            return;
        };
        while let Ok(lookup) = lookups.try_recv() {
            let archive = campaign.archive();
            let shared = lock(&self.shared);
            let found = lookup
                .ids
                .iter()
                .filter_map(|&id| {
                    let feed = shared.feed.as_ref()?;
                    let index = archive.index_of_id(id)?;
                    Some(Found {
                        id,
                        observation: feed.view(id)?,
                        digest: feed.digest(id)?,
                        frames_at: feed.frames_at(id),
                        input: archive.entry_input(index).ok()?.actions,
                        snapshot: archive
                            .entry_snapshot(index)
                            .and_then(|snapshot| raw_state(G::emulator_state(snapshot)).ok())
                            .map(|raw| core_state(&raw).to_vec()),
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
        run_noted(job, run)
    }

    fn admitted(&self, note: Self::Note, campaign: &mut CampaignCoordinator<'_, G>) {
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
        self.serve(campaign);
    }

    fn start(&self, campaign: &mut CampaignCoordinator<'_, G>) -> Result<()> {
        campaign.watch_admissions();
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

fn backlog<V: Serialize + Copy>(
    stats: Stats,
    history: &VecDeque<Point<V>>,
    workers: u32,
) -> Arc<str> {
    batch_json(
        &Batch {
            stats,
            deaths: 0,
            frames: 0,
            points: history.iter().copied().collect(),
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
        .partition(|point| point.retained.is_some());
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

fn feed_loop<V: Serialize + Copy>(
    events: &Receiver<Event>,
    shared: &Mutex<Shared<V>>,
    workers: u32,
) {
    let mut ready: Option<Arc<str>> = None;
    let mut viewers = Viewers {
        senders: Vec::new(),
    };
    let mut history: VecDeque<Point<V>> = VecDeque::new();
    let mut sampler = Sampler(0x9e37_79b9_7f4a_7c15);
    let mut next_tick = wall_now() + TICK;
    loop {
        let wait = next_tick.saturating_duration_since(wall_now());
        match events.recv_timeout(wait) {
            Ok(Event::Ready(message)) => {
                let stats = lock(shared).stats;
                viewers.broadcast(&message);
                viewers.broadcast(&backlog(stats, &history, workers));
                ready = Some(message);
            }
            Ok(Event::Viewer(sender)) => {
                if let Some(ready) = &ready {
                    let stats = lock(shared).stats;
                    let _ = sender.try_send(Arc::clone(ready));
                    let _ = sender.try_send(backlog(stats, &history, workers));
                }
                viewers.senders.push(sender);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Ok(Event::Finished) | Err(RecvTimeoutError::Disconnected) => {
                let won = lock(shared).stats.won;
                if let Some(mut batch) = take_batch(shared) {
                    batch.points.retain(|point| point.retained.is_some());
                    batch.motion.clear();
                    batch.finishes.clear();
                    batch.exits.clear();
                    viewers.broadcast(&batch_json(&batch, workers));
                }
                let done = json!({"type": "limit", "won": won, "finished": true});
                viewers.broadcast(&Arc::from(done.to_string()));
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
        let batch = sampled(batch, &mut sampler);
        for point in &batch.points {
            if history.len() == HISTORY_POINTS {
                history.pop_front();
            }
            history.push_back(*point);
        }
        viewers.broadcast(&batch_json(&batch, workers));
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
    site: Option<PathBuf>,
    games: Games<G>,
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
        self.lookups
            .send(Lookup {
                ids: ids.clone(),
                reply,
            })
            .ok()?;
        let found = answer.recv_timeout(LOOKUP_WAIT).ok()?;
        let mut replay: Option<(G, usize)> = None;
        let mut states = Vec::new();
        let mut answered = Vec::new();
        for found in found {
            let state = feed::archived_state(
                found.id,
                found.observation,
                &[],
                &found.input,
                found.frames_at,
                |input| {
                    if replay.is_none() {
                        let game = (self.games)();
                        let genesis = feed::genesis_actions(&game)?;
                        replay = Some((game, genesis));
                    }
                    let (game, genesis) = replay.as_ref().ok_or("no replay game")?;
                    feed::played(game, None, *genesis, input)
                },
            );
            if let Ok(state) = state {
                states.push(state_json(&state, found.digest, found.snapshot.as_deref()));
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
            if self.events.send(Event::Viewer(sender)).is_err() {
                return respond(
                    &mut stream,
                    "410 Gone",
                    "text/plain",
                    b"the campaign has ended",
                );
            }
            stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\n",
            )?;
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
    G::Snapshot: EndpointIdentity,
{
    let address = request
        .live
        .as_deref()
        .ok_or("live evaluation needs a listen address")?;
    let listener = TcpListener::bind(address)?;
    let local = listener.local_addr()?;
    if !local.ip().is_loopback() {
        return Err(format!(
            "the live view listens only on a loopback address, and {local} is not one; forward the port over SSH to watch from another machine"
        )
        .into());
    }
    eprintln!("live view listening on {local}");
    let shared: SharedFeed<G::View> = Arc::new(Mutex::new(Shared {
        feed: None,
        stats: Stats::default(),
    }));
    let (events, received) = mpsc::channel();
    let (lookups, pending) = mpsc::channel();
    let watch = LiveWatch::<G> {
        shared: Arc::clone(&shared),
        events: events.clone(),
        lookups: Mutex::new(pending),
        memory: native_memory(rom, &request.core, &request.core_sha256, G::MEMORY_BYTES),
        game: id.to_owned(),
    };
    let (game_rom, core, core_sha256) = (
        rom.to_vec(),
        request.core.clone(),
        request.core_sha256.clone(),
    );
    let games: Games<G> = Arc::new(move || native_game(&game_rom, &core, &core_sha256));
    let workers = request.workers;
    let feed = thread::spawn(move || feed_loop(&received, &shared, workers));
    let server = Arc::new(Server {
        events,
        lookups,
        site: request.live_site.clone(),
        games: Arc::clone(&games),
    });
    thread::spawn(move || listen(&listener, &server));
    let result = evaluate_watched(games(), G::run(), request, out, started, &watch);
    let _ = watch.events.send(Event::Finished);
    let _ = feed.join();
    result
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Sampler, encode_snapshot, site_path};

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
    fn snapshots_encode_as_padded_base_sixty_four() {
        assert_eq!(encode_snapshot(b""), "");
        assert_eq!(encode_snapshot(b"f"), "Zg==");
        assert_eq!(encode_snapshot(b"fo"), "Zm8=");
        assert_eq!(encode_snapshot(b"foo"), "Zm9v");
        assert_eq!(encode_snapshot(&[0xff, 0xfe, 0xfd, 0xfc]), "//79/A==");
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
