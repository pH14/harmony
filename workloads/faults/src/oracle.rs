// SPDX-License-Identifier: AGPL-3.0-or-later
//! A tiny replicated-state oracle used by the package acceptance fixture.
//!
//! The fixture deliberately models only the observable contract of the guest:
//! a write can reach the primary while the forward link is partitioned, and a
//! recovery must bring the replica current before the next check.  It gives
//! the campaign a deterministic bug path without depending on a particular
//! database, wire protocol, or host process implementation.

use crate::action::{CatalogAction, FaultAction};

/// The minimal state exposed by the replicated-service check command. Pending
/// work means the peer acknowledged a replication request but has not made it
/// visible to reads; recovery clears that state only after delivery.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReplicatedState {
    pub primary: u64,
    pub replica: u64,
    pub partitioned: bool,
    pub delayed: bool,
    pub pending_work: bool,
    pub bug_seen: bool,
    pub recovered: bool,
}

impl ReplicatedState {
    /// Apply one bounded campaign action to the fixture.
    pub fn apply(&mut self, action: FaultAction) {
        match action.kind() {
            Some(CatalogAction::PartitionForward) => {
                self.partitioned = true;
                // The primary accepts a write while the directional link is
                // down.  The replica intentionally remains at its old value.
                self.primary = self.primary.saturating_add(1);
            }
            Some(CatalogAction::RecoverForward) => {
                self.partitioned = false;
                self.delayed = false;
                self.pending_work = false;
                self.replica = self.primary;
                self.recovered = true;
            }
            Some(CatalogAction::DelayForward) => {
                self.delayed = true;
                self.pending_work = true;
                // The fixture's pending command receives an acknowledgement
                // that the replication request is queued, while leaving the
                // value invisible to replica reads until recovery delivers it.
                self.primary = self.primary.saturating_add(1);
            }
            Some(CatalogAction::Advance) => {
                self.primary = self.primary.saturating_add(1);
                if !self.partitioned && !self.pending_work {
                    self.replica = self.primary;
                }
            }
            Some(CatalogAction::PauseReplica | CatalogAction::RestartReplica) | None => {}
        }
        self.check();
    }

    /// Evaluate the check command's assertion after the current action.
    pub fn check(&mut self) -> bool {
        let stale = self.partitioned && self.primary != self.replica;
        self.bug_seen |= stale;
        stale
    }
}

/// The smallest directional-partition path that reaches the stale-read bug
/// and then proves recovery converges.
#[must_use]
pub fn directional_partition_recovery_path() -> [FaultAction; 2] {
    [
        FaultAction::new(CatalogAction::PartitionForward, 1, 0),
        FaultAction::new(CatalogAction::RecoverForward, 0, 1),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directional_partition_exposes_stale_read_before_recovery() {
        let path = directional_partition_recovery_path();
        let mut state = ReplicatedState::default();
        state.apply(path[0]);
        assert!(state.partitioned);
        assert_ne!(state.primary, state.replica);
        assert!(state.bug_seen);

        state.apply(path[1]);
        assert!(!state.partitioned);
        assert_eq!(state.primary, state.replica);
        assert!(state.recovered);
        assert!(state.bug_seen);
        assert!(!state.check());
    }

    #[test]
    fn delay_persists_until_recovery_without_creating_a_stale_read() {
        let mut state = ReplicatedState::default();
        state.apply(FaultAction::new(CatalogAction::DelayForward, 0, 0));
        assert!(state.delayed);
        assert!(state.pending_work);
        assert!(!state.partitioned);
        assert_ne!(state.primary, state.replica);
        assert!(!state.check());

        state.apply(FaultAction::new(CatalogAction::Advance, 0, 0));
        assert!(state.delayed);
        assert_ne!(state.primary, state.replica);

        state.apply(FaultAction::new(CatalogAction::RecoverForward, 0, 0));
        assert!(!state.delayed);
        assert!(!state.pending_work);
        assert!(!state.partitioned);
        assert_eq!(state.primary, state.replica);
    }

    #[test]
    fn path_is_deterministic_and_wire_stable() {
        let first = directional_partition_recovery_path();
        let second = directional_partition_recovery_path();
        assert_eq!(first, second);
        let bytes: Vec<_> = first.into_iter().flat_map(FaultAction::encode).collect();
        assert_eq!(bytes.len(), 2 * FaultAction::WIRE_LEN);
        assert_eq!(bytes[0..2], 2_u16.to_le_bytes());
    }
}
