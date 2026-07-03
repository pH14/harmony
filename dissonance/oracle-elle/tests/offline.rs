// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task-75 gate 3 — the **strong offline property**.
//!
//! A stored `RunTrace` corpus is re-judged with a **new** oracle and surfaces a
//! planted anomaly with **zero VM time**: the corpus is written to disk, read
//! back, and judged by a freshly-constructed [`ElleOracle`] that never existed
//! when the runs were recorded — and a verb-counting mock [`Machine`] held
//! alongside the judging path records **zero** calls, because
//! [`Oracle::judge`](explorer::Oracle::judge) takes a `RunTrace`, not a guest.
//!
//! The corpus round-trips through the versioned `RunTrace` serde encoding (the
//! shape task 65's `TraceStore` persists); the offline property is
//! format-independent — what matters is that judging is pure over the recorded
//! trace.

mod common;

use common::{CountingMachine, append, commit, read, trace, write};
use explorer::{Oracle, RunTrace};
use oracle_elle::{AnomalyKind, ElleOracle, EventDecoder, IsolationLevel};

/// One clean serial run and one run with a planted lost update — the corpus a
/// campaign would have recorded (with, say, only a crash oracle at the time).
fn corpus() -> Vec<RunTrace> {
    let clean = trace(
        vec![
            write(1, 1, 1, "k", 1),
            commit(2, 1),
            read(3, 1, 2, "k", &[1]),
            write(4, 1, 2, "k", 2),
            commit(5, 2),
        ],
        0,
    );
    let anomalous = trace(
        vec![
            write(1, 0, 10, "acct", 100),
            commit(2, 10),
            read(3, 1, 11, "acct", &[100]),
            write(4, 1, 11, "acct", 101),
            commit(5, 11),
            read(6, 2, 12, "acct", &[100]), // same version → lost update
            write(7, 2, 12, "acct", 102),
            commit(8, 12),
        ],
        1,
    );
    // A G0 dirty-write run too, so re-judging surfaces more than one class.
    let dirty = trace(
        vec![
            append(1, 1, 21, "a", 1),
            append(2, 1, 21, "b", 2),
            commit(3, 21),
            append(4, 2, 22, "b", 3),
            append(5, 2, 22, "a", 4),
            commit(6, 22),
            read(7, 3, 23, "a", &[4, 1]),
            read(8, 3, 23, "b", &[2, 3]),
            commit(9, 23),
        ],
        2,
    );
    vec![clean, anomalous, dirty]
}

/// Write a corpus to a directory as one JSON file per run, then read it back in
/// a deterministic order — a "stored corpus" on disk.
fn store_and_reload(corpus: &[RunTrace], dir: &std::path::Path) -> Vec<RunTrace> {
    for (i, t) in corpus.iter().enumerate() {
        let path = dir.join(format!("trace-{i:04}.json"));
        let bytes = serde_json::to_vec(t).expect("serialize run");
        std::fs::write(&path, bytes).expect("write run");
    }
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .expect("read dir")
        .map(|e| e.expect("dir entry").path())
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|p| {
            serde_json::from_slice(&std::fs::read(&p).expect("read run")).expect("deserialize")
        })
        .collect()
}

/// The gate: a NEW oracle re-judges the stored corpus, surfaces the planted
/// anomalies, and the witness machine records zero verb calls.
#[test]
fn a_new_oracle_rejudges_a_stored_corpus_with_zero_vm_time() {
    let dir = tempfile::tempdir().expect("tempdir");
    let original = corpus();
    let reloaded = store_and_reload(&original, dir.path());
    assert_eq!(reloaded, original, "the corpus round-trips through storage");

    // A guest that would witness any VM time. The offline judge path never
    // receives it — proving zero VM time by construction.
    let witness = CountingMachine::new();

    // A freshly-constructed oracle: it did not exist when the runs were
    // recorded, yet it finds the planted bugs.
    let oracle = ElleOracle::new(
        Box::new(EventDecoder::new()),
        IsolationLevel::SnapshotIsolation,
    );

    let mut bugs = Vec::new();
    for t in &reloaded {
        if let Some(bug) = oracle.judge(t) {
            bugs.push(bug);
        }
    }

    // The clean run is clean; the lost-update and dirty-write runs are caught.
    assert_eq!(bugs.len(), 2, "exactly the two planted anomalies surface");
    // Their classes, read straight off the (re-judged, offline) analyses.
    let mut classes: Vec<AnomalyKind> = reloaded
        .iter()
        .filter_map(|t| oracle.analyze(t).expect("decodes").map(|a| a.kind))
        .collect();
    classes.sort_by_key(|k| k.class());
    assert_eq!(
        classes,
        vec![AnomalyKind::DirtyWrite, AnomalyKind::LostUpdate]
    );

    // Zero VM time: the witness machine was never driven.
    assert_eq!(witness.calls(), 0, "judging touched no guest");

    // The offline property is lossless: judging the in-memory corpus and the
    // disk-reloaded corpus yields byte-identical fingerprints.
    let inmem: Vec<_> = original.iter().filter_map(|t| oracle.judge(t)).collect();
    assert_eq!(inmem.len(), bugs.len());
    for (a, b) in inmem.iter().zip(&bugs) {
        assert_eq!(a.fingerprint, b.fingerprint);
        assert_eq!(a.env.bytes, b.env.bytes);
    }
}

/// A second, *distinct* oracle (a stricter declared level) re-judged over the
/// very same stored corpus yields a different-but-still-zero-VM verdict set —
/// the whole point of the offline plane: new checkers, no re-execution.
#[test]
fn re_judging_at_a_different_level_needs_no_re_execution() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reloaded = store_and_reload(&corpus(), dir.path());
    let witness = CountingMachine::new();

    // Read Uncommitted forbids only dirty writes, so only the G0 run is a bug.
    let lax = ElleOracle::new(
        Box::new(EventDecoder::new()),
        IsolationLevel::ReadUncommitted,
    );
    let bugs: Vec<_> = reloaded.iter().filter_map(|t| lax.judge(t)).collect();
    assert_eq!(bugs.len(), 1, "only the dirty write is forbidden at RU");
    assert_eq!(witness.calls(), 0);
}
