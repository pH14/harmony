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

/// **Every** forbidden anomaly in `h` at `level`, in canonical order (earliest
/// and most fundamental first). Each *independent* occurrence is its own witness:
/// one per strongly-connected write-cycle (G0), one per committed read of an
/// aborted write (G1a), one per `(key, read-version)` lost-update group. So two
/// independent lost updates on different keys yield two witnesses — an
/// enumeration caller (task 76 triage) sees them all. Pure and deterministic.
pub fn check_all(h: &History, g: &DepGraph, level: IsolationLevel) -> Vec<Anomaly> {
    let mut found: Vec<Anomaly> = Vec::new();
    if level.forbids(AnomalyKind::DirtyWrite) {
        found.extend(detect_dirty_writes(h, g));
    }
    if level.forbids(AnomalyKind::AbortedRead) {
        found.extend(detect_aborted_reads(h, g));
    }
    if level.forbids(AnomalyKind::LostUpdate) {
        found.extend(detect_lost_updates(h));
    }
    found.sort_by_key(|a| a.order_key());
    found
}

/// The canonically-first forbidden anomaly, if any — what the oracle reports as
/// one `Bug`.
pub fn check(h: &History, g: &DepGraph, level: IsolationLevel) -> Option<Anomaly> {
    check_all(h, g, level).into_iter().next()
}

/// G0: one witness per **independent** write-write cycle (strongly-connected
/// component), each with the cycle's transactions and the keys whose version
/// order closes it.
fn detect_dirty_writes(h: &History, g: &DepGraph) -> Vec<Anomaly> {
    g.ww_sccs()
        .into_iter()
        .map(|scc| {
            let set: BTreeSet<TxnId> = scc.iter().copied().collect();
            let keys = g.ww_keys_among(&set);
            let at = set
                .iter()
                .filter_map(|t| h.txns.get(t).map(|t| t.first_moment()))
                .min()
                .unwrap_or_default();
            Anomaly {
                kind: AnomalyKind::DirtyWrite,
                txns: scc,
                keys,
                at,
            }
        })
        .collect()
}

/// G1a: one witness per `(reader, aborted-writer, key)` — every committed read of
/// an aborted transaction's write (the earliest moment of each). Deterministic.
fn detect_aborted_reads(h: &History, g: &DepGraph) -> Vec<Anomaly> {
    let mut seen: BTreeMap<(Vec<TxnId>, Key), Moment> = BTreeMap::new();
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
                        seen.entry((txns, op.key.clone()))
                            .and_modify(|m| *m = (*m).min(op.at))
                            .or_insert(op.at);
                    }
                }
            }
        }
    }
    seen.into_iter()
        .map(|((txns, key), at)| Anomaly {
            kind: AnomalyKind::AbortedRead,
            txns,
            keys: vec![key],
            at,
        })
        .collect()
}

/// Lost updates: one witness per `(key, read-version)` group of >= 2 committed
/// transactions that did a read-modify-write on that key based on the same
/// version. Recovered straight from the history (commit status + per-txn
/// read-before-write), so it needs no [`DepGraph`].
fn detect_lost_updates(h: &History) -> Vec<Anomaly> {
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
        // The first write's **index** per key. `t.ops` is canonically sorted, so
        // index order is the deterministic program order — and a read at the SAME
        // Moment as the write sorts *before* it (`Read` < `Write`), so a
        // same-Moment read-modify-write is counted as read-before-write, not
        // dropped (the round-5 raw-Moment strict-`<` bug that misclassified it as
        // a blind write and let the SI lost-update check return clean).
        let mut first_write_idx: BTreeMap<Key, usize> = BTreeMap::new();
        for (idx, op) in t.ops.iter().enumerate() {
            if op.written().is_some() {
                first_write_idx.entry(op.key.clone()).or_insert(idx);
            }
        }
        for (key, &widx) in &first_write_idx {
            // The version read just before the first write of this key, by op
            // order (not raw Moment): the last read of the key at index < widx.
            let based = t.ops[..widx]
                .iter()
                .rev()
                .find(|op| &op.key == key && matches!(op.kind, OpKind::Read(_)))
                .map(|op| op.observed_version());
            // No prior read of the key → a blind write, not a participant.
            let Some(based_version) = based else {
                continue;
            };
            groups
                .entry((key.clone(), based_version))
                .or_default()
                .insert(t.id, t.ops[widx].at);
        }
    }

    let mut out = Vec::new();
    for ((key, _version), txns) in &groups {
        if txns.len() < 2 {
            continue;
        }
        let at = txns.values().copied().min().unwrap_or_default();
        out.push(Anomaly {
            kind: AnomalyKind::LostUpdate,
            txns: txns.keys().copied().collect(),
            keys: vec![key.clone()],
            at,
        });
    }
    out
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
