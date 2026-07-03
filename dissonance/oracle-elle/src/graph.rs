// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **transaction dependency graph** recovered from a [`History`].
//!
//! From unique written values and observed lists (the workload's recoverability
//! contract), the graph recovers, over transactions:
//!
//! - **write-read (wr)**: `T2` read a value `T1` wrote → `T1 →wr T2`;
//! - **write-write (ww)**: the version order of writes to a key → `T1 →ww T2`
//!   when `T1`'s write immediately precedes `T2`'s in that key's order;
//! - **read-write anti-dependency (rw)**: `T` read the version `T'` overwrote →
//!   `T →rw T'`.
//!
//! Recovery is **fail-loud** ([`DecodeError`]): a value written twice, a read of
//! a value no write produced, or reads that disagree on a key's version order
//! all make the history unrecoverable — the graph refuses to guess.
//!
//! Determinism: every map/set is a `BTreeMap`/`BTreeSet`, version order is the
//! (unique, prefix-consistent) observed list, and [`DepGraph::ww_cycle`] is an
//! iterative DFS over sorted nodes and sorted neighbours — so the same history
//! always yields the same edges and the same witnessed cycle.

use std::collections::{BTreeMap, BTreeSet};

use explorer::Moment;

use crate::error::DecodeError;
use crate::op::{Elem, History, Key, OpKind, TxnId};

/// The recovered dependency graph over a history's transactions.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DepGraph {
    /// Which transaction wrote each unique value.
    writer: BTreeMap<Elem, TxnId>,
    /// Which transactions committed.
    committed: BTreeSet<TxnId>,
    /// Each key's recovered version order (the unique, prefix-consistent
    /// observed list — the append order / register version sequence).
    version_order: BTreeMap<Key, Vec<Elem>>,
    /// Write-read edges: writer → readers of its writes.
    wr: BTreeMap<TxnId, BTreeSet<TxnId>>,
    /// Write-write edges (committed writers only): earlier → later in version
    /// order.
    ww: BTreeMap<TxnId, BTreeSet<TxnId>>,
    /// Read-write anti-dependency edges: reader of a version → its overwriter.
    rw: BTreeMap<TxnId, BTreeSet<TxnId>>,
}

impl DepGraph {
    /// Recover the dependency graph from a history, or fail loud if it is
    /// unrecoverable.
    pub fn build(h: &History) -> Result<Self, DecodeError> {
        let mut g = DepGraph::default();

        // 1. Attribute every written value to its (unique) writer AND the key it
        //    was written to. A repeat value is a non-unique write; the write-key
        //    map lets step 2 reject a value observed under the *wrong* key. Also
        //    tally each key's committed append values, so step 2 can prove the
        //    recovered order observed them all (no missing final read).
        let mut write_key: BTreeMap<Elem, Key> = BTreeMap::new();
        let mut committed_appends: BTreeMap<Key, BTreeSet<Elem>> = BTreeMap::new();
        for t in h.iter() {
            if t.committed() {
                g.committed.insert(t.id);
            }
            for op in &t.ops {
                if let Some(v) = op.written() {
                    if let Some(&prev) = g.writer.get(&v) {
                        return Err(DecodeError::DuplicateValue {
                            value: v,
                            first: prev,
                            second: t.id,
                        });
                    }
                    g.writer.insert(v, t.id);
                    write_key.insert(v, op.key.clone());
                    if t.committed() && matches!(op.kind, OpKind::Append(_)) {
                        committed_appends
                            .entry(op.key.clone())
                            .or_default()
                            .insert(v);
                    }
                }
            }
        }

        // 2. Recover each key's version order. The recovery is **model-aware**,
        //    because reads observe different things in the two workload models:
        //
        //    - **append keys** (any `Append` targets them): each read observes a
        //      *prefix* of the true append list, so the order is the longest
        //      observed list and every read must be one of its prefixes (a fork
        //      is unrecoverable → `InconsistentOrder`);
        //    - **register keys** (writes only): each read observes the *current*
        //      single value, so the order is the distinct observed values in
        //      first-observation time order — reads at different times seeing
        //      different values are expected, not a conflict.
        //
        //    Every observed value must have a writer either way (else it appeared
        //    from nowhere).
        let mut append_keys: BTreeSet<Key> = BTreeSet::new();
        for t in h.iter() {
            for op in &t.ops {
                if matches!(op.kind, OpKind::Append(_)) {
                    append_keys.insert(op.key.clone());
                }
            }
        }
        // Per key: the append-model observed lists, and the register-model
        // (value, earliest-moment) observations.
        let mut lists_by_key: BTreeMap<Key, Vec<Vec<Elem>>> = BTreeMap::new();
        let mut reg_first_seen: BTreeMap<Key, BTreeMap<Elem, Moment>> = BTreeMap::new();
        for t in h.iter() {
            for op in &t.ops {
                if let OpKind::Read(vs) = &op.kind {
                    for &e in vs {
                        match write_key.get(&e) {
                            None => {
                                return Err(DecodeError::UnknownValue {
                                    value: e,
                                    key: op.key.clone(),
                                });
                            }
                            // A value written to a different key must never join
                            // this key's order (cross-key contamination).
                            Some(wrote) if wrote != &op.key => {
                                return Err(DecodeError::MisattributedValue {
                                    value: e,
                                    wrote_key: wrote.clone(),
                                    read_key: op.key.clone(),
                                });
                            }
                            Some(_) => {}
                        }
                    }
                    if append_keys.contains(&op.key) {
                        lists_by_key
                            .entry(op.key.clone())
                            .or_default()
                            .push(vs.clone());
                    } else if let Some(&tip) = vs.last() {
                        // Register: keep the earliest moment each value was read.
                        let e = reg_first_seen.entry(op.key.clone()).or_default();
                        e.entry(tip)
                            .and_modify(|m| {
                                if op.at < *m {
                                    *m = op.at;
                                }
                            })
                            .or_insert(op.at);
                    }
                }
            }
        }
        for (key, lists) in &lists_by_key {
            // The longest observed list is the candidate order; ties broken
            // deterministically by content. Every read must be a prefix of it.
            let candidate = lists
                .iter()
                .max_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)))
                .cloned()
                .unwrap_or_default();
            for l in lists {
                if !candidate.starts_with(l.as_slice()) {
                    return Err(DecodeError::InconsistentOrder { key: key.clone() });
                }
            }
            g.version_order.insert(key.clone(), candidate);
        }
        for (key, first_seen) in &reg_first_seen {
            // Distinct values in (first-observation moment, value) order.
            let mut ordered: Vec<(Moment, Elem)> =
                first_seen.iter().map(|(&v, &m)| (m, v)).collect();
            ordered.sort_unstable();
            g.version_order
                .insert(key.clone(), ordered.into_iter().map(|(_, v)| v).collect());
        }

        // The recoverability contract for **append** keys: final reads at quiesce
        // fix version order, so every committed append must appear in the
        // recovered order. If one does not (a missing final read, or no read of
        // the key at all), the order is incomplete — its ww edges are missing and
        // a real dirty-write could be judged clean. Fail loud instead of
        // proceeding on a partial order.
        for (key, appends) in &committed_appends {
            let recovered: BTreeSet<Elem> = g
                .version_order
                .get(key)
                .map(|o| o.iter().copied().collect())
                .unwrap_or_default();
            if let Some(&missing) = appends.iter().find(|v| !recovered.contains(v)) {
                return Err(DecodeError::UnobservedAppend {
                    key: key.clone(),
                    value: missing,
                });
            }
        }

        g.build_edges(h);
        Ok(g)
    }

    /// Populate wr/ww/rw from the writer map and recovered version orders.
    fn build_edges(&mut self, h: &History) {
        // Write-read: each observed value's writer → the reading txn.
        for t in h.iter() {
            for op in &t.ops {
                if let OpKind::Read(vs) = &op.kind {
                    for &e in vs {
                        if let Some(&we) = self.writer.get(&e)
                            && we != t.id
                        {
                            self.wr.entry(we).or_default().insert(t.id);
                        }
                    }
                    // Read-write anti-dependency: the reader of a tip version is
                    // anti-dependent on whoever overwrote it (the next version).
                    if let Some(&tip) = vs.last()
                        && let Some(order) = self.version_order.get(&op.key)
                        && let Some(pos) = order.iter().position(|&e| e == tip)
                        && let Some(&next) = order.get(pos + 1)
                        && let Some(&wnext) = self.writer.get(&next)
                        && wnext != t.id
                    {
                        self.rw.entry(t.id).or_default().insert(wnext);
                    }
                }
            }
        }

        // Write-write: consecutive committed writers in each key's version order.
        for order in self.version_order.values() {
            for pair in order.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                if let (Some(&wa), Some(&wb)) = (self.writer.get(&a), self.writer.get(&b))
                    && wa != wb
                    && self.committed.contains(&wa)
                    && self.committed.contains(&wb)
                {
                    self.ww.entry(wa).or_default().insert(wb);
                }
            }
        }
    }

    /// The transaction that wrote `value`, if any.
    pub fn writer(&self, value: Elem) -> Option<TxnId> {
        self.writer.get(&value).copied()
    }

    /// Whether `txn` committed.
    pub fn is_committed(&self, txn: TxnId) -> bool {
        self.committed.contains(&txn)
    }

    /// A key's recovered version order.
    pub fn version_order(&self, key: &Key) -> Option<&[Elem]> {
        self.version_order.get(key).map(Vec::as_slice)
    }

    /// The write-read edges.
    pub fn wr_edges(&self) -> &BTreeMap<TxnId, BTreeSet<TxnId>> {
        &self.wr
    }

    /// The write-write edges (committed writers).
    pub fn ww_edges(&self) -> &BTreeMap<TxnId, BTreeSet<TxnId>> {
        &self.ww
    }

    /// The read-write anti-dependency edges.
    pub fn rw_edges(&self) -> &BTreeMap<TxnId, BTreeSet<TxnId>> {
        &self.rw
    }

    /// The first write-write cycle, as the transactions on it in cycle order, or
    /// `None` if the ww graph is acyclic. Iterative DFS over sorted nodes and
    /// sorted neighbours — deterministic and stack-safe on untrusted input.
    pub fn ww_cycle(&self) -> Option<Vec<TxnId>> {
        let mut nodes: BTreeSet<TxnId> = BTreeSet::new();
        for (&u, vs) in &self.ww {
            nodes.insert(u);
            nodes.extend(vs.iter().copied());
        }
        // 0 = unvisited, 1 = on the current DFS path (gray), 2 = finished.
        let mut state: BTreeMap<TxnId, u8> = nodes.iter().map(|&n| (n, 0u8)).collect();

        for &root in &nodes {
            if state.get(&root).copied().unwrap_or(0) != 0 {
                continue;
            }
            let mut path: Vec<TxnId> = vec![root];
            let mut frames: Vec<(TxnId, usize)> = vec![(root, 0)];
            state.insert(root, 1);
            while let Some(&(u, idx)) = frames.last() {
                let neighbors: Vec<TxnId> = self
                    .ww
                    .get(&u)
                    .map(|s| s.iter().copied().collect())
                    .unwrap_or_default();
                if idx < neighbors.len() {
                    if let Some(top) = frames.last_mut() {
                        top.1 += 1;
                    }
                    let v = neighbors[idx];
                    match state.get(&v).copied().unwrap_or(0) {
                        0 => {
                            state.insert(v, 1);
                            path.push(v);
                            frames.push((v, 0));
                        }
                        1 => {
                            // Back edge to a gray node: the cycle is v..=u on the
                            // current path.
                            if let Some(start) = path.iter().position(|&x| x == v) {
                                return Some(path[start..].to_vec());
                            }
                        }
                        _ => {}
                    }
                } else {
                    state.insert(u, 2);
                    path.pop();
                    frames.pop();
                }
            }
        }
        None
    }

    /// The keys whose version order places a ww edge between two members of
    /// `set` — the constructive key witness for a ww-cycle (G0) finding. Sorted
    /// and deduplicated.
    pub fn ww_keys_among(&self, set: &BTreeSet<TxnId>) -> Vec<Key> {
        let mut keys: BTreeSet<Key> = BTreeSet::new();
        for (key, order) in &self.version_order {
            for pair in order.windows(2) {
                if let (Some(&wa), Some(&wb)) =
                    (self.writer.get(&pair[0]), self.writer.get(&pair[1]))
                    && wa != wb
                    && set.contains(&wa)
                    && set.contains(&wb)
                {
                    keys.insert(key.clone());
                }
            }
        }
        keys.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op::{Op, Transaction, TxnOutcome};

    fn tx(id: TxnId, outcome: TxnOutcome, ops: Vec<Op>) -> Transaction {
        Transaction {
            id,
            session: id,
            ops,
            outcome,
            at: Moment(1000),
        }
    }

    fn op(at: u64, txn: TxnId, key: &str, kind: OpKind) -> Op {
        Op {
            session: txn,
            txn,
            kind,
            key: key.as_bytes().to_vec(),
            at: Moment(at),
        }
    }

    fn history(txns: Vec<Transaction>) -> History {
        History {
            txns: txns.into_iter().map(|t| (t.id, t)).collect(),
        }
    }

    /// A single serial append chain recovers a linear version order, wr/ww
    /// edges, and an rw anti-dependency — and has no cycle.
    #[test]
    fn edges_recovered_from_a_serial_append_chain() {
        // T1 appends 1; T2 reads [1] then appends 2. Final read [1,2].
        let h = history(vec![
            tx(
                1,
                TxnOutcome::Committed,
                vec![op(1, 1, "k", OpKind::Append(1))],
            ),
            tx(
                2,
                TxnOutcome::Committed,
                vec![
                    op(2, 2, "k", OpKind::Read(vec![1])),
                    op(3, 2, "k", OpKind::Append(2)),
                ],
            ),
            tx(
                3,
                TxnOutcome::Committed,
                vec![op(4, 3, "k", OpKind::Read(vec![1, 2]))],
            ),
        ]);
        let g = DepGraph::build(&h).expect("recoverable");
        assert_eq!(g.version_order(&b"k".to_vec()), Some(&[1, 2][..]));
        assert_eq!(g.writer(1), Some(1));
        assert_eq!(g.writer(2), Some(2));
        // wr: T1's write read by T2 and T3.
        assert!(g.wr_edges()[&1].contains(&2));
        // ww: version 1 (T1) precedes version 2 (T2).
        assert!(g.ww_edges()[&1].contains(&2));
        // rw: T2 read the tip [1] and T2 itself overwrote — same txn, no edge;
        // T3 read [1,2] (tip 2), the last version, so no overwriter → no rw.
        assert!(g.ww_cycle().is_none());
    }

    /// Conflicting per-key version orders across two keys form a ww cycle.
    #[test]
    fn conflicting_orders_form_a_ww_cycle() {
        let h = history(vec![
            tx(
                1,
                TxnOutcome::Committed,
                vec![
                    op(1, 1, "a", OpKind::Append(1)),
                    op(2, 1, "b", OpKind::Append(2)),
                ],
            ),
            tx(
                2,
                TxnOutcome::Committed,
                vec![
                    op(3, 2, "b", OpKind::Append(3)),
                    op(4, 2, "a", OpKind::Append(4)),
                ],
            ),
            // a: 4 (T2) before 1 (T1); b: 2 (T1) before 3 (T2) — a cycle.
            tx(
                3,
                TxnOutcome::Committed,
                vec![
                    op(5, 3, "a", OpKind::Read(vec![4, 1])),
                    op(6, 3, "b", OpKind::Read(vec![2, 3])),
                ],
            ),
        ]);
        let g = DepGraph::build(&h).expect("recoverable");
        let cycle = g.ww_cycle().expect("a ww cycle");
        let set: BTreeSet<TxnId> = cycle.iter().copied().collect();
        assert_eq!(set, [1, 2].into_iter().collect());
        assert_eq!(g.ww_keys_among(&set), vec![b"a".to_vec(), b"b".to_vec()]);
    }

    /// Register reads observing different single values recover a version order
    /// by time — not a false `InconsistentOrder`.
    #[test]
    fn register_version_order_is_recovered_by_time() {
        let h = history(vec![
            tx(
                1,
                TxnOutcome::Committed,
                vec![op(1, 1, "k", OpKind::Write(10))],
            ),
            tx(
                2,
                TxnOutcome::Committed,
                vec![
                    op(2, 2, "k", OpKind::Read(vec![10])),
                    op(3, 2, "k", OpKind::Write(20)),
                ],
            ),
            tx(
                3,
                TxnOutcome::Committed,
                vec![op(4, 3, "k", OpKind::Read(vec![20]))],
            ),
        ]);
        let g = DepGraph::build(&h).expect("recoverable");
        // Distinct observed values in first-seen order: 10 then 20.
        assert_eq!(g.version_order(&b"k".to_vec()), Some(&[10, 20][..]));
        assert!(g.ww_cycle().is_none());
    }

    /// An aborted writer stays out of the committed set (so its writes are
    /// visible to G1a but never a ww/committed edge).
    #[test]
    fn aborted_writer_is_not_committed() {
        let h = history(vec![
            tx(
                1,
                TxnOutcome::Aborted,
                vec![op(1, 1, "k", OpKind::Write(5))],
            ),
            tx(
                2,
                TxnOutcome::Committed,
                vec![op(2, 2, "k", OpKind::Read(vec![5]))],
            ),
        ]);
        let g = DepGraph::build(&h).expect("recoverable");
        assert!(!g.is_committed(1));
        assert!(g.is_committed(2));
        assert_eq!(g.writer(5), Some(1));
    }
}
