// SPDX-License-Identifier: AGPL-3.0-or-later

//! Phase 4a: generated semantic mutators and the adventure experiment.

use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
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
    fuzzer::{Evaluator, Fuzzer, HasFeedback},
    inputs::Input,
    mutators::{MutationResult, Mutator},
    observers::StdMapObserver,
    schedulers::{QueueScheduler, WeightedScheduler},
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
    phase3::ScopedFeedback,
    target::{
        AdventureAction, AdventureObservations, AdventureSnapshot, AdventureToy, Room, Target,
        execute_actions,
    },
};

/// Maximum action count accepted by the adventure campaign.
pub const MAX_ADVENTURE_ACTIONS: usize = 24;

const ADVENTURE_MAP_SIZE: usize = 64;
const ADVENTURE_MAP_SIZE_U64: u64 = 64;
const DEFAULT_MACRO_RETIRE_AFTER: u64 = 512;
const DEFAULT_DETECTOR_RETIRE_AFTER: u64 = 10_000;
const INSTALLED_REPORT_FILE: &str = "phase4a-installed-report.json";
const INSTALLED_DETECTOR_REPORT_FILE: &str = "phase4a-installed-detector-report.json";
const MAX_TRIAGE_CALLS: u64 = 200;

/// Scheduling-path executor selected for an adventure campaign.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdventureExecutorMode {
    /// Historical replay from genesis.
    Legacy,
    /// Resume from the deepest retained testcase prefix.
    #[default]
    SnapshotResume,
}

/// Replayable work performed by an adventure scheduling-path executor.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdventureExecutionWork {
    /// Adventure actions evaluated after genesis or snapshot resume.
    pub evaluated_actions: u64,
    /// Retained snapshots restored.
    pub snapshot_restores: u64,
}

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

/// One deterministic label decision recorded for no-model replay.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecordedTriageEvent {
    /// Exact request presented to the triager.
    pub request: TriageRequest,
    /// Labels applied to the testcase and scheduler.
    pub labels: TriageLabels,
    /// Host-side failure that caused neutral fallback, when present.
    pub failure: Option<String>,
}

struct RecordingTriager {
    inner: Box<dyn Triager>,
    events: Rc<RefCell<Vec<RecordedTriageEvent>>>,
    calls: u64,
    max_calls: u64,
    continue_on_failure: bool,
}

impl RecordingTriager {
    fn new(
        inner: Box<dyn Triager>,
        events: Rc<RefCell<Vec<RecordedTriageEvent>>>,
        max_calls: u64,
        continue_on_failure: bool,
    ) -> Self {
        Self {
            inner,
            events,
            calls: 0,
            max_calls,
            continue_on_failure,
        }
    }
}

impl Triager for RecordingTriager {
    fn triage(&mut self, request: &TriageRequest) -> Result<TriageLabels, LibAflError> {
        let (labels, failure) = if self.calls >= self.max_calls {
            (
                labels_for_request(request, false),
                Some(format!(
                    "triage call budget of {} exhausted",
                    self.max_calls
                )),
            )
        } else {
            self.calls = self.calls.saturating_add(1);
            match self.inner.triage(request) {
                Ok(labels) => (labels, None),
                Err(error) if self.continue_on_failure => {
                    (labels_for_request(request, false), Some(error.to_string()))
                }
                Err(error) => return Err(error),
            }
        };
        self.events.borrow_mut().push(RecordedTriageEvent {
            request: request.clone(),
            labels: labels.clone(),
            failure,
        });
        Ok(labels)
    }
}

struct ReplayTriager {
    events: Vec<RecordedTriageEvent>,
    cursor: Rc<RefCell<usize>>,
}

struct OperatorViewTriager {
    inner: Box<dyn Triager>,
    operator_view: PathBuf,
    retained: u64,
}

impl OperatorViewTriager {
    fn new(inner: Box<dyn Triager>, operator_view: PathBuf) -> Self {
        Self {
            inner,
            operator_view,
            retained: 0,
        }
    }
}

impl Triager for OperatorViewTriager {
    fn triage(&mut self, request: &TriageRequest) -> Result<TriageLabels, LibAflError> {
        self.retained = self.retained.saturating_add(1);
        let testcase_name = format!("testcase-{:020}", request.testcase_id);
        fs::write(
            self.operator_view
                .join("corpus")
                .join(format!("{testcase_name}.json")),
            serde_json::to_vec_pretty(request)
                .map_err(|error| LibAflError::serialize(error.to_string()))?,
        )?;
        fs::write(
            self.operator_view.join("fuzzer_stats"),
            format!(
                "target : adventure-toy\nretained_testcases : {}\nlast_testcase_id : {}\n",
                self.retained, request.testcase_id
            ),
        )?;
        let labels = self.inner.triage(request)?;
        fs::write(
            self.operator_view
                .join("corpus")
                .join(format!("{testcase_name}.labels.json")),
            serde_json::to_vec_pretty(&labels)
                .map_err(|error| LibAflError::serialize(error.to_string()))?,
        )?;
        Ok(labels)
    }
}

impl ReplayTriager {
    fn new(events: &[RecordedTriageEvent], cursor: Rc<RefCell<usize>>) -> Self {
        Self {
            events: events.to_vec(),
            cursor,
        }
    }
}

impl Triager for ReplayTriager {
    fn triage(&mut self, request: &TriageRequest) -> Result<TriageLabels, LibAflError> {
        let index = *self.cursor.borrow();
        let event = self.events.get(index).ok_or_else(|| {
            LibAflError::illegal_state("recorded triage events ended before campaign")
        })?;
        if event.request != *request {
            return Err(LibAflError::illegal_state(format!(
                "recorded triage request mismatch at event {index}"
            )));
        }
        *self.cursor.borrow_mut() = index.saturating_add(1);
        Ok(event.labels.clone())
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
    last_input: AdventureInput,
    mode: AdventureExecutorMode,
    work: AdventureExecutionWork,
    pinned: BTreeMap<Vec<AdventureAction>, AdventureCachedPrefix>,
    transient: BTreeMap<Vec<AdventureAction>, AdventureCachedPrefix>,
}

#[derive(Clone, Debug)]
struct AdventureCachedPrefix {
    snapshot: AdventureSnapshot,
    observations: Vec<AdventureObservations>,
}

impl<D> AdventureExecutor<D> {
    fn new(
        base: AdventureObserver,
        generated: AdventureObserver,
        detector: D,
        mode: AdventureExecutorMode,
    ) -> Self {
        Self {
            observers: tuple_list!(base, generated),
            detector,
            last: Vec::new(),
            last_input: AdventureInput::default(),
            mode,
            work: AdventureExecutionWork::default(),
            pinned: BTreeMap::new(),
            transient: BTreeMap::new(),
        }
    }

    fn pin_last_input(&mut self) {
        if self.mode == AdventureExecutorMode::SnapshotResume
            && let Some(snapshot) = self.transient.remove(&self.last_input.actions)
        {
            self.pinned
                .insert(self.last_input.actions.clone(), snapshot);
        }
    }

    fn work(&self) -> AdventureExecutionWork {
        self.work
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
        self.last_input = input.clone();
        let resume = if self.mode == AdventureExecutorMode::SnapshotResume {
            (0..=input.actions.len()).rev().find_map(|length| {
                self.pinned
                    .get(&input.actions[..length])
                    .cloned()
                    .map(|cached| (length, cached))
            })
        } else {
            None
        };
        let mut target = AdventureToy::default();
        let start = if let Some((length, cached)) = resume {
            target.restore(&cached.snapshot)?;
            self.work.snapshot_restores = self.work.snapshot_restores.saturating_add(1);
            self.last = cached.observations;
            length
        } else {
            self.last = vec![target.observe()];
            0
        };
        for action in &input.actions[start..] {
            target.apply(action);
            self.last.push(target.observe());
            self.work.evaluated_actions = self.work.evaluated_actions.saturating_add(1);
            if target.exit_kind() != ExitKind::Ok {
                break;
            }
        }
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
        if self.mode == AdventureExecutorMode::SnapshotResume {
            let snapshot = target
                .snapshot()
                .ok_or_else(|| LibAflError::illegal_state("adventure snapshot was unavailable"))?;
            self.transient.insert(
                input.actions.clone(),
                AdventureCachedPrefix {
                    snapshot,
                    observations: self.last.clone(),
                },
            );
            if self.transient.len() > 8
                && let Some(key) = self.transient.keys().next().cloned()
            {
                self.transient.remove(&key);
            }
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
    /// Live GPT-5.6 Luna labels supplied through [`SubprocessTriager`].
    Luna,
}

/// Action requested by one model instrumentor invocation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentorAction {
    /// Compile and install a generated novelty detector.
    InstallDetector,
    /// Compile and install a generated semantic mutator.
    InstallMutator,
    /// Compile and install a generated within-cell archive ranking.
    InstallRanking,
    /// Make no change for this invocation.
    None,
}

/// Structured output produced by the model instrumentor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InstrumentorDecision {
    /// Requested host action.
    pub action: InstrumentorAction,
    /// Suggested artifact name; the host sanitizes or replaces it.
    pub name: String,
    /// Complete Rust source implementing the requested generated trait.
    pub rust_source: String,
    /// Optional corpus id whose descendants should receive the detector.
    pub scope_to_lineage: Option<u64>,
    /// Concise evidence-grounded reason for the decision.
    pub rationale: String,
}

/// Request metadata for one bounded instrumentor attempt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InstrumentorRequest {
    /// Independent trial number, starting at one.
    pub trial: u8,
    /// Compile/fixture attempt number within this invocation, starting at one.
    pub attempt: u8,
    /// Error returned by the preceding host validation attempt, if any.
    pub previous_error: Option<String>,
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
    /// Scheduler labels applied before this testcase was selected.
    pub labels: TriageLabels,
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
    /// Fixed execution ceiling supplied to this invocation.
    pub execution_budget: u64,
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
    /// Ordered triage decisions sufficient for no-model replay.
    pub triage_events: Vec<RecordedTriageEvent>,
    /// Number of triage calls that fell back to neutral labels.
    pub triage_failures: u64,
    /// Executor implementation selected for this campaign.
    #[serde(default)]
    pub executor_mode: AdventureExecutorMode,
    /// Deterministic executor work counters.
    #[serde(default)]
    pub executor_work: AdventureExecutionWork,
}

/// Exhaustive evidence that no one-action child of the retained base corpus
/// can add a room-map feature or reach the target.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdventurePlateauProof {
    /// Base room-map indices represented by the retained corpus.
    pub retained_rooms: Vec<u64>,
    /// Number of retained-entry/action children checked.
    pub checked_children: usize,
    /// Whether any checked child adds a base-visible room.
    pub child_can_add_base_novelty: bool,
    /// Whether any checked child reaches the target.
    pub child_can_reach_target: bool,
}

/// Deterministic usefulness and retirement counters for one installed detector.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdventureDetectorStats {
    /// Corpus entries attributable only to generated detector novelty.
    pub novelties: u64,
    /// Executions since the detector most recently produced novelty.
    pub executions_without_novelty: u64,
    /// Whether generated feedback remains installed.
    pub active: bool,
}

impl Default for AdventureDetectorStats {
    fn default() -> Self {
        Self {
            novelties: 0,
            executions_without_novelty: 0,
            active: true,
        }
    }
}

impl AdventureDetectorStats {
    fn record(&mut self, novelty: bool, retire_after: u64) {
        if !self.active {
            return;
        }
        if novelty {
            self.novelties = self.novelties.saturating_add(1);
            self.executions_without_novelty = 0;
        } else {
            self.executions_without_novelty = self.executions_without_novelty.saturating_add(1);
            if self.executions_without_novelty >= retire_after {
                self.active = false;
            }
        }
    }
}

/// Result of rebuilding and resuming the labeled plateau with one detector.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InstalledAdventureDetectorReport {
    /// Independent restart RNG seed.
    pub seed: u64,
    /// Fixed execution allowance after restoring the plateau.
    pub execution_budget: u64,
    /// Executions represented by the restored plateau.
    pub starting_executions: u64,
    /// Executions performed by this installed process.
    pub invocation_executions: u64,
    /// First post-restart execution that reached the target.
    pub time_to_target: Option<u64>,
    /// Maximum semantic progress represented by the final corpus.
    pub maximum_progress: u8,
    /// Corpus count restored before search resumed.
    pub starting_corpus_count: usize,
    /// Optional lineage root selected by the instrumentor.
    pub scope_to_lineage: Option<u64>,
    /// Insertion-ordered final semantic corpus.
    pub corpus: Vec<AdventureCorpusEntry>,
    /// Mechanical detector novelty and retirement accounting.
    pub detector: AdventureDetectorStats,
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
    run_adventure_campaign_with_executor(
        output_dir,
        seed,
        execution_budget,
        triage,
        search,
        AdventureExecutorMode::SnapshotResume,
    )
}

/// Run one adventure campaign through a selected executor for the Phase 1 identity gate.
pub fn run_adventure_campaign_with_executor(
    output_dir: &Path,
    seed: u64,
    execution_budget: u64,
    triage: TriageArm,
    search: SearchArm,
    executor_mode: AdventureExecutorMode,
) -> Result<AdventureRunReport, Box<dyn Error>> {
    run_adventure_campaign_with_artifacts(
        output_dir,
        seed,
        execution_budget,
        triage,
        search,
        InventoryDoorDetector,
        FetchKeyThenOpenDoor,
        executor_mode,
    )
}

#[allow(clippy::too_many_arguments)] // The identity-gate executor mode is an explicit campaign axis.
fn run_adventure_campaign_with_artifacts<D, M>(
    output_dir: &Path,
    seed: u64,
    execution_budget: u64,
    triage: TriageArm,
    search: SearchArm,
    detector: D,
    generated_mutator: M,
    executor_mode: AdventureExecutorMode,
) -> Result<AdventureRunReport, Box<dyn Error>>
where
    D: AdventureDetector,
    M: GeneratedMutator,
{
    let triager: Box<dyn Triager> = match triage {
        TriageArm::Null => Box::new(NullTriager),
        TriageArm::Scripted => Box::new(ScriptedTriager::new()?),
        TriageArm::Luna => {
            return Err("Luna campaigns require an explicit subprocess configuration".into());
        }
    };
    run_adventure_campaign_with_triager(
        output_dir,
        seed,
        execution_budget,
        triage,
        search,
        detector,
        generated_mutator,
        triager,
        MAX_TRIAGE_CALLS,
        false,
        executor_mode,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_adventure_campaign_with_triager<D, M>(
    output_dir: &Path,
    seed: u64,
    execution_budget: u64,
    triage: TriageArm,
    search: SearchArm,
    detector: D,
    generated_mutator: M,
    triager: Box<dyn Triager>,
    max_triage_calls: u64,
    continue_on_triage_failure: bool,
    executor_mode: AdventureExecutorMode,
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
    // M1 labels are the instrumentor's input at this boundary.  The rebuilt
    // detector trial follows phase 3's queue-scheduled restart so newly
    // detector-retained children are not starved behind stale plateau weights.
    let mut fuzzer = StdFuzzer::new(QueueScheduler::new(), feedback, objective);
    let mut manager = NopEventManager::new();
    let detector_enabled = search != SearchArm::Base;
    let mut executor = AdventureExecutor::new(
        base_observer,
        generated_observer,
        ConfiguredDetector {
            enabled: detector_enabled,
            detector,
        },
        executor_mode,
    );
    let seed_id = fuzzer.add_input(
        &mut state,
        &mut executor,
        &mut manager,
        AdventureInput::default(),
    )?;
    executor.pin_last_input();
    let triage_events = Rc::new(RefCell::new(Vec::new()));
    let mut recording_triager = RecordingTriager::new(
        triager,
        Rc::clone(&triage_events),
        max_triage_calls,
        continue_on_triage_failure,
    );
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
        testcase.add_metadata(recording_triager.triage(&seed_request)?);
    }

    let macro_stats = Rc::new(RefCell::new(MutatorStats::default()));
    let generated = GeneratedMutatorAdapter::new(
        generated_mutator,
        "fetch-key-then-open-door",
        DEFAULT_MACRO_RETIRE_AFTER,
        Rc::clone(&macro_stats),
    );
    let mutator = AdventureCampaignMutator::new(
        search == SearchArm::DetectorsAndMacros,
        generated,
        Box::new(recording_triager),
    );
    let mut stages = tuple_list!(StdMutationalStage::with_max_iterations(
        mutator,
        NonZeroUsize::MIN,
    ));

    let mut time_to_target = None;
    while *state.executions() < execution_budget {
        let corpus_count = state.corpus().count();
        fuzzer.fuzz_one(&mut stages, &mut executor, &mut state, &mut manager)?;
        if state.corpus().count() > corpus_count {
            executor.pin_last_input();
        }
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
    let recorded_events = triage_events.borrow().clone();
    let triage_failures = u64::try_from(
        recorded_events
            .iter()
            .filter(|event| event.failure.is_some())
            .count(),
    )?;
    let report = AdventureRunReport {
        seed,
        triage,
        search,
        execution_budget,
        executions: *state.executions(),
        time_to_target,
        maximum_progress,
        corpus,
        macro_stats: macro_stats.borrow().clone(),
        triage_events: recorded_events,
        triage_failures,
        executor_mode,
        executor_work: executor.work(),
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
        let labels = testcase
            .metadata_map()
            .get::<TriageLabels>()
            .ok_or("adventure testcase has no triage labels")?
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
            labels,
            input,
        });
    }
    Ok((entries, maximum))
}

/// Exhaustively prove that append-only search cannot leave a recorded base
/// corpus using only the room map.
pub fn prove_adventure_base_plateau(
    report: &AdventureRunReport,
) -> Result<AdventurePlateauProof, Box<dyn Error>> {
    if report.search != SearchArm::Base {
        return Err("adventure plateau proof requires a base-map report".into());
    }
    if report.time_to_target.is_some() {
        return Err("adventure plateau proof received a target-reaching report".into());
    }

    let mut retained_rooms = BTreeSet::new();
    for entry in &report.corpus {
        retained_rooms.extend(
            run_adventure(&entry.input)
                .into_iter()
                .map(|observation| room_index(observation.room)),
        );
    }

    let mut checked_children = 0_usize;
    let mut child_can_add_base_novelty = false;
    let mut child_can_reach_target = false;
    for entry in &report.corpus {
        for action in ALL_ADVENTURE_ACTIONS {
            checked_children = checked_children.saturating_add(1);
            let mut child = entry.input.clone();
            child.actions.push(action);
            let observations = run_adventure(&child);
            child_can_add_base_novelty |= observations
                .iter()
                .map(|observation| room_index(observation.room))
                .any(|room| !retained_rooms.contains(&room));
            child_can_reach_target |= observations
                .last()
                .is_some_and(|observation| observation.target);
        }
    }

    Ok(AdventurePlateauProof {
        retained_rooms: retained_rooms.into_iter().collect(),
        checked_children,
        child_can_add_base_novelty,
        child_can_reach_target,
    })
}

/// Exercise an installed detector twice on every recorded plateau testcase.
/// A generated process that panics fails before a campaign restart.
pub fn verify_installed_adventure_detector<D>(
    detector: &D,
    plateau_report: &Path,
) -> Result<(), Box<dyn Error>>
where
    D: AdventureDetector,
{
    let report: AdventureRunReport = serde_json::from_slice(&fs::read(plateau_report)?)?;
    let proof = prove_adventure_base_plateau(&report)?;
    if proof.child_can_add_base_novelty || proof.child_can_reach_target {
        return Err("recorded adventure corpus is not a closed base plateau".into());
    }
    for event in &report.triage_events {
        let first = detector.features(&event.request.observations);
        let second = detector.features(&event.request.observations);
        if first != second {
            return Err(format!(
                "installed detector is nondeterministic on testcase {}",
                event.request.testcase_id
            )
            .into());
        }
    }
    Ok(())
}

/// Restore a labeled base plateau, add one generated detector map, and resume
/// append-only search in a separately built process.
pub fn run_installed_adventure_detector<D>(
    plateau_report: &Path,
    output_dir: &Path,
    seed: u64,
    execution_budget: u64,
    scope_to_lineage: Option<u64>,
    detector: D,
) -> Result<InstalledAdventureDetectorReport, Box<dyn Error>>
where
    D: AdventureDetector,
{
    let plateau: AdventureRunReport = serde_json::from_slice(&fs::read(plateau_report)?)?;
    let proof = prove_adventure_base_plateau(&plateau)?;
    if proof.child_can_add_base_novelty || proof.child_can_reach_target {
        return Err("installed detector restart requires a closed base plateau".into());
    }
    if plateau.corpus.is_empty() {
        return Err("installed detector restart requires a nonempty corpus".into());
    }
    if let Some(scope) = scope_to_lineage
        && usize::try_from(scope)? >= plateau.corpus.len()
    {
        return Err(format!("lineage scope {scope} is absent from plateau corpus").into());
    }
    fs::create_dir_all(output_dir)?;

    let base_observer =
        StdMapObserver::owned("phase4a_installed_base", vec![0_u8; ADVENTURE_MAP_SIZE]);
    let generated_observer = StdMapObserver::owned(
        "phase4a_installed_generated",
        vec![0_u8; ADVENTURE_MAP_SIZE],
    );
    let base_feedback = MaxMapFeedback::new(&base_observer);
    let roots = [CorpusId(usize::try_from(scope_to_lineage.unwrap_or(0))?)];
    let generated_feedback = ScopedFeedback::new(MaxMapFeedback::new(&generated_observer), roots);
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
    let mut executor = AdventureExecutor::new(
        base_observer,
        generated_observer,
        detector,
        AdventureExecutorMode::SnapshotResume,
    );

    for entry in &plateau.corpus {
        *state.corpus_mut().current_mut() =
            entry.parent.map(usize::try_from).transpose()?.map(CorpusId);
        let id = fuzzer.add_input(&mut state, &mut executor, &mut manager, entry.input.clone())?;
        executor.pin_last_input();
        if u64::try_from(id.0)? != entry.id {
            return Err("persisted adventure corpus ids changed during restart".into());
        }
        let mut updated = state.corpus().get(id)?.borrow().clone();
        updated.add_metadata(ProducerMetadata {
            mutator: entry.producer.clone(),
        });
        updated.add_metadata(entry.labels.clone());
        let _previous = state.corpus_mut().replace(id, updated)?;
    }
    *state.corpus_mut().current_mut() = None;
    *state.executions_mut() = plateau.executions;
    let starting_executions = plateau.executions;
    let starting_corpus_count = state.corpus().count();

    let macro_stats = Rc::new(RefCell::new(MutatorStats::default()));
    let disabled_macro = GeneratedMutatorAdapter::new(
        FetchKeyThenOpenDoor,
        "disabled-during-detector-trial",
        DEFAULT_MACRO_RETIRE_AFTER,
        macro_stats,
    );
    let mutator = AdventureCampaignMutator::new(false, disabled_macro, Box::new(NullTriager));
    let mut stages = tuple_list!(StdMutationalStage::with_max_iterations(
        mutator,
        NonZeroUsize::MIN,
    ));
    let mut detector_stats = AdventureDetectorStats::default();
    let mut time_to_target = None;
    let limit = starting_executions.saturating_add(execution_budget);
    while *state.executions() < limit {
        let prior_count = state.corpus().count();
        let prior_base_features = adventure_corpus_base_features(&state)?;
        fuzzer.fuzz_one(&mut stages, &mut executor, &mut state, &mut manager)?;
        let corpus_added = state.corpus().count() > prior_count;
        if corpus_added {
            executor.pin_last_input();
        }
        let adds_base_feature = executor
            .last
            .iter()
            .any(|observation| !prior_base_features.contains(&room_index(observation.room)));
        detector_stats.record(
            corpus_added && !adds_base_feature,
            DEFAULT_DETECTOR_RETIRE_AFTER,
        );
        if !detector_stats.active {
            fuzzer.feedback_mut().second.retire();
        }
        if executor
            .last
            .last()
            .is_some_and(|observation| observation.target)
        {
            time_to_target = Some(state.executions().saturating_sub(starting_executions));
            break;
        }
    }

    let (corpus, maximum_progress) = semantic_adventure_corpus(&state)?;
    let report = InstalledAdventureDetectorReport {
        seed,
        execution_budget,
        starting_executions,
        invocation_executions: state.executions().saturating_sub(starting_executions),
        time_to_target,
        maximum_progress,
        starting_corpus_count,
        scope_to_lineage,
        corpus,
        detector: detector_stats,
    };
    fs::write(
        output_dir.join(INSTALLED_DETECTOR_REPORT_FILE),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(report)
}

fn adventure_corpus_base_features(state: &AdventureState) -> Result<BTreeSet<u64>, Box<dyn Error>> {
    let mut features = BTreeSet::new();
    for id in state.corpus().ids() {
        let input = state.corpus().cloned_input_for_id(id)?;
        features.extend(
            run_adventure(&input)
                .into_iter()
                .map(|observation| room_index(observation.room)),
        );
    }
    Ok(features)
}

/// Run one live Luna-triaged adventure campaign through the subprocess seam.
pub fn run_luna_adventure_campaign(
    output_dir: &Path,
    seed: u64,
    execution_budget: u64,
    search: SearchArm,
    triage_program: &Path,
    extra_triage_args: &[OsString],
) -> Result<AdventureRunReport, Box<dyn Error>> {
    let operator_view = prepare_adventure_operator_view(output_dir, execution_budget)?;
    let mut args = vec![
        OsString::from("--operator-view"),
        operator_view.as_os_str().to_owned(),
        OsString::from("--records-dir"),
        output_dir.join("model-interactions").into_os_string(),
    ];
    args.extend_from_slice(extra_triage_args);
    let subprocess = SubprocessTriager::new(triage_program.to_path_buf(), args);
    let operator_triager = OperatorViewTriager::new(Box::new(subprocess), operator_view);
    run_adventure_campaign_with_triager(
        output_dir,
        seed,
        execution_budget,
        TriageArm::Luna,
        search,
        InventoryDoorDetector,
        FetchKeyThenOpenDoor,
        Box::new(operator_triager),
        MAX_TRIAGE_CALLS,
        true,
        AdventureExecutorMode::SnapshotResume,
    )
}

/// Replay a recorded Luna campaign without invoking a model process.
pub fn replay_recorded_adventure_campaign(
    output_dir: &Path,
    recorded: &AdventureRunReport,
) -> Result<AdventureRunReport, Box<dyn Error>> {
    if recorded.triage != TriageArm::Luna {
        return Err("recorded replay requires a Luna campaign report".into());
    }
    let cursor = Rc::new(RefCell::new(0_usize));
    let replay = ReplayTriager::new(&recorded.triage_events, Rc::clone(&cursor));
    let report = run_adventure_campaign_with_triager(
        output_dir,
        recorded.seed,
        recorded.execution_budget,
        TriageArm::Luna,
        recorded.search,
        InventoryDoorDetector,
        FetchKeyThenOpenDoor,
        Box::new(replay),
        u64::MAX,
        false,
        recorded.executor_mode,
    )?;
    if *cursor.borrow() != recorded.triage_events.len() {
        return Err(format!(
            "replay consumed {} of {} recorded triage events",
            *cursor.borrow(),
            recorded.triage_events.len()
        )
        .into());
    }
    Ok(report)
}

fn prepare_adventure_operator_view(
    output_dir: &Path,
    execution_budget: u64,
) -> Result<PathBuf, Box<dyn Error>> {
    fs::create_dir_all(output_dir)?;
    let operator_view = output_dir.join("operator-view");
    fs::create_dir(&operator_view)?;
    fs::create_dir(operator_view.join("corpus"))?;
    fs::write(
        operator_view.join("input-vocabulary.txt"),
        "Inputs are bounded ordered lists of seven total enum actions. An inapplicable action is a deterministic no-op. Individual input actions are not included in a triage request.\n",
    )?;
    fs::write(
        operator_view.join("observation-format.txt"),
        "Each action boundary reports room (Start, Key, Door, Treasure, or Hazard), has_key, door_open, target, and crashed. The log is a compact mechanical rendering of the final observation. Field meaning for scheduling or progress is intentionally unspecified.\n",
    )?;
    fs::write(
        operator_view.join("fuzzer_stats"),
        format!(
            "target : adventure-toy\nexecution_budget : {execution_budget}\nretained_testcases : 0\n"
        ),
    )?;
    fs::write(
        operator_view.join("plot_data"),
        "# deterministic campaign: no wall-clock columns\n# retained_testcases,last_testcase_id\n",
    )?;
    Ok(operator_view)
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
        AdventureExecutorMode::SnapshotResume,
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
        TriageArm::Luna => {
            return Err("generated artifact restart does not configure Luna triage".into());
        }
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
        AdventureDetectorStats, AdventureExecutionWork, AdventureExecutorMode, AdventureInput,
        FetchKeyThenOpenDoor, GeneratedMutator, GeneratedMutatorAdapter, InventoryDoorDetector,
        MAX_ADVENTURE_ACTIONS, MutatorStats, ProducerMetadata, SearchArm, TriageArm, TriageRequest,
        Triager, install_build_restart_adventure, labels_for_request, prove_adventure_base_plateau,
        replay_recorded_adventure_campaign, run_adventure_campaign,
        run_adventure_campaign_with_executor, run_adventure_campaign_with_triager,
        run_adventure_matrix, run_installed_adventure_detector,
    };
    use crate::phase2::TriageLabels;
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
    fn legacy_and_snapshot_resume_have_identical_semantics() {
        let legacy_dir = tempfile::tempdir().expect("legacy campaign directory");
        let snapshot_dir = tempfile::tempdir().expect("snapshot campaign directory");
        let mut legacy = run_adventure_campaign_with_executor(
            legacy_dir.path(),
            0x5eed_ee01,
            5_000,
            TriageArm::Null,
            SearchArm::Base,
            AdventureExecutorMode::Legacy,
        )
        .expect("legacy adventure campaign");
        let mut snapshot = run_adventure_campaign_with_executor(
            snapshot_dir.path(),
            0x5eed_ee01,
            5_000,
            TriageArm::Null,
            SearchArm::Base,
            AdventureExecutorMode::SnapshotResume,
        )
        .expect("snapshot adventure campaign");
        legacy.executor_mode = AdventureExecutorMode::SnapshotResume;
        legacy.executor_work = AdventureExecutionWork::default();
        snapshot.executor_work = AdventureExecutionWork::default();
        assert_eq!(legacy, snapshot);
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

    struct FixtureTriager;

    impl Triager for FixtureTriager {
        fn triage(&mut self, request: &TriageRequest) -> Result<TriageLabels, libafl::Error> {
            Ok(labels_for_request(request, true))
        }
    }

    #[test]
    fn recorded_luna_labels_replay_the_semantic_corpus_without_a_model() {
        let live_dir = tempfile::tempdir().expect("recorded campaign directory");
        let replay_dir = tempfile::tempdir().expect("replay campaign directory");
        let recorded = run_adventure_campaign_with_triager(
            live_dir.path(),
            0x4a00_3000,
            5_000,
            TriageArm::Luna,
            SearchArm::GeneratedDetectors,
            InventoryDoorDetector,
            FetchKeyThenOpenDoor,
            Box::new(FixtureTriager),
            200,
            false,
            super::AdventureExecutorMode::SnapshotResume,
        )
        .expect("record fixture-label campaign");
        let replayed = replay_recorded_adventure_campaign(replay_dir.path(), &recorded)
            .expect("replay without model");
        assert_eq!(recorded.corpus, replayed.corpus);
        assert_eq!(recorded.executions, replayed.executions);
        assert_eq!(recorded.time_to_target, replayed.time_to_target);
        assert_eq!(recorded.triage_events.len(), replayed.triage_events.len());
    }

    #[test]
    fn persisted_adventure_plateau_resumes_with_detector_and_replays() {
        let root = tempfile::tempdir().expect("create installed-detector root");
        let plateau_dir = root.path().join("plateau");
        let plateau = run_adventure_campaign(
            &plateau_dir,
            0x5eed_d4ff,
            10_000,
            TriageArm::Null,
            SearchArm::Base,
        )
        .expect("run base plateau");
        let proof = prove_adventure_base_plateau(&plateau).expect("prove base plateau");
        assert!(!proof.child_can_add_base_novelty);
        assert!(!proof.child_can_reach_target);

        let plateau_report = plateau_dir.join("phase4a-run-report.json");
        let installed = run_installed_adventure_detector(
            &plateau_report,
            &root.path().join("installed"),
            0x5eed_d500,
            10_000,
            None,
            InventoryDoorDetector,
        )
        .expect("resume with detector");
        let replay = run_installed_adventure_detector(
            &plateau_report,
            &root.path().join("replay"),
            0x5eed_d500,
            10_000,
            None,
            InventoryDoorDetector,
        )
        .expect("replay installed detector");
        assert!(installed.time_to_target.is_some());
        assert!(installed.detector.novelties > 0);
        assert_eq!(installed, replay);
    }

    #[test]
    fn installed_detector_retirement_is_execution_counted() {
        let mut stats = AdventureDetectorStats::default();
        stats.record(true, 3);
        stats.record(false, 3);
        stats.record(false, 3);
        assert!(stats.active);
        stats.record(false, 3);
        assert!(!stats.active);
        assert_eq!(stats.novelties, 1);
        stats.record(true, 3);
        assert_eq!(stats.novelties, 1, "retired detector remains retired");
    }

    struct FailingTriager;

    impl Triager for FailingTriager {
        fn triage(&mut self, _request: &TriageRequest) -> Result<TriageLabels, libafl::Error> {
            Err(libafl::Error::illegal_state("fixture triage failure"))
        }
    }

    #[test]
    fn failed_model_calls_fall_back_and_the_campaign_continues() {
        let output = tempfile::tempdir().expect("failed-call campaign directory");
        let report = run_adventure_campaign_with_triager(
            output.path(),
            0x4a00_3001,
            5_000,
            TriageArm::Luna,
            SearchArm::GeneratedDetectors,
            InventoryDoorDetector,
            FetchKeyThenOpenDoor,
            Box::new(FailingTriager),
            200,
            true,
            super::AdventureExecutorMode::SnapshotResume,
        )
        .expect("campaign continues after triage failures");
        assert!(report.triage_failures > 0);
        assert_eq!(report.triage_failures as usize, report.triage_events.len());
        assert!(report.time_to_target.is_some());
    }
}
