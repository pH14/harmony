// SPDX-License-Identifier: AGPL-3.0-or-later

//! Generic deterministic target interface shared by search workloads.

use std::fmt::Debug;

use libafl::{Error, executors::ExitKind};
use serde::{Serialize, de::DeserializeOwned};

/// The target seam required by deterministic search workloads.
pub trait Target {
    /// One total action in this target's input vocabulary.
    type Action: Clone + Debug + Eq + Serialize + DeserializeOwned;
    /// Evidence exposed after each action.
    type Observations: Clone + Debug + Eq + Serialize + DeserializeOwned;
    /// Optional deterministic snapshot representation.
    type Snapshot: Clone + Debug + Eq + Serialize + DeserializeOwned;

    /// Reset to genesis.
    fn reset(&mut self);
    /// Apply one total action. An inapplicable action is a no-op.
    fn apply(&mut self, action: &Self::Action);
    /// Observe the current target state.
    fn observe(&self) -> Self::Observations;
    /// Return the deliberately coarse base-map feature.
    fn fingerprint(&self) -> u64;
    /// Return the current LibAFL exit kind.
    fn exit_kind(&self) -> ExitKind;

    /// Optionally snapshot the target. `None` means replay from genesis.
    fn snapshot(&mut self) -> Option<Self::Snapshot> {
        None
    }

    /// Restore a snapshot when supported.
    fn restore(&mut self, _snapshot: &Self::Snapshot) -> Result<(), Error> {
        Err(Error::not_implemented(
            "target uses deterministic replay instead of snapshots",
        ))
    }
}

/// Reset a target and apply an action list, returning every observation.
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
