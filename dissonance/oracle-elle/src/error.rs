// SPDX-License-Identifier: AGPL-3.0-or-later
//! The single fail-loud error the checker raises: [`DecodeError`].
//!
//! Recoverability is the workload's job. When a history cannot be recovered —
//! a value written twice (so a read can't be attributed to one writer), a
//! transaction with ops but no commit/abort marker, reads that disagree on a
//! key's version order — the checker **fails loud** with a `DecodeError` rather
//! than guessing an anomaly out of ambiguous data (the thin-SDK ruling).

use thiserror::Error;

use crate::op::{Elem, Key, TxnId, TxnOutcome};

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

    /// A single operation carried more than one of the mutually-exclusive
    /// `W`/`A`/`R` payloads, so its kind is ambiguous — a mis-classified op
    /// would corrupt the recovered graph. Never last-wins; always loud.
    #[error("transaction {txn}: op has an ambiguous payload (more than one of W/A/R present)")]
    AmbiguousOp {
        /// The transaction the ambiguous op belongs to.
        txn: TxnId,
    },

    /// A transaction received two contradictory lifecycle markers (e.g. a commit
    /// *and* an abort) — its outcome is undefined, so it can never be judged.
    #[error("transaction {txn} has conflicting lifecycle markers ({first:?} then {second:?})")]
    ConflictingLifecycle {
        /// The transaction with contradictory markers.
        txn: TxnId,
        /// The outcome seen first.
        first: TxnOutcome,
        /// The contradictory outcome seen after it.
        second: TxnOutcome,
    },

    /// A read observed a value that was written to a **different key**, so it
    /// cannot join the read's key's version order — a mis-attribution would
    /// silently merge two keys' histories. The history is unrecoverable.
    #[error(
        "value {value} written to key {wrote_key:?} was observed under key {read_key:?} \
         (cross-key attribution)"
    )]
    MisattributedValue {
        /// The mis-attributed value.
        value: Elem,
        /// The key the value was actually written to.
        wrote_key: Key,
        /// The key it was (wrongly) observed under.
        read_key: Key,
    },

    /// An operation was observed **at or after** its transaction's commit/abort
    /// marker, so it is post-termination activity — a completed transaction
    /// cannot issue more ops. Accepting it would silently mutate the graph.
    #[error(
        "transaction {txn}: op at moment {op_at} is not before its terminal marker at {marker_at}"
    )]
    OpAfterTermination {
        /// The transaction the stray op belongs to.
        txn: TxnId,
        /// The op's moment.
        op_at: u64,
        /// The commit/abort marker's moment.
        marker_at: u64,
    },

    /// A **committed append** value never appeared in any read of its key, so the
    /// key's recovered version order is incomplete (a final read at quiesce is
    /// missing). Proceeding would drop the missing write's dependency edges and
    /// could judge a real anomaly **clean** — so this is a fail-loud unrecoverable
    /// history, never a silent partial order.
    #[error(
        "key {key:?}: committed append value {value} is never observed by a read \
         (missing final read — version order incomplete)"
    )]
    UnobservedAppend {
        /// The key whose version order is incomplete.
        key: Key,
        /// The committed append value that no read observed.
        value: Elem,
    },

    /// A key was targeted by **both** a register write and a list append. The two
    /// version-order models are incompatible (append order comes from observed
    /// lists, register order from write moments), so mixing them on one key is
    /// unrecoverable — never a silent classification that drops one model's
    /// writes from the order.
    #[error("key {key:?} mixes register writes and list appends (incompatible models)")]
    MixedModel {
        /// The key with both a `Write` and an `Append`.
        key: Key,
    },

    /// A single read observed the **same value twice** in its list. Written
    /// values are unique by construction, so a repeat is a malformed observation
    /// — accepting it as a version order would fabricate spurious ww edges (and a
    /// false dirty-write verdict). Fail loud instead.
    #[error(
        "key {key:?}: read observed value {value} more than once in one list (unique values repeat)"
    )]
    RepeatedObservation {
        /// The key whose read repeats a value.
        key: Key,
        /// The value observed more than once.
        value: Elem,
    },

    /// One transaction id carried operations from **two different sessions** — a
    /// reused id. Merging them into one [`Transaction`](crate::Transaction) would
    /// collapse two distinct transactions into one graph node and hide anomalies,
    /// so a session mismatch on an already-seen id is unrecoverable.
    #[error("transaction id {txn} reused across sessions {first_session} and {second_session}")]
    ReusedTxnId {
        /// The reused transaction id.
        txn: TxnId,
        /// The first session seen for the id.
        first_session: u64,
        /// The conflicting session seen after it.
        second_session: u64,
    },

    /// A **register** (non-append) key's read observed more than one value. Under
    /// the op model a register read is a singleton (the current value) or empty
    /// (unwritten) — a multi-value observation is malformed. Never silently fall
    /// through order recovery (which would judge it clean).
    #[error(
        "key {key:?}: register read observed {count} values (a register read is singleton/empty)"
    )]
    MultiValueRegisterRead {
        /// The register key whose read observed multiple values.
        key: Key,
        /// How many values the read observed.
        count: usize,
    },

    /// A **register** key had two or more committed writes but **no quiesce read**
    /// (no committed read of the key after all its writes) to pin the final
    /// version. Register writes overwrite, so with no final read the version order
    /// is unrecoverable — ordering the writes by value would fabricate an order
    /// the workload never witnessed (and a fabricated public `version_order`).
    /// This is the register twin of [`UnobservedAppend`](Self::UnobservedAppend):
    /// a real workload ends with a final read of every key. Fail loud.
    #[error(
        "key {key:?}: {writes} committed register writes but no quiesce read to pin the order \
         (version order unrecoverable — missing final read)"
    )]
    UnpinnedRegister {
        /// The register key whose version order cannot be pinned.
        key: Key,
        /// How many committed writes the key received.
        writes: usize,
    },

    /// A committed **quiesce** read of a register key (a read after every
    /// committed writer of the key has committed) observed the **empty/initial**
    /// version, even though the key HAS a committed write — the final read
    /// contradicts that write (a committed value cannot be unwritten at quiesce).
    /// Silently dropping the observation would let the graph settle on the write
    /// and judge the lost write clean, so this is a fail-loud unrecoverable
    /// history.
    #[error(
        "key {key:?}: a committed final read observed the empty/initial version after all \
         committed writers committed (contradicts a committed write)"
    )]
    EmptyFinalRead {
        /// The register key whose final read contradicts its committed writes.
        key: Key,
    },
}

impl DecodeError {
    /// A **stable per-variant tag** — the fingerprint detail for the
    /// distinguished decode-failure [`Bug`](explorer::Bug)
    /// [`ElleOracle::judge`](crate::ElleOracle) mints (so decode failures dedup by
    /// kind and stay disjoint from the consistency-anomaly classes). Stable across
    /// releases; a rename would re-key existing decode-failure fingerprints.
    pub fn kind_tag(&self) -> &'static str {
        match self {
            DecodeError::Malformed(_) => "malformed",
            DecodeError::DuplicateValue { .. } => "duplicate-value",
            DecodeError::UnknownValue { .. } => "unknown-value",
            DecodeError::InconsistentOrder { .. } => "inconsistent-order",
            DecodeError::UnterminatedTxn(_) => "unterminated-txn",
            DecodeError::AmbiguousOp { .. } => "ambiguous-op",
            DecodeError::ConflictingLifecycle { .. } => "conflicting-lifecycle",
            DecodeError::MisattributedValue { .. } => "misattributed-value",
            DecodeError::OpAfterTermination { .. } => "op-after-termination",
            DecodeError::UnobservedAppend { .. } => "unobserved-append",
            DecodeError::MixedModel { .. } => "mixed-model",
            DecodeError::RepeatedObservation { .. } => "repeated-observation",
            DecodeError::ReusedTxnId { .. } => "reused-txn-id",
            DecodeError::MultiValueRegisterRead { .. } => "multi-value-register-read",
            DecodeError::UnpinnedRegister { .. } => "unpinned-register",
            DecodeError::EmptyFinalRead { .. } => "empty-final-read",
        }
    }
}
