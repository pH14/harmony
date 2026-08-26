// SPDX-License-Identifier: AGPL-3.0-or-later

//! Generic deterministic target interface shared by search workloads.

use std::{error::Error, fmt::Debug};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Deterministic snapshot-restore accounting exposed by machine-backed
/// targets. Genesis and continuation restores stay separate because only the
/// latter proves archived branch points were causally load-bearing.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SnapshotRestoreCounters {
    /// Restores of the clean gameplay genesis.
    pub genesis: u64,
    /// Restores of continuation snapshots.
    pub continuation: u64,
}

impl SnapshotRestoreCounters {
    /// Whether both counters are zero.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.genesis == 0 && self.continuation == 0
    }

    /// Saturating per-job delta from an earlier reading.
    #[must_use]
    pub fn delta_since(self, earlier: Self) -> Self {
        Self {
            genesis: self.genesis.saturating_sub(earlier.genesis),
            continuation: self.continuation.saturating_sub(earlier.continuation),
        }
    }

    /// Saturating accumulation.
    pub fn merge(&mut self, other: Self) {
        self.genesis = self.genesis.saturating_add(other.genesis);
        self.continuation = self.continuation.saturating_add(other.continuation);
    }
}

/// Outcome class of the actions applied since the last reset or restore.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitKind {
    /// The target is still executing normally.
    Ok,
    /// The target failed and its further observations are meaningless.
    Crash,
}

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
    /// Return the current exit kind.
    fn exit_kind(&self) -> ExitKind;

    /// Optionally snapshot the target. `None` means replay from genesis.
    fn snapshot(&mut self) -> Option<Self::Snapshot> {
        None
    }

    /// Restore a snapshot when supported.
    fn restore(&mut self, _snapshot: &Self::Snapshot) -> Result<(), Box<dyn Error>> {
        Err("target uses deterministic replay instead of snapshots".into())
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
