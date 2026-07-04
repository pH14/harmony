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
        //    tally, per key, the committed writes with their moments (for the
        //    register version order) and the committed append values (for the
        //    append completeness check).
        let mut write_key: BTreeMap<Elem, Key> = BTreeMap::new();
        let mut committed_writes: BTreeMap<Key, Vec<(Moment, Elem)>> = BTreeMap::new();
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
                    if t.committed() {
                        committed_writes
                            .entry(op.key.clone())
                            .or_default()
                            .push((op.at, v));
                        if matches!(op.kind, OpKind::Append(_)) {
                            committed_appends
                                .entry(op.key.clone())
                                .or_default()
                                .insert(v);
                        }
                    }
                }
            }
        }

        // 2. Recover each key's version order. The recovery is **model-aware**:
        //
        //    - **append keys** (any `Append` targets them): each read observes a
        //      *prefix* of the true append list, so the order is the longest
        //      observed list and every read must be one of its prefixes (a fork
        //      is unrecoverable → `InconsistentOrder`); completeness is checked
        //      below (every committed append must be observed).
        //    - **register keys** (writes only): the order is the committed writes
        //      in **write-moment** order. The deterministic timeline places every
        //      committed write, so the order is complete — an unobserved committed
        //      write can never silently drop a ww edge and hide a G0 cycle (the
        //      round-2 false-clean). Reads do **not** define register order (a
        //      read seeing a non-current version is an *anomaly*, not a
        //      reordering); they only validate value/key attribution here and
        //      feed wr/rw/lost-update elsewhere.
        //
        //    Every observed value must have a writer, under the right key.
        let mut append_keys: BTreeSet<Key> = BTreeSet::new();
        let mut register_keys: BTreeSet<Key> = BTreeSet::new();
        for t in h.iter() {
            for op in &t.ops {
                match op.kind {
                    OpKind::Append(_) => {
                        append_keys.insert(op.key.clone());
                    }
                    OpKind::Write(_) => {
                        register_keys.insert(op.key.clone());
                    }
                    OpKind::Read(_) => {}
                }
            }
        }
        // A key written by BOTH models is unrecoverable — its version order can't
        // be a list order and a write-moment order at once, and classifying it as
        // one would silently drop the other's writes (a false-clean channel).
        if let Some(key) = append_keys.intersection(&register_keys).next() {
            return Err(DecodeError::MixedModel { key: key.clone() });
        }
        let mut lists_by_key: BTreeMap<Key, Vec<Vec<Elem>>> = BTreeMap::new();
        for t in h.iter() {
            for op in &t.ops {
                if let OpKind::Read(vs) = &op.kind {
                    let mut seen_in_list: BTreeSet<Elem> = BTreeSet::new();
                    for &e in vs {
                        // A value repeated within one observed list is malformed:
                        // written values are unique, so a repeat would fabricate a
                        // spurious ww edge (a false dirty-write) if accepted as an
                        // order. Fail loud.
                        if !seen_in_list.insert(e) {
                            return Err(DecodeError::RepeatedObservation {
                                key: op.key.clone(),
                                value: e,
                            });
                        }
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
                    }
                }
            }
        }
        // Append keys: longest observed list, every read a prefix of it.
        for (key, lists) in &lists_by_key {
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
        // Register keys: committed writes in write-moment order (complete).
        for (key, writes) in &committed_writes {
            if append_keys.contains(key) {
                continue;
            }
            let mut ws = writes.clone();
            ws.sort_unstable(); // by (moment, value) — a deterministic total order
            g.version_order
                .insert(key.clone(), ws.into_iter().map(|(_, v)| v).collect());
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

        // Write-write: consecutive writers in each key's **committed** version
        // order. The committed subsequence is paired (not the raw order), so an
        // aborted version observed between two committed writes never breaks the
        // committed→committed edge across it (round-4: else a G0 cycle spanning
        // the aborted gap would be judged clean).
        for order in self.version_order.values() {
            let seq = self.committed_subseq(order);
            for pair in seq.windows(2) {
                if let (Some(&wa), Some(&wb)) =
                    (self.writer.get(&pair[0]), self.writer.get(&pair[1]))
                    && wa != wb
                {
                    self.ww.entry(wa).or_default().insert(wb);
                }
            }
        }
    }

    /// The values of `order` whose writer **committed**, preserving order — the
    /// sequence ww edges are paired over, so aborted intermediates are skipped
    /// (never breaking a committed→committed edge). A no-op for register keys
    /// (their order is already committed-only) and load-bearing for append keys
    /// (whose observed order may interleave dirty-read aborted values).
    fn committed_subseq(&self, order: &[Elem]) -> Vec<Elem> {
        order
            .iter()
            .copied()
            .filter(|v| {
                self.writer
                    .get(v)
                    .is_some_and(|w| self.committed.contains(w))
            })
            .collect()
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
            // Pair the committed subsequence (round-4 twin of the ww-edge fix),
            // so an aborted intermediate does not hide a witnessing key.
            let seq = self.committed_subseq(order);
            for pair in seq.windows(2) {
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

    /// A register's version order is the committed writes in **write-moment**
    /// order — reads observing different single values are fine (not a false
    /// `InconsistentOrder`), and every committed write is placed.
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
        // Committed writes in write-moment order: 10 (@1) then 20 (@3).
        assert_eq!(g.version_order(&b"k".to_vec()), Some(&[10, 20][..]));
        assert!(g.ww_cycle().is_none());
    }

    /// A committed register write that no read observes is still placed by its
    /// write moment (the round-2 false-clean fix): here `a=4` is never read, but
    /// it is ordered before the final `a=1`, so the ww edge exists.
    #[test]
    fn unobserved_register_write_is_still_ordered() {
        let h = history(vec![
            tx(
                1,
                TxnOutcome::Committed,
                vec![op(3, 1, "a", OpKind::Write(1))], // later moment → final
            ),
            tx(
                2,
                TxnOutcome::Committed,
                vec![op(1, 2, "a", OpKind::Write(4))], // earlier moment, never read
            ),
        ]);
        let g = DepGraph::build(&h).expect("recoverable");
        assert_eq!(
            g.version_order(&b"a".to_vec()),
            Some(&[4, 1][..]),
            "4 before 1 by moment"
        );
        assert!(g.ww_edges()[&2].contains(&1), "T2 (a=4) →ww T1 (a=1)");
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
