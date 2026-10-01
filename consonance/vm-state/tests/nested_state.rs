// SPDX-License-Identifier: AGPL-3.0-or-later

use vm_state::{VmState, VmStateError};

fn nested(size: usize) -> Vec<u8> {
    let mut bytes = vec![0x5a; size];
    bytes[..4].fill(0);
    bytes[4..8].copy_from_slice(&(size as u32).to_le_bytes());
    bytes
}

#[test]
fn nested_state_round_trips_full_payload_and_binds_each_byte() {
    for size in [128, 4224, 8320] {
        let mut state = VmState {
            nested_state: Some(nested(size)),
            ..VmState::default()
        };
        let encoded = state.encode().unwrap();
        assert_eq!(VmState::decode(&encoded).unwrap(), state);
        for offset in [0, 8, size - 1] {
            state.nested_state.as_mut().unwrap()[offset] ^= 1;
            assert_ne!(state.encode().unwrap(), encoded);
            state.nested_state.as_mut().unwrap()[offset] ^= 1;
        }
    }
}

#[test]
fn nested_state_rejects_empty_short_oversized_and_misdeclared_payloads() {
    for size in [0, 127, 8321] {
        let state = VmState {
            nested_state: Some(vec![0; size]),
            ..VmState::default()
        };
        assert_eq!(state.encode(), Err(VmStateError::InvalidField));
    }
    let mut state = VmState {
        nested_state: Some(nested(128)),
        ..VmState::default()
    };
    let mut encoded = state.encode().unwrap();
    let last = encoded.len() - 128;
    encoded[last + 4..last + 8].copy_from_slice(&127_u32.to_le_bytes());
    assert_eq!(VmState::decode(&encoded), Err(VmStateError::InvalidField));
    state.nested_state.as_mut().unwrap()[4..8].copy_from_slice(&129_u32.to_le_bytes());
    assert_eq!(state.encode(), Err(VmStateError::InvalidField));
}
