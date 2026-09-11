// SPDX-License-Identifier: AGPL-3.0-or-later
//! Replay-relevant engine lifecycle state, independent of vendor device records.

use crate::snapshot::SnapshotError;
use crate::vmm::TerminalReason;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EngineState {
    pub terminal: Option<TerminalReason>,
    pub sdk_snapshot_reentry_required: bool,
}

impl EngineState {
    pub fn encode(self) -> Result<Vec<u8>, SnapshotError> {
        if self == Self::default() {
            return Ok(Vec::new());
        }
        let (terminal, code) = match self.terminal {
            None => (0, 0),
            Some(TerminalReason::DebugExit { code }) => (1, code),
            Some(TerminalReason::Idle) => (2, 0),
            Some(TerminalReason::Shutdown) => (3, 0),
            Some(TerminalReason::SdkStop) => {
                return Err(SnapshotError::EngineState(
                    "SDK stop cannot latch a terminal",
                ));
            }
        };
        Ok(vec![
            b'V',
            b'M',
            b'E',
            1,
            terminal,
            code,
            u8::from(self.sdk_snapshot_reentry_required),
        ])
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SnapshotError> {
        if bytes.is_empty() {
            // Legacy snapshots had no engine record and restored as runnable.
            return Ok(Self::default());
        }
        if bytes.len() != 7 || bytes[..4] != [b'V', b'M', b'E', 1] {
            return Err(SnapshotError::EngineState(
                "invalid lifecycle header or length",
            ));
        }
        let terminal = match (bytes[4], bytes[5]) {
            (0, 0) => None,
            (1, code) => Some(TerminalReason::DebugExit { code }),
            (2, 0) => Some(TerminalReason::Idle),
            (3, 0) => Some(TerminalReason::Shutdown),
            _ => return Err(SnapshotError::EngineState("invalid terminal record")),
        };
        let sdk_snapshot_reentry_required = match bytes[6] {
            0 => false,
            1 => true,
            _ => return Err(SnapshotError::EngineState("invalid SDK reentry flag")),
        };
        let state = Self {
            terminal,
            sdk_snapshot_reentry_required,
        };
        if state == Self::default() {
            return Err(SnapshotError::EngineState(
                "default lifecycle must use the empty record",
            ));
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_round_trips_and_legacy_is_runnable() {
        assert_eq!(EngineState::decode(&[]).unwrap(), EngineState::default());
        let terminals = [
            None,
            Some(TerminalReason::Idle),
            Some(TerminalReason::Shutdown),
        ]
        .into_iter()
        .chain((0..=255).map(|code| Some(TerminalReason::DebugExit { code })));
        for terminal in terminals {
            for sdk_snapshot_reentry_required in [false, true] {
                let state = EngineState {
                    terminal,
                    sdk_snapshot_reentry_required,
                };
                assert_eq!(
                    EngineState::decode(&state.encode().unwrap()).unwrap(),
                    state
                );
            }
        }
        assert!(
            EngineState {
                terminal: Some(TerminalReason::SdkStop),
                sdk_snapshot_reentry_required: false
            }
            .encode()
            .is_err()
        );
    }

    #[test]
    fn malformed_lifecycle_is_rejected() {
        let valid = EngineState {
            terminal: Some(TerminalReason::Idle),
            sdk_snapshot_reentry_required: true,
        }
        .encode()
        .unwrap();
        for len in 1..valid.len() {
            assert!(EngineState::decode(&valid[..len]).is_err());
        }
        let mut trailing = valid.clone();
        trailing.push(0);
        assert!(EngineState::decode(&trailing).is_err());
        for (offset, value) in [(0, 0), (3, 2), (4, 4), (5, 1), (6, 2)] {
            let mut bad = valid.clone();
            bad[offset] = value;
            assert!(EngineState::decode(&bad).is_err());
        }
        assert!(EngineState::decode(&[b'V', b'M', b'E', 1, 0, 0, 0]).is_err());
    }
}
