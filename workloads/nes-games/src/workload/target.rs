// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(unix)]
use std::io::Write;
use std::{error::Error, sync::Arc};

#[cfg(unix)]
use machine::nes::MAX_HOLD_FRAMES;
#[cfg(unix)]
use machine::quicknes::{QUICKNES_AUDIO_CHANNELS, QUICKNES_AUDIO_SAMPLE_RATE, QuickNesMachine};
use machine::{
    Machine, MachineError, SnapId, StopConditions,
    nes::{self, ButtonChord, WRAM_SIZE},
};
use serde::{Deserialize, Serialize};

use super::{Game, Lowered, Satisfied};
use crate::{
    nes_backend::{NesBackend, NesMemory},
    search::assertion::{Accumulator, AssertionKey, Record},
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
#[serde(transparent)]
pub struct ActionObservation<S> {
    pub observation: NesObservation<S>,
    #[serde(skip)]
    pub cost: u64,
    #[serde(skip)]
    pub records: Vec<Record>,
}

impl<S> From<NesObservation<S>> for ActionObservation<S> {
    fn from(observation: NesObservation<S>) -> Self {
        Self {
            observation,
            cost: 0,
            records: Vec::new(),
        }
    }
}

enum Played<S> {
    Arrived {
        frames: u64,
        wram: [u8; WRAM_SIZE],
        state: S,
    },
    Overran {
        frames: u64,
        wram: [u8; WRAM_SIZE],
        state: S,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NesSnapshot<S> {
    pub emulator_state: Vec<u8>,
    pub observation: NesObservation<S>,
    pub accumulator: Accumulator,
    pub failed: bool,
}

impl<S: Clone> searcher::search::snapshot_codec::CheckpointSnapshot for NesSnapshot<S>
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
        budget.array::<u8>(self.emulator_state.capacity())
    }
    fn freeze_checkpoint(&self) -> Result<Self, Box<dyn Error>> {
        Ok(self.clone())
    }
}

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

#[derive(Debug)]
pub struct NesTarget<G: Game, M: NesBackend<Vec<u8>>> {
    lowered: Arc<Lowered<G>>,
    machine: M,
    genesis: SnapId,
    current: SnapId,
    genesis_observation: NesObservation<G::State>,
    genesis_wram: [u8; WRAM_SIZE],
    current_wram: [u8; WRAM_SIZE],
    genesis_accumulator: Accumulator,
    accumulator: Accumulator,
    observation: NesObservation<G::State>,
    action_observations: Vec<ActionObservation<G::State>>,
    failed: bool,
    snapshot_base: Option<Vec<u8>>,
    execution_work: u64,
    recording: bool,
}

impl<G: Game, M: NesBackend<Vec<u8>>> NesTarget<G, M> {
    pub fn from_machine(lowered: Arc<Lowered<G>>, mut machine: M) -> Result<Self, MachineError> {
        let wram = read_wram(&machine)?;
        let state = lowered.game.decode(None, &wram);
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
            genesis_observation: observation.clone(),
            genesis_wram: wram,
            current_wram: wram,
            genesis_accumulator: accumulator.clone(),
            accumulator,
            observation: observation.clone(),
            action_observations: vec![observation.into()],
            failed: false,
            snapshot_base: None,
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
    pub fn execution_work(&self) -> u64 {
        self.execution_work
    }

    #[must_use]
    pub fn wram(&self) -> &[u8; WRAM_SIZE] {
        &self.current_wram
    }

    pub fn record_frames(&mut self, recording: bool) {
        self.recording = recording;
    }

    #[must_use]
    pub fn last_action_observations(&self) -> &[ActionObservation<G::State>] {
        &self.action_observations
    }

    pub fn snapshot_of_state(
        &mut self,
        state: &Vec<u8>,
        frame_count: u64,
    ) -> Result<NesSnapshot<G::State>, Box<dyn Error>> {
        self.machine
            .restore_nes(state)
            .map_err(|error| error.to_string())?;
        let wram = read_wram(&self.machine)?;
        let decoded = self.lowered.game.decode(None, &wram);
        let next = self.machine.snapshot()?;
        if self.current != self.genesis {
            self.machine.drop_snapshot(self.current)?;
        }
        self.current = next;
        self.current_wram = wram;
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

    fn fail(&mut self) {
        self.failed = true;
    }

    fn play_forced(
        &mut self,
        mut mv: G::Move,
        mut state: G::State,
        mut wram: [u8; WRAM_SIZE],
    ) -> Result<Played<G::State>, MachineError> {
        let bound = self.lowered.game.move_frames(mv);
        let mut frames = 0_u64;
        while let Some(step) = self.lowered.game.play(mv, &state) {
            let pressed = press(&mut self.machine, step.chord)?;
            self.execution_work = self.execution_work.saturating_add(pressed);
            frames = frames.saturating_add(pressed);
            wram = read_wram(&self.machine)?;
            state = self.lowered.game.decode(Some(&state), &wram);
            if frames > bound {
                return Ok(Played::Overran {
                    frames,
                    wram,
                    state,
                });
            }
            match step.rest {
                Some(next) => mv = next,
                None => break,
            }
        }
        Ok(Played::Arrived {
            frames,
            wram,
            state,
        })
    }

    fn apply_internal(&mut self, action: &ButtonChord) {
        self.action_observations.clear();
        if self.failed || self.observation.stops() {
            return;
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
        let mut previous = self.observation.state.clone();
        let mut accumulator = std::mem::take(&mut self.accumulator);
        let mut endpoint = None;
        let mut records = Vec::new();
        let mut logical_frames = produced_frames;
        let mut forced = None;
        for (offset, wram) in self.machine.frames().iter().enumerate() {
            let state = self.lowered.game.decode(Some(&previous), wram);
            let frame_count = self
                .observation
                .frame_count
                .saturating_add(u64::try_from(offset).unwrap_or(u64::MAX).saturating_add(1));
            if self.recording {
                records.push(self.lowered.record(&state).0);
            }
            let observation = self.lowered.observe(frame_count, state, &mut accumulator);
            previous = observation.state.clone();
            if !observation.stops() {
                forced = self.lowered.game.moves(&observation.state).forced;
            }
            let last = offset + 1 == produced_frames || observation.stops() || forced.is_some();
            if observation.key != prior_key || observation.stops() || last {
                prior_key = observation.key;
                emitted.push(ActionObservation {
                    observation,
                    cost: 0,
                    records: std::mem::take(&mut records),
                });
            }
            if last {
                logical_frames = offset + 1;
                endpoint = Some(*wram);
                break;
            }
        }
        let Some(endpoint_wram) = endpoint else {
            return self.fail();
        };
        if logical_frames < produced_frames {
            let Ok(hold) = u8::try_from(logical_frames) else {
                return self.fail();
            };
            if self.machine.replay(start).is_err()
                || self
                    .machine
                    .branch(
                        start,
                        &nes::reproducer(std::slice::from_ref(&ButtonChord::new(
                            action.buttons,
                            hold,
                        ))),
                    )
                    .is_err()
            {
                return self.fail();
            }
            let replay = self.machine.run(StopConditions::default(), None);
            let replayed_wram = read_wram(&self.machine);
            if !matches!(
                replay,
                Ok(machine::StopReason::Quiescent { .. }
                    | machine::StopReason::SnapshotPoint { .. })
            ) || self.machine.frames().len() != logical_frames
                || replayed_wram.as_ref().ok() != Some(&endpoint_wram)
            {
                return self.fail();
            }
        }
        let mut current_wram = endpoint_wram;
        let mut cost = u64::try_from(logical_frames).unwrap_or(u64::MAX);
        if let Some(mv) = forced {
            let Some(departure) = emitted.last().map(|last| last.observation.clone()) else {
                return self.fail();
            };
            let (frames, wram, state, overran) =
                match self.play_forced(mv, departure.state.clone(), endpoint_wram) {
                    Ok(Played::Arrived {
                        frames,
                        wram,
                        state,
                    }) => (frames, wram, state, false),
                    Ok(Played::Overran {
                        frames,
                        wram,
                        state,
                    }) => (frames, wram, state, true),
                    Err(_) => return self.fail(),
                };
            let frame_count = departure.frame_count.saturating_add(frames);
            cost = cost.saturating_add(frames);
            let arrival = if overran {
                NesObservation {
                    frame_count,
                    state,
                    key: None,
                    overran: true,
                    ..departure
                }
                .into()
            } else {
                if self.recording {
                    records.push(self.lowered.record(&state).0);
                }
                ActionObservation {
                    observation: self.lowered.observe(frame_count, state, &mut accumulator),
                    cost: 0,
                    records,
                }
            };
            emitted.push(arrival);
            current_wram = wram;
        }
        if let Some(last) = emitted.last_mut() {
            last.cost = cost;
        }
        self.action_observations = emitted;
        if let Some(last) = self.action_observations.last() {
            self.observation = last.observation.clone();
        }
        self.accumulator = accumulator;
        self.current_wram = current_wram;
        let Ok(next) = self.machine.snapshot() else {
            return self.fail();
        };
        if start != self.genesis && self.machine.drop_snapshot(start).is_err() {
            let _ = self.machine.drop_snapshot(next);
            return self.fail();
        }
        self.current = next;
    }
}

impl<G: Game, M: NesBackend<Vec<u8>> + NesMemory> NesTarget<G, M> {
    pub fn from_power_on(lowered: Arc<Lowered<G>>, mut machine: M) -> Result<Self, MachineError> {
        run_setup::<G, M>(&mut machine, &lowered.game.setup())?;
        for (address, value) in lowered.game.setup_pokes() {
            machine.poke_wram(address, value)?;
        }
        let start = lowered.game.start();
        if !start.is_empty() {
            run_setup::<G, M>(&mut machine, &start)?;
        }
        Self::from_machine(lowered, machine)
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
    let from = machine.snapshot()?;
    machine.branch(from, &nes::reproducer(tape))?;
    let run = machine.run(StopConditions::default(), None)?;
    if !matches!(run, machine::StopReason::Quiescent { .. }) {
        return Err(MachineError::Backend(format!(
            "{} setup did not quiesce",
            G::NAME
        )));
    }
    machine.drop_snapshot(from)
}

#[cfg(unix)]
impl<G: Game> NesTarget<G, QuickNesMachine> {
    pub fn render_input(
        &mut self,
        actions: &[ButtonChord],
        tail_frames: u32,
        skip_frames: u64,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<(VideoMetadata, G::State), Box<dyn Error>> {
        self.reset();
        if self.failed {
            return Err(format!("could not restore the {} genesis for film", G::NAME).into());
        }
        self.machine.set_video_capture(true);
        self.machine.set_audio_capture(true);
        let result = (|| {
            let mut metadata = None;
            let mut skip = skip_frames;
            let mut accumulator = self.genesis_accumulator.clone();
            for action in actions {
                let forced = self.render_action(
                    *action,
                    Some(&mut accumulator),
                    video_output,
                    audio_output,
                    &mut metadata,
                    &mut skip,
                )?;
                if let Some(mut mv) = forced {
                    let bound = self.lowered.game.move_frames(mv);
                    let mut frames = 0_u64;
                    let mut state = self.lowered.game.decode(None, &read_wram(&self.machine)?);
                    while let Some(step) = self.lowered.game.play(mv, &state) {
                        self.render_action(
                            step.chord,
                            None,
                            video_output,
                            audio_output,
                            &mut metadata,
                            &mut skip,
                        )?;
                        frames = frames.saturating_add(u64::from(step.chord.bounded_hold_frames()));
                        state = self
                            .lowered
                            .game
                            .decode(Some(&state), &read_wram(&self.machine)?);
                        if frames > bound {
                            return Err(format!(
                                "{} forced move {mv:?} ran past its {bound} frames in the film",
                                G::NAME
                            )
                            .into());
                        }
                        match step.rest {
                            Some(next) => mv = next,
                            None => break,
                        }
                    }
                    let _ = self.lowered.observe(0, state, &mut accumulator);
                }
            }
            let endpoint = self.lowered.game.decode(None, &read_wram(&self.machine)?);
            let mut remaining = tail_frames;
            while remaining > 0 {
                let hold = remaining.min(u32::from(MAX_HOLD_FRAMES));
                self.render_action(
                    ButtonChord::new(0, u8::try_from(hold)?),
                    None,
                    video_output,
                    audio_output,
                    &mut metadata,
                    &mut skip,
                )?;
                remaining -= hold;
            }
            let mut metadata: VideoMetadata =
                metadata.ok_or("QuickNES produced no video frames")?;
            if metadata.audio_frames == 0 {
                return Err("QuickNES produced no audio samples".into());
            }
            metadata.skipped_frames = skip_frames - skip;
            Ok((metadata, endpoint))
        })();
        self.machine.set_audio_capture(false);
        self.machine.set_video_capture(false);
        result
    }

    fn render_action(
        &mut self,
        action: ButtonChord,
        mut accumulator: Option<&mut Accumulator>,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
        metadata: &mut Option<VideoMetadata>,
        skip: &mut u64,
    ) -> Result<Option<G::Move>, Box<dyn Error>> {
        let start = self.machine.snapshot()?;
        self.machine
            .branch(start, &nes::reproducer(std::slice::from_ref(&action)))?;
        self.machine.drop_snapshot(start)?;
        for _ in 0..action.bounded_hold_frames() {
            if !self.run_one_frame()? {
                break;
            }
            let frame = self
                .machine
                .take_video_frame()
                .ok_or("QuickNES omitted a requested video frame")?;
            if *skip > 0 {
                *skip -= 1;
                self.machine.take_audio_samples();
                if let Some(mv) = self.film_forced(accumulator.as_deref_mut())? {
                    return Ok(Some(mv));
                }
                continue;
            }
            match metadata {
                Some(existing)
                    if (existing.width, existing.height) != (frame.width, frame.height) =>
                {
                    return Err("QuickNES video geometry changed during replay".into());
                }
                Some(existing) => existing.frames = existing.frames.saturating_add(1),
                None => {
                    *metadata = Some(VideoMetadata {
                        width: frame.width,
                        height: frame.height,
                        frames: 1,
                        audio_sample_rate: QUICKNES_AUDIO_SAMPLE_RATE,
                        audio_channels: QUICKNES_AUDIO_CHANNELS,
                        audio_frames: 0,
                        skipped_frames: 0,
                    });
                }
            }
            video_output.write_all(&frame.rgb24)?;
            let audio = self.machine.take_audio_samples();
            if !audio
                .len()
                .is_multiple_of(usize::from(QUICKNES_AUDIO_CHANNELS))
            {
                return Err("QuickNES produced a partial stereo audio frame".into());
            }
            for sample in &audio {
                audio_output.write_all(&sample.to_le_bytes())?;
            }
            if let Some(existing) = metadata {
                existing.audio_frames = existing.audio_frames.saturating_add(u64::try_from(
                    audio.len() / usize::from(QUICKNES_AUDIO_CHANNELS),
                )?);
            }
            if let Some(mv) = self.film_forced(accumulator.as_deref_mut())? {
                return Ok(Some(mv));
            }
        }
        Ok(None)
    }

    fn film_forced(
        &self,
        accumulator: Option<&mut Accumulator>,
    ) -> Result<Option<G::Move>, Box<dyn Error>> {
        let Some(accumulator) = accumulator else {
            return Ok(None);
        };
        let state = self.lowered.game.decode(None, &read_wram(&self.machine)?);
        let observation = self.lowered.observe(0, state, accumulator);
        Ok(if observation.stops() {
            None
        } else {
            self.lowered.game.moves(&observation.state).forced
        })
    }

    fn run_one_frame(&mut self) -> Result<bool, Box<dyn Error>> {
        let deadline = machine::Moment(self.machine.now().0.saturating_add(1));
        match self.machine.run(
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
}

impl<G: Game, M: NesBackend<Vec<u8>>> Target for NesTarget<G, M> {
    type Action = ButtonChord;
    type Observations = NesObservation<G::State>;
    type Snapshot = NesSnapshot<G::State>;

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
        self.failed = failed;
        self.snapshot_base = None;
        self.current_wram = self.genesis_wram;
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
            for word in [key.place.get(), u64::from(key.identity)] {
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
        if self.failed {
            return None;
        }
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
        })
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), Box<dyn Error>> {
        let imported = self
            .machine
            .import_nes(&snapshot.emulator_state)
            .map_err(|error| error.to_string())?;
        let restored_wram = match self
            .machine
            .replay(imported)
            .and_then(|()| read_wram(&self.machine))
        {
            Ok(wram) => wram,
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
        self.snapshot_base = Some(snapshot.emulator_state.clone());
        self.current_wram = restored_wram;
        self.accumulator = snapshot.accumulator.clone();
        self.observation = snapshot.observation.clone();
        self.action_observations = vec![self.observation.clone().into()];
        self.failed = snapshot.failed;
        Ok(())
    }
}

impl<G: Game, M: NesBackend<Vec<u8>>> Drop for NesTarget<G, M> {
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
            fixture::{LEFT, Ledge, RIGHT, Spot, Stepper, WARP},
        },
    };

    type Ledged = NesTarget<Ledge, Stepper>;

    fn ledged() -> Ledged {
        let lowered = Arc::new(Lowered::new(Ledge, Ledge::STRATEGY).unwrap());
        assert!(!lowered.compiled.is_pure());
        NesTarget::from_machine(lowered, Stepper::default()).unwrap()
    }

    fn walk(target: &mut Ledged, chords: &[(u8, u8)]) -> Vec<Option<AssertionKey>> {
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
    const ON: [(u8, u8); 4] = [(LEFT, 1), (RIGHT, 1), (WARP, 1), (RIGHT, 2)];

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
    }
}
