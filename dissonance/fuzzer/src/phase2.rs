// SPDX-License-Identifier: AGPL-3.0-or-later

//! Phase 2: file-backed labels and deterministic external steering.

use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use libafl::{
    Error as LibAflError, HasMetadata, StdFuzzer,
    corpus::{Corpus, InMemoryCorpus, InMemoryOnDiskCorpus, Testcase},
    events::NopEventManager,
    feedbacks::{ConstFeedback, MaxMapFeedback},
    fuzzer::{Evaluator, Fuzzer, HasScheduler},
    mutators::SingleChoiceScheduledMutator,
    observers::StdMapObserver,
    schedulers::{RemovableScheduler, TestcaseScore, WeightedScheduler},
    stages::{Restartable, Stage, StdMutationalStage},
    state::{HasCorpus, HasExecutions, StdState},
};
use libafl_bolts::{rands::StdRand, tuples::tuple_list};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::phase1::{
    AppendDecisionMutator, CorpusEntry, DEEP_ROUTE, DecisionList, MAP_SIZE, MazeExecutor,
    MazeObserver, PerturbDecisionMutator, SpliceDecisionMutator, TruncateDecisionMutator,
    visited_depths,
};

/// The only triage field used as a scheduler multiplier.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Interest {
    /// Prefer this testcase.
    Boost,
    /// Leave this testcase at the base score.
    Neutral,
    /// Retain this testcase but spend little energy on it.
    Suppress,
}

/// Structured triage flags. Free text never controls scheduling.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Flag {
    /// The observation may indicate a bug.
    BugSuspect,
    /// The observation approaches an invariant boundary.
    InvariantNearMiss,
    /// The testcase appears unable to make progress.
    DeadEnd,
}

/// Labels written outside the fuzzer and imported by [`LoadLabelsStage`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TriageLabels {
    /// Scheduler priority hint.
    pub interest: Interest,
    /// Optional semantic duplicate id.
    pub duplicate_of: Option<u64>,
    /// Machine-readable flags.
    pub flags: Vec<Flag>,
    /// Free-text tags for the instrumentor.
    pub tags: Vec<String>,
    /// One-line human summary.
    pub summary: String,
    /// Free-text hypotheses for the instrumentor.
    pub hypotheses: Vec<String>,
}

impl TriageLabels {
    fn for_progress(progress: usize, maximum: usize, scripted: bool) -> Self {
        let interest = if !scripted {
            Interest::Neutral
        } else if progress == maximum && progress > 0 {
            Interest::Boost
        } else {
            Interest::Suppress
        };
        Self {
            interest,
            duplicate_of: None,
            flags: if progress == 0 {
                vec![Flag::DeadEnd]
            } else {
                Vec::new()
            },
            tags: vec![format!("maze-depth-{progress}")],
            summary: format!("reached maze depth {progress}"),
            hypotheses: if scripted && progress == maximum {
                vec!["extend the deepest retained testcase".to_owned()]
            } else {
                Vec::new()
            },
        }
    }
}

libafl_bolts::impl_serdeany!(TriageLabels);

/// Score testcase metadata for LibAFL's stock weighted scheduler.
#[derive(Clone, Copy, Debug)]
pub struct TriageScore;

impl<I, S> TestcaseScore<I, S> for TriageScore {
    fn compute(_state: &S, entry: &mut Testcase<I>) -> Result<f64, LibAflError> {
        let Some(labels) = entry.metadata_map().get::<TriageLabels>() else {
            return Ok(1.0);
        };
        if labels.duplicate_of.is_some() {
            return Ok(0.01);
        }
        Ok(match labels.interest {
            Interest::Boost => 256.0,
            Interest::Neutral => 1.0,
            Interest::Suppress => 0.01,
        })
    }
}

type Phase2Corpus = InMemoryOnDiskCorpus<DecisionList>;
type Phase2State = StdState<Phase2Corpus, DecisionList, StdRand, InMemoryCorpus<DecisionList>>;

/// Periodic stage that imports changed label sidecars into testcase metadata.
#[derive(Clone, Debug)]
pub struct LoadLabelsStage {
    labels_dir: PathBuf,
    seen: BTreeMap<String, Vec<u8>>,
}

impl LoadLabelsStage {
    /// Construct a stage reading separate label files from `labels_dir`.
    #[must_use]
    pub fn new(labels_dir: PathBuf) -> Self {
        Self {
            labels_dir,
            seen: BTreeMap::new(),
        }
    }

    /// Number of distinct sidecar filenames loaded by this stage instance.
    #[must_use]
    pub fn loaded_files(&self) -> usize {
        self.seen.len()
    }
}

impl<E, EM, Z> Stage<E, EM, Phase2State, Z> for LoadLabelsStage
where
    Z: HasScheduler<DecisionList, Phase2State>,
    Z::Scheduler: RemovableScheduler<DecisionList, Phase2State>,
{
    fn perform(
        &mut self,
        fuzzer: &mut Z,
        _executor: &mut E,
        state: &mut Phase2State,
        _manager: &mut EM,
    ) -> Result<(), LibAflError> {
        let ids: Vec<_> = state.corpus().ids().collect();
        for id in ids {
            let filename = state
                .corpus()
                .get(id)?
                .borrow()
                .filename()
                .clone()
                .ok_or_else(|| LibAflError::illegal_state("on-disk testcase has no filename"))?;
            let label_path = self.labels_dir.join(format!("{filename}.labels.json"));
            let bytes = match fs::read(&label_path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if self.seen.get(&filename) == Some(&bytes) {
                continue;
            }
            let labels: TriageLabels = serde_json::from_slice(&bytes)
                .map_err(|error| LibAflError::serialize(error.to_string()))?;

            let mut updated = state.corpus().get(id)?.borrow().clone();
            state.corpus().load_input_into(&mut updated)?;
            updated.add_metadata(labels);
            let previous = state.corpus_mut().replace(id, updated)?;
            fuzzer.scheduler_mut().on_replace(state, id, &previous)?;
            self.seen.insert(filename, bytes);
        }
        Ok(())
    }
}

impl Restartable<Phase2State> for LoadLabelsStage {
    fn should_restart(&mut self, _state: &mut Phase2State) -> Result<bool, LibAflError> {
        Ok(true)
    }

    fn clear_progress(&mut self, _state: &mut Phase2State) -> Result<(), LibAflError> {
        Ok(())
    }
}

/// One deterministic external label write, suitable for replay without a triager.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LabelEvent {
    /// Completed campaign round after which the write becomes visible.
    pub round: u64,
    /// Corpus filename addressed by this label.
    pub filename: String,
    /// Exact label content.
    pub labels: TriageLabels,
}

/// Summary of one Phase 2 campaign.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Phase2Report {
    /// Target executions including the seed execution.
    pub executions: u64,
    /// Target execution at first discovery, if reached within budget.
    pub time_to_target: Option<u64>,
    /// Semantic corpus insertion order.
    pub corpus: Vec<CorpusEntry>,
    /// Chronological external writes used by this campaign.
    pub label_events: Vec<LabelEvent>,
    /// Number of ordinary on-disk corpus inputs.
    pub persisted_inputs: usize,
}

#[derive(Clone, Copy, Debug)]
enum TriageMode<'a> {
    Null,
    Scripted,
    Replay(&'a [LabelEvent]),
}

/// Run a null-triage campaign.
pub fn run_null(
    output_dir: &Path,
    seed: u64,
    execution_budget: u64,
) -> Result<Phase2Report, Box<dyn Error>> {
    run_campaign(output_dir, seed, execution_budget, TriageMode::Null)
}

/// Run a deterministic regex-triage campaign.
pub fn run_scripted(
    output_dir: &Path,
    seed: u64,
    execution_budget: u64,
) -> Result<Phase2Report, Box<dyn Error>> {
    run_campaign(output_dir, seed, execution_budget, TriageMode::Scripted)
}

/// Replay a campaign using only previously recorded external label writes.
pub fn run_replay(
    output_dir: &Path,
    seed: u64,
    execution_budget: u64,
    events: &[LabelEvent],
) -> Result<Phase2Report, Box<dyn Error>> {
    run_campaign(
        output_dir,
        seed,
        execution_budget,
        TriageMode::Replay(events),
    )
}

fn run_campaign(
    output_dir: &Path,
    seed: u64,
    execution_budget: u64,
    mode: TriageMode<'_>,
) -> Result<Phase2Report, Box<dyn Error>> {
    fs::create_dir_all(output_dir)?;
    let corpus_dir = output_dir.join("corpus");
    let labels_dir = output_dir.join("labels");
    let logs_dir = output_dir.join("logs");
    fs::create_dir_all(&labels_dir)?;
    fs::create_dir_all(&logs_dir)?;

    let observer: MazeObserver = StdMapObserver::owned("phase2_maze_states", vec![0_u8; MAP_SIZE]);
    let mut feedback = MaxMapFeedback::new(&observer);
    let mut objective = ConstFeedback::new(false);
    let mut state = StdState::new(
        StdRand::with_seed(seed),
        InMemoryOnDiskCorpus::new(&corpus_dir)?,
        InMemoryCorpus::<DecisionList>::new(),
        &mut feedback,
        &mut objective,
    )?;
    let scheduler = WeightedScheduler::<_, TriageScore, MazeObserver>::new(&mut state, &observer);
    let mut fuzzer = StdFuzzer::new(scheduler, feedback, objective);
    let mut manager = NopEventManager::new();
    let mut executor = MazeExecutor::new(observer);
    fuzzer.add_input(
        &mut state,
        &mut executor,
        &mut manager,
        DecisionList::default(),
    )?;

    let mutator = SingleChoiceScheduledMutator::new(tuple_list!(
        AppendDecisionMutator::default(),
        PerturbDecisionMutator::default(),
        TruncateDecisionMutator::default(),
        SpliceDecisionMutator::default(),
    ));
    let load_labels = LoadLabelsStage::new(labels_dir.clone());
    let mut stages = tuple_list!(
        load_labels,
        StdMutationalStage::with_max_iterations(mutator, NonZeroUsize::MIN),
    );

    ensure_logs(&state, &logs_dir)?;
    let mut recorded = Vec::new();
    apply_triage(mode, 0, &logs_dir, &labels_dir, &mut recorded)?;
    let mut time_to_target = None;
    let mut round = 0_u64;
    while *state.executions() < execution_budget {
        round = round.saturating_add(1);
        fuzzer.fuzz_one(&mut stages, &mut executor, &mut state, &mut manager)?;
        ensure_logs(&state, &logs_dir)?;
        apply_triage(mode, round, &logs_dir, &labels_dir, &mut recorded)?;
        if executor.last_depth() == DEEP_ROUTE.len() {
            time_to_target = Some(*state.executions());
            break;
        }
    }

    let corpus = semantic_corpus(&state)?;
    let persisted_inputs = ordinary_files(&corpus_dir)?.len();
    Ok(Phase2Report {
        executions: *state.executions(),
        time_to_target,
        corpus,
        label_events: match mode {
            TriageMode::Replay(events) => events.to_vec(),
            TriageMode::Null | TriageMode::Scripted => recorded,
        },
        persisted_inputs,
    })
}

fn ensure_logs(state: &Phase2State, logs_dir: &Path) -> Result<(), Box<dyn Error>> {
    for id in state.corpus().ids() {
        let testcase = state.corpus().get(id)?.borrow();
        let filename = testcase
            .filename()
            .as_ref()
            .ok_or("on-disk testcase has no filename")?;
        let path = logs_dir.join(format!("{filename}.log"));
        if path.exists() {
            continue;
        }
        drop(testcase);
        let input = state.corpus().cloned_input_for_id(id)?;
        let progress = *visited_depths(&input).last().expect("genesis is visited");
        fs::write(
            path,
            format!(
                "progress={progress} target={}\n",
                progress == DEEP_ROUTE.len()
            ),
        )?;
    }
    Ok(())
}

fn apply_triage(
    mode: TriageMode<'_>,
    round: u64,
    logs_dir: &Path,
    labels_dir: &Path,
    recorded: &mut Vec<LabelEvent>,
) -> Result<(), Box<dyn Error>> {
    if let TriageMode::Replay(events) = mode {
        for event in events.iter().filter(|event| event.round == round) {
            write_label(labels_dir, &event.filename, &event.labels)?;
        }
        return Ok(());
    }

    let scripted = matches!(mode, TriageMode::Scripted);
    let progress_pattern = Regex::new(r"progress=(\d+)")?;
    let mut observations = Vec::new();
    for path in ordinary_files(logs_dir)? {
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("non-UTF-8 log filename")?
            .strip_suffix(".log")
            .ok_or("log filename lacks suffix")?
            .to_owned();
        let log = fs::read_to_string(path)?;
        let captures = progress_pattern
            .captures(&log)
            .ok_or("scripted triager could not parse progress")?;
        let progress = captures[1].parse::<usize>()?;
        observations.push((filename, progress));
    }
    observations.sort();
    let maximum = observations
        .iter()
        .map(|(_, progress)| *progress)
        .max()
        .unwrap_or(0);
    for (filename, progress) in observations {
        let labels = TriageLabels::for_progress(progress, maximum, scripted);
        let bytes = serde_json::to_vec_pretty(&labels)?;
        let path = labels_dir.join(format!("{filename}.labels.json"));
        if fs::read(&path).ok().as_deref() == Some(bytes.as_slice()) {
            continue;
        }
        fs::write(path, &bytes)?;
        recorded.push(LabelEvent {
            round,
            filename,
            labels,
        });
    }
    Ok(())
}

fn write_label(
    labels_dir: &Path,
    filename: &str,
    labels: &TriageLabels,
) -> Result<(), Box<dyn Error>> {
    fs::write(
        labels_dir.join(format!("{filename}.labels.json")),
        serde_json::to_vec_pretty(labels)?,
    )?;
    Ok(())
}

fn semantic_corpus(state: &Phase2State) -> Result<Vec<CorpusEntry>, Box<dyn Error>> {
    let mut corpus = Vec::with_capacity(state.corpus().count());
    for id in state.corpus().ids() {
        corpus.push(CorpusEntry {
            input: state.corpus().cloned_input_for_id(id)?,
            parent_id: state.corpus().get(id)?.borrow().parent_id(),
        });
    }
    Ok(corpus)
}

fn ordinary_files(directory: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.is_file() && !name.starts_with('.') {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::{run_null, run_replay, run_scripted};

    #[test]
    fn scripted_triage_beats_null_and_corpus_is_on_disk() {
        const BUDGET: u64 = 100_000;
        let root = tempfile::tempdir().expect("create campaign root");
        let mut null_times = Vec::new();
        let mut scripted_times = Vec::new();
        for offset in 0_u64..12 {
            let seed = 0x5eed_2000 + offset;
            let null = run_null(&root.path().join(format!("null-{offset}")), seed, BUDGET)
                .expect("null campaign");
            let scripted = run_scripted(
                &root.path().join(format!("scripted-{offset}")),
                seed,
                BUDGET,
            )
            .expect("scripted campaign");
            assert_eq!(null.persisted_inputs, null.corpus.len());
            assert_eq!(scripted.persisted_inputs, scripted.corpus.len());
            null_times.push(null.time_to_target.unwrap_or(BUDGET + 1));
            scripted_times.push(scripted.time_to_target.unwrap_or(BUDGET + 1));
        }
        null_times.sort_unstable();
        scripted_times.sort_unstable();
        let null_median = null_times[null_times.len() / 2];
        let scripted_median = scripted_times[scripted_times.len() / 2];
        assert!(scripted_times.iter().all(|time| *time <= BUDGET));
        assert!(
            scripted_median.saturating_mul(2) < null_median,
            "scripted median {scripted_median} did not beat null median {null_median} by 2x"
        );
    }

    #[test]
    fn recorded_labels_reproduce_campaign() {
        const BUDGET: u64 = 100_000;
        let root = tempfile::tempdir().expect("create replay root");
        let seed = 0x5eed_2eef;
        let original =
            run_scripted(&root.path().join("original"), seed, BUDGET).expect("scripted original");
        let replay = run_replay(
            &root.path().join("replay"),
            seed,
            BUDGET,
            &original.label_events,
        )
        .expect("recorded-label replay");
        assert_eq!(replay.time_to_target, original.time_to_target);
        assert_eq!(replay.executions, original.executions);
        assert_eq!(replay.corpus, original.corpus);
        assert_eq!(replay.persisted_inputs, original.persisted_inputs);
    }
}
