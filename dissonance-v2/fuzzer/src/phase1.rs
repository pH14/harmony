// SPDX-License-Identifier: AGPL-3.0-or-later

//! Phase 1: typed decision lists and a deterministic in-process maze.

use std::{borrow::Cow, error::Error, num::NonZeroUsize};

use libafl::{
    Error as LibAflError, StdFuzzer,
    corpus::{Corpus, CorpusId, InMemoryCorpus},
    events::NopEventManager,
    executors::{Executor, ExitKind, HasObservers},
    feedbacks::{ConstFeedback, MaxMapFeedback},
    fuzzer::{Evaluator, Fuzzer},
    inputs::Input,
    mutators::{MutationResult, Mutator, SingleChoiceScheduledMutator},
    observers::StdMapObserver,
    schedulers::QueueScheduler,
    stages::StdMutationalStage,
    state::{HasCorpus, HasExecutions, HasRand, StdState},
};
use libafl_bolts::{
    HasLen, Named,
    rands::{Rand, StdRand},
    tuples::{RefIndexable, tuple_list},
};
use serde::{Deserialize, Serialize};

/// Maximum number of decisions retained by the Phase 1 mutators.
pub const MAX_DECISIONS: usize = 16;
pub(crate) const MAP_SIZE: usize = 64;

/// The known deep route through the combination-lock maze.
pub const DEEP_ROUTE: [Decision; 8] = [
    Decision::North,
    Decision::East,
    Decision::South,
    Decision::West,
    Decision::East,
    Decision::North,
    Decision::West,
    Decision::South,
];

/// One total action in the maze's input vocabulary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum Decision {
    /// Move north.
    North,
    /// Move east.
    East,
    /// Move south.
    South,
    /// Move west.
    West,
}

impl Decision {
    const ALL: [Self; 4] = [Self::North, Self::East, Self::South, Self::West];

    fn from_index(index: usize) -> Self {
        Self::ALL[index % Self::ALL.len()]
    }
}

/// A serializable typed input for the maze target.
#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DecisionList {
    /// Decisions applied from the maze's genesis state.
    pub decisions: Vec<Decision>,
}

impl DecisionList {
    /// Construct a list from decisions.
    #[must_use]
    pub fn new(decisions: Vec<Decision>) -> Self {
        Self { decisions }
    }
}

impl Input for DecisionList {}

impl HasLen for DecisionList {
    fn len(&self) -> usize {
        self.decisions.len()
    }
}

macro_rules! named_mutator {
    ($name:ident, $display:literal) => {
        #[derive(Debug)]
        pub struct $name {
            name: Cow<'static, str>,
        }

        impl Default for $name {
            fn default() -> Self {
                Self {
                    name: Cow::Borrowed($display),
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

named_mutator!(AppendDecisionMutator, "AppendDecisionMutator");
named_mutator!(PerturbDecisionMutator, "PerturbDecisionMutator");
named_mutator!(TruncateDecisionMutator, "TruncateDecisionMutator");
named_mutator!(SpliceDecisionMutator, "SpliceDecisionMutator");

impl<S> Mutator<DecisionList, S> for AppendDecisionMutator
where
    S: HasRand,
{
    fn mutate(
        &mut self,
        state: &mut S,
        input: &mut DecisionList,
    ) -> Result<MutationResult, LibAflError> {
        if input.decisions.len() >= MAX_DECISIONS {
            return Ok(MutationResult::Skipped);
        }
        let choice = state
            .rand_mut()
            .below(NonZeroUsize::new(Decision::ALL.len()).expect("actions are nonempty"));
        input.decisions.push(Decision::from_index(choice));
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

impl<S> Mutator<DecisionList, S> for PerturbDecisionMutator
where
    S: HasRand,
{
    fn mutate(
        &mut self,
        state: &mut S,
        input: &mut DecisionList,
    ) -> Result<MutationResult, LibAflError> {
        let Some(length) = NonZeroUsize::new(input.decisions.len()) else {
            return Ok(MutationResult::Skipped);
        };
        let index = state.rand_mut().below(length);
        let old = input.decisions[index];
        let offset = state
            .rand_mut()
            .below(NonZeroUsize::new(Decision::ALL.len() - 1).expect("multiple actions"))
            + 1;
        let old_index = Decision::ALL
            .iter()
            .position(|candidate| *candidate == old)
            .expect("decision belongs to action vocabulary");
        input.decisions[index] = Decision::from_index(old_index + offset);
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

impl<S> Mutator<DecisionList, S> for TruncateDecisionMutator
where
    S: HasRand,
{
    fn mutate(
        &mut self,
        state: &mut S,
        input: &mut DecisionList,
    ) -> Result<MutationResult, LibAflError> {
        let Some(length) = NonZeroUsize::new(input.decisions.len()) else {
            return Ok(MutationResult::Skipped);
        };
        let new_len = state.rand_mut().below(length);
        input.decisions.truncate(new_len);
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

impl<S> Mutator<DecisionList, S> for SpliceDecisionMutator
where
    S: HasCorpus<DecisionList> + HasRand,
{
    fn mutate(
        &mut self,
        state: &mut S,
        input: &mut DecisionList,
    ) -> Result<MutationResult, LibAflError> {
        let Some(corpus_len) = NonZeroUsize::new(state.corpus().count()) else {
            return Ok(MutationResult::Skipped);
        };
        let corpus_offset = state.rand_mut().below(corpus_len);
        let other_id = state.corpus().nth(corpus_offset);
        let other = state.corpus().cloned_input_for_id(other_id)?;
        if other.decisions.is_empty() {
            return Ok(MutationResult::Skipped);
        }

        let prefix = state.rand_mut().below_or_zero(input.decisions.len() + 1);
        let suffix_start = state.rand_mut().below(
            NonZeroUsize::new(other.decisions.len()).expect("checked nonempty splice input"),
        );
        let mut decisions = input.decisions[..prefix].to_vec();
        decisions.extend_from_slice(&other.decisions[suffix_start..]);
        decisions.truncate(MAX_DECISIONS);
        if decisions == input.decisions {
            return Ok(MutationResult::Skipped);
        }
        input.decisions = decisions;
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

pub(crate) type MazeObserver = StdMapObserver<'static, u8, false>;
type MazeObservers = (MazeObserver, ());

/// Safe in-process executor that owns and updates its map observer.
#[derive(Debug)]
pub struct MazeExecutor {
    observers: MazeObservers,
    last_depth: usize,
}

impl MazeExecutor {
    pub(crate) fn new(observer: MazeObserver) -> Self {
        Self {
            observers: tuple_list!(observer),
            last_depth: 0,
        }
    }

    /// Depth reached by the most recently executed input.
    #[must_use]
    pub fn last_depth(&self) -> usize {
        self.last_depth
    }
}

impl HasObservers for MazeExecutor {
    type Observers = MazeObservers;

    fn observers(&self) -> RefIndexable<&Self::Observers, Self::Observers> {
        RefIndexable::from(&self.observers)
    }

    fn observers_mut(&mut self) -> RefIndexable<&mut Self::Observers, Self::Observers> {
        RefIndexable::from(&mut self.observers)
    }
}

impl<EM, S, Z> Executor<EM, DecisionList, S, Z> for MazeExecutor
where
    S: HasExecutions,
{
    fn run_target(
        &mut self,
        _fuzzer: &mut Z,
        state: &mut S,
        _manager: &mut EM,
        input: &DecisionList,
    ) -> Result<ExitKind, LibAflError> {
        *state.executions_mut() = state.executions().saturating_add(1);
        let depths = visited_depths(input);
        self.last_depth = *depths.last().expect("genesis is always visited");
        for depth in depths {
            self.observers.0[feature_index(depth)] = 1;
        }
        Ok(ExitKind::Ok)
    }
}

fn feature_index(depth: usize) -> usize {
    depth.wrapping_mul(0x9e37_79b1) % MAP_SIZE
}

/// Return each state depth visited before the first wrong decision.
#[must_use]
pub fn visited_depths(input: &DecisionList) -> Vec<usize> {
    let mut result = vec![0];
    for (depth, decision) in input.decisions.iter().enumerate() {
        if DEEP_ROUTE.get(depth) != Some(decision) {
            break;
        }
        result.push(depth + 1);
        if depth + 1 == DEEP_ROUTE.len() {
            break;
        }
    }
    result
}

/// A semantic snapshot of one testcase, used by the replay property.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorpusEntry {
    /// Testcase input.
    pub input: DecisionList,
    /// LibAFL's recorded parent testcase.
    pub parent_id: Option<CorpusId>,
}

/// Result of one bounded guided campaign.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CampaignReport {
    /// Total target executions, including the initial seed.
    pub executions: u64,
    /// Execution at which the known deep state was first reached.
    pub time_to_target: Option<u64>,
    /// Corpus in deterministic insertion order.
    pub corpus: Vec<CorpusEntry>,
}

/// Run the Phase 1 coverage-guided campaign for a deterministic budget.
pub fn run_guided(seed: u64, execution_budget: u64) -> Result<CampaignReport, Box<dyn Error>> {
    let observer = StdMapObserver::owned("maze_states", vec![0_u8; MAP_SIZE]);
    let mut feedback = MaxMapFeedback::new(&observer);
    let mut objective = ConstFeedback::new(false);
    let mut state = StdState::new(
        StdRand::with_seed(seed),
        InMemoryCorpus::<DecisionList>::new(),
        InMemoryCorpus::<DecisionList>::new(),
        &mut feedback,
        &mut objective,
    )?;
    let scheduler = QueueScheduler::new();
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
    let mut stages = tuple_list!(StdMutationalStage::with_max_iterations(
        mutator,
        NonZeroUsize::MIN,
    ));
    let mut time_to_target = None;
    while *state.executions() < execution_budget {
        fuzzer.fuzz_one(&mut stages, &mut executor, &mut state, &mut manager)?;
        if executor.last_depth() == DEEP_ROUTE.len() {
            time_to_target = Some(*state.executions());
            break;
        }
    }

    let mut corpus = Vec::with_capacity(state.corpus().count());
    for id in state.corpus().ids() {
        let input = state.corpus().cloned_input_for_id(id)?;
        let parent_id = state.corpus().get(id)?.borrow().parent_id();
        corpus.push(CorpusEntry { input, parent_id });
    }
    Ok(CampaignReport {
        executions: *state.executions(),
        time_to_target,
        corpus,
    })
}

/// Uniformly sample complete action lists until the target is reached or censored.
#[must_use]
pub fn run_random_walk(seed: u64, execution_budget: u64) -> u64 {
    let mut rand = StdRand::with_seed(seed);
    for execution in 1..=execution_budget {
        let decisions = (0..DEEP_ROUTE.len())
            .map(|_| {
                let choice = rand
                    .below(NonZeroUsize::new(Decision::ALL.len()).expect("actions are nonempty"));
                Decision::from_index(choice)
            })
            .collect();
        if visited_depths(&DecisionList::new(decisions)).last() == Some(&DEEP_ROUTE.len()) {
            return execution;
        }
    }
    execution_budget.saturating_add(1)
}

#[cfg(test)]
mod tests {
    use libafl::{
        corpus::{Corpus, InMemoryCorpus, Testcase},
        feedbacks::ConstFeedback,
        inputs::Input,
        mutators::{MutationResult, Mutator},
        state::StdState,
    };
    use libafl_bolts::rands::StdRand;
    use proptest::prelude::*;

    use super::{
        AppendDecisionMutator, DEEP_ROUTE, Decision, DecisionList, MAX_DECISIONS,
        PerturbDecisionMutator, SpliceDecisionMutator, TruncateDecisionMutator, run_guided,
        run_random_walk, visited_depths,
    };

    fn mutation_state(
        seed: u64,
        splice_input: DecisionList,
    ) -> StdState<InMemoryCorpus<DecisionList>, DecisionList, StdRand, InMemoryCorpus<DecisionList>>
    {
        let mut corpus = InMemoryCorpus::new();
        corpus
            .add(Testcase::new(splice_input))
            .expect("add splice testcase");
        let mut feedback = ConstFeedback::new(false);
        let mut objective = ConstFeedback::new(false);
        StdState::new(
            StdRand::with_seed(seed),
            corpus,
            InMemoryCorpus::new(),
            &mut feedback,
            &mut objective,
        )
        .expect("create mutation state")
    }

    fn decision_strategy() -> impl Strategy<Value = Decision> {
        prop_oneof![
            Just(Decision::North),
            Just(Decision::East),
            Just(Decision::South),
            Just(Decision::West),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn custom_mutators_preserve_bounded_valid_lists(
            raw in prop::collection::vec(decision_strategy(), 0..=MAX_DECISIONS),
            other in prop::collection::vec(decision_strategy(), 1..=MAX_DECISIONS),
            seed in any::<u64>(),
        ) {
            let original = DecisionList::new(raw);
            let mut state = mutation_state(seed, DecisionList::new(other));

            let mut append_input = original.clone();
            let append_result = AppendDecisionMutator::default()
                .mutate(&mut state, &mut append_input)
                .expect("append mutation");
            prop_assert!(append_input.decisions.len() <= MAX_DECISIONS);
            if append_result == MutationResult::Mutated {
                prop_assert_ne!(&append_input, &original);
            }

            let mut perturb_input = original.clone();
            let perturb_result = PerturbDecisionMutator::default()
                .mutate(&mut state, &mut perturb_input)
                .expect("perturb mutation");
            prop_assert!(perturb_input.decisions.len() <= MAX_DECISIONS);
            if perturb_result == MutationResult::Mutated {
                prop_assert_ne!(&perturb_input, &original);
            }

            let mut truncate_input = original.clone();
            let truncate_result = TruncateDecisionMutator::default()
                .mutate(&mut state, &mut truncate_input)
                .expect("truncate mutation");
            prop_assert!(truncate_input.decisions.len() <= MAX_DECISIONS);
            if truncate_result == MutationResult::Mutated {
                prop_assert_ne!(&truncate_input, &original);
            }

            let mut splice_input = original.clone();
            let splice_result = SpliceDecisionMutator::default()
                .mutate(&mut state, &mut splice_input)
                .expect("splice mutation");
            prop_assert!(splice_input.decisions.len() <= MAX_DECISIONS);
            if splice_result == MutationResult::Mutated {
                prop_assert_ne!(&splice_input, &original);
            }
        }
    }

    #[test]
    fn decision_list_uses_input_serialization() {
        let directory = tempfile::tempdir().expect("create input directory");
        let path = directory.path().join("decision-list");
        let input = DecisionList::new(DEEP_ROUTE.to_vec());
        input.to_file(&path).expect("write decision list");
        assert_eq!(
            DecisionList::from_file(&path).expect("read decision list"),
            input
        );
    }

    #[test]
    fn maze_is_deterministic() {
        let input = DecisionList::new(DEEP_ROUTE.to_vec());
        assert_eq!(visited_depths(&input), visited_depths(&input));
        assert_eq!(
            visited_depths(&input),
            (0..=DEEP_ROUTE.len()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn same_seed_campaigns_have_identical_corpora() {
        let first = run_guided(0x5eed_1001, 20_000).expect("first campaign");
        let second = run_guided(0x5eed_1001, 20_000).expect("second campaign");
        assert_eq!(first, second);
        assert!(
            first.time_to_target.is_some(),
            "guided campaign missed target"
        );
    }

    #[test]
    fn guided_campaign_beats_uniform_random_lists() {
        const BUDGET: u64 = 200_000;
        let seeds = 0_u64..16;
        let mut guided = Vec::new();
        let mut random = Vec::new();
        for seed in seeds {
            let report = run_guided(0x5eed_1100 + seed, BUDGET).expect("guided campaign");
            guided.push(report.time_to_target.unwrap_or(BUDGET + 1));
            random.push(run_random_walk(0x5eed_1100 + seed, BUDGET));
        }
        guided.sort_unstable();
        random.sort_unstable();
        let guided_median = guided[guided.len() / 2];
        let random_median = random[random.len() / 2];
        assert!(guided.iter().all(|time| *time <= BUDGET));
        assert!(
            guided_median.saturating_mul(10) < random_median,
            "guided median {guided_median} was not 10x faster than random median {random_median}"
        );
    }
}
