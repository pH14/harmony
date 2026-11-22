// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(unix)]
use std::io::Write;
use std::{error::Error, ops::Range, sync::Arc};

#[cfg(unix)]
use machine::nes::MAX_HOLD_FRAMES;
#[cfg(unix)]
use machine::quicknes::{QUICKNES_AUDIO_CHANNELS, QUICKNES_AUDIO_SAMPLE_RATE};
use machine::{
    Machine, MachineError, SnapId, StopConditions,
    nes::{self, ButtonChord, SAVE_RAM_BASE, WRAM_SIZE},
};
use serde::{Deserialize, Serialize};

use super::{Game, Lowered, Satisfied};
#[cfg(unix)]
use crate::nes_backend::FilmMachine;
use crate::{
    nes_backend::{NesBackend, NesMemory, SnapshotState, WorkRamCopy},
    search::{
        assertion::{Accumulator, AssertionKey, Record},
        choice::ChoiceRecord,
    },
    target::{ExitKind, Target},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NesObservation<S> {
    pub frame_count: u64,
    pub state: S,
    pub key: Option<AssertionKey>,
    pub rejected: Option<u8>,
    pub goal: bool,
    pub satisfied: Satisfied,
    pub overran: bool,
}

impl<S> NesObservation<S> {
    #[must_use]
    pub fn stops(&self) -> bool {
        self.rejected.is_some() || self.goal || self.overran
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActionObservation<S> {
    pub observation: NesObservation<S>,
    pub cost: u64,
    #[serde(skip)]
    pub records: Vec<Record>,
    #[serde(skip)]
    pub choices: Vec<ChoiceRecord>,
    #[serde(skip)]
    pub edited: Option<ButtonChord>,
}

impl<S> From<NesObservation<S>> for ActionObservation<S> {
    fn from(observation: NesObservation<S>) -> Self {
        Self {
            observation,
            cost: 0,
            records: Vec::new(),
            choices: Vec::new(),
            edited: None,
        }
    }
}

struct Played<M> {
    frames: u64,
    frame: Vec<u8>,
    forced: Option<M>,
}

type Tracked<'a, S> = (&'a S, &'a mut Accumulator, &'a mut NesObservation<S>);

enum Walked<S, M> {
    Arrived {
        frames: u64,
        frame: Vec<u8>,
        observation: NesObservation<S>,
        accumulator: Accumulator,
        forced: Option<M>,
    },
    Overran {
        frames: u64,
        frame: Vec<u8>,
        state: S,
    },
}

fn walk_move<G: Game, E>(
    lowered: &Lowered<G>,
    (mut mv, offered): (G::Move, bool),
    (frame_count, parent, mut state): (u64, &G::State, G::State),
    accumulator: &Accumulator,
    mut frame: Vec<u8>,
    mut press: impl FnMut(ButtonChord) -> Result<(u64, Vec<u8>), E>,
    mut seen: impl FnMut(Record),
) -> Result<Walked<G::State, G::Move>, E> {
    let bound = lowered.game.move_frames(mv);
    let mut frames = 0_u64;
    let mut arrival = None;
    while let Some(step) = lowered.game.play(mv, &state) {
        let (pressed, after) = press(step.chord)?;
        frames = frames.saturating_add(pressed);
        frame = after;
        state = lowered.game.decode(Some(parent), Some(&state), &frame);
        if frames > bound {
            seen(lowered.frame_record(&state, true));
            return Ok(Walked::Overran {
                frames,
                frame,
                state,
            });
        }
        let last_step = step
            .rest
            .is_none_or(|next| lowered.game.play(next, &state).is_none());
        let mut advanced = accumulator.clone();
        let framed = lowered.observe_ending(
            frame_count.saturating_add(frames),
            state.clone(),
            &mut advanced,
            |moves| moves.ends_move(last_step, offered),
        );
        seen(framed.record);
        let ends = framed.ends;
        arrival = Some((framed, advanced));
        match step.rest {
            Some(next) if !ends => mv = next,
            _ => break,
        }
    }
    let (framed, accumulator) = arrival.unwrap_or_else(|| {
        let mut advanced = accumulator.clone();
        let framed = lowered.observe_ending(frame_count, state, &mut advanced, |moves| {
            moves.ends_move(true, offered)
        });
        (framed, advanced)
    });
    Ok(Walked::Arrived {
        frames,
        frame,
        observation: framed.observation,
        accumulator,
        forced: framed.forced,
    })
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound(serialize = "S: Serialize", deserialize = "S: Deserialize<'de>"))]
pub struct NesSnapshot<S, P: SnapshotState = Vec<u8>> {
    pub emulator_state: P,
    pub observation: NesObservation<S>,
    pub accumulator: Accumulator,
    pub failed: bool,
    pub work_ram: P::WorkRam,
}

impl<S: Clone + Serialize, P: SnapshotState> NesSnapshot<S, P> {
    pub fn endpoint_identity_sha256(&self) -> Result<String, Box<dyn Error>> {
        crate::nes_backend::endpoint_identity_sha256(self, |snapshot| &mut snapshot.emulator_state)
    }

    pub fn logical_identity(&self) -> Result<Self, MachineError> {
        Ok(Self {
            emulator_state: self.emulator_state.logical_identity()?,
            ..self.clone()
        })
    }
}

impl<S: Clone, P: SnapshotState> searcher::search::snapshot_codec::CheckpointSnapshot
    for NesSnapshot<S, P>
where
    Self: Serialize + serde::de::DeserializeOwned,
{
    type Codec = searcher::search::snapshot_codec::PostcardSnapshotCodec<Self>;
    fn checkpoint_image_charge(
        &self,
        budget: &mut searcher::search::checkpoint::image::ImageBudget,
    ) -> Result<(), Box<dyn Error>> {
        budget.reserve(std::mem::size_of::<Self>())?;
        budget.reserve(self.accumulator.heap_bytes())?;
        self.emulator_state.checkpoint_image_charge(budget)?;
        budget.array::<u8>(self.work_ram.memory_charge())
    }
    fn freeze_checkpoint(&self) -> Result<Self, Box<dyn Error>> {
        Ok(self.clone())
    }
}

#[cfg(unix)]
pub type Filmed<S> = (VideoMetadata, NesObservation<S>);

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VideoMetadata {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub audio_sample_rate: u32,
    pub audio_channels: u8,
    pub audio_frames: u64,
    pub skipped_frames: u64,
}

pub struct NativeMemory<M> {
    pub poke_wram: fn(&mut M, usize, u8) -> Result<(), MachineError>,
    pub capture_save_ram_per_frame: fn(&mut M, Range<usize>),
    pub save_ram_frames: fn(&M) -> &[u8],
}

impl<M: NesMemory> NativeMemory<M> {
    #[must_use]
    pub fn of() -> Self {
        Self {
            poke_wram: <M as NesMemory>::poke_wram,
            capture_save_ram_per_frame: <M as NesMemory>::capture_save_ram_per_frame,
            save_ram_frames: <M as NesMemory>::save_ram_frames,
        }
    }
}

impl<M> Clone for NativeMemory<M> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M> Copy for NativeMemory<M> {}

impl<M> std::fmt::Debug for NativeMemory<M> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeMemory")
            .finish_non_exhaustive()
    }
}

pub(super) fn without_native_memory<G: Game>(need: &str) -> MachineError {
    MachineError::Backend(format!(
        "{} {need}, which only the native QuickNES backend provides",
        G::NAME
    ))
}

#[derive(Debug)]
pub struct NesTarget<G: Game, M: NesBackend<P>, P: SnapshotState = Vec<u8>> {
    lowered: Arc<Lowered<G>>,
    machine: M,
    genesis: SnapId,
    current: SnapId,
    stopped: Option<ButtonChord>,
    genesis_observation: NesObservation<G::State>,
    genesis_frame: Vec<u8>,
    current_frame: Vec<u8>,
    genesis_accumulator: Accumulator,
    accumulator_before_genesis: Accumulator,
    accumulator: Accumulator,
    observation: NesObservation<G::State>,
    action_observations: Vec<ActionObservation<G::State>>,
    failed: bool,
    snapshot_base: Option<P>,
    native_memory: Option<NativeMemory<M>>,
    execution_work: u64,
    recording: bool,
}

impl<G: Game, M: NesBackend<P>, P: SnapshotState> NesTarget<G, M, P> {
    pub fn from_machine(
        lowered: Arc<Lowered<G>>,
        mut machine: M,
        native_memory: Option<NativeMemory<M>>,
    ) -> Result<Self, MachineError> {
        if let Some(span) = save_ram_span(G::MEMORY_WINDOWS) {
            let memory = native_memory
                .ok_or_else(|| without_native_memory::<G>("reads save RAM on every frame"))?;
            (memory.capture_save_ram_per_frame)(&mut machine, span);
        }
        let frame = read_frame::<G, M>(&machine)?;
        let state = lowered.game.decode(None, None, &frame);
        lowered
            .game
            .genesis(&state)
            .map_err(MachineError::Backend)?;
        let mut accumulator = Accumulator::default();
        let observation = lowered.observe(0, state, &mut accumulator);
        let genesis = machine.snapshot()?;
        Ok(Self {
            lowered,
            machine,
            genesis,
            current: genesis,
            stopped: None,
            genesis_observation: observation.clone(),
            genesis_frame: frame.clone(),
            current_frame: frame,
            genesis_accumulator: accumulator.clone(),
            accumulator_before_genesis: Accumulator::default(),
            accumulator,
            observation: observation.clone(),
            action_observations: vec![observation.into()],
            failed: false,
            snapshot_base: None,
            native_memory,
            execution_work: 0,
            recording: false,
        })
    }

    #[must_use]
    pub fn state(&self) -> &G::State {
        &self.observation.state
    }

    #[must_use]
    pub fn observation(&self) -> &NesObservation<G::State> {
        &self.observation
    }

    #[must_use]
    pub fn genesis_accumulator(&self) -> &Accumulator {
        &self.genesis_accumulator
    }

    #[must_use]
    pub fn accumulator_before_genesis(&self) -> &Accumulator {
        &self.accumulator_before_genesis
    }

    #[must_use]
    pub fn execution_work(&self) -> u64 {
        self.execution_work
    }

    #[must_use]
    pub fn frame(&self) -> &[u8] {
        &self.current_frame
    }

    pub fn record_frames(&mut self, recording: bool) {
        self.recording = recording;
    }

    #[must_use]
    pub fn last_action_observations(&self) -> &[ActionObservation<G::State>] {
        &self.action_observations
    }

    fn fail(&mut self) {
        self.failed = true;
    }

    fn settle(&mut self) -> bool {
        let Some(chord) = self.stopped.take() else {
            return true;
        };
        let replayed = self.machine.replay(self.current).is_ok()
            && self
                .machine
                .branch(self.current, &nes::reproducer(std::slice::from_ref(&chord)))
                .is_ok()
            && matches!(
                self.machine.run(StopConditions::default(), None),
                Ok(machine::StopReason::Quiescent { .. }
                    | machine::StopReason::SnapshotPoint { .. })
            )
            && self.machine.frames().len() == usize::from(chord.bounded_hold_frames())
            && read_frame::<G, M>(&self.machine).ok().as_ref() == Some(&self.current_frame);
        if !replayed {
            self.fail();
            return false;
        }
        let Ok(next) = self.machine.snapshot() else {
            self.fail();
            return false;
        };
        if self.current != self.genesis && self.machine.drop_snapshot(self.current).is_err() {
            let _ = self.machine.drop_snapshot(next);
            self.fail();
            return false;
        }
        self.current = next;
        true
    }

    fn play_move(
        &mut self,
        parent: &G::State,
        mv: (G::Move, bool),
        departure: NesObservation<G::State>,
        frame: Vec<u8>,
        accumulator: &mut Accumulator,
        emitted: &mut Vec<ActionObservation<G::State>>,
    ) -> Result<Played<G::Move>, MachineError> {
        let lowered = Arc::clone(&self.lowered);
        let recording = self.recording;
        let mut records = Vec::new();
        let machine = &mut self.machine;
        let work = &mut self.execution_work;
        let walked = walk_move(
            &lowered,
            mv,
            (departure.frame_count, parent, departure.state.clone()),
            accumulator,
            frame,
            |chord| {
                let pressed = press(machine, chord)?;
                *work = work.saturating_add(pressed);
                Ok((pressed, read_frame::<G, M>(machine)?))
            },
            |record| {
                if recording {
                    records.push(record);
                }
            },
        )?;
        let (frames, frame, observation, forced) = match walked {
            Walked::Arrived {
                frames,
                frame,
                observation,
                accumulator: advanced,
                forced,
            } => {
                *accumulator = advanced;
                (frames, frame, observation, forced)
            }
            Walked::Overran {
                frames,
                frame,
                state,
            } => (
                frames,
                frame,
                NesObservation {
                    frame_count: departure.frame_count.saturating_add(frames),
                    state,
                    key: None,
                    overran: true,
                    ..departure
                },
                None,
            ),
        };
        emitted.push(ActionObservation {
            observation,
            cost: 0,
            records,
            choices: Vec::new(),
            edited: None,
        });
        Ok(Played {
            frames,
            frame,
            forced,
        })
    }

    fn play_forced_after(
        &mut self,
        parent: &G::State,
        forced: Option<G::Move>,
        frame: Vec<u8>,
        accumulator: &mut Accumulator,
        emitted: &mut Vec<ActionObservation<G::State>>,
    ) -> Result<(u64, Vec<u8>), MachineError> {
        let (Some(mv), Some(departure)) =
            (forced, emitted.last().map(|last| last.observation.clone()))
        else {
            return Ok((0, frame));
        };
        let played = self.play_move(parent, (mv, false), departure, frame, accumulator, emitted)?;
        Ok((played.frames, played.frame))
    }

    fn store_action(
        &mut self,
        start: SnapId,
        mut emitted: Vec<ActionObservation<G::State>>,
        cost: u64,
        accumulator: Accumulator,
        frame: Vec<u8>,
        stopped: Option<ButtonChord>,
    ) {
        let Some(last) = emitted.last_mut() else {
            return self.fail();
        };
        last.cost = cost;
        self.observation = last.observation.clone();
        self.action_observations = emitted;
        self.accumulator = accumulator;
        self.current_frame = frame;
        if stopped.is_some() {
            self.stopped = stopped;
            return;
        }
        let Ok(next) = self.machine.snapshot() else {
            return self.fail();
        };
        if start != self.genesis && self.machine.drop_snapshot(start).is_err() {
            let _ = self.machine.drop_snapshot(next);
            return self.fail();
        }
        self.current = next;
    }

    fn apply_move(&mut self, mv: G::Move) {
        let start = self.current;
        let mut accumulator = std::mem::take(&mut self.accumulator);
        let mut emitted = Vec::new();
        let departure = self.observation.clone();
        let parent = departure.state.clone();
        let Ok(played) = self.play_move(
            &parent,
            (mv, true),
            departure,
            self.current_frame.clone(),
            &mut accumulator,
            &mut emitted,
        ) else {
            return self.fail();
        };
        let Ok((forced_frames, frame)) = self.play_forced_after(
            &parent,
            played.forced,
            played.frame,
            &mut accumulator,
            &mut emitted,
        ) else {
            return self.fail();
        };
        self.store_action(
            start,
            emitted,
            played.frames.saturating_add(forced_frames),
            accumulator,
            frame,
            None,
        );
    }

    fn apply_internal(&mut self, action: &ButtonChord) {
        self.action_observations.clear();
        if self.failed || self.observation.stops() || !self.settle() {
            return;
        }
        if let Some(offer) = self.lowered.game.moves(&self.observation.state).offer
            && let Some(mv) = offer.chosen(action)
        {
            return self.apply_move(mv);
        }
        let start = self.current;
        if self
            .machine
            .branch(start, &nes::reproducer(std::slice::from_ref(action)))
            .is_err()
        {
            return self.fail();
        }
        let run = self.machine.run(StopConditions::default(), None);
        if !matches!(
            run,
            Ok(machine::StopReason::Quiescent { .. } | machine::StopReason::SnapshotPoint { .. })
        ) {
            return self.fail();
        }
        let produced_frames = self.machine.frames().len();
        if produced_frames == 0 {
            return self.fail();
        }
        self.execution_work = self
            .execution_work
            .saturating_add(u64::try_from(produced_frames).unwrap_or(u64::MAX));
        let mut emitted: Vec<ActionObservation<G::State>> = Vec::new();
        let mut prior_key = self.observation.key;
        let parent = self.observation.state.clone();
        let mut previous = parent.clone();
        let mut accumulator = std::mem::take(&mut self.accumulator);
        let mut endpoint = None;
        let mut records = Vec::new();
        let mut logical_frames = produced_frames;
        let mut forced = None;
        let Ok(played) = played_frames::<G, M>(&self.machine, self.native_memory.as_ref()) else {
            return self.fail();
        };
        for (offset, frame) in played.into_iter().enumerate() {
            let state = self
                .lowered
                .game
                .decode(Some(&parent), Some(&previous), &frame);
            let frame_count = self
                .observation
                .frame_count
                .saturating_add(u64::try_from(offset).unwrap_or(u64::MAX).saturating_add(1));
            let last_frame = offset + 1 == produced_frames;
            let framed =
                self.lowered
                    .observe_ending(frame_count, state, &mut accumulator, |moves| {
                        moves.ends_input(last_frame)
                    });
            if self.recording {
                records.push(framed.record);
            }
            let last = framed.cuts();
            forced = framed.forced;
            let observation = framed.observation;
            previous = observation.state.clone();
            if observation.key != prior_key || last {
                prior_key = observation.key;
                emitted.push(ActionObservation {
                    observation,
                    cost: 0,
                    records: std::mem::take(&mut records),
                    choices: Vec::new(),
                    edited: None,
                });
            }
            if last {
                logical_frames = offset + 1;
                endpoint = Some(frame);
                break;
            }
        }
        let Some(endpoint_frame) = endpoint else {
            return self.fail();
        };
        let Ok(hold) = u8::try_from(logical_frames) else {
            return self.fail();
        };
        let cut = ButtonChord::new(action.buttons, hold);
        let deferred = logical_frames < produced_frames
            && emitted.last().is_some_and(|last| last.observation.stops());
        if logical_frames < produced_frames && !deferred {
            if self.machine.replay(start).is_err()
                || self
                    .machine
                    .branch(start, &nes::reproducer(std::slice::from_ref(&cut)))
                    .is_err()
            {
                return self.fail();
            }
            let replay = self.machine.run(StopConditions::default(), None);
            let replayed_frame = read_frame::<G, M>(&self.machine);
            if !matches!(
                replay,
                Ok(machine::StopReason::Quiescent { .. }
                    | machine::StopReason::SnapshotPoint { .. })
            ) || self.machine.frames().len() != logical_frames
                || replayed_frame.as_ref().ok() != Some(&endpoint_frame)
            {
                return self.fail();
            }
        }
        let Ok((forced_frames, frame)) = self.play_forced_after(
            &parent,
            forced,
            endpoint_frame,
            &mut accumulator,
            &mut emitted,
        ) else {
            return self.fail();
        };
        let cost = u64::try_from(logical_frames)
            .unwrap_or(u64::MAX)
            .saturating_add(forced_frames);
        let stopped = deferred.then_some(cut);
        self.store_action(start, emitted, cost, accumulator, frame, stopped);
    }

    pub fn from_power_on(
        lowered: Arc<Lowered<G>>,
        mut machine: M,
        root: &[ButtonChord],
        native_memory: Option<NativeMemory<M>>,
    ) -> Result<Self, MachineError> {
        let pokes = lowered.game.setup_pokes();
        let poke_wram = match native_memory {
            Some(memory) => Some(memory.poke_wram),
            None if pokes.is_empty() => None,
            None => return Err(without_native_memory::<G>("pokes work RAM during setup")),
        };
        run_setup::<G, M>(&mut machine, &lowered.game.setup())?;
        if let Some(poke_wram) = poke_wram {
            for (address, value) in pokes {
                poke_wram(&mut machine, address, value)?;
            }
        }
        run_setup::<G, M>(&mut machine, &lowered.game.start())?;
        let mut target = Self::from_machine(lowered, machine, native_memory)?;
        if !root.is_empty() {
            target.root_at(root).map_err(MachineError::Backend)?;
        }
        Ok(target)
    }

    fn root_at(&mut self, root: &[ButtonChord]) -> Result<(), String> {
        let recording = std::mem::replace(&mut self.recording, true);
        let mut before = self.accumulator.clone();
        let mut pending = None;
        for (index, action) in root.iter().enumerate() {
            self.apply_internal(action);
            if self.failed {
                return Err(format!("{} root action {index} failed", G::NAME));
            }
            if self.observation.stops() {
                return Err(format!(
                    "{} root action {index} reaches a goal, a reject or a move overrun",
                    G::NAME
                ));
            }
            for record in self
                .action_observations
                .iter()
                .flat_map(|observation| &observation.records)
            {
                if let Some(earlier) = pending.replace(*record) {
                    let _ = self.lowered.compiled.observe(&earlier, &mut before);
                }
            }
        }
        self.recording = recording;
        if self
            .lowered
            .game
            .moves(&self.observation.state)
            .forced
            .is_some()
        {
            return Err(format!("{} root ends inside a forced move", G::NAME));
        }
        if self.observation.key.is_none() {
            return Err(format!("{} root ends on a state with no key", G::NAME));
        }
        self.lowered.game.rooted_genesis(&self.observation.state)?;
        if !self.settle() {
            return Err(format!("{} root endpoint did not replay", G::NAME));
        }
        if self.current != self.genesis {
            self.machine
                .drop_snapshot(self.genesis)
                .map_err(|error| error.to_string())?;
            self.genesis = self.current;
        }
        self.genesis_frame.clone_from(&self.current_frame);
        self.observation.frame_count = 0;
        self.genesis_observation = self.observation.clone();
        self.genesis_accumulator = self.accumulator.clone();
        self.accumulator_before_genesis = before;
        self.action_observations = vec![self.observation.clone().into()];
        self.snapshot_base = None;
        self.execution_work = 0;
        Ok(())
    }
}

fn press<M: Machine>(machine: &mut M, chord: ButtonChord) -> Result<u64, MachineError> {
    let from = machine.snapshot()?;
    let branched = machine.branch(from, &nes::reproducer(std::slice::from_ref(&chord)));
    machine.drop_snapshot(from)?;
    branched?;
    let run = machine.run(StopConditions::default(), None)?;
    let frames = machine.frames().len();
    if !matches!(
        run,
        machine::StopReason::Quiescent { .. } | machine::StopReason::SnapshotPoint { .. }
    ) || frames != usize::from(chord.bounded_hold_frames())
    {
        return Err(MachineError::Backend(
            "a forced move's chord did not run its full hold".to_owned(),
        ));
    }
    Ok(u64::try_from(frames).unwrap_or(u64::MAX))
}

fn run_setup<G: Game, M: Machine>(
    machine: &mut M,
    tape: &[ButtonChord],
) -> Result<(), MachineError> {
    if tape.is_empty() {
        return Ok(());
    }
    let unquiet = || MachineError::Backend(format!("{} setup did not quiesce", G::NAME));
    let total = tape_frames(tape);
    let from = machine.snapshot()?;
    machine.branch(from, &nes::reproducer(tape))?;
    let mut produced = 0_u64;
    loop {
        let run = machine.run(StopConditions::default(), None)?;
        let frames = u64::try_from(machine.frames().len()).unwrap_or(u64::MAX);
        produced = produced.saturating_add(frames);
        match run {
            machine::StopReason::Quiescent { .. } => break,
            machine::StopReason::SnapshotPoint { .. } if frames > 0 && produced < total => {}
            machine::StopReason::SnapshotPoint { .. } if frames > 0 && produced == total => break,
            _ => return Err(unquiet()),
        }
    }
    if produced != total {
        return Err(unquiet());
    }
    machine.drop_snapshot(from)
}

impl<G: Game, M: NesBackend<Vec<u8>>> NesTarget<G, M> {
    pub fn snapshot_of_state(
        &mut self,
        state: &Vec<u8>,
        frame_count: u64,
    ) -> Result<NesSnapshot<G::State>, Box<dyn Error>> {
        self.machine
            .restore_nes(state)
            .map_err(|error| error.to_string())?;
        let frame = read_frame::<G, M>(&self.machine)?;
        let decoded = self.lowered.game.decode(None, None, &frame);
        let next = self.machine.snapshot()?;
        if self.current != self.genesis {
            self.machine.drop_snapshot(self.current)?;
        }
        self.current = next;
        self.stopped = None;
        self.current_frame = frame;
        self.failed = false;
        self.snapshot_base = None;
        self.accumulator = Accumulator::default();
        self.observation = self
            .lowered
            .observe(frame_count, decoded, &mut self.accumulator);
        self.action_observations = vec![self.observation.clone().into()];
        self.snapshot()
            .ok_or_else(|| "failed to snapshot the restored state".into())
    }
}

#[cfg(unix)]
struct Footage<'a> {
    video: &'a mut dyn Write,
    audio: &'a mut dyn Write,
    metadata: Option<VideoMetadata>,
    skip_frames: u64,
    skip: u64,
}

#[cfg(unix)]
impl<'a> Footage<'a> {
    fn new(video: &'a mut dyn Write, audio: &'a mut dyn Write, skip_frames: u64) -> Self {
        Self {
            video,
            audio,
            metadata: None,
            skip_frames,
            skip: skip_frames,
        }
    }

    fn take<M: FilmMachine>(&mut self, machine: &mut M) -> Result<(), Box<dyn Error>> {
        let frame = machine
            .take_video_frame()
            .ok_or("QuickNES omitted a requested video frame")?;
        if self.skip > 0 {
            self.skip -= 1;
            machine.take_audio_samples();
            return Ok(());
        }
        if let Some(existing) = &self.metadata
            && (existing.width, existing.height) != (frame.width, frame.height)
        {
            return Err("QuickNES video geometry changed during replay".into());
        }
        let metadata = self.metadata.get_or_insert(VideoMetadata {
            width: frame.width,
            height: frame.height,
            frames: 0,
            audio_sample_rate: QUICKNES_AUDIO_SAMPLE_RATE,
            audio_channels: QUICKNES_AUDIO_CHANNELS,
            audio_frames: 0,
            skipped_frames: 0,
        });
        metadata.frames = metadata.frames.saturating_add(1);
        self.video.write_all(&frame.rgb24)?;
        let audio = machine.take_audio_samples();
        if !audio
            .len()
            .is_multiple_of(usize::from(QUICKNES_AUDIO_CHANNELS))
        {
            return Err("QuickNES produced a partial stereo audio frame".into());
        }
        for sample in &audio {
            self.audio.write_all(&sample.to_le_bytes())?;
        }
        metadata.audio_frames = metadata.audio_frames.saturating_add(u64::try_from(
            audio.len() / usize::from(QUICKNES_AUDIO_CHANNELS),
        )?);
        Ok(())
    }

    fn finish(self) -> Result<VideoMetadata, Box<dyn Error>> {
        let mut metadata = self.metadata.ok_or("QuickNES produced no video frames")?;
        if metadata.audio_frames == 0 {
            return Err("QuickNES produced no audio samples".into());
        }
        metadata.skipped_frames = self.skip_frames - self.skip;
        Ok(metadata)
    }
}

#[cfg(unix)]
fn render_setup<G: Game, M: Machine + FilmMachine>(
    machine: &mut M,
    tape: &[ButtonChord],
    footage: &mut Footage<'_>,
) -> Result<(), Box<dyn Error>> {
    let from = machine.snapshot()?;
    machine.branch(from, &nes::reproducer(tape))?;
    machine.drop_snapshot(from)?;
    for _ in 0..tape_frames(tape) {
        if !run_one_frame(machine)? {
            return Err(format!("{} setup ended before its last frame", G::NAME).into());
        }
        footage.take(machine)?;
    }
    if !matches!(
        machine.run(StopConditions::default(), None)?,
        machine::StopReason::Quiescent { .. }
    ) {
        return Err(format!("{} setup did not quiesce", G::NAME).into());
    }
    Ok(())
}

#[cfg(unix)]
fn run_one_frame<M: Machine>(machine: &mut M) -> Result<bool, Box<dyn Error>> {
    let deadline = machine::Moment(machine.now().0.saturating_add(1));
    match machine.run(
        StopConditions {
            deadline: Some(deadline),
            on: machine::StopMask::NONE,
        },
        None,
    )? {
        machine::StopReason::Deadline { .. } => Ok(true),
        machine::StopReason::Quiescent { .. } => Ok(false),
        _ => Err("QuickNES film stopped unexpectedly".into()),
    }
}

#[must_use]
pub(super) fn tape_frames(tape: &[ButtonChord]) -> u64 {
    tape.iter()
        .map(|chord| u64::from(chord.bounded_hold_frames()))
        .sum()
}

#[cfg(unix)]
impl<G: Game, M: NesBackend<Vec<u8>> + NesMemory + FilmMachine> NesTarget<G, M> {
    pub fn render_input(
        &mut self,
        actions: &[ButtonChord],
        tail_frames: u32,
        skip_frames: u64,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<(VideoMetadata, G::State), Box<dyn Error>> {
        self.render_observed(
            actions,
            tail_frames,
            skip_frames,
            video_output,
            audio_output,
        )
        .map(|(metadata, endpoint)| (metadata, endpoint.state))
    }

    pub fn render_observed(
        &mut self,
        actions: &[ButtonChord],
        tail_frames: u32,
        skip_frames: u64,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<Filmed<G::State>, Box<dyn Error>> {
        let mut footage = Footage::new(video_output, audio_output, skip_frames);
        let endpoint = self.render_tape(actions, tail_frames, &mut footage)?;
        Ok((footage.finish()?, endpoint))
    }

    pub fn render_from_power_on(
        lowered: Arc<Lowered<G>>,
        mut machine: M,
        actions: &[ButtonChord],
        tail_frames: u32,
        skip_frames: u64,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<Filmed<G::State>, Box<dyn Error>> {
        let mut footage = Footage::new(video_output, audio_output, skip_frames);
        machine.set_video_capture(true);
        machine.set_audio_capture(true);
        let opened = (|| -> Result<(), Box<dyn Error>> {
            render_setup::<G, M>(&mut machine, &lowered.game.setup(), &mut footage)?;
            for (address, value) in lowered.game.setup_pokes() {
                machine.poke_wram(address, value)?;
            }
            let start = lowered.game.start();
            if !start.is_empty() {
                render_setup::<G, M>(&mut machine, &start, &mut footage)?;
            }
            Ok(())
        })();
        machine.set_audio_capture(false);
        machine.set_video_capture(false);
        opened?;
        let mut target = Self::from_machine(lowered, machine, Some(NativeMemory::of()))?;
        let endpoint = target.render_tape(actions, tail_frames, &mut footage)?;
        Ok((footage.finish()?, endpoint))
    }

    fn render_tape(
        &mut self,
        actions: &[ButtonChord],
        tail_frames: u32,
        footage: &mut Footage<'_>,
    ) -> Result<NesObservation<G::State>, Box<dyn Error>> {
        self.reset();
        if self.failed {
            return Err(format!("could not restore the {} genesis for film", G::NAME).into());
        }
        self.machine.set_video_capture(true);
        self.machine.set_audio_capture(true);
        let result = (|| {
            let mut accumulator = self.genesis_accumulator.clone();
            let mut tracked = self.observation.clone();
            for action in actions {
                let parent = tracked.state.clone();
                let chosen = self
                    .lowered
                    .game
                    .moves(&tracked.state)
                    .offer
                    .and_then(|offer| offer.chosen(action));
                let mut next = match chosen {
                    Some(mv) => Some((mv, true)),
                    None => self
                        .render_action(
                            *action,
                            Some((&parent, &mut accumulator, &mut tracked)),
                            footage,
                        )?
                        .map(|forced| (forced, false)),
                };
                while let Some((mv, offered)) = next.take() {
                    let lowered = Arc::clone(&self.lowered);
                    let frame = read_frame::<G, M>(&self.machine)?;
                    let walked = walk_move(
                        &lowered,
                        (mv, offered),
                        (0, &parent, tracked.state.clone()),
                        &accumulator,
                        frame,
                        |chord| -> Result<_, Box<dyn Error>> {
                            self.render_action(chord, None, footage)?;
                            Ok((
                                u64::from(chord.bounded_hold_frames()),
                                read_frame::<G, M>(&self.machine)?,
                            ))
                        },
                        |_| {},
                    )?;
                    let Walked::Arrived {
                        observation,
                        accumulator: advanced,
                        forced,
                        ..
                    } = walked
                    else {
                        return Err(format!(
                            "{} move {mv:?} ran past its {} frames in the film",
                            G::NAME,
                            lowered.game.move_frames(mv)
                        )
                        .into());
                    };
                    accumulator = advanced;
                    tracked = observation;
                    next = forced.map(|forced| (forced, false));
                }
            }
            let endpoint = tracked;
            let mut remaining = tail_frames;
            while remaining > 0 {
                let hold = remaining.min(u32::from(MAX_HOLD_FRAMES));
                self.render_action(ButtonChord::new(0, u8::try_from(hold)?), None, footage)?;
                remaining -= hold;
            }
            Ok(endpoint)
        })();
        self.machine.set_audio_capture(false);
        self.machine.set_video_capture(false);
        result
    }

    fn render_action(
        &mut self,
        action: ButtonChord,
        mut tracked: Option<Tracked<'_, G::State>>,
        footage: &mut Footage<'_>,
    ) -> Result<Option<G::Move>, Box<dyn Error>> {
        let start = self.machine.snapshot()?;
        self.machine
            .branch(start, &nes::reproducer(std::slice::from_ref(&action)))?;
        self.machine.drop_snapshot(start)?;
        let hold = action.bounded_hold_frames();
        for frame in 0..hold {
            if !run_one_frame(&mut self.machine)? {
                break;
            }
            footage.take(&mut self.machine)?;
            if let Some(forced) = self.film_cut(
                tracked.as_mut().map(|(parent, accumulator, observation)| {
                    (*parent, &mut **accumulator, &mut **observation)
                }),
                frame + 1 == hold,
            )? {
                return Ok(forced);
            }
        }
        Ok(None)
    }

    fn film_cut(
        &self,
        tracked: Option<Tracked<'_, G::State>>,
        last_frame: bool,
    ) -> Result<Option<Option<G::Move>>, Box<dyn Error>> {
        let Some((parent, accumulator, tracked)) = tracked else {
            return Ok(None);
        };
        let decoded = self.lowered.game.decode(
            Some(parent),
            Some(&tracked.state),
            &read_frame::<G, M>(&self.machine)?,
        );
        let framed = self
            .lowered
            .observe_ending(0, decoded, accumulator, |moves| {
                moves.ends_input(last_frame)
            });
        let cut = framed.cuts().then_some(framed.forced);
        *tracked = framed.observation;
        Ok(cut)
    }
}

impl<G: Game, M: NesBackend<P>, P: SnapshotState> Target for NesTarget<G, M, P> {
    type Action = ButtonChord;
    type Observations = NesObservation<G::State>;
    type Snapshot = NesSnapshot<G::State, P>;

    fn reset(&mut self) {
        let mut failed = false;
        if self.current != self.genesis {
            if self.machine.drop_snapshot(self.current).is_err() {
                failed = true;
            } else {
                self.current = self.genesis;
            }
        }
        if self.machine.replay(self.genesis).is_err() {
            failed = true;
        }
        self.stopped = None;
        self.failed = failed;
        self.snapshot_base = None;
        self.current_frame.clone_from(&self.genesis_frame);
        self.accumulator = self.genesis_accumulator.clone();
        self.observation = self.genesis_observation.clone();
        self.action_observations = vec![self.observation.clone().into()];
    }

    fn apply(&mut self, action: &Self::Action) {
        self.apply_internal(action);
    }

    fn observe(&self) -> Self::Observations {
        self.observation.clone()
    }

    fn fingerprint(&self) -> u64 {
        self.observation.key.map_or(0, |key| {
            let mut hash = 0xcbf2_9ce4_8422_2325_u64;
            for word in [key.place.get(), key.identity] {
                for byte in word.to_le_bytes() {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
                }
            }
            hash
        })
    }

    fn exit_kind(&self) -> ExitKind {
        if self.failed {
            ExitKind::Crash
        } else {
            ExitKind::Ok
        }
    }

    fn snapshot(&mut self) -> Option<Self::Snapshot> {
        if self.failed || !self.settle() {
            return None;
        }
        let Some(wram) = self
            .current_frame
            .get(..WRAM_SIZE)
            .and_then(|wram| <&[u8; WRAM_SIZE]>::try_from(wram).ok())
        else {
            self.failed = true;
            return None;
        };
        let work_ram = P::WorkRam::copy_of(wram);
        let Ok(emulator_state) = self
            .machine
            .export_nes(self.current, self.snapshot_base.as_ref())
        else {
            self.failed = true;
            return None;
        };
        self.snapshot_base = Some(emulator_state.clone());
        Some(NesSnapshot {
            emulator_state,
            observation: self.observation.clone(),
            accumulator: self.accumulator.clone(),
            failed: self.failed,
            work_ram,
        })
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), Box<dyn Error>> {
        let kept = match snapshot.work_ram.work_ram() {
            Some(_) if !G::MEMORY_WINDOWS.is_empty() => {
                return Err(format!(
                    "the {} snapshot keeps only work RAM, and its frame also holds save RAM",
                    G::NAME
                )
                .into());
            }
            Some(wram) if wram.len() != WRAM_SIZE => {
                return Err(
                    format!("the {} snapshot work RAM is not exactly 2 KiB", G::NAME).into(),
                );
            }
            kept => kept.map(<[u8]>::to_vec),
        };
        let imported = self
            .machine
            .import_nes(&snapshot.emulator_state)
            .map_err(|error| error.to_string())?;
        let restored_frame = match self.machine.replay(imported).and_then(|()| match kept {
            Some(frame) => Ok(frame),
            None => read_frame::<G, M>(&self.machine),
        }) {
            Ok(frame) => frame,
            Err(error) => {
                self.failed = true;
                let _ = self.machine.drop_snapshot(imported);
                return Err(error.to_string().into());
            }
        };
        if self.current != self.genesis
            && let Err(error) = self.machine.drop_snapshot(self.current)
        {
            self.failed = true;
            let _ = self.machine.drop_snapshot(imported);
            return Err(error.to_string().into());
        }
        self.current = imported;
        self.stopped = None;
        self.snapshot_base = Some(snapshot.emulator_state.clone());
        self.current_frame = restored_frame;
        self.accumulator = snapshot.accumulator.clone();
        self.observation = snapshot.observation.clone();
        self.action_observations = vec![self.observation.clone().into()];
        self.failed = snapshot.failed;
        Ok(())
    }
}

impl<G: Game, M: NesBackend<P>, P: SnapshotState> Drop for NesTarget<G, M, P> {
    fn drop(&mut self) {
        if self.current != self.genesis {
            let _ = self.machine.drop_snapshot(self.current);
        }
        let _ = self.machine.drop_snapshot(self.genesis);
    }
}

pub(crate) fn read_wram<M: Machine>(machine: &M) -> Result<[u8; WRAM_SIZE], MachineError> {
    machine.read(0, WRAM_SIZE as u32)?.try_into().map_err(|_| {
        MachineError::Backend("the system RAM window has an invalid length".to_owned())
    })
}

fn window_length(length: u32) -> usize {
    usize::try_from(length).unwrap_or(usize::MAX)
}

fn save_ram_offset(address: u64) -> usize {
    usize::try_from(address.saturating_sub(SAVE_RAM_BASE)).unwrap_or(usize::MAX)
}

fn save_ram_span(windows: &[(u64, u32)]) -> Option<Range<usize>> {
    let start = windows.iter().map(|(address, _)| *address).min()?;
    let end = windows
        .iter()
        .map(|(address, length)| address.saturating_add(u64::from(*length)))
        .max()?;
    Some(save_ram_offset(start)..save_ram_offset(end))
}

pub(crate) fn zero_frame<G: Game>() -> Vec<u8> {
    let windows = G::MEMORY_WINDOWS
        .iter()
        .map(|(_, length)| window_length(*length))
        .fold(0_usize, usize::saturating_add);
    vec![0; WRAM_SIZE.saturating_add(windows)]
}

pub(crate) fn windowed<G: Game>(wram: &[u8], save_ram: &[u8], from: usize) -> Vec<u8> {
    let mut frame = wram.to_vec();
    for (address, length) in G::MEMORY_WINDOWS {
        let length = window_length(*length);
        let start = save_ram_offset(*address).saturating_sub(from);
        let window = save_ram.get(start..).unwrap_or_default();
        let kept = window.len().min(length);
        frame.extend_from_slice(&window[..kept]);
        frame.resize(frame.len() + (length - kept), 0);
    }
    frame
}

pub(crate) fn read_frame<G: Game, M: Machine>(machine: &M) -> Result<Vec<u8>, MachineError> {
    let mut frame = read_wram(machine)?.to_vec();
    for (address, length) in G::MEMORY_WINDOWS {
        frame.extend(machine.read(*address, *length)?);
    }
    Ok(frame)
}

fn played_frames<G: Game, M: Machine>(
    machine: &M,
    native_memory: Option<&NativeMemory<M>>,
) -> Result<Vec<Vec<u8>>, MachineError> {
    let wram = machine.frames();
    let Some(span) = save_ram_span(G::MEMORY_WINDOWS) else {
        return Ok(wram.iter().map(|frame| frame.to_vec()).collect());
    };
    let memory =
        native_memory.ok_or_else(|| without_native_memory::<G>("reads save RAM on every frame"))?;
    let captured = (memory.save_ram_frames)(machine);
    if span.is_empty() || captured.len() != wram.len() * span.len() {
        return Err(MachineError::Backend(format!(
            "{} captured {} bytes of save RAM for {} frames of {} bytes",
            G::NAME,
            captured.len(),
            wram.len(),
            span.len()
        )));
    }
    Ok(wram
        .iter()
        .zip(captured.chunks_exact(span.len()))
        .map(|(wram, save_ram)| windowed::<G>(wram, save_ram, span.start))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        search::{
            campaign::{SnapshotCheckpoint, SnapshotCheckpointEntry},
            snapshot_codec::{read_checkpoint, write_checkpoint},
        },
        workload::{
            SNAPSHOT_CHECKPOINT_FORMAT,
            fixture::{Cellar, Guest, GuestState, LEFT, Ledge, Preset, RIGHT, Spot, Stepper, WARP},
        },
    };

    type Ledged = NesTarget<Ledge, Stepper>;

    fn ledged() -> Ledged {
        let lowered = Arc::new(Lowered::new(Ledge, Ledge::STRATEGY).unwrap());
        assert!(!lowered.compiled.is_pure());
        NesTarget::from_machine(lowered, Stepper::default(), Some(NativeMemory::of())).unwrap()
    }

    fn walk<G: Game, M: NesBackend<P>, P: SnapshotState>(
        target: &mut NesTarget<G, M, P>,
        chords: &[(u8, u8)],
    ) -> Vec<Option<AssertionKey>> {
        chords
            .iter()
            .map(|&(buttons, frames)| {
                target.apply(&ButtonChord::new(buttons, frames));
                assert_eq!(target.exit_kind(), ExitKind::Ok);
                target.observation().key
            })
            .collect()
    }

    const PAST_THE_LEDGE: [(u8, u8); 2] = [(RIGHT, 4), (LEFT, 3)];
    const NEAR_THE_GOAL: [(u8, u8); 2] = [(WARP, 4), (WARP, 4)];

    fn stopped_at_the_goal() -> Ledged {
        let mut target = ledged();
        walk(&mut target, &NEAR_THE_GOAL);
        target.apply(&ButtonChord::new(WARP, 4));
        assert!(target.observation().stops());
        assert_eq!(target.observation().frame_count, 9);
        target
    }

    #[test]
    fn a_game_with_memory_windows_decodes_each_frame_from_save_ram_as_work_ram_would() {
        let lowered = Arc::new(Lowered::new(Cellar, Cellar::STRATEGY).unwrap());
        let mut cellar =
            NesTarget::from_machine(lowered, Stepper::default(), Some(NativeMemory::of())).unwrap();
        let mut ledge = ledged();
        cellar.record_frames(true);
        ledge.record_frames(true);
        assert_eq!(cellar.frame().len(), WRAM_SIZE + 2);
        assert_eq!(ledge.frame().len(), WRAM_SIZE);
        let chords = [
            (RIGHT, 4),
            (LEFT, 3),
            (WARP, 4),
            (RIGHT, 2),
            (WARP, 4),
            (WARP, 4),
        ];
        for (buttons, frames) in chords {
            let chord = ButtonChord::new(buttons, frames);
            cellar.apply(&chord);
            ledge.apply(&chord);
            assert_eq!(cellar.exit_kind(), ExitKind::Ok);
            assert_eq!(
                cellar.last_action_observations(),
                ledge.last_action_observations()
            );
            assert_eq!(&cellar.frame()[..WRAM_SIZE], ledge.frame());
            let snapshot = cellar.snapshot().unwrap();
            cellar.restore(&snapshot).unwrap();
            assert_eq!(cellar.observation(), ledge.observation());
        }
        assert_eq!(cellar.state(), &Spot { x: 0, room: 9 });
        assert!(cellar.observation().stops());
    }

    #[test]
    fn a_memory_window_outside_save_ram_is_refused() {
        #[derive(Clone, Copy, Debug, Default)]
        struct Outside;
        impl Game for Outside {
            type State = Spot;
            type Move = crate::workload::Never;
            const NAME: &'static str = "outside";
            const STRATEGY: &'static str = Ledge::STRATEGY;
            const CHORDS: crate::chord::ChordVocabulary = Ledge::CHORDS;
            const MEMORY_WINDOWS: &'static [(u64, u32)] = &[(0x7fff, 2)];
            fn setup(&self) -> Vec<ButtonChord> {
                Vec::new()
            }
            fn genesis(&self, _: &Spot) -> Result<(), String> {
                Ok(())
            }
            fn decode(&self, _: Option<&Spot>, _: Option<&Spot>, frame: &[u8]) -> Spot {
                Cellar.decode(None, None, frame)
            }
            fn declare(&self, state: &Spot, out: &mut impl crate::search::assertion::Declarations) {
                Ledge.declare(state, out);
            }
            fn moves(&self, _: &Spot) -> crate::workload::Moves<Self::Move> {
                crate::workload::Moves::none()
            }
            fn play(&self, mv: Self::Move, _: &Spot) -> Option<crate::workload::Step<Self::Move>> {
                match mv {}
            }
            fn move_frames(&self, mv: Self::Move) -> u64 {
                match mv {}
            }
        }
        let error = Lowered::new(Outside, Outside::STRATEGY).err().unwrap();
        assert_eq!(
            error.to_string(),
            "outside memory window 0x7fff+2 is not inside save RAM"
        );
    }

    #[test]
    fn an_action_observation_keeps_its_cost_through_postcard() {
        let target = stopped_at_the_goal();
        let observations = target.last_action_observations();
        assert_eq!(
            observations
                .iter()
                .map(|observation| observation.cost)
                .sum::<u64>(),
            1
        );
        let received: Vec<ActionObservation<Spot>> =
            postcard::from_bytes(&postcard::to_allocvec(observations).unwrap()).unwrap();
        assert_eq!(received, observations);
    }

    #[test]
    fn a_terminal_stop_runs_its_chord_once() {
        let mut target = ledged();
        walk(&mut target, &NEAR_THE_GOAL);
        let runs = target.machine.runs;
        target.apply(&ButtonChord::new(WARP, 4));
        assert!(target.observation().stops());
        assert_eq!(target.machine.runs, runs + 1);
    }

    #[test]
    fn a_snapshot_after_a_stop_equals_the_snapshot_of_the_cut_chord() {
        let mut stopped = stopped_at_the_goal();
        let mut cut = ledged();
        walk(&mut cut, &NEAR_THE_GOAL);
        cut.apply(&ButtonChord::new(WARP, 1));
        assert_eq!(
            postcard::to_allocvec(&stopped.snapshot().unwrap()).unwrap(),
            postcard::to_allocvec(&cut.snapshot().unwrap()).unwrap()
        );
        assert_eq!(stopped.exit_kind(), ExitKind::Ok);
    }

    #[test]
    fn a_snapshot_fails_when_the_replay_of_a_stop_reaches_other_work_ram() {
        let mut target = stopped_at_the_goal();
        target.machine.drift = 1;
        assert!(target.snapshot().is_none());
        assert_eq!(target.exit_kind(), ExitKind::Crash);
    }
    const ON: [(u8, u8); 4] = [(LEFT, 1), (RIGHT, 1), (WARP, 1), (RIGHT, 2)];

    fn guest_from_power_on<G: Game>(
        game: G,
    ) -> Result<NesTarget<G, Guest, GuestState>, MachineError> {
        let lowered = Arc::new(Lowered::new(game, G::STRATEGY).unwrap());
        NesTarget::from_power_on(lowered, Guest::default(), &[], None)
    }

    #[test]
    fn a_restore_succeeds_on_a_machine_that_reads_only_after_a_run() {
        let mut target = guest_from_power_on(Ledge).unwrap();
        let genesis = target.observation().clone();
        walk(&mut target, &PAST_THE_LEDGE);
        let snapshot = target.snapshot().unwrap();
        assert_eq!(snapshot.work_ram.work_ram(), Some(target.frame()));
        let observed = target.observation().clone();
        let first = walk(&mut target, &ON);

        target.restore(&snapshot).unwrap();
        assert_eq!(target.observation(), &observed);
        assert_eq!(target.frame(), &snapshot.work_ram[..]);
        let again = target.snapshot().unwrap();
        assert_ne!(again, snapshot);
        assert_eq!(again.logical_identity(), snapshot.logical_identity());
        assert_eq!(walk(&mut target, &ON), first);

        let mut native = ledged();
        walk(&mut native, &PAST_THE_LEDGE);
        assert_eq!(walk(&mut native, &ON), first);
        assert_eq!(native.observation(), target.observation());

        target.reset();
        assert_eq!(target.observation(), &genesis);
        walk(&mut target, &PAST_THE_LEDGE);
        assert_eq!(target.observation(), &observed);
    }

    #[test]
    fn setup_on_a_machine_that_stops_after_every_chord_runs_once_per_chord() {
        let setup = vec![
            ButtonChord::new(RIGHT, 2),
            ButtonChord::new(RIGHT, 1),
            ButtonChord::new(WARP, 1),
            ButtonChord::new(RIGHT, 3),
        ];
        let game = Preset {
            setup: setup.clone(),
            pokes: Vec::new(),
        };
        let guest = guest_from_power_on(game.clone()).unwrap();
        assert_eq!(guest.machine.runs, setup.len());
        assert_eq!(guest.state(), &Spot { x: 3, room: 1 });
        let lowered = Arc::new(Lowered::new(game, Preset::STRATEGY).unwrap());
        let native =
            NesTarget::from_power_on(lowered, Stepper::default(), &[], Some(NativeMemory::of()))
                .unwrap();
        assert_eq!(native.machine.runs, 1);
        assert_eq!(native.frame(), guest.frame());
        assert_eq!(native.observation(), guest.observation());
    }

    #[test]
    fn a_target_without_native_memory_refuses_setup_pokes_and_memory_windows() {
        let poked = Preset {
            setup: Vec::new(),
            pokes: vec![(0, 5)],
        };
        assert_eq!(
            guest_from_power_on(poked.clone()).err(),
            Some(MachineError::Backend(
                "preset pokes work RAM during setup, which only the native QuickNES backend provides"
                    .to_owned()
            ))
        );
        let lowered = Arc::new(Lowered::new(poked, Preset::STRATEGY).unwrap());
        let native =
            NesTarget::from_power_on(lowered, Stepper::default(), &[], Some(NativeMemory::of()))
                .unwrap();
        assert_eq!(native.state(), &Spot { x: 5, room: 0 });
        assert_eq!(
            guest_from_power_on(Cellar).err(),
            Some(MachineError::Backend(
                "cellar reads save RAM on every frame, which only the native QuickNES backend provides"
                    .to_owned()
            ))
        );
    }

    #[test]
    fn a_restore_brings_back_the_accumulator_so_the_keys_after_it_repeat() {
        let mut target = ledged();
        walk(&mut target, &PAST_THE_LEDGE);
        let snapshot = target.snapshot().unwrap();
        let first = walk(&mut target, &ON);
        let view = target.lowered.compiled.view(first[0].unwrap());
        assert_eq!(
            (view.stage.as_deref(), view.identity),
            (Some("beyond"), vec![4])
        );
        target.restore(&snapshot).unwrap();
        assert_eq!(walk(&mut target, &ON), first);
    }

    #[test]
    fn the_snapshot_checkpoint_round_trips_the_accumulator() {
        let mut target = ledged();
        walk(&mut target, &PAST_THE_LEDGE);
        let snapshot = target.snapshot().unwrap();
        assert!(snapshot.accumulator.heap_bytes() > 0);
        let mut bytes = Vec::new();
        write_checkpoint(
            &SnapshotCheckpoint {
                format: SNAPSHOT_CHECKPOINT_FORMAT.to_owned(),
                entries: vec![SnapshotCheckpointEntry {
                    id: 7,
                    snapshot: snapshot.clone(),
                }],
            },
            &mut bytes,
        )
        .unwrap();
        let read: SnapshotCheckpoint<NesSnapshot<Spot>> =
            read_checkpoint(&mut bytes.as_slice(), SNAPSHOT_CHECKPOINT_FORMAT).unwrap();
        let decoded = &read.entries[0].snapshot;
        assert_eq!(decoded, &snapshot);
        let first = walk(&mut target, &ON);
        target.restore(decoded).unwrap();
        assert_eq!(walk(&mut target, &ON), first);
        let mut previous = Vec::new();
        write_checkpoint(
            &SnapshotCheckpoint {
                format: "nes-workload-snapshot-checkpoint-v2".to_owned(),
                entries: vec![SnapshotCheckpointEntry { id: 7, snapshot }],
            },
            &mut previous,
        )
        .unwrap();
        let error = read_checkpoint::<NesSnapshot<Spot>>(
            &mut previous.as_slice(),
            SNAPSHOT_CHECKPOINT_FORMAT,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "snapshot checkpoint format is nes-workload-snapshot-checkpoint-v2; this build reads {SNAPSHOT_CHECKPOINT_FORMAT}"
            )
        );
    }
}

#[cfg(test)]
mod moves {
    use std::{collections::BTreeMap, ops::Range, sync::Arc};

    use machine::{
        Answer, Machine, MachineError, Moment, Reproducer, SnapId, StopConditions, StopReason,
        nes::{ButtonChord, WRAM_SIZE, actions_of},
    };

    use super::{NativeMemory, NesTarget};
    use crate::search::assertion::Accumulator;
    use crate::workload::fixture::{LEFT, Ledge as FixtureLedge, RIGHT, Spot, Stepper, WARP};
    use crate::{
        chord::{self, CHORD_SITES, ChordVocabulary},
        nes_backend::{NesBackend, NesMemory},
        search::{
            archive::RetentionPolicy,
            assertion::{Assertion, Declarations, Number},
            campaign::{Reporting, TargetExecution},
            choice::{CERTAIN, Choice, ChoicePlan, ChoiceSite},
            rollout::RolloutStop,
        },
        target::{ExitKind, Target},
        workload::{
            EVERY_OPTION, Game, Lowered, Moves, NesRun, NesWorkload, Offer, Play, Preroll,
            Satisfied, SetupPoke, Step, move_answer, move_chord,
        },
    };

    #[derive(Default)]
    struct Walker {
        x: u8,
        held: BTreeMap<u64, u8>,
        next: u64,
        staged: Vec<ButtonChord>,
        frames: Vec<[u8; WRAM_SIZE]>,
    }

    impl Walker {
        fn wram(&self) -> [u8; WRAM_SIZE] {
            let mut wram = [0; WRAM_SIZE];
            wram[0] = self.x;
            wram
        }
    }

    impl Machine for Walker {
        type Portable = Vec<u8>;

        fn snapshot(&mut self) -> Result<SnapId, MachineError> {
            self.next += 1;
            self.held.insert(self.next, self.x);
            Ok(SnapId(self.next))
        }

        fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError> {
            self.held.remove(&snap.0);
            Ok(())
        }

        fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), MachineError> {
            self.replay(snap)?;
            self.staged = actions_of(env)?;
            Ok(())
        }

        fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
            self.x = *self
                .held
                .get(&snap.0)
                .ok_or(MachineError::UnknownSnapshot)?;
            Ok(())
        }

        fn run(
            &mut self,
            _until: StopConditions,
            _resolve: Option<&Answer>,
        ) -> Result<StopReason, MachineError> {
            self.frames.clear();
            for chord in std::mem::take(&mut self.staged) {
                for _ in 0..chord.bounded_hold_frames() {
                    self.x = self.x.saturating_add(chord.buttons & 1);
                    self.frames.push(self.wram());
                }
            }
            Ok(StopReason::Quiescent { vtime: Moment(0) })
        }

        fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError> {
            let start = usize::try_from(addr).map_err(|_| MachineError::ReadOutOfBounds)?;
            self.wram()
                .get(start..start + len as usize)
                .map(<[u8]>::to_vec)
                .ok_or(MachineError::ReadOutOfBounds)
        }

        fn export(
            &mut self,
            snap: SnapId,
            _base: Option<&Vec<u8>>,
        ) -> Result<Vec<u8>, MachineError> {
            self.held
                .get(&snap.0)
                .map(|x| vec![*x])
                .ok_or(MachineError::UnknownSnapshot)
        }

        fn import(&mut self, portable: &Vec<u8>) -> Result<SnapId, MachineError> {
            self.next += 1;
            self.held.insert(self.next, portable[0]);
            Ok(SnapId(self.next))
        }

        fn portable_memory_charge(portable: &Vec<u8>) -> usize {
            portable.len()
        }

        fn now(&self) -> Moment {
            Moment(0)
        }

        fn frames(&self) -> &[[u8; WRAM_SIZE]] {
            &self.frames
        }
    }

    impl NesBackend<Vec<u8>> for Walker {
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

    impl NesMemory for Walker {
        fn read_wram(&self) -> Result<[u8; WRAM_SIZE], MachineError> {
            Ok(self.wram())
        }

        fn poke_wram(&mut self, _addr: usize, _byte: u8) -> Result<(), MachineError> {
            Ok(())
        }

        fn capture_save_ram_per_frame(&mut self, _range: Range<usize>) {}

        fn save_ram_frames(&self) -> &[u8] {
            &[]
        }

        fn read_save_ram(&self) -> Result<Vec<u8>, MachineError> {
            Ok(Vec::new())
        }

        fn write_save_ram(&mut self, _offset: usize, _bytes: &[u8]) -> Result<(), MachineError> {
            Ok(())
        }
    }

    const FAR_FROM: u8 = 10;

    const STEPS: [(u16, Play<u8>); 3] = [(0, Play::Chord), (1, Play::Move(4)), (2, Play::Move(8))];

    #[derive(Clone, Debug)]
    struct Walk;

    impl Game for Walk {
        type State = u8;
        type Move = u8;

        const NAME: &'static str = "walk";
        const STRATEGY: &'static str = "goal = \"never\"\ncapacity = 1\ntier = [\"x\"]\n";
        const CHORDS: ChordVocabulary = ChordVocabulary {
            held: &[0x00, 0x01],
            tap: None,
            short_hold: (2, 12),
            long_hold: (48, 120),
        };

        fn setup(&self) -> Vec<ButtonChord> {
            Vec::new()
        }

        fn genesis(&self, _state: &u8) -> Result<(), String> {
            Ok(())
        }

        fn decode(&self, _: Option<&u8>, _previous: Option<&u8>, ram: &[u8]) -> u8 {
            ram[0]
        }

        fn declare(&self, x: &u8, out: &mut impl Declarations) {
            out.declare(Assertion::Value {
                name: "x",
                value: Number::byte(*x),
            });
            out.declare(Assertion::Value {
                name: "never",
                value: Number::flag(false),
            });
        }

        fn moves(&self, x: &u8) -> Moves<u8> {
            Moves::offer(Offer {
                site: 0,
                default: 0,
                options: &STEPS,
                offered: if *x >= FAR_FROM { EVERY_OPTION } else { 0b011 },
            })
        }

        fn move_sites(&self) -> Vec<ChoiceSite> {
            vec![
                ChoiceSite::new("step", ["chord", "4", "8"].map(str::to_owned))
                    .with_contrary(CERTAIN / 4),
            ]
        }

        fn play(&self, mv: u8, _state: &u8) -> Option<Step<u8>> {
            (mv > 0).then(|| Step {
                chord: ButtonChord::new(1, 1),
                rest: (mv > 1).then(|| mv - 1),
            })
        }

        fn move_frames(&self, mv: u8) -> u64 {
            u64::from(mv)
        }

        fn answer_frames(_site: u16, label: u16) -> u64 {
            4 << label.saturating_sub(1)
        }
    }

    const LEDGE: u8 = 4;

    const CLIMB: [(u16, Play<u8>); 2] = [(0, Play::Chord), (1, Play::Move(LEDGE))];

    #[derive(Clone, Debug)]
    struct Ledge;

    impl Game for Ledge {
        type State = u8;
        type Move = u8;

        const NAME: &'static str = "ledge";
        const STRATEGY: &'static str = Walk::STRATEGY;
        const CHORDS: ChordVocabulary = Walk::CHORDS;

        fn setup(&self) -> Vec<ButtonChord> {
            Vec::new()
        }

        fn genesis(&self, _state: &u8) -> Result<(), String> {
            Ok(())
        }

        fn decode(&self, _: Option<&u8>, _previous: Option<&u8>, ram: &[u8]) -> u8 {
            ram[0]
        }

        fn declare(&self, x: &u8, out: &mut impl Declarations) {
            Walk.declare(x, out);
        }

        fn moves(&self, x: &u8) -> Moves<u8> {
            match *x {
                x if x < LEDGE => Moves::offer(Offer {
                    site: 0,
                    default: 0,
                    options: &CLIMB,
                    offered: EVERY_OPTION,
                }),
                LEDGE => Moves::forced(3),
                _ => Moves::none(),
            }
        }

        fn move_sites(&self) -> Vec<ChoiceSite> {
            vec![ChoiceSite::new("climb", ["chord", "4"].map(str::to_owned))]
        }

        fn play(&self, mv: u8, state: &u8) -> Option<Step<u8>> {
            Walk.play(mv, state)
        }

        fn move_frames(&self, mv: u8) -> u64 {
            u64::from(mv)
        }

        fn answer_frames(_site: u16, label: u16) -> u64 {
            u64::from(label) * u64::from(LEDGE)
        }
    }

    const PERCH: u8 = 3;

    const HOPS: [(u16, Play<u8>); 2] = [(0, Play::Move(1)), (1, Play::Move(2))];

    #[derive(Clone, Debug)]
    struct Perch;

    impl Game for Perch {
        type State = u8;
        type Move = u8;

        const NAME: &'static str = "perch";
        const STRATEGY: &'static str = Walk::STRATEGY;
        const CHORDS: ChordVocabulary = Walk::CHORDS;

        fn setup(&self) -> Vec<ButtonChord> {
            Vec::new()
        }

        fn genesis(&self, _state: &u8) -> Result<(), String> {
            Ok(())
        }

        fn decode(&self, _: Option<&u8>, _previous: Option<&u8>, ram: &[u8]) -> u8 {
            ram[0]
        }

        fn declare(&self, x: &u8, out: &mut impl Declarations) {
            Walk.declare(x, out);
        }

        fn moves(&self, x: &u8) -> Moves<u8> {
            if *x == PERCH {
                Moves::offer(Offer {
                    site: 0,
                    default: 0,
                    options: &HOPS,
                    offered: EVERY_OPTION,
                })
            } else {
                Moves::none()
            }
        }

        fn move_sites(&self) -> Vec<ChoiceSite> {
            vec![ChoiceSite::new("hop", ["1", "2"].map(str::to_owned))]
        }

        fn play(&self, mv: u8, state: &u8) -> Option<Step<u8>> {
            Walk.play(mv, state)
        }

        fn move_frames(&self, mv: u8) -> u64 {
            u64::from(mv)
        }
    }

    fn perched(chord: ButtonChord) -> NesTarget<Perch, Walker> {
        let lowered = Arc::new(Lowered::new(Perch, Perch::STRATEGY).expect("the perch compiles"));
        let mut target =
            NesTarget::from_power_on(lowered, Walker::default(), &[], Some(NativeMemory::of()))
                .expect("a perch target");
        target.apply(&chord);
        assert!(!target.observation().stops());
        assert_eq!(
            (target.observation().frame_count, target.observation().state),
            (3, PERCH)
        );
        assert_eq!(target.machine.wram()[0], PERCH);
        target
    }

    fn arrivals(target: &NesTarget<Ledge, Walker>) -> Vec<(u64, u8, u64)> {
        target
            .last_action_observations()
            .iter()
            .map(|arrival| {
                (
                    arrival.observation.frame_count,
                    arrival.observation.state,
                    arrival.cost,
                )
            })
            .collect()
    }

    const TRAIL: [u8; 10] = [
        RIGHT, RIGHT, RIGHT, RIGHT, RIGHT, RIGHT, LEFT, LEFT, LEFT, LEFT,
    ];

    const TRAIL_OPTIONS: [(u16, Play<usize>); 2] = [(0, Play::Chord), (1, Play::Move(0))];

    const PLAIN: &str = "capacity = 1\ngoal = \"room >= 9\"\ntier = [\"x\"]\n";
    const GOAL_INSIDE: &str = "capacity = 1\ngoal = \"x >= 6\"\ntier = [\"x\"]\n";
    const REJECT_INSIDE: &str =
        "capacity = 1\ngoal = \"room >= 9\"\nreject = \"x >= 6\"\ntier = [\"x\"]\n";
    const GOAL_AFTER_A_PEAK: &str =
        "capacity = 1\ngoal = \"highest(x >= 8) and x == 6 and room == 1\"\ntier = [\"x\"]\n";

    #[derive(Clone, Debug)]
    struct Trail;

    impl Game for Trail {
        type State = Spot;
        type Move = usize;

        const NAME: &'static str = "trail";
        const STRATEGY: &'static str = PLAIN;
        const CHORDS: ChordVocabulary = ChordVocabulary {
            held: &[RIGHT, LEFT, WARP],
            tap: None,
            short_hold: (1, 2),
            long_hold: (3, 8),
        };

        fn setup(&self) -> Vec<ButtonChord> {
            Vec::new()
        }

        fn genesis(&self, _: &Spot) -> Result<(), String> {
            Ok(())
        }

        fn decode(&self, parent: Option<&Spot>, previous: Option<&Spot>, ram: &[u8]) -> Spot {
            FixtureLedge.decode(parent, previous, ram)
        }

        fn declare(&self, state: &Spot, out: &mut impl Declarations) {
            FixtureLedge.declare(state, out);
        }

        fn moves(&self, spot: &Spot) -> Moves<usize> {
            match (spot.room, spot.x) {
                (0 | 1, x) if x < 6 => Moves::offer(Offer {
                    site: 0,
                    default: 0,
                    options: &TRAIL_OPTIONS,
                    offered: EVERY_OPTION,
                }),
                (2, _) => Moves::forced(0),
                _ => Moves::none(),
            }
        }

        fn move_sites(&self) -> Vec<ChoiceSite> {
            vec![ChoiceSite::new(
                "trail",
                ["chord", "out and back"].map(str::to_owned),
            )]
        }

        fn play(&self, at: usize, _: &Spot) -> Option<Step<usize>> {
            TRAIL.get(at).map(|&buttons| Step {
                chord: ButtonChord::new(buttons, 1),
                rest: (at + 1 < TRAIL.len()).then_some(at + 1),
            })
        }

        fn move_frames(&self, _: usize) -> u64 {
            TRAIL.len() as u64
        }

        fn answer_frames(_site: u16, label: u16) -> u64 {
            if label == 1 { TRAIL.len() as u64 } else { 1 }
        }
    }

    fn trail(strategy: &str) -> NesTarget<Trail, Stepper> {
        let lowered = Arc::new(Lowered::new(Trail, strategy).expect("the trail compiles"));
        NesTarget::from_machine(lowered, Stepper::default(), Some(NativeMemory::of()))
            .expect("a trail target")
    }

    #[cfg(unix)]
    #[derive(Clone, Debug)]
    struct Primed;

    #[cfg(unix)]
    impl Game for Primed {
        type State = Spot;
        type Move = usize;

        const NAME: &'static str = "primed";
        const STRATEGY: &'static str = PLAIN;
        const CHORDS: ChordVocabulary = Trail::CHORDS;

        fn setup(&self) -> Vec<ButtonChord> {
            vec![ButtonChord::new(RIGHT, 2)]
        }

        fn setup_pokes(&self) -> Vec<(usize, u8)> {
            vec![(1, 1)]
        }

        fn start(&self) -> Vec<ButtonChord> {
            vec![ButtonChord::new(LEFT, 1)]
        }

        fn genesis(&self, _: &Spot) -> Result<(), String> {
            Ok(())
        }

        fn decode(&self, parent: Option<&Spot>, previous: Option<&Spot>, ram: &[u8]) -> Spot {
            Trail.decode(parent, previous, ram)
        }

        fn declare(&self, state: &Spot, out: &mut impl Declarations) {
            Trail.declare(state, out);
        }

        fn moves(&self, spot: &Spot) -> Moves<usize> {
            Trail.moves(spot)
        }

        fn move_sites(&self) -> Vec<ChoiceSite> {
            Trail.move_sites()
        }

        fn play(&self, at: usize, spot: &Spot) -> Option<Step<usize>> {
            Trail.play(at, spot)
        }

        fn move_frames(&self, at: usize) -> u64 {
            Trail.move_frames(at)
        }

        fn answer_frames(site: u16, label: u16) -> u64 {
            Trail::answer_frames(site, label)
        }
    }

    #[cfg(unix)]
    fn primed() -> NesWorkload<Primed, Stepper> {
        NesWorkload::with_machines(
            Primed,
            PLAIN,
            &[],
            "primed",
            "0",
            Arc::new(|| Ok(Stepper::default())),
        )
        .expect("a primed workload")
    }

    #[cfg(unix)]
    fn from_genesis(
        workload: &NesWorkload<Primed, Stepper>,
        actions: &[ButtonChord],
    ) -> (u64, Vec<u8>, Spot) {
        let mut target = workload.new_target().expect("a primed target");
        let (mut video, mut audio) = (Vec::new(), Vec::new());
        let (metadata, endpoint) = target
            .render_input(actions, 1, 0, &mut video, &mut audio)
            .expect("a film from the genesis");
        (metadata.frames, video, endpoint)
    }

    #[cfg(unix)]
    #[test]
    fn a_film_from_power_on_shows_the_setup_and_start_before_the_tape() {
        let workload = primed();
        let actions = [walk_out_and_back(), ButtonChord::new(RIGHT, 2)];
        let (genesis_frames, genesis_video, genesis_endpoint) = from_genesis(&workload, &actions);
        let (mut video, mut audio) = (Vec::new(), Vec::new());
        let (metadata, endpoint) = workload
            .render_from_power_on(&actions, 1, 0, &mut video, &mut audio)
            .expect("a film from power-on");
        let mut expected = vec![1, 0, 0, 2, 0, 0, 1, 1, 0];
        expected.extend(&genesis_video);
        assert_eq!(video, expected);
        assert_eq!(metadata.frames, 3 + genesis_frames);
        assert_eq!(metadata.skipped_frames, 0);
        assert_eq!(metadata.audio_frames, metadata.frames);
        assert_eq!(endpoint.state, genesis_endpoint);
        assert_eq!(
            workload.preroll().expect("a preroll"),
            Preroll {
                setup_frames: 2,
                setup_pokes: vec![SetupPoke {
                    address: 1,
                    value: 1
                }],
                start_frames: 1,
                root_actions: 0,
                root_frames: 0,
            }
        );

        let (mut clipped, mut clipped_audio) = (Vec::new(), Vec::new());
        let (skipped, _) = workload
            .render_from_power_on(&actions, 1, 2, &mut clipped, &mut clipped_audio)
            .expect("a clipped film from power-on");
        assert_eq!(skipped.skipped_frames, 2);
        assert_eq!(skipped.frames, metadata.frames - 2);
        assert_eq!(clipped, video[6..]);
    }

    #[cfg(unix)]
    #[test]
    fn a_rooted_film_from_power_on_shows_the_root_after_the_start() {
        let root = [ButtonChord::new(RIGHT, 2)];
        let actions = [ButtonChord::new(RIGHT, 3)];
        let unrooted = primed();
        let mut tape = root.to_vec();
        tape.extend(actions);
        let (_, genesis_video, genesis_endpoint) = from_genesis(&unrooted, &tape);
        let rooted = primed()
            .with_root(root.to_vec())
            .expect("a rooted workload");
        let (mut video, mut audio) = (Vec::new(), Vec::new());
        let (metadata, endpoint) = rooted
            .render_from_power_on(&actions, 1, 0, &mut video, &mut audio)
            .expect("a rooted film from power-on");
        let mut expected = vec![1, 0, 0, 2, 0, 0, 1, 1, 0];
        expected.extend(&genesis_video);
        assert_eq!(video, expected);
        assert_eq!(video[9..15], [2, 1, 0, 3, 1, 0]);
        assert_eq!(endpoint.state, genesis_endpoint);
        let preroll = rooted.preroll().expect("a preroll");
        assert_eq!((preroll.root_actions, preroll.root_frames), (1, 2));
        assert_eq!(
            metadata.frames,
            preroll.setup_frames + preroll.start_frames + preroll.root_frames + 3 + 1
        );
    }

    fn spot(target: &NesTarget<Trail, Stepper>) -> (u64, u8, u8) {
        let observation = target.observation();
        (
            observation.frame_count,
            observation.state.x,
            observation.state.room,
        )
    }

    fn costs(target: &NesTarget<Trail, Stepper>) -> Vec<u64> {
        target
            .last_action_observations()
            .iter()
            .map(|arrival| arrival.cost)
            .collect()
    }

    fn folded(target: &NesTarget<Trail, Stepper>, before: &Accumulator, at: Spot) -> Accumulator {
        let mut accumulator = before.clone();
        let _ = target.lowered.observe(0, at, &mut accumulator);
        accumulator
    }

    fn assert_stays(target: &mut NesTarget<Trail, Stepper>) {
        let (observation, work) = (target.observation().clone(), target.execution_work());
        target.apply(&ButtonChord::new(RIGHT, 1));
        assert_eq!(target.observation(), &observation);
        assert_eq!(target.execution_work(), work);
        assert!(target.last_action_observations().is_empty());
    }

    fn walk_out_and_back() -> ButtonChord {
        move_chord(0, 1).expect("a move chord")
    }

    fn target() -> NesTarget<Walk, Walker> {
        let lowered = Arc::new(Lowered::new(Walk, Walk::STRATEGY).expect("the walk compiles"));
        NesTarget::from_power_on(lowered, Walker::default(), &[], Some(NativeMemory::of()))
            .expect("a walk target")
    }

    fn at(target: &NesTarget<Walk, Walker>) -> (u64, u8) {
        (target.observation().frame_count, *target.state())
    }

    #[test]
    fn a_chord_at_an_offer_beside_chords_plays_its_whole_hold_and_a_move_chord_its_move() {
        let mut target = target();
        target.apply(&ButtonChord::new(1, 6));
        assert_eq!(at(&target), (6, 6));
        target.apply(&move_chord(0, 1).expect("a move chord"));
        assert_eq!(at(&target), (10, 10));
        target.apply(&move_chord(0, 2).expect("a move chord"));
        assert_eq!(at(&target), (18, 18));
        target.apply(&ButtonChord::new(0, 3));
        assert_eq!(at(&target), (21, 18));
    }

    #[test]
    fn an_offered_move_that_ends_where_a_forced_move_starts_hands_off_as_a_chord_does() {
        let lowered = Arc::new(Lowered::new(Ledge, Ledge::STRATEGY).expect("the ledge compiles"));
        let mut by_move = NesTarget::from_power_on(
            Arc::clone(&lowered),
            Walker::default(),
            &[],
            Some(NativeMemory::of()),
        )
        .expect("a ledge target");
        by_move.apply(&move_chord(0, 1).expect("a move chord"));
        assert_eq!(arrivals(&by_move), [(4, 4, 0), (7, 7, 7)]);
        assert_eq!(by_move.execution_work(), 7);
        let mut by_chord =
            NesTarget::from_power_on(lowered, Walker::default(), &[], Some(NativeMemory::of()))
                .expect("a ledge target");
        by_chord.apply(&ButtonChord::new(1, 6));
        assert_eq!(arrivals(&by_chord)[3..], [(4, 4, 0), (7, 7, 7)]);
        assert_eq!(by_chord.execution_work(), by_move.execution_work() + 2);
        assert_eq!(by_chord.observation(), by_move.observation());
    }

    #[test]
    fn a_chord_cut_at_an_offer_replays_to_the_cut_frame_before_the_move_plays() {
        let mut target = perched(ButtonChord::new(1, 6));
        assert_eq!(target.execution_work(), 6);
        target.apply(&move_chord(0, 1).expect("a move chord"));
        assert_eq!(
            (target.observation().frame_count, target.observation().state),
            (5, 5)
        );
        assert_eq!(target.machine.wram()[0], 5);
        assert_eq!(target.execution_work(), 8);
        assert_eq!(target.exit_kind(), ExitKind::Ok);
    }

    #[test]
    fn a_snapshot_after_a_chord_cut_at_an_offer_equals_the_snapshot_of_the_cut_chord() {
        let mut cut = perched(ButtonChord::new(1, 6));
        let mut exact = perched(ButtonChord::new(1, 3));
        assert_eq!(
            postcard::to_allocvec(&cut.snapshot().expect("a snapshot")).unwrap(),
            postcard::to_allocvec(&exact.snapshot().expect("a snapshot")).unwrap()
        );
        assert_eq!(cut.exit_kind(), ExitKind::Ok);
    }

    #[test]
    fn an_offered_move_without_a_stop_walks_its_whole_trail() {
        let mut target = trail(PLAIN);
        let before = target.accumulator.clone();
        target.apply(&walk_out_and_back());
        assert_eq!(spot(&target), (10, 2, 0));
        assert_eq!(costs(&target), [10]);
        assert_eq!(
            target.accumulator,
            folded(&target, &before, Spot { x: 2, room: 0 })
        );
    }

    #[test]
    fn a_goal_inside_an_offered_move_ends_the_move_at_that_step() {
        let mut target = trail(GOAL_INSIDE);
        let before = target.accumulator.clone();
        target.apply(&walk_out_and_back());
        assert!(target.observation().goal);
        assert_eq!(spot(&target), (6, 6, 0));
        assert_eq!(costs(&target), [6]);
        assert_eq!(target.execution_work(), 6);
        assert_eq!(
            target.accumulator,
            folded(&target, &before, Spot { x: 6, room: 0 })
        );
        assert_stays(&mut target);
    }

    #[test]
    fn a_reject_inside_an_offered_move_ends_the_move_at_that_step() {
        let mut target = trail(REJECT_INSIDE);
        let before = target.accumulator.clone();
        target.apply(&walk_out_and_back());
        assert!(target.observation().rejected.is_some());
        assert_eq!(spot(&target), (6, 6, 0));
        assert_eq!(costs(&target), [6]);
        assert_eq!(target.execution_work(), 6);
        assert_eq!(
            target.accumulator,
            folded(&target, &before, Spot { x: 6, room: 0 })
        );
        assert_stays(&mut target);
    }

    #[test]
    fn a_goal_inside_a_forced_move_after_a_chord_ends_it_and_no_forced_move_follows() {
        let mut target = trail(GOAL_INSIDE);
        target.apply(&ButtonChord::new(WARP, 1));
        assert_eq!(spot(&target), (1, 0, 1));
        let before = target.accumulator.clone();
        target.apply(&ButtonChord::new(WARP, 1));
        assert!(target.observation().goal);
        assert_eq!(spot(&target), (8, 6, 2));
        assert_eq!(costs(&target), [0, 7]);
        assert_eq!(target.execution_work(), 8);
        let mut expected = before;
        let _ = target
            .lowered
            .observe(0, Spot { x: 0, room: 2 }, &mut expected);
        assert_eq!(
            target.accumulator,
            folded(&target, &expected, Spot { x: 6, room: 2 })
        );
        assert_stays(&mut target);
    }

    #[test]
    fn a_step_is_judged_against_the_accumulator_at_the_move_departure() {
        let mut climbed = trail(GOAL_AFTER_A_PEAK);
        climbed.apply(&ButtonChord::new(RIGHT, 8));
        climbed.apply(&ButtonChord::new(WARP, 1));
        assert_eq!(spot(&climbed), (9, 0, 1));
        assert!(!climbed.observation().goal);
        let before = climbed.accumulator.clone();
        climbed.apply(&walk_out_and_back());
        assert!(climbed.observation().goal);
        assert_eq!(spot(&climbed), (15, 6, 1));
        assert_eq!(costs(&climbed), [6]);
        assert_eq!(climbed.execution_work(), 15);
        assert_eq!(
            climbed.accumulator,
            folded(&climbed, &before, Spot { x: 6, room: 1 })
        );
        assert_stays(&mut climbed);

        let mut flat = trail(GOAL_AFTER_A_PEAK);
        flat.apply(&ButtonChord::new(WARP, 1));
        let before = flat.accumulator.clone();
        flat.apply(&walk_out_and_back());
        assert!(!flat.observation().goal);
        assert_eq!(spot(&flat), (11, 2, 1));
        assert_eq!(costs(&flat), [10]);
        assert_eq!(
            flat.accumulator,
            folded(&flat, &before, Spot { x: 2, room: 1 })
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_film_renders_the_frames_the_target_plays_and_cuts_a_move_where_it_stops() {
        let film = |strategy: &str, actions: &[ButtonChord]| {
            let mut replay = trail(strategy);
            for action in actions {
                replay.apply(action);
            }
            let mut target = trail(strategy);
            let (mut video, mut audio) = (Vec::new(), Vec::new());
            let (metadata, endpoint) = target
                .render_input(actions, 0, 0, &mut video, &mut audio)
                .expect("a film");
            assert_eq!(metadata.frames, replay.observation().frame_count);
            let last = video.chunks(3).last().map(<[u8]>::to_vec);
            (metadata.frames, endpoint, last)
        };
        let out_and_back = [walk_out_and_back()];
        let warps = [ButtonChord::new(WARP, 1), ButtonChord::new(WARP, 1)];
        assert_eq!(
            film(GOAL_INSIDE, &out_and_back),
            (6, Spot { x: 6, room: 0 }, Some(vec![6, 0, 0]))
        );
        assert_eq!(
            film(PLAIN, &out_and_back),
            (10, Spot { x: 2, room: 0 }, Some(vec![2, 0, 0]))
        );
        assert_eq!(
            film(GOAL_INSIDE, &warps),
            (8, Spot { x: 6, room: 2 }, Some(vec![6, 2, 0]))
        );
    }

    #[test]
    fn a_move_chord_for_a_masked_label_is_not_played_as_its_move() {
        let mut target = target();
        target.apply(&move_chord(0, 2).expect("a move chord"));
        assert_eq!(at(&target), (u64::from(super::MAX_HOLD_FRAMES), 0));
    }

    #[test]
    fn a_job_asks_the_move_site_and_resolves_the_chord_tree_when_the_chord_is_taken() {
        let workload = NesWorkload::with_machines(
            Walk,
            Walk::STRATEGY,
            &[],
            "walk",
            "0",
            Arc::new(|| Ok(Walker::default())),
        )
        .expect("a walk workload");
        let mut target = workload.new_target().expect("a walk target");
        let origin = target.snapshot().expect("a genesis snapshot");
        let plan = ChoicePlan {
            profile: Walk::CHORDS.reproducing_profile(),
            redrawn: false,
            seed: 11,
            untried: None,
            untried_site: 0,
            held: Vec::new(),
            declared: ChoiceSite::declared_contrary(&workload.lowered().choice_sites()),
            permuted: Vec::new(),
        };
        assert_eq!(plan.declared, [(CHORD_SITES, CERTAIN / 4)]);
        let suffix = vec![chord::ask(None); 400];
        let result = workload
            .execute_job(
                &NesRun,
                &mut target,
                &origin,
                &[],
                Satisfied::default(),
                &suffix,
                RetentionPolicy::Unprobed,
                RolloutStop::new(false).with_choices(Some(plan)),
            )
            .expect("a job");
        assert_eq!(result.actions.len(), suffix.len());
        let mut x = 0_u8;
        let mut moves = BTreeMap::new();
        for action in &result.actions {
            let records: Vec<_> = action
                .observations
                .iter()
                .flat_map(|observation| observation.choices.iter())
                .collect();
            let offered: &[u16] = if x >= FAR_FROM { &[0, 1, 2] } else { &[0, 1] };
            assert_eq!(records[0].site, CHORD_SITES);
            assert_eq!(records[0].offered, offered);
            match move_answer(&action.action) {
                Some((0, label)) => {
                    assert_eq!(records.len(), 1);
                    assert_eq!(records[0].taken, Choice::Option(label));
                    assert!(offered.contains(&label));
                    *moves.entry(label).or_insert(0_u32) += 1;
                }
                Some(other) => panic!("{other:?}"),
                None => {
                    assert_eq!(records[0].taken, Choice::Default);
                    assert!(records[1..].iter().all(|record| record.site < CHORD_SITES));
                    assert!(Walk::CHORDS.held.contains(&action.action.buttons));
                }
            }
            x = action
                .observations
                .last()
                .expect("an arrival")
                .observation
                .state;
        }
        let taken: u32 = moves.values().sum();
        assert!((60..140).contains(&taken), "{moves:?}");
        assert_eq!(moves.keys().copied().collect::<Vec<_>>(), [1, 2]);
    }

    #[test]
    fn the_stall_report_reads_an_action_endpoint_record_and_its_choice_records() {
        let workload = NesWorkload::with_machines(
            Walk,
            Walk::STRATEGY,
            &[],
            "walk",
            "0",
            Arc::new(|| Ok(Walker::default())),
        )
        .expect("a walk workload");
        let lowered = workload.lowered();
        let names: Vec<String> = lowered
            .catalog
            .signals()
            .iter()
            .map(|signal| signal.name.clone())
            .collect();
        assert!(!names.is_empty());
        assert_eq!(workload.catalog_signals(&NesRun), names);
        let mut target = workload.new_target().expect("a walk target");
        let origin = target.snapshot().expect("a genesis snapshot");
        let plan = ChoicePlan {
            profile: Walk::CHORDS.reproducing_profile(),
            redrawn: false,
            seed: 11,
            untried: None,
            untried_site: 0,
            held: Vec::new(),
            declared: ChoiceSite::declared_contrary(&lowered.choice_sites()),
            permuted: Vec::new(),
        };
        let suffix = vec![chord::ask(None); 40];
        let result = workload
            .execute_job(
                &NesRun,
                &mut target,
                &origin,
                &[],
                Satisfied::default(),
                &suffix,
                RetentionPolicy::Unprobed,
                RolloutStop::new(false).with_choices(Some(plan)),
            )
            .expect("a job");
        let mut before = workload
            .state_record(&NesRun, &origin)
            .expect("a genesis record");
        assert_eq!(before, lowered.record(&origin.observation.state).0);
        let mut moved = 0;
        for action in &result.actions {
            let last = action.observations.last().expect("an arrival");
            let after = workload
                .observation_record(&NesRun, last)
                .expect("an endpoint record");
            assert_eq!(after, lowered.record(&last.observation.state).0);
            assert_eq!(after.values().len(), names.len());
            moved += usize::from(after.values() != before.values());
            before = after;
            let records = workload.choice_records(&NesRun, &action.action, &action.observations);
            let expected: Vec<_> = action
                .observations
                .iter()
                .flat_map(|observation| observation.choices.iter().cloned())
                .collect();
            assert!(!records.is_empty());
            assert_eq!(records, expected);
        }
        assert!(moved > 0);
    }

    fn plain_job<G: Game<State = u8>>(game: G, suffix: &[ButtonChord]) -> Vec<(ButtonChord, u8)> {
        let workload = NesWorkload::with_machines(
            game,
            G::STRATEGY,
            &[],
            "walk",
            "0",
            Arc::new(|| Ok(Walker::default())),
        )
        .expect("a walk workload");
        let mut target = workload.new_target().expect("a walk target");
        let origin = target.snapshot().expect("a genesis snapshot");
        workload
            .execute_job(
                &NesRun,
                &mut target,
                &origin,
                &[],
                Satisfied::default(),
                suffix,
                RetentionPolicy::Unprobed,
                RolloutStop::new(false),
            )
            .expect("a job without a plan")
            .actions
            .iter()
            .map(|action| {
                let x = action
                    .observations
                    .last()
                    .expect("an arrival")
                    .observation
                    .state;
                (action.action, x)
            })
            .collect()
    }

    #[test]
    fn a_continuation_tail_whose_move_meets_no_offer_is_cut_there() {
        let climb = move_chord(0, 1).expect("a move chord");
        assert_eq!(plain_job(Ledge, &[climb]), [(climb, LEDGE + 3)]);
        assert_eq!(
            plain_job(
                Ledge,
                &[ButtonChord::new(1, 8), climb, ButtonChord::new(1, 2)]
            ),
            [(ButtonChord::new(1, 8), LEDGE + 3)]
        );
    }

    #[test]
    fn a_shrink_candidate_whose_move_is_not_offered_ends_before_it() {
        let far = move_chord(0, 2).expect("a move chord");
        let witness = plain_job(Walk, &[ButtonChord::new(1, FAR_FROM), far]);
        assert_eq!(
            witness,
            [
                (ButtonChord::new(1, FAR_FROM), FAR_FROM),
                (far, FAR_FROM + 8)
            ]
        );
        assert!(plain_job(Walk, &[far]).is_empty());
    }

    #[test]
    fn a_continuation_tail_whose_move_is_not_offered_is_cut_there() {
        let far = move_chord(0, 2).expect("a move chord");
        assert_eq!(
            plain_job(Walk, &[ButtonChord::new(1, 3), far, ButtonChord::new(1, 4)]),
            [(ButtonChord::new(1, 3), 3)]
        );
    }
}
