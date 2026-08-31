// SPDX-License-Identifier: AGPL-3.0-or-later

//! Backend-neutral NES controller actions and environment encoding.
//!
//! The environment is a controller action suffix: each action is one button
//! mask held for a bounded frame count, applied during [`Machine::run`] and
//! released at the end of its hold. It travels as an opaque [`Reproducer`]
//! blob so the generic searcher never parses controller input.

use serde::{Deserialize, Serialize};

use crate::{MachineError, Reproducer};

/// Size of the NES CPU work RAM, the low mirror-free window of the address
/// space [`Machine::read`] serves.
pub const WRAM_SIZE: usize = 2 * 1024;
/// Bytes the NES CPU can address. Backends may expose a smaller readable
/// window through [`Machine::read`]; QuickNES exposes only [`WRAM_SIZE`].
pub const ADDRESS_SPACE_SIZE: u64 = 64 * 1024;
/// Longest controller hold accepted from an input.
pub const MAX_HOLD_FRAMES: u8 = 120;

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
    use crate::{MachineError, Reproducer};

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
}
