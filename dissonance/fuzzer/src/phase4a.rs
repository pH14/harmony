// SPDX-License-Identifier: AGPL-3.0-or-later

//! Phase 4a: generated semantic mutators and the adventure experiment.

use std::{
    borrow::Cow,
    cell::RefCell,
    error::Error,
    ffi::OsString,
    fs,
    io::Write,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    process::{Command, Stdio},
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
    mutators::{MutationResult, Mutator},
    observers::StdMapObserver,
    schedulers::WeightedScheduler,
    stages::StdMutationalStage,
    state::{HasCorpus, HasExecutions, StdState},
};
use libafl_bolts::{
    HasLen, Named,
    rands::{Rand, StdRand},
    tuples::{RefIndexable, tuple_list},
};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{
    phase2::{Flag, Interest, TriageLabels, TriageScore},
    target::{AdventureAction, AdventureObservations, AdventureToy, Room, execute_actions},
};

/// Maximum action count accepted by the adventure campaign.
pub const MAX_ADVENTURE_ACTIONS: usize = 24;

const ADVENTURE_MAP_SIZE: usize = 64;
const ADVENTURE_MAP_SIZE_U64: u64 = 64;
const DEFAULT_MACRO_RETIRE_AFTER: u64 = 512;
const INSTALLED_REPORT_FILE: &str = "phase4a-installed-report.json";

const ALL_ADVENTURE_ACTIONS: [AdventureAction; 7] = [
    AdventureAction::North,
    AdventureAction::South,
    AdventureAction::East,
    AdventureAction::West,
    AdventureAction::TakeKey,
    AdventureAction::OpenDoor,
    AdventureAction::Wait,
];

/// An adventure input replayed from genesis.
#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct AdventureInput {
    /// Total actions to replay.
    pub actions: Vec<AdventureAction>,
}

impl Input for AdventureInput {}

impl HasLen for AdventureInput {
    fn len(&self) -> usize {
        self.actions.len()
    }
}

/// The complete generated semantic-mutator surface.
///
/// Generated source receives an immutable input and returns one candidate. The
/// host adapter owns provenance, accounting, retirement, and corpus mutation.
pub trait GeneratedMutator {
    /// Produce one deterministic candidate from `input`.
    fn mutate(&self, input: &AdventureInput) -> AdventureInput;
}

/// Hand-written equivalent of the source emitted by the install harness.
#[derive(Clone, Copy, Debug, Default)]
pub struct FetchKeyThenOpenDoor;

impl GeneratedMutator for FetchKeyThenOpenDoor {
    fn mutate(&self, _input: &AdventureInput) -> AdventureInput {
        AdventureInput {
            actions: vec![
                AdventureAction::North,
                AdventureAction::TakeKey,
                AdventureAction::South,
                AdventureAction::South,
                AdventureAction::OpenDoor,
                AdventureAction::East,
            ],
        }
    }
}

/// Corpus provenance attached to every retained generated offspring.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProducerMetadata {
    /// Stable host-assigned mutator name; generated code cannot forge it.
    pub mutator: String,
}

libafl_bolts::impl_serdeany!(ProducerMetadata);

/// Deterministic generated-mutator usefulness counters.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MutatorStats {
    /// Candidates actually emitted by this mutator.
    pub offspring: u64,
    /// Emitted candidates retained as novel corpus entries.
    pub novel_offspring: u64,
    /// Consecutive emitted candidates that were not retained.
    pub executions_without_novelty: u64,
    /// Whether the host will still invoke this mutator.
    pub active: bool,
}

impl Default for MutatorStats {
    fn default() -> Self {
        Self {
            offspring: 0,
            novel_offspring: 0,
            executions_without_novelty: 0,
            active: true,
        }
    }
}

/// Host-owned adapter for one generated mutator.
#[derive(Debug)]
pub struct GeneratedMutatorAdapter<M> {
    generated: M,
    name: Cow<'static, str>,
    retire_after: u64,
    stats: Rc<RefCell<MutatorStats>>,
    emitted: bool,
}

impl<M> GeneratedMutatorAdapter<M> {
    /// Wrap generated code with deterministic host policy.
    #[must_use]
    pub fn new(
        generated: M,
        name: impl Into<Cow<'static, str>>,
        retire_after: u64,
        stats: Rc<RefCell<MutatorStats>>,
    ) -> Self {
        Self {
            generated,
            name: name.into(),
            retire_after,
            stats,
            emitted: false,
        }
    }
}

impl<M> Named for GeneratedMutatorAdapter<M> {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<M, S> Mutator<AdventureInput, S> for GeneratedMutatorAdapter<M>
where
    M: GeneratedMutator,
    S: HasCorpus<AdventureInput>,
{
    fn mutate(
        &mut self,
        _state: &mut S,
        input: &mut AdventureInput,
    ) -> Result<MutationResult, LibAflError> {
        self.emitted = false;
        if !self.stats.borrow().active {
            return Ok(MutationResult::Skipped);
        }

        let candidate = self.generated.mutate(input);
        if candidate.actions.len() > MAX_ADVENTURE_ACTIONS {
            return Err(LibAflError::illegal_state(
                "generated mutator exceeded the adventure action limit",
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

        let retained = new_corpus_id.is_some();
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
        if retained {
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

/// Pure generated-detector surface for the adventure observations.
pub trait AdventureDetector {
    /// Map one complete run's observations to deterministic feature keys.
    fn features(&self, observations: &[AdventureObservations]) -> Vec<u64>;
}

/// Detector for inventory and door state hidden from the room-only base map.
#[derive(Clone, Copy, Debug, Default)]
pub struct InventoryDoorDetector;

impl AdventureDetector for InventoryDoorDetector {
    fn features(&self, observations: &[AdventureObservations]) -> Vec<u64> {
        let Some(final_observation) = observations.last() else {
            return Vec::new();
        };
        let room = room_index(final_observation.room);
        let mut features = Vec::new();
        if final_observation.has_key {
            features.push(0x10 + room);
        }
        if final_observation.door_open {
            features.push(0x20 + room);
        }
        features
    }
}

#[derive(Clone, Copy, Debug)]
struct ConfiguredDetector<D> {
    enabled: bool,
    detector: D,
}

impl<D> AdventureDetector for ConfiguredDetector<D>
where
    D: AdventureDetector,
{
    fn features(&self, observations: &[AdventureObservations]) -> Vec<u64> {
        if self.enabled {
            self.detector.features(observations)
        } else {
            Vec::new()
        }
    }
}

fn room_index(room: Room) -> u64 {
    match room {
        Room::Start => 0,
        Room::Key => 1,
        Room::Door => 2,
        Room::Treasure => 3,
        Room::Hazard => 4,
    }
}

fn adventure_progress(observation: &AdventureObservations) -> u8 {
    if observation.target {
        6
    } else if observation.room == Room::Door && observation.door_open {
        5
    } else if observation.room == Room::Door && observation.has_key {
        4
    } else if observation.room == Room::Start && observation.has_key {
        3
    } else if observation.room == Room::Key && observation.has_key {
        2
    } else if observation.room == Room::Key {
        1
    } else {
        0
    }
}

fn run_adventure(input: &AdventureInput) -> Vec<AdventureObservations> {
    execute_actions(&mut AdventureToy::default(), &input.actions)
}

/// Stable JSON request passed to an external triage process.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TriageRequest {
    /// Monotonic LibAFL corpus identifier.
    pub testcase_id: u64,
    /// Complete deterministic run evidence.
    pub observations: Vec<AdventureObservations>,
    /// Compact line-oriented evidence for regex fixtures and human operators.
    pub log: String,
}

/// Boundary implemented by null, scripted, and future model-backed triagers.
pub trait Triager {
    /// Produce scheduler labels from a stable request.
    fn triage(&mut self, request: &TriageRequest) -> Result<TriageLabels, LibAflError>;
}

/// A triager that leaves every retained testcase neutral.
#[derive(Clone, Copy, Debug, Default)]
pub struct NullTriager;

impl Triager for NullTriager {
    fn triage(&mut self, request: &TriageRequest) -> Result<TriageLabels, LibAflError> {
        Ok(labels_for_request(request, false))
    }
}

/// Deterministic regex fixture used in the scripted experiment arm.
#[derive(Clone, Debug)]
pub struct ScriptedTriager {
    progress: Regex,
}

impl ScriptedTriager {
    /// Compile the fixed fixture expression.
    pub fn new() -> Result<Self, regex::Error> {
        Ok(Self {
            progress: Regex::new(r"progress=([0-6])")?,
        })
    }
}

impl Triager for ScriptedTriager {
    fn triage(&mut self, request: &TriageRequest) -> Result<TriageLabels, LibAflError> {
        let progress = self
            .progress
            .captures(&request.log)
            .and_then(|captures| captures.get(1))
            .and_then(|capture| capture.as_str().parse::<u8>().ok())
            .ok_or_else(|| LibAflError::illegal_state("scripted triage log has no progress"))?;
        Ok(labels_for_progress(progress, true))
    }
}

/// JSON-over-stdin/stdout adapter for a future external triage program.
///
/// The child receives one [`TriageRequest`] and must return one
/// [`TriageLabels`]. Experiment tests use only deterministic in-process
/// implementations; model output can later be recorded at this boundary.
#[derive(Clone, Debug)]
pub struct SubprocessTriager {
    program: PathBuf,
    args: Vec<OsString>,
}

impl SubprocessTriager {
    /// Configure an external triage executable and its arguments.
    #[must_use]
    pub fn new(program: PathBuf, args: Vec<OsString>) -> Self {
        Self { program, args }
    }
}

impl Triager for SubprocessTriager {
    fn triage(&mut self, request: &TriageRequest) -> Result<TriageLabels, LibAflError> {
        let mut child = Command::new(&self.program)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let encoded = serde_json::to_vec(request)
            .map_err(|error| LibAflError::serialize(error.to_string()))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| LibAflError::illegal_state("triage subprocess has no stdin"))?;
        stdin.write_all(&encoded)?;
        drop(stdin);

        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(LibAflError::illegal_state(format!(
                "triage subprocess exited with {}",
                output.status
            )));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|error| LibAflError::serialize(error.to_string()))
    }
}

fn labels_for_request(request: &TriageRequest, scripted: bool) -> TriageLabels {
    let progress = request.observations.last().map_or(0, adventure_progress);
    labels_for_progress(progress, scripted)
}

fn labels_for_progress(progress: u8, scripted: bool) -> TriageLabels {
    TriageLabels {
        interest: if !scripted {
            Interest::Neutral
        } else if progress > 0 {
            Interest::Boost
        } else {
            Interest::Suppress
        },
        duplicate_of: None,
        flags: if progress == 0 {
            vec![Flag::DeadEnd]
        } else {
            Vec::new()
        },
        tags: vec![format!("adventure-progress-{progress}")],
        summary: format!("reached adventure progress {progress}"),
        hypotheses: if scripted && progress == 5 {
            vec!["cross the open door as one coherent action pattern".to_owned()]
        } else {
            Vec::new()
        },
    }
}

type AdventureObserver = StdMapObserver<'static, u8, false>;
type AdventureObservers = (AdventureObserver, (AdventureObserver, ()));

#[derive(Debug)]
struct AdventureExecutor<D> {
    observers: AdventureObservers,
    detector: D,
    last: Vec<AdventureObservations>,
}

impl<D> AdventureExecutor<D> {
    fn new(base: AdventureObserver, generated: AdventureObserver, detector: D) -> Self {
        Self {
            observers: tuple_list!(base, generated),
            detector,
            last: Vec::new(),
        }
    }
}

impl<D> HasObservers for AdventureExecutor<D> {
    type Observers = AdventureObservers;

    fn observers(&self) -> RefIndexable<&Self::Observers, Self::Observers> {
        RefIndexable::from(&self.observers)
    }

    fn observers_mut(&mut self) -> RefIndexable<&mut Self::Observers, Self::Observers> {
        RefIndexable::from(&mut self.observers)
    }
}

impl<D, EM, S, Z> Executor<EM, AdventureInput, S, Z> for AdventureExecutor<D>
where
    D: AdventureDetector,
    S: libafl::state::HasExecutions,
{
    fn run_target(
        &mut self,
        _fuzzer: &mut Z,
        state: &mut S,
        _manager: &mut EM,
        input: &AdventureInput,
    ) -> Result<ExitKind, LibAflError> {
        *state.executions_mut() = state.executions().saturating_add(1);
        self.last = run_adventure(input);
        for observation in &self.last {
            let index = usize::try_from(room_index(observation.room) % ADVENTURE_MAP_SIZE_U64)
                .map_err(|_| LibAflError::illegal_state("base feature index overflow"))?;
            self.observers.0[index] = 1;
        }
        for feature in self.detector.features(&self.last) {
            let index = usize::try_from(feature % ADVENTURE_MAP_SIZE_U64)
                .map_err(|_| LibAflError::illegal_state("detector feature index overflow"))?;
            self.observers.1.0[index] = 1;
        }
        Ok(self.last.last().map_or(ExitKind::Ok, |observation| {
            if observation.crashed {
                ExitKind::Crash
            } else {
                ExitKind::Ok
            }
        }))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LastProducer {
    Base,
    Macro,
}

struct AdventureCampaignMutator<M> {
    name: Cow<'static, str>,
    macro_enabled: bool,
    generated: GeneratedMutatorAdapter<M>,
    triager: Box<dyn Triager>,
    last_producer: Option<LastProducer>,
}

impl<M> AdventureCampaignMutator<M> {
    fn new(
        macro_enabled: bool,
        generated: GeneratedMutatorAdapter<M>,
        triager: Box<dyn Triager>,
    ) -> Self {
        Self {
            name: Cow::Borrowed("AdventureCampaignMutator"),
            macro_enabled,
            generated,
            triager,
            last_producer: None,
        }
    }
}

impl<M> Named for AdventureCampaignMutator<M> {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<M, S> Mutator<AdventureInput, S> for AdventureCampaignMutator<M>
where
    M: GeneratedMutator,
    S: HasCorpus<AdventureInput> + libafl::state::HasRand,
{
    fn mutate(
        &mut self,
        state: &mut S,
        input: &mut AdventureInput,
    ) -> Result<MutationResult, LibAflError> {
        self.last_producer = None;
        let choose_macro = self.macro_enabled
            && state
                .rand_mut()
                .below(NonZeroUsize::new(4).expect("constant is nonzero"))
                == 0;
        if choose_macro {
            let result = self.generated.mutate(state, input)?;
            if result == MutationResult::Mutated {
                self.last_producer = Some(LastProducer::Macro);
            }
            return Ok(result);
        }

        if input.actions.len() >= MAX_ADVENTURE_ACTIONS {
            return Ok(MutationResult::Skipped);
        }
        let action = state
            .rand_mut()
            .below(NonZeroUsize::new(ALL_ADVENTURE_ACTIONS.len()).expect("actions are nonempty"));
        input.actions.push(ALL_ADVENTURE_ACTIONS[action]);
        self.last_producer = Some(LastProducer::Base);
        Ok(MutationResult::Mutated)
    }

    fn post_exec(
        &mut self,
        state: &mut S,
        new_corpus_id: Option<CorpusId>,
    ) -> Result<(), LibAflError> {
        if self.last_producer == Some(LastProducer::Macro) {
            self.generated.post_exec(state, new_corpus_id)?;
        }
        let Some(id) = new_corpus_id else {
            self.last_producer = None;
            return Ok(());
        };

        if self.last_producer == Some(LastProducer::Base) {
            state
                .corpus()
                .get(id)?
                .borrow_mut()
                .add_metadata(ProducerMetadata {
                    mutator: "base-append".to_owned(),
                });
        }

        let input = state.corpus().cloned_input_for_id(id)?;
        let observations = run_adventure(&input);
        let final_observation = observations
            .last()
            .ok_or_else(|| LibAflError::illegal_state("adventure run omitted genesis"))?;
        let progress = adventure_progress(final_observation);
        let target = final_observation.target;
        let crashed = final_observation.crashed;
        let request = TriageRequest {
            testcase_id: u64::try_from(id.0)
                .map_err(|_| LibAflError::illegal_state("corpus id does not fit u64"))?,
            observations,
            log: format!("progress={progress} target={target} crashed={crashed}"),
        };
        let labels = self.triager.triage(&request)?;
        state.corpus().get(id)?.borrow_mut().add_metadata(labels);
        self.last_producer = None;
        Ok(())
    }
}

/// Triage arm in the Phase 4a matrix.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum TriageArm {
    /// Neutral labels.
    Null,
    /// Deterministic regex labels.
    Scripted,
}

/// Instrumentation/mutation arm in the Phase 4a matrix.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum SearchArm {
    /// Coarse room map and single-action mutation.
    Base,
    /// Base map plus generated inventory/door features.
    GeneratedDetectors,
    /// Generated features plus the semantic action macro.
    DetectorsAndMacros,
}

/// One semantic corpus entry used by deterministic replay assertions.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdventureCorpusEntry {
    /// Corpus identifier in insertion order.
    pub id: u64,
    /// Parent corpus identifier when produced by mutation.
    pub parent: Option<u64>,
    /// Host-owned producer tag.
    pub producer: String,
    /// Retained input.
    pub input: AdventureInput,
}

/// Metrics for one seed/configuration run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdventureRunReport {
    /// Explicit RNG seed.
    pub seed: u64,
    /// Triage configuration.
    pub triage: TriageArm,
    /// Search configuration.
    pub search: SearchArm,
    /// Target executions including genesis evaluation.
    pub executions: u64,
    /// First target execution, or `None` at the budget.
    pub time_to_target: Option<u64>,
    /// Exact maximum semantic progress retained.
    pub maximum_progress: u8,
    /// Insertion-ordered semantic corpus.
    pub corpus: Vec<AdventureCorpusEntry>,
    /// Generated macro accounting.
    pub macro_stats: MutatorStats,
}

/// Aggregated deterministic measurements for one matrix cell.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdventureMatrixCell {
    /// Triage configuration.
    pub triage: TriageArm,
    /// Search configuration.
    pub search: SearchArm,
    /// Per-seed execution counts, censored to `budget + 1` on failure.
    pub execution_counts: Vec<u64>,
    /// Upper median of the sorted execution counts.
    pub median_executions: u64,
    /// Number of seeds that reached the target.
    pub reached: usize,
}

/// Complete 2 × 3 Phase 4a experiment report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdventureMatrixReport {
    /// Fixed execution budget per seed.
    pub execution_budget: u64,
    /// Seeds in caller-provided order.
    pub seeds: Vec<u64>,
    /// Six cells in triage-major, search-minor order.
    pub cells: Vec<AdventureMatrixCell>,
}

type AdventureState = StdState<
    InMemoryCorpus<AdventureInput>,
    AdventureInput,
    StdRand,
    InMemoryCorpus<AdventureInput>,
>;

/// Run one deterministic adventure campaign and write its report.
pub fn run_adventure_campaign(
    output_dir: &Path,
    seed: u64,
    execution_budget: u64,
    triage: TriageArm,
    search: SearchArm,
) -> Result<AdventureRunReport, Box<dyn Error>> {
    run_adventure_campaign_with_artifacts(
        output_dir,
        seed,
        execution_budget,
        triage,
        search,
        InventoryDoorDetector,
        FetchKeyThenOpenDoor,
    )
}

fn run_adventure_campaign_with_artifacts<D, M>(
    output_dir: &Path,
    seed: u64,
    execution_budget: u64,
    triage: TriageArm,
    search: SearchArm,
    detector: D,
    generated_mutator: M,
) -> Result<AdventureRunReport, Box<dyn Error>>
where
    D: AdventureDetector,
    M: GeneratedMutator,
{
    fs::create_dir_all(output_dir)?;
    let base_observer = StdMapObserver::owned("phase4a_base", vec![0_u8; ADVENTURE_MAP_SIZE]);
    let generated_observer =
        StdMapObserver::owned("phase4a_generated", vec![0_u8; ADVENTURE_MAP_SIZE]);
    let base_feedback = MaxMapFeedback::new(&base_observer);
    let generated_feedback = MaxMapFeedback::new(&generated_observer);
    let mut feedback = EagerOrFeedback::new(base_feedback, generated_feedback);
    let mut objective = ConstFeedback::new(false);
    let mut state = StdState::new(
        StdRand::with_seed(seed),
        InMemoryCorpus::new(),
        InMemoryCorpus::new(),
        &mut feedback,
        &mut objective,
    )?;
    let scheduler =
        WeightedScheduler::<_, TriageScore, AdventureObserver>::new(&mut state, &base_observer);
    let mut fuzzer = StdFuzzer::new(scheduler, feedback, objective);
    let mut manager = NopEventManager::new();
    let detector_enabled = search != SearchArm::Base;
    let mut executor = AdventureExecutor::new(
        base_observer,
        generated_observer,
        ConfiguredDetector {
            enabled: detector_enabled,
            detector,
        },
    );
    let seed_id = fuzzer.add_input(
        &mut state,
        &mut executor,
        &mut manager,
        AdventureInput::default(),
    )?;
    let mut seed_triager: Box<dyn Triager> = match triage {
        TriageArm::Null => Box::new(NullTriager),
        TriageArm::Scripted => Box::new(ScriptedTriager::new()?),
    };
    let seed_observations = run_adventure(&AdventureInput::default());
    let seed_request = TriageRequest {
        testcase_id: u64::try_from(seed_id.0)?,
        observations: seed_observations,
        log: "progress=0 target=false crashed=false".to_owned(),
    };
    {
        let mut testcase = state.corpus().get(seed_id)?.borrow_mut();
        testcase.add_metadata(ProducerMetadata {
            mutator: "seed".to_owned(),
        });
        testcase.add_metadata(seed_triager.triage(&seed_request)?);
    }

    let macro_stats = Rc::new(RefCell::new(MutatorStats::default()));
    let generated = GeneratedMutatorAdapter::new(
        generated_mutator,
        "fetch-key-then-open-door",
        DEFAULT_MACRO_RETIRE_AFTER,
        Rc::clone(&macro_stats),
    );
    let triager: Box<dyn Triager> = match triage {
        TriageArm::Null => Box::new(NullTriager),
        TriageArm::Scripted => Box::new(ScriptedTriager::new()?),
    };
    let mutator =
        AdventureCampaignMutator::new(search == SearchArm::DetectorsAndMacros, generated, triager);
    let mut stages = tuple_list!(StdMutationalStage::with_max_iterations(
        mutator,
        NonZeroUsize::MIN,
    ));

    let mut time_to_target = None;
    while *state.executions() < execution_budget {
        fuzzer.fuzz_one(&mut stages, &mut executor, &mut state, &mut manager)?;
        if executor
            .last
            .last()
            .is_some_and(|observation| observation.target)
        {
            time_to_target = Some(*state.executions());
            break;
        }
    }

    let (corpus, maximum_progress) = semantic_adventure_corpus(&state)?;
    let report = AdventureRunReport {
        seed,
        triage,
        search,
        executions: *state.executions(),
        time_to_target,
        maximum_progress,
        corpus,
        macro_stats: macro_stats.borrow().clone(),
    };
    fs::write(
        output_dir.join("phase4a-run-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(report)
}

fn semantic_adventure_corpus(
    state: &AdventureState,
) -> Result<(Vec<AdventureCorpusEntry>, u8), Box<dyn Error>> {
    let mut entries = Vec::new();
    let mut maximum = 0;
    for id in state.corpus().ids() {
        let testcase = state.corpus().get(id)?.borrow();
        let input = testcase
            .input()
            .as_ref()
            .ok_or("in-memory adventure testcase has no input")?
            .clone();
        let producer = testcase
            .metadata_map()
            .get::<ProducerMetadata>()
            .ok_or("adventure testcase has no producer metadata")?
            .mutator
            .clone();
        let parent = testcase
            .parent_id()
            .map(|parent_id| u64::try_from(parent_id.0))
            .transpose()?;
        let progress = run_adventure(&input).last().map_or(0, adventure_progress);
        maximum = maximum.max(progress);
        entries.push(AdventureCorpusEntry {
            id: u64::try_from(id.0)?,
            parent,
            producer,
            input,
        });
    }
    Ok((entries, maximum))
}

/// Run and persist all six null/scripted × base/detector/macro cells.
pub fn run_adventure_matrix(
    output_dir: &Path,
    seeds: &[u64],
    execution_budget: u64,
) -> Result<AdventureMatrixReport, Box<dyn Error>> {
    if seeds.is_empty() {
        return Err("adventure matrix requires at least one seed".into());
    }
    fs::create_dir_all(output_dir)?;
    let mut cells = Vec::new();
    for triage in [TriageArm::Null, TriageArm::Scripted] {
        for search in [
            SearchArm::Base,
            SearchArm::GeneratedDetectors,
            SearchArm::DetectorsAndMacros,
        ] {
            let mut execution_counts = Vec::new();
            let mut reached = 0;
            for (index, seed) in seeds.iter().copied().enumerate() {
                let run_dir = output_dir.join(format!("{:?}-{:?}-{index}", triage, search));
                let run = run_adventure_campaign(&run_dir, seed, execution_budget, triage, search)?;
                if run.time_to_target.is_some() {
                    reached += 1;
                }
                execution_counts.push(
                    run.time_to_target
                        .unwrap_or_else(|| execution_budget.saturating_add(1)),
                );
            }
            let mut sorted = execution_counts.clone();
            sorted.sort_unstable();
            let median_executions = sorted[sorted.len() / 2];
            cells.push(AdventureMatrixCell {
                triage,
                search,
                execution_counts,
                median_executions,
                reached,
            });
        }
    }
    let report = AdventureMatrixReport {
        execution_budget,
        seeds: seeds.to_vec(),
        cells,
    };
    fs::write(
        output_dir.join("phase4a-matrix-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(report)
}

/// Run generated detector and mutator types inside the separately built process.
pub fn run_installed_adventure_and_write_report<D, M>(
    output_dir: &Path,
    seed: u64,
    execution_budget: u64,
    triage: TriageArm,
    detector: D,
    generated_mutator: M,
) -> Result<(), Box<dyn Error>>
where
    D: AdventureDetector,
    M: GeneratedMutator,
{
    let report = run_adventure_campaign_with_artifacts(
        output_dir,
        seed,
        execution_budget,
        triage,
        SearchArm::DetectorsAndMacros,
        detector,
        generated_mutator,
    )?;
    fs::write(
        output_dir.join(INSTALLED_REPORT_FILE),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(())
}

/// Emit detector and semantic-mutator source, build it, and restart a campaign.
pub fn install_build_restart_adventure(
    output_dir: &Path,
    build_dir: &Path,
    seed: u64,
    execution_budget: u64,
    triage: TriageArm,
) -> Result<AdventureRunReport, Box<dyn Error>> {
    fs::create_dir_all(output_dir)?;
    let crate_dir = build_dir.join("installed-adventure-artifacts");
    let source_dir = crate_dir.join("src");
    fs::create_dir_all(&source_dir)?;
    let dependency_path = env!("CARGO_MANIFEST_DIR")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    fs::write(
        crate_dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"phase4a-installed\"\nversion = \"0.1.0\"\nedition = \"2024\"\nlicense = \"AGPL-3.0-or-later\"\n\n[dependencies]\nfuzzer = {{ path = \"{dependency_path}\" }}\n\n[workspace]\n"
        ),
    )?;
    fs::write(
        source_dir.join("artifacts.rs"),
        "// SPDX-License-Identifier: AGPL-3.0-or-later\n\nuse fuzzer::{\n    phase4a::{AdventureDetector, AdventureInput, GeneratedMutator},\n    target::{AdventureAction, AdventureObservations, Room},\n};\n\npub struct InstalledDetector;\n\nimpl AdventureDetector for InstalledDetector {\n    fn features(&self, observations: &[AdventureObservations]) -> Vec<u64> {\n        let Some(final_observation) = observations.last() else { return Vec::new(); };\n        let room = match final_observation.room {\n            Room::Start => 0, Room::Key => 1, Room::Door => 2,\n            Room::Treasure => 3, Room::Hazard => 4,\n        };\n        let mut features = Vec::new();\n        if final_observation.has_key { features.push(0x10 + room); }\n        if final_observation.door_open { features.push(0x20 + room); }\n        features\n    }\n}\n\npub struct InstalledMacro;\n\nimpl GeneratedMutator for InstalledMacro {\n    fn mutate(&self, _input: &AdventureInput) -> AdventureInput {\n        AdventureInput { actions: vec![\n            AdventureAction::North, AdventureAction::TakeKey,\n            AdventureAction::South, AdventureAction::South,\n            AdventureAction::OpenDoor, AdventureAction::East,\n        ] }\n    }\n}\n",
    )?;
    let triage_variant = match triage {
        TriageArm::Null => "Null",
        TriageArm::Scripted => "Scripted",
    };
    fs::write(
        source_dir.join("main.rs"),
        format!(
            "// SPDX-License-Identifier: AGPL-3.0-or-later\n\nmod artifacts;\n\nuse std::{{error::Error, path::PathBuf}};\nuse artifacts::{{InstalledDetector, InstalledMacro}};\nuse fuzzer::phase4a::{{TriageArm, run_installed_adventure_and_write_report}};\n\nfn main() -> Result<(), Box<dyn Error>> {{\n    let output = PathBuf::from(std::env::args_os().nth(1).ok_or(\"missing output directory\")?);\n    run_installed_adventure_and_write_report(\n        &output, {seed}, {execution_budget}, TriageArm::{triage_variant},\n        InstalledDetector, InstalledMacro,\n    )\n}}\n"
        ),
    )?;

    let target_dir = build_dir.join("target");
    let build = Command::new("cargo")
        .arg("build")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(crate_dir.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target_dir)
        .status()?;
    if !build.success() {
        return Err("generated adventure artifact build failed".into());
    }
    let binary = target_dir.join("debug").join(if cfg!(windows) {
        "phase4a-installed.exe"
    } else {
        "phase4a-installed"
    });
    let restart = Command::new(binary).arg(output_dir).status()?;
    if !restart.success() {
        return Err("restarted adventure campaign failed".into());
    }
    Ok(serde_json::from_slice(&fs::read(
        output_dir.join(INSTALLED_REPORT_FILE),
    )?)?)
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use libafl::{
        HasMetadata,
        corpus::{Corpus, InMemoryCorpus, Testcase},
        mutators::{MutationResult, Mutator},
        state::{HasCorpus, StdState},
    };
    use libafl_bolts::rands::StdRand;
    use proptest::prelude::*;

    use super::{
        AdventureInput, FetchKeyThenOpenDoor, GeneratedMutator, GeneratedMutatorAdapter,
        MAX_ADVENTURE_ACTIONS, MutatorStats, ProducerMetadata, SearchArm, TriageArm,
        install_build_restart_adventure, run_adventure_campaign, run_adventure_matrix,
    };
    use crate::target::{AdventureAction, AdventureToy, Target, execute_actions};

    type TestState = StdState<
        InMemoryCorpus<AdventureInput>,
        AdventureInput,
        StdRand,
        InMemoryCorpus<AdventureInput>,
    >;

    fn state() -> TestState {
        StdState::new(
            StdRand::with_seed(7),
            InMemoryCorpus::new(),
            InMemoryCorpus::new(),
            &mut (),
            &mut (),
        )
        .expect("construct test state")
    }

    fn action_strategy() -> impl Strategy<Value = AdventureAction> {
        prop_oneof![
            Just(AdventureAction::North),
            Just(AdventureAction::South),
            Just(AdventureAction::East),
            Just(AdventureAction::West),
            Just(AdventureAction::TakeKey),
            Just(AdventureAction::OpenDoor),
            Just(AdventureAction::Wait),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn generated_macro_always_emits_a_valid_total_input(
            actions in prop::collection::vec(action_strategy(), 0..=MAX_ADVENTURE_ACTIONS)
        ) {
            let output = FetchKeyThenOpenDoor.mutate(&AdventureInput { actions });
            prop_assert!(output.actions.len() <= MAX_ADVENTURE_ACTIONS);

            let mut target = AdventureToy::default();
            let observations = execute_actions(&mut target, &output.actions);
            prop_assert_eq!(observations.len(), output.actions.len() + 1);
            prop_assert!(target.observe().target);
        }
    }

    #[test]
    fn adapter_accounts_tags_and_retires_mechanically() {
        let stats = Rc::new(RefCell::new(MutatorStats::default()));
        let mut adapter = GeneratedMutatorAdapter::new(
            FetchKeyThenOpenDoor,
            "fetch-key-then-open-door",
            2,
            Rc::clone(&stats),
        );
        let mut state = state();

        let mut first = AdventureInput::default();
        assert_eq!(
            adapter
                .mutate(&mut state, &mut first)
                .expect("first mutation"),
            MutationResult::Mutated
        );
        let id = state
            .corpus_mut()
            .add(Testcase::new(first))
            .expect("add generated offspring");
        adapter
            .post_exec(&mut state, Some(id))
            .expect("account novel offspring");
        assert_eq!(
            state
                .corpus()
                .get(id)
                .expect("generated testcase")
                .borrow()
                .metadata_map()
                .get::<ProducerMetadata>()
                .expect("producer metadata")
                .mutator,
            "fetch-key-then-open-door"
        );

        for _ in 0..2 {
            let mut input = AdventureInput::default();
            assert_eq!(
                adapter.mutate(&mut state, &mut input).expect("mutation"),
                MutationResult::Mutated
            );
            adapter
                .post_exec(&mut state, None)
                .expect("account non-novel offspring");
        }

        let snapshot = stats.borrow().clone();
        assert_eq!(snapshot.offspring, 3);
        assert_eq!(snapshot.novel_offspring, 1);
        assert_eq!(snapshot.executions_without_novelty, 2);
        assert!(!snapshot.active);

        let mut input = AdventureInput::default();
        assert_eq!(
            adapter
                .mutate(&mut state, &mut input)
                .expect("retired mutation"),
            MutationResult::Skipped
        );
        assert_eq!(stats.borrow().offspring, 3);
    }

    #[test]
    fn same_seed_macro_campaigns_are_bit_identical() {
        let first_dir = tempfile::tempdir().expect("first campaign directory");
        let second_dir = tempfile::tempdir().expect("second campaign directory");
        let first = run_adventure_campaign(
            first_dir.path(),
            0x4a00_0001,
            5_000,
            TriageArm::Scripted,
            SearchArm::DetectorsAndMacros,
        )
        .expect("first macro campaign");
        let second = run_adventure_campaign(
            second_dir.path(),
            0x4a00_0001,
            5_000,
            TriageArm::Scripted,
            SearchArm::DetectorsAndMacros,
        )
        .expect("second macro campaign");
        assert_eq!(first, second);
        assert!(first.time_to_target.is_some());
        assert!(first.corpus.iter().all(|entry| !entry.producer.is_empty()));
    }

    #[test]
    fn full_matrix_records_all_cells_and_macros_beat_detectors() {
        let output = tempfile::tempdir().expect("matrix directory");
        let seeds = [0x4a00_1000, 0x4a00_1001, 0x4a00_1002, 0x4a00_1003];
        let report = run_adventure_matrix(output.path(), &seeds, 10_000).expect("adventure matrix");
        assert_eq!(report.cells.len(), 6);

        for triage in [TriageArm::Null, TriageArm::Scripted] {
            let base = report
                .cells
                .iter()
                .find(|cell| cell.triage == triage && cell.search == SearchArm::Base)
                .expect("base matrix cell");
            let detectors = report
                .cells
                .iter()
                .find(|cell| cell.triage == triage && cell.search == SearchArm::GeneratedDetectors)
                .expect("detector matrix cell");
            let macros = report
                .cells
                .iter()
                .find(|cell| cell.triage == triage && cell.search == SearchArm::DetectorsAndMacros)
                .expect("macro matrix cell");
            assert_eq!(base.reached, 0);
            assert_eq!(detectors.reached, seeds.len());
            assert_eq!(macros.reached, seeds.len());
            assert!(macros.median_executions < detectors.median_executions);
        }
        assert!(output.path().join("phase4a-matrix-report.json").is_file());
    }

    #[test]
    fn generated_macro_build_restart_rescues_the_adventure() {
        let output = tempfile::tempdir().expect("installed campaign directory");
        let build = tempfile::tempdir().expect("generated build directory");
        let report = install_build_restart_adventure(
            output.path(),
            build.path(),
            0x4a00_2000,
            5_000,
            TriageArm::Scripted,
        )
        .expect("install, build, and restart generated adventure artifacts");
        assert!(report.time_to_target.is_some());
        assert!(report.macro_stats.offspring > 0);
        assert!(report.macro_stats.novel_offspring > 0);
        assert!(
            report
                .corpus
                .iter()
                .any(|entry| entry.producer == "fetch-key-then-open-door")
        );
    }
}
