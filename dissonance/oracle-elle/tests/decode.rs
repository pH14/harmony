// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task-75 — the **`OpDecode` seam**: the record and event decoders agree, and
//! an unrecoverable/malformed history fails loud (never a guessed anomaly).

mod common;

use common::{append, commit, read, trace, write};
use explorer::{Environment, Moment, Record, RunTrace, StopReason, StreamId, VTime};
use oracle_elle::{DecodeError, DepGraph, EventDecoder, OpDecode, RecordDecoder};

/// A records-backed trace from raw `(Moment, line-bytes)` pairs (for keys/values
/// that are not valid UTF-8).
fn raw_record_trace(lines: Vec<(u64, Vec<u8>)>) -> RunTrace {
    RunTrace {
        terminal: StopReason::Quiescent { vtime: VTime(100) },
        env: Environment {
            blob_version: 1,
            bytes: vec![],
        },
        coverage: None,
        events: Vec::new(),
        records: lines
            .into_iter()
            .map(|(at, line)| {
                (
                    Moment(at),
                    Record {
                        stream: StreamId(0),
                        line,
                    },
                )
            })
            .collect(),
    }
}

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

// --- round-1 review regressions (the false-clean class) ---

/// Item 1: record keys are kept **byte-exact**, never UTF-8-lossy-decoded — a
/// mangled key could collide two distinct keys and hide/fabricate an anomaly.
#[test]
fn record_keys_are_byte_exact() {
    // Key `x` + a non-UTF-8 byte 0xFF; must survive verbatim, not become
    // `x` + U+FFFD (bytes 0xEF 0xBF 0xBD).
    let mut line = b"elle op s=1 t=1 k=x".to_vec();
    line.push(0xFF);
    line.extend_from_slice(b" W=1");
    let t = raw_record_trace(vec![(1, line), (2, b"elle commit t=1".to_vec())]);
    let h = RecordDecoder::new().decode(&t).expect("decode");
    let op = &h.txns.get(&1).expect("txn 1").ops[0];
    assert_eq!(op.key, vec![b'x', 0xFF], "key bytes are verbatim");
    assert_ne!(
        op.key,
        vec![b'x', 0xEF, 0xBF, 0xBD],
        "not the lossy-mangled form"
    );
}

/// Item 2: an op carrying more than one of W/A/R is ambiguous — a loud
/// `AmbiguousOp`, never a silently-picked (mis-classified) kind. Both decoders.
#[test]
fn ambiguous_op_payload_fails_loud() {
    let by_record = record_trace(&[(1, "elle op s=1 t=1 k=k W=1 R=1"), (2, "elle commit t=1")]);
    match RecordDecoder::new().decode(&by_record) {
        Err(DecodeError::AmbiguousOp { txn: 1 }) => {}
        other => panic!("record: expected AmbiguousOp, got {other:?}"),
    }
    // The event path too: both a W and an R attribute on one op event.
    let op_ev = explorer::GuestEvent {
        kind: "op".into(),
        attrs: [
            ("s".into(), explorer::Value::UInt(1)),
            ("t".into(), explorer::Value::UInt(1)),
            ("k".into(), explorer::Value::Str("k".into())),
            ("W".into(), explorer::Value::Int(1)),
            ("R".into(), explorer::Value::Str("1".into())),
        ]
        .into_iter()
        .collect(),
    };
    let commit_ev = explorer::GuestEvent {
        kind: "commit".into(),
        attrs: [("t".into(), explorer::Value::UInt(1))]
            .into_iter()
            .collect(),
    };
    let t = RunTrace {
        terminal: StopReason::Quiescent { vtime: VTime(10) },
        env: Environment {
            blob_version: 1,
            bytes: vec![],
        },
        coverage: None,
        events: vec![(Moment(1), op_ev), (Moment(2), commit_ev)],
        records: vec![],
    };
    match EventDecoder::new().decode(&t) {
        Err(DecodeError::AmbiguousOp { txn: 1 }) => {}
        other => panic!("event: expected AmbiguousOp, got {other:?}"),
    }
}

/// Item 3: contradictory lifecycle markers (a commit AND an abort for one txn)
/// are a loud `ConflictingLifecycle`, never last-wins (which could flip a bug's
/// visibility).
#[test]
fn conflicting_lifecycle_fails_loud() {
    let t = record_trace(&[
        (1, "elle op s=1 t=1 k=k W=1"),
        (2, "elle commit t=1"),
        (3, "elle abort t=1"), // contradicts the commit
    ]);
    match RecordDecoder::new().decode(&t) {
        Err(DecodeError::ConflictingLifecycle { txn: 1, .. }) => {}
        other => panic!("expected ConflictingLifecycle, got {other:?}"),
    }
}

/// Item 3 corollary: an *idempotent* repeat of the same marker is harmless (only
/// contradictory markers fail).
#[test]
fn duplicate_identical_lifecycle_is_idempotent() {
    let t = record_trace(&[
        (1, "elle op s=1 t=1 k=k W=1"),
        (2, "elle commit t=1"),
        (3, "elle commit t=1"), // same outcome — idempotent
    ]);
    let h = RecordDecoder::new().decode(&t).expect("decode");
    assert!(h.txns.get(&1).expect("txn 1").committed());
}

/// Item 4: a value written to one key but observed under another must not join
/// that key's order — a loud `MisattributedValue`.
#[test]
fn cross_key_value_attribution_fails_loud() {
    let t = trace(
        vec![
            write(1, 1, 1, "a", 7), // value 7 written to key `a`
            commit(2, 1),
            read(3, 2, 2, "b", &[7]), // ...but observed under key `b`
            commit(4, 2),
        ],
        0,
    );
    let h = EventDecoder::new()
        .decode(&t)
        .expect("decodes into a history");
    match DepGraph::build(&h) {
        Err(DecodeError::MisattributedValue { value: 7, .. }) => {}
        other => panic!("expected MisattributedValue, got {other:?}"),
    }
}

/// Item 5 (the sharpest): an append dirty-write cycle where one key's final read
/// is **missing** — its version order is incomplete, so the ww cycle can't be
/// recovered. This must fail loud (`UnobservedAppend`), never build a partial
/// graph that judges the run clean. A positive control confirms that *with* the
/// final read the cycle is recovered (no false negative the other way).
#[test]
fn append_missing_final_read_fails_loud() {
    // T21: a<-1, b<-2 ; T22: b<-3, a<-4 — a conflicting-order (G0) pair. Only
    // `a` is read back; `b`'s appends 2,3 are never observed → b's order is
    // incomplete.
    let incomplete = vec![
        append(1, 1, 21, "a", 1),
        append(2, 1, 21, "b", 2),
        commit(3, 21),
        append(4, 2, 22, "b", 3),
        append(5, 2, 22, "a", 4),
        commit(6, 22),
        read(7, 3, 23, "a", &[4, 1]), // a observed; b NOT observed
        commit(8, 23),
    ];
    let t = trace(incomplete.clone(), 0);
    let h = EventDecoder::new()
        .decode(&t)
        .expect("decodes into a history");
    match DepGraph::build(&h) {
        Err(DecodeError::UnobservedAppend { value, .. }) => {
            assert!(value == 2 || value == 3, "one of b's unobserved appends");
        }
        other => panic!("expected UnobservedAppend, got {other:?}"),
    }

    // Positive control: add b's final read; the order completes and the cycle
    // is recovered.
    let mut full = incomplete;
    full.insert(7, read(8, 3, 23, "b", &[2, 3])); // before the commit at 8
    let h2 = EventDecoder::new()
        .decode(&trace(full, 0))
        .expect("decodes");
    assert!(
        DepGraph::build(&h2)
            .expect("recoverable")
            .ww_cycle()
            .is_some(),
        "with the final read, the dirty-write cycle is recovered"
    );
}
