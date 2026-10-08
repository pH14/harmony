// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, io::Write, path::Path};

use machine::{
    Machine, MachineError, SnapId, StopConditions, nes,
    quicknes::{QUICKNES_AUDIO_CHANNELS, QUICKNES_AUDIO_SAMPLE_RATE, QuickNesMachine},
};
use serde::{Deserialize, Serialize};

use crate::{
    nes_backend::{NesBackend, SnapshotState},
    target::{ExitKind, Target},
};

pub use machine::nes::{ButtonChord, MAX_HOLD_FRAMES, WRAM_SIZE};

const PLAYER_X_LOW: usize = 0x25;
const PLAYER_X_HIGH: usize = 0x26;
const PLAYER_Y_HIGH: usize = 0x27;
const PLAYER_Y_LOW: usize = 0x28;
const PLAYER_HEALTH: usize = 0x4b;
const OBJECT_TYPE: usize = 0x2d;
const LEVEL_NUMBER: usize = 0xa7;
const STARTED_LEVEL_NUMBER: usize = 0xa8;
const NEED_LEVEL_RELOAD: usize = 0xa9;
const LEVEL_VARIABLE: usize = 0x38e;
const OBJECT_VX_HIGH: usize = 0x423;
const OBJECT_STATE: usize = 0x463;
const OBJECT_F3: usize = 0x473;
const OBJECT_F4: usize = 0x483;
const OBJECT_SLOTS: usize = 16;
const OBJECT_STATE_INIT: u8 = 0x84;
const BOSS_FIGHT: u8 = 0x66;
const MOLSNO: u8 = 0x86;
const FOREHEAD_BLOCK_GUY: u8 = 0x90;
const FIGHTER_MAKER: u8 = 0x94;
const JOHN: u8 = 0x9a;
const FINAL_BOSS: u8 = 0xa8;
const SCHEME_TEAM_FIGHT_SIZES: [u8; 2] = [12, 10];
const JACK_STONE_FIGHT: u8 = 2;
const JACK_STONE_HITS: u8 = 16;
const SHOT_BOSS_HITS: u8 = 8;
const FIGHTER_MAKER_PHASES: u8 = 3;
const FIGHTER_MAKER_PHASE_HITS: u8 = 5;
const FINAL_BOSS_HITS: u8 = 20;
const CARRYING_SUN_KEY: usize = 0x500;
const CARRYING_PICKUP_BLOCK: usize = 0x501;
const TOGGLE_BLOCK_ENABLED: usize = 0x505;
const CHIP_COUNT: usize = 0x508;
const CHIPS_NEEDED: usize = 0x509;
const SAVE_RAM_BASE: usize = 0x6000;
const SAVE_RAM_SIZE: usize = 0x2000;
const LEVEL_MAP_BYTES: usize = 0x1000;
const ARROW_BLOCKS: [u8; 10] = [41, 42, 43, 44, 151, 152, 153, 154, 157, 158];
const PLAYER_ABILITY: usize = 0x7200 - SAVE_RAM_BASE;
const CHECKPOINT_LEVEL: usize = 0x7259 - SAVE_RAM_BASE;
const PER_LEVEL_ITEM_TYPE: usize = 0x720d - SAVE_RAM_BASE;
const PER_LEVEL_ITEM_AMOUNT: usize = 0x7221 - SAVE_RAM_BASE;
const PER_LEVEL_ITEM_SLOTS: usize = 10;
const RED_KEY_ITEM: u8 = 2;
pub const KEY_COLORS: usize = 3;
const LEVEL_CLEARED: usize = 0x7f1f - SAVE_RAM_BASE;
const LEVEL_AVAILABLE: usize = 0x7f27 - SAVE_RAM_BASE;
const COLLECTIBLE_BITS: usize = 0x7f2f - SAVE_RAM_BASE;
const PERSISTENT_BITMAP_LEN: usize = 8;

pub const NOVA_CAMPAIGN_LEVEL_COUNT: u8 = 40;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct NovaLevel(u8);

impl NovaLevel {
    pub fn from_number(number: u8) -> Result<Self, MachineError> {
        if (1..=NOVA_CAMPAIGN_LEVEL_COUNT).contains(&number) {
            Ok(Self(number))
        } else {
            Err(MachineError::Backend(format!(
                "Nova campaign level must be 1..={NOVA_CAMPAIGN_LEVEL_COUNT}, got {number}"
            )))
        }
    }

    #[must_use]
    pub fn number(self) -> u8 {
        self.0
    }

    fn index(self) -> u8 {
        self.0 - 1
    }
}

impl Default for NovaLevel {
    fn default() -> Self {
        Self(1)
    }
}

fn level_prefix_bitmap(count: u8) -> [u8; PERSISTENT_BITMAP_LEN] {
    let mut bitmap = [0_u8; PERSISTENT_BITMAP_LEN];
    for index in 0..usize::from(count) {
        bitmap[index / 8] |= 1 << (index % 8);
    }
    bitmap
}

pub type NovaInput = crate::search::archive::Input<ButtonChord>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct NovaMechanicalState {
    pub level: u8,
    pub started_level: u8,
    pub x: u16,
    pub y: u16,
    pub health: u8,
    pub chips: u8,
    pub chips_needed: u8,
    pub fight: u8,
    pub keys: [u8; KEY_COLORS],
    pub sun_key: bool,
    pub carrying_block: bool,
    pub toggle: bool,
    pub arrow_blocks: u16,
    pub ability: u8,
    pub level_reload_pending: bool,
    pub levels_cleared: [u8; PERSISTENT_BITMAP_LEN],
    pub levels_available: [u8; PERSISTENT_BITMAP_LEN],
    pub collectibles: [u8; PERSISTENT_BITMAP_LEN],
}

impl NovaMechanicalState {
    #[must_use]
    pub fn cleared(self, index: u8) -> bool {
        self.levels_cleared
            .get(usize::from(index / 8))
            .is_some_and(|byte| byte & (1 << (index % 8)) != 0)
    }

    #[must_use]
    pub fn cleared_in_order(self) -> u8 {
        (0..NOVA_CAMPAIGN_LEVEL_COUNT)
            .take_while(|index| self.cleared(*index))
            .count()
            .try_into()
            .unwrap_or(u8::MAX)
    }

    #[must_use]
    pub fn in_campaign_level(self) -> bool {
        self.started_level == self.cleared_in_order()
    }

    #[must_use]
    pub fn available_count(self) -> u8 {
        self.levels_available
            .iter()
            .map(|byte| byte.count_ones())
            .sum::<u32>()
            .try_into()
            .unwrap_or(u8::MAX)
    }

    #[must_use]
    pub fn collectible_count(self) -> u8 {
        self.collectibles
            .iter()
            .map(|byte| byte.count_ones())
            .sum::<u32>()
            .try_into()
            .unwrap_or(u8::MAX)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaObservations {
    pub frame_count: u64,
    pub decoded: NovaMechanicalState,
    pub dead: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaVideoMetadata {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub audio_sample_rate: u32,
    pub audio_channels: u8,
    pub audio_frames: u64,
    pub skipped_frames: u64,
    pub input_endpoint: NovaMechanicalState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaSnapshot<P = Vec<u8>> {
    pub(crate) emulator_state: P,
    pub(crate) observation: NovaObservations,
    pub(crate) failed: bool,
}

impl<P> NovaSnapshot<P> {
    #[must_use]
    pub fn state(&self) -> NovaMechanicalState {
        self.observation.decoded
    }
}

const JOYPAD_A: u8 = 1 << 0;
const JOYPAD_START: u8 = 1 << 3;
const JOYPAD_UP: u8 = 1 << 4;

const BOOT_TO_MAIN_MENU: [ButtonChord; 3] = [
    ButtonChord {
        buttons: 0,
        hold_frames: 60,
    },
    ButtonChord {
        buttons: JOYPAD_START,
        hold_frames: 6,
    },
    ButtonChord {
        buttons: 0,
        hold_frames: 114,
    },
];

const MAIN_MENU_TO_PRE_LEVEL: [ButtonChord; 4] = [
    ButtonChord {
        buttons: JOYPAD_START,
        hold_frames: 6,
    },
    ButtonChord {
        buttons: 0,
        hold_frames: 54,
    },
    ButtonChord {
        buttons: JOYPAD_A,
        hold_frames: 6,
    },
    ButtonChord {
        buttons: 0,
        hold_frames: 54,
    },
];

const PRE_LEVEL_TO_GAMEPLAY: [ButtonChord; 8] = [
    ButtonChord {
        buttons: JOYPAD_UP,
        hold_frames: 6,
    },
    ButtonChord {
        buttons: 0,
        hold_frames: 6,
    },
    ButtonChord {
        buttons: JOYPAD_UP,
        hold_frames: 6,
    },
    ButtonChord {
        buttons: 0,
        hold_frames: 6,
    },
    ButtonChord {
        buttons: JOYPAD_UP,
        hold_frames: 6,
    },
    ButtonChord {
        buttons: 0,
        hold_frames: 54,
    },
    ButtonChord {
        buttons: JOYPAD_A,
        hold_frames: 6,
    },
    ButtonChord {
        buttons: 0,
        hold_frames: 60,
    },
];

const LEVEL_END_WAIT: ButtonChord = ButtonChord {
    buttons: 0,
    hold_frames: 120,
};

#[derive(Debug)]
pub struct NovaTarget<M = QuickNesMachine, P = Vec<u8>>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    machine: M,
    genesis: SnapId,
    current: SnapId,
    genesis_observation: NovaObservations,
    observation: NovaObservations,
    action_observations: Vec<NovaObservations>,
    failed: bool,
    snapshot_base: Option<P>,
    genesis_level: u8,
    halt_on_level_clear: bool,
    execution_work: u64,
}

impl<M, P> NovaTarget<M, P>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    pub fn from_power_on(mut machine: M) -> Result<Self, MachineError> {
        for actions in [
            &BOOT_TO_MAIN_MENU[..],
            &MAIN_MENU_TO_PRE_LEVEL[..],
            &PRE_LEVEL_TO_GAMEPLAY[..],
        ] {
            machine::nes::run_actions(&mut machine, actions)?;
        }
        Self::from_machine(machine)
    }

    pub fn from_machine(mut machine: M) -> Result<Self, MachineError> {
        let (wram, save_ram) = read_memory(&machine)?;
        let state = decode_state(&wram, &save_ram)?;
        if state.health == 0 || state.x == 0 || state.y == 0 {
            return Err(MachineError::Backend(
                "Nova machine is not at live gameplay genesis".to_owned(),
            ));
        }
        let power_on = machine.snapshot()?;
        let observation = NovaObservations {
            frame_count: 0,
            decoded: state,
            dead: false,
        };
        Ok(Self {
            machine,
            genesis: power_on,
            current: power_on,
            genesis_observation: observation.clone(),
            action_observations: vec![observation.clone()],
            observation,
            failed: false,
            snapshot_base: None,
            genesis_level: state.started_level,
            halt_on_level_clear: true,
            execution_work: 0,
        })
    }
}

impl NovaTarget<QuickNesMachine> {
    fn from_quicknes_machine(
        mut machine: QuickNesMachine,
        selected_level: NovaLevel,
    ) -> Result<Self, MachineError> {
        let power_on = machine.snapshot()?;
        machine.branch(power_on, &nes::reproducer(&BOOT_TO_MAIN_MENU))?;
        machine.run(StopConditions::default(), None)?;
        machine.drop_snapshot(power_on)?;

        let cleared = level_prefix_bitmap(selected_level.index());
        let available = level_prefix_bitmap(selected_level.number());
        machine.write_save_ram(LEVEL_CLEARED, &cleared)?;
        machine.write_save_ram(LEVEL_AVAILABLE, &available)?;

        let main_menu = machine.snapshot()?;
        machine.branch(
            main_menu,
            &nes::reproducer(&[&MAIN_MENU_TO_PRE_LEVEL[..], &PRE_LEVEL_TO_GAMEPLAY[..]].concat()),
        )?;
        machine.run(StopConditions::default(), None)?;
        machine.drop_snapshot(main_menu)?;
        let (wram, save_ram) = read_memory(&machine)?;
        let state = decode_state(&wram, &save_ram)?;
        if state.health == 0
            || state.x == 0
            || state.y == 0
            || state.started_level != selected_level.index()
        {
            return Err(MachineError::Backend(format!(
                "Nova setup did not reach requested level {}: health={} x={} y={} started_level={}",
                selected_level.number(),
                state.health,
                state.x,
                state.y,
                state.started_level,
            )));
        }
        Self::from_machine(machine)
    }

    pub fn from_rom_bytes_headless(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
    ) -> Result<Self, MachineError> {
        Self::from_rom_bytes_headless_at_level(rom, core_path, core_sha256, NovaLevel::default())
    }

    pub fn from_rom_bytes_headless_at_level(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        selected_level: NovaLevel,
    ) -> Result<Self, MachineError> {
        Self::from_quicknes_machine(
            QuickNesMachine::from_rom_bytes(rom, core_path, core_sha256)?,
            selected_level,
        )
    }
}

impl<M, P> NovaTarget<M, P>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    #[must_use]
    pub fn mechanical_state(&self) -> NovaMechanicalState {
        self.observation.decoded
    }

    #[must_use]
    pub fn is_dead(&self) -> bool {
        self.observation.decoded.health == 0
    }

    #[must_use]
    pub fn cleared_a_level(&self) -> bool {
        self.observation.decoded.cleared(self.genesis_level)
    }

    #[must_use]
    pub fn cleared_every_level(&self) -> bool {
        self.observation.decoded.cleared_in_order() >= NOVA_CAMPAIGN_LEVEL_COUNT
    }

    pub fn set_halt_on_level_clear(&mut self, halt: bool) {
        self.halt_on_level_clear = halt;
    }

    fn halted(&self) -> bool {
        self.failed || self.is_dead() || (self.halt_on_level_clear && self.cleared_a_level())
    }

    fn starts_next_level(&self, prior: NovaMechanicalState, state: NovaMechanicalState) -> bool {
        !self.halt_on_level_clear
            && state.cleared_in_order() > prior.cleared_in_order()
            && state.cleared_in_order() < NOVA_CAMPAIGN_LEVEL_COUNT
    }

    fn start_next_level(&mut self) -> Result<(NovaMechanicalState, u64), MachineError> {
        let mut frames = 0_u64;
        let mut endpoint = None;
        for chord in std::iter::once(LEVEL_END_WAIT).chain(PRE_LEVEL_TO_GAMEPLAY) {
            let from = self.machine.snapshot()?;
            let ran = self
                .machine
                .branch(from, &nes::reproducer(std::slice::from_ref(&chord)))
                .and_then(|()| self.machine.run(StopConditions::default(), None));
            self.machine.drop_snapshot(from)?;
            if !matches!(
                ran?,
                machine::StopReason::Quiescent { .. } | machine::StopReason::SnapshotPoint { .. }
            ) {
                return Err(MachineError::Backend(
                    "Nova level start stopped before its last menu press".to_owned(),
                ));
            }
            let produced = self.machine.frames();
            frames = frames.saturating_add(u64::try_from(produced.len()).unwrap_or(u64::MAX));
            endpoint = produced.last().copied();
        }
        let wram = endpoint.ok_or_else(|| {
            MachineError::Backend("Nova level start produced no frames".to_owned())
        })?;
        let save_ram = self
            .machine
            .read(SAVE_RAM_BASE as u64, SAVE_RAM_SIZE as u32)?;
        let state = decode_state(&wram, &save_ram)?;
        if read_byte(&save_ram, CHECKPOINT_LEVEL)? != state.started_level || state.health == 0 {
            return Err(MachineError::Backend(format!(
                "Nova did not start level {} after the exit door",
                state.started_level
            )));
        }
        Ok((state, frames))
    }

    #[must_use]
    pub fn execution_work(&self) -> u64 {
        self.execution_work
    }

    #[must_use]
    pub fn last_action_observations(&self) -> &[NovaObservations] {
        &self.action_observations
    }

    pub fn survives_probe(&mut self, buttons: u8, frames: u16) -> bool {
        if self.halted() {
            return false;
        }
        if frames == 0 {
            return false;
        }
        let mut actions = Vec::new();
        let mut remaining = frames;
        while remaining > 0 {
            let hold = remaining.min(u16::from(MAX_HOLD_FRAMES));
            let Ok(hold) = u8::try_from(hold) else {
                self.failed = true;
                return false;
            };
            actions.push(ButtonChord::new(buttons, hold));
            remaining -= u16::from(hold);
        }
        let current = self.current;
        if self
            .machine
            .branch(current, &nes::reproducer(&actions))
            .is_err()
        {
            self.failed = true;
            return false;
        }
        let requested_frames = usize::from(frames);
        let mut observed_frames = 0_usize;
        let mut survived = true;
        while observed_frames < requested_frames {
            let stop = self.machine.run(StopConditions::default(), None);
            if matches!(stop, Ok(machine::StopReason::Deadline { .. })) {
                self.failed = true;
                survived = false;
                break;
            }
            let produced = self.machine.frames();
            if produced.is_empty() {
                survived = false;
                break;
            }
            let remaining = requested_frames.saturating_sub(observed_frames);
            let take = produced.len().min(remaining);
            let save_ram = match self
                .machine
                .read(SAVE_RAM_BASE as u64, SAVE_RAM_SIZE as u32)
            {
                Ok(save_ram) => save_ram,
                Err(_) => {
                    self.failed = true;
                    survived = false;
                    Vec::new()
                }
            };
            if !survived {
                break;
            }
            let acceptable_stop = matches!(
                stop,
                Ok(machine::StopReason::SnapshotPoint { .. }
                    | machine::StopReason::Quiescent { .. })
            );
            if !acceptable_stop {
                self.failed = true;
                survived = false;
                break;
            }
            for wram in produced.iter().take(take) {
                match decode_state(wram, &save_ram) {
                    Ok(state) if state.health != 0 => {}
                    Ok(_) => {
                        survived = false;
                        break;
                    }
                    Err(_) => {
                        self.failed = true;
                        survived = false;
                        break;
                    }
                }
            }
            observed_frames = observed_frames.saturating_add(take);
            if !survived || observed_frames >= requested_frames {
                break;
            }
            match stop {
                Ok(machine::StopReason::SnapshotPoint { .. }) => {}
                Ok(machine::StopReason::Quiescent { .. }) | Ok(_) | Err(_) => {
                    survived = false;
                    break;
                }
            }
        }
        if self.machine.replay(current).is_err() {
            self.failed = true;
            return false;
        }
        survived
    }
}

impl NovaTarget<QuickNesMachine> {
    pub fn render_input(
        &mut self,
        input: &NovaInput,
        tail_frames: u32,
        skip_frames: u64,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<NovaVideoMetadata, Box<dyn Error>> {
        self.reset();
        if self.failed {
            return Err("could not restore Nova gameplay genesis for film".into());
        }
        self.machine.set_video_capture(true);
        self.machine.set_audio_capture(true);
        let result = (|| {
            let mut metadata = None;
            let mut skip = skip_frames;
            let mut prior = self.observation.decoded;
            for action in &input.actions {
                self.render_action(
                    *action,
                    video_output,
                    audio_output,
                    &mut metadata,
                    &mut skip,
                )?;
                if self.halt_on_level_clear {
                    continue;
                }
                let (wram, save_ram) = read_memory(&self.machine)?;
                let state = decode_state(&wram, &save_ram)?;
                if self.starts_next_level(prior, state) {
                    for chord in [&[LEVEL_END_WAIT][..], &PRE_LEVEL_TO_GAMEPLAY[..]].concat() {
                        self.render_action(
                            chord,
                            video_output,
                            audio_output,
                            &mut metadata,
                            &mut skip,
                        )?;
                    }
                }
                prior = state;
            }
            let (endpoint_wram, endpoint_save_ram) = read_memory(&self.machine)?;
            let input_endpoint = decode_state(&endpoint_wram, &endpoint_save_ram)?;
            let mut remaining = tail_frames;
            while remaining > 0 {
                let hold = remaining.min(u32::from(MAX_HOLD_FRAMES));
                let hold = u8::try_from(hold)?;
                self.render_action(
                    ButtonChord::new(0, hold),
                    video_output,
                    audio_output,
                    &mut metadata,
                    &mut skip,
                )?;
                remaining -= u32::from(hold);
            }
            let mut metadata = metadata.ok_or("QuickNES produced no video frames")?;
            if metadata.audio_frames == 0 {
                return Err("QuickNES produced no audio samples".into());
            }
            metadata.input_endpoint = input_endpoint;
            metadata.skipped_frames = skip_frames - skip;
            Ok(metadata)
        })();
        self.machine.set_audio_capture(false);
        self.machine.set_video_capture(false);
        result
    }

    fn render_action(
        &mut self,
        action: ButtonChord,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
        metadata: &mut Option<NovaVideoMetadata>,
        skip: &mut u64,
    ) -> Result<(), Box<dyn Error>> {
        let start = self.machine.snapshot()?;
        self.machine
            .branch(start, &nes::reproducer(std::slice::from_ref(&action)))?;
        self.machine.drop_snapshot(start)?;
        for _ in 0..action.bounded_hold_frames() {
            if !self
                .run_one_frame()
                .map_err(|_| "QuickNES film frame failed")?
            {
                break;
            }
            let frame = self
                .machine
                .take_video_frame()
                .ok_or("QuickNES omitted a requested video frame")?;
            if *skip > 0 {
                *skip -= 1;
                self.machine.take_audio_samples();
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
                    *metadata = Some(NovaVideoMetadata {
                        width: frame.width,
                        height: frame.height,
                        frames: 1,
                        audio_sample_rate: QUICKNES_AUDIO_SAMPLE_RATE,
                        audio_channels: QUICKNES_AUDIO_CHANNELS,
                        audio_frames: 0,
                        skipped_frames: 0,
                        input_endpoint: NovaMechanicalState::default(),
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
            let audio_frames = u64::try_from(audio.len() / usize::from(QUICKNES_AUDIO_CHANNELS))?;
            if let Some(existing) = metadata {
                existing.audio_frames = existing.audio_frames.saturating_add(audio_frames);
            }
        }
        Ok(())
    }

    fn run_one_frame(&mut self) -> Result<bool, ()> {
        let deadline = machine::Moment(self.machine.now().0.saturating_add(1));
        match self.machine.run(
            StopConditions {
                deadline: Some(deadline),
                on: machine::StopMask::NONE,
            },
            None,
        ) {
            Ok(machine::StopReason::Deadline { .. }) => Ok(true),
            Ok(machine::StopReason::Quiescent { .. }) => Ok(false),
            Ok(_) | Err(_) => {
                self.failed = true;
                Err(())
            }
        }
    }
}

impl<M, P> NovaTarget<M, P>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    fn make_observation(&self, frame_count: u64, state: NovaMechanicalState) -> NovaObservations {
        NovaObservations {
            frame_count,
            decoded: state,
            dead: state.health == 0,
        }
    }
}

impl<M, P> Target for NovaTarget<M, P>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    type Action = ButtonChord;
    type Observations = NovaObservations;
    type Snapshot = NovaSnapshot<P>;

    fn reset(&mut self) {
        let mut handle_error = false;
        if self.current != self.genesis {
            if self.machine.drop_snapshot(self.current).is_err() {
                handle_error = true;
            }
            if !handle_error {
                self.current = self.genesis;
            }
        }
        let replay_error = self.machine.replay(self.genesis).is_err();
        self.failed = handle_error || replay_error;
        self.snapshot_base = None;
        self.observation = self.genesis_observation.clone();
        self.action_observations = vec![self.observation.clone()];
    }

    fn apply(&mut self, action: &Self::Action) {
        self.action_observations.clear();
        if self.halted() {
            return;
        }
        let prior_state = self.observation.decoded;
        let action_start = prior_state;
        let start = self.current;
        if self
            .machine
            .branch(start, &nes::reproducer(std::slice::from_ref(action)))
            .is_err()
        {
            self.failed = true;
            return;
        }
        let run = self.machine.run(StopConditions::default(), None);
        if matches!(run, Ok(machine::StopReason::Deadline { .. })) {
            self.failed = true;
            return;
        }
        if !matches!(
            run,
            Ok(machine::StopReason::Quiescent { .. } | machine::StopReason::SnapshotPoint { .. })
        ) {
            self.failed = true;
            return;
        }

        let frame_count = self.machine.frames().len();
        if frame_count == 0 {
            self.failed = true;
            return;
        }
        self.execution_work = self
            .execution_work
            .saturating_add(u64::try_from(frame_count).unwrap_or(u64::MAX));
        let save_ram = match self
            .machine
            .read(SAVE_RAM_BASE as u64, SAVE_RAM_SIZE as u32)
        {
            Ok(save_ram) => save_ram,
            Err(_) => {
                self.failed = true;
                return;
            }
        };
        let frames = self.machine.frames();
        let mut prior_state = prior_state;
        let mut emitted = false;
        for (offset, wram) in frames.iter().enumerate() {
            let Ok(state) = decode_state(wram, &save_ram) else {
                self.failed = true;
                return;
            };
            let boundary = spatial_bucket(state) != spatial_bucket(prior_state)
                || preference_tuple(state) != preference_tuple(prior_state)
                || state.keys != prior_state.keys
                || state.ability != prior_state.ability
                || state.level_reload_pending != prior_state.level_reload_pending;
            if boundary {
                let frame_count = self
                    .observation
                    .frame_count
                    .saturating_add(u64::try_from(offset).unwrap_or(u64::MAX).saturating_add(1));
                self.action_observations
                    .push(self.make_observation(frame_count, state));
                prior_state = state;
                emitted = true;
            }
        }
        let Some(endpoint_wram) = frames.last().copied() else {
            self.failed = true;
            return;
        };
        let endpoint_frame = self
            .observation
            .frame_count
            .saturating_add(u64::try_from(frames.len()).unwrap_or(u64::MAX));
        if !emitted
            || !self
                .action_observations
                .last()
                .is_some_and(|observation| observation.frame_count == endpoint_frame)
        {
            let endpoint_state = decode_state(&endpoint_wram, &save_ram);
            let Ok(endpoint_state) = endpoint_state else {
                self.failed = true;
                return;
            };
            self.action_observations
                .push(self.make_observation(endpoint_frame, endpoint_state));
        }
        if let Some(observation) = self.action_observations.last() {
            self.observation = observation.clone();
        }
        if self.starts_next_level(action_start, self.observation.decoded) {
            let Ok((state, frames)) = self.start_next_level() else {
                self.failed = true;
                return;
            };
            self.execution_work = self.execution_work.saturating_add(frames);
            let observation =
                self.make_observation(self.observation.frame_count.saturating_add(frames), state);
            self.action_observations.push(observation.clone());
            self.observation = observation;
        }
        let next = match self.machine.snapshot() {
            Ok(next) => next,
            Err(_) => {
                self.failed = true;
                return;
            }
        };
        if start != self.genesis && self.machine.drop_snapshot(start).is_err() {
            let _ = self.machine.drop_snapshot(next);
            self.failed = true;
            return;
        }
        self.current = next;
    }

    fn observe(&self) -> Self::Observations {
        self.observation.clone()
    }

    fn fingerprint(&self) -> u64 {
        let state = self.observation.decoded;
        (u64::from(state.started_level) << 40)
            | (u64::from(state.level) << 32)
            | (u64::from(state.x / 32) << 16)
            | u64::from(state.y / 32)
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
        let emulator_state = match self
            .machine
            .export_nes(self.current, self.snapshot_base.as_ref())
        {
            Ok(state) => state,
            Err(_) => {
                self.failed = true;
                return None;
            }
        };
        self.snapshot_base = Some(emulator_state.clone());
        Some(NovaSnapshot {
            emulator_state,
            observation: self.observation.clone(),
            failed: self.failed,
        })
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), Box<dyn Error>> {
        let imported = self
            .machine
            .import_nes(&snapshot.emulator_state)
            .map_err(|error| error.to_string())?;
        if let Err(error) = self.machine.replay(imported) {
            let _ = self.machine.drop_snapshot(imported);
            let _ = self.machine.replay(self.current);
            return Err(error.to_string().into());
        }
        if self.current != self.genesis
            && let Err(error) = self.machine.drop_snapshot(self.current)
        {
            let _ = self.machine.drop_snapshot(imported);
            let _ = self.machine.replay(self.current);
            return Err(error.to_string().into());
        }
        self.current = imported;
        self.snapshot_base = Some(snapshot.emulator_state.clone());
        self.observation = snapshot.observation.clone();
        self.action_observations = vec![self.observation.clone()];
        self.failed = snapshot.failed;
        Ok(())
    }
}

impl<M, P> Drop for NovaTarget<M, P>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    fn drop(&mut self) {
        if self.current != self.genesis {
            let _ = self.machine.drop_snapshot(self.current);
        }
        let _ = self.machine.drop_snapshot(self.genesis);
    }
}

fn read_memory<M: Machine>(machine: &M) -> Result<([u8; WRAM_SIZE], Vec<u8>), MachineError> {
    let wram = machine.read(0, WRAM_SIZE as u32)?;
    let wram = wram.try_into().map_err(|_| {
        MachineError::Backend("Nova work RAM window has an invalid length".to_owned())
    })?;
    let save_ram = machine.read(SAVE_RAM_BASE as u64, SAVE_RAM_SIZE as u32)?;
    if save_ram.len() != SAVE_RAM_SIZE {
        return Err(MachineError::Backend(
            "Nova save RAM window has an invalid length".to_owned(),
        ));
    }
    Ok((wram, save_ram))
}

fn read_byte(bytes: &[u8], address: usize) -> Result<u8, MachineError> {
    bytes
        .get(address)
        .copied()
        .ok_or_else(|| MachineError::Backend(format!("Nova RAM address {address:#x} is absent")))
}

fn read_bitmap(bytes: &[u8], address: usize) -> Result<[u8; PERSISTENT_BITMAP_LEN], MachineError> {
    bytes
        .get(address..address.saturating_add(PERSISTENT_BITMAP_LEN))
        .ok_or_else(|| MachineError::Backend(format!("Nova bitmap at {address:#x} is absent")))?
        .try_into()
        .map_err(|_| MachineError::Backend(format!("Nova bitmap at {address:#x} is truncated")))
}

fn fixed_point_pixels(high: u8, low: u8) -> u16 {
    u16::from(high) * 16 + u16::from(low >> 4)
}

fn held_keys(save_ram: &[u8]) -> Result<[u8; KEY_COLORS], MachineError> {
    let mut keys = [0_u8; KEY_COLORS];
    for slot in 0..PER_LEVEL_ITEM_SLOTS {
        let item = read_byte(save_ram, PER_LEVEL_ITEM_TYPE + slot)?;
        let Some(count) = item
            .checked_sub(RED_KEY_ITEM)
            .and_then(|color| keys.get_mut(usize::from(color)))
        else {
            continue;
        };
        let amount = read_byte(save_ram, PER_LEVEL_ITEM_AMOUNT + slot)?;
        *count = count.saturating_add(amount.saturating_add(1));
    }
    Ok(keys)
}

fn arrow_blocks(save_ram: &[u8]) -> Result<u16, MachineError> {
    let map = save_ram
        .get(..LEVEL_MAP_BYTES)
        .ok_or_else(|| MachineError::Backend("Nova level map is absent".to_owned()))?;
    let count = map
        .iter()
        .filter(|block| ARROW_BLOCKS.contains(block))
        .count();
    Ok(u16::try_from(count).unwrap_or(u16::MAX))
}

fn fight_progress(wram: &[u8]) -> Result<u8, MachineError> {
    for slot in 0..OBJECT_SLOTS {
        let kind = read_byte(wram, OBJECT_TYPE + slot)? & !1;
        let f3 = read_byte(wram, OBJECT_F3 + slot)?;
        let f4 = read_byte(wram, OBJECT_F4 + slot)?;
        let hits = read_byte(wram, OBJECT_VX_HIGH + slot)?;
        let progress = match kind {
            BOSS_FIGHT if f3 == JACK_STONE_FIGHT => hits.min(JACK_STONE_HITS),
            BOSS_FIGHT => {
                let Some(&size) = SCHEME_TEAM_FIGHT_SIZES.get(usize::from(f3)) else {
                    continue;
                };
                if read_byte(wram, OBJECT_STATE + slot)? == OBJECT_STATE_INIT {
                    0
                } else {
                    size.saturating_sub(read_byte(wram, LEVEL_VARIABLE)?)
                }
            }
            MOLSNO | FOREHEAD_BLOCK_GUY | JOHN => f4.min(SHOT_BOSS_HITS),
            FIGHTER_MAKER => {
                f3.min(FIGHTER_MAKER_PHASES) * FIGHTER_MAKER_PHASE_HITS
                    + f4.min(FIGHTER_MAKER_PHASE_HITS - 1)
            }
            FINAL_BOSS => hits.min(FINAL_BOSS_HITS),
            _ => continue,
        };
        return Ok(progress);
    }
    Ok(0)
}

pub fn decode_state(wram: &[u8], save_ram: &[u8]) -> Result<NovaMechanicalState, MachineError> {
    Ok(NovaMechanicalState {
        level: read_byte(wram, LEVEL_NUMBER)?,
        started_level: read_byte(wram, STARTED_LEVEL_NUMBER)?,
        x: fixed_point_pixels(
            read_byte(wram, PLAYER_X_HIGH)?,
            read_byte(wram, PLAYER_X_LOW)?,
        ),
        y: fixed_point_pixels(
            read_byte(wram, PLAYER_Y_HIGH)?,
            read_byte(wram, PLAYER_Y_LOW)?,
        ),
        health: read_byte(wram, PLAYER_HEALTH)?,
        chips: read_byte(wram, CHIP_COUNT)?,
        chips_needed: read_byte(wram, CHIPS_NEEDED)?,
        fight: fight_progress(wram)?,
        keys: held_keys(save_ram)?,
        sun_key: read_byte(wram, CARRYING_SUN_KEY)? != 0,
        carrying_block: read_byte(wram, CARRYING_PICKUP_BLOCK)? != 0,
        toggle: read_byte(wram, TOGGLE_BLOCK_ENABLED)? != 0,
        arrow_blocks: arrow_blocks(save_ram)?,
        ability: read_byte(save_ram, PLAYER_ABILITY)?,
        level_reload_pending: read_byte(wram, NEED_LEVEL_RELOAD)? != 0,
        levels_cleared: read_bitmap(save_ram, LEVEL_CLEARED)?,
        levels_available: read_bitmap(save_ram, LEVEL_AVAILABLE)?,
        collectibles: read_bitmap(save_ram, COLLECTIBLE_BITS)?,
    })
}

#[must_use]
pub fn spatial_bucket(state: NovaMechanicalState) -> (u8, u8, u16, u16) {
    (state.started_level, state.level, state.x / 32, state.y / 32)
}

#[must_use]
pub fn preference_tuple(state: NovaMechanicalState) -> (u8, u8, u8, bool, u8, u8) {
    (
        state.cleared_in_order(),
        state.collectible_count(),
        state.available_count(),
        state.ability != 0,
        state.health,
        state.chips,
    )
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        collections::{BTreeMap, VecDeque},
    };

    use super::*;

    #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
    struct FakePortable(Vec<u8>);

    impl SnapshotState for FakePortable {
        type WorkRam = ();

        fn memory_charge(&self) -> usize {
            self.0.len()
        }
    }

    impl NesBackend<FakePortable> for FakeMachine {
        fn export_nes(
            &mut self,
            snapshot: SnapId,
            base: Option<&FakePortable>,
        ) -> Result<FakePortable, MachineError> {
            self.export(snapshot, base)
        }

        fn import_nes(&mut self, portable: &FakePortable) -> Result<SnapId, MachineError> {
            self.import(portable)
        }
    }

    #[derive(Debug)]
    struct FakeMachine {
        state: Vec<u8>,
        snapshots: BTreeMap<u64, Vec<u8>>,
        next_snapshot: u64,
        staged: Vec<ButtonChord>,
        frames: Vec<[u8; WRAM_SIZE]>,
        vtime: u64,
        snapshot_calls: usize,
        drop_calls: usize,
        branch_calls: usize,
        replay_calls: usize,
        run_calls: usize,
        read_calls: Cell<usize>,
        export_calls: usize,
        export_base_calls: usize,
        import_calls: usize,
        run_stops: VecDeque<machine::StopReason>,
        max_chords_per_run: Option<usize>,
        append_sentinel: bool,
        zero_frames: bool,
        fail_next_drop: bool,
        reads_need_a_run: bool,
        stale_reads: bool,
        exit_door_on_up: bool,
        start_level_on_a: bool,
        lifecycle: Vec<&'static str>,
    }

    impl FakeMachine {
        fn new() -> Self {
            let mut state = vec![0_u8; SAVE_RAM_BASE + SAVE_RAM_SIZE];
            state[PLAYER_X_HIGH] = 1;
            state[PLAYER_Y_HIGH] = 1;
            state[PLAYER_HEALTH] = 4;
            state[LEVEL_NUMBER] = 1;
            state[STARTED_LEVEL_NUMBER] = 0;
            Self {
                state,
                snapshots: BTreeMap::new(),
                next_snapshot: 0,
                staged: Vec::new(),
                frames: Vec::new(),
                vtime: 0,
                snapshot_calls: 0,
                drop_calls: 0,
                branch_calls: 0,
                replay_calls: 0,
                run_calls: 0,
                read_calls: Cell::new(0),
                export_calls: 0,
                export_base_calls: 0,
                import_calls: 0,
                run_stops: VecDeque::new(),
                max_chords_per_run: None,
                append_sentinel: false,
                zero_frames: false,
                fail_next_drop: false,
                reads_need_a_run: false,
                stale_reads: false,
                exit_door_on_up: false,
                start_level_on_a: false,
                lifecycle: Vec::new(),
            }
        }

        fn insert_snapshot(&mut self, bytes: Vec<u8>) -> SnapId {
            let id = SnapId(self.next_snapshot);
            self.next_snapshot = self.next_snapshot.saturating_add(1);
            self.snapshots.insert(id.0, bytes);
            id
        }
    }

    impl Machine for FakeMachine {
        type Portable = FakePortable;

        fn snapshot(&mut self) -> Result<SnapId, MachineError> {
            self.lifecycle.push("snapshot");
            self.snapshot_calls = self.snapshot_calls.saturating_add(1);
            Ok(self.insert_snapshot(self.state.clone()))
        }

        fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError> {
            self.lifecycle.push("drop");
            self.drop_calls = self.drop_calls.saturating_add(1);
            if self.fail_next_drop {
                self.fail_next_drop = false;
                return Err(MachineError::Backend("injected drop failure".to_owned()));
            }
            self.snapshots
                .remove(&snap.0)
                .map(|_| ())
                .ok_or(MachineError::UnknownSnapshot)
        }

        fn branch(&mut self, snap: SnapId, env: &machine::Reproducer) -> Result<(), MachineError> {
            self.lifecycle.push("branch");
            self.branch_calls = self.branch_calls.saturating_add(1);
            self.state = self
                .snapshots
                .get(&snap.0)
                .cloned()
                .ok_or(MachineError::UnknownSnapshot)?;
            self.staged = nes::actions_of(env)?;
            if self.append_sentinel {
                self.staged.push(ButtonChord::new(0, 1));
            }
            Ok(())
        }

        fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
            self.replay_calls = self.replay_calls.saturating_add(1);
            self.stale_reads = self.reads_need_a_run;
            self.state = self
                .snapshots
                .get(&snap.0)
                .cloned()
                .ok_or(MachineError::UnknownSnapshot)?;
            self.staged.clear();
            Ok(())
        }

        fn run(
            &mut self,
            _until: StopConditions,
            _resolve: Option<&machine::Answer>,
        ) -> Result<machine::StopReason, MachineError> {
            self.lifecycle.push("run");
            self.run_calls = self.run_calls.saturating_add(1);
            self.stale_reads = false;
            self.frames.clear();
            let chord_count = self
                .max_chords_per_run
                .unwrap_or(self.staged.len())
                .min(self.staged.len());
            for action in self.staged.drain(..chord_count).collect::<Vec<_>>() {
                let checkpoint = SAVE_RAM_BASE + CHECKPOINT_LEVEL;
                let level = self.state[STARTED_LEVEL_NUMBER];
                if self.exit_door_on_up
                    && action.buttons == JOYPAD_UP
                    && self.state[checkpoint] == level
                {
                    self.state[SAVE_RAM_BASE + LEVEL_CLEARED + usize::from(level / 8)] |=
                        1 << (level % 8);
                    self.state[STARTED_LEVEL_NUMBER] = level + 1;
                }
                if self.start_level_on_a && action.buttons == JOYPAD_A {
                    self.state[checkpoint] = level;
                }
                for _ in 0..action.bounded_hold_frames() {
                    self.state[0] = self.state[0].wrapping_add(1);
                    if !self.zero_frames {
                        let mut wram = [0_u8; WRAM_SIZE];
                        wram.copy_from_slice(
                            self.state
                                .get(..WRAM_SIZE)
                                .ok_or(MachineError::Backend("short fake state".to_owned()))?,
                        );
                        self.frames.push(wram);
                    }
                    self.vtime = self.vtime.saturating_add(1);
                }
            }
            let stop = self
                .run_stops
                .pop_front()
                .unwrap_or(machine::StopReason::Quiescent {
                    vtime: machine::Moment(self.vtime),
                });
            if self.append_sentinel && !matches!(stop, machine::StopReason::SnapshotPoint { .. }) {
                self.frames.clear();
                self.stale_reads = true;
            }
            Ok(stop)
        }

        fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError> {
            self.read_calls.set(self.read_calls.get().saturating_add(1));
            if self.stale_reads {
                return Err(MachineError::Backend(
                    "no cached observation at the current stop".to_owned(),
                ));
            }
            let end = addr
                .checked_add(u64::from(len))
                .ok_or(MachineError::ReadOutOfBounds)?;
            let (start, finish) = if addr == 0 && end == WRAM_SIZE as u64 {
                (0, WRAM_SIZE)
            } else if addr == SAVE_RAM_BASE as u64 && end == (SAVE_RAM_BASE + SAVE_RAM_SIZE) as u64
            {
                (SAVE_RAM_BASE, SAVE_RAM_BASE + SAVE_RAM_SIZE)
            } else {
                return Err(MachineError::ReadOutOfBounds);
            };
            self.state
                .get(start..finish)
                .map(ToOwned::to_owned)
                .ok_or(MachineError::ReadOutOfBounds)
        }

        fn export(
            &mut self,
            snap: SnapId,
            base: Option<&Self::Portable>,
        ) -> Result<Self::Portable, MachineError> {
            self.export_calls = self.export_calls.saturating_add(1);
            if base.is_some() {
                self.export_base_calls = self.export_base_calls.saturating_add(1);
            }
            self.snapshots
                .get(&snap.0)
                .cloned()
                .map(FakePortable)
                .ok_or(MachineError::UnknownSnapshot)
        }

        fn import(&mut self, portable: &Self::Portable) -> Result<SnapId, MachineError> {
            self.import_calls = self.import_calls.saturating_add(1);
            Ok(self.insert_snapshot(portable.0.clone()))
        }

        fn portable_memory_charge(portable: &Self::Portable) -> usize {
            portable.0.len()
        }

        fn now(&self) -> machine::Moment {
            machine::Moment(self.vtime)
        }

        fn frames(&self) -> &[[u8; WRAM_SIZE]] {
            &self.frames
        }
    }

    #[test]
    fn a_restore_succeeds_on_a_machine_that_reads_only_after_a_run() {
        let mut machine = FakeMachine::new();
        machine.reads_need_a_run = true;
        let mut target = NovaTarget::from_machine(machine).unwrap();
        target.apply(&ButtonChord::new(1, 1));
        let snapshot = target.snapshot().unwrap();
        target.apply(&ButtonChord::new(2, 1));
        target.restore(&snapshot).unwrap();
        assert_eq!(target.snapshot(), Some(snapshot.clone()));
        target.apply(&ButtonChord::new(2, 1));
        assert!(!target.failed);
        assert_eq!(
            target.observation.frame_count,
            snapshot.observation.frame_count + 1
        );
    }

    #[test]
    fn whole_game_policy_executes_after_a_level_clear() {
        let mut target = NovaTarget::from_machine(FakeMachine::new()).expect("genesis");
        target.genesis_level = 0;
        target.observation.decoded.levels_cleared[0] = 1;
        assert!(target.cleared_a_level());
        assert!(!target.cleared_every_level());
        target.apply(&ButtonChord::new(0, 3));
        assert_eq!(target.machine.run_calls, 0);
        target.set_halt_on_level_clear(false);
        target.apply(&ButtonChord::new(0, 3));
        assert_eq!(target.machine.run_calls, 1);
    }

    fn exit_door_machine(cleared_before: u8) -> FakeMachine {
        let mut machine = FakeMachine::new();
        machine.exit_door_on_up = true;
        machine.start_level_on_a = true;
        let cleared = level_prefix_bitmap(cleared_before);
        machine.state[SAVE_RAM_BASE + LEVEL_CLEARED..][..PERSISTENT_BITMAP_LEN]
            .copy_from_slice(&cleared);
        machine.state[STARTED_LEVEL_NUMBER] = cleared_before;
        machine.state[SAVE_RAM_BASE + CHECKPOINT_LEVEL] = cleared_before;
        machine
    }

    fn level_start_frames() -> u64 {
        std::iter::once(LEVEL_END_WAIT)
            .chain(PRE_LEVEL_TO_GAMEPLAY)
            .map(|chord| u64::from(chord.bounded_hold_frames()))
            .sum()
    }

    #[test]
    fn whole_game_starts_the_next_level_after_an_exit_door() {
        let mut target = NovaTarget::from_machine(exit_door_machine(0)).expect("genesis");
        target.set_halt_on_level_clear(false);
        target.apply(&ButtonChord::new(JOYPAD_UP, 2));
        assert!(!target.failed);
        let state = target.mechanical_state();
        assert_eq!((state.cleared_in_order(), state.started_level), (1, 1));
        assert_eq!(target.observe().frame_count, 2 + level_start_frames());
        assert_eq!(target.execution_work(), 2 + level_start_frames());
        target.apply(&ButtonChord::new(0, 1));
        assert!(!target.failed);
        assert_eq!(target.observe().frame_count, 3 + level_start_frames());
    }

    #[test]
    fn level_start_reads_each_menu_press_at_its_snapshot_point() {
        let mut machine = exit_door_machine(0);
        machine.max_chords_per_run = Some(1);
        machine.append_sentinel = true;
        let chords = 2 + PRE_LEVEL_TO_GAMEPLAY.len();
        machine.run_stops.extend(std::iter::repeat_n(
            machine::StopReason::SnapshotPoint {
                vtime: machine::Moment(0),
            },
            chords,
        ));
        let mut target = NovaTarget::from_machine(machine).expect("genesis");
        target.set_halt_on_level_clear(false);
        target.apply(&ButtonChord::new(JOYPAD_UP, 2));
        assert!(!target.failed);
        let state = target.mechanical_state();
        assert_eq!((state.cleared_in_order(), state.started_level), (1, 1));
        assert_eq!(target.observe().frame_count, 2 + level_start_frames());
        assert_eq!(target.machine.vtime, 2 + level_start_frames());
        assert_eq!(target.machine.run_calls, chords);
    }

    #[test]
    fn a_level_that_fails_to_start_is_an_execution_failure() {
        let mut machine = exit_door_machine(0);
        machine.start_level_on_a = false;
        let mut target = NovaTarget::from_machine(machine).expect("genesis");
        target.set_halt_on_level_clear(false);
        target.apply(&ButtonChord::new(JOYPAD_UP, 2));
        assert!(target.failed);
        assert!(matches!(target.exit_kind(), ExitKind::Crash));
    }

    #[test]
    fn a_level_clear_reads_the_selected_level_bit_alone() {
        let mut target = NovaTarget::from_machine(FakeMachine::new()).expect("genesis");
        target.genesis_level = 20;
        target.observation.decoded.levels_cleared = [255, 255, 140, 0, 0, 128, 0, 0];
        assert_eq!(target.observation.decoded.cleared_in_order(), 16);
        assert!(!target.cleared_a_level());
        target.observation.decoded.levels_cleared[2] |= 1 << 4;
        assert!(target.cleared_a_level());
        assert_eq!(target.observation.decoded.cleared_in_order(), 16);
    }

    #[test]
    fn only_the_next_level_in_order_is_the_campaign_level() {
        let state = NovaMechanicalState {
            started_level: 20,
            levels_cleared: level_prefix_bitmap(20),
            ..NovaMechanicalState::default()
        };
        assert!(state.in_campaign_level());
        assert!(
            !NovaMechanicalState {
                started_level: 12,
                ..state
            }
            .in_campaign_level()
        );
        assert!(
            !NovaMechanicalState {
                started_level: 21,
                ..state
            }
            .in_campaign_level()
        );
        let mut stray = state;
        stray.levels_cleared[2] |= 1 << 6;
        assert!(stray.in_campaign_level());
    }

    #[test]
    fn no_level_starts_after_a_level_campaign_clear_or_the_last_level() {
        let mut level = NovaTarget::from_machine(exit_door_machine(0)).expect("genesis");
        level.apply(&ButtonChord::new(JOYPAD_UP, 2));
        assert_eq!(level.observe().frame_count, 2);
        assert!(level.cleared_a_level());
        let mut game = NovaTarget::from_machine(exit_door_machine(NOVA_CAMPAIGN_LEVEL_COUNT - 1))
            .expect("genesis");
        game.set_halt_on_level_clear(false);
        game.apply(&ButtonChord::new(JOYPAD_UP, 2));
        assert!(!game.failed);
        assert_eq!(game.observe().frame_count, 2);
        assert!(game.cleared_every_level());
    }

    #[test]
    fn generic_action_runs_once_and_observes_returned_frames() {
        let mut target = NovaTarget::from_machine(FakeMachine::new()).expect("genesis");
        let action = ButtonChord::new(0x81, 3);

        target.apply(&action);
        assert_eq!(target.machine.branch_calls, 1);
        assert_eq!(target.machine.run_calls, 1);
        assert_eq!(target.machine.snapshot_calls, 2);
        assert_eq!(target.machine.drop_calls, 0);
        assert_eq!(target.machine.read_calls.get(), 3);
        assert_eq!(target.observe().frame_count, 3);

        let before = (
            target.machine.branch_calls,
            target.machine.run_calls,
            target.machine.snapshot_calls,
            target.machine.drop_calls,
            target.machine.read_calls.get(),
        );
        target.apply(&action);
        assert_eq!(target.machine.branch_calls, before.0 + 1);
        assert_eq!(target.machine.run_calls, before.1 + 1);
        assert_eq!(target.machine.snapshot_calls, before.2 + 1);
        assert_eq!(target.machine.drop_calls, before.3 + 1);
        assert_eq!(target.observe().frame_count, 6);
        assert_eq!(target.machine.snapshots.len(), 2);

        let before_probe = (
            target.machine.snapshot_calls,
            target.machine.drop_calls,
            target.machine.branch_calls,
            target.machine.run_calls,
            target.machine.replay_calls,
            target.machine.snapshots.len(),
        );
        assert!(target.survives_probe(0, 2));
        assert_eq!(target.machine.snapshot_calls, before_probe.0);
        assert_eq!(target.machine.drop_calls, before_probe.1);
        assert_eq!(target.machine.branch_calls, before_probe.2 + 1);
        assert_eq!(target.machine.run_calls, before_probe.3 + 1);
        assert_eq!(target.machine.replay_calls, before_probe.4 + 1);
        assert_eq!(target.machine.snapshots.len(), before_probe.5);
    }

    #[test]
    fn generic_snapshot_restore_and_reset_keep_handles_bounded() {
        let mut target = NovaTarget::from_machine(FakeMachine::new()).expect("genesis");
        target.apply(&ButtonChord::new(0x01, 2));
        let first_work = target.execution_work();
        assert_eq!(first_work, 2);
        let snapshot = target.snapshot().expect("portable snapshot");
        assert_eq!(target.machine.export_base_calls, 0);
        let same = target.snapshot().expect("shared portable snapshot");
        assert_eq!(target.machine.export_calls, 2);
        assert_eq!(target.machine.export_base_calls, 1);
        assert_eq!(
            <FakeMachine as Machine>::portable_memory_charge(&snapshot.emulator_state),
            <FakeMachine as Machine>::portable_memory_charge(&same.emulator_state)
        );
        assert_eq!(target.machine.snapshots.len(), 2);

        target.apply(&ButtonChord::new(0x02, 2));
        let second_work = target.execution_work();
        assert_eq!(second_work, 4);
        assert_eq!(target.machine.snapshots.len(), 2);
        target
            .restore(&snapshot)
            .expect("restore portable snapshot");
        assert_eq!(target.execution_work(), second_work);
        assert_eq!(target.machine.import_calls, 1);
        assert_eq!(target.machine.replay_calls, 1);
        assert_eq!(target.machine.snapshots.len(), 2);
        assert_eq!(target.observe().frame_count, 2);

        target.reset();
        assert_eq!(target.execution_work(), second_work);
        assert_eq!(target.machine.snapshots.len(), 1);
        assert_eq!(target.machine.drop_calls, 3);
        assert_eq!(target.observe().frame_count, 0);
    }

    #[test]
    fn child_is_sealed_before_derive_parent_is_dropped() {
        let mut target = NovaTarget::from_machine(FakeMachine::new()).expect("genesis");
        target.apply(&ButtonChord::new(0x01, 2));
        target.machine.lifecycle.clear();

        target.apply(&ButtonChord::new(0x02, 2));

        assert_eq!(
            target.machine.lifecycle,
            ["branch", "run", "snapshot", "drop"]
        );
        assert_eq!(target.exit_kind(), ExitKind::Ok);
        assert_eq!(target.machine.snapshots.len(), 2);
    }

    #[test]
    fn snapshot_point_is_a_successful_action_and_probe_boundary() {
        let mut target = NovaTarget::from_machine(FakeMachine::new()).expect("genesis");
        target
            .machine
            .run_stops
            .push_back(machine::StopReason::SnapshotPoint {
                vtime: machine::Moment(3),
            });
        target.apply(&ButtonChord::new(0x81, 3));
        assert_eq!(target.exit_kind(), ExitKind::Ok);
        assert_eq!(target.observe().frame_count, 3);

        let mut probe = NovaTarget::from_machine(FakeMachine::new()).expect("genesis");
        probe
            .machine
            .run_stops
            .push_back(machine::StopReason::SnapshotPoint {
                vtime: machine::Moment(2),
            });
        assert!(probe.survives_probe(0, 2));
        assert_eq!(probe.exit_kind(), ExitKind::Ok);
    }

    #[test]
    fn infrastructure_deadline_is_a_failure_not_a_game_death() {
        let mut target = NovaTarget::from_machine(FakeMachine::new()).expect("genesis");
        target
            .machine
            .run_stops
            .push_back(machine::StopReason::Deadline {
                vtime: machine::Moment(3),
            });
        let reads_before = target.machine.read_calls.get();
        target.apply(&ButtonChord::new(0x81, 3));

        assert_eq!(target.exit_kind(), ExitKind::Crash);
        assert!(!target.is_dead());
        assert_eq!(target.machine.read_calls.get(), reads_before);
        assert_eq!(target.machine.snapshot_calls, 1);
        assert!(target.last_action_observations().is_empty());
    }

    #[test]
    fn zero_frame_action_and_probe_are_rejected() {
        let mut target = NovaTarget::from_machine(FakeMachine::new()).expect("genesis");
        target.machine.zero_frames = true;
        target.apply(&ButtonChord::new(0x81, 3));
        assert_eq!(target.exit_kind(), ExitKind::Crash);
        assert_eq!(target.machine.run_calls, 1);
        assert_eq!(target.machine.snapshots.len(), 1);

        target.reset();
        assert!(!target.survives_probe(0, 3));
        assert_eq!(target.exit_kind(), ExitKind::Ok);
        assert_eq!(target.machine.run_calls, 2);
        assert_eq!(target.machine.snapshots.len(), 1);
    }

    #[test]
    fn probe_spans_chords_without_consuming_following_sentinel() {
        let mut target = NovaTarget::from_machine(FakeMachine::new()).expect("genesis");
        target.machine.max_chords_per_run = Some(1);
        target.machine.append_sentinel = true;
        target.machine.run_stops.extend([
            machine::StopReason::SnapshotPoint {
                vtime: machine::Moment(120),
            },
            machine::StopReason::SnapshotPoint {
                vtime: machine::Moment(121),
            },
        ]);
        assert!(target.survives_probe(0, u16::from(MAX_HOLD_FRAMES) + 1));
        assert_eq!(target.machine.run_calls, 2);
        assert_eq!(target.machine.replay_calls, 1);
        assert_eq!(target.machine.state[0], 0);
        assert_eq!(target.machine.staged.len(), 0);
    }

    #[test]
    fn reset_replays_genesis_even_when_current_drop_fails() {
        let mut target = NovaTarget::from_machine(FakeMachine::new()).expect("genesis");
        target.apply(&ButtonChord::new(0x81, 2));
        let replay_calls = target.machine.replay_calls;
        target.machine.fail_next_drop = true;
        target.reset();
        assert_eq!(target.machine.replay_calls, replay_calls + 1);
        assert_eq!(target.exit_kind(), ExitKind::Crash);
        assert_eq!(target.observe().frame_count, 0);
        assert_eq!(target.machine.snapshots.len(), 2);
    }

    #[test]
    fn level_fixture_uses_one_based_campaign_levels() {
        assert!(NovaLevel::from_number(0).is_err());
        assert!(NovaLevel::from_number(NOVA_CAMPAIGN_LEVEL_COUNT + 1).is_err());
        let first = NovaLevel::from_number(1).expect("first level");
        let last = NovaLevel::from_number(40).expect("last level");
        assert_eq!((first.number(), first.index()), (1, 0));
        assert_eq!((last.number(), last.index()), (40, 39));
        assert_eq!(level_prefix_bitmap(0), [0; PERSISTENT_BITMAP_LEN]);
        assert_eq!(level_prefix_bitmap(1)[0], 0x01);
        assert_eq!(&level_prefix_bitmap(9)[..2], &[0xff, 0x01]);
        assert_eq!(&level_prefix_bitmap(40)[..5], &[0xff; 5]);
    }

    #[test]
    fn decoder_reads_source_mapped_state() {
        let mut wram = [0_u8; WRAM_SIZE];
        wram[PLAYER_X_HIGH] = 0x34;
        wram[PLAYER_X_LOW] = 0xa0;
        wram[PLAYER_Y_HIGH] = 0x0b;
        wram[PLAYER_Y_LOW] = 0x80;
        wram[PLAYER_HEALTH] = 4;
        wram[LEVEL_NUMBER] = 9;
        wram[STARTED_LEVEL_NUMBER] = 7;
        wram[CHIP_COUNT] = 3;
        wram[CHIPS_NEEDED] = 5;
        let mut save = vec![0_u8; 8 * 1024];
        save[PLAYER_ABILITY] = 6;
        save[LEVEL_CLEARED] = 0b1011;
        save[LEVEL_CLEARED + 5] = 0x80;
        save[LEVEL_AVAILABLE] = 0xff;
        save[COLLECTIBLE_BITS + 7] = 0x80;
        let state = decode_state(&wram, &save).expect("decode fixture");
        assert_eq!((state.x, state.y), (0x34a, 0x0b8));
        assert_eq!((state.level, state.started_level), (9, 7));
        assert_eq!((state.health, state.chips, state.chips_needed), (4, 3, 5));
        assert_eq!((state.cleared_in_order(), state.available_count()), (2, 8));
        assert_eq!(state.collectible_count(), 1);
        assert_eq!((state.keys, state.sun_key), ([0, 0, 0], false));
        assert_eq!((state.carrying_block, state.toggle), (false, false));
        assert_eq!(state.arrow_blocks, 0);
    }

    #[test]
    fn decoder_counts_arrow_puzzle_blocks_in_the_level_map() {
        let wram = [0_u8; WRAM_SIZE];
        let mut save = vec![0_u8; 8 * 1024];
        for (offset, block) in [
            (0, 41),
            (17, 153),
            (0x0fff, 158),
            (0x0ffe, 157),
            (40, 1),
            (41, 46),
            (42, 11),
            (43, 45),
            (44, 155),
            (45, 156),
        ] {
            save[offset] = block;
        }
        save[LEVEL_MAP_BYTES] = 43;
        let state = decode_state(&wram, &save).expect("decode fixture");
        assert_eq!(state.arrow_blocks, 4);
        save[..300].fill(43);
        let many = decode_state(&wram, &save).expect("decode crowded map");
        save[0] = 0;
        let spent = decode_state(&wram, &save).expect("decode spent arrow");
        assert_eq!((many.arrow_blocks, spent.arrow_blocks), (302, 301));
        save[..LEVEL_MAP_BYTES].fill(43);
        let full = decode_state(&wram, &save).expect("decode full map");
        assert_eq!(full.arrow_blocks, 4096);
    }

    #[test]
    fn decoder_counts_held_keys_by_color() {
        let mut wram = [0_u8; WRAM_SIZE];
        wram[CARRYING_SUN_KEY] = 1;
        wram[CARRYING_PICKUP_BLOCK] = 1;
        wram[TOGGLE_BLOCK_ENABLED] = 64;
        let mut save = vec![0_u8; 8 * 1024];
        for (slot, item, amount) in [(0, 4, 0), (3, 2, 1), (5, 9, 4), (9, 3, 0)] {
            save[PER_LEVEL_ITEM_TYPE + slot] = item;
            save[PER_LEVEL_ITEM_AMOUNT + slot] = amount;
        }
        let state = decode_state(&wram, &save).expect("decode fixture");
        assert_eq!((state.keys, state.sun_key), ([2, 1, 1], true));
        assert_eq!((state.carrying_block, state.toggle), (true, true));
    }

    #[test]
    fn decoder_counts_progress_in_every_boss_fight() {
        let save = vec![0_u8; 8 * 1024];
        let fight = |setup: &dyn Fn(&mut [u8; WRAM_SIZE])| {
            let mut wram = [0_u8; WRAM_SIZE];
            wram[OBJECT_TYPE] = 0x20;
            setup(&mut wram);
            decode_state(&wram, &save).expect("decode fixture").fight
        };
        assert_eq!(fight(&|_| {}), 0);
        assert_eq!(
            fight(&|wram| {
                wram[OBJECT_TYPE + 9] = BOSS_FIGHT | 1;
                wram[LEVEL_VARIABLE] = 7;
            }),
            5
        );
        assert_eq!(
            fight(&|wram| {
                wram[OBJECT_TYPE + 9] = BOSS_FIGHT;
                wram[OBJECT_STATE + 9] = OBJECT_STATE_INIT;
            }),
            0
        );
        assert_eq!(
            fight(&|wram| {
                wram[OBJECT_TYPE + 3] = BOSS_FIGHT;
                wram[OBJECT_F3 + 3] = 1;
                wram[LEVEL_VARIABLE] = 10;
            }),
            0
        );
        assert_eq!(
            fight(&|wram| {
                wram[OBJECT_TYPE + 15] = BOSS_FIGHT;
                wram[OBJECT_F3 + 15] = JACK_STONE_FIGHT;
                wram[OBJECT_F4 + 15] = 5;
                wram[OBJECT_VX_HIGH + 15] = 11;
            }),
            11
        );
        for boss in [MOLSNO, FOREHEAD_BLOCK_GUY, JOHN] {
            assert_eq!(
                fight(&|wram| {
                    wram[OBJECT_TYPE + 4] = boss | 1;
                    wram[OBJECT_F3 + 4] = 90;
                    wram[OBJECT_F4 + 4] = 6;
                }),
                6
            );
        }
        assert_eq!(
            fight(&|wram| {
                wram[OBJECT_TYPE] = FIGHTER_MAKER;
                wram[OBJECT_F3] = 2;
                wram[OBJECT_F4] = 3;
            }),
            13
        );
        assert_eq!(
            fight(&|wram| {
                wram[OBJECT_TYPE + 2] = FINAL_BOSS;
                wram[OBJECT_STATE + 2] = 255;
                wram[OBJECT_VX_HIGH + 2] = 30;
            }),
            FINAL_BOSS_HITS
        );
    }

    #[test]
    fn decoder_rejects_short_untrusted_regions() {
        assert!(decode_state(&[], &[0; 8 * 1024]).is_err());
        assert!(decode_state(&[0; WRAM_SIZE], &[]).is_err());
        assert!(decode_state(&[0; CHIP_COUNT], &[0; 8 * 1024]).is_err());
        assert!(decode_state(&[0; WRAM_SIZE], &[0; COLLECTIBLE_BITS + 7]).is_err());
    }
}
