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

/// All permutations of `0..n` (n small; the reference is for <= 4 committed txns).
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

fn arb_history() -> impl Strategy<Value = Vec<RTxn>> {
    prop::collection::vec((arb_shape(), any::<bool>()), 2..=4).prop_map(build)
}

/// Convert a reference history into a `RunTrace` the oracle judges (register
/// model: `W`/`R` on `k<key>`, one session per txn, increasing Moments, ops
/// before the commit/abort marker).
fn to_trace(txns: &[RTxn]) -> RunTrace {
    let mut events = Vec::new();
    let mut at = 1u64;
    for t in txns {
        for op in &t.ops {
            let key = format!("k{}", key_of(op));
            match op {
                ROp::R(_, obs) => {
                    let vs: Vec<i64> = obs.iter().copied().collect();
                    events.push(read(at, t.id, t.id, &key, &vs));
                }
                ROp::W(_, v) => events.push(write(at, t.id, t.id, &key, *v)),
            }
            at += 1;
        }
        events.push(if t.committed {
            commit(at, t.id)
        } else {
            abort(at, t.id)
        });
        at += 1;
    }
    trace(events, 0)
}

fn key_of(op: &ROp) -> K {
    match op {
        ROp::R(k, _) | ROp::W(k, _) => *k,
    }
}

// ---------------------------------------------------------------------------
// The differential property
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// The oracle at Serializable reports an anomaly **iff** the independent
    /// reference finds the history non-serializable — on both clean and
    /// anomalous inputs. (On this fragment the v1 ladder coincides with
    /// serializability.)
    #[test]
    fn oracle_matches_the_reference(txns in arb_history()) {
        let t = to_trace(&txns);
        let oracle = ElleOracle::new(Box::new(EventDecoder::new()), IsolationLevel::Serializable);
        // The generator is always recoverable, so judging never decode-fails.
        let verdict = oracle
            .analyze(&t)
            .expect("the generated history is recoverable");
        let oracle_flags = verdict.is_some();
        let ref_flags = !is_serializable(&txns);
        prop_assert_eq!(
            oracle_flags,
            ref_flags,
            "oracle={:?} reference_non_serializable={} for {:#?}",
            verdict,
            ref_flags,
            txns
        );
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
    ]);
    assert!(
        !is_serializable(&txns),
        "the planted lost update is not serializable"
    );
    let t = to_trace(&txns);
    let oracle = ElleOracle::new(Box::new(EventDecoder::new()), IsolationLevel::Serializable);
    assert!(
        oracle.analyze(&t).expect("recoverable").is_some(),
        "the oracle flags the planted lost update, matching the reference"
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
