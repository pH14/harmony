// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task-75 — the **`OpDecode` seam**: the record and event decoders agree, and
//! an unrecoverable/malformed history fails loud (never a guessed anomaly).

mod common;

use common::{commit, read, trace, write};
use explorer::{Environment, Moment, Record, RunTrace, StopReason, StreamId, VTime};
use oracle_elle::{DecodeError, DepGraph, EventDecoder, OpDecode, RecordDecoder};

/// Build a records-backed trace from `elle ...` lines (plus interleaved noise
/// the decoder must ignore).
fn record_trace(lines: &[(u64, &str)]) -> RunTrace {
    let records = lines
        .iter()
        .map(|(at, l)| {
            (
                Moment(*at),
                Record {
                    stream: StreamId(0),
                    line: format!("{l}\n").into_bytes(),
                },
            )
        })
        .collect();
    RunTrace {
        terminal: StopReason::Quiescent { vtime: VTime(100) },
        env: Environment {
            blob_version: 1,
            bytes: vec![],
        },
        coverage: None,
        events: Vec::new(),
        records,
    }
}

/// The record (line) decoder and the event decoder recover the *same* history
/// from the same logical operations — the seam is source-agnostic.
#[test]
fn record_and_event_decoders_agree() {
    let by_records = record_trace(&[
        (1, "some unrelated log line"), // ignored (no `elle ` prefix)
        (1, "elle op s=0 t=10 k=k W=1"),
        (2, "elle commit t=10"),
        (3, "elle op s=1 t=11 k=k R=1"),
        (4, "elle op s=1 t=11 k=k W=2"),
        (5, "elle commit t=11"),
    ]);
    let by_events = trace(
        vec![
            write(1, 0, 10, "k", 1),
            commit(2, 10),
            read(3, 1, 11, "k", &[1]),
            write(4, 1, 11, "k", 2),
            commit(5, 11),
        ],
        0,
    );
    let from_records = RecordDecoder::new()
        .decode(&by_records)
        .expect("records decode");
    let from_events = EventDecoder::new()
        .decode(&by_events)
        .expect("events decode");
    assert_eq!(from_records, from_events);
}

/// A read observing an empty list decodes as a read of an unwritten key.
#[test]
fn empty_read_list_is_an_unwritten_read() {
    let t = record_trace(&[(1, "elle op s=1 t=11 k=k R="), (2, "elle commit t=11")]);
    let h = RecordDecoder::new().decode(&t).expect("decode");
    let txn = h.txns.get(&11).expect("txn 11");
    assert_eq!(txn.ops.len(), 1);
    assert_eq!(txn.ops[0].observed(), Some(&[][..]));
}

/// A value written by two transactions is non-unique — unrecoverable.
#[test]
fn duplicate_value_fails_loud() {
    let t = trace(
        vec![
            write(1, 1, 1, "a", 7),
            commit(2, 1),
            write(3, 2, 2, "b", 7), // same value 7 — non-unique
            commit(4, 2),
        ],
        0,
    );
    let h = EventDecoder::new()
        .decode(&t)
        .expect("decodes into a history");
    match DepGraph::build(&h) {
        Err(DecodeError::DuplicateValue { value: 7, .. }) => {}
        other => panic!("expected DuplicateValue, got {other:?}"),
    }
}

/// A read of a value no write produced is unrecoverable.
#[test]
fn unknown_read_value_fails_loud() {
    let t = trace(vec![read(1, 1, 1, "k", &[42]), commit(2, 1)], 0);
    let h = EventDecoder::new().decode(&t).expect("decodes");
    match DepGraph::build(&h) {
        Err(DecodeError::UnknownValue { value: 42, .. }) => {}
        other => panic!("expected UnknownValue, got {other:?}"),
    }
}

/// A transaction that issued ops but never committed/aborted is unrecoverable.
#[test]
fn unterminated_txn_fails_loud() {
    let t = record_trace(&[(1, "elle op s=1 t=11 k=k W=1")]); // no commit/abort
    match RecordDecoder::new().decode(&t) {
        Err(DecodeError::UnterminatedTxn(11)) => {}
        other => panic!("expected UnterminatedTxn, got {other:?}"),
    }
}

/// Reads of an append key that disagree on version order (a fork) are
/// unrecoverable.
#[test]
fn inconsistent_append_order_fails_loud() {
    // Two appends to `k` (values 1, 2); one reader sees [1,2], another sees
    // [2,1] — neither a prefix of the other.
    let t = trace(
        vec![
            common::append(1, 1, 1, "k", 1),
            common::append(2, 2, 2, "k", 2),
            commit(3, 1),
            commit(4, 2),
            read(5, 3, 3, "k", &[1, 2]),
            read(6, 4, 4, "k", &[2, 1]),
            commit(7, 3),
            commit(8, 4),
        ],
        0,
    );
    let h = EventDecoder::new().decode(&t).expect("decodes");
    match DepGraph::build(&h) {
        Err(DecodeError::InconsistentOrder { .. }) => {}
        other => panic!("expected InconsistentOrder, got {other:?}"),
    }
}

/// A malformed `elle` line fails loud.
#[test]
fn malformed_line_fails_loud() {
    let t = record_trace(&[
        (1, "elle op s=1 t=notanumber k=k W=1"),
        (2, "elle commit t=1"),
    ]);
    match RecordDecoder::new().decode(&t) {
        Err(DecodeError::Malformed(_)) => {}
        other => panic!("expected Malformed, got {other:?}"),
    }
}
