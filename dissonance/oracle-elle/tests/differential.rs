// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task-75 — the **differential property harness** (the convergence mechanism).
//!
//! Seven review rounds surfaced false-clean / false-positive holes one point
//! fix at a time. This closes the loop: a random generator of small recoverable
//! register histories, an **independent brute-force reference** (view-
//! serializability by enumerating serial orders — no shared code with the
//! oracle's dependency graph), and a proptest (>=256 cases) asserting the
//! oracle's verdict matches the reference on **both** clean and anomalous inputs.
//!
//! ## Why the two agree
//!
//! The generator is restricted to a fragment where the v1 anomaly ladder (G0
//! dirty write, G1a aborted read, lost update) coincides with full
//! serializability, so `oracle-reports-anomaly` ⟺ `no serial order reproduces
//! the reads`:
//!
//! - every transaction is a **single-key RMW**, **write-only** (distinct keys,
//!   one write each), or **single-key read** — never read-one-key-write-another
//!   (no write-skew G2, the serializability violation v1 does not catch), never
//!   a multi-key read (no fractured read), never an intra-txn overwrite (no
//!   intermediate read);
//! - **read-only observations are the current value** — a quiesce read honoring
//!   the recoverability contract (a read at quiesce sees the final version);
//!   only an **RMW** may read an older version (a stale read → a lost update if
//!   two coincide), which is safe because an RMW read is never a quiesce read.
//!
//! This is exactly the space where the checker is sound. The harness found the
//! abort-read order bug (aborted transactions' reads must not fix version order)
//! as a shrunk 3-txn counterexample; register-order-from-final-reads (round 7)
//! and the append G0 cases are pinned by the named tests in `checker.rs`, which
//! use the consistent multi-key snapshot reads a real G0 witness requires.

mod common;

use std::collections::BTreeMap;

use common::{abort, commit, read, trace, write};
use explorer::RunTrace;
use oracle_elle::{ElleOracle, EventDecoder, IsolationLevel};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// The independent reference model
// ---------------------------------------------------------------------------

type K = u8;
type V = i64;

/// A reference operation: a read observing a value (or the initial/unwritten
/// version, `None`), or a write of a unique value.
#[derive(Clone, Debug, PartialEq)]
enum ROp {
    R(K, Option<V>),
    W(K, V),
}

/// A reference transaction: an id, its ops in program order, and whether it
/// committed.
#[derive(Clone, Debug)]
struct RTxn {
    id: u64,
    ops: Vec<ROp>,
    committed: bool,
}

/// All permutations of `0..n` (n small; the reference enumerates up to six
/// committed txns — `6! = 720`, still cheap per case).
fn perms(n: usize) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    let mut cur: Vec<usize> = (0..n).collect();
    fn go(arr: &mut Vec<usize>, k: usize, out: &mut Vec<Vec<usize>>) {
        if k == arr.len() {
            out.push(arr.clone());
            return;
        }
        for i in k..arr.len() {
            arr.swap(k, i);
            go(arr, k + 1, out);
            arr.swap(k, i);
        }
    }
    go(&mut cur, 0, &mut out);
    out
}

/// **View-serializable?** True iff some serial order of the *committed*
/// transactions reproduces every committed read (each read sees the value the
/// last preceding write in that order produced, or the initial version). This is
/// the ground truth, computed by enumeration — nothing here shares code with the
/// oracle's `DepGraph`.
fn is_serializable(txns: &[RTxn]) -> bool {
    let committed: Vec<&RTxn> = txns.iter().filter(|t| t.committed).collect();
    if committed.is_empty() {
        return true;
    }
    for perm in perms(committed.len()) {
        let mut state: BTreeMap<K, V> = BTreeMap::new();
        let mut ok = true;
        'perm: for &i in &perm {
            for op in &committed[i].ops {
                match op {
                    ROp::R(k, obs) => {
                        if state.get(k).copied() != *obs {
                            ok = false;
                            break 'perm;
                        }
                    }
                    ROp::W(k, v) => {
                        state.insert(*k, *v);
                    }
                }
            }
        }
        if ok {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// The generator (fragment-restricted, always recoverable)
// ---------------------------------------------------------------------------

/// A transaction shape, all within the sound register fragment (see the module
/// doc). `stale` on an RMW makes it read a *pre-write* older version — the only
/// non-current read permitted, because an RMW read is never a quiesce read (a
/// later write to the key follows it), so it drives lost-update detection
/// without perturbing register-order recovery.
#[derive(Clone, Debug)]
enum Shape {
    /// Read one key then write it; `stale` reads an older committed version.
    Rmw { key: K, stale: bool },
    /// Write one or two distinct keys (blind).
    Writes(Vec<K>),
    /// Read one key — a *current* observation (a final/quiesce read).
    Read(K),
}

fn arb_shape() -> impl Strategy<Value = Shape> {
    prop_oneof![
        (0u8..2, any::<bool>()).prop_map(|(key, stale)| Shape::Rmw { key, stale }),
        prop::collection::vec(0u8..2, 1..=2).prop_map(Shape::Writes),
        (0u8..2).prop_map(Shape::Read),
    ]
}

/// Build a recoverable register history in the sound fragment. Read-only
/// observations are always **current** (so a quiesce read sees the true final —
/// no ambiguous register order); an RMW may read an older version (a stale
/// read → a lost update if two coincide). Writes mint fresh unique values, at
/// most one per key per txn. Always decodable — the reference has no
/// `DecodeError` concept.
fn build(specs: Vec<(Shape, bool)>) -> Vec<RTxn> {
    let mut vcounter: V = 1;
    let mut state: BTreeMap<K, V> = BTreeMap::new(); // current value per key
    let mut history: BTreeMap<K, Vec<V>> = BTreeMap::new(); // committed values, in order
    let mut txns = Vec::new();
    for (i, (shape, commit)) in specs.iter().enumerate() {
        let id = i as u64 + 1;
        let mut ops = Vec::new();
        match shape {
            Shape::Rmw { key, stale } => {
                // Read the pre-write version when `stale` and an older value
                // exists; else the current value.
                let obs = if *stale {
                    history
                        .get(key)
                        .and_then(|vs| vs.iter().rev().nth(1).copied())
                        .map(Some)
                        .unwrap_or_else(|| state.get(key).copied())
                } else {
                    state.get(key).copied()
                };
                ops.push(ROp::R(*key, obs));
                let v = vcounter;
                vcounter += 1;
                ops.push(ROp::W(*key, v));
                state.insert(*key, v);
                history.entry(*key).or_default().push(v);
            }
            Shape::Writes(keys) => {
                // At most one write per key per txn (an intra-txn overwrite would
                // make the earlier value an *intermediate* whose read is an
                // off-fragment anomaly class).
                let mut done = std::collections::BTreeSet::new();
                for &k in keys {
                    if !done.insert(k) {
                        continue;
                    }
                    let v = vcounter;
                    vcounter += 1;
                    ops.push(ROp::W(k, v));
                    state.insert(k, v);
                    history.entry(k).or_default().push(v);
                }
            }
            Shape::Read(k) => {
                ops.push(ROp::R(*k, state.get(k).copied()));
            }
        }
        txns.push(RTxn {
            id,
            ops,
            committed: *commit,
        });
    }
    txns
}

/// A generated history plus its per-txn commit schedule: `(shape, committed,
/// commit_deferred)` for each txn. Up to six transactions on two keys, so a
/// register key can receive three or more committed writes (round 10); the
/// per-txn `commit_deferred` flag decouples a write's op Moment from its commit
/// Moment (round 11: writes-then-late-commits). The reference enumerates the
/// committed subset's permutations, cheap at this size.
fn arb_history() -> impl Strategy<Value = (Vec<RTxn>, Vec<bool>)> {
    prop::collection::vec((arb_shape(), any::<bool>(), any::<bool>()), 2..=6).prop_map(|specs| {
        let delays: Vec<bool> = specs.iter().map(|(_, _, d)| *d).collect();
        let txns = build(specs.into_iter().map(|(s, c, _)| (s, c)).collect());
        (txns, delays)
    })
}

fn key_of(op: &ROp) -> K {
    match op {
        ROp::R(k, _) | ROp::W(k, _) => *k,
    }
}

/// Assign Moments to a history's ops and commit markers. Ops run sequentially in
/// txn order; each txn's commit/abort marker lands right after its ops **unless
/// deferred** (`delays[i]`), in which case every deferred marker lands after all
/// ops and non-deferred markers — a **writes-then-late-commits** interleaving, so
/// a read can fall between the writes and a writer's commit. Returns the op
/// Moments per txn and the commit Moment per txn.
fn layout(txns: &[RTxn], delays: &[bool]) -> (Vec<Vec<u64>>, Vec<u64>) {
    let mut at = 1u64;
    let mut op_moments: Vec<Vec<u64>> = Vec::with_capacity(txns.len());
    let mut commit_moments = vec![0u64; txns.len()];
    let mut deferred: Vec<usize> = Vec::new();
    for (i, t) in txns.iter().enumerate() {
        let mut oms = Vec::with_capacity(t.ops.len());
        for _ in &t.ops {
            oms.push(at);
            at += 1;
        }
        op_moments.push(oms);
        if delays.get(i).copied().unwrap_or(false) {
            deferred.push(i);
        } else {
            commit_moments[i] = at;
            at += 1;
        }
    }
    for i in deferred {
        commit_moments[i] = at;
        at += 1;
    }
    (op_moments, commit_moments)
}

/// Convert a reference history + commit schedule into a `RunTrace` the oracle
/// judges (register model: `W`/`R` on `k<key>`, one session per txn). Events are
/// emitted at their laid-out Moments and sorted, so a deferred commit really does
/// follow the reads stamped before it.
fn to_trace(txns: &[RTxn], delays: &[bool]) -> RunTrace {
    let (op_moments, commit_moments) = layout(txns, delays);
    let mut events = Vec::new();
    for (i, t) in txns.iter().enumerate() {
        for (j, op) in t.ops.iter().enumerate() {
            let at = op_moments[i][j];
            let key = format!("k{}", key_of(op));
            events.push(match op {
                ROp::R(_, obs) => {
                    let vs: Vec<i64> = obs.iter().copied().collect();
                    read(at, t.id, t.id, &key, &vs)
                }
                ROp::W(_, v) => write(at, t.id, t.id, &key, *v),
            });
        }
        let at = commit_moments[i];
        events.push(if t.committed {
            commit(at, t.id)
        } else {
            abort(at, t.id)
        });
    }
    events.sort_by_key(|(m, _)| m.0);
    trace(events, 0)
}

/// The checker's **register recoverability** contract, computed independently: a
/// register key with two or more committed writes needs a *quiesce read* — a
/// committed read of a **committed** value strictly after every committed
/// writer's **commit** Moment (round 11: the commit, not the write op). Without
/// one the version order is unrecoverable (`DecodeError::UnpinnedRegister`), never
/// fabricated. Moments come from the same [`layout`] `to_trace` uses.
fn recoverable(txns: &[RTxn], delays: &[bool]) -> bool {
    let (op_moments, commit_moments) = layout(txns, delays);
    let mut committed_writer: BTreeMap<V, bool> = BTreeMap::new();
    for t in txns {
        for op in &t.ops {
            if let ROp::W(_, v) = op {
                committed_writer.insert(*v, t.committed);
            }
        }
    }
    let mut writes: BTreeMap<K, usize> = BTreeMap::new();
    let mut last_commit: BTreeMap<K, u64> = BTreeMap::new();
    let mut quiesce_reads: BTreeMap<K, Vec<u64>> = BTreeMap::new();
    for (i, t) in txns.iter().enumerate() {
        for (j, op) in t.ops.iter().enumerate() {
            match op {
                ROp::W(k, _) if t.committed => {
                    *writes.entry(*k).or_default() += 1;
                    let c = commit_moments[i];
                    last_commit
                        .entry(*k)
                        .and_modify(|m| *m = (*m).max(c))
                        .or_insert(c);
                }
                ROp::R(k, obs)
                    if t.committed
                        && obs
                            .as_ref()
                            .is_some_and(|v| committed_writer.get(v).copied().unwrap_or(false)) =>
                {
                    quiesce_reads.entry(*k).or_default().push(op_moments[i][j])
                }
                _ => {}
            }
        }
    }
    for (k, &n) in &writes {
        if n < 2 {
            continue; // a single committed write is unambiguous
        }
        let cutoff = last_commit.get(k).copied().unwrap_or(0);
        let pinned = quiesce_reads
            .get(k)
            .is_some_and(|rs| rs.iter().any(|&r| r > cutoff));
        if !pinned {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// The differential property
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// On a **recoverable** history the oracle at Serializable reports an anomaly
    /// **iff** the independent reference finds it non-serializable (on this
    /// fragment the v1 ladder coincides with serializability); on an
    /// **unrecoverable** one — a multi-write register key with no quiesce read
    /// (including one whose only late reads precede a deferred commit) — it fails
    /// loud with a `DecodeError` instead of fabricating an order. The oracle's
    /// Ok/Err split must match the independent `recoverable` predicate exactly,
    /// across commit schedules.
    #[test]
    fn oracle_matches_the_reference((txns, delays) in arb_history()) {
        let t = to_trace(&txns, &delays);
        let oracle = ElleOracle::new(Box::new(EventDecoder::new()), IsolationLevel::Serializable);
        let result = oracle.analyze(&t);
        if recoverable(&txns, &delays) {
            let verdict = result.expect("a recoverable history decodes");
            let ref_flags = !is_serializable(&txns);
            prop_assert_eq!(
                verdict.is_some(),
                ref_flags,
                "oracle={:?} reference_non_serializable={} for {:#?} delays={:?}",
                verdict,
                ref_flags,
                txns,
                delays
            );
        } else {
            prop_assert!(
                result.is_err(),
                "multi-write register with no quiesce read must DecodeError: {:#?} delays={:?}",
                txns,
                delays
            );
        }
    }
}

// ---------------------------------------------------------------------------
// A couple of pinned reference sanity checks (the reference itself is correct).
// ---------------------------------------------------------------------------

/// The harness is not vacuous: the generator + pipeline produce **anomalous**
/// histories too, and the oracle agrees with the reference on them. A stale RMW
/// read of an earlier version coinciding with another RMW is a lost update — the
/// reference calls it non-serializable and the oracle flags it.
#[test]
fn harness_covers_anomalous_histories() {
    let txns = build(vec![
        (Shape::Writes(vec![0]), true), // T1: W(0,1)
        (
            Shape::Rmw {
                key: 0,
                stale: false,
            },
            true,
        ), // T2: R(0,1) W(0,2)
        (
            Shape::Rmw {
                key: 0,
                stale: true,
            },
            true,
        ), // T3: R(0,1 STALE) W(0,3)
        (Shape::Read(0), true),         // T4: final read pins key 0's order (quiesce)
    ]);
    let no_delay = vec![false; txns.len()];
    assert!(
        !is_serializable(&txns),
        "the planted lost update is not serializable"
    );
    assert!(
        recoverable(&txns, &no_delay),
        "the final read makes it recoverable"
    );
    let t = to_trace(&txns, &no_delay);
    let oracle = ElleOracle::new(Box::new(EventDecoder::new()), IsolationLevel::Serializable);
    assert!(
        oracle.analyze(&t).expect("recoverable").is_some(),
        "the oracle flags the planted lost update, matching the reference"
    );
}

/// Round-11 coverage: the same three-writer register, but the writers' commits
/// are **deferred** past the final read (writes-then-late-commits). The read now
/// precedes every writer's commit, so it is not a quiesce read — the register is
/// unrecoverable and the oracle must fail loud, exactly as the commit-cutoff
/// predicate says (a write-op cutoff would wrongly recover and could invent an
/// anomaly).
#[test]
fn harness_covers_a_read_before_a_late_commit() {
    let txns = build(vec![
        (Shape::Writes(vec![0]), true), // T1: W(0,1)
        (Shape::Writes(vec![0]), true), // T2: W(0,2)
        (Shape::Read(0), true),         // T3: reads current (2)
    ]);
    // Defer T1's and T2's commits to the very end — after T3's read.
    let delays = vec![true, true, false];
    assert!(
        !recoverable(&txns, &delays),
        "the read precedes the writers' deferred commits — unrecoverable"
    );
    let t = to_trace(&txns, &delays);
    let oracle = ElleOracle::new(Box::new(EventDecoder::new()), IsolationLevel::Serializable);
    assert!(
        oracle.analyze(&t).is_err(),
        "a read before the writers' commits must not pin the order — fail loud"
    );
}

/// Round-10 coverage: a register key with **three** committed writes and only a
/// final read (no intermediate reads) — the class whose non-final predecessor
/// order the checker must not fabricate. It is recoverable (the final read pins
/// the settled version) and serializable (a serial write order reproduces the
/// one read), so the oracle must judge it clean, agreeing with the reference.
#[test]
fn harness_covers_a_three_writer_register_with_only_a_final_read() {
    let txns = build(vec![
        (Shape::Writes(vec![0]), true), // T1: W(0,1)
        (Shape::Writes(vec![0]), true), // T2: W(0,2)
        (Shape::Writes(vec![0]), true), // T3: W(0,3)
        (Shape::Read(0), true),         // T4: final read (current = 3)
    ]);
    let no_delay = vec![false; txns.len()];
    assert!(
        recoverable(&txns, &no_delay),
        "the final read pins the settled version — recoverable"
    );
    assert!(
        is_serializable(&txns),
        "a serial write order reproduces the single read — serializable"
    );
    let t = to_trace(&txns, &no_delay);
    let oracle = ElleOracle::new(Box::new(EventDecoder::new()), IsolationLevel::Serializable);
    assert!(
        oracle.analyze(&t).expect("recoverable").is_none(),
        "clean: the checker must not invent a predecessor order that reads as an anomaly"
    );
}

#[test]
fn reference_labels_known_cases() {
    // A serial RMW chain is serializable.
    let clean = vec![
        RTxn {
            id: 1,
            ops: vec![ROp::W(0, 1)],
            committed: true,
        },
        RTxn {
            id: 2,
            ops: vec![ROp::R(0, Some(1)), ROp::W(0, 2)],
            committed: true,
        },
    ];
    assert!(is_serializable(&clean));

    // A committed read of an aborted write is NOT serializable (G1a).
    let g1a = vec![
        RTxn {
            id: 1,
            ops: vec![ROp::W(0, 1)],
            committed: false,
        }, // aborts
        RTxn {
            id: 2,
            ops: vec![ROp::R(0, Some(1))],
            committed: true,
        }, // read the aborted 1
    ];
    assert!(!is_serializable(&g1a));

    // Two RMWs reading the same version and both committing (lost update) — not
    // serializable.
    let lost = vec![
        RTxn {
            id: 1,
            ops: vec![ROp::W(0, 1)],
            committed: true,
        },
        RTxn {
            id: 2,
            ops: vec![ROp::R(0, Some(1)), ROp::W(0, 2)],
            committed: true,
        },
        RTxn {
            id: 3,
            ops: vec![ROp::R(0, Some(1)), ROp::W(0, 3)],
            committed: true,
        },
    ];
    assert!(!is_serializable(&lost));

    // Conflicting final reads across two keys (G0) — not serializable.
    let g0 = vec![
        RTxn {
            id: 1,
            ops: vec![ROp::W(0, 1), ROp::W(1, 2)],
            committed: true,
        },
        RTxn {
            id: 2,
            ops: vec![ROp::W(1, 3), ROp::W(0, 4)],
            committed: true,
        },
        RTxn {
            id: 3,
            ops: vec![ROp::R(0, Some(1)), ROp::R(1, Some(3))],
            committed: true,
        },
    ];
    assert!(!is_serializable(&g0));
}
