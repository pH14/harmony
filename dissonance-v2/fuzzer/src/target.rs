// SPDX-License-Identifier: AGPL-3.0-or-later

//! Phase 4a start: the target seam extracted from two concrete targets.

use std::fmt::Debug;

use libafl::{Error, executors::ExitKind};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::phase1::{DEEP_ROUTE, Decision};

/// The target seam required by both the maze and adventure toy.
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

/// Maze evidence at one decision boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MazeObservations {
    /// Correct prefix depth.
    pub depth: usize,
    /// Whether the known deep state was reached.
    pub complete: bool,
    /// Whether an incorrect decision ended this input.
    pub stopped: bool,
}

/// Deterministic combination-lock maze as the first target implementor.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MazeTarget {
    depth: usize,
    stopped: bool,
}

impl Target for MazeTarget {
    type Action = Decision;
    type Observations = MazeObservations;
    type Snapshot = MazeObservations;

    fn reset(&mut self) {
        self.depth = 0;
        self.stopped = false;
    }

    fn apply(&mut self, action: &Self::Action) {
        if self.stopped || self.depth == DEEP_ROUTE.len() {
            return;
        }
        if DEEP_ROUTE.get(self.depth) == Some(action) {
            self.depth += 1;
        } else {
            self.stopped = true;
        }
    }

    fn observe(&self) -> Self::Observations {
        MazeObservations {
            depth: self.depth,
            complete: self.depth == DEEP_ROUTE.len(),
            stopped: self.stopped,
        }
    }

    fn fingerprint(&self) -> u64 {
        u64::try_from(self.depth).expect("maze depth fits u64")
    }

    fn exit_kind(&self) -> ExitKind {
        ExitKind::Ok
    }

    fn snapshot(&mut self) -> Option<Self::Snapshot> {
        Some(self.observe())
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), Error> {
        if snapshot.depth > DEEP_ROUTE.len()
            || snapshot.complete != (snapshot.depth == DEEP_ROUTE.len())
        {
            return Err(Error::illegal_argument("invalid maze snapshot"));
        }
        self.depth = snapshot.depth;
        self.stopped = snapshot.stopped;
        Ok(())
    }
}

/// Rooms in the Phase 4a adventure toy.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Room {
    /// Genesis room.
    Start,
    /// Room containing the key.
    Key,
    /// Room containing the locked door.
    Door,
    /// Goal room beyond the door.
    Treasure,
    /// Hazard room that produces a crash exit.
    Hazard,
}

/// Total adventure action vocabulary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum AdventureAction {
    /// Move north when a passage exists.
    North,
    /// Move south when a passage exists.
    South,
    /// Move east when a passage exists.
    East,
    /// Move west when a passage exists.
    West,
    /// Pick up the key when present.
    TakeKey,
    /// Open the door when present and carrying the key.
    OpenDoor,
    /// Deterministic no-op.
    Wait,
}

/// Adventure evidence read by feedback and triage.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdventureObservations {
    /// Current room.
    pub room: Room,
    /// Inventory state hidden from the coarse fingerprint.
    pub has_key: bool,
    /// Whether the locked door has opened.
    pub door_open: bool,
    /// Whether the goal room was reached.
    pub target: bool,
    /// Whether a hazard ended the run.
    pub crashed: bool,
}

/// Complete deterministic adventure snapshot.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdventureSnapshot(AdventureObservations);

/// Small adventure toy with rooms, inventory, a locked door, and a hazard.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdventureToy {
    state: AdventureObservations,
}

impl Default for AdventureToy {
    fn default() -> Self {
        Self {
            state: AdventureObservations {
                room: Room::Start,
                has_key: false,
                door_open: false,
                target: false,
                crashed: false,
            },
        }
    }
}

impl Target for AdventureToy {
    type Action = AdventureAction;
    type Observations = AdventureObservations;
    type Snapshot = AdventureSnapshot;

    fn reset(&mut self) {
        *self = Self::default();
    }

    fn apply(&mut self, action: &Self::Action) {
        if self.state.crashed || self.state.target {
            return;
        }
        match (self.state.room, action) {
            (Room::Start, AdventureAction::North) => self.state.room = Room::Key,
            (Room::Start, AdventureAction::South) => self.state.room = Room::Door,
            (Room::Start, AdventureAction::East) => {
                self.state.room = Room::Hazard;
                self.state.crashed = true;
            }
            (Room::Key, AdventureAction::TakeKey) => self.state.has_key = true,
            (Room::Key, AdventureAction::South) => self.state.room = Room::Start,
            (Room::Door, AdventureAction::OpenDoor) if self.state.has_key => {
                self.state.door_open = true;
            }
            (Room::Door, AdventureAction::North) => self.state.room = Room::Start,
            (Room::Door, AdventureAction::East) if self.state.door_open => {
                self.state.room = Room::Treasure;
                self.state.target = true;
            }
            (_, AdventureAction::Wait)
            | (_, AdventureAction::North)
            | (_, AdventureAction::South)
            | (_, AdventureAction::East)
            | (_, AdventureAction::West)
            | (_, AdventureAction::TakeKey)
            | (_, AdventureAction::OpenDoor) => {}
        }
    }

    fn observe(&self) -> Self::Observations {
        self.state.clone()
    }

    fn fingerprint(&self) -> u64 {
        match self.state.room {
            Room::Start => 0,
            Room::Key => 1,
            Room::Door => 2,
            Room::Treasure => 3,
            Room::Hazard => 4,
        }
    }

    fn exit_kind(&self) -> ExitKind {
        if self.state.crashed {
            ExitKind::Crash
        } else {
            ExitKind::Ok
        }
    }

    fn snapshot(&mut self) -> Option<Self::Snapshot> {
        Some(AdventureSnapshot(self.observe()))
    }

    fn restore(&mut self, snapshot: &Self::Snapshot) -> Result<(), Error> {
        if snapshot.0.target != (snapshot.0.room == Room::Treasure)
            || snapshot.0.crashed != (snapshot.0.room == Room::Hazard)
        {
            return Err(Error::illegal_argument("invalid adventure snapshot"));
        }
        self.state = snapshot.0.clone();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use libafl::executors::ExitKind;

    use super::{AdventureAction, AdventureToy, MazeTarget, Room, Target, execute_actions};
    use crate::phase1::DEEP_ROUTE;

    #[test]
    fn maze_target_reproduces_the_phase1_deep_state() {
        let mut first = MazeTarget::default();
        let mut second = MazeTarget::default();
        let first_run = execute_actions(&mut first, &DEEP_ROUTE);
        let second_run = execute_actions(&mut second, &DEEP_ROUTE);
        assert_eq!(first_run, second_run);
        assert!(first_run.last().expect("final maze observation").complete);
        assert_eq!(first.fingerprint(), DEEP_ROUTE.len() as u64);
    }

    #[test]
    fn adventure_key_opens_the_door_and_snapshot_replays() {
        let route = [
            AdventureAction::North,
            AdventureAction::TakeKey,
            AdventureAction::South,
            AdventureAction::South,
            AdventureAction::OpenDoor,
        ];
        let mut target = AdventureToy::default();
        execute_actions(&mut target, &route);
        let snapshot = target.snapshot().expect("adventure snapshot");
        assert_eq!(target.observe().room, Room::Door);
        assert!(target.observe().has_key);
        assert!(target.observe().door_open);

        target.apply(&AdventureAction::East);
        let reached = target.observe();
        assert!(reached.target);
        assert_eq!(reached.room, Room::Treasure);

        target.restore(&snapshot).expect("restore adventure");
        target.apply(&AdventureAction::East);
        assert_eq!(target.observe(), reached);
    }

    #[test]
    fn adventure_hazard_reports_a_crash_and_actions_are_total() {
        let actions = [
            AdventureAction::North,
            AdventureAction::South,
            AdventureAction::East,
            AdventureAction::West,
            AdventureAction::TakeKey,
            AdventureAction::OpenDoor,
            AdventureAction::Wait,
        ];
        for room_setup in [
            &[][..],
            &[AdventureAction::North][..],
            &[AdventureAction::South][..],
        ] {
            for action in actions {
                let mut target = AdventureToy::default();
                execute_actions(&mut target, room_setup);
                target.apply(&action);
            }
        }

        let mut hazard = AdventureToy::default();
        hazard.apply(&AdventureAction::East);
        assert_eq!(hazard.exit_kind(), ExitKind::Crash);
        assert_eq!(hazard.observe().room, Room::Hazard);
    }
}
