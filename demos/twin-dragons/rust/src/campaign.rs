// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    error::Error,
    io::Write,
    ops::Range,
    rc::Rc,
};

use machine::{
    Answer, Machine, MachineError, Moment, Reproducer, SnapId, StopConditions, StopReason,
    nes::{ButtonChord, WRAM_SIZE, actions_of},
};
use nes_games::{
    nes_backend::{NesBackend, NesMemory, pack_quicknes_state, unpack_state_into},
    search::{
        archive::{MAX_ARCHIVE_ENTRIES, RetentionPolicy},
        campaign::{
            CampaignAdmissionDecision, CampaignCheckpoint, CampaignConfig, CampaignCoordinator,
            CampaignExecutionOptions, CampaignJobResult, CampaignOrigin, JobOutcome, JobSpec,
            SnapshotCheckpoint, SnapshotCheckpointEntry, Workload, run_job,
        },
        draw::DrawMixture,
        rollout::ExecutionDisposition,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wasm_bindgen::prelude::*;

use crate::{
    famidash::Famidash, mega_man::MegaMan, metroid::Metroid, smb::Smb, snapshot_digest,
    twin_dragons::TwinDragons,
};

const WINDOW: usize = 16;
const TRAIL_STRIDE: usize = 4;
pub const HIDDEN: u16 = u16::MAX;

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

pub trait BrowserGame:
    Workload<Action = ButtonChord, ArchiveReport: Serialize> + Sized + 'static
{
    type View: Serialize + Copy;

    const MEMORY_BYTES: usize = WRAM_SIZE;

    fn create(core: u32, rom: &[u8], identity: &str, identity_sha256: &str) -> Self;

    fn run() -> Self::Run;

    fn mixture() -> DrawMixture;

    fn motion(memory: &[u8]) -> [u16; 4];

    fn memory_cleared(memory: &[u8]) -> u8;

    fn memory_view(memory: &[u8]) -> Result<Self::View, Box<dyn Error>>;

    fn view(observation: &Self::Observations) -> Self::View;

    fn frame_count(observation: &Self::Observations) -> u64;

    fn cleared(observation: &Self::Observations) -> u8;

    fn emulator_state(snapshot: &Self::Snapshot) -> &[u8];

    fn root_snapshot(
        target: &mut Self::Target,
        core: u32,
        raw: &[u8],
        frames: u64,
    ) -> Result<Self::Snapshot, Box<dyn Error>>;
}

#[derive(Default)]
struct Trail {
    skip: u64,
    samples: Vec<u16>,
    start_cleared: u8,
}

thread_local! {
    static TRAILS: RefCell<BTreeMap<u32, Trail>> = const { RefCell::new(BTreeMap::new()) };
    static RECORDED_PATH: RefCell<Option<Option<RecordedPath>>> = const { RefCell::new(None) };
}

struct RecordedPath {
    actions: Vec<ButtonChord>,
    from_power_on: bool,
}

fn recording<T>(run: impl FnOnce() -> T) -> (T, Option<RecordedPath>) {
    RECORDED_PATH.set(Some(None));
    let value = run();
    (value, RECORDED_PATH.take().flatten())
}

struct Held {
    state: Vec<u8>,
    elapsed: u64,
    trail: usize,
    path: Option<(usize, bool)>,
}

pub struct JsMachine {
    core: u32,
    state_len: usize,
    memory_bytes: usize,
    motion: fn(&[u8]) -> [u16; 4],
    cleared: fn(&[u8]) -> u8,
    snapshots: BTreeMap<u64, Held>,
    next: u64,
    staged: VecDeque<ButtonChord>,
    elapsed: u64,
    vtime: u64,
    memory: Vec<u8>,
    frames: Vec<[u8; WRAM_SIZE]>,
    save_ram_range: Option<Range<usize>>,
    save_ram_frames: Vec<u8>,
    path: Option<RecordedPath>,
}

impl JsMachine {
    pub fn new<G: BrowserGame>(core: u32) -> Result<Self, MachineError> {
        core_reset(core).map_err(backend)?;
        let state_len = core_capture(core).map_err(backend)?.len();
        let memory = core_ram(core).map_err(backend)?;
        if memory.len() != G::MEMORY_BYTES {
            return Err(MachineError::Backend(
                "QuickNES returned the wrong amount of memory".to_owned(),
            ));
        }
        Ok(Self {
            core,
            state_len,
            memory_bytes: G::MEMORY_BYTES,
            motion: G::motion,
            cleared: G::memory_cleared,
            snapshots: BTreeMap::new(),
            next: 0,
            staged: VecDeque::new(),
            elapsed: 0,
            vtime: 0,
            memory,
            frames: Vec::new(),
            save_ram_range: None,
            save_ram_frames: Vec::new(),
            path: RECORDED_PATH
                .with_borrow(Option::is_some)
                .then(|| RecordedPath {
                    actions: Vec::new(),
                    from_power_on: true,
                }),
        })
    }

    fn hold(&mut self, held: Held) -> SnapId {
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        self.snapshots.insert(id, held);
        SnapId(id)
    }

    fn restore(&mut self, snap: SnapId) -> Result<(), MachineError> {
        let held = self
            .snapshots
            .get(&snap.0)
            .ok_or(MachineError::UnknownSnapshot)?;
        core_restore(self.core, &held.state).map_err(backend)?;
        self.elapsed = held.elapsed;
        let trail = held.trail;
        if let Some(path) = self.path.as_mut() {
            let (len, from_power_on) = held.path.unwrap_or((0, false));
            path.actions.truncate(len);
            path.from_power_on = from_power_on;
        }
        self.memory = core_ram(self.core).map_err(backend)?;
        self.staged.clear();
        self.with_trail(|trail_state, _, _| trail_state.samples.truncate(trail));
        self.begin_trail();
        Ok(())
    }

    fn with_trail(&self, update: impl FnOnce(&mut Trail, u64, &[u8])) {
        TRAILS.with_borrow_mut(|trails| {
            if let Some(trail) = trails.get_mut(&self.core) {
                update(trail, self.elapsed, &self.memory);
            }
        });
    }

    fn begin_trail(&self) {
        let (motion, cleared) = (self.motion, self.cleared);
        self.with_trail(|trail, elapsed, memory| {
            if elapsed == trail.skip && trail.samples.is_empty() {
                trail.samples.push(0);
                trail.samples.extend(motion(memory));
                trail.start_cleared = cleared(memory);
            }
        });
    }

    fn trail_len(&self) -> usize {
        TRAILS.with_borrow(|trails| {
            trails
                .get(&self.core)
                .map_or(0, |trail| trail.samples.len())
        })
    }

    fn wram(&self) -> Result<[u8; WRAM_SIZE], MachineError> {
        self.memory[..WRAM_SIZE].try_into().map_err(|_| {
            MachineError::Backend("QuickNES returned a short work RAM copy".to_owned())
        })
    }
}

impl Drop for JsMachine {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            RECORDED_PATH.set(Some(Some(path)));
        }
    }
}

impl Machine for JsMachine {
    type Portable = Vec<u8>;

    fn snapshot(&mut self) -> Result<SnapId, MachineError> {
        let state = core_capture(self.core).map_err(backend)?;
        let held = Held {
            state,
            elapsed: self.elapsed,
            trail: self.trail_len(),
            path: self
                .path
                .as_ref()
                .map(|path| (path.actions.len(), path.from_power_on)),
        };
        Ok(self.hold(held))
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
        let (motion, size) = (self.motion, self.memory_bytes);
        while let Some(chord) = self.staged.pop_front() {
            self.begin_trail();
            let hold = chord.bounded_hold_frames();
            if let Some(path) = self.path.as_mut() {
                path.actions.push(ButtonChord::new(chord.buttons, hold));
            }
            let bytes = core_frames(self.core, chord.buttons, hold).map_err(backend)?;
            if bytes.len() != usize::from(hold) * size {
                return Err(MachineError::Backend(
                    "QuickNES returned the wrong number of frames".to_owned(),
                ));
            }
            for (index, frame) in bytes.chunks_exact(size).enumerate() {
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
                self.elapsed += 1;
                self.vtime += 1;
                if (index + 1) % TRAIL_STRIDE == 0 || index + 1 == usize::from(hold) {
                    self.with_trail(|trail, elapsed, memory| {
                        if elapsed > trail.skip && !trail.samples.is_empty() {
                            let frame = u16::try_from(elapsed - trail.skip).unwrap_or(u16::MAX);
                            trail.samples.push(frame);
                            trail.samples.extend(motion(memory));
                        }
                    });
                }
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
        let held = self
            .snapshots
            .get(&snap.0)
            .ok_or(MachineError::UnknownSnapshot)?;
        pack_quicknes_state(&held.state)
    }

    fn import(&mut self, portable: &Vec<u8>) -> Result<SnapId, MachineError> {
        let mut state = Vec::new();
        unpack_state_into(portable, self.state_len, &mut state)?;
        Ok(self.hold(Held {
            state,
            elapsed: 0,
            trail: 0,
            path: None,
        }))
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
#[serde(tag = "event", rename_all = "snake_case")]
enum Line {
    Job {
        reservation: usize,
        decisions: Vec<CampaignAdmissionDecision>,
    },
    Skip {},
}

struct Step<V> {
    view: V,
    frame: u64,
    digest: Option<u64>,
    objective: bool,
}

#[derive(Serialize)]
struct Point<V> {
    observation: V,
    retained: Option<u64>,
    frame: u64,
}

#[derive(Serialize)]
struct Batch<V> {
    executions: u64,
    states: usize,
    deaths: u64,
    frames: u64,
    snapshot_bytes: usize,
    stopped: bool,
    won: bool,
    points: Vec<Point<V>>,
}

#[derive(Serialize)]
pub(crate) struct Action {
    buttons: u8,
    frames: u8,
}

#[derive(Serialize)]
struct State<V> {
    id: u64,
    observation: V,
    actions: Vec<Action>,
    frames: u64,
}

#[derive(Deserialize)]
struct PrefixAction {
    buttons: u8,
    frames: u8,
}

fn steps<G: BrowserGame>(
    result: &CampaignJobResult<G>,
) -> Result<Vec<Step<G::View>>, MachineError> {
    let mut steps = Vec::with_capacity(result.actions.len());
    for action in &result.actions {
        let Some(last) = action.observations.last() else {
            continue;
        };
        let digest = match &action.candidate {
            Some(candidate) => Some(snapshot_digest(&raw_state(G::emulator_state(
                &candidate.snapshot,
            ))?)),
            None => None,
        };
        steps.push(Step {
            view: G::view(last),
            frame: G::frame_count(last),
            digest,
            objective: action.outcome.objective_reached,
        });
    }
    Ok(steps)
}

pub(crate) fn actions(chords: &[ButtonChord]) -> Vec<Action> {
    chords
        .iter()
        .map(|chord| Action {
            buttons: chord.buttons,
            frames: chord.bounded_hold_frames(),
        })
        .collect()
}

fn raw_state(packed: &[u8]) -> Result<Vec<u8>, MachineError> {
    let declared = packed
        .get(..4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| MachineError::Backend("snapshot has no size prefix".to_owned()))?;
    let mut state = Vec::new();
    unpack_state_into(packed, declared as usize, &mut state)?;
    Ok(state)
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
    fn admit(&mut self, outcome: &[u8]) -> Result<Vec<u32>, JsValue>;
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
    pending: BTreeMap<usize, Vec<Step<G::View>>>,
    views: BTreeMap<u64, G::View>,
    frames_at: BTreeMap<u64, u64>,
    digests: BTreeMap<u64, u64>,
    points: Vec<Point<G::View>>,
    frames: u64,
    deaths: u64,
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
        let frames: u64 = prefix
            .iter()
            .map(|action| u64::from(action.bounded_hold_frames()))
            .sum();
        let (origin, raw, root, genesis_actions) = match options.root {
            None => {
                let (raw, path) = recording(|| {
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
        let coordinator = CampaignCoordinator::start(
            game,
            config,
            &origin,
            sink,
            None,
            CampaignExecutionOptions::default(),
        )?;
        let mut search = Self {
            game,
            root,
            genesis_actions,
            coordinator,
            stream,
            recorded: options.record.then(String::new),
            prefix,
            pending: BTreeMap::new(),
            views: BTreeMap::new(),
            frames_at: BTreeMap::from([(0, frames)]),
            digests: BTreeMap::new(),
            points: Vec::new(),
            frames: 0,
            deaths: 0,
        };
        search.drain_header();
        search
            .views
            .insert(0, G::memory_view(&restore_ram(options.core, &raw)?)?);
        search.digests.insert(0, snapshot_digest(&raw));
        Ok(search)
    }

    fn drain_header(&mut self) {
        let mut stream = self.stream.borrow_mut();
        if let Some(end) = stream.iter().position(|&byte| byte == b'\n') {
            let header: Vec<u8> = stream.drain(..=end).collect();
            if let Some(recorded) = self.recorded.as_mut() {
                recorded.push_str(&String::from_utf8_lossy(&header));
            }
        }
    }

    fn read_stream(&mut self) -> Result<Vec<u32>, Box<dyn Error>> {
        let mut admitted = Vec::new();
        let bytes = std::mem::take(&mut *self.stream.borrow_mut());
        if let Some(recorded) = self.recorded.as_mut() {
            recorded.push_str(std::str::from_utf8(&bytes)?);
        }
        for line in bytes
            .split(|&byte| byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let Line::Job {
                reservation,
                decisions,
            } = serde_json::from_slice(line)?
            else {
                continue;
            };
            admitted.push(u32::try_from(reservation)?);
            let steps = self
                .pending
                .remove(&reservation)
                .ok_or("the search admitted a job it did not receive")?;
            let mut decisions = decisions.into_iter();
            for step in steps {
                if step.objective {
                    decisions.next();
                }
                let retained = if step.digest.is_some() {
                    match decisions.next() {
                        Some(
                            CampaignAdmissionDecision::Retained { id }
                            | CampaignAdmissionDecision::Duplicate { id },
                        ) => Some(id),
                        _ => None,
                    }
                } else {
                    None
                };
                if let (Some(id), Some(digest)) = (retained, step.digest)
                    && !self.views.contains_key(&id)
                {
                    self.views.insert(id, step.view);
                    self.frames_at.insert(id, step.frame);
                    self.digests.insert(id, digest);
                }
                self.points.push(Point {
                    observation: step.view,
                    retained,
                    frame: step.frame,
                });
            }
        }
        Ok(admitted)
    }
}

impl<G: BrowserGame> Search<G> {
    fn played(&self, input: &[ButtonChord]) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
        let (played, path) = recording(|| -> Result<(), Box<dyn Error>> {
            let mut target = self.game.new_target()?;
            match &self.root {
                Some(root) => self.game.restore(&mut target, root)?,
                None => self.game.reset(&mut target),
            }
            let mut milestones = G::Milestones::default();
            for action in input {
                self.game
                    .apply_action(&mut target, action, &mut milestones)?;
                if self.game.execution_disposition(&target).is_terminal() {
                    break;
                }
            }
            Ok(())
        });
        played?;
        let path = path.ok_or("the replay recorded no controller path")?;
        let skip = if path.from_power_on {
            self.genesis_actions
        } else {
            0
        };
        Ok(path
            .actions
            .get(skip..)
            .ok_or("the replay path is shorter than its starting point")?
            .to_vec())
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

    fn admit(&mut self, outcome: &[u8]) -> Result<Vec<u32>, JsValue> {
        let outcome: JobOutcome<G> = postcard::from_bytes(outcome).map_err(js_error)?;
        self.frames = self.frames.saturating_add(outcome.execution_work);
        if outcome.result.actions.last().is_some_and(|action| {
            action.outcome.disposition == ExecutionDisposition::Terminal
                && !action.outcome.objective_reached
        }) {
            self.deaths += 1;
        }
        self.pending.insert(
            outcome.reservation,
            steps(&outcome.result).map_err(js_error)?,
        );
        self.coordinator.admit(outcome).map_err(js_error)?;
        self.read_stream().map_err(js_error)
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
        serde_json::to_string(&Batch {
            executions: self.coordinator.executions(),
            states: self.coordinator.archive().active_count(),
            deaths: self.deaths,
            frames: self.frames,
            snapshot_bytes: self.snapshot_bytes(),
            stopped: self.stopped(),
            won: self.won(),
            points: std::mem::take(&mut self.points),
        })
        .map_err(js_error)
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
        let observation = *self
            .views
            .get(&id)
            .ok_or_else(|| js_error("unknown state"))?;
        let input = archive.entry_input(index).map_err(js_error)?.actions;
        let mut chords = self.prefix.clone();
        chords.extend(&input);
        let sum = |chords: &[ButtonChord]| -> u64 {
            chords
                .iter()
                .map(|chord| u64::from(chord.bounded_hold_frames()))
                .sum()
        };
        if self
            .frames_at
            .get(&id)
            .is_some_and(|&frames| frames != sum(&chords))
        {
            chords.truncate(self.prefix.len());
            chords.extend(self.played(&input).map_err(js_error)?);
        }
        let frames = sum(&chords);
        let actions = actions(&chords);
        serde_json::to_string(&State {
            id,
            observation,
            actions,
            frames,
        })
        .map_err(js_error)
    }

    fn digest(&self, id: u32) -> Result<String, JsValue> {
        self.digests
            .get(&u64::from(id))
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
    fn trail(&mut self) -> Option<Vec<u16>>;
    fn reservation(&self) -> u32;
    fn next_level(&self) -> u8;
}

struct Worker<G: BrowserGame> {
    core: u32,
    game: &'static G,
    config: CampaignConfig<G>,
    target: G::Target,
    trail: Option<Vec<u16>>,
    next_level: u8,
    reservation: u32,
}

impl<G: BrowserGame> Worker<G> {
    fn new(core: u32, rom: &[u8], identity: &str, identity_sha256: &str) -> Result<Self, String> {
        let game = game::<G>(core, rom, identity, identity_sha256);
        Ok(Self {
            core,
            game,
            config: config(0, 0, 0),
            target: game.new_target()?,
            trail: None,
            next_level: 0,
            reservation: 0,
        })
    }
}

impl<G: BrowserGame> WorkerApi for Worker<G> {
    fn run(&mut self, job: &[u8]) -> Result<Vec<u8>, JsValue> {
        let job: JobSpec<G> = postcard::from_bytes(job).map_err(js_error)?;
        self.reservation = u32::try_from(job.reservation).map_err(js_error)?;
        let skip = job
            .replay
            .iter()
            .map(|action| u64::from(action.bounded_hold_frames()))
            .sum();
        TRAILS.with_borrow_mut(|trails| {
            trails.insert(
                self.core,
                Trail {
                    skip,
                    ..Trail::default()
                },
            )
        });
        let outcome = run_job(self.game, &self.config, &mut self.target, &job);
        let trail = TRAILS
            .with_borrow_mut(|trails| trails.remove(&self.core))
            .unwrap_or_default();
        let outcome = outcome.map_err(js_error)?;
        self.next_level = outcome
            .result
            .actions
            .iter()
            .flat_map(|action| action.observations.last())
            .map(G::cleared)
            .find(|&cleared| cleared > trail.start_cleared)
            .unwrap_or(0);
        self.trail = (trail.samples.len() > 5).then_some(trail.samples);
        postcard::to_allocvec(&outcome).map_err(js_error)
    }

    fn trail(&mut self) -> Option<Vec<u16>> {
        self.trail.take()
    }

    fn reservation(&self) -> u32 {
        self.reservation
    }

    fn next_level(&self) -> u8 {
        self.next_level
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

    pub fn admit(&mut self, outcome: &[u8]) -> Result<Vec<u32>, JsValue> {
        self.0.admit(outcome)
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

    pub fn trail(&mut self) -> Option<Vec<u16>> {
        self.0.trail()
    }

    pub fn reservation(&self) -> u32 {
        self.0.reservation()
    }

    pub fn next_level(&self) -> u8 {
        self.0.next_level()
    }
}

fn genesis_of<G: BrowserGame>(core: u32, rom: &[u8]) -> Result<String, Box<dyn Error>> {
    let game = G::create(core, rom, "genesis", "");
    let (target, path) = recording(|| {
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
