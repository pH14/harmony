// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **operation-history model** the checker judges (task 75).
//!
//! An [`Op`] is one read/write/append inside a transaction, stamped with the
//! [`Moment`](explorer::Moment) it was observed. A [`Transaction`] groups a
//! session's ops with its commit/abort [`TxnOutcome`], and a [`History`] is the
//! whole run's set of transactions, keyed by id for deterministic iteration.
//!
//! **Recoverability is the workload's job** (the thin-SDK ruling): writes carry
//! **unique values** (or list-append), so a read that observes a value recovers
//! *which* write it read; and a final read at quiesce fixes each key's version
//! order. The model itself stores only what a run emitted — recovering the
//! dependency structure, and refusing to *guess* when a history is
//! unrecoverable, is [`graph`](crate::graph)'s job.

use std::collections::BTreeMap;

use explorer::Moment;

/// A transaction identifier — unique within a run.
pub type TxnId = u64;

/// A session (client/connection) identifier: which serial stream of
/// transactions an op belongs to.
pub type Session = u64;

/// A key the workload reads and writes. Bytes, so a key is never lossy-decoded.
pub type Key = Vec<u8>;

/// A written element / value. **Unique** across a recoverable history's writes,
/// so a read that observes it attributes it to exactly one writer.
pub type Elem = i64;

/// One operation inside a transaction.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum OpKind {
    /// A read observing the key's value(s): for a register key the singleton
    /// current value (empty when the key is unwritten); for an append key the
    /// full observed list, in append order. The observed list is what recovers
    /// version order and write-read edges.
    Read(Vec<Elem>),
    /// A register write of a (unique) value.
    Write(Elem),
    /// An append of a (unique) element onto the key's list.
    Append(Elem),
}

/// One operation: which session/transaction issued it, its kind, the key, and
/// the [`Moment`] it was observed.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Op {
    /// The session that issued the op.
    pub session: Session,
    /// The transaction the op belongs to.
    pub txn: TxnId,
    /// The op's kind (and its value / observed list).
    pub kind: OpKind,
    /// The key read or written.
    pub key: Key,
    /// When the op was observed.
    pub at: Moment,
}

impl Op {
    /// The value this op *wrote*, if it is a write or an append.
    pub fn written(&self) -> Option<Elem> {
        match self.kind {
            OpKind::Write(v) | OpKind::Append(v) => Some(v),
            OpKind::Read(_) => None,
        }
    }

    /// The list this op *observed*, if it is a read.
    pub fn observed(&self) -> Option<&[Elem]> {
        match &self.kind {
            OpKind::Read(vs) => Some(vs),
            _ => None,
        }
    }

    /// The **version** this read observed of its key: the tip (last element) of
    /// the observed list — the specific version the transaction saw. `None` for
    /// a read of an unwritten key (empty list), or for a non-read op.
    pub fn observed_version(&self) -> Option<Elem> {
        self.observed().and_then(|vs| vs.last().copied())
    }
}

/// Whether a transaction committed or aborted — the recoverability boundary a
/// committed read of an aborted write (G1a) crosses.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum TxnOutcome {
    /// The transaction committed; its writes are durable.
    Committed,
    /// The transaction aborted; its writes should never have been observed.
    Aborted,
}

/// One transaction: its session, its ops in program (Moment) order, and its
/// outcome.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Transaction {
    /// The transaction id.
    pub id: TxnId,
    /// The session it ran in.
    pub session: Session,
    /// The ops, in ascending [`Moment`] order (program order within the txn).
    pub ops: Vec<Op>,
    /// Whether it committed or aborted.
    pub outcome: TxnOutcome,
    /// The [`Moment`] it committed or aborted at.
    pub at: Moment,
}

impl Transaction {
    /// Whether the transaction committed.
    pub fn committed(&self) -> bool {
        self.outcome == TxnOutcome::Committed
    }

    /// The earliest [`Moment`] this transaction touched — its earliest op, or
    /// its commit moment if it had no ops. Computed as the minimum, so it is
    /// robust to op order (the decoder sorts, but callers need not).
    pub fn first_moment(&self) -> Moment {
        self.ops.iter().map(|o| o.at).min().unwrap_or(self.at)
    }
}

/// A whole run's operation history: transactions keyed by id (a `BTreeMap`, so
/// every traversal is deterministic — conventions rule 4).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct History {
    /// The transactions, by id.
    pub txns: BTreeMap<TxnId, Transaction>,
}

impl History {
    /// An empty history.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of transactions.
    pub fn len(&self) -> usize {
        self.txns.len()
    }

    /// Whether the history holds no transactions.
    pub fn is_empty(&self) -> bool {
        self.txns.is_empty()
    }

    /// The transactions in id order.
    pub fn iter(&self) -> impl Iterator<Item = &Transaction> {
        self.txns.values()
    }

    /// Every op across all transactions, ascending by `(Moment, txn, kind)` then
    /// a **total** tie-break on the whole [`Op`] — so the canonical global order
    /// is a pure function of content (two ops equal on `(Moment, txn, kind)` but
    /// differing in key/session can never reorder non-deterministically across
    /// runs), independent of decode order.
    pub fn ops_in_time_order(&self) -> Vec<&Op> {
        let mut ops: Vec<&Op> = self.txns.values().flat_map(|t| t.ops.iter()).collect();
        ops.sort_by(|a, b| {
            a.at.cmp(&b.at)
                .then(a.txn.cmp(&b.txn))
                .then(a.kind.cmp(&b.kind))
                .then_with(|| a.cmp(b))
        });
        ops
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(at: u64, txn: TxnId, kind: OpKind) -> Op {
        Op {
            session: 1,
            txn,
            kind,
            key: b"k".to_vec(),
            at: Moment(at),
        }
    }

    /// `written`/`observed`/`observed_version` read the right field per kind.
    #[test]
    fn op_accessors_are_pinned() {
        let w = op(1, 1, OpKind::Write(9));
        assert_eq!(w.written(), Some(9));
        assert_eq!(w.observed(), None);
        assert_eq!(w.observed_version(), None);

        let a = op(2, 1, OpKind::Append(4));
        assert_eq!(a.written(), Some(4));

        let r = op(3, 1, OpKind::Read(vec![7, 8]));
        assert_eq!(r.written(), None);
        assert_eq!(r.observed(), Some(&[7, 8][..]));
        assert_eq!(r.observed_version(), Some(8), "the tip is the version read");

        let empty = op(4, 1, OpKind::Read(vec![]));
        assert_eq!(
            empty.observed_version(),
            None,
            "an unwritten read has no version"
        );
    }

    /// `committed`, `first_moment`, and the canonical global op order.
    #[test]
    fn transaction_and_history_helpers() {
        let t = Transaction {
            id: 5,
            session: 2,
            ops: vec![op(30, 5, OpKind::Read(vec![])), op(10, 5, OpKind::Write(1))],
            outcome: TxnOutcome::Committed,
            at: Moment(40),
        };
        assert!(t.committed());
        assert_eq!(t.first_moment(), Moment(10), "the earliest op moment");

        let empty = Transaction {
            id: 6,
            session: 2,
            ops: vec![],
            outcome: TxnOutcome::Aborted,
            at: Moment(50),
        };
        assert!(!empty.committed());
        assert_eq!(
            empty.first_moment(),
            Moment(50),
            "falls back to the commit moment"
        );

        let mut h = History::new();
        assert!(h.is_empty());
        h.txns.insert(5, t);
        h.txns.insert(6, empty);
        assert_eq!(h.len(), 2);
        let ordered: Vec<Moment> = h.ops_in_time_order().iter().map(|o| o.at).collect();
        assert_eq!(ordered, vec![Moment(10), Moment(30)], "sorted by moment");
    }

    /// Round-13 P3: the global op order has a **total** tie-break, so ops equal on
    /// `(Moment, txn, kind)` but differing in key still sort deterministically —
    /// a pure function of content, independent of which transaction they came
    /// from (map iteration order can't leak in).
    #[test]
    fn global_op_order_is_total() {
        // Two reads of `[]` at the same Moment in the same txn on different keys —
        // equal on (at, txn, kind), distinguished only by key.
        let on = |key: &str| Op {
            session: 1,
            txn: 7,
            kind: OpKind::Read(vec![]),
            key: key.as_bytes().to_vec(),
            at: Moment(5),
        };
        let order = |ops: Vec<Op>| {
            let t = Transaction {
                id: 7,
                session: 1,
                ops,
                outcome: TxnOutcome::Committed,
                at: Moment(9),
            };
            let mut h = History::new();
            h.txns.insert(7, t);
            h.ops_in_time_order()
                .iter()
                .map(|o| o.key.clone())
                .collect::<Vec<_>>()
        };
        // Insertion order does not decide the tie: both orderings canonicalize the
        // same way (by key).
        assert_eq!(order(vec![on("b"), on("a")]), order(vec![on("a"), on("b")]));
        assert_eq!(
            order(vec![on("b"), on("a")]),
            vec![b"a".to_vec(), b"b".to_vec()]
        );
    }
}
