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
    /// Each key's recovered version order. For an **append** key this is the full
    /// observed list (a total order). For a **register** key it holds only what is
    /// *witnessed*: the pinned final version last, preceded by the sole
    /// predecessor when there is exactly one — with two or more unwitnessed
    /// predecessors their relative order is unrecoverable, so it is **not**
    /// fabricated (only the final appears; the predecessors are known via
    /// [`ww_contrib`](Self::ww_contrib) and [`writer`](Self::writer), unordered).
    version_order: BTreeMap<Key, Vec<Elem>>,
    /// A **register** key's pinned final version (its settled current value): the
    /// value a quiesce read observed, or the lone committed write. Drives the
    /// register rw edge (a read of any non-final version is anti-dependent on the
    /// final writer) without appealing to a fabricated predecessor order.
    register_final: BTreeMap<Key, Elem>,
    /// Write-read edges: writer → readers of its writes.
    wr: BTreeMap<TxnId, BTreeSet<TxnId>>,
    /// Write-write edges (committed writers only): earlier → later in version
    /// order. Derived from [`ww_contrib`](Self::ww_contrib).
    ww: BTreeMap<TxnId, BTreeSet<TxnId>>,
    /// Every ww edge with the key that witnesses it: `(earlier, later, key)`.
    /// Register keys contribute a **star** (each non-final writer → the final
    /// writer) rather than adjacency, so this cannot be recovered by re-pairing a
    /// linear order — it is stored directly.
    ww_contrib: BTreeSet<(TxnId, TxnId, Key)>,
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
        // The latest **commit** Moment among a key's committed writers — the
        // cutoff for a quiesce read. A write op's Moment is NOT the cutoff: a
        // writer's value only becomes the settled version at its *commit*, which
        // can be arbitrarily later (writes-then-late-commits). A read is a final
        // read only if it follows every committed writer's commit; a read after
        // the writes but before some writer's commit saw a not-yet-final state and
        // must not pin the order.
        let mut last_commit_moment: BTreeMap<Key, Moment> = BTreeMap::new();
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
                        last_commit_moment
                            .entry(op.key.clone())
                            .and_modify(|m| *m = (*m).max(t.at))
                            .or_insert(t.at);
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
        //    - **register keys** (writes only): the order is fixed by **quiesce
        //      reads** — a read after **every committed writer has committed** sees
        //      the *final* version; every other committed writer precedes it (a
        //      **star** of ww edges to the final writer). Write Moments are NOT
        //      order evidence (they are the issue order, which can differ from the
        //      committed version order — the round-7 counterexample). A single
        //      committed write needs no read (unambiguous); **two or more** with
        //      no quiesce read are unrecoverable (`UnpinnedRegister`) — a real
        //      workload ends with a final read, so a missing one is a fail-loud
        //      recoverability gap, never an order fabricated by sorting.
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
        // be a list order and a register order at once, and classifying it as one
        // would silently drop the other's writes (a false-clean channel).
        if let Some(key) = append_keys.intersection(&register_keys).next() {
            return Err(DecodeError::MixedModel { key: key.clone() });
        }
        let mut lists_by_key: BTreeMap<Key, Vec<Vec<Elem>>> = BTreeMap::new();
        // Register quiesce reads: `(value)` observed by a read after all writes.
        let mut reg_quiesce: BTreeMap<Key, BTreeSet<Elem>> = BTreeMap::new();
        for t in h.iter() {
            for op in &t.ops {
                if let OpKind::Read(vs) = &op.kind {
                    // A register (non-append) key's reads observe a singleton or
                    // empty value. A multi-value read of one is malformed under
                    // the op model — never let it fall silently through order
                    // recovery (which would judge it clean).
                    if !append_keys.contains(&op.key) && vs.len() > 1 {
                        return Err(DecodeError::MultiValueRegisterRead {
                            key: op.key.clone(),
                            count: vs.len(),
                        });
                    }
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
                    // Only a **committed** reader's observations are authoritative
                    // order evidence — an aborted transaction may have read a
                    // dirty/inconsistent snapshot (indeed that can be why it
                    // aborted), so its reads must never fix the version order.
                    if !t.committed() {
                        continue;
                    }
                    if append_keys.contains(&op.key) {
                        lists_by_key
                            .entry(op.key.clone())
                            .or_default()
                            .push(vs.clone());
                    } else {
                        // A register read is a quiesce (final-version) read iff it
                        // is stamped AFTER **every committed writer has committed**
                        // — not merely after their write ops. A read between the
                        // writes and a late commit saw a not-yet-settled state and
                        // pins nothing.
                        let after_all_commits =
                            last_commit_moment.get(&op.key).is_none_or(|&lc| op.at > lc);
                        match vs.last() {
                            // Observes a committed value at quiesce → pins the final.
                            Some(&tip) => {
                                if after_all_commits
                                    && g.writer.get(&tip).is_some_and(|w| g.committed.contains(w))
                                {
                                    reg_quiesce.entry(op.key.clone()).or_default().insert(tip);
                                }
                            }
                            // The **empty** initial version at quiesce, while the
                            // key has a committed writer, contradicts that write —
                            // a committed value cannot be unwritten at quiesce.
                            // Never drop it (which settles on the write and judges
                            // the lost write clean); fail loud.
                            None => {
                                if after_all_commits && last_commit_moment.contains_key(&op.key) {
                                    return Err(DecodeError::EmptyFinalRead {
                                        key: op.key.clone(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        // Append keys: longest observed list, every read a prefix of it; then ww
        // over the committed subsequence's adjacency.
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
            let committed_seq: Vec<Elem> = candidate
                .iter()
                .copied()
                .filter(|&v| g.is_committed_value(v))
                .collect();
            for pair in committed_seq.windows(2) {
                g.add_ww_edge(pair[0], pair[1], key);
            }
            g.version_order.insert(key.clone(), candidate);
        }
        // The recoverability contract for **append** keys: final reads at quiesce
        // fix version order, so every committed append must appear in the
        // recovered order. If one does not (a missing final read, or no read of
        // the key at all), the order is incomplete — its ww edges are missing and
        // a real dirty-write could be judged clean. Fail loud instead.
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
        // Register keys: a quiesce read pins the final version; every other
        // committed writer precedes it (a **star** of ww edges to the final
        // writer). The predecessors are otherwise **unordered** — with two or
        // more, their relative order is unrecoverable, so it is never fabricated
        // (that fabrication would leak into `version_order` and mint rw edges to
        // an arbitrary "next" writer chosen by numeric value). A single committed
        // write is its own unambiguous final; two or more with no quiesce read
        // are unrecoverable (`UnpinnedRegister`, the register twin of
        // `UnobservedAppend`).
        for key in &register_keys {
            let committed_vals: Vec<Elem> = committed_writes
                .get(key)
                .map(|w| w.iter().map(|(_, v)| *v).collect())
                .unwrap_or_default();
            let quiesced = reg_quiesce.get(key);
            // All quiesce reads (after all writes) must agree on the final value.
            if let Some(q) = quiesced
                && q.len() > 1
            {
                return Err(DecodeError::InconsistentOrder { key: key.clone() });
            }
            let final_v = quiesced.and_then(|q| q.iter().next().copied());
            if committed_vals.len() >= 2 && final_v.is_none() {
                return Err(DecodeError::UnpinnedRegister {
                    key: key.clone(),
                    writes: committed_vals.len(),
                });
            }
            // The settled final: the quiesce-pinned value, or a lone committed
            // write (its own final). Absent only when no write committed.
            let settled = final_v.or_else(|| committed_vals.first().copied());
            let Some(fv) = settled else {
                continue;
            };
            g.register_final.insert(key.clone(), fv);
            let predecessors: Vec<Elem> = committed_vals
                .iter()
                .copied()
                .filter(|&v| v != fv)
                .collect();
            // Star: every predecessor precedes the final writer.
            for &v in &predecessors {
                g.add_ww_edge(v, fv, key);
            }
            // version_order is witnessed only when there is at most one
            // predecessor (a unique order); with two or more it is left partial
            // (just the final) rather than invent one.
            let order = match predecessors.as_slice() {
                [] => vec![fv],
                [pred] => vec![*pred, fv],
                _ => vec![fv],
            };
            g.version_order.insert(key.clone(), order);
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
                    // Read-write anti-dependency: the reader of a version is
                    // anti-dependent on whoever overwrote it.
                    let overwriter = if let Some(&fv) = self.register_final.get(&op.key) {
                        // A **register**: any read that did not observe the settled
                        // final version (a non-final version, or the empty initial)
                        // is anti-dependent on the final writer — the one witnessed
                        // overwriter, chosen without a fabricated predecessor order.
                        match vs.last() {
                            Some(&tip) if tip == fv => None,
                            _ => Some(fv),
                        }
                    } else if let Some(order) = self.version_order.get(&op.key) {
                        // An **append**: the next version in the observed list. An
                        // empty read observed the initial version, so its
                        // overwriter is the key's first appended value.
                        match vs.last() {
                            Some(&tip) => order
                                .iter()
                                .position(|&e| e == tip)
                                .and_then(|pos| order.get(pos + 1))
                                .copied(),
                            None => order.first().copied(),
                        }
                    } else {
                        None
                    };
                    if let Some(next) = overwriter
                        && let Some(&wnext) = self.writer.get(&next)
                        && wnext != t.id
                    {
                        self.rw.entry(t.id).or_default().insert(wnext);
                    }
                }
            }
        }
        // ww edges are minted during recovery (append adjacency + register star);
        // see [`add_ww_edge`](Self::add_ww_edge).
    }

    /// Whether `value`'s writer exists and committed.
    fn is_committed_value(&self, value: Elem) -> bool {
        self.writer
            .get(&value)
            .is_some_and(|w| self.committed.contains(w))
    }

    /// Record a ww edge writer(`a`) → writer(`b`) witnessed by `key`, iff both
    /// writers are distinct and committed. Updates both the adjacency
    /// ([`ww`](Self::ww)) and the witnessed contributions
    /// ([`ww_contrib`](Self::ww_contrib)).
    fn add_ww_edge(&mut self, a: Elem, b: Elem, key: &Key) {
        if let (Some(&wa), Some(&wb)) = (self.writer.get(&a), self.writer.get(&b))
            && wa != wb
            && self.committed.contains(&wa)
            && self.committed.contains(&wb)
        {
            self.ww.entry(wa).or_default().insert(wb);
            self.ww_contrib.insert((wa, wb, key.clone()));
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

    /// The nodes reachable from `start` by following ww edges (excluding `start`
    /// unless a cycle returns to it).
    fn ww_reachable(&self, start: TxnId) -> BTreeSet<TxnId> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![start];
        while let Some(u) = stack.pop() {
            if let Some(vs) = self.ww.get(&u) {
                for &v in vs {
                    if seen.insert(v) {
                        stack.push(v);
                    }
                }
            }
        }
        seen
    }

    /// Every **independent** write-write cycle as a strongly-connected component
    /// (each with >= 2 transactions, since ww has no self-loops), sorted. Two
    /// disjoint G0 cycles are two SCCs. Deterministic (`u, v` are in one SCC iff
    /// `u` reaches `v` and `v` reaches `u`).
    pub fn ww_sccs(&self) -> Vec<Vec<TxnId>> {
        let mut nodes: BTreeSet<TxnId> = BTreeSet::new();
        for (&u, vs) in &self.ww {
            nodes.insert(u);
            nodes.extend(vs.iter().copied());
        }
        let reaches: BTreeMap<TxnId, BTreeSet<TxnId>> =
            nodes.iter().map(|&u| (u, self.ww_reachable(u))).collect();
        let mut assigned: BTreeSet<TxnId> = BTreeSet::new();
        let mut sccs = Vec::new();
        for &u in &nodes {
            // A node is on a cycle iff it reaches itself; skip trivial SCCs.
            if assigned.contains(&u) || !reaches[&u].contains(&u) {
                continue;
            }
            let scc: Vec<TxnId> = nodes
                .iter()
                .copied()
                .filter(|&v| reaches[&u].contains(&v) && reaches[&v].contains(&u))
                .collect();
            for &v in &scc {
                assigned.insert(v);
            }
            if scc.len() >= 2 {
                sccs.push(scc);
            }
        }
        sccs
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

    /// The keys that witness a ww edge between two members of `set` — the
    /// constructive key witness for a ww-cycle (G0) finding. Read straight off
    /// the recorded [`ww_contrib`](Self::ww_contrib) (which stores the star edges
    /// register keys mint), so it stays exact for both models. Sorted and
    /// deduplicated.
    pub fn ww_keys_among(&self, set: &BTreeSet<TxnId>) -> Vec<Key> {
        let mut keys: BTreeSet<Key> = BTreeSet::new();
        for (a, b, key) in &self.ww_contrib {
            if set.contains(a) && set.contains(b) {
                keys.insert(key.clone());
            }
        }
        keys.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op::{Op, Transaction, TxnOutcome};

    // The commit Moment is the txn's last op Moment — so a read stamped after a
    // key's writers' ops is also after their commits (the quiesce cutoff is the
    // commit Moment). Tests wanting a late commit set `at` explicitly.
    fn tx(id: TxnId, outcome: TxnOutcome, ops: Vec<Op>) -> Transaction {
        let at = ops.iter().map(|o| o.at).max().unwrap_or(Moment(1000));
        Transaction {
            id,
            session: id,
            ops,
            outcome,
            at,
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

    /// A register's version order is pinned by the **quiesce read** (the read
    /// after all writes): `R[20]` after both writes fixes 20 as the final
    /// version, so 10 precedes it. Reads observing different single values are
    /// fine (not a false `InconsistentOrder`).
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

    /// Register order comes from the **quiesce read** (round-7): a final read
    /// pinning `a=1` means every *other* committed writer of `a` (T2's `a=4`)
    /// precedes it — the star ww edge T2 → T1 — even though `a=4` was written at
    /// a *later* Moment (write Moments are the issue order, not the version
    /// order). Without any read, the two writes would be unordered (no edge).
    #[test]
    fn register_order_from_quiesce_read_pins_final() {
        let h = history(vec![
            tx(
                1,
                TxnOutcome::Committed,
                vec![op(1, 1, "a", OpKind::Write(1))],
            ),
            tx(
                2,
                TxnOutcome::Committed,
                vec![op(2, 2, "a", OpKind::Write(4))], // later Moment...
            ),
            tx(
                3,
                TxnOutcome::Committed,
                vec![op(3, 3, "a", OpKind::Read(vec![1]))], // ...but the final read sees a=1
            ),
        ]);
        let g = DepGraph::build(&h).expect("recoverable");
        assert_eq!(
            g.version_order(&b"a".to_vec()),
            Some(&[4, 1][..]),
            "4 precedes the read-pinned final 1"
        );
        assert!(
            g.ww_edges()[&2].contains(&1),
            "T2 (a=4) →ww T1 (a=1, final)"
        );
    }

    /// Round-9 P1: two committed register writes to a key with **no quiesce read**
    /// are unrecoverable — the order can't be fabricated by sorting, so build
    /// fails loud with `UnpinnedRegister` rather than silently ordering by value.
    #[test]
    fn multi_write_register_without_a_quiesce_read_is_unrecoverable() {
        let h = history(vec![
            tx(
                1,
                TxnOutcome::Committed,
                vec![op(1, 1, "a", OpKind::Write(1))],
            ),
            tx(
                2,
                TxnOutcome::Committed,
                vec![op(2, 2, "a", OpKind::Write(2))],
            ),
        ]);
        assert_eq!(
            DepGraph::build(&h),
            Err(DecodeError::UnpinnedRegister {
                key: b"a".to_vec(),
                writes: 2,
            })
        );
    }

    /// A **single** committed register write needs no read — it is unambiguous, so
    /// it is recoverable (not `UnpinnedRegister`).
    #[test]
    fn a_single_register_write_needs_no_quiesce_read() {
        let h = history(vec![tx(
            1,
            TxnOutcome::Committed,
            vec![op(1, 1, "a", OpKind::Write(1))],
        )]);
        let g = DepGraph::build(&h).expect("a single write is recoverable");
        assert_eq!(g.version_order(&b"a".to_vec()), Some(&[1][..]));
    }

    /// Round-10 P2: with **three** committed register writers and only a final
    /// read pinning the last version, the two predecessors' relative order is
    /// unrecoverable and must NOT be fabricated. Writers `100, 1`, final `3`: the
    /// old code sorted `{100, 1}` by value into `[1, 100, 3]` and minted a
    /// spurious rw `T3 -> T1` (value `1`'s "next" is `100`). Now only the star to
    /// the final and the pinned final are witnessed.
    #[test]
    fn three_writer_register_does_not_fabricate_predecessor_order() {
        let h = history(vec![
            tx(
                1,
                TxnOutcome::Committed,
                vec![op(1, 1, "k", OpKind::Write(100))],
            ),
            tx(
                2,
                TxnOutcome::Committed,
                vec![
                    op(2, 2, "k", OpKind::Read(vec![100])),
                    op(3, 2, "k", OpKind::Write(1)),
                ],
            ),
            tx(
                3,
                TxnOutcome::Committed,
                vec![
                    op(4, 3, "k", OpKind::Read(vec![1])),
                    op(5, 3, "k", OpKind::Write(3)),
                ],
            ),
            // The final read pins 3 as the settled version.
            tx(
                4,
                TxnOutcome::Committed,
                vec![op(6, 4, "k", OpKind::Read(vec![3]))],
            ),
        ]);
        let g = DepGraph::build(&h).expect("a quiesce read pins the final");
        // Only the final is in the public order — no fabricated `[1, 100, 3]`.
        assert_eq!(g.version_order(&b"k".to_vec()), Some(&[3][..]));
        // A star to the final writer (T3); no predecessor→predecessor edge.
        assert!(g.ww_edges()[&1].contains(&3), "T1 (100) →ww T3 (final)");
        assert!(g.ww_edges()[&2].contains(&3), "T2 (1) →ww T3 (final)");
        assert!(g.ww_cycle().is_none(), "a star has no cycle");
        // The value-sorted fabrication would have minted rw `T3 -> T1`; it must not.
        assert!(
            g.rw_edges().get(&3).is_none_or(|s| !s.contains(&1)),
            "no rw edge fabricated from numeric predecessor value order"
        );
    }

    /// Round-11 P1: the quiesce cutoff is a committed writer's **commit** Moment,
    /// not its write-op Moment. T1 writes `k=1` @1 but commits @10; T2 writes
    /// `k=2` @2 but commits @11; T3 reads `k=1` @5 — after both write *ops* but
    /// before either *commit*. A write-op cutoff would wrongly treat the read as
    /// final and pin the order; the commit cutoff pins nothing, so the two-writer
    /// register is unrecoverable (`UnpinnedRegister`).
    #[test]
    fn a_read_before_a_late_commit_is_not_a_quiesce_read() {
        let late = |id: TxnId, at: u64, ops: Vec<Op>| Transaction {
            id,
            session: id,
            ops,
            outcome: TxnOutcome::Committed,
            at: Moment(at),
        };
        let h = history(vec![
            late(1, 10, vec![op(1, 1, "k", OpKind::Write(1))]),
            late(2, 11, vec![op(2, 2, "k", OpKind::Write(2))]),
            late(3, 12, vec![op(5, 3, "k", OpKind::Read(vec![1]))]),
        ]);
        assert_eq!(
            DepGraph::build(&h),
            Err(DecodeError::UnpinnedRegister {
                key: b"k".to_vec(),
                writes: 2,
            }),
            "a read before the writers' commits pins nothing"
        );

        // The same read stamped after both commits (@20) IS a quiesce read.
        let ok = history(vec![
            late(1, 10, vec![op(1, 1, "k", OpKind::Write(1))]),
            late(2, 11, vec![op(2, 2, "k", OpKind::Write(2))]),
            late(3, 21, vec![op(20, 3, "k", OpKind::Read(vec![1]))]),
        ]);
        let g = DepGraph::build(&ok).expect("a post-commit read pins the final");
        assert_eq!(g.version_order(&b"k".to_vec()), Some(&[2, 1][..]));
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

    /// Round-6 P2: an empty read observes the initial/unwritten version, so it
    /// mints an rw anti-dependency to the key's FIRST writer. Two cross-reading
    /// transactions form an **initial-version rw cycle** the public graph must
    /// represent (previously empty reads minted no rw edge at all).
    #[test]
    fn empty_read_mints_rw_to_first_writer() {
        // T1 reads a (initial) then writes b; T2 reads b (initial) then writes a.
        // Both empty reads (@1,@2) are BEFORE either writer's commit (@3,@4), so
        // they legitimately saw the initial version (not the round-13 post-commit
        // contradiction) and are anti-dependent on the overwriter.
        let h = history(vec![
            tx(
                1,
                TxnOutcome::Committed,
                vec![
                    op(1, 1, "a", OpKind::Read(vec![])), // initial version of a
                    op(3, 1, "b", OpKind::Write(10)),
                ],
            ),
            tx(
                2,
                TxnOutcome::Committed,
                vec![
                    op(2, 2, "b", OpKind::Read(vec![])), // initial version of b
                    op(4, 2, "a", OpKind::Write(20)),
                ],
            ),
        ]);
        let g = DepGraph::build(&h).expect("recoverable");
        // a's first writer is T2 (a<-20); T1's empty read of a is anti-dependent
        // on it. b's first writer is T1; T2's empty read of b is anti-dependent
        // on it. Together: an rw cycle T1 ⇄ T2.
        assert!(g.rw_edges()[&1].contains(&2), "T1 →rw T2 (empty read of a)");
        assert!(g.rw_edges()[&2].contains(&1), "T2 →rw T1 (empty read of b)");
    }
}
