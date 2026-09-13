// SPDX-License-Identifier: AGPL-3.0-or-later

use vm_state::{Arm64VmState, SnapshotRecords, VmState};

fn engine_state_lifecycle<S: SnapshotRecords>(mut state: S) {
    let legacy = <S as SnapshotRecords>::encode(&state).unwrap();
    let restored = <S as SnapshotRecords>::decode(&legacy).unwrap();
    assert!(restored.engine_state().is_empty());

    let first = vec![0xA5, 0x00, 0xFE, 0x11];
    state.set_engine_state(first.clone());
    assert_eq!(state.engine_state(), first.as_slice());

    let first_blob = <S as SnapshotRecords>::encode(&state).unwrap();
    let mut restored = <S as SnapshotRecords>::decode(&first_blob).unwrap();
    assert_eq!(restored.engine_state(), first.as_slice());

    let replacement = vec![0x01, 0x7F, 0xC3];
    restored.set_engine_state(replacement.clone());
    assert_eq!(restored.engine_state(), replacement.as_slice());
    let replacement_blob = <S as SnapshotRecords>::encode(&restored).unwrap();
    let mut reset = <S as SnapshotRecords>::decode(&replacement_blob).unwrap();
    assert_eq!(reset.engine_state(), replacement.as_slice());

    reset.set_engine_state(Vec::new());
    assert!(reset.engine_state().is_empty());
    let reset_blob = <S as SnapshotRecords>::encode(&reset).unwrap();
    let reset_restored = <S as SnapshotRecords>::decode(&reset_blob).unwrap();
    assert!(reset_restored.engine_state().is_empty());
    assert_eq!(reset_blob, legacy);
}

#[test]
fn x86_engine_state_uses_the_public_snapshot_records_seam() {
    engine_state_lifecycle(VmState::default());
}

#[test]
fn arm64_engine_state_uses_the_public_snapshot_records_seam() {
    engine_state_lifecycle(Arm64VmState::default());
}
