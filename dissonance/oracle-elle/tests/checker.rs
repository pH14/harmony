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
use oracle_elle::{AnomalyKind, DepGraph, ElleOracle, EventDecoder, IsolationLevel, OpDecode};
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
            read(9, 3, 13, "k", &[3]), // final read at quiesce pins the order
            commit(10, 13),
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

/// Round-8 P2 (`check_all` contract): the enumeration API returns **every**
/// independent forbidden anomaly — two lost updates on different keys yield two
/// witnesses, not one. (`analyze`/`judge` still report the canonical first as one
/// `Bug`; this pins the fuller `check_all`.)
#[test]
fn check_all_returns_every_independent_lost_update() {
    let t = trace(
        vec![
            // Lost update on k1: T11, T12 both read version 1, both write.
            write(1, 0, 10, "k1", 1),
            commit(2, 10),
            read(3, 1, 11, "k1", &[1]),
            write(4, 1, 11, "k1", 2),
            commit(5, 11),
            read(6, 2, 12, "k1", &[1]),
            write(7, 2, 12, "k1", 3),
            commit(8, 12),
            read(17, 5, 13, "k1", &[3]), // final read pins k1's order
            commit(18, 13),
            // Independent lost update on k2: T21, T22 both read version 10.
            write(9, 0, 20, "k2", 10),
            commit(10, 20),
            read(11, 3, 21, "k2", &[10]),
            write(12, 3, 21, "k2", 20),
            commit(13, 21),
            read(14, 4, 22, "k2", &[10]),
            write(15, 4, 22, "k2", 30),
            commit(16, 22),
            read(19, 6, 23, "k2", &[30]), // final read pins k2's order
            commit(20, 23),
        ],
        0,
    );
    let h = EventDecoder::new().decode(&t).expect("decode");
    let g = DepGraph::build(&h).expect("recoverable");
    let all = oracle_elle::anomaly::check_all(&h, &g, IsolationLevel::SnapshotIsolation);
    let lost: Vec<_> = all
        .iter()
        .filter(|a| a.kind == AnomalyKind::LostUpdate)
        .collect();
    assert_eq!(
        lost.len(),
        2,
        "both independent lost updates are reported: {all:#?}"
    );
    let keys: std::collections::BTreeSet<Vec<u8>> =
        lost.iter().flat_map(|a| a.keys.iter().cloned()).collect();
    assert_eq!(
        keys,
        [b"k1".to_vec(), b"k2".to_vec()].into_iter().collect(),
        "one witness per key"
    );
}

/// Round-7 P1 (register order from final reads): T1 writes a,b; T2 writes b,a;
/// the **final reads** see `a` from T1 and `b` from T2 — establishing OPPOSITE
/// per-key orders (a G0 cycle). Register order is pinned by those quiesce reads,
/// not by write Moments (which here would report clean), so the cycle is caught.
#[test]
fn register_g0_from_final_reads_contradicting_write_moments() {
    let t = trace(
        vec![
            write(1, 1, 1, "a", 1), // T1: a<-1
            write(2, 1, 1, "b", 2), // T1: b<-2
            commit(3, 1),
            write(4, 2, 2, "b", 3), // T2: b<-3 (later Moment on b)
            write(5, 2, 2, "a", 4), // T2: a<-4 (later Moment on a)
            commit(6, 2),
            // Final reads AFTER all writes: a's final is T1's (1), b's is T2's (3)
            // — contradicting the write-Moment order (which had T2 last on both).
            read(7, 3, 3, "a", &[1]),
            read(8, 3, 3, "b", &[3]),
            commit(9, 3),
        ],
        0,
    );
    let a = oracle(IsolationLevel::ReadUncommitted)
        .analyze(&t)
        .expect("decodes")
        .expect("the register dirty-write the final reads establish (not clean)");
    assert_eq!(a.kind, AnomalyKind::DirtyWrite);
    assert_eq!(a.txns, vec![1, 2]);
    assert_eq!(a.keys, vec![b"a".to_vec(), b"b".to_vec()]);
}

/// Round-7 P2 (same-Moment RMW): each transaction's read and write of `k` share
/// one Moment. The read-before-write must still be counted (via canonical op
/// order, not a strict `read.at < write.at`), so both RMWs are recognized as
/// based on version 1 — a lost update, not two blind writes judged clean.
#[test]
fn same_moment_rmw_reads_count_as_lost_update() {
    let t = trace(
        vec![
            write(1, 0, 10, "k", 1), // install version 1
            commit(2, 10),
            read(5, 1, 11, "k", &[1]), // T11 reads v1...
            write(5, 1, 11, "k", 2),   // ...and writes at the SAME Moment
            commit(6, 11),
            read(7, 2, 12, "k", &[1]), // T12 reads the same v1...
            write(7, 2, 12, "k", 3),   // ...and writes at the SAME Moment
            commit(8, 12),
            read(9, 3, 13, "k", &[3]), // final read at quiesce pins the order
            commit(10, 13),
        ],
        0,
    );
    let a = oracle(IsolationLevel::SnapshotIsolation)
        .analyze(&t)
        .expect("decodes")
        .expect("a lost update (same-Moment RMW reads counted)");
    assert_eq!(a.kind, AnomalyKind::LostUpdate);
    assert_eq!(a.txns, vec![11, 12]);
    assert_eq!(a.keys, vec![b"k".to_vec()]);
}

/// Round-4 P1 (aborted-gap false-clean): a G0 dirty-write cycle over two append
/// keys where one key's committed versions are separated in the observed order
/// by an **aborted** value (a dirty read, legal at Read Uncommitted). ww edges
/// are paired over the *committed subsequence*, so the aborted intermediate does
/// not break the committed→committed edge across it — the cycle is caught, not
/// judged clean, and both keys are in the witness.
#[test]
fn dirty_write_across_an_aborted_gap_is_caught() {
    let t = trace(
        vec![
            append(1, 1, 1, "A", 1), // T1: A<-1
            append(2, 1, 1, "B", 2), // T1: B<-2
            commit(3, 1),
            append(4, 2, 2, "B", 3), // T2: B<-3
            append(5, 2, 2, "A", 4), // T2: A<-4
            commit(6, 2),
            append(7, 3, 3, "A", 5), // T3: A<-5, but T3 ABORTS
            abort(8, 3),
            // The observer's order of A interleaves the aborted 5 between the two
            // committed versions: [4 (T2), 5 (aborted), 1 (T1)].
            read(9, 4, 4, "A", &[4, 5, 1]),
            read(10, 4, 4, "B", &[2, 3]),
            commit(11, 4),
        ],
        0,
    );
    // Read Uncommitted: the aborted read is permitted, but the dirty write is
    // still forbidden — and must be caught across the aborted gap.
    let a = oracle(IsolationLevel::ReadUncommitted)
        .analyze(&t)
        .expect("decodes")
        .expect("the dirty-write cycle spanning the aborted gap (not a false clean)");
    assert_eq!(a.kind, AnomalyKind::DirtyWrite);
    assert_eq!(
        a.txns,
        vec![1, 2],
        "the two committed writers, across the aborted 5"
    );
    assert_eq!(
        a.keys,
        vec![b"A".to_vec(), b"B".to_vec()],
        "both keys witness the cycle"
    );
}

/// Round-5 P1 (fail-loud through the plugin path): a malformed history judged via
/// `Box<dyn Oracle>` — the advertised Explorer integration — must NOT report
/// clean. It surfaces a **distinguished decode-failure Bug** (class disjoint from
/// the anomaly ladder), whose fingerprint differs from a real anomaly's; a clean
/// run still reports `None`.
#[test]
fn malformed_history_through_dyn_oracle_is_not_clean() {
    let o: Box<dyn Oracle> = Box::new(ElleOracle::new(
        Box::new(EventDecoder::new()),
        IsolationLevel::SnapshotIsolation,
    ));

    // Malformed: value 7 written twice (DuplicateValue) — the plugin path must
    // surface it, not swallow it as clean.
    let malformed = trace(
        vec![
            write(1, 1, 1, "a", 7),
            commit(2, 1),
            write(3, 2, 2, "b", 7),
            commit(4, 2),
        ],
        0,
    );
    let decode_bug = o
        .judge(&malformed)
        .expect("a decode failure must NOT be reported as clean");

    // A genuinely clean run still reports None.
    let clean = trace(vec![write(1, 1, 1, "k", 1), commit(2, 1)], 0);
    assert!(o.judge(&clean).is_none(), "a clean run is still clean");

    // A real anomaly's fingerprint is distinct from the decode-failure Bug's
    // (the decode failure is not a consistency anomaly).
    let lost_update = trace(
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
    let anomaly_bug = o.judge(&lost_update).expect("a real lost update");
    assert_ne!(
        decode_bug.fingerprint, anomaly_bug.fingerprint,
        "a decode failure is distinguished from a consistency anomaly"
    );
}

/// Round-4 P2 (malformed history must not FABRICATE a verdict): a read list that
/// repeats a (unique) written value is malformed; the oracle must fail loud, not
/// report a fabricated dirty-write.
#[test]
fn repeated_observation_is_not_judged_a_violation() {
    let t = trace(
        vec![
            append(1, 1, 1, "k", 1),
            commit(2, 1),
            append(3, 2, 2, "k", 2),
            commit(4, 2),
            read(5, 3, 3, "k", &[1, 2, 1]), // value 1 repeats — malformed
            commit(6, 3),
        ],
        0,
    );
    let verdict = oracle(IsolationLevel::ReadUncommitted).analyze(&t);
    assert!(
        verdict.is_err(),
        "a repeated observation must fail loud, never a fabricated verdict: {verdict:?}"
    );
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
            read(9, 1, 44, "k", &[3]), // final read at quiesce pins the order
            commit(10, 44),
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

/// Round-9 P1 (the register twin of the above): two committed register writes to
/// a key with **no quiesce read** cannot have their order recovered — `analyze`
/// must fail loud (`UnpinnedRegister`), not fabricate an order by sorting and
/// judge clean.
#[test]
fn multi_write_register_without_final_read_is_not_judged_clean() {
    let t = trace(
        vec![
            write(1, 1, 1, "k", 1),
            commit(2, 1),
            write(3, 2, 2, "k", 2), // second committed write, never read back
            commit(4, 2),
        ],
        0,
    );
    let verdict = oracle(IsolationLevel::Serializable).analyze(&t);
    assert!(
        matches!(
            verdict,
            Err(oracle_elle::DecodeError::UnpinnedRegister { .. })
        ),
        "an unpinned multi-write register must fail loud: {verdict:?}"
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
        read(9, 3, 13, "k", &[vb]), // final read at quiesce pins k's order
        commit(10, 13),
    ];
    for i in 0..noise as u64 {
        // A blind write to a unique key with a globally-unique value: no read,
        // so it is never a lost-update participant; a unique key, so no ww edge.
        // A single write needs no final read (unambiguous — not `UnpinnedRegister`).
        let key = format!("noise{i}");
        let at = 11 + i * 2;
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
        // A final read at quiesce pins the last version (the recoverability
        // contract for a multi-write register key).
        ev.push(read(at, 1, len as u64 + 1, "k", &[len as i64]));
        ev.push(commit(at + 1, len as u64 + 1));
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
