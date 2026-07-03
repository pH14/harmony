// SPDX-License-Identifier: AGPL-3.0-or-later
//! The single fail-loud error the checker raises: [`DecodeError`].
//!
//! Recoverability is the workload's job. When a history cannot be recovered —
//! a value written twice (so a read can't be attributed to one writer), a
//! transaction with ops but no commit/abort marker, reads that disagree on a
//! key's version order — the checker **fails loud** with a `DecodeError` rather
//! than guessing an anomaly out of ambiguous data (the thin-SDK ruling).

use thiserror::Error;

use crate::op::{Elem, Key, TxnId};

/// A history that cannot be decoded or recovered. Never a silent "no anomaly":
/// the checker refuses to guess, and the campaign surfaces this loudly.
#[derive(Clone, PartialEq, Eq, Debug, Error)]
pub enum DecodeError {
    /// A record line or event could not be parsed into an [`Op`](crate::Op) or
    /// a lifecycle marker. Carries the offending text/context.
    #[error("malformed op source: {0}")]
    Malformed(String),

    /// A value was written by two different operations, so a read observing it
    /// cannot be attributed to a single writer — the history is unrecoverable
    /// (the workload must emit unique written values).
    #[error("value {value} written by both txn {first} and txn {second} (non-unique write)")]
    DuplicateValue {
        /// The value written twice.
        value: Elem,
        /// The first writer seen.
        first: TxnId,
        /// The second writer seen.
        second: TxnId,
    },

    /// A read observed a value no operation in the history wrote — the history
    /// is unrecoverable (a value appeared from nowhere).
    #[error("read observed value {value} on key {key:?} that no write produced")]
    UnknownValue {
        /// The unattributable value.
        value: Elem,
        /// The key it was observed on.
        key: Key,
    },

    /// Two reads of the same key observed incompatible version orders (neither a
    /// prefix of the other), so the key's version order cannot be recovered.
    #[error("reads of key {key:?} disagree on version order (unrecoverable)")]
    InconsistentOrder {
        /// The key whose reads disagree.
        key: Key,
    },

    /// A transaction issued operations but never committed or aborted, so its
    /// outcome (and thus whether its writes are visible) is unknown.
    #[error("transaction {0} has operations but no commit/abort marker")]
    UnterminatedTxn(TxnId),
}
