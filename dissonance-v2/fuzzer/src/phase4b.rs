// SPDX-License-Identifier: AGPL-3.0-or-later

//! Phase 4b's deterministic NES target and Super Mario Bros observation seam.

use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    error::Error,
    io::Cursor,
    num::NonZeroUsize,
    rc::Rc,
};

use libafl::{
    Error as LibAflError, HasMetadata, StdFuzzer,
    corpus::{Corpus, CorpusId, InMemoryCorpus},
    events::NopEventManager,
    executors::{Executor, ExitKind, HasObservers},
    feedbacks::{ConstFeedback, EagerOrFeedback, MaxMapFeedback},
    fuzzer::{Evaluator, Fuzzer},
    inputs::Input,
    mutators::{MutationResult, Mutator, SingleChoiceScheduledMutator},
    observers::StdMapObserver,
    schedulers::{QueueScheduler, WeightedScheduler},
    stages::StdMutationalStage,
    state::{HasCorpus, HasExecutions, HasRand, StdState},
};
use libafl_bolts::{
    HasLen, Named,
    rands::{Rand, StdRand},
    tuples::{RefIndexable, tuple_list},
};
use serde::{Deserialize, Serialize};
use tetanes_core::{
    control_deck::{Config, ControlDeck, HeadlessMode},
    input::{JoypadBtnState, Player},
    memory::RamState,
};

use crate::{
    phase2::{TriageLabels, TriageScore},
    phase4a::{MutatorStats, ProducerMetadata},
    target::Target,
};

/// Size of the NES CPU work RAM exposed to an operator.
pub const WRAM_SIZE: usize = 2 * 1024;
/// Longest controller hold accepted from an input.
pub const MAX_HOLD_FRAMES: u8 = 120;
/// Longest action list retained by the SMB mutator stack.
pub const MAX_SMB_ACTIONS: usize = 96;

const SMB_MAP_SIZE: usize = 4096;
const PREFIX_CACHE_CAPACITY: usize = 512;
const DETECTOR_MAP_SIZE: usize = 4096;

/// Pure generated-detector surface for complete SMB RAM traces.
pub trait SmbDetector {
    /// Map mechanical action-boundary observations to deterministic feature keys.
    fn features(&self, observations: &[SmbObservations]) -> Vec<u64>;
}

/// Detector that contributes no generated features.
#[derive(Clone, Copy, Debug, Default)]
pub struct NullSmbDetector;

impl SmbDetector for NullSmbDetector {
    fn features(&self, _observations: &[SmbObservations]) -> Vec<u64> {
        Vec::new()
    }
}

/// Deterministic accounting owned by the host around a generated SMB detector.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbDetectorStats {
    /// Target executions during which the detector was active.
    pub executions: u64,
    /// Distinct generated feature keys first observed by the host.
    pub novelties: u64,
    /// Consecutive active executions without a new generated key.
    pub executions_without_novelty: u64,
    /// Whether the detector remains enabled.
    pub active: bool,
}

// These offsets are used only by the deliberately coarse, operator-authored base map.
const SCREEN_PAGE_OFFSET: usize = 0x071a;
const SCREEN_X_OFFSET: usize = 0x071c;
const PLAYER_Y_OFFSET: usize = 0x00ce;
const WORLD_NUMBER_OFFSET: usize = 0x075f;
const LEVEL_NUMBER_OFFSET: usize = 0x075c;
const FLAG_TASK_OFFSET: usize = 0x0746;
const MUTATION_BUTTON_MASKS: [u8; 10] =
    [0x00, 0x01, 0x02, 0x08, 0x40, 0x80, 0x81, 0x82, 0x83, 0x10];

/// One total NES input action: an eight-button mask held for a bounded frame count.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ButtonChord {
    /// Standard NES controller bits: A, B, Select, Start, Up, Down, Left, Right.
    pub buttons: u8,
    /// Requested hold duration. Execution clamps this to `1..=MAX_HOLD_FRAMES`.
    pub hold_frames: u8,
}

impl ButtonChord {
    /// Construct a chord, normalizing its duration into the target's total domain.
    #[must_use]
    pub fn new(buttons: u8, hold_frames: u8) -> Self {
        Self {
            buttons,
            hold_frames: hold_frames.clamp(1, MAX_HOLD_FRAMES),
        }
    }

    /// Return the normalized hold duration used by execution.
    #[must_use]
    pub fn bounded_hold_frames(self) -> u8 {
        self.hold_frames.clamp(1, MAX_HOLD_FRAMES)
    }
}

/// A Super Mario Bros input replayed from the deterministic power-on state.
#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SmbInput {
    /// Controller chords in execution order.
    pub actions: Vec<ButtonChord>,
}

impl Input for SmbInput {}

impl HasLen for SmbInput {
    fn len(&self) -> usize {
        self.actions.len()
    }
}

/// Pure generated semantic-mutator surface for SMB action lists.
pub trait SmbMacro {
    /// Produce one deterministic candidate from an immutable input.
    fn mutate(&self, input: &SmbInput) -> SmbInput;
}

/// Macro that contributes no candidate.
#[derive(Clone, Copy, Debug, Default)]
pub struct NullSmbMacro;

impl SmbMacro for NullSmbMacro {
    fn mutate(&self, input: &SmbInput) -> SmbInput {
        input.clone()
    }
}

/// Host-owned adapter providing provenance and mechanical retirement for one generated macro.
#[derive(Debug)]
pub struct GeneratedSmbMacroAdapter<M> {
    generated: M,
    enabled: bool,
    name: Cow<'static, str>,
    retire_after: u64,
    stats: Rc<RefCell<MutatorStats>>,
    emitted: bool,
}

impl<M> GeneratedSmbMacroAdapter<M> {
    /// Wrap generated code with deterministic host policy.
    #[must_use]
    pub fn new(
        generated: M,
        enabled: bool,
        name: impl Into<Cow<'static, str>>,
        retire_after: u64,
        stats: Rc<RefCell<MutatorStats>>,
    ) -> Self {
        Self {
            generated,
            enabled,
            name: name.into(),
            retire_after,
            stats,
            emitted: false,
        }
    }
}

impl<M> Named for GeneratedSmbMacroAdapter<M> {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<M, S> Mutator<SmbInput, S> for GeneratedSmbMacroAdapter<M>
where
    M: SmbMacro,
    S: HasCorpus<SmbInput>,
{
    fn mutate(
        &mut self,
        _state: &mut S,
        input: &mut SmbInput,
    ) -> Result<MutationResult, LibAflError> {
        self.emitted = false;
        if !self.enabled || !self.stats.borrow().active {
            return Ok(MutationResult::Skipped);
        }
        let candidate = self.generated.mutate(input);
        if candidate.actions.len() > MAX_SMB_ACTIONS {
            return Err(LibAflError::illegal_state(
                "generated SMB macro exceeded the action limit",
            ));
        }
        if candidate == *input {
            return Ok(MutationResult::Skipped);
        }
        *input = candidate;
        self.emitted = true;
        let offspring = self.stats.borrow().offspring.saturating_add(1);
        self.stats.borrow_mut().offspring = offspring;
        Ok(MutationResult::Mutated)
    }

    fn post_exec(
        &mut self,
        state: &mut S,
        new_corpus_id: Option<CorpusId>,
    ) -> Result<(), LibAflError> {
        if !self.emitted {
            return Ok(());
        }
        self.emitted = false;
        if let Some(id) = new_corpus_id {
            state
                .corpus()
                .get(id)?
                .borrow_mut()
                .add_metadata(ProducerMetadata {
                    mutator: self.name.to_string(),
                });
        }
        let mut stats = self.stats.borrow_mut();
        if new_corpus_id.is_some() {
            stats.novel_offspring = stats.novel_offspring.saturating_add(1);
            stats.executions_without_novelty = 0;
        } else {
            stats.executions_without_novelty = stats.executions_without_novelty.saturating_add(1);
            if stats.executions_without_novelty >= self.retire_after {
                stats.active = false;
            }
        }
        Ok(())
    }
}

macro_rules! smb_named_mutator {
    ($name:ident) => {
        #[doc = concat!("Generic SMB list mutator: `", stringify!($name), "`.")]
        #[derive(Debug)]
        pub struct $name {
            name: Cow<'static, str>,
        }

        impl Default for $name {
            fn default() -> Self {
                Self {
                    name: Cow::Borrowed(stringify!($name)),
                }
            }
        }

        impl Named for $name {
            fn name(&self) -> &Cow<'static, str> {
                &self.name
            }
        }
    };
}

smb_named_mutator!(AppendButtonChordMutator);
smb_named_mutator!(PerturbButtonChordMutator);
smb_named_mutator!(TruncateButtonChordMutator);
smb_named_mutator!(SpliceButtonChordMutator);

fn random_chord<S>(state: &mut S) -> ButtonChord
where
    S: HasRand,
{
    sample_chord(state.rand_mut())
}

fn sample_chord<R>(rand: &mut R) -> ButtonChord
where
    R: Rand,
{
    let button_index = rand
        .below(NonZeroUsize::new(MUTATION_BUTTON_MASKS.len()).expect("nonempty button vocabulary"));
    let buttons = MUTATION_BUTTON_MASKS[button_index];
    let hold = if buttons == 0x08 {
        1
    } else {
        rand.below(NonZeroUsize::new(usize::from(MAX_HOLD_FRAMES)).expect("positive hold bound"))
            + 1
    };
    ButtonChord::new(buttons, hold as u8)
}

impl<S> Mutator<SmbInput, S> for AppendButtonChordMutator
where
    S: HasRand,
{
    fn mutate(
        &mut self,
        state: &mut S,
        input: &mut SmbInput,
    ) -> Result<MutationResult, LibAflError> {
        if input.actions.len() >= MAX_SMB_ACTIONS {
            return Ok(MutationResult::Skipped);
        }
        input.actions.push(random_chord(state));
        Ok(MutationResult::Mutated)
    }

    fn post_exec(
        &mut self,
        _state: &mut S,
        _new_corpus_id: Option<CorpusId>,
    ) -> Result<(), LibAflError> {
        Ok(())
    }
}

impl<S> Mutator<SmbInput, S> for PerturbButtonChordMutator
where
    S: HasRand,
{
    fn mutate(
        &mut self,
        state: &mut S,
        input: &mut SmbInput,
    ) -> Result<MutationResult, LibAflError> {
        let Some(length) = NonZeroUsize::new(input.actions.len()) else {
            return Ok(MutationResult::Skipped);
        };
        let index = state.rand_mut().below(length);
        let replacement = random_chord(state);
        if replacement == input.actions[index] {
            return Ok(MutationResult::Skipped);
        }
        input.actions[index] = replacement;
        Ok(MutationResult::Mutated)
    }

    fn post_exec(
        &mut self,
        _state: &mut S,
        _new_corpus_id: Option<CorpusId>,
    ) -> Result<(), LibAflError> {
        Ok(())
    }
}

impl<S> Mutator<SmbInput, S> for TruncateButtonChordMutator
where
    S: HasRand,
{
    fn mutate(
        &mut self,
        state: &mut S,
        input: &mut SmbInput,
    ) -> Result<MutationResult, LibAflError> {
        let Some(length) = NonZeroUsize::new(input.actions.len()) else {
            return Ok(MutationResult::Skipped);
        };
        input.actions.truncate(state.rand_mut().below(length));
        Ok(MutationResult::Mutated)
    }

    fn post_exec(
        &mut self,
        _state: &mut S,
        _new_corpus_id: Option<CorpusId>,
    ) -> Result<(), LibAflError> {
        Ok(())
    }
}

impl<S> Mutator<SmbInput, S> for SpliceButtonChordMutator
where
    S: HasCorpus<SmbInput> + HasRand,
{
    fn mutate(
        &mut self,
        state: &mut S,
        input: &mut SmbInput,
    ) -> Result<MutationResult, LibAflError> {
        let Some(corpus_len) = NonZeroUsize::new(state.corpus().count()) else {
            return Ok(MutationResult::Skipped);
        };
        let corpus_offset = state.rand_mut().below(corpus_len);
        let other_id = state.corpus().nth(corpus_offset);
        let other = state.corpus().cloned_input_for_id(other_id)?;
        let Some(other_len) = NonZeroUsize::new(other.actions.len()) else {
            return Ok(MutationResult::Skipped);
        };
        let prefix = state.rand_mut().below_or_zero(input.actions.len() + 1);
        let suffix = state.rand_mut().below(other_len);
        let mut actions = input.actions[..prefix].to_vec();
        actions.extend_from_slice(&other.actions[suffix..]);
        actions.truncate(MAX_SMB_ACTIONS);
        if actions == input.actions {
            return Ok(MutationResult::Skipped);
        }
        input.actions = actions;
        Ok(MutationResult::Mutated)
    }

    fn post_exec(
        &mut self,
        _state: &mut S,
        _new_corpus_id: Option<CorpusId>,
    ) -> Result<(), LibAflError> {
        Ok(())
    }
}

/// Mechanical evidence captured at one NES action boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbObservations {
    /// Number of requested emulated frames since genesis.
    pub frame_count: u64,
    /// Complete 2 KiB NES CPU work RAM, with no semantic decoding.
    pub wram: Vec<u8>,
    /// Sorted work-RAM indices whose bytes changed since the previous boundary.
    pub changed_indices: Vec<u16>,
    /// Compact mechanical log line; it deliberately contains no decoded game fields.
    pub log_line: String,
}

/// Stable JSON request passed to the model triager for one retained SMB testcase.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbTriageRequest {
    /// Monotonic corpus index assigned by the host view.
    pub testcase_id: u64,
    /// Complete mechanical RAM evidence at action boundaries.
    pub observations: Vec<SmbObservations>,
    /// Compact mechanical log with no decoded game fields.
    pub log: String,
}

/// Complete in-memory state needed to resume an NES prefix exactly.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbSnapshot {
    emulator_state: Vec<u8>,
    observation: SmbObservations,
    failed: bool,
}

/// Deterministic TetaNES-backed target used by the Super Mario Bros campaigns.
#[derive(Debug)]
pub struct SmbTarget {
    deck: ControlDeck,
    genesis_state: Vec<u8>,
    observation: SmbObservations,
    failed: bool,
}

impl SmbTarget {
    /// Load an iNES ROM from memory with deterministic RAM and no persistent SRAM access.
    ///
    /// # Errors
    ///
    /// Returns a TetaNES error if the ROM cannot be loaded or its genesis state cannot be saved.
    pub fn from_rom_bytes(rom: &[u8]) -> tetanes_core::control_deck::Result<Self> {
        Self::from_rom_bytes_with_mode(rom, HeadlessMode::NO_AUDIO)
    }

    fn from_rom_bytes_with_mode(
        rom: &[u8],
        headless_mode: HeadlessMode,
    ) -> tetanes_core::control_deck::Result<Self> {
        let mut deck = ControlDeck::with_config(Config {
            ram_state: RamState::AllZeros,
            headless_mode,
            sram_dir: None,
            run_ahead: 0,
            ..Config::default()
        });
        deck.load_rom("campaign.nes", &mut Cursor::new(rom))?;
        let mut genesis_state = Vec::new();
        deck.save_state(&mut genesis_state)?;
        let observation = observation_from(&deck, 0, &[0; WRAM_SIZE]);
        Ok(Self {
            deck,
            genesis_state,
            observation,
            failed: false,
        })
    }

    /// Load SMB and deterministically advance through its title screen to a gameplay genesis.
    ///
    /// This fixed boot sequence is target setup rather than model-visible search guidance. The
    /// campaign input begins at the resulting state, and `reset` returns to it exactly.
    ///
    /// # Errors
    ///
    /// Returns a TetaNES error if ROM loading, frame execution, or state saving fails.
    pub fn from_smb_rom_bytes(rom: &[u8]) -> tetanes_core::control_deck::Result<Self> {
        Self::from_smb_rom_bytes_with_mode(rom, HeadlessMode::NO_AUDIO)
    }

    /// Load SMB at gameplay genesis with both audio and video work disabled for campaigns.
    ///
    /// # Errors
    ///
    /// Returns a TetaNES error if ROM loading, frame execution, or state saving fails.
    pub fn from_smb_rom_bytes_headless(rom: &[u8]) -> tetanes_core::control_deck::Result<Self> {
        Self::from_smb_rom_bytes_with_mode(rom, HeadlessMode::NO_AUDIO | HeadlessMode::NO_VIDEO)
    }

    fn from_smb_rom_bytes_with_mode(
        rom: &[u8],
        headless_mode: HeadlessMode,
    ) -> tetanes_core::control_deck::Result<Self> {
        let mut target = Self::from_rom_bytes_with_mode(rom, headless_mode)?;
        target.deck.joypad_mut(Player::One).buttons = JoypadBtnState::empty();
        for _ in 0..120 {
            let _ = target.deck.clock_frame()?;
        }
        target.deck.joypad_mut(Player::One).buttons = JoypadBtnState::START;
        let _ = target.deck.clock_frame()?;
        target.deck.joypad_mut(Player::One).buttons = JoypadBtnState::empty();
        for _ in 0..240 {
            let _ = target.deck.clock_frame()?;
        }
        let mut genesis_state = Vec::new();
        target.deck.save_state(&mut genesis_state)?;
        target.genesis_state = genesis_state;
        target.observation = observation_from(&target.deck, 0, target.deck.wram());
        target.observation.changed_indices.clear();
        target.observation.log_line = "frame=0 changed=[]".to_owned();
        Ok(target)
    }

    /// Return the latest RGBA frame for film generation.
    #[must_use]
    pub fn frame_rgba(&mut self) -> Vec<u8> {
        self.deck.frame_buffer().to_vec()
    }

    /// Advance exactly one video frame with the supplied raw controller mask.
    ///
    /// This is the film-generation seam: campaign execution continues to use bounded
    /// [`ButtonChord`] actions, while a renderer can capture every intermediate frame.
    ///
    /// # Errors
    ///
    /// Returns a TetaNES error if frame execution fails.
    pub fn clock_frame_for_film(&mut self, buttons: u8) -> tetanes_core::control_deck::Result<()> {
        self.deck.joypad_mut(Player::One).buttons =
            JoypadBtnState::from_bits_truncate(u16::from(buttons));
        let result = self.deck.clock_frame();
        if result.is_err() {
            self.failed = true;
        }
        result.map(|_| ())
    }

    /// Release every controller button after a filmed chord.
    pub fn release_buttons_for_film(&mut self) {
        self.deck.joypad_mut(Player::One).buttons = JoypadBtnState::empty();
    }

    /// Return the latest raw work RAM without semantic decoding.
    #[must_use]
    pub fn wram(&self) -> &[u8; WRAM_SIZE] {
        self.deck.wram()
    }
}

/// Replay one SMB input from gameplay genesis and return its mechanical observation trace.
pub fn observe_smb_input(
    rom: &[u8],
    input: &SmbInput,
) -> tetanes_core::control_deck::Result<Vec<SmbObservations>> {
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    let mut observations = vec![target.observe()];
    for action in &input.actions {
        target.apply(action);
        observations.push(target.observe());
        if target.exit_kind() != ExitKind::Ok {
            break;
        }
    }
    Ok(observations)
}

impl Target for SmbTarget {
    type Action = ButtonChord;
    type Observations = SmbObservations;
    type Snapshot = SmbSnapshot;

    fn reset(&mut self) {
        self.failed = self
            .deck
            .load_state(Cursor::new(&self.genesis_state))
            .is_err();
        self.observation = observation_from(&self.deck, 0, &[0; WRAM_SIZE]);
    }

    fn apply(&mut self, action: &Self::Action) {
        if self.failed {
            return;
        }
        let prior_wram = self.deck.wram().to_owned();
        self.deck.joypad_mut(Player::One).buttons =
            JoypadBtnState::from_bits_truncate(u16::from(action.buttons));
        let hold_frames = action.bounded_hold_frames();
        for _ in 0..hold_frames {
            if self.deck.clock_frame().is_err() {
                self.failed = true;
                break;
            }
        }
        self.deck.joypad_mut(Player::One).buttons = JoypadBtnState::empty();
        self.observation = observation_from(
            &self.deck,
            self.observation.frame_count + u64::from(hold_frames),
            &prior_wram,
        );
    }

    fn observe(&self) -> Self::Observations {
        self.observation.clone()
    }

    fn fingerprint(&self) -> u64 {
        let wram = self.deck.wram();
        let screen_page = u64::from(wram[SCREEN_PAGE_OFFSET]);
        let screen_x_bucket = u64::from(wram[SCREEN_X_OFFSET] / 64);
        let player_y_bucket = u64::from(wram[PLAYER_Y_OFFSET] / 32);
        (screen_page << 8) | (screen_x_bucket << 4) | player_y_bucket
    }

    fn exit_kind(&self) -> ExitKind {
        if self.failed {
            ExitKind::Crash
        } else {
            ExitKind::Ok
        }
    }

    fn snapshot(&mut self) -> Option<Self::Snapshot> {
        let mut emulator_state = Vec::new();
        if self.deck.save_state(&mut emulator_state).is_err() {
            self.failed = true;
            return None;
        }
        Some(SmbSnapshot {
            emulator_state,
            observation: self.observation.clone(),
            failed: self.failed,
        })
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), LibAflError> {
        self.deck
            .load_state(Cursor::new(&snapshot.emulator_state))
            .map_err(|error| LibAflError::illegal_state(error.to_string()))?;
        self.observation = snapshot.observation.clone();
        self.failed = snapshot.failed;
        Ok(())
    }
}

fn observation_from(
    deck: &ControlDeck,
    frame_count: u64,
    prior_wram: &[u8; WRAM_SIZE],
) -> SmbObservations {
    let wram = deck.wram();
    let changed_indices = wram
        .iter()
        .zip(prior_wram)
        .enumerate()
        .filter_map(|(index, (current, prior))| {
            (current != prior)
                .then(|| u16::try_from(index).ok())
                .flatten()
        })
        .collect::<Vec<_>>();
    let log_line = format!("frame={frame_count} changed={changed_indices:?}");
    SmbObservations {
        frame_count,
        wram: wram.to_vec(),
        changed_indices,
        log_line,
    }
}

/// Frozen M5 milestone ladder, accumulated over every action boundary in a run.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbMilestones {
    /// Greatest 64-pixel scroll bucket observed while the RAM level tuple is 1-1.
    pub max_1_1_scroll_bucket: u16,
    /// Whether the 1-1 flag-task byte was observed active.
    pub reached_1_1_flag: bool,
    /// Whether the RAM level tuple reached 1-2.
    pub reached_1_2: bool,
    /// Whether the RAM level tuple advanced beyond 1-2.
    pub reached_onward: bool,
}

impl SmbMilestones {
    fn merge(&mut self, other: Self) {
        self.max_1_1_scroll_bucket = self.max_1_1_scroll_bucket.max(other.max_1_1_scroll_bucket);
        self.reached_1_1_flag |= other.reached_1_1_flag;
        self.reached_1_2 |= other.reached_1_2;
        self.reached_onward |= other.reached_onward;
    }
}

/// First deterministic execution reaching each frozen M5 rung.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbMilestoneTimes {
    /// First execution reaching a nonzero 1-1 scroll bucket.
    pub progress_into_1_1: Option<u64>,
    /// First execution observing the 1-1 flag task.
    pub flag_1_1: Option<u64>,
    /// First execution observing level 1-2.
    pub level_1_2: Option<u64>,
    /// First execution observing a level beyond 1-2.
    pub onward: Option<u64>,
}

/// First testcase reaching each frozen ladder rung, retained for M7 films.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbMilestoneInputs {
    /// First testcase making nonzero progress into 1-1.
    pub progress_into_1_1: Option<SmbInput>,
    /// First testcase observing the 1-1 flag task.
    pub flag_1_1: Option<SmbInput>,
    /// First testcase observing level 1-2.
    pub level_1_2: Option<SmbInput>,
    /// First testcase observing a level beyond 1-2.
    pub onward: Option<SmbInput>,
}

/// Deterministic report for one M5 ratchet or random-mash run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbCampaignReport {
    /// Seed supplied to the LibAFL RNG.
    pub seed: u64,
    /// Number of target executions performed.
    pub executions: u64,
    /// Strongest milestone values observed over the campaign.
    pub milestones: SmbMilestones,
    /// First execution reaching each boolean ladder rung.
    pub first_reached: SmbMilestoneTimes,
    /// First testcase reaching each rung, for exact replay and film generation.
    pub first_inputs: SmbMilestoneInputs,
    /// Retained inputs in insertion order; empty for random mash.
    pub corpus: Vec<SmbInput>,
}

/// Report for an M6 arm with host-owned generated-artifact accounting.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbConfiguredReport {
    /// Search and milestone result.
    pub campaign: SmbCampaignReport,
    /// Host-assigned detector provenance name.
    pub detector_name: String,
    /// Generated-detector novelty and retirement counters.
    pub detector: SmbDetectorStats,
    /// Host-assigned macro provenance name.
    pub macro_name: String,
    /// Generated-macro offspring and retirement counters.
    pub macro_stats: MutatorStats,
}

/// Host policy and provenance names for one configured M6 arm.
#[derive(Clone, Copy, Debug)]
pub struct SmbArtifactConfig<'a> {
    /// Host-assigned detector provenance name.
    pub detector_name: &'a str,
    /// Detector retirement threshold in executions without novelty.
    pub detector_retire_after: u64,
    /// Host-assigned macro provenance name.
    pub macro_name: &'a str,
    /// Macro retirement threshold in emitted candidates without novelty.
    pub macro_retire_after: u64,
    /// Whether the generated macro participates in mutation scheduling.
    pub enable_macro: bool,
}

/// One persisted M5 corpus input with model or neutral scheduler labels.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbLabeledCorpusEntry {
    /// Retained typed input.
    pub input: SmbInput,
    /// Structured labels consumed by the weighted scheduler.
    pub labels: TriageLabels,
}

type SmbObserver = StdMapObserver<'static, u8, false>;
type SmbObservers = (SmbObserver, (SmbObserver, ()));

/// In-process TetaNES executor that writes the base map and one generated-detector map.
#[derive(Debug)]
pub struct SmbExecutor<D> {
    observers: SmbObservers,
    target: SmbTarget,
    detector: D,
    detector_seen: BTreeMap<u64, ()>,
    detector_stats: SmbDetectorStats,
    detector_retire_after: u64,
    last_milestones: SmbMilestones,
    last_input: SmbInput,
    prefix_cache: BTreeMap<Vec<ButtonChord>, CachedPrefix>,
    cache_order: VecDeque<Vec<ButtonChord>>,
}

#[derive(Clone, Debug)]
struct CachedPrefix {
    snapshot: SmbSnapshot,
    milestones: SmbMilestones,
    base_features: Vec<usize>,
    observations: Vec<SmbObservations>,
}

impl<D> SmbExecutor<D>
where
    D: SmbDetector,
{
    fn new(
        base_observer: SmbObserver,
        detector_observer: SmbObserver,
        rom: &[u8],
        detector: D,
        detector_retire_after: u64,
    ) -> tetanes_core::control_deck::Result<Self> {
        Ok(Self {
            observers: tuple_list!(base_observer, detector_observer),
            target: SmbTarget::from_smb_rom_bytes_headless(rom)?,
            detector,
            detector_seen: BTreeMap::new(),
            detector_stats: SmbDetectorStats {
                active: true,
                ..SmbDetectorStats::default()
            },
            detector_retire_after,
            last_milestones: SmbMilestones::default(),
            last_input: SmbInput::default(),
            prefix_cache: BTreeMap::new(),
            cache_order: VecDeque::new(),
        })
    }

    /// Milestones reached by the most recently executed input.
    #[must_use]
    pub fn last_milestones(&self) -> SmbMilestones {
        self.last_milestones
    }

    /// Current host-owned generated-detector accounting.
    #[must_use]
    pub fn detector_stats(&self) -> SmbDetectorStats {
        self.detector_stats
    }

    /// Complete mechanical trace for the most recently executed input.
    #[must_use]
    pub fn last_observations(&self) -> &[SmbObservations] {
        self.prefix_cache
            .get(&self.last_input.actions)
            .map_or(&[], |cached| cached.observations.as_slice())
    }

    fn last_input(&self) -> &SmbInput {
        &self.last_input
    }

    fn longest_cached_prefix(&self, input: &SmbInput) -> Option<(usize, CachedPrefix)> {
        (0..=input.actions.len()).rev().find_map(|length| {
            self.prefix_cache
                .get(&input.actions[..length])
                .cloned()
                .map(|entry| (length, entry))
        })
    }

    fn cache_prefix(
        &mut self,
        key: Vec<ButtonChord>,
        milestones: SmbMilestones,
        base_features: Vec<usize>,
        observations: Vec<SmbObservations>,
    ) -> Result<(), LibAflError> {
        if self.prefix_cache.contains_key(&key) {
            return Ok(());
        }
        let snapshot = self.target.snapshot().ok_or_else(|| {
            LibAflError::illegal_state("TetaNES failed to save a campaign prefix")
        })?;
        if self.prefix_cache.len() >= PREFIX_CACHE_CAPACITY {
            let oldest = self.cache_order.pop_front().ok_or_else(|| {
                LibAflError::illegal_state("SMB prefix cache order was unexpectedly empty")
            })?;
            self.prefix_cache.remove(&oldest);
        }
        self.cache_order.push_back(key.clone());
        self.prefix_cache.insert(
            key,
            CachedPrefix {
                snapshot,
                milestones,
                base_features,
                observations,
            },
        );
        Ok(())
    }
}

impl<D> HasObservers for SmbExecutor<D> {
    type Observers = SmbObservers;

    fn observers(&self) -> RefIndexable<&Self::Observers, Self::Observers> {
        RefIndexable::from(&self.observers)
    }

    fn observers_mut(&mut self) -> RefIndexable<&mut Self::Observers, Self::Observers> {
        RefIndexable::from(&mut self.observers)
    }
}

impl<D, EM, S, Z> Executor<EM, SmbInput, S, Z> for SmbExecutor<D>
where
    D: SmbDetector,
    S: HasExecutions,
{
    fn run_target(
        &mut self,
        _fuzzer: &mut Z,
        state: &mut S,
        _manager: &mut EM,
        input: &SmbInput,
    ) -> Result<ExitKind, LibAflError> {
        *state.executions_mut() = state.executions().saturating_add(1);
        self.last_input = input.clone();
        let (start, mut base_features, mut observations) =
            if let Some((length, cached)) = self.longest_cached_prefix(input) {
                self.target.restore(&cached.snapshot)?;
                self.last_milestones = cached.milestones;
                for index in &cached.base_features {
                    self.observers.0[*index] = 1;
                }
                (length, cached.base_features, cached.observations)
            } else {
                self.target.reset();
                self.last_milestones = smb_milestones_from_wram(self.target.wram());
                let index = set_base_feature(&mut self.observers.0, self.target.fingerprint());
                (0, vec![index], vec![self.target.observe()])
            };
        for action in &input.actions[start..] {
            self.target.apply(action);
            self.last_milestones
                .merge(smb_milestones_from_wram(self.target.wram()));
            base_features.push(set_base_feature(
                &mut self.observers.0,
                self.target.fingerprint(),
            ));
            observations.push(self.target.observe());
            if self.target.exit_kind() != ExitKind::Ok {
                break;
            }
        }
        if self.detector_stats.active {
            self.detector_stats.executions = self.detector_stats.executions.saturating_add(1);
            let mut novel = 0_u64;
            for key in self.detector.features(&observations) {
                let index = usize::try_from(
                    key.wrapping_mul(0x517c_c1b7) % u64::try_from(DETECTOR_MAP_SIZE).unwrap_or(1),
                )
                .unwrap_or(0);
                self.observers.1.0[index] = 1;
                if self.detector_seen.insert(key, ()).is_none() {
                    novel = novel.saturating_add(1);
                }
            }
            self.detector_stats.novelties = self.detector_stats.novelties.saturating_add(novel);
            if novel > 0 {
                self.detector_stats.executions_without_novelty = 0;
            } else {
                self.detector_stats.executions_without_novelty = self
                    .detector_stats
                    .executions_without_novelty
                    .saturating_add(1);
                if self.detector_stats.executions_without_novelty >= self.detector_retire_after {
                    self.detector_stats.active = false;
                }
            }
        }
        self.cache_prefix(
            input.actions.clone(),
            self.last_milestones,
            base_features,
            observations,
        )?;
        Ok(self.target.exit_kind())
    }
}

fn set_base_feature(observer: &mut SmbObserver, fingerprint: u64) -> usize {
    let index = usize::try_from(
        fingerprint.wrapping_mul(0x9e37_79b1) % u64::try_from(SMB_MAP_SIZE).unwrap_or(1),
    )
    .unwrap_or(0);
    observer[index] = 1;
    index
}

/// Decode only the predeclared campaign metric from SMB work RAM.
#[must_use]
pub fn smb_milestones_from_wram(wram: &[u8; WRAM_SIZE]) -> SmbMilestones {
    let world = wram[WORLD_NUMBER_OFFSET];
    let level = wram[LEVEL_NUMBER_OFFSET];
    let in_1_1 = world == 0 && level == 0;
    let scroll_bucket = if in_1_1 {
        u16::from(wram[SCREEN_PAGE_OFFSET]) * 4 + u16::from(wram[SCREEN_X_OFFSET] / 64)
    } else {
        0
    };
    SmbMilestones {
        max_1_1_scroll_bucket: scroll_bucket,
        reached_1_1_flag: in_1_1 && wram[FLAG_TASK_OFFSET] != 0,
        reached_1_2: world == 0 && level == 1,
        reached_onward: world > 0 || (world == 0 && level > 1),
    }
}

fn update_campaign_milestones(
    aggregate: &mut SmbMilestones,
    times: &mut SmbMilestoneTimes,
    first_inputs: &mut SmbMilestoneInputs,
    current: SmbMilestones,
    execution: u64,
    input: &SmbInput,
) {
    aggregate.merge(current);
    if current.max_1_1_scroll_bucket > 0 {
        times.progress_into_1_1.get_or_insert(execution);
        first_inputs
            .progress_into_1_1
            .get_or_insert_with(|| input.clone());
    }
    if current.reached_1_1_flag {
        times.flag_1_1.get_or_insert(execution);
        first_inputs.flag_1_1.get_or_insert_with(|| input.clone());
    }
    if current.reached_1_2 {
        times.level_1_2.get_or_insert(execution);
        first_inputs.level_1_2.get_or_insert_with(|| input.clone());
    }
    if current.reached_onward {
        times.onward.get_or_insert(execution);
        first_inputs.onward.get_or_insert_with(|| input.clone());
    }
}

/// Run the M5 null-triage, base-map corpus ratchet for a bounded execution count.
pub fn run_smb_ratchet(
    rom: &[u8],
    seed: u64,
    execution_budget: u64,
) -> Result<SmbCampaignReport, Box<dyn Error>> {
    let base_observer = StdMapObserver::owned("smb_base_position", vec![0_u8; SMB_MAP_SIZE]);
    let detector_observer =
        StdMapObserver::owned("smb_generated_detector", vec![0_u8; DETECTOR_MAP_SIZE]);
    let mut feedback = EagerOrFeedback::new(
        MaxMapFeedback::new(&base_observer),
        MaxMapFeedback::new(&detector_observer),
    );
    let mut objective = ConstFeedback::new(false);
    let mut state = StdState::new(
        StdRand::with_seed(seed),
        InMemoryCorpus::<SmbInput>::new(),
        InMemoryCorpus::<SmbInput>::new(),
        &mut feedback,
        &mut objective,
    )?;
    let mut fuzzer = StdFuzzer::new(QueueScheduler::new(), feedback, objective);
    let mut manager = NopEventManager::new();
    let mut executor = SmbExecutor::new(
        base_observer,
        detector_observer,
        rom,
        NullSmbDetector,
        u64::MAX,
    )?;
    fuzzer.add_input(&mut state, &mut executor, &mut manager, SmbInput::default())?;

    let mutator = SingleChoiceScheduledMutator::new(tuple_list!(
        AppendButtonChordMutator::default(),
        PerturbButtonChordMutator::default(),
        TruncateButtonChordMutator::default(),
        SpliceButtonChordMutator::default(),
    ));
    let mut stages = tuple_list!(StdMutationalStage::with_max_iterations(
        mutator,
        NonZeroUsize::MIN,
    ));
    let mut milestones = executor.last_milestones();
    let mut first_reached = SmbMilestoneTimes::default();
    let mut first_inputs = SmbMilestoneInputs::default();
    while *state.executions() < execution_budget {
        fuzzer.fuzz_one(&mut stages, &mut executor, &mut state, &mut manager)?;
        update_campaign_milestones(
            &mut milestones,
            &mut first_reached,
            &mut first_inputs,
            executor.last_milestones(),
            *state.executions(),
            executor.last_input(),
        );
    }

    let mut corpus = Vec::with_capacity(state.corpus().count());
    for id in state.corpus().ids() {
        corpus.push(state.corpus().cloned_input_for_id(id)?);
    }
    Ok(SmbCampaignReport {
        seed,
        executions: *state.executions(),
        milestones,
        first_reached,
        first_inputs,
        corpus,
    })
}

/// Run an M6 arm with one compiled detector and one optional compiled macro.
pub fn run_smb_configured<D, M>(
    rom: &[u8],
    seed: u64,
    execution_budget: u64,
    detector: D,
    macro_generator: M,
    artifacts: SmbArtifactConfig<'_>,
) -> Result<SmbConfiguredReport, Box<dyn Error>>
where
    D: SmbDetector,
    M: SmbMacro,
{
    let base_observer = StdMapObserver::owned("smb_base_position", vec![0_u8; SMB_MAP_SIZE]);
    let detector_observer =
        StdMapObserver::owned("smb_generated_detector", vec![0_u8; DETECTOR_MAP_SIZE]);
    let mut feedback = EagerOrFeedback::new(
        MaxMapFeedback::new(&base_observer),
        MaxMapFeedback::new(&detector_observer),
    );
    let mut objective = ConstFeedback::new(false);
    let mut state = StdState::new(
        StdRand::with_seed(seed),
        InMemoryCorpus::<SmbInput>::new(),
        InMemoryCorpus::<SmbInput>::new(),
        &mut feedback,
        &mut objective,
    )?;
    let mut fuzzer = StdFuzzer::new(QueueScheduler::new(), feedback, objective);
    let mut manager = NopEventManager::new();
    let mut executor = SmbExecutor::new(
        base_observer,
        detector_observer,
        rom,
        detector,
        artifacts.detector_retire_after,
    )?;
    fuzzer.add_input(&mut state, &mut executor, &mut manager, SmbInput::default())?;

    let macro_stats = Rc::new(RefCell::new(MutatorStats::default()));
    let generated_macro = GeneratedSmbMacroAdapter::new(
        macro_generator,
        artifacts.enable_macro,
        artifacts.macro_name.to_owned(),
        artifacts.macro_retire_after,
        Rc::clone(&macro_stats),
    );
    let mutator = SingleChoiceScheduledMutator::new(tuple_list!(
        AppendButtonChordMutator::default(),
        PerturbButtonChordMutator::default(),
        TruncateButtonChordMutator::default(),
        SpliceButtonChordMutator::default(),
        generated_macro,
    ));
    let mut stages = tuple_list!(StdMutationalStage::with_max_iterations(
        mutator,
        NonZeroUsize::MIN,
    ));
    let mut milestones = executor.last_milestones();
    let mut first_reached = SmbMilestoneTimes::default();
    let mut first_inputs = SmbMilestoneInputs::default();
    while *state.executions() < execution_budget {
        fuzzer.fuzz_one(&mut stages, &mut executor, &mut state, &mut manager)?;
        update_campaign_milestones(
            &mut milestones,
            &mut first_reached,
            &mut first_inputs,
            executor.last_milestones(),
            *state.executions(),
            executor.last_input(),
        );
    }

    let mut corpus = Vec::with_capacity(state.corpus().count());
    for id in state.corpus().ids() {
        corpus.push(state.corpus().cloned_input_for_id(id)?);
    }
    let campaign = SmbCampaignReport {
        seed,
        executions: *state.executions(),
        milestones,
        first_reached,
        first_inputs,
        corpus,
    };
    Ok(SmbConfiguredReport {
        campaign,
        detector_name: artifacts.detector_name.to_owned(),
        detector: executor.detector_stats(),
        macro_name: artifacts.macro_name.to_owned(),
        macro_stats: macro_stats.borrow().clone(),
    })
}

/// Restore an M5 corpus, attach recorded labels, and run one cumulative M6 arm.
pub fn run_smb_restart_configured<D, M>(
    rom: &[u8],
    initial_corpus: &[SmbLabeledCorpusEntry],
    seed: u64,
    execution_budget: u64,
    detector: D,
    macro_generator: M,
    artifacts: SmbArtifactConfig<'_>,
) -> Result<SmbConfiguredReport, Box<dyn Error>>
where
    D: SmbDetector,
    M: SmbMacro,
{
    if initial_corpus.is_empty() {
        return Err("M6 restart requires a nonempty M5 corpus".into());
    }
    let base_observer = StdMapObserver::owned("smb_base_position", vec![0_u8; SMB_MAP_SIZE]);
    let detector_observer =
        StdMapObserver::owned("smb_generated_detector", vec![0_u8; DETECTOR_MAP_SIZE]);
    let mut feedback = EagerOrFeedback::new(
        MaxMapFeedback::new(&base_observer),
        MaxMapFeedback::new(&detector_observer),
    );
    let mut objective = ConstFeedback::new(false);
    let mut state = StdState::new(
        StdRand::with_seed(seed),
        InMemoryCorpus::<SmbInput>::new(),
        InMemoryCorpus::<SmbInput>::new(),
        &mut feedback,
        &mut objective,
    )?;
    let scheduler =
        WeightedScheduler::<_, TriageScore, SmbObserver>::new(&mut state, &base_observer);
    let mut fuzzer = StdFuzzer::new(scheduler, feedback, objective);
    let mut manager = NopEventManager::new();
    let mut executor = SmbExecutor::new(
        base_observer,
        detector_observer,
        rom,
        detector,
        artifacts.detector_retire_after,
    )?;

    let mut milestones = SmbMilestones::default();
    let mut first_reached = SmbMilestoneTimes::default();
    let mut first_inputs = SmbMilestoneInputs::default();
    for entry in initial_corpus {
        let id = fuzzer.add_input(&mut state, &mut executor, &mut manager, entry.input.clone())?;
        state
            .corpus()
            .get(id)?
            .borrow_mut()
            .add_metadata(entry.labels.clone());
        update_campaign_milestones(
            &mut milestones,
            &mut first_reached,
            &mut first_inputs,
            executor.last_milestones(),
            0,
            &entry.input,
        );
    }
    *state.executions_mut() = 0;

    let macro_stats = Rc::new(RefCell::new(MutatorStats::default()));
    let generated_macro = GeneratedSmbMacroAdapter::new(
        macro_generator,
        artifacts.enable_macro,
        artifacts.macro_name.to_owned(),
        artifacts.macro_retire_after,
        Rc::clone(&macro_stats),
    );
    let mutator = SingleChoiceScheduledMutator::new(tuple_list!(
        AppendButtonChordMutator::default(),
        PerturbButtonChordMutator::default(),
        TruncateButtonChordMutator::default(),
        SpliceButtonChordMutator::default(),
        generated_macro,
    ));
    let mut stages = tuple_list!(StdMutationalStage::with_max_iterations(
        mutator,
        NonZeroUsize::MIN,
    ));
    while *state.executions() < execution_budget {
        fuzzer.fuzz_one(&mut stages, &mut executor, &mut state, &mut manager)?;
        update_campaign_milestones(
            &mut milestones,
            &mut first_reached,
            &mut first_inputs,
            executor.last_milestones(),
            *state.executions(),
            executor.last_input(),
        );
    }

    let mut corpus = Vec::with_capacity(state.corpus().count());
    for id in state.corpus().ids() {
        corpus.push(state.corpus().cloned_input_for_id(id)?);
    }
    let campaign = SmbCampaignReport {
        seed,
        executions: *state.executions(),
        milestones,
        first_reached,
        first_inputs,
        corpus,
    };
    Ok(SmbConfiguredReport {
        campaign,
        detector_name: artifacts.detector_name.to_owned(),
        detector: executor.detector_stats(),
        macro_name: artifacts.macro_name.to_owned(),
        macro_stats: macro_stats.borrow().clone(),
    })
}

/// Run the M5 pure random-mash control without a retained corpus.
pub fn run_smb_random_mash(
    rom: &[u8],
    seed: u64,
    execution_budget: u64,
) -> Result<SmbCampaignReport, Box<dyn Error>> {
    let mut rand = StdRand::with_seed(seed);
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    let mut milestones = SmbMilestones::default();
    let mut first_reached = SmbMilestoneTimes::default();
    let mut first_inputs = SmbMilestoneInputs::default();
    let mut input = SmbInput::default();
    for execution in 1..=execution_budget {
        match rand.below(NonZeroUsize::new(4).expect("nonempty mutation set")) {
            0 | 1 if input.actions.len() < MAX_SMB_ACTIONS => {
                input.actions.push(sample_chord(&mut rand));
            }
            2 if !input.actions.is_empty() => {
                let index = rand
                    .below(NonZeroUsize::new(input.actions.len()).expect("checked nonempty input"));
                input.actions[index] = sample_chord(&mut rand);
            }
            3 if !input.actions.is_empty() => {
                let new_len = rand
                    .below(NonZeroUsize::new(input.actions.len()).expect("checked nonempty input"));
                input.actions.truncate(new_len);
            }
            _ => {}
        }
        target.reset();
        let mut run = smb_milestones_from_wram(target.wram());
        for action in &input.actions {
            target.apply(action);
            run.merge(smb_milestones_from_wram(target.wram()));
            if target.exit_kind() != ExitKind::Ok {
                break;
            }
        }
        update_campaign_milestones(
            &mut milestones,
            &mut first_reached,
            &mut first_inputs,
            run,
            execution,
            &input,
        );
    }
    Ok(SmbCampaignReport {
        seed,
        executions: execution_budget,
        milestones,
        first_reached,
        first_inputs,
        corpus: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::{AppendButtonChordMutator, ButtonChord, MAX_SMB_ACTIONS, SmbInput, SmbTarget};
    use crate::target::{Target, execute_actions};
    use libafl::{
        corpus::{Corpus, InMemoryCorpus, Testcase},
        feedbacks::ConstFeedback,
        mutators::{MutationResult, Mutator},
        state::StdState,
    };
    use libafl_bolts::rands::StdRand;

    fn synthetic_nrom() -> Vec<u8> {
        let mut rom = vec![0_u8; 16 + (16 * 1024) + (8 * 1024)];
        rom[..16].copy_from_slice(&[b'N', b'E', b'S', 0x1a, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let prg = &mut rom[16..16 + (16 * 1024)];
        prg.fill(0xea);
        prg[..3].copy_from_slice(&[0x4c, 0x00, 0x80]);
        for vector in [0x3ffa, 0x3ffc, 0x3ffe] {
            prg[vector..vector + 2].copy_from_slice(&0x8000_u16.to_le_bytes());
        }
        rom
    }

    #[test]
    fn same_input_has_identical_ram_trace() {
        let rom = synthetic_nrom();
        let actions = [
            ButtonChord::new(0x80, 12),
            ButtonChord::new(0x81, 60),
            ButtonChord::new(0, 4),
        ];
        let mut first = SmbTarget::from_rom_bytes(&rom).expect("load synthetic ROM");
        let mut second = SmbTarget::from_rom_bytes(&rom).expect("load synthetic ROM");
        assert_eq!(
            execute_actions(&mut first, &actions),
            execute_actions(&mut second, &actions)
        );
    }

    #[test]
    fn filmed_frames_end_at_the_exact_campaign_state() {
        let rom = synthetic_nrom();
        let actions = [
            ButtonChord::new(0x80, 12),
            ButtonChord::new(0x81, 60),
            ButtonChord::new(0, 4),
        ];
        let mut campaign = SmbTarget::from_rom_bytes(&rom).expect("load synthetic ROM");
        let mut filmed = SmbTarget::from_rom_bytes(&rom).expect("load synthetic ROM");
        for action in &actions {
            campaign.apply(action);
            for _ in 0..action.bounded_hold_frames() {
                filmed
                    .clock_frame_for_film(action.buttons)
                    .expect("clock filmed frame");
            }
            filmed.release_buttons_for_film();
        }
        assert_eq!(campaign.wram(), filmed.wram());
        assert_eq!(campaign.frame_rgba(), filmed.frame_rgba());
    }

    #[test]
    fn snapshot_restore_matches_uncached_suffix() {
        let rom = synthetic_nrom();
        let prefix = ButtonChord::new(0x08, 18);
        let suffix = ButtonChord::new(0x81, 37);
        let mut target = SmbTarget::from_rom_bytes(&rom).expect("load synthetic ROM");
        target.apply(&prefix);
        let snapshot = target.snapshot().expect("save prefix");
        target.apply(&suffix);
        let cached = target.observe();

        target.reset();
        target.apply(&prefix);
        target.apply(&suffix);
        let uncached = target.observe();
        assert_eq!(cached, uncached);

        target.restore(&snapshot).expect("restore prefix");
        target.apply(&suffix);
        assert_eq!(target.observe(), uncached);
    }

    #[test]
    fn malformed_hold_duration_is_total_and_bounded() {
        let rom = synthetic_nrom();
        let mut target = SmbTarget::from_rom_bytes(&rom).expect("load synthetic ROM");
        target.apply(&ButtonChord {
            buttons: u8::MAX,
            hold_frames: 0,
        });
        assert_eq!(target.observe().frame_count, 1);
    }

    #[test]
    fn generic_append_mutator_preserves_the_action_bound() {
        let mut corpus = InMemoryCorpus::new();
        corpus
            .add(Testcase::new(SmbInput::default()))
            .expect("add seed");
        let mut feedback = ConstFeedback::new(false);
        let mut objective = ConstFeedback::new(false);
        let mut state = StdState::new(
            StdRand::with_seed(7),
            corpus,
            InMemoryCorpus::new(),
            &mut feedback,
            &mut objective,
        )
        .expect("mutation state");
        let mut input = SmbInput {
            actions: vec![ButtonChord::new(0, 1); MAX_SMB_ACTIONS - 1],
        };
        assert_eq!(
            AppendButtonChordMutator::default()
                .mutate(&mut state, &mut input)
                .expect("append"),
            MutationResult::Mutated
        );
        assert_eq!(input.actions.len(), MAX_SMB_ACTIONS);
        assert_eq!(
            AppendButtonChordMutator::default()
                .mutate(&mut state, &mut input)
                .expect("bounded append"),
            MutationResult::Skipped
        );
    }
}
