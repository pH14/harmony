// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 4 — version-bump compatibility.
//!
//! A golden journal is pinned **byte-for-byte** (`tests/fixtures/golden_v1.trace`),
//! so any accidental change to the on-disk layout is a failing diff. A synthetic
//! envelope carrying a *different* format version decodes to a loud
//! [`TraceError::Version`] — never a silent reinterpretation of old bytes under
//! a new schema.
//!
//! Refresh the golden after an intentional, reviewed format change:
//!   `UPDATE_FIXTURES=1 cargo test -p runtrace --test version_bump`
//! and bump [`runtrace::TRACE_FORMAT_VERSION`] (the bump procedure, mirroring
//! `control-proto`'s `PROTO_VERSION`, is documented in `IMPLEMENTATION.md`).

use std::collections::BTreeMap;

use explorer::{
    CoverageView, GuestEvent, Moment, Record, Reproducer, RunTrace, StopReason, StreamId, Value,
};
use runtrace::{TRACE_FORMAT_VERSION, TraceError, decode, encode};

const GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/golden_v1.trace"
);

/// A fixed, hand-built trace exercising every field: a `Crash` terminal, an env
/// blob, a coverage map, a non-empty (task-73-shaped) event, and three console
/// records (one with a non-UTF-8 byte, one unterminated). Deterministic, so its
/// encoding is a stable golden.
fn golden_trace() -> RunTrace {
    let mut attrs = BTreeMap::new();
    attrs.insert("lsn".to_string(), Value::UInt(42));
    attrs.insert("ok".to_string(), Value::Bool(true));
    RunTrace {
        terminal: StopReason::Crash {
            vtime: Moment(5_000_000),
            info: vec![0xDE, 0xAD],
        },
        env: Reproducer {
            blob_version: 3,
            bytes: vec![1, 2, 3, 4, 5],
        },
        coverage: Some(CoverageView {
            map: vec![0, 1, 0, 9, 0],
        }),
        events: vec![(
            Moment(1234),
            GuestEvent {
                kind: "assert_sometimes".to_string(),
                attrs,
            },
        )],
        records: vec![
            (
                Moment(5_000_000),
                Record {
                    stream: StreamId(0),
                    line: b"database system is ready to accept connections\n".to_vec(),
                },
            ),
            (
                Moment(5_000_000),
                Record {
                    stream: StreamId(0),
                    line: vec![b'b', 0xFF, b'\n'],
                },
            ),
            (
                Moment(5_000_000),
                Record {
                    stream: StreamId(0),
                    line: b"trailing".to_vec(),
                },
            ),
        ],
    }
}

#[test]
fn golden_journal_is_pinned_byte_for_byte() {
    let bytes = encode(&golden_trace()).expect("golden encodes");

    if std::env::var_os("UPDATE_FIXTURES").is_some() {
        std::fs::write(GOLDEN_PATH, &bytes).expect("write golden fixture");
        eprintln!("updated {GOLDEN_PATH} ({} bytes)", bytes.len());
        return;
    }

    let pinned = std::fs::read(GOLDEN_PATH).expect(
        "read golden fixture — regenerate with `UPDATE_FIXTURES=1 cargo test -p runtrace --test version_bump`",
    );
    assert_eq!(
        bytes, pinned,
        "the journal byte layout drifted from the golden fixture; if intentional, bump \
         TRACE_FORMAT_VERSION and refresh with UPDATE_FIXTURES=1"
    );

    // And it round-trips back to the exact trace.
    let back = decode(&pinned).expect("golden decodes");
    assert_eq!(back, golden_trace());
}

#[test]
fn a_bumped_version_envelope_is_a_loud_version_error() {
    let mut bytes = encode(&golden_trace()).expect("golden encodes");
    // The version field is bytes [4..6] (after the 4-byte magic), little-endian.
    let bumped: u16 = TRACE_FORMAT_VERSION.wrapping_add(0x100);
    bytes[4..6].copy_from_slice(&bumped.to_le_bytes());

    match decode(&bytes) {
        Err(TraceError::Version { found, supported }) => {
            assert_eq!(found, bumped);
            assert_eq!(supported, TRACE_FORMAT_VERSION);
        }
        other => panic!("expected a loud TraceError::Version, got {other:?}"),
    }
}

#[test]
fn malformed_journals_are_loud_not_panics() {
    // Empty / too-short → Truncated.
    assert!(matches!(decode(&[]), Err(TraceError::Truncated)));
    assert!(matches!(decode(&[1, 2, 3]), Err(TraceError::Truncated)));

    // Right length header, wrong magic → Magic.
    let mut wrong_magic = encode(&golden_trace()).expect("golden encodes");
    wrong_magic[0] ^= 0xFF;
    assert!(matches!(decode(&wrong_magic), Err(TraceError::Magic)));

    // A valid journal with an extra trailing byte → Trailing (non-canonical).
    let mut trailing = encode(&golden_trace()).expect("golden encodes");
    trailing.push(0);
    assert!(matches!(decode(&trailing), Err(TraceError::Trailing)));

    // Truncated payload (drop the last byte of a valid journal) → Truncated.
    let mut truncated = encode(&golden_trace()).expect("golden encodes");
    truncated.pop();
    assert!(matches!(decode(&truncated), Err(TraceError::Truncated)));
}
