// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, io};

use searcher::search::{
    archive::ArchiveKey,
    campaign::{CampaignTypes, TargetExecution},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct TinyAction(u8);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct TinySnapshot {
    value: u8,
    execution_work: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct TinyKey(u8);

impl ArchiveKey for TinyKey {
    type Group = u8;

    fn groups() -> usize {
        1
    }

    fn group(self, depth: usize) -> Self::Group {
        assert_eq!(depth, 0);
        self.0
    }

    type Lineage = ();

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

#[derive(Default)]
struct TinyTarget {
    value: u8,
    execution_work: u64,
}

struct TinyExecution;

impl CampaignTypes for TinyExecution {
    type Target = TinyTarget;
    type Action = TinyAction;
    type Key = TinyKey;
    type Milestones = u8;
    type Progress = u8;
    type Snapshot = TinySnapshot;
    type Observations = u8;
    type Evidence = ();
    type ArchiveReport = ();
    type Run = ();
    type DrawState = ();
    type DrawCheckpoint = ();
    type DrawHeader = ();
}

fn action_cost(action: &TinyAction) -> u64 {
    u64::from(action.0).saturating_mul(2)
}

impl TargetExecution for TinyExecution {
    fn new_target(&self) -> Result<Self::Target, String> {
        Ok(TinyTarget::default())
    }

    fn reset(&self, target: &mut Self::Target) {
        target.value = 0;
    }

    fn restore(
        &self,
        target: &mut Self::Target,
        snapshot: &Self::Snapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.value = snapshot.value;
        Ok(())
    }

    fn execution_work(&self, target: &Self::Target) -> u64 {
        target.execution_work
    }

    fn action_cost_fn(&self) -> fn(&Self::Action) -> u64 {
        action_cost
    }

    fn snapshot_memory_charge(snapshot: &Self::Snapshot) -> usize {
        std::mem::size_of_val(snapshot)
    }

    fn apply_action(
        &self,
        target: &mut Self::Target,
        action: &Self::Action,
        milestones: &mut Self::Milestones,
    ) -> Result<(), Box<dyn Error>> {
        target.value = target.value.saturating_add(action.0);
        target.execution_work = target.execution_work.saturating_add(u64::from(action.0));
        *milestones = target.value;
        Ok(())
    }

    fn rollout_observations(&self, target: &Self::Target) -> Vec<Self::Observations> {
        vec![target.value]
    }

    fn snapshot(&self, target: &mut Self::Target) -> Result<Self::Snapshot, Box<dyn Error>> {
        Ok(TinySnapshot {
            value: target.value,
            execution_work: target.execution_work,
        })
    }
}

fn exercise_execution<T: TargetExecution>(
    execution: &T,
    run: &T::Run,
    action: T::Action,
) -> Result<(), Box<dyn Error>> {
    let mut target = execution.new_target().map_err(io::Error::other)?;
    execution.reset(&mut target);
    let origin = execution.snapshot(&mut target)?;
    assert_eq!(execution.execution_work(&target), 0);
    assert_eq!(
        T::snapshot_memory_charge(&origin),
        std::mem::size_of_val(&origin)
    );
    assert!((execution.action_cost_fn())(&action) > 0);

    let mut milestones = T::Milestones::default();
    execution.apply_action(&mut target, &action, &mut milestones)?;
    assert!(execution.execution_work(&target) > 0);
    assert!(!execution.rollout_observations(&target).is_empty());
    let after_action = execution.snapshot(&mut target)?;
    assert_ne!(after_action, origin);

    execution.restore(&mut target, &origin)?;
    assert!(execution.execution_work(&target) > 0);
    assert!(execution.rollout_probe(run, &mut target, &after_action)?);
    assert_eq!(execution.snapshot(&mut target)?, after_action);
    Ok(())
}

#[test]
fn target_execution_is_independently_usable() {
    exercise_execution(&TinyExecution, &(), TinyAction(3)).unwrap();
}
