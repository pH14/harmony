// SPDX-License-Identifier: AGPL-3.0-or-later

//! Backend-neutral NES controller actions and environment encoding.
//!
//! The environment is a controller action suffix: each action is one button
//! mask held for a bounded frame count, applied during [`Machine::run`] and
//! released at the end of its hold. It travels as an opaque [`Reproducer`]
//! blob so the generic searcher never parses controller input.

use serde::{Deserialize, Serialize};

use crate::{Machine, MachineError, Reproducer, StopConditions};

/// Size of the NES CPU work RAM, the low mirror-free window of the address
/// space [`Machine::read`] serves.
pub const WRAM_SIZE: usize = 2 * 1024;
/// Bytes the NES CPU can address. Backends may expose a smaller readable
/// window through [`Machine::read`]; QuickNES exposes only [`WRAM_SIZE`].
pub const ADDRESS_SPACE_SIZE: u64 = 64 * 1024;
/// Longest controller hold accepted from an input.
pub const MAX_HOLD_FRAMES: u8 = 120;

/// Length of an iNES header.
const INES_HEADER_LEN: usize = 16;
/// iNES header byte holding the low mapper nibble and the cartridge flags.
const INES_FLAGS6: usize = 6;
/// Flag 6 bit that declares battery-backed cartridge work RAM at `$6000`.
const INES_BATTERY: u8 = 0x02;

/// Copy a ROM image with its cartridge work RAM declared present.
///
/// Mappers that map RAM at `$6000` allocate it whatever the header says, but
/// a backend publishes that region through its memory interface only for an
/// image that declares it. Games without a battery keep durable progress
/// there — items collected, equipment carried — so an observation decoder
/// cannot see their progress until the region is published.
///
/// # Errors
///
/// Returns an error when the bytes are not an iNES image.
pub fn with_cartridge_ram(rom: &[u8]) -> Result<Vec<u8>, MachineError> {
    if rom.len() < INES_HEADER_LEN || &rom[..4] != b"NES\x1a" {
        return Err(MachineError::Backend(
            "cartridge work RAM needs an iNES image".to_owned(),
        ));
    }
    let mut image = rom.to_vec();
    image[INES_FLAGS6] |= INES_BATTERY;
    Ok(image)
}

/// Blob format version of a NES [`Reproducer`]: a flat sequence of
/// `(buttons, hold_frames)` byte pairs in execution order.
pub const ENV_BLOB_VERSION: u16 = 1;

/// Mint the environment blob for one controller action suffix.
#[must_use]
pub fn reproducer(actions: &[ButtonChord]) -> Reproducer {
    let mut bytes = Vec::with_capacity(actions.len() * 2);
    for action in actions {
        bytes.push(action.buttons);
        bytes.push(action.bounded_hold_frames());
    }
    Reproducer {
        blob_version: ENV_BLOB_VERSION,
        bytes,
    }
}

/// Execute a fixed controller walk one chord at a time, retaining only the
/// current continuation snapshot between chords.
///
/// Native machines can consume an opaque multi-chord reproducer to quiescence.
/// The guest NES agent instead emits a lifecycle snapshot point after each
/// chord, so staging the whole walk in one branch would make a generic `run`
/// stop after its first chord. This helper gives both backends the same walk
/// semantics while keeping the action wire format unchanged.
pub fn run_actions<M: Machine>(
    machine: &mut M,
    actions: &[ButtonChord],
) -> Result<(), MachineError> {
    let mut current = machine.snapshot()?;
    for action in actions {
        let result = machine
            .branch(current, &reproducer(std::slice::from_ref(action)))
            .and_then(|_| machine.run(StopConditions::default(), None).map(|_| ()));
        if let Err(error) = result {
            let _ = machine.drop_snapshot(current);
            return Err(error);
        }
        let next = match machine.snapshot() {
            Ok(next) => next,
            Err(error) => {
                let _ = machine.drop_snapshot(current);
                return Err(error);
            }
        };
        if let Err(error) = machine.drop_snapshot(current) {
            let _ = machine.drop_snapshot(next);
            return Err(error);
        }
        current = next;
    }
    machine.drop_snapshot(current)
}

/// Parse an environment blob back into its controller action suffix.
///
/// # Errors
///
/// Returns an error for another format version or a truncated blob.
pub fn actions_of(env: &Reproducer) -> Result<Vec<ButtonChord>, MachineError> {
    if env.blob_version != ENV_BLOB_VERSION {
        return Err(MachineError::BadEnvVersion);
    }
    if !env.bytes.len().is_multiple_of(2) {
        return Err(MachineError::MalformedEnv);
    }
    Ok(env
        .bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| ButtonChord::new(pair[0], pair[1]))
        .collect())
}

/// One total NES input action: an eight-button mask held for a bounded frame count.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ButtonChord {
    /// Standard NES controller bits: A, B, Select, Start, Up, Down, Left, Right.
    pub buttons: u8,
    /// Requested hold duration. Execution clamps this to `1..=MAX_HOLD_FRAMES`.
    pub hold_frames: u8,
}

impl ButtonChord {
    /// Construct a chord, normalizing its duration into the machine's total domain.
    #[must_use]
    pub fn new(buttons: u8, hold_frames: u8) -> Self {
        Self {
            buttons,
            hold_frames: hold_frames.clamp(1, MAX_HOLD_FRAMES),
        }
    }

    /// Return the normalized hold duration used by execution.
    #[must_use]
    pub fn bounded_hold_frames(self) -> u8 {
        self.hold_frames.clamp(1, MAX_HOLD_FRAMES)
    }
}

#[cfg(test)]
mod tests {
    use super::{ButtonChord, ENV_BLOB_VERSION, MAX_HOLD_FRAMES, actions_of, reproducer};
    use crate::{
        Answer, Machine, MachineError, Moment, Reproducer, SnapId, StopConditions, StopReason,
    };
    use std::collections::BTreeSet;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Failure {
        Branch(usize),
        Drop(usize),
        Run(usize),
        Snapshot(usize),
    }

    #[derive(Default)]
    struct LifecycleMachine {
        next_snapshot: u64,
        snapshot_calls: usize,
        branch_calls: usize,
        drop_calls: usize,
        run_calls: usize,
        held: BTreeSet<u64>,
        attempted: Vec<Vec<ButtonChord>>,
        completed: Vec<ButtonChord>,
        stops: Vec<StopReason>,
        dropped: Vec<SnapId>,
        failure: Option<Failure>,
    }

    impl LifecycleMachine {
        fn fail(&self, failure: Failure) -> Result<(), MachineError> {
            if self.failure == Some(failure) {
                return Err(MachineError::Backend(format!("fake {failure:?} failure")));
            }
            Ok(())
        }
    }

    impl Machine for LifecycleMachine {
        type Portable = u64;

        fn snapshot(&mut self) -> Result<SnapId, MachineError> {
            self.snapshot_calls += 1;
            self.fail(Failure::Snapshot(self.snapshot_calls))?;
            let id = SnapId(self.next_snapshot);
            self.next_snapshot += 1;
            self.held.insert(id.0);
            Ok(id)
        }

        fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError> {
            self.drop_calls += 1;
            self.fail(Failure::Drop(self.drop_calls))?;
            if !self.held.remove(&snap.0) {
                return Err(MachineError::UnknownSnapshot);
            }
            self.dropped.push(snap);
            Ok(())
        }

        fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), MachineError> {
            if !self.held.contains(&snap.0) {
                return Err(MachineError::UnknownSnapshot);
            }
            self.branch_calls += 1;
            let actions = actions_of(env)?;
            self.attempted.push(actions.clone());
            self.fail(Failure::Branch(self.branch_calls))?;
            let [action] = actions.as_slice() else {
                return Err(MachineError::MalformedEnv);
            };
            self.completed.push(*action);
            Ok(())
        }

        fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
            self.held
                .contains(&snap.0)
                .then_some(())
                .ok_or(MachineError::UnknownSnapshot)
        }

        fn run(
            &mut self,
            until: StopConditions,
            resolve: Option<&Answer>,
        ) -> Result<StopReason, MachineError> {
            assert_eq!(until, StopConditions::default());
            assert!(resolve.is_none());
            self.run_calls += 1;
            self.fail(Failure::Run(self.run_calls))?;
            let stop = StopReason::SnapshotPoint {
                vtime: Moment(self.run_calls as u64),
            };
            self.stops.push(stop.clone());
            Ok(stop)
        }

        fn read(&self, _addr: u64, _len: u32) -> Result<Vec<u8>, MachineError> {
            Ok(Vec::new())
        }

        fn export(
            &mut self,
            snap: SnapId,
            _base: Option<&Self::Portable>,
        ) -> Result<Self::Portable, MachineError> {
            self.held
                .contains(&snap.0)
                .then_some(snap.0)
                .ok_or(MachineError::UnknownSnapshot)
        }

        fn import(&mut self, portable: &Self::Portable) -> Result<SnapId, MachineError> {
            let snap = SnapId(*portable);
            self.held.insert(snap.0);
            Ok(snap)
        }

        fn portable_memory_charge(_portable: &Self::Portable) -> usize {
            0
        }

        fn now(&self) -> Moment {
            Moment(self.run_calls as u64)
        }

        fn frames(&self) -> &[[u8; 2048]] {
            &[]
        }
    }

    #[test]
    fn cartridge_ram_declaration_only_sets_the_battery_flag() {
        for flags in 0..=u8::MAX {
            let mut rom: Vec<_> = (0..64_u8).collect();
            rom[..4].copy_from_slice(b"NES\x1a");
            rom[6] = flags;
            let mut expected = rom.clone();
            expected[6] |= 2;
            let declared = super::with_cartridge_ram(&rom).unwrap();
            assert_eq!(declared, expected);
            assert_eq!(rom[6], flags, "the supplied ROM must remain unchanged");
            assert_eq!(super::with_cartridge_ram(&declared).unwrap(), declared);
        }
        for bad in [&[][..], &b"NES\x1a"[..], &[0_u8; 16][..]] {
            assert!(super::with_cartridge_ram(bad).is_err());
        }
    }

    #[test]
    fn chord_duration_is_total_and_bounded() {
        assert_eq!(ButtonChord::new(0x81, 0).hold_frames, 1);
        assert_eq!(ButtonChord::new(0x81, u8::MAX).hold_frames, MAX_HOLD_FRAMES);
    }

    #[test]
    fn an_environment_blob_round_trips_and_rejects_foreign_versions() {
        let actions = vec![ButtonChord::new(0x81, 4), ButtonChord::new(0, 200)];
        let env = reproducer(&actions);
        assert_eq!(env.blob_version, ENV_BLOB_VERSION);
        assert_eq!(actions_of(&env).expect("round trip"), actions);
        assert_eq!(
            actions_of(&Reproducer {
                blob_version: ENV_BLOB_VERSION + 1,
                bytes: env.bytes.clone(),
            }),
            Err(MachineError::BadEnvVersion)
        );
        assert_eq!(
            actions_of(&Reproducer {
                blob_version: ENV_BLOB_VERSION,
                bytes: vec![0x01],
            }),
            Err(MachineError::MalformedEnv)
        );
    }

    #[test]
    fn run_actions_executes_each_chord_at_a_lifecycle_boundary() {
        let actions = [ButtonChord::new(0x81, 4), ButtonChord::new(0x40, 2)];
        let mut machine = LifecycleMachine::default();

        super::run_actions(&mut machine, &actions).expect("walk");

        assert_eq!(
            machine.attempted,
            actions
                .iter()
                .map(|action| vec![*action])
                .collect::<Vec<_>>()
        );
        assert_eq!(machine.completed, actions);
        assert_eq!(machine.run_calls, actions.len());
        assert!(
            machine
                .stops
                .iter()
                .all(|stop| matches!(stop, StopReason::SnapshotPoint { .. }))
        );
        assert!(machine.held.is_empty());
        assert_eq!(machine.dropped.len(), actions.len() + 1);
    }

    #[test]
    fn run_actions_releases_the_current_snapshot_when_branch_or_run_fails() {
        for failure in [Failure::Branch(2), Failure::Run(2)] {
            let actions = [ButtonChord::new(0x01, 1), ButtonChord::new(0x02, 1)];
            let mut machine = LifecycleMachine {
                failure: Some(failure),
                ..LifecycleMachine::default()
            };

            assert!(super::run_actions(&mut machine, &actions).is_err());
            assert!(machine.held.is_empty());
            assert_eq!(machine.dropped.len(), 2);
        }
    }

    #[test]
    fn run_actions_releases_the_current_snapshot_when_next_snapshot_fails() {
        let actions = [ButtonChord::new(0x01, 1), ButtonChord::new(0x02, 1)];
        let mut machine = LifecycleMachine {
            failure: Some(Failure::Snapshot(2)),
            ..LifecycleMachine::default()
        };

        assert!(super::run_actions(&mut machine, &actions).is_err());
        assert!(machine.held.is_empty());
        assert_eq!(machine.run_calls, 1);
        assert_eq!(machine.dropped.len(), 1);
    }

    #[test]
    fn run_actions_cleans_up_the_next_snapshot_when_old_release_fails() {
        let actions = [ButtonChord::new(0x01, 1), ButtonChord::new(0x02, 1)];
        let mut machine = LifecycleMachine {
            failure: Some(Failure::Drop(2)),
            ..LifecycleMachine::default()
        };

        assert!(super::run_actions(&mut machine, &actions).is_err());
        assert_eq!(machine.held.len(), 1);
        assert_eq!(machine.drop_calls, 3);
        assert_eq!(machine.dropped.len(), 2);
    }
}
