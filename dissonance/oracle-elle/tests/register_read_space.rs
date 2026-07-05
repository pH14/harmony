// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task-75 — **systematic enumeration of the register-read edge space** (round
//! 13). Twelve review rounds surfaced register-recovery holes one shrunk
//! counterexample at a time (unpinned multi-write, fabricated predecessor order,
//! write-op vs commit cutoff, the empty final read). This closes the family: for
//! a single register key it enumerates **every** committed-writer count × read
//! observation (empty / a committed value / a multi-value) × read timing
//! (before vs after all committed writers' commits) × read multiplicity (0, 1, 2
//! reads, incl. contradictory finals), computes the recoverability verdict
//! **independently**, and asserts the checker agrees — so a regression in any
//! cell fails here instead of being found by hand later.
//!
//! The rules the independent classifier encodes (each a past round's fix):
//! - a register read of **> 1 value** is malformed (`MultiValueRegisterRead`);
//! - an **empty** read after every writer's commit contradicts a committed write
//!   (`EmptyFinalRead`, round 13);
//! - **two quiesce reads disagreeing** on the final value are unrecoverable
//!   (`InconsistentOrder` — contradictory finals);
//! - **>= 2 committed writes with no committed-value quiesce read** cannot pin the
//!   order (`UnpinnedRegister`, rounds 9/11);
//! - otherwise the key is recoverable and, single-key with no RMW, clean.

mod common;

use std::collections::BTreeSet;

use common::{commit, read, trace, write};
use explorer::Oracle;
use oracle_elle::{ElleOracle, EventDecoder, IsolationLevel};

fn oracle() -> ElleOracle {
    ElleOracle::new(Box::new(EventDecoder::new()), IsolationLevel::Serializable)
}

/// What a read observed.
#[derive(Clone, Debug, PartialEq)]
enum Obs {
    /// The empty/initial (unwritten) version.
    Empty,
    /// A single committed value.
    Val(i64),
    /// A multi-value list (malformed for a register).
    Multi(Vec<i64>),
}

/// Build the history: each writer commits **late** (so a read can land before or
/// after the commits), each read is a committed transaction whose Moment encodes
/// its timing (before all commits, or after — a quiesce read).
fn build(writers: &[i64], reads: &[(Obs, bool)]) -> explorer::RunTrace {
    let mut ev = Vec::new();
    for (i, &w) in writers.iter().enumerate() {
        let txn = i as u64 + 1;
        ev.push(write(i as u64 + 1, txn, txn, "k", w)); // write op at Moment i+1
        ev.push(commit(1000 + i as u64, txn)); // ...but commit LATE
    }
    for (j, (obs, after)) in reads.iter().enumerate() {
        let txn = 100 + j as u64;
        // Before all commits (>= 1000): Moment ~500. After all commits: ~2000.
        let at = if *after {
            2000 + j as u64
        } else {
            500 + j as u64
        };
        let vs: Vec<i64> = match obs {
            Obs::Empty => vec![],
            Obs::Val(v) => vec![*v],
            Obs::Multi(vs) => vs.clone(),
        };
        ev.push(read(at, txn, txn, "k", &vs));
        ev.push(commit(3000 + j as u64, txn));
    }
    trace(ev, 0)
}

/// The recoverability verdict, computed **independently** of the checker.
fn is_unrecoverable(writers: &[i64], reads: &[(Obs, bool)]) -> bool {
    // A register read of more than one value is malformed.
    if reads.iter().any(|(o, _)| matches!(o, Obs::Multi(_))) {
        return true;
    }
    // An empty read after all commits, with a committed writer, contradicts it.
    if !writers.is_empty()
        && reads
            .iter()
            .any(|(o, after)| *after && matches!(o, Obs::Empty))
    {
        return true;
    }
    // Distinct committed values pinned by quiesce reads.
    let pinned: BTreeSet<i64> = reads
        .iter()
        .filter_map(|(o, after)| match o {
            Obs::Val(v) if *after && writers.contains(v) => Some(*v),
            _ => None,
        })
        .collect();
    // Two quiesce reads disagreeing on the final — contradictory finals.
    if pinned.len() > 1 {
        return true;
    }
    // Two or more committed writes with nothing pinning the final.
    if writers.len() >= 2 && pinned.is_empty() {
        return true;
    }
    false
}

/// Every `(Obs, timing)` option valid for a writer set (reads only observe values
/// the writers produced; a multi-value read needs two).
fn options(writers: &[i64]) -> Vec<(Obs, bool)> {
    let mut opts = vec![(Obs::Empty, false), (Obs::Empty, true)];
    for &w in writers {
        opts.push((Obs::Val(w), false));
        opts.push((Obs::Val(w), true));
    }
    if writers.len() >= 2 {
        opts.push((Obs::Multi(writers.to_vec()), false));
        opts.push((Obs::Multi(writers.to_vec()), true));
    }
    opts
}

#[test]
fn register_read_edge_space_is_systematically_covered() {
    let mut cases = 0usize;
    for writers in [vec![10i64], vec![10i64, 20]] {
        let opts = options(&writers);
        // Read lists of length 0, 1, and 2 (all ordered pairs — so contradictory
        // finals like [Val(10)@after, Val(20)@after] are included).
        let mut read_lists: Vec<Vec<(Obs, bool)>> = vec![vec![]];
        for a in &opts {
            read_lists.push(vec![a.clone()]);
            for b in &opts {
                read_lists.push(vec![a.clone(), b.clone()]);
            }
        }
        for reads in &read_lists {
            cases += 1;
            let t = build(&writers, reads);
            let result = oracle().judge(&t); // dyn path: Some(bug) on any failure
            let want_err = is_unrecoverable(&writers, reads);
            let analyze = oracle().analyze(&t);
            assert_eq!(
                analyze.is_err(),
                want_err,
                "writers={writers:?} reads={reads:?}: analyze={analyze:?}"
            );
            if want_err {
                // Fail-loud surfaces a distinguished decode-failure Bug (never
                // clean) through the dyn Oracle path too.
                assert!(
                    result.is_some(),
                    "an unrecoverable history must not judge clean: {writers:?} {reads:?}"
                );
            } else {
                // Recoverable, single-key, no RMW → clean.
                assert!(
                    analyze.expect("recoverable").is_none(),
                    "a recoverable single-key read history is clean: {writers:?} {reads:?}"
                );
            }
        }
    }
    assert!(cases >= 90, "the enumeration is non-trivial: {cases} cases");
}
