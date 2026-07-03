// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **anomaly ladder v1** and the declared isolation levels it judges.
//!
//! For a declared [`IsolationLevel`], the checker reports the anomalies that
//! level forbids, each carrying a **constructive witness** (the participating
//! transactions and keys, and the earliest violating [`Moment`](explorer::Moment)):
//!
//! - **G0 dirty write** — a write-write cycle: two committed transactions wrote
//!   keys in conflicting orders. Forbidden at every level.
//! - **G1a aborted read** — a committed transaction read a value an aborted
//!   transaction wrote. Forbidden at Read Committed and above.
//! - **Lost update** — two committed transactions read the *same version* of a
//!   key and both wrote it (a read-modify-write both based on one snapshot).
//!   Forbidden at Snapshot Isolation and above.
//!
//! Cycle-typed anomalies through SI/serializability are the follow-on ladder
//! (task-75 non-goal); v1 anchors on these three. Everything here is a pure,
//! deterministic function of the [`History`] and its [`DepGraph`].

use std::collections::{BTreeMap, BTreeSet};

use explorer::Moment;

use crate::graph::DepGraph;
use crate::op::{Elem, History, Key, OpKind, TxnId};

/// A declared transaction-isolation level — what the workload claims to
/// provide, and therefore what the checker holds it to.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum IsolationLevel {
    /// Forbids only dirty writes (G0).
    ReadUncommitted,
    /// Also forbids aborted reads (G1a).
    ReadCommitted,
    /// Also forbids lost updates.
    SnapshotIsolation,
    /// The strongest level v1 checks (the same forbidden set as SI; full
    /// cycle-typed serializability is the follow-on ladder).
    Serializable,
}

impl IsolationLevel {
    /// Whether this level forbids `kind`.
    pub fn forbids(&self, kind: AnomalyKind) -> bool {
        match kind {
            // A dirty write is forbidden at every level.
            AnomalyKind::DirtyWrite => true,
            // Aborted reads: Read Committed and above.
            AnomalyKind::AbortedRead => *self >= IsolationLevel::ReadCommitted,
            // Lost updates: Snapshot Isolation and above.
            AnomalyKind::LostUpdate => *self >= IsolationLevel::SnapshotIsolation,
        }
    }
}

/// The kind of isolation anomaly a witness reports.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum AnomalyKind {
    /// G0: a write-write cycle among committed transactions.
    DirtyWrite,
    /// G1a: a committed read of an aborted transaction's write.
    AbortedRead,
    /// Two committed read-modify-writes based on the same version of a key.
    LostUpdate,
}

impl AnomalyKind {
    /// The stable class code for the fingerprint's terminal signature
    /// (coordinate 1). Fixed per kind — a reorder of the enum must not change
    /// these.
    pub fn class(&self) -> u32 {
        match self {
            AnomalyKind::DirtyWrite => 0,
            AnomalyKind::AbortedRead => 1,
            AnomalyKind::LostUpdate => 2,
        }
    }

    /// A stable short name (for diagnostics).
    pub fn name(&self) -> &'static str {
        match self {
            AnomalyKind::DirtyWrite => "G0-dirty-write",
            AnomalyKind::AbortedRead => "G1a-aborted-read",
            AnomalyKind::LostUpdate => "lost-update",
        }
    }
}

/// A detected anomaly with its constructive witness.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Anomaly {
    /// Which anomaly.
    pub kind: AnomalyKind,
    /// The participating transactions (sorted, deduplicated).
    pub txns: Vec<TxnId>,
    /// The participating keys (sorted, deduplicated).
    pub keys: Vec<Key>,
    /// The earliest violating [`Moment`] — the fingerprint's V-time coordinate.
    pub at: Moment,
}

impl Anomaly {
    /// The canonical sort key: earliest first, then by class, then by witness —
    /// so "the first anomaly" is deterministic across runs.
    fn order_key(&self) -> (Moment, u32, Vec<TxnId>, Vec<Key>) {
        (
            self.at,
            self.kind.class(),
            self.txns.clone(),
            self.keys.clone(),
        )
    }
}

/// Every forbidden anomaly in `h` at `level`, in canonical order (earliest and
/// most fundamental first). Pure and deterministic.
pub fn check_all(h: &History, g: &DepGraph, level: IsolationLevel) -> Vec<Anomaly> {
    let mut found: Vec<Anomaly> = Vec::new();
    if level.forbids(AnomalyKind::DirtyWrite)
        && let Some(a) = detect_dirty_write(h, g)
    {
        found.push(a);
    }
    if level.forbids(AnomalyKind::AbortedRead)
        && let Some(a) = detect_aborted_read(h, g)
    {
        found.push(a);
    }
    if level.forbids(AnomalyKind::LostUpdate)
        && let Some(a) = detect_lost_update(h)
    {
        found.push(a);
    }
    found.sort_by_key(|a| a.order_key());
    found
}

/// The canonically-first forbidden anomaly, if any — what the oracle reports as
/// one `Bug`.
pub fn check(h: &History, g: &DepGraph, level: IsolationLevel) -> Option<Anomaly> {
    check_all(h, g, level).into_iter().next()
}

/// G0: the first write-write cycle, witnessed by its transactions and the keys
/// whose version order closes it.
fn detect_dirty_write(h: &History, g: &DepGraph) -> Option<Anomaly> {
    let cycle = g.ww_cycle()?;
    let set: BTreeSet<TxnId> = cycle.iter().copied().collect();
    let keys = g.ww_keys_among(&set);
    let at = set
        .iter()
        .filter_map(|t| h.txns.get(t).map(|t| t.first_moment()))
        .min()
        .unwrap_or_default();
    Some(Anomaly {
        kind: AnomalyKind::DirtyWrite,
        txns: set.into_iter().collect(),
        keys,
        at,
    })
}

/// G1a: the earliest committed read of an aborted transaction's write.
fn detect_aborted_read(h: &History, g: &DepGraph) -> Option<Anomaly> {
    let mut best: Option<Anomaly> = None;
    for t in h.iter() {
        if !t.committed() {
            continue;
        }
        for op in &t.ops {
            if let OpKind::Read(vs) = &op.kind {
                for &e in vs {
                    if let Some(w) = g.writer(e)
                        && w != t.id
                        && !g.is_committed(w)
                    {
                        let mut txns = vec![t.id, w];
                        txns.sort_unstable();
                        txns.dedup();
                        let cand = Anomaly {
                            kind: AnomalyKind::AbortedRead,
                            txns,
                            keys: vec![op.key.clone()],
                            at: op.at,
                        };
                        if best
                            .as_ref()
                            .is_none_or(|b| cand.order_key() < b.order_key())
                        {
                            best = Some(cand);
                        }
                    }
                }
            }
        }
    }
    best
}

/// A lost update: two committed transactions did a read-modify-write on one key
/// based on the *same version*. Returns the earliest such conflict. Recovered
/// straight from the history (commit status + per-txn read-before-write), so it
/// needs no [`DepGraph`].
fn detect_lost_update(h: &History) -> Option<Anomaly> {
    // For each committed txn and key it writes, the version its write was based
    // on: the tip of the last read of the key *before* its first write of it. A
    // blind write (no prior read of the key) is not a lost-update participant.
    // Group by (key, based-version); a group of >= 2 distinct txns is a lost
    // update.
    let mut groups: BTreeMap<(Key, Option<Elem>), BTreeMap<TxnId, Moment>> = BTreeMap::new();
    for t in h.iter() {
        if !t.committed() {
            continue;
        }
        // First write moment per key in this txn.
        let mut first_write: BTreeMap<Key, Moment> = BTreeMap::new();
        for op in &t.ops {
            if op.written().is_some() {
                first_write
                    .entry(op.key.clone())
                    .and_modify(|m| {
                        if op.at < *m {
                            *m = op.at;
                        }
                    })
                    .or_insert(op.at);
            }
        }
        for (key, &wmoment) in &first_write {
            // The version read just before the first write of this key.
            let based = t
                .ops
                .iter()
                .filter(|op| {
                    &op.key == key && op.at < wmoment && matches!(op.kind, OpKind::Read(_))
                })
                .max_by_key(|op| op.at)
                .map(|op| op.observed_version());
            // No prior read of the key → a blind write, not a participant.
            let Some(based_version) = based else {
                continue;
            };
            groups
                .entry((key.clone(), based_version))
                .or_default()
                .insert(t.id, wmoment);
        }
    }

    let mut best: Option<Anomaly> = None;
    for ((key, _version), txns) in &groups {
        if txns.len() < 2 {
            continue;
        }
        let at = txns.values().copied().min().unwrap_or_default();
        let cand = Anomaly {
            kind: AnomalyKind::LostUpdate,
            txns: txns.keys().copied().collect(),
            keys: vec![key.clone()],
            at,
        };
        if best
            .as_ref()
            .is_none_or(|b| cand.order_key() < b.order_key())
        {
            best = Some(cand);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The forbidden-set matrix per level: dirty writes everywhere, aborted
    /// reads at RC+, lost updates at SI+ (one representative cell per boundary).
    #[test]
    fn level_forbids_matrix_is_pinned() {
        use AnomalyKind::*;
        use IsolationLevel::*;
        // Dirty writes: forbidden at every level.
        for lvl in [
            ReadUncommitted,
            ReadCommitted,
            SnapshotIsolation,
            Serializable,
        ] {
            assert!(lvl.forbids(DirtyWrite));
        }
        // Aborted reads: RC and above only.
        assert!(!ReadUncommitted.forbids(AbortedRead));
        assert!(ReadCommitted.forbids(AbortedRead));
        assert!(Serializable.forbids(AbortedRead));
        // Lost updates: SI and above only.
        assert!(!ReadCommitted.forbids(LostUpdate));
        assert!(SnapshotIsolation.forbids(LostUpdate));
        assert!(Serializable.forbids(LostUpdate));
    }

    /// The anomaly-class codes are stable and distinct (a fingerprint
    /// coordinate — a reorder must not change them).
    #[test]
    fn anomaly_class_codes_are_stable() {
        assert_eq!(AnomalyKind::DirtyWrite.class(), 0);
        assert_eq!(AnomalyKind::AbortedRead.class(), 1);
        assert_eq!(AnomalyKind::LostUpdate.class(), 2);
    }
}
