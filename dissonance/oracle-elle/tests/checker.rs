// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task-75 gate 2 — the **checker**.
//!
//! Planted histories catch each rung of the anomaly ladder with the right
//! constructive witness (G0 dirty write, G1a aborted read, lost update); a
//! serializable history passes clean; level gating is exact; and property tests
//! (>=256 cases) pin lost-update recovery, serial-clean, and verdict
//! determinism (the same `RunTrace` judged twice yields byte-equal verdicts).

mod common;

use common::{abort, append, commit, read, trace, write};
use explorer::{Bug, Moment, Oracle};
use oracle_elle::{AnomalyKind, ElleOracle, EventDecoder, IsolationLevel};
use proptest::prelude::*;

fn oracle(level: IsolationLevel) -> ElleOracle {
    ElleOracle::new(Box::new(EventDecoder::new()), level)
}

// ---------------------------------------------------------------------------
// Planted-history unit tests — one witnessed rung each
// ---------------------------------------------------------------------------

/// Lost update: two committed transactions read the same version of `k` and both
/// write it. Caught at Snapshot Isolation, with both txns and the key as the
/// witness — and *not* caught at Read Committed (below the rung).
#[test]
fn lost_update_is_caught_with_its_witness() {
    let t = trace(
        vec![
            write(1, 0, 10, "k", 1), // T10: install version 1
            commit(2, 10),
            read(3, 1, 11, "k", &[1]), // T11 reads version 1
            write(4, 1, 11, "k", 2),   // ...and overwrites
            commit(5, 11),
            read(6, 2, 12, "k", &[1]), // T12 reads the SAME version 1
            write(7, 2, 12, "k", 3),   // ...and overwrites — lost update
            commit(8, 12),
        ],
        0,
    );

    let a = oracle(IsolationLevel::SnapshotIsolation)
        .analyze(&t)
        .expect("decodes")
        .expect("a lost update");
    assert_eq!(a.kind, AnomalyKind::LostUpdate);
    assert_eq!(
        a.txns,
        vec![11, 12],
        "both read-modify-writers are witnessed"
    );
    assert_eq!(a.keys, vec![b"k".to_vec()]);
    assert_eq!(a.at, Moment(4), "earliest conflicting write");

    // Below the rung: Read Committed does not forbid lost updates.
    assert!(
        oracle(IsolationLevel::ReadCommitted)
            .analyze(&t)
            .expect("decodes")
            .is_none()
    );
}

/// G0 dirty write: two committed transactions wrote two append keys in
/// conflicting orders (a ww cycle). Caught at every level.
#[test]
fn dirty_write_cycle_is_caught_with_its_witness() {
    let t = trace(
        vec![
            append(1, 1, 21, "a", 1), // T21: a<-1
            append(2, 1, 21, "b", 2), // T21: b<-2
            commit(3, 21),
            append(4, 2, 22, "b", 3), // T22: b<-3
            append(5, 2, 22, "a", 4), // T22: a<-4
            commit(6, 22),
            // A final observer fixes each key's version order — and they
            // conflict: on `a`, T22 before T21; on `b`, T21 before T22.
            read(7, 3, 23, "a", &[4, 1]),
            read(8, 3, 23, "b", &[2, 3]),
            commit(9, 23),
        ],
        0,
    );

    let a = oracle(IsolationLevel::ReadUncommitted)
        .analyze(&t)
        .expect("decodes")
        .expect("a dirty write");
    assert_eq!(a.kind, AnomalyKind::DirtyWrite);
    assert_eq!(a.txns, vec![21, 22], "both writers are on the cycle");
    assert_eq!(a.keys, vec![b"a".to_vec(), b"b".to_vec()]);
}

/// Round-2 P1 (the register false-clean): two committed transactions wrote two
/// **register** keys in conflicting orders — a provable ww cycle — but the
/// conflicting versions (`a=4`, `b=2`) are never read back. Register order is
/// recovered from write **moments** (not just observed values), so both writes
/// are placed and the cycle is caught, not judged clean.
///
/// Codex's counterexample: T21 writes b then a; T22 writes a then b, interleaved
/// so `a`'s order is [4(T22), 1(T21)] and `b`'s is [2(T21), 3(T22)] — T22→T21 on
/// a, T21→T22 on b: a cycle.
#[test]
fn register_conflicting_write_order_is_a_dirty_write() {
    let t = trace(
        vec![
            write(1, 1, 21, "b", 2), // T21: b<-2 (early)
            write(2, 2, 22, "a", 4), // T22: a<-4 (early)
            write(3, 2, 22, "b", 3), // T22: b<-3 (late → b order [2,3])
            write(4, 1, 21, "a", 1), // T21: a<-1 (late → a order [4,1])
            commit(5, 21),
            commit(6, 22),
            // Final reads pin the last version of each (a=1, b=3); the conflict
            // is already provable from the write moments regardless.
            read(7, 3, 23, "a", &[1]),
            read(8, 3, 23, "b", &[3]),
            commit(9, 23),
        ],
        0,
    );
    let a = oracle(IsolationLevel::ReadUncommitted)
        .analyze(&t)
        .expect("decodes")
        .expect("the register dirty-write cycle (not a false clean)");
    assert_eq!(a.kind, AnomalyKind::DirtyWrite);
    assert_eq!(a.txns, vec![21, 22], "both writers are on the cycle");
    assert_eq!(a.keys, vec![b"a".to_vec(), b"b".to_vec()]);
}

/// G1a aborted read: a committed transaction read a value an aborted transaction
/// wrote. Caught at Read Committed and above; not at Read Uncommitted.
#[test]
fn aborted_read_is_caught_with_its_witness() {
    let t = trace(
        vec![
            write(1, 1, 31, "x", 9),   // T31 writes x=9...
            abort(2, 31),              // ...then aborts
            read(3, 2, 32, "x", &[9]), // T32 reads the aborted value
            commit(4, 32),
        ],
        0,
    );

    let a = oracle(IsolationLevel::ReadCommitted)
        .analyze(&t)
        .expect("decodes")
        .expect("an aborted read");
    assert_eq!(a.kind, AnomalyKind::AbortedRead);
    assert_eq!(a.txns, vec![31, 32]);
    assert_eq!(a.keys, vec![b"x".to_vec()]);
    assert_eq!(a.at, Moment(3), "the reading op's moment");

    // Read Uncommitted permits reading uncommitted/aborted data.
    assert!(
        oracle(IsolationLevel::ReadUncommitted)
            .analyze(&t)
            .expect("decodes")
            .is_none()
    );
}

/// A strictly serial history — each transaction reads the previous committed
/// version and installs the next — is clean at the strongest level.
#[test]
fn a_serial_history_is_clean() {
    let t = trace(
        vec![
            write(1, 1, 41, "k", 1),
            commit(2, 41),
            read(3, 1, 42, "k", &[1]),
            write(4, 1, 42, "k", 2),
            commit(5, 42),
            read(6, 1, 43, "k", &[2]),
            write(7, 1, 43, "k", 3),
            commit(8, 43),
        ],
        0,
    );
    assert!(
        oracle(IsolationLevel::Serializable)
            .analyze(&t)
            .expect("decodes")
            .is_none(),
        "a serial read-modify-write chain has no anomaly"
    );
}

/// The reported `Bug` carries the run's own terminal stop and a fingerprint that
/// mints through the shared schema — the finding lives in the terminal
/// signature, not a fabricated stop.
#[test]
fn reported_bug_uses_the_runs_terminal_and_a_stable_fingerprint() {
    let t = trace(
        vec![
            write(1, 0, 10, "k", 1),
            commit(2, 10),
            read(3, 1, 11, "k", &[1]),
            write(4, 1, 11, "k", 2),
            commit(5, 11),
            read(6, 2, 12, "k", &[1]),
            write(7, 2, 12, "k", 3),
            commit(8, 12),
        ],
        0,
    );
    let o = oracle(IsolationLevel::SnapshotIsolation);
    let bug: Bug = o.judge(&t).expect("a bug");
    assert_eq!(bug.stop, t.terminal, "the run's own terminal stop");
    assert_eq!(bug.env, t.env, "the run's genesis-complete reproducer");
    // Judging again yields a byte-equal fingerprint.
    assert_eq!(bug.fingerprint, o.judge(&t).unwrap().fingerprint);
}

/// Round-1 review, item 5 at the oracle layer: an append dirty-write history
/// whose recovered order is incomplete (one key's final read missing) must
/// **not** be judged clean. `analyze` surfaces the recoverability failure loud
/// instead of returning `Ok(None)` — the false-clean the review flagged.
#[test]
fn incomplete_append_history_is_not_judged_clean() {
    let t = trace(
        vec![
            append(1, 1, 21, "a", 1),
            append(2, 1, 21, "b", 2),
            commit(3, 21),
            append(4, 2, 22, "b", 3),
            append(5, 2, 22, "a", 4),
            commit(6, 22),
            read(7, 3, 23, "a", &[4, 1]), // `b` is never read back
            commit(8, 23),
        ],
        0,
    );
    // A dirty write is forbidden at every level; the run must not read as clean.
    let verdict = oracle(IsolationLevel::ReadUncommitted).analyze(&t);
    assert!(
        verdict.is_err(),
        "an incomplete order must fail loud, not judge clean: {verdict:?}"
    );
}

// ---------------------------------------------------------------------------
// Property tests (>= 256 cases)
// ---------------------------------------------------------------------------

fn cfg() -> ProptestConfig {
    ProptestConfig::with_cases(256)
}

/// Build a history with a **planted lost update** on key `"k"` (two txns read
/// version `v0` and both overwrite) plus `noise` anomaly-free blind writes, each
/// on its own unique key. The planted anomaly is the only one.
fn planted_lost_update(v0: i64, noise: u8) -> explorer::RunTrace {
    let va = v0 + 1_000;
    let vb = v0 + 2_000;
    let mut ev = vec![
        write(1, 0, 10, "k", v0),
        commit(2, 10),
        read(3, 1, 11, "k", &[v0]),
        write(4, 1, 11, "k", va),
        commit(5, 11),
        read(6, 2, 12, "k", &[v0]),
        write(7, 2, 12, "k", vb),
        commit(8, 12),
    ];
    for i in 0..noise as u64 {
        // A blind write to a unique key with a globally-unique value: no read,
        // so it is never a lost-update participant; a unique key, so no ww edge.
        let key = format!("noise{i}");
        let at = 10 + i * 2;
        ev.push(write(at, 9, 100 + i, &key, 100_000 + i as i64));
        ev.push(commit(at + 1, 100 + i));
    }
    trace(ev, noise)
}

proptest! {
    #![proptest_config(cfg())]

    /// A seeded lost-update history is caught at SI with the right witness,
    /// whatever anomaly-free noise surrounds it — and re-judging is byte-equal
    /// (verdict determinism on a real anomaly).
    #[test]
    fn lost_update_is_always_caught_with_witness(v0 in 1i64..500, noise in 0u8..8) {
        let t = planted_lost_update(v0, noise);
        let o = oracle(IsolationLevel::SnapshotIsolation);
        let a = o.analyze(&t).expect("decodes").expect("the planted lost update");
        prop_assert_eq!(a.kind, AnomalyKind::LostUpdate);
        prop_assert_eq!(&a.txns, &vec![11u64, 12]);
        prop_assert_eq!(&a.keys, &vec![b"k".to_vec()]);

        // Verdict determinism: a fresh oracle judges the same trace identically.
        let b1 = o.judge(&t).expect("bug");
        let b2 = oracle(IsolationLevel::SnapshotIsolation).judge(&t).expect("bug");
        prop_assert_eq!(b1.fingerprint, b2.fingerprint);
        prop_assert_eq!(&b1.env.bytes, &b2.env.bytes);
        prop_assert_eq!(b1.stop, b2.stop);
    }

    /// A generated strictly-serial read-modify-write chain of any length is clean
    /// at the strongest level.
    #[test]
    fn serial_chains_are_always_clean(len in 1usize..12) {
        let mut ev = vec![write(1, 1, 1, "k", 1), commit(2, 1)];
        let mut at = 3u64;
        for step in 1..len as u64 {
            let txn = step + 1;
            ev.push(read(at, 1, txn, "k", &[step as i64]));
            ev.push(write(at + 1, 1, txn, "k", (step + 1) as i64));
            ev.push(commit(at + 2, txn));
            at += 3;
        }
        let t = trace(ev, 0);
        prop_assert!(
            oracle(IsolationLevel::Serializable)
                .analyze(&t)
                .expect("decodes")
                .is_none()
        );
    }

    /// Judging is a pure, deterministic function: an arbitrary (possibly
    /// unrecoverable) history judged twice yields byte-equal verdicts, and
    /// `analyze` agrees with itself.
    #[test]
    fn judging_is_deterministic(ops in prop::collection::vec(arb_action(), 0..14)) {
        let t = assemble(ops);
        let o1 = oracle(IsolationLevel::SnapshotIsolation);
        let o2 = oracle(IsolationLevel::SnapshotIsolation);
        prop_assert_eq!(verdict_bytes(&o1, &t), verdict_bytes(&o2, &t));
        prop_assert_eq!(o1.analyze(&t).is_ok(), o2.analyze(&t).is_ok());
    }
}

/// One generated action: a committed/aborted txn doing a read-or-write of a
/// small key with a value, at an increasing moment.
#[derive(Clone, Debug)]
struct Action {
    txn: u64,
    key: u8,
    write: bool,
    value: i64,
    commit: bool,
}

fn arb_action() -> impl Strategy<Value = Action> {
    (0u64..4, 0u8..3, any::<bool>(), 0i64..6, any::<bool>()).prop_map(
        |(txn, key, write, value, commit)| Action {
            txn,
            key,
            write,
            value,
            commit,
        },
    )
}

/// Assemble generated actions into a trace, assigning increasing moments and a
/// terminal commit/abort per touched txn. Values are used as-is, so histories
/// may be unrecoverable (duplicate/unknown values) — exactly the fail-loud path
/// determinism must survive.
fn assemble(actions: Vec<Action>) -> explorer::RunTrace {
    use std::collections::BTreeMap;
    let mut ev = Vec::new();
    let mut at = 1u64;
    let mut outcome: BTreeMap<u64, bool> = BTreeMap::new();
    for a in &actions {
        let key = format!("k{}", a.key);
        if a.write {
            ev.push(write(at, a.txn, a.txn, &key, a.value));
        } else {
            ev.push(read(at, a.txn, a.txn, &key, &[a.value]));
        }
        outcome
            .entry(a.txn)
            .and_modify(|c| *c &= a.commit)
            .or_insert(a.commit);
        at += 1;
    }
    for (txn, committed) in outcome {
        if committed {
            ev.push(commit(at, txn));
        } else {
            ev.push(abort(at, txn));
        }
        at += 1;
    }
    trace(ev, 0)
}

/// A byte-comparable rendering of a judge verdict for determinism assertions.
fn verdict_bytes(o: &ElleOracle, t: &explorer::RunTrace) -> Vec<u8> {
    match o.judge(t) {
        None => vec![0],
        Some(b) => {
            let mut v = vec![1];
            v.extend_from_slice(&b.fingerprint);
            v.extend_from_slice(&b.env.bytes);
            v
        }
    }
}
