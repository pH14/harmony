// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    error::Error,
    io::Write,
    ops::Range,
    rc::Rc,
    sync::Arc,
};

use machine::{
    Answer, Machine, MachineError, Moment, Reproducer, SnapId, StopConditions, StopReason,
    nes::{ButtonChord, WRAM_SIZE, actions_of},
};
use nes_games::{
    arcade::{
        ArcadeGame,
        feed::{self, Feed, Note, actions, chord_frames, raw_state, run_noted},
        snapshot_digest,
        trail::{Traced, record_path},
    },
    nes_backend::{NesBackend, NesMemory, pack_quicknes_state, unpack_state_into},
    search::{
        archive::{MAX_ARCHIVE_ENTRIES, RetentionPolicy},
        campaign::{
            CampaignCheckpoint, CampaignConfig, CampaignCoordinator, CampaignExecutionOptions,
            CampaignOrigin, JobOutcome, JobSpec, SnapshotCheckpoint, SnapshotCheckpointEntry,
            run_job,
        },
    },
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use wasm_bindgen::prelude::*;

use crate::{
    famidash::Famidash, mega_man::MegaMan, metroid::Metroid, smb::Smb, twin_dragons::TwinDragons,
};

const WINDOW: usize = 16;

#[wasm_bindgen(
    inline_js = "const core = (id) => globalThis.harmonyCores[id]; export function core_reset(id){core(id).reset()} export function core_frames(id,buttons,frames){return core(id).frames(buttons,frames)} export function core_capture(id){return core(id).capture()} export function core_restore(id,state){core(id).restore(state)} export function core_ram(id){return core(id).memory()} export function core_poke(id,addr,value){core(id).ramView()[addr]=value}"
)]
extern "C" {
    #[wasm_bindgen(catch)]
    fn core_reset(id: u32) -> Result<(), JsValue>;
    #[wasm_bindgen(catch)]
    fn core_frames(id: u32, buttons: u8, frames: u8) -> Result<Vec<u8>, JsValue>;
    #[wasm_bindgen(catch)]
    fn core_capture(id: u32) -> Result<Vec<u8>, JsValue>;
    #[wasm_bindgen(catch)]
    fn core_restore(id: u32, state: &[u8]) -> Result<(), JsValue>;
    #[wasm_bindgen(catch)]
    fn core_ram(id: u32) -> Result<Vec<u8>, JsValue>;
    #[wasm_bindgen(catch)]
    fn core_poke(id: u32, addr: usize, value: u8) -> Result<(), JsValue>;
}

fn backend(error: JsValue) -> MachineError {
    MachineError::Backend(error.as_string().unwrap_or_else(|| format!("{error:?}")))
}

pub fn js_error(error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
}

pub fn restore_ram(core: u32, raw: &[u8]) -> Result<Vec<u8>, MachineError> {
    core_restore(core, raw).map_err(backend)?;
    core_ram(core).map_err(backend)
}

pub trait BrowserGame: ArcadeGame<Machine = Traced<JsMachine>> {
    fn root_snapshot(
        target: &mut Self::Target,
        core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<Self::Snapshot, Box<dyn Error>>;

    fn create(core: u32, rom: &[u8], identity: &str, identity_sha256: &str) -> Self {
        Self::arcade(
            rom,
            identity,
            identity_sha256,
            Arc::new(move || {
                Ok(Traced::new(
                    JsMachine::new(core, Self::MEMORY_BYTES)?,
                    Self::probe(),
                ))
            }),
        )
    }
}

pub struct JsMachine {
    core: u32,
    state_len: usize,
    memory_bytes: usize,
    snapshots: BTreeMap<u64, Vec<u8>>,
    next: u64,
    staged: VecDeque<ButtonChord>,
    vtime: u64,
    memory: Vec<u8>,
    frames: Vec<[u8; WRAM_SIZE]>,
    save_ram_range: Option<Range<usize>>,
    save_ram_frames: Vec<u8>,
}

impl JsMachine {
    pub fn new(core: u32, memory_bytes: usize) -> Result<Self, MachineError> {
        core_reset(core).map_err(backend)?;
        let state_len = core_capture(core).map_err(backend)?.len();
        let memory = core_ram(core).map_err(backend)?;
        if memory.len() != memory_bytes {
            return Err(MachineError::Backend(
                "QuickNES returned the wrong amount of memory".to_owned(),
            ));
        }
        Ok(Self {
            core,
            state_len,
            memory_bytes,
            snapshots: BTreeMap::new(),
            next: 0,
            staged: VecDeque::new(),
            vtime: 0,
            memory,
            frames: Vec::new(),
            save_ram_range: None,
            save_ram_frames: Vec::new(),
        })
    }

    fn hold(&mut self, state: Vec<u8>) -> SnapId {
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        self.snapshots.insert(id, state);
        SnapId(id)
    }

    fn restore(&mut self, snap: SnapId) -> Result<(), MachineError> {
        let state = self
            .snapshots
            .get(&snap.0)
            .ok_or(MachineError::UnknownSnapshot)?;
        core_restore(self.core, state).map_err(backend)?;
        self.memory = core_ram(self.core).map_err(backend)?;
        self.staged.clear();
        Ok(())
    }

    fn wram(&self) -> Result<[u8; WRAM_SIZE], MachineError> {
        self.memory[..WRAM_SIZE].try_into().map_err(|_| {
            MachineError::Backend("QuickNES returned a short work RAM copy".to_owned())
        })
    }
}

impl Machine for JsMachine {
    type Portable = Vec<u8>;

    fn snapshot(&mut self) -> Result<SnapId, MachineError> {
        let state = core_capture(self.core).map_err(backend)?;
        Ok(self.hold(state))
    }

    fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.snapshots
            .remove(&snap.0)
            .map(|_| ())
            .ok_or(MachineError::UnknownSnapshot)
    }

    fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), MachineError> {
        let actions = actions_of(env)?;
        self.restore(snap)?;
        self.staged = actions.into();
        Ok(())
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.restore(snap)
    }

    fn run(
        &mut self,
        until: StopConditions,
        resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError> {
        self.frames.clear();
        self.save_ram_frames.clear();
        if resolve.is_some() {
            return Err(MachineError::ResolveWithoutDecision);
        }
        if until.deadline.is_some() {
            return Err(MachineError::Backend(
                "the browser QuickNES machine runs whole actions".to_owned(),
            ));
        }
        let size = self.memory_bytes;
        while let Some(chord) = self.staged.pop_front() {
            let hold = chord.bounded_hold_frames();
            let bytes = core_frames(self.core, chord.buttons, hold).map_err(backend)?;
            if bytes.len() != usize::from(hold) * size {
                return Err(MachineError::Backend(
                    "QuickNES returned the wrong number of frames".to_owned(),
                ));
            }
            for frame in bytes.chunks_exact(size) {
                self.memory.copy_from_slice(frame);
                self.frames.push(self.wram()?);
                if let Some(range) = &self.save_ram_range {
                    let cartridge = &frame[WRAM_SIZE..];
                    self.save_ram_frames.extend_from_slice(
                        cartridge
                            .get(range.clone())
                            .ok_or(MachineError::ReadOutOfBounds)?,
                    );
                }
                self.vtime += 1;
            }
        }
        Ok(StopReason::Quiescent {
            vtime: Moment(self.vtime),
        })
    }

    fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError> {
        let start = usize::try_from(addr).map_err(|_| MachineError::ReadOutOfBounds)?;
        let end = start
            .checked_add(usize::try_from(len).map_err(|_| MachineError::ReadOutOfBounds)?)
            .ok_or(MachineError::ReadOutOfBounds)?;
        if end > WRAM_SIZE {
            return Err(MachineError::ReadOutOfBounds);
        }
        Ok(self.memory[start..end].to_vec())
    }

    fn export(&mut self, snap: SnapId, _base: Option<&Vec<u8>>) -> Result<Vec<u8>, MachineError> {
        let state = self
            .snapshots
            .get(&snap.0)
            .ok_or(MachineError::UnknownSnapshot)?;
        pack_quicknes_state(state)
    }

    fn import(&mut self, portable: &Vec<u8>) -> Result<SnapId, MachineError> {
        let mut state = Vec::new();
        unpack_state_into(portable, self.state_len, &mut state)?;
        Ok(self.hold(state))
    }

    fn portable_memory_charge(portable: &Vec<u8>) -> usize {
        portable.len()
    }

    fn now(&self) -> Moment {
        Moment(self.vtime)
    }

    fn frames(&self) -> &[[u8; WRAM_SIZE]] {
        &self.frames
    }
}

impl NesBackend<Vec<u8>> for JsMachine {
    fn export_nes(
        &mut self,
        snapshot: SnapId,
        base: Option<&Vec<u8>>,
    ) -> Result<Vec<u8>, MachineError> {
        self.export(snapshot, base)
    }

    fn import_nes(&mut self, portable: &Vec<u8>) -> Result<SnapId, MachineError> {
        self.import(portable)
    }
}

impl NesMemory for JsMachine {
    fn read_wram(&self) -> Result<[u8; WRAM_SIZE], MachineError> {
        self.wram()
    }

    fn poke_wram(&mut self, addr: usize, byte: u8) {
        if addr < WRAM_SIZE && core_poke(self.core, addr, byte).is_ok() {
            self.memory[addr] = byte;
        }
    }

    fn capture_save_ram_per_frame(&mut self, range: Range<usize>) {
        self.save_ram_range = Some(range);
    }

    fn save_ram_frames(&self) -> &[u8] {
        &self.save_ram_frames
    }

    fn read_save_ram(&self) -> Result<Vec<u8>, MachineError> {
        Ok(self.memory[WRAM_SIZE..].to_vec())
    }

    fn write_save_ram(&mut self, _offset: usize, _bytes: &[u8]) -> Result<(), MachineError> {
        Err(MachineError::Backend(
            "the browser QuickNES machine does not write cartridge RAM".to_owned(),
        ))
    }
}

fn config<G: BrowserGame>(seed: u64, executions: u64, memory_mib: usize) -> CampaignConfig<G> {
    CampaignConfig {
        campaign_seed: seed,
        workers: WINDOW as u32,
        execution_budget: if executions == 0 {
            u64::MAX
        } else {
            executions
        },
        host: "browser".to_owned(),
        wall_budget: None,
        stop_rollout_on_objective: true,
        stop_campaign_on_objective: true,
        archive_entry_limit: MAX_ARCHIVE_ENTRIES,
        window: WINDOW,
        memory_budget_mib: (memory_mib > 0).then_some(memory_mib),
        materialize_final_artifacts: false,
        run: G::run(),
        mixture: G::mixture(),
        retention: RetentionPolicy::Unprobed,
        objective_witness_path: None,
    }
}

fn game<G: BrowserGame>(
    core: u32,
    rom: &[u8],
    identity: &str,
    identity_sha256: &str,
) -> &'static G {
    Box::leak(Box::new(G::create(core, rom, identity, identity_sha256)))
}

struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Deserialize)]
struct PrefixAction {
    buttons: u8,
    frames: u8,
}

fn root_checkpoint<G: BrowserGame>(
    game: &G,
    core: u32,
    raw: &[u8],
    frames: u64,
) -> Result<CampaignCheckpoint<G::Snapshot>, Box<dyn Error>> {
    let mut target = game.new_target()?;
    let snapshot = G::root_snapshot(&mut target, core, raw, frames)?;
    if !game.rollout_probe(&G::run(), &mut target, &snapshot)? {
        return Err("this starting point is not one the search can continue from".into());
    }
    let snapshots = SnapshotCheckpoint {
        format: game.checkpoint_format().to_owned(),
        entries: vec![SnapshotCheckpointEntry { id: 0, snapshot }],
    };
    Ok(CampaignCheckpoint {
        path: "browser-root".to_owned(),
        file_sha256: format!("{:x}", Sha256::digest(snapshots.to_bytes()?)),
        snapshots,
    })
}

trait SearchApi {
    fn plan(&mut self) -> Result<Option<Vec<u8>>, JsValue>;
    fn admit(&mut self, outcome: &[u8], note: &[u8]) -> Result<(), JsValue>;
    fn won(&self) -> bool;
    fn stopped(&self) -> bool;
    fn snapshot_bytes(&self) -> usize;
    fn take_batch(&mut self) -> Result<String, JsValue>;
    fn take_stream(&mut self) -> String;
    fn state(&self, id: u32) -> Result<String, JsValue>;
    fn digest(&self, id: u32) -> Result<String, JsValue>;
    fn snapshot(&self, id: u32) -> Result<Vec<u8>, JsValue>;
}

struct Search<G: BrowserGame> {
    game: &'static G,
    root: Option<G::Snapshot>,
    genesis_actions: usize,
    coordinator: CampaignCoordinator<'static, G>,
    stream: Rc<RefCell<Vec<u8>>>,
    recorded: Option<String>,
    prefix: Vec<ButtonChord>,
    feed: Feed<G::View>,
}

pub struct SearchOptions<'a> {
    pub core: u32,
    pub rom: &'a [u8],
    pub identity: &'a str,
    pub identity_sha256: &'a str,
    pub seed: u32,
    pub root: Option<Vec<u8>>,
    pub prefix: &'a str,
    pub memory_mib: u32,
    pub executions: u32,
    pub record: bool,
}

impl<G: BrowserGame> Search<G> {
    fn new(options: SearchOptions<'_>) -> Result<Self, Box<dyn Error>> {
        let prefix: Vec<ButtonChord> = serde_json::from_str::<Vec<PrefixAction>>(options.prefix)?
            .into_iter()
            .map(|action| ButtonChord::new(action.buttons, action.frames))
            .collect();
        let game = game::<G>(
            options.core,
            options.rom,
            options.identity,
            options.identity_sha256,
        );
        let config: &'static CampaignConfig<G> = Box::leak(Box::new(config(
            u64::from(options.seed),
            u64::from(options.executions),
            options.memory_mib as usize,
        )));
        let frames = chord_frames(&prefix);
        let (origin, raw, root, genesis_actions) = match options.root {
            None => {
                let (raw, path) = record_path(|| {
                    let mut target = game.new_target()?;
                    game.reset(&mut target);
                    core_capture(options.core).map_err(|error| backend(error).to_string())
                });
                let genesis_actions = path.map_or(0, |path| path.actions.len());
                (CampaignOrigin::Genesis, raw?, None, genesis_actions)
            }
            Some(raw) => {
                let checkpoint = root_checkpoint(game, options.core, &raw, frames)?;
                let root = checkpoint.snapshots.entries[0].snapshot.clone();
                (
                    CampaignOrigin::SnapshotRoot { checkpoint },
                    raw,
                    Some(root),
                    0,
                )
            }
        };
        let stream = Rc::new(RefCell::new(Vec::new()));
        let sink: &'static mut Sink = Box::leak(Box::new(Sink(Rc::clone(&stream))));
        let mut coordinator = CampaignCoordinator::start(
            game,
            config,
            &origin,
            sink,
            None,
            CampaignExecutionOptions::default(),
        )?;
        coordinator.watch_admissions();
        let feed = Feed::new(
            G::memory_view(&restore_ram(options.core, &raw)?)?,
            frames,
            snapshot_digest(&raw),
        );
        let mut search = Self {
            game,
            root,
            genesis_actions,
            coordinator,
            stream,
            recorded: options.record.then(String::new),
            prefix,
            feed,
        };
        search.record_stream()?;
        Ok(search)
    }

    fn record_stream(&mut self) -> Result<(), Box<dyn Error>> {
        let bytes = std::mem::take(&mut *self.stream.borrow_mut());
        if let Some(recorded) = self.recorded.as_mut() {
            recorded.push_str(std::str::from_utf8(&bytes)?);
        }
        Ok(())
    }
}

impl<G: BrowserGame> SearchApi for Search<G> {
    fn plan(&mut self) -> Result<Option<Vec<u8>>, JsValue> {
        self.coordinator
            .plan(1)
            .pop()
            .map(|job: JobSpec<G>| postcard::to_allocvec(&job).map_err(js_error))
            .transpose()
    }

    fn admit(&mut self, outcome: &[u8], note: &[u8]) -> Result<(), JsValue> {
        let outcome: JobOutcome<G> = postcard::from_bytes(outcome).map_err(js_error)?;
        let note: Note<G::View> = postcard::from_bytes(note).map_err(js_error)?;
        self.feed.note(note);
        self.coordinator.admit(outcome).map_err(js_error)?;
        self.feed.admit(self.coordinator.take_admissions());
        self.feed.forget_dropped(&self.coordinator);
        self.record_stream().map_err(js_error)
    }

    fn won(&self) -> bool {
        self.coordinator.objective_witness().is_some()
    }

    fn stopped(&self) -> bool {
        self.won() || self.coordinator.finished()
    }

    fn snapshot_bytes(&self) -> usize {
        self.coordinator.archive().resident_snapshot_bytes()
    }

    fn take_batch(&mut self) -> Result<String, JsValue> {
        let stats = feed::stats(&self.coordinator);
        serde_json::to_string(&self.feed.take_batch(stats)).map_err(js_error)
    }

    fn take_stream(&mut self) -> String {
        self.recorded
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    fn state(&self, id: u32) -> Result<String, JsValue> {
        let id = u64::from(id);
        let archive = self.coordinator.archive();
        let index = archive
            .index_of_id(id)
            .ok_or_else(|| js_error("unknown state"))?;
        let observation = self
            .feed
            .view(id)
            .ok_or_else(|| js_error("unknown state"))?;
        let input = archive.entry_input(index).map_err(js_error)?.actions;
        let state = feed::archived_state(
            id,
            observation,
            &self.prefix,
            &input,
            self.feed.frames_at(id),
            |input| feed::played(self.game, self.root.as_ref(), self.genesis_actions, input),
        )
        .map_err(js_error)?;
        serde_json::to_string(&state).map_err(js_error)
    }

    fn digest(&self, id: u32) -> Result<String, JsValue> {
        self.feed
            .digest(u64::from(id))
            .map(|digest| format!("{digest:016x}"))
            .ok_or_else(|| js_error("digest unavailable"))
    }

    fn snapshot(&self, id: u32) -> Result<Vec<u8>, JsValue> {
        let archive = self.coordinator.archive();
        let snapshot = archive
            .index_of_id(u64::from(id))
            .and_then(|index| archive.entry_snapshot(index))
            .ok_or_else(|| js_error("snapshot unavailable"))?;
        raw_state(G::emulator_state(snapshot)).map_err(js_error)
    }
}

trait WorkerApi {
    fn run(&mut self, job: &[u8]) -> Result<Vec<u8>, JsValue>;
    fn note(&mut self) -> Option<Vec<u8>>;
}

struct Worker<G: BrowserGame> {
    game: &'static G,
    config: CampaignConfig<G>,
    target: G::Target,
    note: Option<Vec<u8>>,
}

impl<G: BrowserGame> Worker<G> {
    fn new(core: u32, rom: &[u8], identity: &str, identity_sha256: &str) -> Result<Self, String> {
        let game = game::<G>(core, rom, identity, identity_sha256);
        Ok(Self {
            game,
            config: config(0, 0, 0),
            target: game.new_target()?,
            note: None,
        })
    }
}

impl<G: BrowserGame> WorkerApi for Worker<G> {
    fn run(&mut self, job: &[u8]) -> Result<Vec<u8>, JsValue> {
        let job: JobSpec<G> = postcard::from_bytes(job).map_err(js_error)?;
        let (outcome, note) = run_noted(&job, || {
            run_job(self.game, &self.config, &mut self.target, &job)
        })
        .map_err(js_error)?;
        self.note = Some(postcard::to_allocvec(&note).map_err(js_error)?);
        postcard::to_allocvec(&outcome).map_err(js_error)
    }

    fn note(&mut self) -> Option<Vec<u8>> {
        self.note.take()
    }
}

fn unknown(game: &str) -> JsValue {
    js_error(format!("{game} has no campaign search"))
}

#[wasm_bindgen]
pub struct CampaignSearch(Box<dyn SearchApi>);

#[wasm_bindgen]
impl CampaignSearch {
    #[wasm_bindgen(constructor)]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        game: &str,
        core: u32,
        rom: &[u8],
        identity: &str,
        identity_sha256: &str,
        seed: u32,
        root: Option<Vec<u8>>,
        prefix: &str,
        memory_mib: u32,
        executions: u32,
        record: bool,
    ) -> Result<CampaignSearch, JsValue> {
        let options = SearchOptions {
            core,
            rom,
            identity,
            identity_sha256,
            seed,
            root,
            prefix,
            memory_mib,
            executions,
            record,
        };
        let search: Box<dyn SearchApi> = match game {
            "twin-dragons" => Box::new(Search::<TwinDragons>::new(options).map_err(js_error)?),
            "super-mario-bros" => Box::new(Search::<Smb>::new(options).map_err(js_error)?),
            "mega-man-2" => Box::new(Search::<MegaMan>::new(options).map_err(js_error)?),
            "metroid" => Box::new(Search::<Metroid>::new(options).map_err(js_error)?),
            "famidash" => Box::new(Search::<Famidash>::new(options).map_err(js_error)?),
            _ => return Err(unknown(game)),
        };
        Ok(Self(search))
    }

    pub fn plan(&mut self) -> Result<Option<Vec<u8>>, JsValue> {
        self.0.plan()
    }

    pub fn admit(&mut self, outcome: &[u8], note: &[u8]) -> Result<(), JsValue> {
        self.0.admit(outcome, note)
    }

    pub fn won(&self) -> bool {
        self.0.won()
    }

    pub fn stopped(&self) -> bool {
        self.0.stopped()
    }

    pub fn snapshot_bytes(&self) -> usize {
        self.0.snapshot_bytes()
    }

    pub fn take_batch(&mut self) -> Result<String, JsValue> {
        self.0.take_batch()
    }

    pub fn take_stream(&mut self) -> String {
        self.0.take_stream()
    }

    pub fn state(&self, id: u32) -> Result<String, JsValue> {
        self.0.state(id)
    }

    pub fn digest(&self, id: u32) -> Result<String, JsValue> {
        self.0.digest(id)
    }

    pub fn snapshot(&self, id: u32) -> Result<Vec<u8>, JsValue> {
        self.0.snapshot(id)
    }
}

#[wasm_bindgen]
pub struct CampaignWorker(Box<dyn WorkerApi>);

#[wasm_bindgen]
impl CampaignWorker {
    #[wasm_bindgen(constructor)]
    pub fn new(
        game: &str,
        core: u32,
        rom: &[u8],
        identity: &str,
        identity_sha256: &str,
    ) -> Result<CampaignWorker, JsValue> {
        let worker: Box<dyn WorkerApi> = match game {
            "twin-dragons" => Box::new(
                Worker::<TwinDragons>::new(core, rom, identity, identity_sha256)
                    .map_err(js_error)?,
            ),
            "super-mario-bros" => Box::new(
                Worker::<Smb>::new(core, rom, identity, identity_sha256).map_err(js_error)?,
            ),
            "mega-man-2" => Box::new(
                Worker::<MegaMan>::new(core, rom, identity, identity_sha256).map_err(js_error)?,
            ),
            "metroid" => Box::new(
                Worker::<Metroid>::new(core, rom, identity, identity_sha256).map_err(js_error)?,
            ),
            "famidash" => Box::new(
                Worker::<Famidash>::new(core, rom, identity, identity_sha256).map_err(js_error)?,
            ),
            _ => return Err(unknown(game)),
        };
        Ok(Self(worker))
    }

    pub fn run(&mut self, job: &[u8]) -> Result<Vec<u8>, JsValue> {
        self.0.run(job)
    }

    pub fn note(&mut self) -> Option<Vec<u8>> {
        self.0.note()
    }
}

fn genesis_of<G: BrowserGame>(core: u32, rom: &[u8]) -> Result<String, Box<dyn Error>> {
    let game = G::create(core, rom, "genesis", "");
    let (target, path) = record_path(|| {
        game.new_target().map(|mut target| {
            game.reset(&mut target);
        })
    });
    target?;
    let path = path
        .filter(|path| path.from_power_on)
        .ok_or("the genesis state has no recorded path from power-on")?;
    Ok(serde_json::to_string(&actions(&path.actions))?)
}

#[wasm_bindgen]
pub fn campaign_genesis(game: &str, core: u32, rom: &[u8]) -> Result<String, JsValue> {
    match game {
        "twin-dragons" => genesis_of::<TwinDragons>(core, rom),
        "super-mario-bros" => genesis_of::<Smb>(core, rom),
        "mega-man-2" => genesis_of::<MegaMan>(core, rom),
        "metroid" => genesis_of::<Metroid>(core, rom),
        "famidash" => genesis_of::<Famidash>(core, rom),
        _ => return Err(unknown(game)),
    }
    .map_err(js_error)
}
