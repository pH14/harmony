// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod arcade;
#[cfg(test)]
mod fixture;
pub mod shadow;
pub mod target;

use std::{
    error::Error,
    fmt::Debug,
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use machine::nes::{ButtonChord, MAX_HOLD_FRAMES, SAVE_RAM_BASE, SAVE_RAM_SIZE};
#[cfg(unix)]
use machine::quicknes::QuickNesMachine;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

pub use arcade::Arcade;
pub use shadow::{Shadow, ShadowHook, ShadowToken, StateHook};
pub use target::{ActionObservation, NativeMemory, NesObservation, NesSnapshot, NesTarget};

use crate::{
    arcade::Machines,
    chord::{self, CHORD_DRAW_FIELD, CHORD_SITES, CHORD_TREE_IDENTIFIER, ChordVocabulary},
    nes_backend::{NesBackend, NesMemory, SnapshotState, WorkRamCopy},
    search::{
        archive::{
            ArchiveEntryReport, Input, KeyShape, ProgressPoint, RetentionPolicy,
            SelectorAccounting, entries_by_suffix,
        },
        assertion::{
            Accumulator, Assertion, AssertionKey, Catalog, Compiled, Declarations, RECORDS_FORMAT,
            Record, RecordWriter, RecordedAction, RecordsHeader, RecordsSize, RecordsWriter,
            StreamIdentity,
        },
        campaign::{
            ArchiveFormat, ArchiveFormatName, ArchiveReportState, CampaignActionResult,
            CampaignCandidate, CampaignJobResult, CampaignTypes, Evaluation, InputPolicy,
            Reporting, TargetExecution, WorkloadPolicies, postcard_semantic_result_sha256,
            postcard_value_sha256,
        },
        choice::{Choice, ChoicePolicy, ChoiceProfiles, ChoiceRecord, ChoiceSite, Chooser},
        rand::RomuDuoJrRand,
        rollout::{ExecutionDisposition, Outcome, RolloutStop},
    },
    target::{ExitKind, Target},
};

pub const CAMPAIGN_STREAM_FORMAT: &str = "nes-workload-campaign-stream-v2";
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "nes-workload-snapshot-checkpoint-v3";
pub const CONSONANCE_SNAPSHOT_CHECKPOINT_FORMAT: &str =
    "nes-workload-consonance-snapshot-checkpoint-v1";
pub const NAMED_PROGRESS_FORMAT: &str = "nes-named-progress-v1";
pub const ARCHIVE_FORMAT: &str = "nes-workload-archive-v1";
pub const MAX_SOMETIMES: usize = 128;
pub const RECORDS_FILE: &str = "records.bin";
pub const REPLAY_RECORDS_FILE: &str = "records-replay.bin";

const GAME_FIELD: &str = "game";
const CONTROLLER_VOCABULARY_FIELD: &str = "controller_vocabulary";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
pub const EMULATOR_BACKEND_FIELD: &str = "emulator_backend";
const DURATION_IDENTIFIER: &str = "stratified_short_or_long_v1";
const REPLACEMENT_IDENTIFIER: &str = "opaque_preference_then_fewest_frames";
const TERMINAL_IDENTIFIER: &str = "strategy_goal_or_reject_v1";
const RESULT_DIGEST: &str = "result_digest=semantic-postcard-1.1.3-sha256-hex-v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum Never {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Play<M> {
    Chord,
    Move(M),
}

pub const MAX_OFFER_OPTIONS: usize = 64;

pub const EVERY_OPTION: u64 = u64::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Offer<M: 'static> {
    pub site: u16,
    pub default: u16,
    pub options: &'static [(u16, Play<M>)],
    pub offered: u64,
}

impl<M: Copy> Offer<M> {
    fn offered_options(&self) -> impl Iterator<Item = &(u16, Play<M>)> {
        self.options
            .iter()
            .enumerate()
            .filter(|(index, (label, _))| {
                *label == self.default
                    || (*index < MAX_OFFER_OPTIONS && (self.offered >> index) & 1 == 1)
            })
            .map(|(_, option)| option)
    }

    #[must_use]
    pub fn labels(&self) -> Vec<u16> {
        self.offered_options().map(|(label, _)| *label).collect()
    }

    #[must_use]
    pub fn plays(&self, label: u16) -> Option<Play<M>> {
        self.offered_options()
            .find(|(offered, _)| *offered == label)
            .map(|(_, play)| *play)
    }

    #[must_use]
    pub fn cuts(&self) -> bool {
        !self
            .offered_options()
            .any(|(_, play)| matches!(play, Play::Chord))
    }

    fn option(&self, action: &ButtonChord) -> Option<(u16, Play<M>)> {
        let answer = move_answer(action);
        answer
            .filter(|(site, _)| *site == self.site)
            .and_then(|(_, label)| {
                self.plays(label)
                    .filter(|play| matches!(play, Play::Move(_)))
                    .map(|play| (label, play))
            })
            .or_else(|| {
                answer
                    .is_none()
                    .then(|| {
                        self.offered_options()
                            .find(|(_, play)| matches!(play, Play::Chord))
                            .copied()
                    })
                    .flatten()
            })
            .or_else(|| self.plays(self.default).map(|play| (self.default, play)))
            .or_else(|| self.offered_options().next().copied())
    }

    #[must_use]
    pub fn chosen(&self, action: &ButtonChord) -> Option<M> {
        match self.option(action) {
            Some((_, Play::Move(mv))) => Some(mv),
            Some((_, Play::Chord)) | None => None,
        }
    }

    pub fn played(&self, action: &ButtonChord) -> Result<ButtonChord, Box<dyn Error>> {
        match self.option(action) {
            Some((label, Play::Move(_))) => move_chord(self.site, label),
            Some((_, Play::Chord)) | None => Ok(*action),
        }
    }

    pub fn resolve(
        &self,
        chords: &ChordVocabulary,
        chooser: &mut Chooser,
        asked: &ButtonChord,
        previous: Option<&ButtonChord>,
        records: &mut Vec<ChoiceRecord>,
    ) -> Result<ButtonChord, Box<dyn Error>> {
        let site = CHORD_SITES + self.site;
        let labels = self.labels();
        let taken = chooser.choose(site, &labels, self.default);
        let record = ChoiceRecord::new(site, labels, self.default, taken);
        let label = record.resolved;
        records.push(record);
        if matches!(self.plays(label), Some(Play::Chord)) {
            chords.resolve(chooser, asked, previous, records)
        } else {
            move_chord(self.site, label)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Moves<M: 'static> {
    pub forced: Option<M>,
    pub offer: Option<Offer<M>>,
}

impl<M> Moves<M> {
    #[must_use]
    pub fn none() -> Self {
        Self {
            forced: None,
            offer: None,
        }
    }

    #[must_use]
    pub fn forced(mv: M) -> Self {
        Self {
            forced: Some(mv),
            offer: None,
        }
    }

    #[must_use]
    pub fn offer(offer: Offer<M>) -> Self {
        Self {
            forced: None,
            offer: Some(offer),
        }
    }

    #[must_use]
    pub fn ends_input(&self, last_frame: bool) -> bool
    where
        M: Copy,
    {
        self.forced.is_none() && (last_frame || self.offer.is_some_and(|offer| offer.cuts()))
    }

    #[must_use]
    pub fn ends_move(&self, last_step: bool, offered: bool) -> bool {
        last_step && !(offered && self.forced.is_some())
    }

    #[must_use]
    pub fn forced_after(&self, ends: bool) -> Option<M>
    where
        M: Copy,
    {
        self.forced.filter(|_| !ends)
    }
}

#[derive(Clone, Debug)]
pub struct Framed<S, M> {
    pub observation: NesObservation<S>,
    pub record: Record,
    pub ends: bool,
    pub forced: Option<M>,
}

impl<S, M> Framed<S, M> {
    #[must_use]
    pub fn cuts(&self) -> bool {
        self.ends || self.forced.is_some()
    }
}

const MOVE_HOLD: u8 = u8::MAX;

pub fn move_chord(site: u16, label: u16) -> Result<ButtonChord, Box<dyn Error>> {
    let site = u8::try_from(site)?;
    if site >= MOVE_HOLD - MAX_HOLD_FRAMES {
        return Err(format!("move site {site} is outside the move chord range").into());
    }
    Ok(ButtonChord {
        buttons: u8::try_from(label)?,
        hold_frames: MOVE_HOLD - site,
    })
}

#[must_use]
pub fn move_answer(chord: &ButtonChord) -> Option<(u16, u16)> {
    (chord.hold_frames > MAX_HOLD_FRAMES).then(|| {
        (
            u16::from(MOVE_HOLD - chord.hold_frames),
            u16::from(chord.buttons),
        )
    })
}

#[must_use]
pub fn reproducing_choices<G: Game>() -> ChoicePolicy {
    ChoicePolicy {
        profiles: ChoiceProfiles::Fixed(G::CHORDS.reproducing_profile()),
        untried_first: false,
    }
}

pub const FORCED_MOVE_BOUND: &str = "forced_move_within_bound";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Step<M> {
    pub chord: ButtonChord,
    pub rest: Option<M>,
}

pub trait Game: Clone + Debug + Send + Sync + 'static {
    type State: Clone + Debug + Eq + Send + Sync + Serialize + DeserializeOwned + 'static;
    type Move: Copy + Debug + 'static;

    const NAME: &'static str;
    const STRATEGY: &'static str;
    const CHORDS: ChordVocabulary;
    const MEMORY_WINDOWS: &'static [(u64, u32)] = &[];

    fn setup(&self) -> Vec<ButtonChord>;
    fn setup_pokes(&self) -> Vec<(usize, u8)> {
        Vec::new()
    }
    fn start(&self) -> Vec<ButtonChord> {
        Vec::new()
    }
    fn genesis(&self, state: &Self::State) -> Result<(), String>;
    fn rooted_genesis(&self, _state: &Self::State) -> Result<(), String> {
        Ok(())
    }
    fn genesis_id(&self) -> String {
        format!("{}-setup-v1", Self::NAME)
    }
    fn decode(
        &self,
        parent: Option<&Self::State>,
        previous: Option<&Self::State>,
        ram: &[u8],
    ) -> Self::State;
    fn declare(&self, state: &Self::State, out: &mut impl Declarations);
    fn moves(&self, state: &Self::State) -> Moves<Self::Move>;
    fn move_sites(&self) -> Vec<ChoiceSite> {
        Vec::new()
    }
    fn play(&self, mv: Self::Move, state: &Self::State) -> Option<Step<Self::Move>>;
    fn move_frames(&self, mv: Self::Move) -> u64;
    fn answer_frames(_site: u16, _label: u16) -> u64 {
        1
    }
}

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
pub struct Satisfied(pub u128);

impl Satisfied {
    #[must_use]
    pub fn holds(self, index: usize) -> bool {
        index < MAX_SOMETIMES && (self.0 >> index) & 1 == 1
    }
}

fn declared<G: Game>(game: &G, state: &G::State) -> Vec<Assertion> {
    let mut declared = Vec::new();
    game.declare(state, &mut declared);
    declared
}

struct Frame<'a> {
    record: RecordWriter<'a>,
    satisfied: u128,
    sometimes: usize,
}

impl Declarations for Frame<'_> {
    #[inline(always)]
    fn declare(&mut self, assertion: Assertion) {
        if assertion.is_sometimes() {
            if assertion.holds() && self.sometimes < MAX_SOMETIMES {
                self.satisfied |= 1 << self.sometimes;
            }
            self.sometimes += 1;
        }
        self.record.push(&assertion);
    }
}

#[derive(Debug)]
pub struct Lowered<G: Game> {
    pub game: G,
    pub catalog: Catalog,
    pub compiled: Compiled,
    strategy: String,
    sometimes: Vec<String>,
}

impl<G: Game> Lowered<G> {
    #[must_use]
    pub fn choice_sites(&self) -> Vec<ChoiceSite> {
        let mut sites = G::CHORDS.sites();
        sites.extend(self.game.move_sites());
        sites
    }

    pub fn records_catalog(&self) -> Result<Catalog, Box<dyn Error>> {
        self.catalog.clone().with_choices(self.choice_sites())
    }

    pub fn new(game: G, strategy: &str) -> Result<Self, Box<dyn Error>> {
        if let Some(site) = game
            .move_sites()
            .into_iter()
            .find(|site| site.labels.len() > MAX_OFFER_OPTIONS)
        {
            return Err(format!(
                "{} move site {} declares {} labels and an offer masks {MAX_OFFER_OPTIONS}",
                G::NAME,
                site.name,
                site.labels.len()
            )
            .into());
        }
        if let Some((address, length)) = G::MEMORY_WINDOWS.iter().find(|(address, length)| {
            *length == 0
                || *address < SAVE_RAM_BASE
                || address.saturating_add(u64::from(*length)) > SAVE_RAM_BASE + SAVE_RAM_SIZE
        }) {
            return Err(format!(
                "{} memory window {address:#06x}+{length} is not inside save RAM",
                G::NAME
            )
            .into());
        }
        let declared = declared(&game, &game.decode(None, None, &target::zero_frame::<G>()));
        let sometimes: Vec<String> = declared
            .iter()
            .filter(|assertion| assertion.is_sometimes())
            .map(|assertion| assertion.name().to_owned())
            .collect();
        if sometimes.len() > MAX_SOMETIMES {
            return Err(format!(
                "{} declares {} sometimes assertions and a NES workload tracks {MAX_SOMETIMES}",
                G::NAME,
                sometimes.len()
            )
            .into());
        }
        let catalog = Catalog::from_assertions(declared)?;
        let compiled = Compiled::parse(strategy, &catalog)?;
        if !compiled.has_goal() {
            return Err(format!("the {} strategy names no goal", G::NAME).into());
        }
        Ok(Self {
            game,
            catalog,
            compiled,
            strategy: strategy.to_owned(),
            sometimes,
        })
    }

    #[must_use]
    pub fn strategy(&self) -> &str {
        &self.strategy
    }

    #[must_use]
    pub fn sometimes(&self) -> &[String] {
        &self.sometimes
    }

    #[must_use]
    pub fn record(&self, state: &G::State) -> (Record, Satisfied) {
        let mut record = Record::default();
        let satisfied = self.write_record(state, &mut record);
        (record, satisfied)
    }

    #[must_use]
    pub fn genesis_record(&self, state: &G::State) -> Record {
        self.frame_record(state, true)
    }

    #[must_use]
    pub fn frame_record(&self, state: &G::State, action_end: bool) -> Record {
        let mut record = self.record(state).0;
        record.set_action_end(action_end);
        record
    }

    fn write_record(&self, state: &G::State, record: &mut Record) -> Satisfied {
        let mut frame = Frame {
            record: record.writer(),
            satisfied: 0,
            sometimes: 0,
        };
        self.game.declare(state, &mut frame);
        let satisfied = Satisfied(frame.satisfied);
        frame.record.finish();
        satisfied
    }

    pub fn checked_record(&self, state: &G::State) -> Result<Record, Box<dyn Error>> {
        self.catalog.checked_record(declared(&self.game, state))
    }

    #[must_use]
    pub fn observe(
        &self,
        frame_count: u64,
        state: G::State,
        accumulator: &mut Accumulator,
    ) -> NesObservation<G::State> {
        self.observe_ending(frame_count, state, accumulator, |_| true)
            .observation
    }

    #[must_use]
    pub fn observe_ending(
        &self,
        frame_count: u64,
        state: G::State,
        accumulator: &mut Accumulator,
        ends: impl FnOnce(&Moves<G::Move>) -> bool,
    ) -> Framed<G::State, G::Move> {
        let moves = self.game.moves(&state);
        let mut record = Record::default();
        let satisfied = self.write_record(&state, &mut record);
        let (observed, ends) = self
            .compiled
            .observe_ending(&mut record, accumulator, ends(&moves));
        Framed {
            observation: NesObservation {
                frame_count,
                state,
                key: observed.key,
                rejected: observed.rejected,
                goal: observed.goal,
                satisfied,
                overran: false,
            },
            record,
            ends,
            forced: moves.forced_after(ends),
        }
    }

    pub fn records_header(
        &self,
        stream: StreamIdentity,
        genesis: &G::State,
        choice_policy: Option<String>,
        root: Option<(String, Accumulator)>,
    ) -> Result<RecordsHeader, Box<dyn Error>> {
        let (root_sha256, accumulator_before_genesis) = root.unzip();
        Ok(RecordsHeader {
            format: RECORDS_FORMAT.to_owned(),
            stream,
            policy: self.compiled.policy_identifier().to_owned(),
            choice_policy,
            strategy: self.strategy.clone(),
            catalog: self.records_catalog()?,
            memory_windows: G::MEMORY_WINDOWS.to_vec(),
            genesis: self.genesis_record(genesis),
            root_sha256,
            accumulator_before_genesis: accumulator_before_genesis.unwrap_or_default(),
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NesRun;

fn emulator_identity(before_genesis: &str, genesis: &str, after_genesis: &str) -> String {
    format!("{before_genesis};genesis={genesis};{after_genesis}")
}

#[must_use]
pub fn root_sha256(root: &[ButtonChord]) -> String {
    let mut digest = Sha256::new();
    for chord in root {
        digest.update([chord.buttons, chord.hold_frames]);
    }
    format!("{:x}", digest.finalize())
}

pub struct NesWorkload<G: Game, M: NesBackend<P>, P: SnapshotState = Vec<u8>> {
    lowered: Arc<Lowered<G>>,
    rom_sha256: String,
    identity_before_genesis: String,
    identity_after_genesis: String,
    identity: String,
    checkpoint_format: &'static str,
    native_memory: Option<NativeMemory<M>>,
    root: Vec<ButtonChord>,
    machines: Machines<M>,
    records: Option<Arc<Records>>,
    choices: ChoicePolicy,
    milestone_input_dir: Option<PathBuf>,
    state: PhantomData<fn() -> P>,
}

struct Records {
    header: RecordsHeader,
    writer: Mutex<Option<RecordsWriter<ButtonChord>>>,
}

impl<G: Game, M: NesBackend<Vec<u8>> + NesMemory> NesWorkload<G, M> {
    pub fn with_machines(
        game: G,
        strategy: &str,
        rom: &[u8],
        core: &str,
        core_sha256: &str,
        machines: Machines<M>,
    ) -> Result<Self, Box<dyn Error>> {
        Self::assemble(
            game,
            strategy,
            rom,
            format!("{core};state=ppu-unused2-zero-v1"),
            format!("{RESULT_DIGEST};sha256={core_sha256}"),
            SNAPSHOT_CHECKPOINT_FORMAT,
            Some(NativeMemory::of()),
            machines,
        )
    }
}

impl<G: Game, M: NesBackend<P>, P: SnapshotState> NesWorkload<G, M, P> {
    #[allow(clippy::too_many_arguments)]
    fn assemble(
        game: G,
        strategy: &str,
        rom: &[u8],
        identity_before_genesis: String,
        identity_after_genesis: String,
        checkpoint_format: &'static str,
        native_memory: Option<NativeMemory<M>>,
        machines: Machines<M>,
    ) -> Result<Self, Box<dyn Error>> {
        let identity = emulator_identity(
            &identity_before_genesis,
            &game.genesis_id(),
            &identity_after_genesis,
        );
        Ok(Self {
            lowered: Arc::new(Lowered::new(game, strategy)?),
            rom_sha256: format!("{:x}", Sha256::digest(rom)),
            identity_before_genesis,
            identity_after_genesis,
            identity,
            checkpoint_format,
            native_memory,
            root: Vec::new(),
            machines,
            records: None,
            choices: reproducing_choices::<G>(),
            milestone_input_dir: None,
            state: PhantomData,
        })
    }

    fn merge_root(&self, evidence: &mut NesEvidence, target: &NesTarget<G, M, P>) -> Vec<String> {
        evidence.name(&self.lowered);
        let observation = target.observation();
        if let Some(key) = observation.key {
            evidence.watermark = evidence.watermark.max(key.progress);
        }
        evidence.aggregate.0 |= observation.satisfied.0;
        evidence.update_first_seen(observation.satisfied, 0, Some(&Input::default()))
    }

    pub fn with_strategy(&self, strategy: &str) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            lowered: Arc::new(Lowered::new(self.lowered.game.clone(), strategy)?),
            rom_sha256: self.rom_sha256.clone(),
            identity_before_genesis: self.identity_before_genesis.clone(),
            identity_after_genesis: self.identity_after_genesis.clone(),
            identity: self.identity.clone(),
            checkpoint_format: self.checkpoint_format,
            native_memory: self.native_memory,
            root: self.root.clone(),
            machines: Arc::clone(&self.machines),
            records: None,
            choices: self.choices,
            milestone_input_dir: self.milestone_input_dir.clone(),
            state: PhantomData,
        })
    }

    pub fn with_root(mut self, root: Vec<ButtonChord>) -> Result<Self, Box<dyn Error>> {
        if root.is_empty() {
            return Err(format!("the {} root carries no actions", G::NAME).into());
        }
        if self.records.is_some() {
            return Err(format!(
                "the {} records were opened for the unrooted genesis, so root the workload before opening its records",
                G::NAME
            )
            .into());
        }
        let genesis = format!(
            "{}:root-sha256={}",
            self.lowered.game.genesis_id(),
            root_sha256(&root)
        );
        self.identity = emulator_identity(
            &self.identity_before_genesis,
            &genesis,
            &self.identity_after_genesis,
        );
        self.root = root;
        Ok(self)
    }

    #[must_use]
    pub fn root(&self) -> &[ButtonChord] {
        &self.root
    }

    #[must_use]
    pub fn with_milestone_input_dir(mut self, directory: PathBuf) -> Self {
        self.milestone_input_dir = Some(directory);
        self
    }

    pub fn with_choice_policy(mut self, choices: ChoicePolicy) -> Result<Self, Box<dyn Error>> {
        self.choices = choices.validate()?;
        Ok(self)
    }

    pub fn with_records(mut self, path: &Path, campaign_seed: u64) -> Result<Self, Box<dyn Error>> {
        let target = self.new_target()?;
        let header = self.lowered.records_header(
            StreamIdentity {
                campaign_seed,
                workload_identity_sha256: self.rom_sha256.clone(),
            },
            &target.observation().state,
            Some(self.choices.identifier()),
            (!self.root.is_empty()).then(|| {
                (
                    root_sha256(&self.root),
                    target.accumulator_before_genesis().clone(),
                )
            }),
        )?;
        let writer = RecordsWriter::create(path, &header)?;
        self.records = Some(Arc::new(Records {
            header,
            writer: Mutex::new(Some(writer)),
        }));
        Ok(self)
    }

    #[must_use]
    pub fn lowered(&self) -> &Lowered<G> {
        &self.lowered
    }

    #[must_use]
    pub fn emulator_identity(&self) -> &str {
        &self.identity
    }

    pub fn preroll(&self) -> Result<Preroll, Box<dyn Error>> {
        let game = &self.lowered.game;
        let root_frames = if self.root.is_empty() {
            0
        } else {
            let machine = (self.machines)().map_err(|error| error.to_string())?;
            let mut unrooted = NesTarget::from_power_on(
                Arc::clone(&self.lowered),
                machine,
                &[],
                self.native_memory,
            )?;
            for action in &self.root {
                unrooted.apply(action);
            }
            if unrooted.exit_kind() != ExitKind::Ok {
                return Err(format!("the {} root failed from power-on", G::NAME).into());
            }
            unrooted.observation().frame_count
        };
        Ok(Preroll {
            setup_frames: target::tape_frames(&game.setup()),
            setup_pokes: game
                .setup_pokes()
                .into_iter()
                .map(|(address, value)| SetupPoke { address, value })
                .collect(),
            start_frames: target::tape_frames(&game.start()),
            root_actions: self.root.len(),
            root_frames,
        })
    }
}

#[cfg(unix)]
impl<G: Game, M: NesBackend<Vec<u8>> + NesMemory + crate::nes_backend::FilmMachine>
    NesWorkload<G, M>
{
    pub fn render_from_power_on(
        &self,
        actions: &[ButtonChord],
        tail_frames: u32,
        skip_frames: u64,
        video_output: &mut dyn std::io::Write,
        audio_output: &mut dyn std::io::Write,
    ) -> Result<target::Filmed<G::State>, Box<dyn Error>> {
        let machine = (self.machines)().map_err(|error| error.to_string())?;
        let tape: Vec<ButtonChord> = self.root.iter().chain(actions).copied().collect();
        NesTarget::render_from_power_on(
            Arc::clone(&self.lowered),
            machine,
            &tape,
            tail_frames,
            skip_frames,
            video_output,
            audio_output,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Preroll {
    pub setup_frames: u64,
    pub setup_pokes: Vec<SetupPoke>,
    pub start_frames: u64,
    pub root_actions: usize,
    pub root_frames: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SetupPoke {
    pub address: usize,
    pub value: u8,
}

#[cfg(unix)]
impl<G: Game> NesWorkload<G, QuickNesMachine> {
    pub fn new(
        game: G,
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        execution_sha256: &str,
    ) -> Result<Self, Box<dyn Error>> {
        let target_rom = if G::MEMORY_WINDOWS.is_empty() {
            rom.to_vec()
        } else {
            machine::nes::with_cartridge_ram(rom)?
        };
        let target_core = core_path.to_path_buf();
        let target_sha256 = core_sha256.to_owned();
        let target_execution = execution_sha256.to_owned();
        Self::with_machines(
            game,
            G::STRATEGY,
            rom,
            &machine::quicknes::core_identity(),
            &crate::nes_backend::native_identity_sha256(core_sha256, execution_sha256),
            Arc::new(move || {
                QuickNesMachine::from_rom_bytes_with_identity(
                    &target_rom,
                    &target_core,
                    &target_sha256,
                    &target_execution,
                )
            }),
        )
    }

    pub fn from_environment(game: G, rom: &[u8]) -> Result<Self, Box<dyn Error>> {
        let core_path = PathBuf::from(
            std::env::var_os("HARMONY_QUICKNES_CORE")
                .ok_or("HARMONY_QUICKNES_CORE must name the pinned QuickNES core")?,
        );
        let core_sha256 = format!("{:x}", Sha256::digest(std::fs::read(&core_path)?));
        let execution_sha256 =
            machine::quicknes::prepared_execution_sha256(&core_path, &core_sha256)?;
        Self::new(game, rom, &core_path, &core_sha256, &execution_sha256)
    }
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
impl<G: Game>
    NesWorkload<G, machine::consonance::ConsonanceMachine, machine::consonance::ConsonancePortable>
{
    pub fn new_consonance(
        game: G,
        rom: &[u8],
        kernel: &[u8],
        initramfs: &[u8],
    ) -> Result<Self, Box<dyn Error>> {
        Self::refuse_native_memory(&game)?;
        let identity_before_genesis = machine::consonance::identity(kernel, initramfs)?;
        let target_kernel = kernel.to_vec();
        let target_initramfs = initramfs.to_vec();
        Self::assemble(
            game,
            G::STRATEGY,
            rom,
            identity_before_genesis,
            RESULT_DIGEST.to_owned(),
            CONSONANCE_SNAPSHOT_CHECKPOINT_FORMAT,
            None,
            Arc::new(move || {
                machine::consonance::ConsonanceMachine::new(&target_kernel, &target_initramfs)
            }),
        )
    }

    fn refuse_native_memory(game: &G) -> Result<(), machine::MachineError> {
        if !game.setup_pokes().is_empty() {
            return Err(target::without_native_memory::<G>(
                "pokes work RAM during setup",
            ));
        }
        if !G::MEMORY_WINDOWS.is_empty() {
            return Err(target::without_native_memory::<G>(
                "reads save RAM on every frame",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FirstSeen {
    pub name: String,
    pub execution: Option<u64>,
    pub input: Option<Input<ButtonChord>>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Violation {
    pub name: String,
    pub count: u64,
    pub execution: u64,
    pub input: Input<ButtonChord>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct NesEvidence {
    aggregate: Satisfied,
    watermark: u32,
    first_seen: Vec<FirstSeen>,
    champion_input: Input<ButtonChord>,
    champion: Option<u32>,
    violations: Vec<Violation>,
}

impl NesEvidence {
    fn name<G: Game>(&mut self, lowered: &Lowered<G>) {
        if self.first_seen.len() != lowered.sometimes().len() {
            self.first_seen = lowered
                .sometimes()
                .iter()
                .map(|name| FirstSeen {
                    name: name.clone(),
                    ..FirstSeen::default()
                })
                .collect();
        }
    }

    fn needs_input(&self, value: Satisfied) -> bool {
        self.first_seen
            .iter()
            .enumerate()
            .any(|(index, seen)| seen.execution.is_none() && value.holds(index))
    }

    fn update_first_seen(
        &mut self,
        value: Satisfied,
        sequence: u64,
        input: Option<&Input<ButtonChord>>,
    ) -> Vec<String> {
        let mut named = Vec::new();
        for (index, seen) in self.first_seen.iter_mut().enumerate() {
            if seen.execution.is_none() && value.holds(index) {
                seen.execution = Some(sequence);
                seen.input = input.cloned();
                named.push(seen.name.clone());
            }
        }
        named
    }
}

pub fn publish_milestone_inputs<I: Serialize, F: AsRef<str>>(
    directory: Option<&Path>,
    files: impl IntoIterator<Item = F>,
    input: &I,
) -> Result<(), Box<dyn Error>> {
    let Some(directory) = directory else {
        return Ok(());
    };
    let mut bytes = None;
    for file in files {
        std::fs::create_dir_all(directory)?;
        let path = directory.join(format!("{}.json", file.as_ref()));
        let temporary = path.with_extension("json.tmp");
        if bytes.is_none() {
            bytes = Some(serde_json::to_vec(input)?);
        }
        std::fs::write(&temporary, bytes.as_deref().unwrap_or_default())?;
        std::fs::rename(temporary, path)?;
    }
    Ok(())
}

pub type NesEntryReport = ArchiveEntryReport<ButtonChord, AssertionKey, Satisfied>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NesArchive;

impl ArchiveFormatName for NesArchive {
    const FORMAT: &'static str = ARCHIVE_FORMAT;
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NesArchiveReport {
    pub format: ArchiveFormat<NesArchive>,
    pub seed: u64,
    pub executions: u64,
    pub milestones: Satisfied,
    pub progress_watermark: u32,
    pub first_seen: Vec<FirstSeen>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<Violation>,
    pub champion_input: Input<ButtonChord>,
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<NesEntryReport>,
    pub progress_curve: Vec<ProgressPoint<Satisfied, u32>>,
    pub retained: u64,
    pub rejected: u64,
    pub deaths: u64,
    #[serde(default)]
    pub selector: SelectorAccounting,
}

type NesActionResult<G, M, P> = CampaignActionResult<NesWorkload<G, M, P>>;
type NesJobResult<G, M, P> = CampaignJobResult<NesWorkload<G, M, P>>;

pub fn chord_time<G: Game>(action: &ButtonChord) -> u64 {
    move_answer(action).map_or_else(
        || u64::from(action.bounded_hold_frames()),
        |(site, label)| G::answer_frames(site, label),
    )
}

fn merge_action_milestones<G: Game, M: NesBackend<P>, P: SnapshotState>(
    aggregate: &mut Satisfied,
    target: &NesTarget<G, M, P>,
) {
    if target.exit_kind() != ExitKind::Ok {
        return;
    }
    for observation in target.last_action_observations() {
        aggregate.0 |= observation.observation.satisfied.0;
    }
}

fn outcome<G: Game, M: NesBackend<P>, P: SnapshotState>(target: &NesTarget<G, M, P>) -> Outcome {
    let observation = target.observation();
    if target.exit_kind() != ExitKind::Ok {
        return Outcome {
            objective_reached: false,
            disposition: ExecutionDisposition::Failed,
            doomed: false,
        };
    }
    let doomed = !observation.goal && (observation.rejected.is_some() || observation.overran);
    Outcome {
        objective_reached: observation.goal,
        disposition: if observation.goal || doomed {
            ExecutionDisposition::Terminal
        } else {
            ExecutionDisposition::Runnable
        },
        doomed,
    }
}

fn asked_chord<M: Copy>(
    drawn: &ButtonChord,
    offer: Option<&Offer<M>>,
    previous: Option<&ButtonChord>,
) -> Option<ButtonChord> {
    if chord::is_ask(drawn) {
        Some(*drawn)
    } else if move_answer(drawn).is_some() && offer.and_then(|offer| offer.chosen(drawn)).is_none()
    {
        Some(chord::ask(previous))
    } else {
        None
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_suffix<G: Game, M: NesBackend<P>, P: SnapshotState>(
    game: &NesWorkload<G, M, P>,
    run: &NesRun,
    target: &mut NesTarget<G, M, P>,
    parent_milestones: Satisfied,
    mut previous: Option<ButtonChord>,
    suffix: &[ButtonChord],
    retention: RetentionPolicy,
    stop: RolloutStop,
) -> Result<NesJobResult<G, M, P>, Box<dyn Error>> {
    if matches!(retention, RetentionPolicy::ProbeAtAdmission) {
        return Err(format!(
            "{} rejects ProbeAtAdmission: ordinary admission is sufficient",
            G::NAME
        )
        .into());
    }
    let mut aggregate = parent_milestones;
    let mut actions = Vec::with_capacity(suffix.len());
    let parent = outcome(target);
    let mut objective_seen = parent.objective_reached;
    if parent.disposition.is_terminal() {
        return Ok(CampaignJobResult {
            preparation_failure: None,
            hang: None,
            actions,
        });
    }
    let mut chooser = stop.choices().map(Chooser::new);
    for drawn in suffix {
        let mut choices = Vec::new();
        let mut edited = None;
        let offer = (!target.observation().stops())
            .then(|| game.lowered.game.moves(&target.observation().state).offer)
            .flatten();
        let asked = asked_chord(drawn, offer.as_ref(), previous.as_ref());
        if asked.is_some() && chooser.is_none() && !chord::is_ask(drawn) {
            break;
        }
        let resolved = if let Some(asked) = asked {
            edited = chord::edited(&asked, previous.as_ref());
            let chooser = chooser.as_mut().ok_or_else(|| {
                format!("{} drew a chord for a job without a choice plan", G::NAME)
            })?;
            match offer {
                Some(offer) => {
                    offer.resolve(&G::CHORDS, chooser, &asked, previous.as_ref(), &mut choices)?
                }
                None => G::CHORDS.resolve(chooser, &asked, previous.as_ref(), &mut choices)?,
            }
        } else if let Some(offer) = offer {
            offer.played(drawn)?
        } else {
            *drawn
        };
        if move_answer(&resolved).is_none() {
            previous = Some(resolved);
        }
        let action = &resolved;
        let action_start_frame = target.observation().frame_count;
        target.apply(action);
        merge_action_milestones(&mut aggregate, target);
        let mut observations = game.rollout_observations(target);
        if let Some(last) = observations.last_mut() {
            last.choices = choices;
            last.edited = edited;
        }
        let raw = outcome(target);
        let outcome = Outcome {
            objective_reached: raw.objective_reached && !objective_seen,
            ..raw
        };
        objective_seen |= raw.objective_reached;
        let recorded_action = if matches!(outcome.disposition, ExecutionDisposition::Terminal)
            && move_answer(action).is_none()
        {
            let elapsed = target
                .observation()
                .frame_count
                .saturating_sub(action_start_frame)
                .min(u64::from(MAX_HOLD_FRAMES));
            ButtonChord::new(action.buttons, u8::try_from(elapsed.max(1))?)
        } else {
            *action
        };
        let candidate = match (outcome.disposition, target.observation().key) {
            (ExecutionDisposition::Runnable, Some(key)) => Some(CampaignCandidate {
                key,
                viable: true,
                snapshot: target
                    .snapshot()
                    .ok_or_else(|| format!("failed to snapshot {}", G::NAME))?,
            }),
            _ => None,
        };
        let cost = game.shrink_cost(&recorded_action, target, &observations);
        actions.push(CampaignActionResult {
            action: recorded_action,
            observations,
            milestones: aggregate,
            outcome,
            candidate,
            cost,
            bound: game.remaining_cost_bound(run, target),
        });
        if outcome.should_stop(&stop) {
            break;
        }
    }
    Ok(CampaignJobResult {
        preparation_failure: None,
        hang: None,
        actions,
    })
}

fn watermark<S>(observations: &[ActionObservation<S>]) -> Option<u32> {
    observations
        .iter()
        .filter_map(|action| action.observation.key.map(|key| key.progress))
        .max()
}

impl<G: Game, M: NesBackend<P>, P: SnapshotState> CampaignTypes for NesWorkload<G, M, P> {
    type JobObservation = ();
    type Target = NesTarget<G, M, P>;
    type Action = ButtonChord;
    type Key = AssertionKey;
    type Milestones = Satisfied;
    type Progress = u32;
    type Snapshot = NesSnapshot<G::State, P>;
    type Observations = ActionObservation<G::State>;
    type Evidence = NesEvidence;
    type ArchiveReport = NesArchiveReport;
    type Run = NesRun;
}

fn named_progress(evidence: &NesEvidence) -> serde_json::Value {
    let first_seen: serde_json::Map<String, serde_json::Value> = evidence
        .first_seen
        .iter()
        .map(|seen| {
            (
                seen.name.clone(),
                seen.execution.map_or(
                    serde_json::Value::Null,
                    |execution| serde_json::json!({ "execution": execution }),
                ),
            )
        })
        .collect();
    serde_json::json!({ "format": NAMED_PROGRESS_FORMAT, "first_seen": first_seen })
}

fn violations(evidence: &NesEvidence) -> serde_json::Value {
    evidence
        .violations
        .iter()
        .map(|violation| {
            serde_json::json!({
                "name": violation.name,
                "kind": "always",
                "count": violation.count,
                "execution": violation.execution,
                "input": violation.input,
            })
        })
        .collect()
}

impl<G: Game, M: NesBackend<P>, P: SnapshotState> Reporting for NesWorkload<G, M, P> {
    fn diagnostics(evidence: &NesEvidence) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "named_progress": named_progress(evidence),
            "violations": violations(evidence),
        }))
    }

    fn merge_witness_diagnostics(
        evidence: &mut NesEvidence,
        observations: &[ActionObservation<G::State>],
        sequence: u64,
    ) {
        for observation in observations {
            evidence.update_first_seen(observation.observation.satisfied, sequence, None);
        }
    }

    fn catalog_signals(&self, _run: &NesRun) -> Vec<String> {
        self.lowered
            .catalog
            .signals()
            .iter()
            .map(|signal| signal.name.clone())
            .collect()
    }

    fn state_record(&self, _run: &NesRun, snapshot: &NesSnapshot<G::State, P>) -> Option<Record> {
        Some(self.lowered.record(&snapshot.observation.state).0)
    }

    fn observation_record(
        &self,
        _run: &NesRun,
        observation: &ActionObservation<G::State>,
    ) -> Option<Record> {
        Some(self.lowered.record(&observation.observation.state).0)
    }

    fn choice_records(
        &self,
        _run: &NesRun,
        _action: &ButtonChord,
        observations: &[ActionObservation<G::State>],
    ) -> Vec<ChoiceRecord> {
        observations
            .iter()
            .flat_map(|observation| observation.choices.iter().cloned())
            .collect()
    }

    fn direct_records(&self, path: Option<&Path>) -> Result<Option<RecordsSize>, Box<dyn Error>> {
        let Some(records) = &self.records else {
            return Ok(None);
        };
        let mut writer = records
            .writer
            .lock()
            .map_err(|_| "the records writer is poisoned")?;
        let size = writer.take().map(RecordsWriter::finish).transpose()?;
        if let Some(path) = path {
            *writer = Some(RecordsWriter::create(path, &records.header)?);
        }
        Ok(size)
    }

    fn write_evidence_checkpoint(
        evidence: &NesEvidence,
        out: &mut dyn std::io::Write,
    ) -> Result<(), Box<dyn Error>> {
        postcard::to_io(evidence, out)?;
        Ok(())
    }

    fn evidence_from_checkpoint(bytes: &[u8]) -> Result<NesEvidence, Box<dyn Error>> {
        Ok(postcard::from_bytes(bytes)?)
    }

    fn checkpoint_marks(evidence: &NesEvidence) -> usize {
        evidence
            .first_seen
            .iter()
            .filter(|seen| seen.execution.is_some())
            .count()
    }

    fn stream_format(&self) -> &'static str {
        CAMPAIGN_STREAM_FORMAT
    }

    fn checkpoint_format(&self) -> &'static str {
        self.checkpoint_format
    }

    fn workload_identity_sha256(&self) -> String {
        self.rom_sha256.clone()
    }

    fn action_cost_unit(&self) -> &'static str {
        "frames"
    }

    fn execution_work_unit(&self) -> &'static str {
        "frames"
    }

    fn result_sha256(&self, result: &NesJobResult<G, M, P>) -> Result<String, Box<dyn Error>> {
        postcard_semantic_result_sha256(result)
    }

    fn snapshot_identity_sha256(
        &self,
        snapshot: &Self::Snapshot,
    ) -> Result<String, Box<dyn Error>> {
        postcard_value_sha256(&(
            self.lowered.compiled.policy_identifier(),
            snapshot.logical_identity()?,
        ))
    }

    fn archive_report(
        &self,
        evidence: &NesEvidence,
        state: ArchiveReportState<Self>,
    ) -> NesArchiveReport {
        NesArchiveReport {
            format: ArchiveFormat::default(),
            seed: state.seed,
            executions: state.executions,
            milestones: evidence.aggregate,
            progress_watermark: evidence.watermark,
            first_seen: evidence.first_seen.clone(),
            violations: evidence.violations.clone(),
            champion_input: evidence.champion_input.clone(),
            entries: state.entries,
            progress_curve: state.progress_curve,
            retained: state.retained,
            rejected: state.rejected,
            deaths: state.doomed_endpoints,
            selector: state.selector,
        }
    }
}

impl<G: Game, M: NesBackend<P>, P: SnapshotState> InputPolicy for NesWorkload<G, M, P> {
    fn policies(&self, _run: &NesRun) -> WorkloadPolicies {
        [
            (GAME_FIELD, G::NAME.to_owned()),
            (CONTROLLER_VOCABULARY_FIELD, G::CHORDS.identifier()),
            (
                KEY_POLICY_FIELD,
                self.lowered.compiled.policy_identifier().to_owned(),
            ),
            (DURATION_POLICY_FIELD, DURATION_IDENTIFIER.to_owned()),
            (CHORD_DRAW_FIELD, CHORD_TREE_IDENTIFIER.to_owned()),
            (REPLACEMENT_POLICY_FIELD, REPLACEMENT_IDENTIFIER.to_owned()),
            (TERMINAL_POLICY_FIELD, TERMINAL_IDENTIFIER.to_owned()),
            (EMULATOR_BACKEND_FIELD, self.identity.clone()),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
    }

    fn resolve_recorded(&self, policies: &WorkloadPolicies) -> Result<NesRun, Box<dyn Error>> {
        let expected = self.policies(&NesRun);
        for (field, value) in &expected {
            match policies.get(field) {
                None => {
                    return Err(format!("the {} stream is missing {field}", G::NAME).into());
                }
                Some(recorded) if recorded != value => {
                    return Err(
                        format!("the {} stream {field} policy is not recognized", G::NAME).into(),
                    );
                }
                Some(_) => {}
            }
        }
        if policies.len() != expected.len() {
            return Err(format!("the {} stream carries an unknown policy", G::NAME).into());
        }
        Ok(NesRun)
    }

    fn sample_alphabet(
        &self,
        _run: &NesRun,
        previous: Option<&ButtonChord>,
        _rand: &mut RomuDuoJrRand,
    ) -> Result<ButtonChord, Box<dyn Error>> {
        Ok(chord::ask(
            previous.filter(|previous| move_answer(previous).is_none()),
        ))
    }

    fn choice_sites(&self, _run: &NesRun) -> Vec<ChoiceSite> {
        self.lowered.choice_sites()
    }

    fn choice_policy(&self, _run: &NesRun) -> ChoicePolicy {
        self.choices
    }
}

impl<G: Game, M: NesBackend<P>, P: SnapshotState> TargetExecution for NesWorkload<G, M, P> {
    fn planned_cost_fn(&self) -> fn(&ButtonChord) -> u64 {
        chord_time::<G>
    }

    fn snapshot_memory_charge(snapshot: &Self::Snapshot) -> usize {
        std::mem::size_of::<Self::Snapshot>()
            .saturating_add(snapshot.emulator_state.memory_charge())
            .saturating_add(snapshot.work_ram.memory_charge())
            .saturating_add(snapshot.accumulator.heap_bytes())
    }

    fn new_target(&self) -> Result<NesTarget<G, M, P>, String> {
        let machine = (self.machines)().map_err(|error| error.to_string())?;
        let mut target = NesTarget::from_power_on(
            Arc::clone(&self.lowered),
            machine,
            &self.root,
            self.native_memory,
        )
        .map_err(|error| error.to_string())?;
        target.record_frames(self.records.is_some());
        Ok(target)
    }

    fn reset(&self, target: &mut NesTarget<G, M, P>) {
        target.reset();
    }

    fn restore(
        &self,
        target: &mut NesTarget<G, M, P>,
        snapshot: &Self::Snapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.restore(snapshot)
    }

    fn execution_work(&self, target: &NesTarget<G, M, P>) -> u64 {
        target.execution_work()
    }

    fn action_choices(&self, action: &ButtonChord) -> Vec<(u16, Choice)> {
        move_answer(action)
            .map(|(site, label)| (CHORD_SITES + site, Choice::Option(label)))
            .into_iter()
            .collect()
    }

    fn apply_action(
        &self,
        target: &mut NesTarget<G, M, P>,
        action: &ButtonChord,
        aggregate: &mut Satisfied,
    ) -> Result<(), Box<dyn Error>> {
        target.apply(action);
        merge_action_milestones(aggregate, target);
        Ok(())
    }

    fn rollout_observations(
        &self,
        target: &NesTarget<G, M, P>,
    ) -> Vec<ActionObservation<G::State>> {
        if target.exit_kind() == ExitKind::Ok {
            target.last_action_observations().to_vec()
        } else {
            Vec::new()
        }
    }

    fn snapshot(&self, target: &mut NesTarget<G, M, P>) -> Result<Self::Snapshot, Box<dyn Error>> {
        target
            .snapshot()
            .ok_or_else(|| format!("failed to snapshot {}", G::NAME).into())
    }

    fn execute_job(
        &self,
        run: &Self::Run,
        target: &mut Self::Target,
        origin_snapshot: &Self::Snapshot,
        replay: &[Self::Action],
        parent_milestones: Self::Milestones,
        suffix: &[Self::Action],
        retention: RetentionPolicy,
        stop: RolloutStop,
    ) -> Result<CampaignJobResult<Self>, Box<dyn Error>> {
        target.restore(origin_snapshot)?;
        for action in replay {
            target.apply(action);
        }
        execute_suffix(
            self,
            run,
            target,
            parent_milestones,
            replay
                .last()
                .copied()
                .filter(|action| move_answer(action).is_none()),
            suffix,
            retention,
            stop,
        )
    }
}

impl<G: Game, M: NesBackend<P>, P: SnapshotState> Evaluation for NesWorkload<G, M, P> {
    fn key_shape(&self) -> KeyShape {
        self.lowered.compiled.shape()
    }

    fn execution_disposition(&self, target: &NesTarget<G, M, P>) -> ExecutionDisposition {
        if target.exit_kind() != ExitKind::Ok {
            ExecutionDisposition::Failed
        } else if target.observation().goal {
            ExecutionDisposition::Terminal
        } else {
            ExecutionDisposition::Runnable
        }
    }

    fn objective_reached(
        &self,
        _run: &NesRun,
        target: &NesTarget<G, M, P>,
    ) -> Result<bool, Box<dyn Error>> {
        Ok(target.exit_kind() == ExitKind::Ok && target.observation().goal)
    }

    fn is_doomed(&self, target: &NesTarget<G, M, P>) -> bool {
        target.observation().rejected.is_some() || target.observation().overran
    }

    fn action_cost(
        &self,
        _action: &ButtonChord,
        observations: &[ActionObservation<G::State>],
    ) -> u64 {
        observations
            .iter()
            .map(|observation| observation.cost)
            .fold(0, u64::saturating_add)
    }

    fn current_key(&self, target: &NesTarget<G, M, P>) -> Result<AssertionKey, Box<dyn Error>> {
        target
            .observation()
            .key
            .ok_or_else(|| format!("the current {} state has no key", G::NAME).into())
    }

    fn complete_candidate_key(
        &self,
        key: AssertionKey,
        _snapshot: &Self::Snapshot,
    ) -> Result<AssertionKey, Box<dyn Error>> {
        Ok(key)
    }

    fn merge_milestones(&self, into: &mut Satisfied, from: Satisfied) {
        into.0 |= from.0;
    }

    fn aggregate_milestones(evidence: &NesEvidence) -> Satisfied {
        evidence.aggregate
    }

    fn aggregate_progress(evidence: &NesEvidence) -> u32 {
        evidence.watermark
    }

    fn merge_origin_evidence(&self, evidence: &mut NesEvidence, source: &NesArchiveReport) {
        evidence.watermark = evidence.watermark.max(source.progress_watermark);
    }

    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut NesEvidence,
        target: &NesTarget<G, M, P>,
    ) -> Result<(), Box<dyn Error>> {
        self.merge_root(evidence, target);
        Ok(())
    }

    fn merge_genesis_evidence(
        &self,
        evidence: &mut NesEvidence,
        target: &NesTarget<G, M, P>,
    ) -> Result<(), Box<dyn Error>> {
        let named = self.merge_root(evidence, target);
        publish_milestone_inputs(
            self.milestone_input_dir.as_deref(),
            named,
            &Input::<ButtonChord>::default(),
        )
    }

    fn merge_import_evidence(
        &self,
        evidence: &mut NesEvidence,
        value: Satisfied,
        input: &Input<ButtonChord>,
    ) {
        evidence.name(&self.lowered);
        evidence.aggregate.0 |= value.0;
        evidence.update_first_seen(value, 0, Some(input));
        if evidence.champion.is_none() {
            evidence.champion_input = input.clone();
        }
    }

    fn merge_action_evidence<F>(
        &self,
        evidence: &mut NesEvidence,
        action: &NesActionResult<G, M, P>,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<Input<ButtonChord>, Box<dyn Error>>,
    {
        evidence.name(&self.lowered);
        if let Some(records) = &self.records
            && let Some(writer) = records
                .writer
                .lock()
                .map_err(|_| "the records writer is poisoned")?
                .as_mut()
        {
            writer.push(&RecordedAction {
                sequence,
                action: action.action,
                failed: action.outcome.disposition.is_failed()
                    || action
                        .observations
                        .iter()
                        .any(|observation| observation.observation.overran),
                cost: self.action_cost(&action.action, &action.observations),
                frames: action
                    .observations
                    .iter()
                    .flat_map(|observation| observation.records.iter().copied())
                    .collect(),
                choices: action
                    .observations
                    .iter()
                    .flat_map(|observation| observation.choices.iter().cloned())
                    .collect::<Vec<ChoiceRecord>>(),
                context: action
                    .observations
                    .iter()
                    .find_map(|observation| observation.edited),
            })?;
        }
        if let Some(progress) = watermark(&action.observations) {
            evidence.watermark = evidence.watermark.max(progress);
        }
        evidence.aggregate.0 |= action.milestones.0;
        let champion = action
            .observations
            .last()
            .and_then(|observation| observation.observation.key)
            .map(|key| key.progress)
            .filter(|progress| evidence.champion.is_none_or(|current| *progress > current));
        let overran = action
            .observations
            .iter()
            .any(|observation| observation.observation.overran);
        if overran
            && let Some(violation) = evidence
                .violations
                .iter_mut()
                .find(|violation| violation.name == FORCED_MOVE_BOUND)
        {
            violation.count += 1;
        }
        let first_violation = overran
            && !evidence
                .violations
                .iter()
                .any(|violation| violation.name == FORCED_MOVE_BOUND);
        if evidence.needs_input(action.milestones) || champion.is_some() || first_violation {
            let input = input()?;
            if first_violation {
                evidence.violations.push(Violation {
                    name: FORCED_MOVE_BOUND.to_owned(),
                    count: 1,
                    execution: sequence,
                    input: input.clone(),
                });
            }
            let named = evidence.update_first_seen(action.milestones, sequence, Some(&input));
            publish_milestone_inputs(self.milestone_input_dir.as_deref(), named, &input)?;
            if let Some(progress) = champion {
                evidence.champion = Some(progress);
                evidence.champion_input = input;
            }
        }
        Ok(())
    }

    fn source_entries<'a>(&self, source: &'a NesArchiveReport) -> &'a [NesEntryReport] {
        &source.entries
    }

    fn resume_input(
        &self,
        source: &NesArchiveReport,
    ) -> Result<Input<ButtonChord>, Box<dyn Error>> {
        source
            .entries
            .iter()
            .max_by_key(|entry| {
                (
                    entry.key,
                    std::cmp::Reverse(entry.input.actions.len()),
                    std::cmp::Reverse(entry.id),
                )
            })
            .map(|entry| entry.input.clone())
            .ok_or_else(|| format!("the {} source archive has no retained entries", G::NAME).into())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::search::assertion::{RecordedJob, Recording, Rekeyed, Rekeying, same_decisions};
    use crate::search::campaign::CampaignAdmissionDecision;
    use crate::search::campaign::{
        ArchiveSource, CampaignCheckpoint, CampaignConfig, CampaignCoordinator,
        CampaignExecutionOptions, CampaignOrigin, JobOutcome, MinimizeConfig, run_job,
    };
    use crate::search::cost::ShrinkCost;
    use crate::search::draw::DrawMixture;
    use fixture::{LEFT, Ledge, RIGHT, Spot, Stairs, Stepper, WARP};

    type StairsWorkload = NesWorkload<Stairs, Stepper>;

    const CLIMB_RECORD_COST: u64 = 6 + 3;

    fn climb() -> ButtonChord {
        ButtonChord::new(RIGHT, 6)
    }

    fn stairs() -> StairsWorkload {
        NesWorkload::with_machines(
            Stairs,
            Stairs::STRATEGY,
            b"stairs",
            "stepper",
            "0",
            Arc::new(|| Ok(Stepper::default())),
        )
        .unwrap()
    }

    fn config<G: Game>(minimize: bool) -> CampaignConfig<NesWorkload<G, Stepper>> {
        CampaignConfig {
            campaign_seed: 7,
            workers: 1,
            execution_budget: 1,
            host: "fixture".to_owned(),
            wall_budget: None,
            stop_rollout_on_objective: true,
            stop_campaign_on_objective: false,
            archive_entry_limit: 256,
            window: 1,
            memory_budget_mib: None,
            materialize_final_artifacts: true,
            run: NesRun,
            mixture: DrawMixture::AlphabetOnly,
            retention: RetentionPolicy::Unprobed,
            objective_witness_path: None,
            minimize: minimize.then(MinimizeConfig::default),
        }
    }

    type Charge = (&'static str, Option<u64>, Option<ShrinkCost>);

    fn charged(
        label: &'static str,
        coordinator: &CampaignCoordinator<'_, StairsWorkload>,
    ) -> Charge {
        let archive = coordinator.archive();
        let index = (0..archive.active.len())
            .find(|&index| {
                archive
                    .entry_input(index)
                    .is_ok_and(|input| input.actions == [climb()])
            })
            .expect("an entry for the climb");
        (
            label,
            archive.entry_cost_in_group(index),
            archive.entry_shrink_total(index),
        )
    }

    fn start<'a>(
        workload: &'a StairsWorkload,
        config: &'a CampaignConfig<StairsWorkload>,
        origin: &CampaignOrigin<StairsWorkload>,
        stream: &'a mut Vec<u8>,
    ) -> CampaignCoordinator<'a, StairsWorkload> {
        CampaignCoordinator::start(
            workload,
            config,
            origin,
            stream,
            None,
            CampaignExecutionOptions::default(),
        )
        .unwrap()
    }

    #[test]
    fn every_insert_path_charges_a_forced_move_its_record_cost() {
        let workload = stairs();
        let ordinary = config(false);
        let mut charges = Vec::new();
        let mut stream = Vec::new();
        let mut live = start(&workload, &ordinary, &CampaignOrigin::Genesis, &mut stream);
        assert!(live.reserve_next().unwrap());
        let mut job = live.plan(1).pop().expect("a job from genesis");
        job.suffix = vec![climb()];
        let mut target = workload.new_target().unwrap();
        let outcome = run_job(&workload, &ordinary, &mut target, &job, None).unwrap();
        let received: JobOutcome<StairsWorkload> =
            postcard::from_bytes(&postcard::to_allocvec(&outcome).unwrap()).unwrap();
        let received_cost = received.result.actions[0]
            .observations
            .last()
            .map(|observation| observation.cost);
        live.admit(received).unwrap();
        charges.push(charged("live admission", &live));
        let (report, snapshots) = live.finish(Vec::new()).unwrap();
        let source = serde_json::to_vec(&report.archive).unwrap();
        let mut renamed = serde_json::to_value(&report.archive).unwrap();
        assert_eq!(renamed["format"], ARCHIVE_FORMAT);
        renamed["format"] = "nes-workload-archive-v0".into();
        let refused =
            ArchiveSource::<NesArchiveReport>::from_bytes(&serde_json::to_vec(&renamed).unwrap())
                .err()
                .expect("another archive format is refused")
                .to_string();
        assert!(
            refused.contains(&format!(
                "archive format is nes-workload-archive-v0; this build reads {ARCHIVE_FORMAT}"
            )),
            "{refused}"
        );
        renamed.as_object_mut().unwrap().remove("format");
        let refused =
            ArchiveSource::<NesArchiveReport>::from_bytes(&serde_json::to_vec(&renamed).unwrap())
                .err()
                .expect("an archive without a format is refused")
                .to_string();
        assert!(
            refused.contains(&format!(
                "archive names no format; this build reads {ARCHIVE_FORMAT}"
            )),
            "{refused}"
        );
        let mut imports = Vec::new();
        for (label, checkpoint) in [
            ("replayed import", None),
            ("checkpointed import", Some(snapshots)),
        ] {
            let origin = CampaignOrigin::Archive {
                path: "source.json".to_owned(),
                source: ArchiveSource::from_bytes(&source).unwrap(),
                checkpoint: checkpoint.map(|snapshots| CampaignCheckpoint {
                    path: "checkpoint.bin".to_owned(),
                    file_sha256: snapshots.sha256().unwrap(),
                    snapshots,
                }),
            };
            let mut stream = Vec::new();
            let imported = start(&workload, &ordinary, &origin, &mut stream);
            charges.push(charged(label, &imported));
            let counts = imported.finish(Vec::new()).unwrap().0.tree_import.unwrap();
            imports.push((counts.imported, counts.checkpointed));
        }
        let shrink = config(true);
        let tape = CampaignOrigin::Tape {
            input: Input {
                actions: vec![climb(), ButtonChord::new(WARP, 9)],
            },
            root: 0,
        };
        let mut stream = Vec::new();
        charges.push(charged(
            "incumbent chain",
            &start(&workload, &shrink, &tape, &mut stream),
        ));

        let record = (
            Some(CLIMB_RECORD_COST),
            Some(ShrinkCost::new(CLIMB_RECORD_COST, 0)),
        );
        assert_eq!(
            charges,
            [
                "live admission",
                "replayed import",
                "checkpointed import",
                "incumbent chain"
            ]
            .map(|label| (label, record.0, record.1))
        );
        assert_eq!(imports, [(1, 0), (1, 1)]);
        assert_eq!(
            report
                .archive
                .entries
                .iter()
                .find(|entry| entry.input.actions == [climb()])
                .and_then(|entry| entry.shrink_total),
            record.1
        );
        assert_eq!(received_cost, Some(CLIMB_RECORD_COST));
    }
    use crate::search::choice::{CERTAIN, ChoicePlan, ChoiceProfile};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Jump {
        Short,
        Long,
        Far,
    }

    const JUMPS: [(u16, Play<Jump>); 4] = [
        (0, Play::Chord),
        (1, Play::Move(Jump::Short)),
        (2, Play::Move(Jump::Long)),
        (3, Play::Move(Jump::Far)),
    ];

    const WAITS: [(u16, Play<Jump>); 2] =
        [(1, Play::Move(Jump::Short)), (7, Play::Move(Jump::Far))];

    const SMALL: ChordVocabulary = ChordVocabulary {
        held: &[0x00, 0x01, 0x80, 0x81],
        tap: None,
        short_hold: (2, 12),
        long_hold: (48, 120),
    };

    fn jumps(offered: u64) -> Offer<Jump> {
        Offer {
            site: 0,
            default: 0,
            options: &JUMPS,
            offered,
        }
    }

    fn waits() -> Offer<Jump> {
        Offer {
            site: 0,
            default: 1,
            options: &WAITS,
            offered: EVERY_OPTION,
        }
    }

    fn chooser(seed: u64, contrary: u32, untried: Option<u32>) -> Chooser {
        Chooser::new(&ChoicePlan {
            profile: SMALL.reproducing_profile(),
            redrawn: false,
            seed,
            untried,
            untried_site: 0,
            held: Vec::new(),
            declared: vec![(CHORD_SITES, contrary)],
            permuted: Vec::new(),
        })
    }

    fn undeclared(seed: u64, untried: Option<u32>) -> Chooser {
        Chooser::new(&ChoicePlan {
            profile: ChoiceProfile::new(0, 0),
            redrawn: false,
            seed,
            untried,
            untried_site: 0,
            held: Vec::new(),
            declared: Vec::new(),
            permuted: Vec::new(),
        })
    }

    fn resolve(offer: &Offer<Jump>, chooser: &mut Chooser) -> (ButtonChord, Vec<ChoiceRecord>) {
        let previous = ButtonChord::new(0x81, 30);
        let mut records = Vec::new();
        let action = offer
            .resolve(
                &SMALL,
                chooser,
                &chord::ask(Some(&previous)),
                Some(&previous),
                &mut records,
            )
            .expect("resolve");
        (action, records)
    }

    #[test]
    fn a_move_chord_away_from_its_offer_is_resolved_as_a_chord() {
        let previous = ButtonChord::new(0x40, 12);
        let wait = move_chord(0, 7).expect("a move chord");
        let asked = asked_chord::<Jump>(&wait, None, Some(&previous)).expect("resolved as a chord");
        assert!(chord::is_ask(&asked));
        assert_eq!(asked.buttons, previous.buttons);
        assert_eq!(asked_chord(&wait, Some(&waits()), Some(&previous)), None);
        assert_eq!(asked_chord::<Jump>(&previous, None, None), None);
        let drawn = chord::ask(Some(&previous));
        assert_eq!(asked_chord(&drawn, Some(&waits()), None), Some(drawn));
        let far = move_chord(0, 3).expect("a move chord");
        assert_eq!(asked_chord(&far, Some(&jumps(EVERY_OPTION)), None), None);
        let long = move_chord(0, 2).expect("a move chord");
        assert!(
            asked_chord(&long, Some(&jumps(0b1011)), None).is_some_and(|ask| chord::is_ask(&ask))
        );
    }

    #[test]
    fn an_offer_beside_chords_plays_a_chord_as_itself_and_a_move_chord_as_its_move() {
        let offer = jumps(0b1011);
        let held = ButtonChord::new(0x81, 9);
        let far = move_chord(0, 3).expect("a move chord");
        assert_eq!(
            (offer.chosen(&held), offer.played(&held).ok()),
            (None, Some(held))
        );
        assert_eq!(
            (offer.chosen(&far), offer.played(&far).ok()),
            (Some(Jump::Far), Some(far))
        );
        assert_eq!(offer.chosen(&move_chord(0, 2).expect("a move chord")), None);
        assert_eq!(offer.chosen(&move_chord(1, 3).expect("a move chord")), None);
        assert_eq!(offer.labels(), [0, 1, 3]);
        assert_eq!(jumps(0b1010).labels(), [0, 1, 3]);
        assert!(!offer.cuts() && !Moves::offer(offer).ends_input(false));
        assert!(waits().cuts() && Moves::offer(waits()).ends_input(false));
        assert!(!Moves::<Jump>::forced(Jump::Short).ends_input(true));
        assert!(Moves::<Jump>::none().ends_input(true) && !Moves::<Jump>::none().ends_input(false));
        assert_eq!(waits().chosen(&held), Some(Jump::Short));
        assert_eq!(waits().played(&held).ok(), move_chord(0, 1).ok());
    }

    #[test]
    fn an_unoffered_label_is_never_taken_and_untried_first_skips_it() {
        let taken = |offer: Offer<Jump>| {
            let mut taken = BTreeMap::new();
            for seed in 0..4_000 {
                let (action, records) = resolve(&offer, &mut chooser(seed, CERTAIN, None));
                let (_, label) = move_answer(&action).expect("a certain rate takes a move");
                *taken.entry(label).or_insert(0_u32) += 1;
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].offered, offer.labels());
            }
            taken.into_keys().collect::<Vec<u16>>()
        };
        let untried = |offer: Offer<Jump>| -> Vec<Option<u16>> {
            (0..4)
                .map(|ordinal| {
                    let (action, _) = resolve(&offer, &mut undeclared(5, Some(ordinal)));
                    move_answer(&action).map(|(_, label)| label)
                })
                .collect()
        };
        assert_eq!(taken(jumps(0b1011)), [1, 3]);
        assert_eq!(untried(jumps(0b1011)), [None, Some(1), Some(3), None]);
        assert_eq!(taken(jumps(EVERY_OPTION)), [1, 2, 3]);
        assert_eq!(
            untried(jumps(EVERY_OPTION)),
            [None, Some(1), Some(2), Some(3)]
        );
    }

    #[test]
    fn the_chord_option_resolves_the_chord_tree_as_without_an_offer() {
        for seed in 0..500 {
            let (action, records) = resolve(&jumps(EVERY_OPTION), &mut chooser(seed, 0, None));
            let previous = ButtonChord::new(0x81, 30);
            let mut alone = Vec::new();
            let without = SMALL
                .resolve(
                    &mut chooser(seed, 0, None),
                    &chord::ask(Some(&previous)),
                    Some(&previous),
                    &mut alone,
                )
                .expect("resolve");
            assert_eq!(action, without);
            assert_eq!(records[0].site, CHORD_SITES);
            assert_eq!(records[0].taken, Choice::Default);
            assert_eq!(records[1..], alone[..]);
        }
    }

    #[test]
    fn a_declared_rate_sets_the_share_of_moves_beside_chords() {
        let mut moves = 0_u32;
        for job in 0..2_400 {
            let mut chooser = chooser(job, CERTAIN / 12, None);
            for _ in 0..10 {
                let (action, records) = resolve(&jumps(EVERY_OPTION), &mut chooser);
                match move_answer(&action) {
                    Some(_) => moves += 1,
                    None => assert!(records.len() > 1),
                }
            }
        }
        assert!((1_807..2_193).contains(&moves), "{moves}");
    }

    #[test]
    fn a_first_sighting_writes_its_tape_and_its_witness_names_it() {
        let directory =
            std::env::temp_dir().join(format!("nes-milestone-inputs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let workload = NesWorkload::<Ledge, Stepper>::with_machines(
            Ledge,
            Ledge::STRATEGY,
            b"ledge",
            "stepper",
            "0",
            Arc::new(|| Ok(Stepper::default())),
        )
        .unwrap()
        .with_milestone_input_dir(directory.clone());
        let input = Input {
            actions: vec![ButtonChord::new(RIGHT, 4)],
        };
        let mut target = workload.new_target().unwrap();
        let mut witness = NesEvidence::default();
        workload
            .merge_snapshot_root_evidence(&mut witness, &target)
            .unwrap();
        assert_eq!(
            named_progress(&witness)["first_seen"]["far"],
            serde_json::Value::Null
        );
        let snapshot = workload.snapshot(&mut target).unwrap();
        let result = workload
            .execute_job(
                &NesRun,
                &mut target,
                &snapshot,
                &[],
                Satisfied::default(),
                &input.actions,
                RetentionPolicy::Unprobed,
                RolloutStop::new(true),
            )
            .unwrap();
        let [action] = result.actions.as_slice() else {
            panic!("one action");
        };
        NesWorkload::<Ledge, Stepper>::merge_witness_diagnostics(
            &mut witness,
            &action.observations,
            1,
        );
        assert_eq!(
            named_progress(&witness)["first_seen"]["far"],
            serde_json::json!({ "execution": 1 })
        );
        let mut evidence = NesEvidence::default();
        workload
            .merge_action_evidence(&mut evidence, action, 5, || Ok(input.clone()))
            .unwrap();
        assert_eq!(evidence.first_seen[0].execution, Some(5));
        let tape: Input<ButtonChord> =
            serde_json::from_slice(&std::fs::read(directory.join("far.json")).unwrap()).unwrap();
        assert_eq!(tape, input);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    fn ledge() -> NesWorkload<Ledge, Stepper> {
        NesWorkload::with_machines(
            Ledge,
            Ledge::STRATEGY,
            b"ledge",
            "stepper",
            "0",
            Arc::new(|| Ok(Stepper::default())),
        )
        .unwrap()
    }

    #[test]
    fn a_rooted_workload_starts_where_its_root_ends_with_the_lineage_the_root_earned() {
        let root = vec![ButtonChord::new(RIGHT, 4), ButtonChord::new(LEFT, 3)];
        let unrooted = ledge();
        let rooted = ledge().with_root(root.clone()).unwrap();
        assert_eq!(
            unrooted.emulator_identity(),
            "stepper;state=ppu-unused2-zero-v1;genesis=ledge-setup-v1;result_digest=semantic-postcard-1.1.3-sha256-hex-v1;sha256=0"
        );
        assert_eq!(
            rooted.emulator_identity(),
            format!(
                "stepper;state=ppu-unused2-zero-v1;genesis=ledge-setup-v1:root-sha256={};result_digest=semantic-postcard-1.1.3-sha256-hex-v1;sha256=0",
                root_sha256(&root)
            )
        );
        assert_ne!(
            root_sha256(&root),
            root_sha256(&[ButtonChord::new(RIGHT, 4), ButtonChord::new(LEFT, 2)])
        );
        let mut played = unrooted.new_target().unwrap();
        for action in &root {
            played.apply(action);
        }
        let played = unrooted.snapshot(&mut played).unwrap();
        let mut target = rooted.new_target().unwrap();
        assert_eq!(target.execution_work(), 0);
        let genesis = rooted.snapshot(&mut target).unwrap();
        assert_eq!(genesis.observation.state, Spot { x: 1, room: 0 });
        assert_eq!(genesis.observation.frame_count, 0);
        assert_eq!(
            NesObservation {
                frame_count: played.observation.frame_count,
                ..genesis.observation.clone()
            },
            played.observation
        );
        assert_eq!(genesis.accumulator, played.accumulator);
        let fresh = NesWorkload::<Ledge, Stepper>::with_machines(
            Ledge,
            Ledge::STRATEGY,
            b"ledge",
            "stepper",
            "0",
            Arc::new(|| Ok(Stepper::at(1))),
        )
        .unwrap();
        let fresh = fresh.new_target().unwrap();
        assert_ne!(
            fresh.observation().key.map(|key| key.scope),
            genesis.observation.key.map(|key| key.scope)
        );
        target.apply(&ButtonChord::new(RIGHT, 2));
        target.reset();
        assert_eq!(rooted.snapshot(&mut target).unwrap(), genesis);
        assert!(ledge().with_root(Vec::new()).is_err());
    }

    #[test]
    fn a_native_snapshot_keeps_its_checkpoint_bytes_and_its_identities() {
        let workload = ledge();
        let mut target = workload.new_target().unwrap();
        for action in [ButtonChord::new(RIGHT, 4), ButtonChord::new(LEFT, 3)] {
            target.apply(&action);
        }
        let snapshot = workload.snapshot(&mut target).unwrap();
        assert!(snapshot.accumulator.heap_bytes() > 0);
        let mut bytes = Vec::new();
        crate::search::snapshot_codec::write_checkpoint(
            &crate::search::campaign::SnapshotCheckpoint {
                format: SNAPSHOT_CHECKPOINT_FORMAT.to_owned(),
                entries: vec![crate::search::campaign::SnapshotCheckpointEntry {
                    id: 7,
                    snapshot: snapshot.clone(),
                }],
            },
            &mut bytes,
        )
        .unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            "3a19fa71f49a8102f96575684d8282ebe4f8f74a925be909d536daa0f3f902b2"
        );
        assert_eq!(
            workload.snapshot_identity_sha256(&snapshot).unwrap(),
            "958579e32174eab332eee50e54ab7c2b76c2d3ddb8ffe6a5c49d3d1c9f600296"
        );
        assert_eq!(
            snapshot.endpoint_identity_sha256().unwrap(),
            "6f55a776803f08bb739eb2b8246dc869a0e427310b422727e1f2b6d8e96c49ac"
        );
    }

    #[cfg(all(
        feature = "consonance",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ))]
    #[test]
    fn a_consonance_workload_refuses_setup_pokes_and_memory_windows_before_it_starts_a_session() {
        let poked = fixture::Preset {
            setup: Vec::new(),
            pokes: vec![(0, 5)],
        };
        assert_eq!(
            NesWorkload::new_consonance(poked, b"preset", &[], &[])
                .err()
                .map(|error| error.to_string()),
            Some(
                "machine backend failed: preset pokes work RAM during setup, which only the native QuickNES backend provides"
                    .to_owned()
            )
        );
        assert_eq!(
            NesWorkload::new_consonance(fixture::Cellar, b"cellar", &[], &[])
                .err()
                .map(|error| error.to_string()),
            Some(
                "machine backend failed: cellar reads save RAM on every frame, which only the native QuickNES backend provides"
                    .to_owned()
            )
        );
    }

    #[cfg(all(
        feature = "consonance",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ))]
    #[test]
    fn consonance_policy_binds_the_admitted_identity_before_guest_replay() {
        let expected = "consonance-whole-vm-v3;session=admitted-test-session";
        let workload = NesWorkload::<
            Ledge,
            machine::consonance::ConsonanceMachine,
            machine::consonance::ConsonancePortable,
        >::assemble(
            Ledge,
            Ledge::STRATEGY,
            b"ledge",
            expected.to_owned(),
            RESULT_DIGEST.to_owned(),
            CONSONANCE_SNAPSHOT_CHECKPOINT_FORMAT,
            None,
            Arc::new(|| {
                Err(machine::MachineError::Backend(
                    "the policy check never builds a target".to_owned(),
                ))
            }),
        )
        .unwrap();
        assert_eq!(
            workload.emulator_identity(),
            format!("{expected};genesis=ledge-setup-v1;{RESULT_DIGEST}")
        );
        assert_eq!(
            workload.checkpoint_format(),
            CONSONANCE_SNAPSHOT_CHECKPOINT_FORMAT
        );
        let mut policies = workload.policies(&NesRun);
        assert!(workload.resolve_recorded(&policies).is_ok());
        policies.insert(
            EMULATOR_BACKEND_FIELD.to_owned(),
            "another loaded executable".to_owned(),
        );
        assert!(workload.resolve_recorded(&policies).is_err());
    }

    #[test]
    fn a_root_that_reaches_the_goal_is_refused_at_that_action() {
        let error = ledge()
            .with_root(vec![ButtonChord::new(RIGHT, 1), ButtonChord::new(WARP, 9)])
            .unwrap()
            .new_target()
            .err()
            .unwrap();
        assert!(
            error.contains("ledge root action 1 reaches a goal"),
            "{error}"
        );
    }

    const LATCHED: &str = "capacity = 16\ngoal = \"room >= 9\"\nspread = \"room\"\nidentity = [\"highest(x)\", \"x\"]\n[[stage]]\nname = \"ledge\"\ngoal = \"far\"\nlatch = true\n[[stage]]\nname = \"beyond\"\ngoal = \"false\"\n";

    fn header_of(workload: NesWorkload<Ledge, Stepper>, name: &str) -> RecordsHeader {
        static FILES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "nes-records-header-{name}-{}-{}.bin",
            std::process::id(),
            FILES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let workload = workload.with_records(&path, 7).unwrap();
        workload.direct_records(None).unwrap();
        let header = Recording::<ButtonChord>::from_bytes(&std::fs::read(&path).unwrap())
            .unwrap()
            .header;
        std::fs::remove_file(&path).unwrap();
        header
    }

    type Played = (
        RecordsHeader,
        Vec<RecordedJob>,
        Vec<RecordedAction<ButtonChord>>,
        Vec<Option<AssertionKey>>,
    );

    const PARENT_OBJECTIVE: &str = "capacity = 16\ngoal = \"room >= 9\"\nspread = \"room\"\nidentity = \"x\"\n[[stage]]\nname = \"ledge\"\nobjective = \"x\"\ngoal = \"highest(objective >= 3)\"\n[[stage]]\nname = \"beyond\"\ngoal = \"false\"\n";

    fn ledge_with(strategy: &str) -> NesWorkload<Ledge, Stepper> {
        NesWorkload::<Ledge, Stepper>::with_machines(
            Ledge,
            strategy,
            b"ledge",
            "stepper",
            "0",
            Arc::new(|| Ok(Stepper::default())),
        )
        .unwrap()
    }

    fn rooted_run(strategy: &str, root: &[ButtonChord]) -> Played {
        let unrooted = header_of(ledge_with(strategy), "unrooted");
        assert_eq!(unrooted.root_sha256, None);
        assert_eq!(unrooted.accumulator_before_genesis, Accumulator::default());
        let workload = ledge_with(strategy).with_root(root.to_vec()).unwrap();
        let mut target = workload.new_target().unwrap();
        let header = header_of(workload, "rooted");
        assert_eq!(header.root_sha256, Some(root_sha256(root)));
        assert_eq!(
            &header.accumulator_before_genesis,
            target.accumulator_before_genesis()
        );
        let mut genesis = header.accumulator_before_genesis.clone();
        let observed = Lowered::new(Ledge, strategy)
            .unwrap()
            .compiled
            .observe(&header.genesis, &mut genesis);
        assert_eq!(&genesis, target.genesis_accumulator());
        assert_eq!(observed.key, target.observation().key);
        target.record_frames(true);
        let mut keys = vec![target.observation().key];
        let mut actions = Vec::new();
        let mut jobs = Vec::new();
        for (sequence, hold) in (1..).zip([1, 1, 2]) {
            let chord = ButtonChord::new(RIGHT, hold);
            target.apply(&chord);
            let emitted = target.last_action_observations();
            actions.push(RecordedAction {
                sequence,
                action: chord,
                failed: false,
                cost: emitted.iter().map(|observation| observation.cost).sum(),
                frames: emitted
                    .iter()
                    .flat_map(|observation| observation.records.iter().copied())
                    .collect(),
                choices: Vec::new(),
                context: None,
            });
            jobs.push(RecordedJob {
                sequence,
                parent_id: sequence - 1,
                decisions: vec![CampaignAdmissionDecision::Retained { id: sequence }],
            });
            keys.push(target.observation().key);
        }
        (header, jobs, actions, keys)
    }

    fn rekey(
        (header, jobs, actions, _): &Played,
        strategy: &str,
        rooted: Option<&Accumulator>,
    ) -> Result<Rekeyed, Box<dyn Error>> {
        let recorded = Lowered::new(Ledge, &header.strategy)?;
        let keyed = Lowered::new(Ledge, strategy)?;
        Rekeying {
            recorded: &recorded.compiled,
            strategy: &keyed.compiled,
            entry_limit: 64,
            rooted,
        }
        .run(&header.genesis, jobs, actions)
    }

    fn held(rekeyed: &Rekeyed) -> Vec<Option<AssertionKey>> {
        let held: BTreeMap<u64, AssertionKey> = rekeyed
            .holders
            .iter()
            .flat_map(|held| held.ids.iter().copied().zip(held.keys.iter().copied()))
            .collect();
        (0..4).map(|id| held.get(&id).copied()).collect()
    }

    fn root() -> Vec<ButtonChord> {
        vec![ButtonChord::new(RIGHT, 4), ButtonChord::new(LEFT, 3)]
    }

    #[test]
    fn a_rooted_run_with_a_latched_stage_re_keys_to_the_keys_the_live_search_recorded() {
        let played = rooted_run(LATCHED, &root());
        let rooted = Some(&played.0.accumulator_before_genesis);
        let rekeyed = rekey(&played, LATCHED, rooted).unwrap();
        assert_eq!(same_decisions(&played.1, &rekeyed), played.1.len());
        assert_eq!(held(&rekeyed), played.3);
        let unseeded = rekey(&played, LATCHED, None)
            .ok()
            .map(|unseeded| held(&unseeded));
        assert_ne!(unseeded, Some(played.3.clone()));
    }

    #[test]
    fn a_rooted_genesis_re_keys_from_the_accumulator_before_its_record() {
        let played = rooted_run(PARENT_OBJECTIVE, &[ButtonChord::new(RIGHT, 3)]);
        let compiled = Lowered::new(Ledge, PARENT_OBJECTIVE).unwrap().compiled;
        let mut genesis = played.0.accumulator_before_genesis.clone();
        let _ = compiled.observe(&played.0.genesis, &mut genesis);
        assert_eq!(genesis.stage(), Some(0));
        let mut again = genesis.clone();
        let _ = compiled.observe(&played.0.genesis, &mut again);
        assert_eq!(again.stage(), Some(1));
        let rooted = Some(&played.0.accumulator_before_genesis);
        let rekeyed = rekey(&played, PARENT_OBJECTIVE, rooted).unwrap();
        assert_eq!(held(&rekeyed), played.3);
        assert_eq!(same_decisions(&played.1, &rekeyed), played.1.len());
    }

    #[test]
    fn a_workload_with_open_records_refuses_a_root() {
        let path = std::env::temp_dir().join(format!(
            "nes-records-before-root-{}.bin",
            std::process::id()
        ));
        let error = ledge_with(LATCHED)
            .with_records(&path, 7)
            .unwrap()
            .with_root(root())
            .err()
            .unwrap()
            .to_string();
        std::fs::remove_file(&path).unwrap();
        assert!(
            error.contains("root the workload before opening its records"),
            "{error}"
        );
    }

    #[test]
    fn a_rooted_run_refuses_another_strategy_that_reads_its_lineage() {
        let played = rooted_run(LATCHED, &root());
        let rooted = Some(&played.0.accumulator_before_genesis);
        let error = rekey(
            &played,
            &LATCHED.replace("capacity = 16", "capacity = 8"),
            rooted,
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("cannot be rebuilt without the root's frames"),
            "{error}"
        );
    }

    #[test]
    fn a_rooted_run_re_keys_under_another_strategy_without_lineage() {
        let played = rooted_run(LATCHED, &root());
        let rooted = Some(&played.0.accumulator_before_genesis);
        let pure = "capacity = 16\ngoal = \"room >= 9\"\nspread = \"room\"\nidentity = \"x\"\n";
        let rekeyed = rekey(&played, pure, rooted).unwrap();
        assert_eq!(same_decisions(&played.1, &rekeyed), played.1.len());
        let keyed = Lowered::new(Ledge, pure).unwrap();
        let mut expected = vec![
            keyed
                .compiled
                .observe(&played.0.genesis, &mut Accumulator::default())
                .key,
        ];
        expected.extend(played.2.iter().map(|action| {
            keyed
                .compiled
                .observe(action.frames.last().unwrap(), &mut Accumulator::default())
                .key
        }));
        assert_eq!(held(&rekeyed), expected);
    }

    #[test]
    fn a_mark_that_holds_at_power_on_is_seen_at_execution_zero_by_the_campaign_and_the_witness() {
        let directory =
            std::env::temp_dir().join(format!("nes-genesis-inputs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let workload = NesWorkload::<Ledge, Stepper>::with_machines(
            Ledge,
            Ledge::STRATEGY,
            b"ledge",
            "stepper",
            "0",
            Arc::new(|| Ok(Stepper::at(3))),
        )
        .unwrap()
        .with_milestone_input_dir(directory.clone());
        let target = workload.new_target().unwrap();
        let mut witness = NesEvidence::default();
        workload
            .merge_snapshot_root_evidence(&mut witness, &target)
            .unwrap();
        assert_eq!(
            named_progress(&witness)["first_seen"]["far"],
            serde_json::json!({ "execution": 0 })
        );
        assert!(!directory.exists());
        let config = config(false);
        let mut stream = Vec::new();
        let coordinator = CampaignCoordinator::start(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
            CampaignExecutionOptions::default(),
        )
        .unwrap();
        let (report, _) = coordinator.finish(Vec::new()).unwrap();
        assert_eq!(report.archive.first_seen[0].execution, Some(0));
        assert_eq!(report.archive.milestones, Satisfied(1));
        let tape: Input<ButtonChord> =
            serde_json::from_slice(&std::fs::read(directory.join("far.json")).unwrap()).unwrap();
        assert_eq!(tape, Input::default());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    const FALLS_AT_FOUR: &str = "capacity = 16\ngoal = \"room >= 9\"\nspread = \"room\"\nidentity = \"x\"\nreject = \"x >= 4\"\n[[stage]]\nname = \"ledge\"\nobjective = \"x\"\ngoal = \"x >= 4\"\n[[stage]]\nname = \"beyond\"\ngoal = \"false\"\n";

    fn fall(
        workload: &NesWorkload<Ledge, Stepper>,
        actions: &[ButtonChord],
    ) -> Vec<NesActionResult<Ledge, Stepper, Vec<u8>>> {
        let mut target = workload.new_target().unwrap();
        let snapshot = workload.snapshot(&mut target).unwrap();
        let result = workload
            .execute_job(
                &NesRun,
                &mut target,
                &snapshot,
                &[],
                Satisfied::default(),
                actions,
                RetentionPolicy::Unprobed,
                RolloutStop::new(true),
            )
            .unwrap();
        let fallen = result.actions.last().unwrap();
        let last = &fallen.observations.last().unwrap().observation;
        assert_eq!((last.state.x, last.rejected, last.key), (4, Some(0), None));
        assert!(fallen.outcome.doomed && fallen.candidate.is_none());
        result.actions
    }

    fn stage_and_objective(
        workload: &NesWorkload<Ledge, Stepper>,
        progress: u32,
    ) -> (Option<String>, Vec<i64>) {
        let view = workload.lowered.compiled.view(AssertionKey {
            progress,
            ..AssertionKey::default()
        });
        (view.stage, view.progress)
    }

    #[test]
    fn a_rejected_frame_raises_no_watermark() {
        let workload = ledge_with(FALLS_AT_FOUR);
        let actions = fall(&workload, &[ButtonChord::new(RIGHT, 4)]);
        let [action] = actions.as_slice() else {
            panic!("one action");
        };
        let mut evidence = NesEvidence::default();
        workload
            .merge_action_evidence(&mut evidence, action, 1, || Ok(Input::default()))
            .unwrap();
        assert_eq!(
            stage_and_objective(&workload, evidence.watermark),
            (Some("ledge".to_owned()), vec![3])
        );
    }

    #[test]
    fn a_rejected_frame_becomes_no_champion() {
        let workload = ledge_with(FALLS_AT_FOUR);
        let step = ButtonChord::new(RIGHT, 2);
        let actions = fall(&workload, &[step, step]);
        let [walked, fallen] = actions.as_slice() else {
            panic!("two actions");
        };
        let one = Input {
            actions: vec![step],
        };
        let mut evidence = NesEvidence::default();
        workload
            .merge_action_evidence(&mut evidence, walked, 1, || Ok(one.clone()))
            .unwrap();
        workload
            .merge_action_evidence(&mut evidence, fallen, 2, || {
                Ok(Input {
                    actions: vec![step, step],
                })
            })
            .unwrap();
        let champion = evidence.champion.unwrap();
        assert_eq!(
            stage_and_objective(&workload, champion),
            (Some("ledge".to_owned()), vec![2])
        );
        assert_eq!(evidence.champion_input, one);
    }

    #[test]
    fn a_violation_report_carries_the_input_that_reproduces_it() {
        let input = Input {
            actions: vec![ButtonChord::new(1, 2), ButtonChord::new(0, 120)],
        };
        let evidence = NesEvidence {
            violations: vec![Violation {
                name: FORCED_MOVE_BOUND.to_owned(),
                count: 3,
                execution: 167_208,
                input: input.clone(),
            }],
            ..NesEvidence::default()
        };
        let report = violations(&evidence);
        assert_eq!(report[0]["name"], FORCED_MOVE_BOUND);
        assert_eq!(report[0]["execution"], 167_208);
        let replayed: Input<ButtonChord> =
            serde_json::from_value(report[0]["input"].clone()).expect("an input");
        assert_eq!(replayed, input);
    }
}
