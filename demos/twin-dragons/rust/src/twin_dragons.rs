// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    error::Error,
    io::Write,
    rc::Rc,
};

use machine::{
    Answer, Machine, MachineError, Moment, Reproducer, SnapId, StopConditions, StopReason,
    nes::{ButtonChord, WRAM_SIZE, actions_of},
};
use nes_games::{
    nes_backend::{NesBackend, pack_quicknes_state, unpack_state_into},
    search::{
        archive::{MAX_ARCHIVE_ENTRIES, RetentionPolicy},
        campaign::{
            CampaignAdmissionDecision, CampaignCheckpoint, CampaignConfig, CampaignCoordinator,
            CampaignExecutionOptions, CampaignJobResult, CampaignOrigin, JobOutcome, JobSpec,
            Reporting, SnapshotCheckpoint, SnapshotCheckpointEntry, run_job,
        },
        draw::DrawMixture,
        rollout::ExecutionDisposition,
    },
    twin_dragons::{
        campaign::{TwinDragonsCampaignRun, TwinDragonsGame},
        target::{
            TwinDragonsMechanicalState, TwinDragonsObservations, TwinDragonsSnapshot,
            TwinDragonsTarget, decode_state,
        },
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wasm_bindgen::prelude::*;

use crate::snapshot_digest;

type Game = TwinDragonsGame<JsMachine>;

const WINDOW: usize = 16;
const ANIMATION: usize = 0x71;
const ANIMATION_FRAME: usize = 0x72;
const FACING: usize = 0x74;
const TRAIL_STRIDE: usize = 4;
const HIDDEN: u16 = u16::MAX;

#[wasm_bindgen(
    inline_js = "const core = (id) => globalThis.harmonyCores[id]; export function core_reset(id){core(id).reset()} export function core_frames(id,buttons,frames){return core(id).frames(buttons,frames)} export function core_capture(id){return core(id).capture()} export function core_restore(id,state){core(id).restore(state)} export function core_ram(id){return core(id).memory()}"
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
}

fn backend(error: JsValue) -> MachineError {
    MachineError::Backend(error.as_string().unwrap_or_else(|| format!("{error:?}")))
}

fn js_error(error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
}

fn ram_of(bytes: &[u8]) -> Result<[u8; WRAM_SIZE], MachineError> {
    bytes
        .try_into()
        .map_err(|_| MachineError::Backend("QuickNES returned a short work RAM copy".to_owned()))
}

#[derive(Default)]
struct Trail {
    skip: u64,
    samples: Vec<u16>,
    start_cleared: u8,
}

thread_local! {
    static TRAILS: RefCell<BTreeMap<u32, Trail>> = const { RefCell::new(BTreeMap::new()) };
    static GAMES: RefCell<BTreeMap<u32, &'static Game>> = const { RefCell::new(BTreeMap::new()) };
}

fn pose(wram: &[u8]) -> u16 {
    let pose = match (wram[ANIMATION], wram[ANIMATION_FRAME]) {
        (1, frame) => 1 + u16::from(frame.min(3)),
        (2, _) => 5,
        (3, _) => 6,
        _ => 0,
    };
    pose * 2 + u16::from(wram[FACING] != 0)
}

fn motion(wram: &[u8]) -> [u16; 4] {
    match decode_state(wram) {
        Ok(state) => match state.gameplay {
            Some(gameplay) => [
                u16::from(state.level),
                gameplay.x.max(0) as u16,
                gameplay.y.max(0) as u16,
                pose(wram),
            ],
            None => [HIDDEN, 0, 0, 0],
        },
        Err(_) => [HIDDEN, 0, 0, 0],
    }
}

struct Held {
    state: Vec<u8>,
    elapsed: u64,
    trail: usize,
}

pub struct JsMachine {
    core: u32,
    state_len: usize,
    snapshots: BTreeMap<u64, Held>,
    next: u64,
    staged: VecDeque<ButtonChord>,
    elapsed: u64,
    vtime: u64,
    ram: [u8; WRAM_SIZE],
    frames: Vec<[u8; WRAM_SIZE]>,
}

impl JsMachine {
    fn new(core: u32) -> Result<Self, MachineError> {
        core_reset(core).map_err(backend)?;
        let state_len = core_capture(core).map_err(backend)?.len();
        Ok(Self {
            core,
            state_len,
            snapshots: BTreeMap::new(),
            next: 0,
            staged: VecDeque::new(),
            elapsed: 0,
            vtime: 0,
            ram: ram_of(&core_ram(core).map_err(backend)?)?,
            frames: Vec::new(),
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
        self.ram = ram_of(&core_ram(self.core).map_err(backend)?)?;
        self.staged.clear();
        self.with_trail(|trail_state, _, _| trail_state.samples.truncate(trail));
        self.begin_trail();
        Ok(())
    }

    fn with_trail(&self, update: impl FnOnce(&mut Trail, u64, &[u8; WRAM_SIZE])) {
        TRAILS.with_borrow_mut(|trails| {
            if let Some(trail) = trails.get_mut(&self.core) {
                update(trail, self.elapsed, &self.ram);
            }
        });
    }

    fn begin_trail(&self) {
        self.with_trail(|trail, elapsed, ram| {
            if elapsed == trail.skip && trail.samples.is_empty() {
                trail.samples.push(0);
                trail.samples.extend(motion(ram));
                trail.start_cleared = decode_state(ram).map_or(0, |state| state.levels_cleared());
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
}

impl Machine for JsMachine {
    type Portable = Vec<u8>;

    fn snapshot(&mut self) -> Result<SnapId, MachineError> {
        let state = core_capture(self.core).map_err(backend)?;
        let held = Held {
            state,
            elapsed: self.elapsed,
            trail: self.trail_len(),
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
        if resolve.is_some() {
            return Err(MachineError::ResolveWithoutDecision);
        }
        if until.deadline.is_some() {
            return Err(MachineError::Backend(
                "the browser QuickNES machine runs whole actions".to_owned(),
            ));
        }
        while let Some(chord) = self.staged.pop_front() {
            self.begin_trail();
            let hold = chord.bounded_hold_frames();
            let bytes = core_frames(self.core, chord.buttons, hold).map_err(backend)?;
            if bytes.len() != usize::from(hold) * WRAM_SIZE {
                return Err(MachineError::Backend(
                    "QuickNES returned the wrong number of frames".to_owned(),
                ));
            }
            for (index, frame) in bytes.chunks_exact(WRAM_SIZE).enumerate() {
                self.ram = ram_of(frame)?;
                self.frames.push(self.ram);
                self.elapsed += 1;
                self.vtime += 1;
                if (index + 1) % TRAIL_STRIDE == 0 || index + 1 == usize::from(hold) {
                    self.with_trail(|trail, elapsed, ram| {
                        if elapsed > trail.skip && !trail.samples.is_empty() {
                            let frame = u16::try_from(elapsed - trail.skip).unwrap_or(u16::MAX);
                            trail.samples.push(frame);
                            trail.samples.extend(motion(ram));
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
        self.ram
            .get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or(MachineError::ReadOutOfBounds)
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

fn game(core: u32, rom: &[u8], identity: &str, identity_sha256: &str) -> &'static Game {
    GAMES.with_borrow_mut(|games| {
        *games.entry(core).or_insert_with(|| {
            Box::leak(Box::new(Game::with_targets(
                rom,
                identity,
                identity_sha256,
                Box::new(move || {
                    TwinDragonsTarget::from_power_on(
                        JsMachine::new(core).map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())
                }),
            )))
        })
    })
}

fn config(seed: u64, executions: u64, memory_mib: usize) -> CampaignConfig<Game> {
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
        run: TwinDragonsCampaignRun,
        mixture: DrawMixture::AlphabetOnly,
        retention: RetentionPolicy::Unprobed,
        objective_witness_path: None,
    }
}

#[derive(Clone, Copy, Serialize)]
struct View {
    x: i16,
    y: i16,
    health: u8,
    level: u8,
    next_level: u8,
    gamestate: u8,
    goal: bool,
    on_ground: bool,
    facing: u8,
    loading: bool,
    cleared: u8,
}

fn view(state: TwinDragonsMechanicalState) -> View {
    let gameplay = state.gameplay.unwrap_or_default();
    View {
        x: gameplay.x,
        y: gameplay.y,
        health: gameplay.health,
        level: state.level,
        next_level: state.next_level,
        gamestate: state.game_state,
        goal: state.goal,
        on_ground: gameplay.on_ground,
        facing: gameplay.facing,
        loading: !state.playing() || !state.loaded,
        cleared: state.levels_cleared(),
    }
}

#[wasm_bindgen]
pub fn twin_dragons_observation(ram: &[u8]) -> Result<String, JsValue> {
    if ram.len() != WRAM_SIZE {
        return Err(js_error("Twin Dragons observations need 2 KiB of work RAM"));
    }
    serde_json::to_string(&view(decode_state(ram).map_err(js_error)?)).map_err(js_error)
}

#[wasm_bindgen]
pub fn twin_dragons_pose(ram: &[u8]) -> Result<u16, JsValue> {
    if ram.len() != WRAM_SIZE {
        return Err(js_error("Twin Dragons poses need 2 KiB of work RAM"));
    }
    Ok(pose(ram))
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

struct Step {
    view: View,
    frame: u64,
    digest: Option<u64>,
    objective: bool,
}

#[derive(Serialize)]
struct Point {
    observation: View,
    retained: Option<u64>,
    frame: u64,
}

#[derive(Serialize)]
struct Batch {
    executions: u64,
    states: usize,
    deaths: u64,
    frames: u64,
    snapshot_bytes: usize,
    stopped: bool,
    won: bool,
    points: Vec<Point>,
}

#[derive(Serialize)]
struct Action {
    buttons: u8,
    frames: u8,
}

#[derive(Serialize)]
struct State {
    id: u64,
    observation: View,
    actions: Vec<Action>,
    frames: u64,
}

fn steps(result: &CampaignJobResult<Game>) -> Result<Vec<Step>, MachineError> {
    let mut steps = Vec::with_capacity(result.actions.len());
    for action in &result.actions {
        let Some(last) = action.observations.last() else {
            continue;
        };
        let digest = match &action.candidate {
            Some(candidate) => Some(snapshot_digest(&raw_state(&candidate.snapshot)?)),
            None => None,
        };
        steps.push(Step {
            view: view(last.decoded),
            frame: last.frame_count,
            digest,
            objective: action.outcome.objective_reached,
        });
    }
    Ok(steps)
}

fn raw_state(snapshot: &TwinDragonsSnapshot) -> Result<Vec<u8>, MachineError> {
    let (declared, _) = lz4_size(&snapshot.emulator_state)?;
    let mut state = Vec::new();
    unpack_state_into(&snapshot.emulator_state, declared, &mut state)?;
    Ok(state)
}

fn lz4_size(packed: &[u8]) -> Result<(usize, &[u8]), MachineError> {
    let declared = packed
        .get(..4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| MachineError::Backend("snapshot has no size prefix".to_owned()))?;
    Ok((declared as usize, &packed[4..]))
}

#[wasm_bindgen]
pub struct TwinDragonsSearch {
    coordinator: CampaignCoordinator<'static, Game>,
    stream: Rc<RefCell<Vec<u8>>>,
    recorded: Option<String>,
    prefix: Vec<ButtonChord>,
    pending: BTreeMap<usize, Vec<Step>>,
    views: BTreeMap<u64, View>,
    digests: BTreeMap<u64, u64>,
    points: Vec<Point>,
    frames: u64,
    deaths: u64,
}

#[wasm_bindgen]
impl TwinDragonsSearch {
    #[wasm_bindgen(constructor)]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
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
    ) -> Result<TwinDragonsSearch, JsValue> {
        let prefix: Vec<ButtonChord> = serde_json::from_str::<Vec<PrefixAction>>(prefix)
            .map_err(js_error)?
            .into_iter()
            .map(|action| ButtonChord::new(action.buttons, action.frames))
            .collect();
        let game = game(core, rom, identity, identity_sha256);
        let config: &'static CampaignConfig<Game> = Box::leak(Box::new(config(
            u64::from(seed),
            u64::from(executions),
            memory_mib as usize,
        )));
        let origin = match root {
            None => CampaignOrigin::Genesis,
            Some(raw) => {
                let frames = prefix
                    .iter()
                    .map(|action| u64::from(action.bounded_hold_frames()))
                    .sum();
                CampaignOrigin::SnapshotRoot {
                    checkpoint: root_checkpoint(game, core, &raw, frames).map_err(js_error)?,
                }
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
        )
        .map_err(js_error)?;
        let mut search = Self {
            coordinator,
            stream,
            recorded: record.then(String::new),
            prefix,
            pending: BTreeMap::new(),
            views: BTreeMap::new(),
            digests: BTreeMap::new(),
            points: Vec::new(),
            frames: 0,
            deaths: 0,
        };
        search.drain_header();
        let root = search
            .coordinator
            .archive()
            .entry_snapshot(0)
            .ok_or_else(|| js_error("the search root has no snapshot"))?;
        let raw = raw_state(root).map_err(js_error)?;
        search.views.insert(0, view(root.observation.decoded));
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

    pub fn plan(&mut self) -> Result<Option<Vec<u8>>, JsValue> {
        self.coordinator
            .plan(1)
            .pop()
            .map(|job: JobSpec<Game>| postcard::to_allocvec(&job).map_err(js_error))
            .transpose()
    }

    pub fn admit(&mut self, outcome: &[u8]) -> Result<Vec<u32>, JsValue> {
        let outcome: JobOutcome<Game> = postcard::from_bytes(outcome).map_err(js_error)?;
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

    pub fn won(&self) -> bool {
        self.coordinator.objective_witness().is_some()
    }

    pub fn stopped(&self) -> bool {
        self.won() || self.coordinator.finished()
    }

    pub fn snapshot_bytes(&self) -> usize {
        self.coordinator.archive().resident_snapshot_bytes()
    }

    pub fn take_batch(&mut self) -> Result<String, JsValue> {
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

    pub fn take_stream(&mut self) -> String {
        self.recorded
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    pub fn state(&self, id: u32) -> Result<String, JsValue> {
        let id = u64::from(id);
        let archive = self.coordinator.archive();
        let index = archive
            .index_of_id(id)
            .ok_or_else(|| js_error("unknown state"))?;
        let observation = *self
            .views
            .get(&id)
            .ok_or_else(|| js_error("unknown state"))?;
        let actions: Vec<Action> = self
            .prefix
            .iter()
            .chain(archive.entry_input(index).map_err(js_error)?.actions.iter())
            .map(|action| Action {
                buttons: action.buttons,
                frames: action.bounded_hold_frames(),
            })
            .collect();
        let frames = actions.iter().map(|action| u64::from(action.frames)).sum();
        serde_json::to_string(&State {
            id,
            observation,
            actions,
            frames,
        })
        .map_err(js_error)
    }

    pub fn digest(&self, id: u32) -> Result<String, JsValue> {
        self.digests
            .get(&u64::from(id))
            .map(|digest| format!("{digest:016x}"))
            .ok_or_else(|| js_error("digest unavailable"))
    }

    pub fn snapshot(&self, id: u32) -> Result<Vec<u8>, JsValue> {
        let archive = self.coordinator.archive();
        let snapshot = archive
            .index_of_id(u64::from(id))
            .and_then(|index| archive.entry_snapshot(index))
            .ok_or_else(|| js_error("snapshot unavailable"))?;
        raw_state(snapshot).map_err(js_error)
    }
}

#[derive(Deserialize)]
struct PrefixAction {
    buttons: u8,
    frames: u8,
}

fn root_checkpoint(
    game: &Game,
    core: u32,
    raw: &[u8],
    frames: u64,
) -> Result<CampaignCheckpoint<TwinDragonsSnapshot>, Box<dyn Error>> {
    core_restore(core, raw).map_err(backend)?;
    let decoded = decode_state(&core_ram(core).map_err(backend)?)?;
    let snapshots = SnapshotCheckpoint {
        format: game.checkpoint_format().to_owned(),
        entries: vec![SnapshotCheckpointEntry {
            id: 0,
            snapshot: TwinDragonsSnapshot {
                emulator_state: pack_quicknes_state(raw)?,
                observation: TwinDragonsObservations {
                    frame_count: frames,
                    decoded,
                    terminal: decoded.terminal(),
                },
                failed: false,
            },
        }],
    };
    Ok(CampaignCheckpoint {
        path: "browser-root".to_owned(),
        file_sha256: format!("{:x}", Sha256::digest(snapshots.to_bytes()?)),
        snapshots,
    })
}

#[wasm_bindgen]
pub struct TwinDragonsWorker {
    core: u32,
    game: &'static Game,
    config: CampaignConfig<Game>,
    target: TwinDragonsTarget<JsMachine>,
    trail: Option<Vec<u16>>,
    clears: bool,
    reservation: u32,
}

#[wasm_bindgen]
impl TwinDragonsWorker {
    #[wasm_bindgen(constructor)]
    pub fn new(
        core: u32,
        rom: &[u8],
        identity: &str,
        identity_sha256: &str,
    ) -> Result<TwinDragonsWorker, JsValue> {
        let game = game(core, rom, identity, identity_sha256);
        Ok(Self {
            core,
            game,
            config: config(0, 0, 0),
            target: nes_games::search::campaign::TargetExecution::new_target(game)
                .map_err(js_error)?,
            trail: None,
            clears: false,
            reservation: 0,
        })
    }

    pub fn run(&mut self, job: &[u8]) -> Result<Vec<u8>, JsValue> {
        let job: JobSpec<Game> = postcard::from_bytes(job).map_err(js_error)?;
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
        let cleared = outcome
            .result
            .actions
            .iter()
            .flat_map(|action| action.observations.last())
            .map(|observation| observation.decoded.levels_cleared())
            .max()
            .unwrap_or(0);
        self.clears = cleared > trail.start_cleared;
        self.trail = (trail.samples.len() > 5).then_some(trail.samples);
        postcard::to_allocvec(&outcome).map_err(js_error)
    }

    pub fn trail(&mut self) -> Option<Vec<u16>> {
        self.trail.take()
    }

    pub fn reservation(&self) -> u32 {
        self.reservation
    }

    pub fn clears(&self) -> bool {
        self.clears
    }
}
