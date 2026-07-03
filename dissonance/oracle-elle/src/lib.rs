// SPDX-License-Identifier: AGPL-3.0-or-later
//! # oracle-elle — an Elle-shaped transaction-isolation trace oracle
//!
//! `oracle-elle` is a task-64 **plugin** for the search-plane spine: it
//! implements [`explorer::Oracle`] to judge whether a recorded run violated a
//! declared transaction-isolation level. It is an Elle-*shaped* checker (not a
//! full Elle port — cycle-typed SI/serializability anomalies and linearizability
//! are the follow-on ladder): from an operation history decoded off a
//! [`RunTrace`], it recovers the write-read / write-write / read-write
//! dependency graph and reports an **anomaly ladder v1** — G0 dirty write, G1a
//! aborted read, and lost update — each with a constructive witness.
//!
//! It is a **pure trace oracle**: judging is offline and touches no guest, so
//! re-running a fresh `ElleOracle` over a stored corpus finds real bugs with
//! zero VM time (the strong offline property). Recoverability is the workload's
//! job (unique written values, final reads at quiesce); an unrecoverable history
//! is a fail-loud [`DecodeError`], never a guessed anomaly.
//!
//! The crate depends only on `explorer` (conventions rule 2 / the task-75
//! surface note): the [`OpDecode`] seam is defined **here**, and the pinned
//! [`Bug`](explorer::Bug) fingerprint schema is minted through
//! [`explorer::mint_fingerprint`].
//!
//! ## Layout
//!
//! [`op`] (the operation-history model) · [`decode`] (the [`OpDecode`] seam and
//! the record/event decoders) · [`graph`] (the recovered [`DepGraph`]) ·
//! [`anomaly`] (the isolation levels and the ladder) · [`oracle`] (the
//! [`ElleOracle`] itself).

pub mod anomaly;
pub mod decode;
pub mod error;
pub mod graph;
pub mod op;
pub mod oracle;

pub use anomaly::{Anomaly, AnomalyKind, IsolationLevel};
pub use decode::{EventDecoder, OpDecode, RecordDecoder};
pub use error::DecodeError;
pub use graph::DepGraph;
pub use op::{History, Key, Op, OpKind, Session, Transaction, TxnId, TxnOutcome};
pub use oracle::ElleOracle;
