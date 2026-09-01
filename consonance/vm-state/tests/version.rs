// SPDX-License-Identifier: AGPL-3.0-or-later
//! Snapshot-format version rejection.

mod common;

use common::fully_populated;
use vm_state::{VM_STATE_VERSION, VmState, VmStateError};

#[test]
fn future_version_is_rejected_but_peekable() {
    let mut blob = fully_populated().encode().unwrap();
    let future = VM_STATE_VERSION + 1;
    blob[4..6].copy_from_slice(&future.to_le_bytes());
    assert_eq!(
        VmState::decode(&blob),
        Err(VmStateError::UnsupportedVersion(future))
    );
    assert_eq!(VmState::peek_version(&blob), Ok(future));
}

#[test]
fn current_version_round_trips_and_peeks() {
    let blob = fully_populated().encode().unwrap();
    assert_eq!(VmState::peek_version(&blob), Ok(VM_STATE_VERSION));
    assert!(VmState::decode(&blob).is_ok());
}
