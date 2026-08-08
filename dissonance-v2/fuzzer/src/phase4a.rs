// SPDX-License-Identifier: AGPL-3.0-or-later

//! Phase 4a: generated semantic mutators and the adventure experiment.

use std::{borrow::Cow, cell::RefCell, rc::Rc};

use libafl::{
    Error as LibAflError, HasMetadata,
    corpus::{Corpus, CorpusId},
    inputs::Input,
    mutators::{MutationResult, Mutator},
    state::HasCorpus,
};
use libafl_bolts::{HasLen, Named};
use serde::{Deserialize, Serialize};

use crate::target::AdventureAction;

/// Maximum action count accepted by the adventure campaign.
pub const MAX_ADVENTURE_ACTIONS: usize = 24;

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
        MAX_ADVENTURE_ACTIONS, MutatorStats, ProducerMetadata,
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
}
