// SPDX-License-Identifier: AGPL-3.0-or-later

//! Phase 0: a stock byte-input LibAFL fuzzer and a deliberately crashing toy.

use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use libafl::{
    StdFuzzer,
    corpus::{Corpus, InMemoryOnDiskCorpus, Testcase},
    events::NopEventManager,
    executors::{ExitKind, InProcessExecutor},
    feedbacks::{ConstFeedback, CrashFeedback},
    fuzzer::Fuzzer,
    inputs::BytesInput,
    mutators::mutations::BitFlipMutator,
    schedulers::RandScheduler,
    stages::StdMutationalStage,
    state::{HasCorpus, HasExecutions, HasSolutions, StdState},
};
use libafl_bolts::{rands::StdRand, tuples::tuple_list};

const PHASE0_SEED: u64 = 0x5eed_0000;
const INITIAL_INPUT: &[u8] = &[0];

/// The one-byte input for which the toy target reports a crash.
pub const PLANTED_CRASH: &[u8] = &[0x80];

type Phase0Corpus = InMemoryOnDiskCorpus<BytesInput>;
type Phase0State = StdState<Phase0Corpus, BytesInput, StdRand, Phase0Corpus>;
type Phase0Result<T> = Result<T, Box<dyn Error>>;

/// Summary of one bounded Phase 0 process invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Phase0Report {
    /// Whether this invocation restored a previously saved LibAFL state.
    pub resumed: bool,
    /// Total target executions, including executions before a restart.
    pub executions: u64,
    /// Number of entries in the ordinary corpus.
    pub corpus_count: usize,
    /// Number of inputs which reached the planted crash.
    pub solutions_count: usize,
    /// First testcase, loaded from the on-disk corpus after state restoration.
    pub first_corpus_input: Vec<u8>,
}

/// Run a bounded stock LibAFL byte-input campaign.
///
/// `round_budget` counts scheduler rounds. Each round runs LibAFL's stock
/// mutational stage, which may execute the target more than once. State is
/// stored beneath `output_dir`; calling this function again with the same
/// directory resumes the saved campaign.
pub fn run(output_dir: &Path, round_budget: usize) -> Phase0Result<Phase0Report> {
    fs::create_dir_all(output_dir)?;
    let state_path = output_dir.join("state.postcard");

    let mut feedback = ConstFeedback::new(false);
    let mut objective = CrashFeedback::new();
    let resumed = state_path.is_file();
    let mut state = if resumed {
        let encoded = fs::read(&state_path)?;
        postcard::from_bytes::<Phase0State>(&encoded)?
    } else {
        fresh_state(output_dir, &mut feedback, &mut objective)?
    };

    // Loading this before fuzzing makes restart depend on the persisted input
    // file. Before saving, all inputs are unloaded from the state snapshot.
    let first_corpus_input = load_first_corpus_input(&state)?;

    if state.solutions().count() == 0 {
        let scheduler = RandScheduler::new();
        let mut fuzzer = StdFuzzer::new(scheduler, feedback, objective);
        let mut manager = NopEventManager::new();
        let mut harness = |input: &BytesInput| {
            if input.as_ref() == PLANTED_CRASH {
                ExitKind::Crash
            } else {
                ExitKind::Ok
            }
        };
        let mut executor = InProcessExecutor::new(
            &mut harness,
            tuple_list!(),
            &mut fuzzer,
            &mut state,
            &mut manager,
        )?;
        let mut stages = tuple_list!(StdMutationalStage::new(BitFlipMutator::new()));

        for _ in 0..round_budget {
            fuzzer.fuzz_one(&mut stages, &mut executor, &mut state, &mut manager)?;
            if state.solutions().count() != 0 {
                break;
            }
        }
    }

    unload_inputs(state.corpus())?;
    unload_inputs(state.solutions())?;
    fs::write(&state_path, postcard::to_allocvec(&state)?)?;

    Ok(Phase0Report {
        resumed,
        executions: *state.executions(),
        corpus_count: state.corpus().count(),
        solutions_count: state.solutions().count(),
        first_corpus_input,
    })
}

fn fresh_state(
    output_dir: &Path,
    feedback: &mut ConstFeedback,
    objective: &mut CrashFeedback,
) -> Phase0Result<Phase0State> {
    let mut corpus = InMemoryOnDiskCorpus::new(output_dir.join("corpus"))?;
    corpus.add(Testcase::new(BytesInput::new(INITIAL_INPUT.to_vec())))?;
    let solutions = InMemoryOnDiskCorpus::new(output_dir.join("solutions"))?;
    Ok(StdState::new(
        StdRand::with_seed(PHASE0_SEED),
        corpus,
        solutions,
        feedback,
        objective,
    )?)
}

fn load_first_corpus_input(state: &Phase0State) -> Phase0Result<Vec<u8>> {
    let id = state.corpus().first().ok_or("phase 0 corpus is empty")?;
    let mut testcase = state.corpus().get(id)?.borrow_mut();
    state.corpus().load_input_into(&mut testcase)?;
    let input = testcase
        .input()
        .as_ref()
        .ok_or("phase 0 testcase has no persisted input")?;
    Ok(input.clone().into())
}

fn unload_inputs(corpus: &Phase0Corpus) -> Phase0Result<()> {
    let mut next = corpus.first();
    while let Some(id) = next {
        next = corpus.next(id);
        *corpus.get(id)?.borrow_mut().input_mut() = None;
    }
    Ok(())
}

/// Return ordinary files in a LibAFL on-disk corpus, excluding metadata.
pub fn persisted_inputs(corpus_dir: &Path) -> Phase0Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(corpus_dir)? {
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
    use super::{INITIAL_INPUT, persisted_inputs, run};

    #[test]
    fn planted_crash_is_found_and_corpus_survives_restart() {
        let output = tempfile::tempdir().expect("create test output directory");

        let first = run(output.path(), 256).expect("run initial campaign");
        assert!(!first.resumed);
        assert!(first.solutions_count > 0, "planted crash was not found");
        assert!(first.executions > 0);
        assert_eq!(first.corpus_count, 1);

        let before_restart =
            persisted_inputs(&output.path().join("corpus")).expect("list corpus before restart");
        assert_eq!(before_restart.len(), first.corpus_count);
        assert_eq!(
            std::fs::read(&before_restart[0]).expect("read persisted testcase"),
            INITIAL_INPUT
        );

        // A zero-round second invocation can only obtain the testcase by
        // deserializing state and loading its deliberately-unloaded input from
        // the corpus file written by the first invocation.
        let second = run(output.path(), 0).expect("resume campaign");
        assert!(second.resumed);
        assert_eq!(second.corpus_count, first.corpus_count);
        assert_eq!(second.solutions_count, first.solutions_count);
        assert_eq!(second.executions, first.executions);
        assert_eq!(second.first_corpus_input, INITIAL_INPUT);
        assert_eq!(
            persisted_inputs(&output.path().join("corpus")).expect("list corpus after restart"),
            before_restart
        );
    }
}
