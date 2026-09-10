// SPDX-License-Identifier: AGPL-3.0-or-later
//! Behavioral tests for the architecture-neutral `SnapshotRecords` seam.
//!
//! These tests intentionally use only the public trait for engine-state access
//! and serialization. That keeps the consumer contract visible: an engine can
//! replace, seal, restore, and reset opaque lifecycle bytes without naming an
//! architecture's record fields or inherent codec methods.

use vm_state::{Arm64VmState, SnapshotRecords, VmState};

fn engine_state_lifecycle<S: SnapshotRecords>(mut state: S) {
    // An untouched record set retains the legacy empty-engine shape.
    let legacy = <S as SnapshotRecords>::encode(&state).unwrap();
    let restored = <S as SnapshotRecords>::decode(&legacy).unwrap();
    assert!(restored.engine_state().is_empty());

    // Set and read through the trait before encoding. This makes a no-op setter
    // or a fabricated getter observable even if the codec still emits bytes.
    let first = vec![0xA5, 0x00, 0xFE, 0x11];
    state.set_engine_state(first.clone());
    assert_eq!(state.engine_state(), first.as_slice());

    // The engine seals and restores through the trait, preserving opaque bytes
    // exactly, including an embedded zero byte.
    let first_blob = <S as SnapshotRecords>::encode(&state).unwrap();
    let mut restored = <S as SnapshotRecords>::decode(&first_blob).unwrap();
    assert_eq!(restored.engine_state(), first.as_slice());

    // A restored snapshot may replace an existing lifecycle payload before it
    // is sealed again; the replacement must be the value that survives.
    let replacement = vec![0x01, 0x7F, 0xC3];
    restored.set_engine_state(replacement.clone());
    assert_eq!(restored.engine_state(), replacement.as_slice());
    let replacement_blob = <S as SnapshotRecords>::encode(&restored).unwrap();
    let mut reset = <S as SnapshotRecords>::decode(&replacement_blob).unwrap();
    assert_eq!(reset.engine_state(), replacement.as_slice());

    // Resetting to empty returns to the byte-compatible legacy shape and
    // decodes back to an empty trait field.
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
