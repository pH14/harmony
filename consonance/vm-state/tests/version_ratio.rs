// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 5 (version rejection) and gate 6 (integer-ratio rejection).

mod common;

use common::fully_populated;
use vm_state::{VM_STATE_VERSION, VmState, VmStateError};

#[test]
fn future_version_is_rejected_but_peekable() {
    let mut blob = fully_populated().encode().unwrap();
    let future = VM_STATE_VERSION + 1;
    blob[4..6].copy_from_slice(&future.to_le_bytes());

    // `decode` refuses a version it does not understand …
    assert_eq!(
        VmState::decode(&blob),
        Err(VmStateError::UnsupportedVersion(future))
    );
    // … but `peek_version` still reports it without decoding the body.
    assert_eq!(VmState::peek_version(&blob), Ok(future));
}

#[test]
fn current_version_round_trips_and_peeks() {
    let blob = fully_populated().encode().unwrap();
    assert_eq!(VmState::peek_version(&blob), Ok(VM_STATE_VERSION));
    assert!(VmState::decode(&blob).is_ok());
}

#[test]
fn fractional_ratio_is_rejected_at_encode() {
    let mut s = fully_populated();
    s.vtime.ratio_den = 2;
    assert_eq!(s.encode(), Err(VmStateError::FractionalRatio));
}

#[test]
fn zero_ratio_den_is_also_rejected() {
    // A zero denominator is `!= 1`, so the same gate refuses it (and the default
    // VmState, whose ratio_den is 0, is therefore not encodable as-is).
    let mut s = fully_populated();
    s.vtime.ratio_den = 0;
    assert_eq!(s.encode(), Err(VmStateError::FractionalRatio));
}
