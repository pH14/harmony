// SPDX-License-Identifier: AGPL-3.0-or-later

//! Phase 3: scoped generated detectors and rebuild/restart/resume.

use std::{
    borrow::Cow, collections::BTreeSet, error::Error, fs, num::NonZeroUsize, path::Path,
    process::Command,
};

use libafl::{
    Error as LibAflError, StdFuzzer,
    corpus::{Corpus, CorpusId, InMemoryCorpus, InMemoryOnDiskCorpus, Testcase},
    events::NopEventManager,
    executors::{Executor, ExitKind, HasObservers},
    feedbacks::{ConstFeedback, EagerOrFeedback, Feedback, MaxMapFeedback, StateInitializer},
    fuzzer::{Evaluator, Fuzzer, HasFeedback},
    inputs::Input,
    mutators::{MutationResult, Mutator},
    observers::StdMapObserver,
    schedulers::QueueScheduler,
    stages::StdMutationalStage,
    state::{HasCorpus, HasExecutions, HasRand, HasSolutions, StdState},
};
use libafl_bolts::{
    HasLen, Named,
    rands::{Rand, StdRand},
    tuples::{RefIndexable, tuple_list},
};
use serde::{Deserialize, Serialize};

use crate::phase2::{Flag, Interest, TriageLabels};

const PHASE3_MAP_SIZE: usize = 64;
const PHASE3_MAP_SIZE_U64: u64 = 64;
const STATE_FILE: &str = "phase3-state.postcard";
const CONFIG_FILE: &str = "phase3-scope.json";
const REPORT_FILE: &str = "phase3-resume-report.json";
const RETIRE_AFTER_EXECUTIONS: u64 = 10_000;

/// Phase 3 input action.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum BlindAction {
    /// Advance from genesis to the corridor.
    Enter,
    /// Advance from the corridor to the key room.
    ApproachKey,
    /// Pick up the key without changing the base-visible position.
    TakeKey,
    /// Open the door when carrying the key.
    OpenDoor,
    /// Advance beyond the door to the target.
    Finish,
    /// A total no-op action.
    Wait,
}

impl BlindAction {
    const ALL: [Self; 6] = [
        Self::Enter,
        Self::ApproachKey,
        Self::TakeKey,
        Self::OpenDoor,
        Self::Finish,
        Self::Wait,
    ];
}

/// Append-only input used to make the blind-spot proof mechanical.
#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct BlindInput {
    /// Actions applied from genesis.
    pub actions: Vec<BlindAction>,
}

impl Input for BlindInput {}

impl HasLen for BlindInput {
    fn len(&self) -> usize {
        self.actions.len()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct BlindState {
    position: u8,
    has_key: bool,
    target: bool,
}

impl BlindState {
    const GENESIS: Self = Self {
        position: 0,
        has_key: false,
        target: false,
    };

    fn apply(self, action: BlindAction) -> Option<Self> {
        match (self.position, self.has_key, action) {
            (0, false, BlindAction::Enter) => Some(Self {
                position: 1,
                ..self
            }),
            (1, false, BlindAction::ApproachKey) => Some(Self {
                position: 2,
                ..self
            }),
            (2, false, BlindAction::TakeKey) => Some(Self {
                has_key: true,
                ..self
            }),
            (2, true, BlindAction::OpenDoor) => Some(Self {
                position: 3,
                ..self
            }),
            (3, true, BlindAction::Finish) => Some(Self {
                position: 4,
                target: true,
                ..self
            }),
            (_, _, BlindAction::Wait) => Some(self),
            _ => None,
        }
    }
}

/// Observations exposed to generated detector source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunObservations {
    /// Coarse base-visible position.
    pub position: u8,
    /// Inventory distinction hidden from the base map.
    pub has_key: bool,
    /// Whether the run reached the target.
    pub target: bool,
    /// Deterministic run log.
    pub log: String,
}

/// One generated feature-map key.
pub type FeatureKey = u64;

/// The complete generated detector surface.
pub trait GeneratedDetector {
    /// Map observations to deterministic feature keys.
    fn features(&self, run: &RunObservations) -> Vec<FeatureKey>;
}

#[derive(Clone, Copy, Debug)]
struct NoDetector;

impl GeneratedDetector for NoDetector {
    fn features(&self, _run: &RunObservations) -> Vec<FeatureKey> {
        Vec::new()
    }
}

/// Hand-written detector used by unit tests; the install loop emits equivalent source.
#[derive(Clone, Copy, Debug)]
pub struct KeyInventoryDetector;

impl GeneratedDetector for KeyInventoryDetector {
    fn features(&self, run: &RunObservations) -> Vec<FeatureKey> {
        if run.has_key {
            vec![0x100 + u64::from(run.position)]
        } else {
            Vec::new()
        }
    }
}

fn execute_blind(input: &BlindInput) -> (Vec<BlindState>, RunObservations) {
    let mut state = BlindState::GENESIS;
    let mut visited = vec![state];
    for action in &input.actions {
        let Some(next) = state.apply(*action) else {
            break;
        };
        state = next;
        visited.push(state);
        if state.target {
            break;
        }
    }
    let observations = RunObservations {
        position: state.position,
        has_key: state.has_key,
        target: state.target,
        log: format!(
            "position={} inventory_key={} target={}",
            state.position, state.has_key, state.target
        ),
    };
    (visited, observations)
}

#[derive(Debug)]
struct AppendBlindActionMutator {
    name: Cow<'static, str>,
}

impl Default for AppendBlindActionMutator {
    fn default() -> Self {
        Self {
            name: Cow::Borrowed("AppendBlindActionMutator"),
        }
    }
}

impl Named for AppendBlindActionMutator {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<S> Mutator<BlindInput, S> for AppendBlindActionMutator
where
    S: HasRand,
{
    fn mutate(
        &mut self,
        state: &mut S,
        input: &mut BlindInput,
    ) -> Result<MutationResult, LibAflError> {
        if input.actions.len() >= 8 {
            return Ok(MutationResult::Skipped);
        }
        let index = state.rand_mut().below(
            NonZeroUsize::new(BlindAction::ALL.len()).expect("action vocabulary is nonempty"),
        );
        input.actions.push(BlindAction::ALL[index]);
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

/// Lineage-gating wrapper for generated detector feedback.
#[derive(Debug)]
pub struct ScopedFeedback<F> {
    inner: F,
    roots: BTreeSet<CorpusId>,
    name: Cow<'static, str>,
    last_in_scope: bool,
}

impl<F> ScopedFeedback<F>
where
    F: Named,
{
    /// Restrict `inner` to descendants of the listed corpus roots.
    #[must_use]
    pub fn new(inner: F, roots: impl IntoIterator<Item = CorpusId>) -> Self {
        Self {
            name: Cow::Owned(format!("Scoped({})", inner.name())),
            inner,
            roots: roots.into_iter().collect(),
            last_in_scope: false,
        }
    }

    fn lineage_matches<I, S>(&self, state: &S) -> Result<bool, LibAflError>
    where
        S: HasCorpus<I>,
    {
        let mut current = *state.corpus().current();
        for _ in 0..=state.corpus().count() {
            let Some(id) = current else {
                return Ok(false);
            };
            if self.roots.contains(&id) {
                return Ok(true);
            }
            current = state.corpus().get(id)?.borrow().parent_id();
        }
        Err(LibAflError::illegal_state(
            "cycle in testcase parent lineage",
        ))
    }

    fn retire(&mut self) {
        self.roots.clear();
    }
}

impl<F> Named for ScopedFeedback<F> {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<F, S> StateInitializer<S> for ScopedFeedback<F>
where
    F: StateInitializer<S>,
{
    fn init_state(&mut self, state: &mut S) -> Result<(), LibAflError> {
        self.inner.init_state(state)
    }
}

impl<EM, F, I, OT, S> Feedback<EM, I, OT, S> for ScopedFeedback<F>
where
    F: Feedback<EM, I, OT, S>,
    S: HasCorpus<I>,
{
    fn is_interesting(
        &mut self,
        state: &mut S,
        manager: &mut EM,
        input: &I,
        observers: &OT,
        exit_kind: &ExitKind,
    ) -> Result<bool, LibAflError> {
        self.last_in_scope = self.lineage_matches(state)?;
        if self.last_in_scope {
            self.inner
                .is_interesting(state, manager, input, observers, exit_kind)
        } else {
            Ok(false)
        }
    }

    fn append_metadata(
        &mut self,
        state: &mut S,
        manager: &mut EM,
        observers: &OT,
        testcase: &mut Testcase<I>,
    ) -> Result<(), LibAflError> {
        if self.last_in_scope {
            self.inner
                .append_metadata(state, manager, observers, testcase)?;
        }
        Ok(())
    }
}

type BlindObserver = StdMapObserver<'static, u8, false>;
type BlindObservers = (BlindObserver, (BlindObserver, ()));

#[derive(Debug)]
struct BlindExecutor<D> {
    observers: BlindObservers,
    detector: D,
    last: RunObservations,
}

impl<D> BlindExecutor<D> {
    fn new(base: BlindObserver, generated: BlindObserver, detector: D) -> Self {
        Self {
            observers: tuple_list!(base, generated),
            detector,
            last: RunObservations {
                position: 0,
                has_key: false,
                target: false,
                log: String::new(),
            },
        }
    }
}

impl<D> HasObservers for BlindExecutor<D> {
    type Observers = BlindObservers;

    fn observers(&self) -> RefIndexable<&Self::Observers, Self::Observers> {
        RefIndexable::from(&self.observers)
    }

    fn observers_mut(&mut self) -> RefIndexable<&mut Self::Observers, Self::Observers> {
        RefIndexable::from(&mut self.observers)
    }
}

impl<D, EM, S, Z> Executor<EM, BlindInput, S, Z> for BlindExecutor<D>
where
    D: GeneratedDetector,
    S: HasExecutions,
{
    fn run_target(
        &mut self,
        _fuzzer: &mut Z,
        state: &mut S,
        _manager: &mut EM,
        input: &BlindInput,
    ) -> Result<ExitKind, LibAflError> {
        *state.executions_mut() = state.executions().saturating_add(1);
        let (visited, observations) = execute_blind(input);
        for state in visited {
            self.observers.0[usize::from(state.position) % PHASE3_MAP_SIZE] = 1;
        }
        for feature in self.detector.features(&observations) {
            let index = usize::try_from(feature % PHASE3_MAP_SIZE_U64)
                .map_err(|_| LibAflError::illegal_state("detector feature index overflow"))?;
            self.observers.1.0[index] = 1;
        }
        self.last = observations;
        Ok(ExitKind::Ok)
    }
}

type Phase3Corpus = InMemoryOnDiskCorpus<BlindInput>;
type Phase3State = StdState<Phase3Corpus, BlindInput, StdRand, InMemoryCorpus<BlindInput>>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Phase3Config {
    scope_root: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CheckpointEntry {
    input: BlindInput,
    parent: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Phase3Checkpoint {
    rand: StdRand,
    executions: u64,
    current: Option<u64>,
    entries: Vec<CheckpointEntry>,
}

/// Exhaustive proof summary for the append-only baseline plateau.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlateauProof {
    /// Base positions retained by the corpus.
    pub retained_positions: Vec<u8>,
    /// Number of retained-entry/action children checked exhaustively.
    pub checked_children: usize,
    /// Whether any child can add a base-visible position.
    pub child_can_add_base_novelty: bool,
    /// Whether any child can reach the target.
    pub child_can_reach_target: bool,
}

/// Per-detector deterministic novelty and retirement accounting.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DetectorStats {
    /// Novel corpus entries attributable to this detector.
    pub novelties: u64,
    /// Executions since the detector most recently produced novelty.
    pub executions_without_novelty: u64,
    /// Whether feedback remains installed.
    pub active: bool,
}

impl Default for DetectorStats {
    fn default() -> Self {
        Self {
            novelties: 0,
            executions_without_novelty: 0,
            active: true,
        }
    }
}

impl DetectorStats {
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

/// Phase 3 run report, serialized across the installed process boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Phase3Report {
    /// Total target executions in persisted state.
    pub executions: u64,
    /// Executions performed by this invocation.
    pub invocation_executions: u64,
    /// Whether the target was reached.
    pub target_reached: bool,
    /// Maximum base-visible position represented in the corpus.
    pub maximum_position: u8,
    /// Number of persisted corpus inputs.
    pub corpus_count: usize,
    /// Baseline plateau proof when this is the baseline invocation.
    pub plateau: Option<PlateauProof>,
    /// Generated detector accounting.
    pub detector: DetectorStats,
}

/// Run and persist the detector-free baseline until it reaches a closed plateau.
pub fn run_blind_baseline(
    output_dir: &Path,
    seed: u64,
    execution_budget: u64,
) -> Result<Phase3Report, Box<dyn Error>> {
    fs::create_dir_all(output_dir)?;
    let corpus_dir = output_dir.join("corpus");
    let base_observer = StdMapObserver::owned("phase3_base", vec![0_u8; PHASE3_MAP_SIZE]);
    let generated_observer = StdMapObserver::owned("phase3_generated", vec![0_u8; PHASE3_MAP_SIZE]);
    let base_feedback = MaxMapFeedback::new(&base_observer);
    let generated_feedback = MaxMapFeedback::new(&generated_observer);
    let mut feedback = EagerOrFeedback::new(
        base_feedback,
        ScopedFeedback::new(generated_feedback, std::iter::empty()),
    );
    let mut objective = ConstFeedback::new(false);
    let mut state = StdState::new(
        StdRand::with_seed(seed),
        InMemoryOnDiskCorpus::new(&corpus_dir)?,
        InMemoryCorpus::<BlindInput>::new(),
        &mut feedback,
        &mut objective,
    )?;
    let mut fuzzer = StdFuzzer::new(QueueScheduler::new(), feedback, objective);
    let mut manager = NopEventManager::new();
    let mut executor = BlindExecutor::new(base_observer, generated_observer, NoDetector);
    fuzzer.add_input(
        &mut state,
        &mut executor,
        &mut manager,
        BlindInput::default(),
    )?;
    let mut stages = tuple_list!(StdMutationalStage::with_max_iterations(
        AppendBlindActionMutator::default(),
        NonZeroUsize::MIN,
    ));
    while *state.executions() < execution_budget {
        fuzzer.fuzz_one(&mut stages, &mut executor, &mut state, &mut manager)?;
    }

    let (maximum_position, scope_root) = find_maximum_and_scope(&state)?;
    let plateau = prove_plateau(&state)?;
    if plateau.child_can_add_base_novelty || plateau.child_can_reach_target {
        return Err("baseline did not reach a closed append-only plateau".into());
    }
    write_operator_artifacts(output_dir, &state, &plateau)?;
    fs::write(
        output_dir.join(CONFIG_FILE),
        serde_json::to_vec_pretty(&Phase3Config {
            scope_root: u64::try_from(scope_root.0)?,
        })?,
    )?;
    save_state(output_dir, &mut state)?;

    let report = Phase3Report {
        executions: *state.executions(),
        invocation_executions: *state.executions(),
        target_reached: false,
        maximum_position,
        corpus_count: state.corpus().count(),
        plateau: Some(plateau),
        detector: DetectorStats::default(),
    };
    Ok(report)
}

/// Resume persisted state with an installed detector.
pub fn resume_with_detector<D>(
    output_dir: &Path,
    detector: D,
    execution_budget: u64,
) -> Result<Phase3Report, Box<dyn Error>>
where
    D: GeneratedDetector,
{
    let config: Phase3Config = serde_json::from_slice(&fs::read(output_dir.join(CONFIG_FILE))?)?;
    let scope_root = CorpusId(usize::try_from(config.scope_root)?);
    let checkpoint: Phase3Checkpoint =
        postcard::from_bytes(&fs::read(output_dir.join(STATE_FILE))?)?;
    let starting_executions = checkpoint.executions;

    let base_observer = StdMapObserver::owned("phase3_base", vec![0_u8; PHASE3_MAP_SIZE]);
    let generated_observer = StdMapObserver::owned("phase3_generated", vec![0_u8; PHASE3_MAP_SIZE]);
    let base_feedback = MaxMapFeedback::new(&base_observer);
    let generated_feedback = MaxMapFeedback::new(&generated_observer);
    let mut feedback = EagerOrFeedback::new(
        base_feedback,
        ScopedFeedback::new(generated_feedback, [scope_root]),
    );
    let mut objective = ConstFeedback::new(false);
    let mut state = StdState::new(
        checkpoint.rand,
        InMemoryOnDiskCorpus::new(output_dir.join("corpus"))?,
        InMemoryCorpus::<BlindInput>::new(),
        &mut feedback,
        &mut objective,
    )?;
    let mut fuzzer = StdFuzzer::new(QueueScheduler::new(), feedback, objective);
    let mut executor = BlindExecutor::new(base_observer, generated_observer, detector);
    let mut manager = NopEventManager::new();
    for entry in checkpoint.entries {
        *state.corpus_mut().current_mut() =
            entry.parent.map(usize::try_from).transpose()?.map(CorpusId);
        fuzzer.add_input(&mut state, &mut executor, &mut manager, entry.input)?;
    }
    *state.corpus_mut().current_mut() = checkpoint
        .current
        .map(usize::try_from)
        .transpose()?
        .map(CorpusId);
    *state.executions_mut() = starting_executions;
    let mut stages = tuple_list!(StdMutationalStage::with_max_iterations(
        AppendBlindActionMutator::default(),
        NonZeroUsize::MIN,
    ));
    let mut stats = DetectorStats::default();
    let mut target_reached = false;
    let limit = starting_executions.saturating_add(execution_budget);
    while *state.executions() < limit {
        let prior_count = state.corpus().count();
        let prior_maximum = maximum_position(&state)?;
        fuzzer.fuzz_one(&mut stages, &mut executor, &mut state, &mut manager)?;
        let corpus_added = state.corpus().count() > prior_count;
        let detector_novelty = corpus_added && executor.last.position <= prior_maximum;
        stats.record(detector_novelty, RETIRE_AFTER_EXECUTIONS);
        if !stats.active {
            fuzzer.feedback_mut().second.retire();
        }
        if executor.last.target {
            target_reached = true;
            break;
        }
    }
    let maximum_position = maximum_position(&state)?;
    save_state(output_dir, &mut state)?;
    Ok(Phase3Report {
        executions: *state.executions(),
        invocation_executions: state.executions().saturating_sub(starting_executions),
        target_reached,
        maximum_position,
        corpus_count: state.corpus().count(),
        plateau: None,
        detector: stats,
    })
}

/// Resume and write the report for the separately built installed process.
pub fn resume_and_write_report<D>(
    output_dir: &Path,
    detector: D,
    execution_budget: u64,
) -> Result<(), Box<dyn Error>>
where
    D: GeneratedDetector,
{
    let report = resume_with_detector(output_dir, detector, execution_budget)?;
    fs::write(
        output_dir.join(REPORT_FILE),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(())
}

/// Read operator artifacts, install detector source, build it, and restart the campaign.
pub fn install_build_restart(
    output_dir: &Path,
    build_dir: &Path,
    execution_budget: u64,
) -> Result<Phase3Report, Box<dyn Error>> {
    let stats = fs::read_to_string(output_dir.join("fuzzer_stats"))?;
    if !stats.contains("plateau_proven : true") {
        return Err("instrumentor refused to install without plateau evidence".into());
    }
    let labels_dir = output_dir.join("labels");
    let labels_mention_inventory = fs::read_dir(&labels_dir)?.try_fold(false, |found, entry| {
        let contents = fs::read_to_string(entry?.path())?;
        Ok::<_, std::io::Error>(found || contents.contains("inventory_key=false"))
    })?;
    if !labels_mention_inventory {
        return Err("instrumentor found no labeled inventory blind spot".into());
    }

    let crate_dir = build_dir.join("installed-detector");
    let source_dir = crate_dir.join("src");
    fs::create_dir_all(&source_dir)?;
    let dependency_path = env!("CARGO_MANIFEST_DIR")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    fs::write(
        crate_dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"phase3-installed\"\nversion = \"0.1.0\"\nedition = \"2024\"\nlicense = \"AGPL-3.0-or-later\"\n\n[dependencies]\nfuzzer = {{ path = \"{dependency_path}\" }}\n\n[workspace]\n"
        ),
    )?;
    fs::write(
        source_dir.join("detector.rs"),
        "// SPDX-License-Identifier: AGPL-3.0-or-later\n\nuse fuzzer::phase3::{FeatureKey, GeneratedDetector, RunObservations};\n\npub struct InstalledDetector;\n\nimpl GeneratedDetector for InstalledDetector {\n    fn features(&self, run: &RunObservations) -> Vec<FeatureKey> {\n        if run.has_key { vec![0x100 + u64::from(run.position)] } else { Vec::new() }\n    }\n}\n",
    )?;
    fs::write(
        source_dir.join("main.rs"),
        format!(
            "// SPDX-License-Identifier: AGPL-3.0-or-later\n\nmod detector;\n\nuse std::{{error::Error, path::PathBuf}};\nuse detector::InstalledDetector;\nuse fuzzer::phase3::resume_and_write_report;\n\nfn main() -> Result<(), Box<dyn Error>> {{\n    let output = PathBuf::from(std::env::args_os().nth(1).ok_or(\"missing output directory\")?);\n    resume_and_write_report(&output, InstalledDetector, {execution_budget})\n}}\n"
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
        return Err("generated detector build failed".into());
    }
    let binary = target_dir.join("debug").join(if cfg!(windows) {
        "phase3-installed.exe"
    } else {
        "phase3-installed"
    });
    let restart = Command::new(binary).arg(output_dir).status()?;
    if !restart.success() {
        return Err("restarted campaign failed".into());
    }
    Ok(serde_json::from_slice(&fs::read(
        output_dir.join(REPORT_FILE),
    )?)?)
}

fn find_maximum_and_scope(state: &Phase3State) -> Result<(u8, CorpusId), Box<dyn Error>> {
    let mut maximum = 0_u8;
    let mut scope = None;
    for id in state.corpus().ids() {
        let input = state.corpus().cloned_input_for_id(id)?;
        let (_, observation) = execute_blind(&input);
        if observation.position >= maximum {
            maximum = observation.position;
            scope = Some(id);
        }
    }
    Ok((maximum, scope.ok_or("phase 3 corpus is empty")?))
}

fn maximum_position(state: &Phase3State) -> Result<u8, Box<dyn Error>> {
    Ok(find_maximum_and_scope(state)?.0)
}

fn prove_plateau(state: &Phase3State) -> Result<PlateauProof, Box<dyn Error>> {
    let mut positions = BTreeSet::new();
    let mut inputs = Vec::new();
    for id in state.corpus().ids() {
        let input = state.corpus().cloned_input_for_id(id)?;
        let (visited, _) = execute_blind(&input);
        positions.extend(visited.into_iter().map(|state| state.position));
        inputs.push(input);
    }
    let mut child_can_add_base_novelty = false;
    let mut child_can_reach_target = false;
    let mut checked_children = 0;
    for input in inputs {
        for action in BlindAction::ALL {
            checked_children += 1;
            let mut child = input.clone();
            child.actions.push(action);
            let (visited, observation) = execute_blind(&child);
            child_can_add_base_novelty |= visited
                .iter()
                .any(|state| !positions.contains(&state.position));
            child_can_reach_target |= observation.target;
        }
    }
    Ok(PlateauProof {
        retained_positions: positions.into_iter().collect(),
        checked_children,
        child_can_add_base_novelty,
        child_can_reach_target,
    })
}

fn write_operator_artifacts(
    output_dir: &Path,
    state: &Phase3State,
    plateau: &PlateauProof,
) -> Result<(), Box<dyn Error>> {
    let labels_dir = output_dir.join("labels");
    fs::create_dir_all(&labels_dir)?;
    for id in state.corpus().ids() {
        let testcase = state.corpus().get(id)?.borrow();
        let filename = testcase
            .filename()
            .as_ref()
            .ok_or("phase 3 testcase lacks filename")?
            .clone();
        drop(testcase);
        let input = state.corpus().cloned_input_for_id(id)?;
        let (_, observation) = execute_blind(&input);
        let labels = TriageLabels {
            interest: if observation.position == 2 {
                Interest::Boost
            } else {
                Interest::Neutral
            },
            duplicate_of: None,
            flags: if observation.position == 2 {
                vec![Flag::InvariantNearMiss]
            } else {
                Vec::new()
            },
            tags: vec![format!("position-{}", observation.position)],
            summary: observation.log.clone(),
            hypotheses: if observation.position == 2 {
                vec!["base position map cannot distinguish inventory state".to_owned()]
            } else {
                Vec::new()
            },
        };
        fs::write(
            labels_dir.join(format!("{filename}.labels.json")),
            serde_json::to_vec_pretty(&labels)?,
        )?;
    }
    fs::write(
        output_dir.join("fuzzer_stats"),
        format!(
            "execs_done : {}\ncorpus_count : {}\nmaximum_position : {}\nplateau_checked_children : {}\nplateau_proven : true\n",
            state.executions(),
            state.corpus().count(),
            plateau.retained_positions.last().copied().unwrap_or(0),
            plateau.checked_children,
        ),
    )?;
    Ok(())
}

fn save_state(output_dir: &Path, state: &mut Phase3State) -> Result<(), Box<dyn Error>> {
    let mut entries = Vec::with_capacity(state.corpus().count());
    for id in state.corpus().ids() {
        entries.push(CheckpointEntry {
            input: state.corpus().cloned_input_for_id(id)?,
            parent: state
                .corpus()
                .get(id)?
                .borrow()
                .parent_id()
                .map(|id| u64::try_from(id.0))
                .transpose()?,
        });
    }
    let checkpoint = Phase3Checkpoint {
        rand: *state.rand(),
        executions: *state.executions(),
        current: state
            .corpus()
            .current()
            .map(|id| u64::try_from(id.0))
            .transpose()?,
        entries,
    };
    unload_inputs(state.corpus())?;
    unload_inputs(state.solutions())?;
    fs::write(
        output_dir.join(STATE_FILE),
        postcard::to_allocvec(&checkpoint)?,
    )?;
    Ok(())
}

fn unload_inputs<I>(corpus: &impl Corpus<I>) -> Result<(), LibAflError> {
    for id in corpus.ids() {
        *corpus.get(id)?.borrow_mut().input_mut() = None;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use libafl::{
        corpus::{Corpus, InMemoryCorpus, Testcase},
        feedbacks::ConstFeedback,
        state::{HasCorpus, StdState},
    };
    use libafl_bolts::rands::StdRand;

    use super::{
        BlindInput, DetectorStats, KeyInventoryDetector, ScopedFeedback, install_build_restart,
        resume_with_detector, run_blind_baseline,
    };

    #[test]
    fn scoped_feedback_matches_only_selected_lineage() {
        let mut corpus = InMemoryCorpus::new();
        let root = corpus
            .add(Testcase::new(BlindInput::default()))
            .expect("add root");
        let mut child = Testcase::new(BlindInput::default());
        child.set_parent_id(root);
        let child = corpus.add(child).expect("add child");
        let other = corpus
            .add(Testcase::new(BlindInput::default()))
            .expect("add unrelated root");
        let mut feedback = ConstFeedback::new(false);
        let mut objective = ConstFeedback::new(false);
        let mut state = StdState::new(
            StdRand::with_seed(1),
            corpus,
            InMemoryCorpus::<BlindInput>::new(),
            &mut feedback,
            &mut objective,
        )
        .expect("make state");
        let scoped = ScopedFeedback::new(ConstFeedback::new(true), [root]);
        *state.corpus_mut().current_mut() = Some(child);
        assert!(scoped.lineage_matches(&state).expect("child lineage"));
        *state.corpus_mut().current_mut() = Some(other);
        assert!(!scoped.lineage_matches(&state).expect("unrelated lineage"));
    }

    #[test]
    fn mechanical_retirement_is_execution_counted() {
        let mut stats = DetectorStats::default();
        stats.record(true, 3);
        stats.record(false, 3);
        stats.record(false, 3);
        assert!(stats.active);
        stats.record(false, 3);
        assert!(!stats.active);
        assert_eq!(stats.novelties, 1);
        stats.record(true, 3);
        assert_eq!(stats.novelties, 1, "retired detector stays retired");
    }

    #[test]
    fn baseline_plateaus_and_installed_detector_rescues_it() {
        let root = tempfile::tempdir().expect("create phase 3 root");
        let output = root.path().join("campaign");
        let baseline = run_blind_baseline(&output, 0x5eed_3000, 2_000).expect("run blind baseline");
        assert!(!baseline.target_reached);
        assert_eq!(baseline.maximum_position, 2);
        let proof = baseline.plateau.expect("baseline plateau proof");
        assert!(!proof.child_can_add_base_novelty);
        assert!(!proof.child_can_reach_target);

        // The in-process hand-written detector validates the same pure seam as
        // the generated source before paying for a separate Cargo build.
        let fixture = super::RunObservations {
            position: 2,
            has_key: true,
            target: false,
            log: String::new(),
        };
        assert_eq!(
            super::GeneratedDetector::features(&KeyInventoryDetector, &fixture),
            vec![0x102]
        );

        let rescued = install_build_restart(&output, &root.path().join("build"), 20_000)
            .expect("install detector and resume");
        assert!(rescued.target_reached);
        assert_eq!(rescued.maximum_position, 4);
        assert!(rescued.detector.novelties > 0);
        assert!(rescued.invocation_executions < 20_000);
    }

    #[test]
    fn persisted_state_resumes_in_process() {
        let root = tempfile::tempdir().expect("create direct-resume root");
        let output = root.path().join("campaign");
        run_blind_baseline(&output, 0x5eed_3001, 2_000).expect("run baseline");
        let rescued = resume_with_detector(&output, KeyInventoryDetector, 20_000)
            .expect("resume detector in process");
        assert!(rescued.target_reached);
    }
}
