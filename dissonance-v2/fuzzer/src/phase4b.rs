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
    Error as LibAflError, HasMetadata, HasScheduler, StdFuzzer,
    corpus::{Corpus, CorpusId, InMemoryCorpus, Testcase},
    events::NopEventManager,
    executors::{Executor, ExitKind, HasObservers},
    feedbacks::{ConstFeedback, EagerOrFeedback, MaxMapFeedback},
    fuzzer::{Evaluator, Fuzzer},
    inputs::Input,
    mutators::{MutationResult, Mutator, SingleChoiceScheduledMutator},
    observers::StdMapObserver,
    schedulers::{QueueScheduler, RemovableScheduler, TestcaseScore, WeightedScheduler},
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
    phase2::TriageLabels,
    phase4a::{MutatorStats, ProducerMetadata},
    target::Target,
};

/// Size of the NES CPU work RAM exposed to an operator.
pub const WRAM_SIZE: usize = 2 * 1024;
/// Longest controller hold accepted from an input.
pub const MAX_HOLD_FRAMES: u8 = 120;
/// Longest action list retained by the SMB mutator stack.
pub const MAX_SMB_ACTIONS: usize = 96;
/// Fixed execution interval between synchronous SMB label refreshes.
pub const SMB_TRIAGE_BATCH_EXECUTIONS: u64 = 500;
/// Maximum live SMB labels accepted during one campaign.
pub const SMB_MAX_TRIAGE_CALLS: usize = 200;

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
const PLAYER_ENGINE_STATE_OFFSET: usize = 0x000e;
const PLAYER_KILLED_STATE: u8 = 0x0b;
const WORLD_NUMBER_OFFSET: usize = 0x075f;
const LEVEL_NUMBER_OFFSET: usize = 0x075c;
const FLAG_TASK_OFFSET: usize = 0x0746;
const LEVEL_ADVANCED_FLAG_TASK: u8 = 0x05;
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
    /// Produce one deterministic candidate from an immutable input and host-supplied seed.
    fn mutate(&self, input: &SmbInput, seed: u64) -> SmbInput;
}

/// Macro that contributes no candidate.
#[derive(Clone, Copy, Debug, Default)]
pub struct NullSmbMacro;

impl SmbMacro for NullSmbMacro {
    fn mutate(&self, input: &SmbInput, _seed: u64) -> SmbInput {
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
    S: HasCorpus<SmbInput> + HasRand,
{
    fn mutate(
        &mut self,
        state: &mut S,
        input: &mut SmbInput,
    ) -> Result<MutationResult, LibAflError> {
        self.emitted = false;
        if !self.enabled || !self.stats.borrow().active {
            return Ok(MutationResult::Skipped);
        }
        let seed = state.rand_mut().next();
        let candidate = self.generated.mutate(input, seed);
        if candidate.actions.len() > MAX_SMB_ACTIONS
            || candidate
                .actions
                .iter()
                .any(|chord| chord.hold_frames == 0 || chord.hold_frames > MAX_HOLD_FRAMES)
        {
            return Err(LibAflError::illegal_state(
                "generated SMB macro emitted an out-of-bounds action list",
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SmbLastProducer {
    Base,
    Generated,
}

#[derive(Debug)]
struct SmbCampaignMutator<M> {
    name: Cow<'static, str>,
    generated_enabled: bool,
    append: AppendButtonChordMutator,
    perturb: PerturbButtonChordMutator,
    truncate: TruncateButtonChordMutator,
    splice: SpliceButtonChordMutator,
    generated: GeneratedSmbMacroAdapter<M>,
    last_producer: Option<SmbLastProducer>,
}

impl<M> SmbCampaignMutator<M> {
    fn new(generated_enabled: bool, generated: GeneratedSmbMacroAdapter<M>) -> Self {
        Self {
            name: Cow::Borrowed("SmbCampaignMutator"),
            generated_enabled,
            append: AppendButtonChordMutator::default(),
            perturb: PerturbButtonChordMutator::default(),
            truncate: TruncateButtonChordMutator::default(),
            splice: SpliceButtonChordMutator::default(),
            generated,
            last_producer: None,
        }
    }
}

impl<M> Named for SmbCampaignMutator<M> {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<M, S> Mutator<SmbInput, S> for SmbCampaignMutator<M>
where
    M: SmbMacro,
    S: HasCorpus<SmbInput> + HasRand,
{
    fn mutate(
        &mut self,
        state: &mut S,
        input: &mut SmbInput,
    ) -> Result<MutationResult, LibAflError> {
        self.last_producer = None;
        let choices = if self.generated_enabled { 5 } else { 4 };
        let choice = state
            .rand_mut()
            .below(NonZeroUsize::new(choices).expect("constant is nonzero"));
        let result = match choice {
            0 => self.append.mutate(state, input)?,
            1 => self.perturb.mutate(state, input)?,
            2 => self.truncate.mutate(state, input)?,
            3 => self.splice.mutate(state, input)?,
            4 => self.generated.mutate(state, input)?,
            _ => {
                return Err(LibAflError::illegal_state(
                    "SMB mutator choice is out of range",
                ));
            }
        };
        if result == MutationResult::Mutated {
            self.last_producer = Some(if choice == 4 {
                SmbLastProducer::Generated
            } else {
                SmbLastProducer::Base
            });
        }
        Ok(result)
    }

    fn post_exec(
        &mut self,
        state: &mut S,
        new_corpus_id: Option<CorpusId>,
    ) -> Result<(), LibAflError> {
        if self.last_producer == Some(SmbLastProducer::Generated) {
            self.generated.post_exec(state, new_corpus_id)?;
        }
        self.last_producer = None;
        Ok(())
    }
}

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
    } else if rand.below(NonZeroUsize::new(4).expect("positive short-hold ratio")) != 0 {
        rand.below(NonZeroUsize::new(11).expect("positive short-hold range")) + 2
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

/// Mechanical evidence captured at one NES observer event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbObservations {
    /// Number of frames actually emulated since genesis.
    pub frame_count: u64,
    /// Complete 2 KiB NES CPU work RAM, with no semantic decoding.
    pub wram: Vec<u8>,
    /// Sorted work-RAM indices whose bytes changed since the previous observer event.
    pub changed_indices: Vec<u16>,
    /// Whether this event is the first observed player-death frame.
    #[serde(default)]
    pub dead: bool,
    /// Compact mechanical log line; it deliberately contains no decoded game fields.
    pub log_line: String,
}

/// Stable JSON request passed to the model triager for one retained SMB testcase.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbTriageRequest {
    /// Monotonic corpus index assigned by the host view.
    pub testcase_id: u64,
    /// Deterministic target-execution count at which these labels become visible.
    pub execution_count: u64,
    /// Complete retained input supplied to the scheduler triager.
    pub input: SmbInput,
    /// Complete mechanical RAM evidence at bucket transitions, death, and action endpoints.
    pub observations: Vec<SmbObservations>,
}

/// Complete in-memory state needed to resume an NES prefix exactly.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbSnapshot {
    emulator_state: Vec<u8>,
    observation: SmbObservations,
    dead: bool,
    failed: bool,
}

/// Deterministic TetaNES-backed target used by the Super Mario Bros campaigns.
#[derive(Debug)]
pub struct SmbTarget {
    deck: ControlDeck,
    genesis_state: Vec<u8>,
    observation: SmbObservations,
    action_observations: Vec<SmbObservations>,
    dead: bool,
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
        let observation = observation_from(&deck, 0, &[0; WRAM_SIZE], false);
        Ok(Self {
            deck,
            genesis_state,
            action_observations: vec![observation.clone()],
            observation,
            dead: false,
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
        target.observation = observation_from(&target.deck, 0, target.deck.wram(), false);
        target.observation.changed_indices.clear();
        target.observation.log_line = "frame=0 changed=[]".to_owned();
        target.action_observations = vec![target.observation.clone()];
        target.dead = false;
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
        if self.dead {
            return Ok(());
        }
        self.deck.joypad_mut(Player::One).buttons =
            JoypadBtnState::from_bits_truncate(u16::from(buttons));
        let result = self.deck.clock_frame();
        if result.is_err() {
            self.failed = true;
        } else {
            self.dead = smb_player_is_dead(self.deck.wram());
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

    /// Return whether execution reached the first player-death frame.
    #[must_use]
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// Return every observer event emitted by the most recently applied action.
    #[must_use]
    pub fn last_action_observations(&self) -> &[SmbObservations] {
        &self.action_observations
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
        if target.is_dead() {
            break;
        }
        target.apply(action);
        observations.extend_from_slice(target.last_action_observations());
        if target.is_dead() || target.exit_kind() != ExitKind::Ok {
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
        self.dead = false;
        self.observation = observation_from(&self.deck, 0, &[0; WRAM_SIZE], false);
        self.action_observations = vec![self.observation.clone()];
    }

    fn apply(&mut self, action: &Self::Action) {
        self.action_observations.clear();
        if self.failed || self.dead {
            return;
        }
        let mut prior_observed_wram = self.deck.wram().to_owned();
        let mut prior_bucket = smb_scroll_bucket(self.deck.wram());
        self.deck.joypad_mut(Player::One).buttons =
            JoypadBtnState::from_bits_truncate(u16::from(action.buttons));
        let hold_frames = action.bounded_hold_frames();
        let mut executed_frames = 0_u64;
        for _ in 0..hold_frames {
            if self.deck.clock_frame().is_err() {
                self.failed = true;
                break;
            }
            executed_frames = executed_frames.saturating_add(1);
            let current_bucket = smb_scroll_bucket(self.deck.wram());
            self.dead = smb_player_is_dead(self.deck.wram());
            if current_bucket != prior_bucket || self.dead {
                let observation = observation_from(
                    &self.deck,
                    self.observation.frame_count.saturating_add(executed_frames),
                    &prior_observed_wram,
                    self.dead,
                );
                prior_observed_wram = self.deck.wram().to_owned();
                prior_bucket = current_bucket;
                self.action_observations.push(observation);
            }
            if self.dead {
                break;
            }
        }
        self.deck.joypad_mut(Player::One).buttons = JoypadBtnState::empty();
        let endpoint_frame = self.observation.frame_count.saturating_add(executed_frames);
        let endpoint_already_recorded = self
            .action_observations
            .last()
            .is_some_and(|observation| observation.frame_count == endpoint_frame);
        if !endpoint_already_recorded {
            self.action_observations.push(observation_from(
                &self.deck,
                endpoint_frame,
                &prior_observed_wram,
                self.dead,
            ));
        }
        if let Some(observation) = self.action_observations.last() {
            self.observation = observation.clone();
        }
    }

    fn observe(&self) -> Self::Observations {
        self.observation.clone()
    }

    fn fingerprint(&self) -> u64 {
        smb_fingerprint_from_wram(self.deck.wram())
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
            dead: self.dead,
            failed: self.failed,
        })
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), LibAflError> {
        self.deck
            .load_state(Cursor::new(&snapshot.emulator_state))
            .map_err(|error| LibAflError::illegal_state(error.to_string()))?;
        self.observation = snapshot.observation.clone();
        self.action_observations = vec![self.observation.clone()];
        self.dead = snapshot.dead;
        self.failed = snapshot.failed;
        Ok(())
    }
}

fn observation_from(
    deck: &ControlDeck,
    frame_count: u64,
    prior_wram: &[u8; WRAM_SIZE],
    dead: bool,
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
        dead,
        log_line,
    }
}

fn smb_scroll_bucket(wram: &[u8; WRAM_SIZE]) -> u16 {
    u16::from(wram[SCREEN_PAGE_OFFSET]) * 16 + u16::from(wram[SCREEN_X_OFFSET] / 16)
}

fn smb_player_is_dead(wram: &[u8; WRAM_SIZE]) -> bool {
    wram[PLAYER_ENGINE_STATE_OFFSET] == PLAYER_KILLED_STATE
}

fn smb_fingerprint_from_wram(wram: &[u8; WRAM_SIZE]) -> u64 {
    let screen_page = u64::from(wram[SCREEN_PAGE_OFFSET]);
    let screen_x_bucket = u64::from(wram[SCREEN_X_OFFSET] / 16);
    let player_y_bucket = u64::from(wram[PLAYER_Y_OFFSET] / 32);
    (screen_page << 8) | (screen_x_bucket << 4) | player_y_bucket
}

/// Campaign milestone ladder, accumulated over every observer event in a run.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbMilestones {
    /// Greatest 16-pixel scroll bucket observed while the RAM level tuple is 1-1.
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
    /// Synchronous label writes, in visibility order, for no-model replay.
    #[serde(default)]
    pub label_events: Vec<SmbLabelEvent>,
}

/// One SMB scheduler label and the exact boundary where it became visible.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbLabelEvent {
    /// Monotonic LibAFL corpus identifier.
    pub testcase_id: u64,
    /// Deterministic target-execution count at which the labels became visible.
    pub execution_count: u64,
    /// Exact retained input used to detect replay divergence.
    pub input: SmbInput,
    /// Structured scheduler labels applied at that boundary.
    pub labels: TriageLabels,
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

/// SMB-specific weighted-scheduler score with a bounded Boost multiplier.
#[derive(Clone, Copy, Debug)]
pub struct SmbTriageScore;

impl<I, S> TestcaseScore<I, S> for SmbTriageScore {
    fn compute(_state: &S, entry: &mut Testcase<I>) -> Result<f64, LibAflError> {
        let Some(labels) = entry.metadata_map().get::<TriageLabels>() else {
            return Ok(1.0);
        };
        if labels.duplicate_of.is_some() {
            return Ok(0.01);
        }
        Ok(match labels.interest {
            crate::phase2::Interest::Boost => 4.0,
            crate::phase2::Interest::Neutral => 1.0,
            crate::phase2::Interest::Suppress => 0.01,
        })
    }
}

type SmbObserver = StdMapObserver<'static, u8, false>;
type SmbObservers = (SmbObserver, (SmbObserver, ()));
type SmbCampaignState =
    StdState<InMemoryCorpus<SmbInput>, SmbInput, StdRand, InMemoryCorpus<SmbInput>>;

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
            if self.target.is_dead() {
                break;
            }
            self.target.apply(action);
            for observation in self.target.last_action_observations() {
                let wram: &[u8; WRAM_SIZE] =
                    observation.wram.as_slice().try_into().map_err(|_| {
                        LibAflError::illegal_state("SMB observer emitted malformed work RAM")
                    })?;
                self.last_milestones.merge(smb_milestones_from_wram(wram));
                base_features.push(set_base_feature(
                    &mut self.observers.0,
                    smb_fingerprint_from_wram(wram),
                ));
                observations.push(observation.clone());
            }
            if self.target.is_dead() || self.target.exit_kind() != ExitKind::Ok {
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
    let level = smb_current_level(wram);
    let in_1_1 = world == 0 && level == 0;
    let scroll_bucket = if in_1_1 { smb_scroll_bucket(wram) } else { 0 };
    SmbMilestones {
        max_1_1_scroll_bucket: scroll_bucket,
        reached_1_1_flag: in_1_1 && wram[FLAG_TASK_OFFSET] != 0,
        reached_1_2: world == 0 && level == 1,
        reached_onward: world > 0 || (world == 0 && level > 1),
    }
}

/// Route-agnostic mechanical state available to completion search and evaluation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SmbMechanicalState {
    /// Zero-based world number decoded from work RAM.
    pub world: u8,
    /// Zero-based level number within the current world.
    pub level: u8,
    /// Current 16-pixel horizontal progress bucket.
    pub progress: u16,
    /// Coarse player vertical-position bucket.
    pub player_y_bucket: u8,
    /// Mechanical player engine state, without interpreting a route.
    pub player_engine_state: u8,
    /// Whether the target's first-death state is active.
    pub dead: bool,
    /// Whether the level-end flag task is active.
    pub flag_active: bool,
}

/// Decode the bounded mechanical state used by generic completion search.
#[must_use]
pub fn smb_mechanical_state_from_wram(wram: &[u8; WRAM_SIZE]) -> SmbMechanicalState {
    SmbMechanicalState {
        world: wram[WORLD_NUMBER_OFFSET],
        level: smb_current_level(wram),
        progress: smb_scroll_bucket(wram),
        player_y_bucket: wram[PLAYER_Y_OFFSET] / 16,
        player_engine_state: wram[PLAYER_ENGINE_STATE_OFFSET],
        dead: smb_player_is_dead(wram),
        flag_active: wram[FLAG_TASK_OFFSET] != 0,
    }
}

fn smb_current_level(wram: &[u8; WRAM_SIZE]) -> u8 {
    let level = wram[LEVEL_NUMBER_OFFSET];
    if wram[FLAG_TASK_OFFSET] == LEVEL_ADVANCED_FLAG_TASK {
        level.saturating_sub(1)
    } else {
        level
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
    let mutator = SmbCampaignMutator::new(artifacts.enable_macro, generated_macro);
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
        label_events: Vec::new(),
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
    let mut unused_triage = |_request: &SmbTriageRequest| {
        Err::<TriageLabels, Box<dyn Error>>("SMB triage is disabled".into())
    };
    run_smb_restart_configured_inner(
        rom,
        initial_corpus,
        seed,
        execution_budget,
        detector,
        macro_generator,
        artifacts,
        0,
        SMB_TRIAGE_BATCH_EXECUTIONS,
        &mut unused_triage,
    )
}

/// Run an SMB restart with synchronous labels applied every fixed 500 executions.
#[allow(clippy::too_many_arguments)] // existing configured runner plus the synchronous label callback
pub fn run_smb_restart_configured_with_triage<D, M, F>(
    rom: &[u8],
    initial_corpus: &[SmbLabeledCorpusEntry],
    seed: u64,
    execution_budget: u64,
    detector: D,
    macro_generator: M,
    artifacts: SmbArtifactConfig<'_>,
    mut triage: F,
) -> Result<SmbConfiguredReport, Box<dyn Error>>
where
    D: SmbDetector,
    M: SmbMacro,
    F: FnMut(&SmbTriageRequest) -> Result<TriageLabels, Box<dyn Error>>,
{
    run_smb_restart_configured_inner(
        rom,
        initial_corpus,
        seed,
        execution_budget,
        detector,
        macro_generator,
        artifacts,
        SMB_MAX_TRIAGE_CALLS,
        SMB_TRIAGE_BATCH_EXECUTIONS,
        &mut triage,
    )
}

/// Replay synchronous SMB labels at their recorded testcase and execution counts.
#[allow(clippy::too_many_arguments)] // mirrors the configured runner and adds recorded events
pub fn replay_smb_restart_configured<D, M>(
    rom: &[u8],
    initial_corpus: &[SmbLabeledCorpusEntry],
    seed: u64,
    execution_budget: u64,
    detector: D,
    macro_generator: M,
    artifacts: SmbArtifactConfig<'_>,
    events: &[SmbLabelEvent],
) -> Result<SmbConfiguredReport, Box<dyn Error>>
where
    D: SmbDetector,
    M: SmbMacro,
{
    if events.len() > SMB_MAX_TRIAGE_CALLS {
        return Err("recorded SMB labels exceed the 200-call budget".into());
    }
    let mut next_event = 0_usize;
    let mut replay = |request: &SmbTriageRequest| {
        let event = events
            .get(next_event)
            .ok_or("replay requested an unrecorded SMB label")?;
        if event.testcase_id != request.testcase_id
            || event.execution_count != request.execution_count
            || event.input != request.input
        {
            return Err("recorded SMB label request became visible at a different boundary".into());
        }
        next_event = next_event.saturating_add(1);
        Ok(event.labels.clone())
    };
    let report = run_smb_restart_configured_inner(
        rom,
        initial_corpus,
        seed,
        execution_budget,
        detector,
        macro_generator,
        artifacts,
        events.len(),
        SMB_TRIAGE_BATCH_EXECUTIONS,
        &mut replay,
    )?;
    if next_event != events.len() {
        return Err("replay did not consume every recorded SMB label".into());
    }
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn run_smb_restart_configured_inner<D, M, F>(
    rom: &[u8],
    initial_corpus: &[SmbLabeledCorpusEntry],
    seed: u64,
    execution_budget: u64,
    detector: D,
    macro_generator: M,
    artifacts: SmbArtifactConfig<'_>,
    max_triage_calls: usize,
    triage_batch_executions: u64,
    triage: &mut F,
) -> Result<SmbConfiguredReport, Box<dyn Error>>
where
    D: SmbDetector,
    M: SmbMacro,
    F: FnMut(&SmbTriageRequest) -> Result<TriageLabels, Box<dyn Error>>,
{
    if initial_corpus.is_empty() {
        return Err("M6 restart requires a nonempty M5 corpus".into());
    }
    if triage_batch_executions == 0 {
        return Err("SMB triage batch size must be positive".into());
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
        WeightedScheduler::<_, SmbTriageScore, SmbObserver>::new(&mut state, &base_observer);
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
    let mut labeled_ids = BTreeMap::new();
    for entry in initial_corpus {
        let id = fuzzer.add_input(&mut state, &mut executor, &mut manager, entry.input.clone())?;
        let mut updated = state.corpus().get(id)?.borrow().clone();
        updated.add_metadata(entry.labels.clone());
        let previous = state.corpus_mut().replace(id, updated)?;
        <_ as HasScheduler<SmbInput, SmbCampaignState>>::scheduler_mut(&mut fuzzer)
            .on_replace(&mut state, id, &previous)?;
        labeled_ids.insert(id, ());
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
    let mutator = SmbCampaignMutator::new(artifacts.enable_macro, generated_macro);
    let mut stages = tuple_list!(StdMutationalStage::with_max_iterations(
        mutator,
        NonZeroUsize::MIN,
    ));
    let mut label_events = Vec::new();
    while *state.executions() < execution_budget {
        let batch_end = state
            .executions()
            .saturating_add(triage_batch_executions)
            .min(execution_budget);
        while *state.executions() < batch_end {
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
        if *state.executions() == execution_budget || label_events.len() >= max_triage_calls {
            continue;
        }
        let new_ids = state
            .corpus()
            .ids()
            .filter(|id| !labeled_ids.contains_key(id))
            .collect::<Vec<_>>();
        for id in new_ids {
            if label_events.len() >= max_triage_calls {
                break;
            }
            let input = state.corpus().cloned_input_for_id(id)?;
            let request = SmbTriageRequest {
                testcase_id: u64::try_from(usize::from(id))?,
                execution_count: *state.executions(),
                observations: observe_smb_input(rom, &input)?,
                input,
            };
            let labels = triage(&request)?;
            let mut updated = state.corpus().get(id)?.borrow().clone();
            updated.add_metadata(labels.clone());
            let previous = state.corpus_mut().replace(id, updated)?;
            <_ as HasScheduler<SmbInput, SmbCampaignState>>::scheduler_mut(&mut fuzzer)
                .on_replace(&mut state, id, &previous)?;
            labeled_ids.insert(id, ());
            label_events.push(SmbLabelEvent {
                testcase_id: request.testcase_id,
                execution_count: request.execution_count,
                input: request.input,
                labels,
            });
        }
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
        label_events,
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
            if target.is_dead() || target.exit_kind() != ExitKind::Ok {
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
    use super::{
        AppendButtonChordMutator, ButtonChord, MAX_SMB_ACTIONS, NullSmbDetector, NullSmbMacro,
        SCREEN_PAGE_OFFSET, SCREEN_X_OFFSET, SMB_TRIAGE_BATCH_EXECUTIONS, SmbArtifactConfig,
        SmbDetector, SmbInput, SmbLabeledCorpusEntry, SmbMacro, SmbObservations, SmbTarget,
        SmbTriageScore, WRAM_SIZE, observe_smb_input, run_smb_configured,
        run_smb_restart_configured_inner, sample_chord, smb_mechanical_state_from_wram,
        smb_milestones_from_wram,
    };
    use crate::phase2::{Interest, TriageLabels};
    use crate::target::{Target, execute_actions};
    use libafl::{
        HasMetadata,
        corpus::{Corpus, InMemoryCorpus, Testcase},
        feedbacks::ConstFeedback,
        mutators::{MutationResult, Mutator},
        schedulers::TestcaseScore,
        state::StdState,
    };
    use libafl_bolts::rands::StdRand;

    fn synthetic_nrom() -> Vec<u8> {
        synthetic_nrom_with_program(&[0x4c, 0x00, 0x80])
    }

    fn synthetic_nrom_with_program(program: &[u8]) -> Vec<u8> {
        let mut rom = vec![0_u8; 16 + (16 * 1024) + (8 * 1024)];
        rom[..16].copy_from_slice(&[b'N', b'E', b'S', 0x1a, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let prg = &mut rom[16..16 + (16 * 1024)];
        prg.fill(0xea);
        prg[..program.len()].copy_from_slice(program);
        for vector in [0x3ffa, 0x3ffc, 0x3ffe] {
            prg[vector..vector + 2].copy_from_slice(&0x8000_u16.to_le_bytes());
        }
        rom
    }

    #[derive(Clone, Copy, Debug)]
    struct AlwaysAppendMacro;

    impl SmbMacro for AlwaysAppendMacro {
        fn mutate(&self, input: &SmbInput, _seed: u64) -> SmbInput {
            let mut candidate = input.clone();
            if candidate.actions.len() < MAX_SMB_ACTIONS {
                candidate.actions.push(ButtonChord::new(0x81, 2));
            }
            candidate
        }
    }

    #[derive(Clone, Copy, Debug)]
    struct SeedVariantMacro;

    impl SmbMacro for SeedVariantMacro {
        fn mutate(&self, input: &SmbInput, seed: u64) -> SmbInput {
            let mut candidate = input.clone();
            if candidate.actions.len() < MAX_SMB_ACTIONS {
                let variant = u8::try_from(seed % 11).unwrap_or(0);
                candidate
                    .actions
                    .push(ButtonChord::new(0x81, variant.saturating_add(2)));
            }
            candidate
        }
    }

    #[derive(Clone, Copy, Debug)]
    struct ObservationCountDetector;

    impl SmbDetector for ObservationCountDetector {
        fn features(&self, observations: &[SmbObservations]) -> Vec<u64> {
            vec![u64::try_from(observations.len()).unwrap_or(u64::MAX)]
        }
    }

    fn labels(interest: Interest) -> TriageLabels {
        TriageLabels {
            interest,
            duplicate_of: None,
            flags: Vec::new(),
            tags: Vec::new(),
            summary: "deterministic M9 fixture".to_owned(),
            hypotheses: Vec::new(),
        }
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
    fn level_decode_waits_for_the_flag_task_to_clear() {
        let mut wram = [0_u8; WRAM_SIZE];
        wram[super::LEVEL_NUMBER_OFFSET] = 2;
        wram[super::FLAG_TASK_OFFSET] = super::LEVEL_ADVANCED_FLAG_TASK;
        assert_eq!(smb_mechanical_state_from_wram(&wram).level, 1);
        let flag = smb_milestones_from_wram(&wram);
        assert!(flag.reached_1_2);
        assert!(!flag.reached_onward);

        wram[super::FLAG_TASK_OFFSET] = 0;
        assert_eq!(smb_mechanical_state_from_wram(&wram).level, 2);
        assert!(smb_milestones_from_wram(&wram).reached_onward);

        wram[super::LEVEL_NUMBER_OFFSET] = 0;
        wram[super::FLAG_TASK_OFFSET] = 2;
        assert_eq!(smb_mechanical_state_from_wram(&wram).level, 0);
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
    fn long_action_observes_intermediate_sixteen_pixel_buckets() {
        let rom = synthetic_nrom_with_program(&[
            0xee, 0x1c, 0x07, // INC $071c
            0xa0, 0x14, // LDY #20
            0xa2, 0xff, // outer: LDX #255
            0xca, // inner: DEX
            0xd0, 0xfd, // BNE inner
            0x88, // DEY
            0xd0, 0xf8, // BNE outer
            0x4c, 0x00, 0x80, // JMP $8000
        ]);
        let mut target = SmbTarget::from_rom_bytes(&rom).expect("load bucket ROM");
        target.apply(&ButtonChord::new(0x80, 120));

        let observations = target.last_action_observations();
        assert!(observations.len() > 2);
        assert_eq!(
            observations.last().map(|event| event.frame_count),
            Some(120)
        );
        assert!(
            observations
                .windows(2)
                .all(|events| events[0].frame_count < events[1].frame_count)
        );
        let buckets = observations
            .iter()
            .map(|event| {
                u16::from(event.wram[SCREEN_PAGE_OFFSET]) * 16
                    + u16::from(event.wram[SCREEN_X_OFFSET] / 16)
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert!(buckets.len() > 2);
    }

    #[test]
    fn death_is_terminal_and_replays_at_the_same_frame() {
        let rom = synthetic_nrom_with_program(&[
            0xa9, 0x0b, // LDA #$0b
            0x85, 0x0e, // STA $0e
            0x4c, 0x04, 0x80, // JMP $8004
        ]);
        let input = SmbInput {
            actions: vec![ButtonChord::new(0x80, 120), ButtonChord::new(0x81, 120)],
        };
        let first = observe_smb_input(&rom, &input).expect("first terminal replay");
        let second = observe_smb_input(&rom, &input).expect("second terminal replay");

        assert_eq!(first, second);
        assert_eq!(first.len(), 2);
        assert_eq!(first.last().map(|event| event.frame_count), Some(1));
        assert!(first.last().is_some_and(|event| event.dead));
    }

    #[test]
    fn sampled_non_start_holds_are_biased_short() {
        let mut rand = StdRand::with_seed(0x5eed_da10);
        let mut short = 0_usize;
        let mut non_start = 0_usize;
        for _ in 0..1_024 {
            let chord = sample_chord(&mut rand);
            if chord.buttons != 0x08 {
                non_start += 1;
                if (2..=12).contains(&chord.hold_frames) {
                    short += 1;
                }
            }
        }
        assert!(short * 4 > non_start * 3);
    }

    #[test]
    fn generated_macro_seed_variants_are_replayable() {
        let input = SmbInput::default();
        let first = SeedVariantMacro.mutate(&input, 3);
        let replay = SeedVariantMacro.mutate(&input, 3);
        let variant = SeedVariantMacro.mutate(&input, 9);
        assert_eq!(first, replay);
        assert_ne!(first, variant);
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

    #[test]
    fn configured_path_forwards_generated_mutator_post_exec() {
        let report = run_smb_configured(
            &synthetic_nrom(),
            7,
            64,
            NullSmbDetector,
            AlwaysAppendMacro,
            SmbArtifactConfig {
                detector_name: "none",
                detector_retire_after: 1,
                macro_name: "always_append",
                macro_retire_after: 1,
                enable_macro: true,
            },
        )
        .expect("run configured production path");

        assert_eq!(report.macro_stats.offspring, 1);
        assert_eq!(report.macro_stats.novel_offspring, 0);
        assert_eq!(report.macro_stats.executions_without_novelty, 1);
        assert!(!report.macro_stats.active);
    }

    #[test]
    fn smb_boost_score_is_capped_at_four() {
        assert_eq!(SMB_TRIAGE_BATCH_EXECUTIONS, 500);
        let mut testcase = Testcase::new(SmbInput::default());
        testcase.add_metadata(labels(Interest::Boost));
        assert_eq!(
            <SmbTriageScore as TestcaseScore<SmbInput, ()>>::compute(&(), &mut testcase)
                .expect("compute SMB score"),
            4.0
        );
    }

    #[test]
    fn batched_labels_are_visible_at_the_recorded_count_and_replay() {
        let rom = synthetic_nrom();
        let initial_corpus = vec![SmbLabeledCorpusEntry {
            input: SmbInput::default(),
            labels: labels(Interest::Neutral),
        }];
        let artifacts = SmbArtifactConfig {
            detector_name: "observation_count",
            detector_retire_after: u64::MAX,
            macro_name: "none",
            macro_retire_after: u64::MAX,
            enable_macro: false,
        };
        let mut live_triage = |request: &super::SmbTriageRequest| {
            let encoded = serde_json::to_value(request).expect("serialize SMB triage request");
            assert!(encoded.get("input").is_some());
            assert!(encoded.get("observations").is_some());
            assert!(encoded.get("log").is_none());
            Ok(labels(Interest::Boost))
        };
        let live = run_smb_restart_configured_inner(
            &rom,
            &initial_corpus,
            9,
            24,
            ObservationCountDetector,
            NullSmbMacro,
            artifacts,
            200,
            8,
            &mut live_triage,
        )
        .expect("run batched labels");
        assert!(!live.label_events.is_empty());
        assert!(
            live.label_events
                .iter()
                .all(|event| matches!(event.execution_count, 8 | 16))
        );
        assert!(live.label_events.iter().all(|event| {
            usize::try_from(event.testcase_id)
                .ok()
                .and_then(|id| live.campaign.corpus.get(id))
                == Some(&event.input)
        }));
        let encoded = serde_json::to_value(&live.label_events[0]).expect("serialize label event");
        assert!(encoded.get("input").is_some());
        assert!(encoded.get("observations").is_none());

        let events = live.label_events.clone();
        let mut next_event = 0_usize;
        let mut replay_triage = |request: &super::SmbTriageRequest| {
            let event = events.get(next_event).ok_or_else(|| {
                Box::<dyn std::error::Error>::from("missing recorded fixture label")
            })?;
            if event.testcase_id != request.testcase_id
                || event.execution_count != request.execution_count
                || event.input != request.input
            {
                return Err("fixture label became visible at a different boundary".into());
            }
            next_event = next_event.saturating_add(1);
            Ok(event.labels.clone())
        };
        let replay = run_smb_restart_configured_inner(
            &rom,
            &initial_corpus,
            9,
            24,
            ObservationCountDetector,
            NullSmbMacro,
            artifacts,
            events.len(),
            8,
            &mut replay_triage,
        )
        .expect("replay batched labels");
        assert_eq!(next_event, events.len());
        assert_eq!(replay, live);
    }
}
