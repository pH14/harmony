// SPDX-License-Identifier: AGPL-3.0-or-later
//! Committed-fixture gates (the task's Environment section): decode the
//! real recordings and re-derive over them offline — the replay-plane path,
//! exercised on artifacts produced *outside* this crate.
//!
//! - `mock_recording.trace` — a real mock-mode `conductor` recording, committed
//!   here (regenerate with
//!   `UPDATE_FIXTURES=1 cargo test -p conductor --test recording`).
//! - `real_guest_slice.trace` — a trimmed real-guest journal committed by the
//!   **box gate** (task 65 gate 6). Absent until the box run lands; the test
//!   skips loudly rather than failing so the portable suite stays green.

mod common;

use common::MarkerSensor;
use explorer::Sensor;
use runtrace::{decode, encode};

const MOCK: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/mock_recording.trace"
);
const REAL: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/real_guest_slice.trace"
);

#[test]
fn mock_recording_decodes_reencodes_and_rederives() {
    let bytes = std::fs::read(MOCK).expect(
        "mock recording fixture — regenerate with `UPDATE_FIXTURES=1 cargo test -p conductor --test recording`",
    );
    let trace = decode(&bytes).expect("mock recording decodes");

    // Canonical: re-encoding the decoded trace reproduces the committed bytes.
    assert_eq!(encode(&trace), bytes, "the committed journal is canonical");

    // The recording fork writes the "MOCK-READY" banner, so the console decodes
    // into at least one record and the marker sensor finds it.
    assert!(
        !trace.records.is_empty(),
        "the mock recording has console records"
    );
    let sensor = MarkerSensor::new(b"MOCK-READY");
    let features = sensor.observe(&trace);
    assert!(
        !features.is_empty(),
        "the readiness marker is present in the records"
    );

    // Re-derivation is stable across a decode round-trip.
    let reloaded = decode(&encode(&trace)).unwrap();
    assert_eq!(sensor.observe(&reloaded), features);
}

#[test]
fn real_guest_slice_decodes_and_rederives_when_present() {
    let Ok(bytes) = std::fs::read(REAL) else {
        eprintln!(
            "SKIP: real_guest_slice.trace absent — committed by the box gate (task 65 gate 6)"
        );
        return;
    };
    let trace = decode(&bytes).expect("real-guest slice decodes");
    assert_eq!(encode(&trace), bytes, "the committed slice is canonical");
    assert!(
        !trace.records.is_empty(),
        "the real-guest slice has console records"
    );
    // Re-derive with the readiness-banner marker (present in a Postgres console).
    let sensor = MarkerSensor::new(b"database system is ready to accept connections");
    let reloaded = decode(&encode(&trace)).unwrap();
    assert_eq!(sensor.observe(&reloaded), sensor.observe(&trace));
}
