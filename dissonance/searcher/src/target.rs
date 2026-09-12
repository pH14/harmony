// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, fmt::Debug};

use serde::{Serialize, de::DeserializeOwned};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitKind {
    Ok,
    Crash,
}

pub trait Target {
    type Action: Clone + Debug + Eq + Serialize + DeserializeOwned;
    type Observations: Clone + Debug + Eq + Serialize + DeserializeOwned;
    type Snapshot: Clone + Debug + Eq + Serialize + DeserializeOwned;

    fn reset(&mut self);
    fn apply(&mut self, action: &Self::Action);
    fn observe(&self) -> Self::Observations;
    fn fingerprint(&self) -> u64;
    fn exit_kind(&self) -> ExitKind;

    fn snapshot(&mut self) -> Option<Self::Snapshot> {
        None
    }

    fn restore(&mut self, _snapshot: &Self::Snapshot) -> Result<(), Box<dyn Error>> {
        Err("target uses deterministic replay instead of snapshots".into())
    }
}

pub fn execute_actions<T>(target: &mut T, actions: &[T::Action]) -> Vec<T::Observations>
where
    T: Target,
{
    target.reset();
    let mut observations = vec![target.observe()];
    for action in actions {
        target.apply(action);
        observations.push(target.observe());
        if target.exit_kind() != ExitKind::Ok {
            break;
        }
    }
    observations
}
