// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **`OpDecode` seam** — how a recorded [`RunTrace`] becomes a [`History`].
//!
//! The seam is defined **locally** (conventions rule 2): the workload's op
//! encoding is oracle-elle's own convention, and other channels (OTel spans per
//! task 74, SDK events per task 73, scrape logs per task 65) supply their own
//! [`OpDecode`] implementations. Two ship here, both **fail-loud** on an
//! unrecoverable/malformed history (never a guess):
//!
//! - [`RecordDecoder`] over `RunTrace.records` — the scrape-tier line format
//!   `elle op s=<session> t=<txn> k=<key> <W|A|R>=<value|list>` (plus
//!   `elle commit t=<txn>` / `elle abort t=<txn>`); non-`elle` lines are
//!   ignored, so op records ride alongside ordinary logs.
//! - [`EventDecoder`] over `RunTrace.events` — the same fields as link-tier
//!   [`GuestEvent`] attributes (`kind = "op" | "commit" | "abort"`).
//!
//! Both stamp each op with the [`Moment`] of its record/event, so program order
//! within a transaction is the recorded time order.

use std::collections::BTreeMap;

use explorer::{GuestEvent, Moment, RunTrace, Value};

use crate::error::DecodeError;
use crate::op::{Elem, History, Op, OpKind, Session, Transaction, TxnId, TxnOutcome};

/// Decodes a recorded [`RunTrace`] into the [`History`] the checker judges.
/// Implementations are channel plugins; the seam lives in the consumer.
pub trait OpDecode {
    /// Recover the operation history, or fail loud if it is unrecoverable.
    fn decode(&self, t: &RunTrace) -> Result<History, DecodeError>;
}

/// The verb an op-source record carries.
enum Marker {
    Op(Op),
    Commit(TxnId),
    Abort(TxnId),
}

/// Accumulates decoded markers into a [`History`], enforcing that every
/// transaction with operations is terminated by a commit/abort.
#[derive(Default)]
struct Builder {
    ops: BTreeMap<TxnId, Vec<Op>>,
    sessions: BTreeMap<TxnId, Session>,
    outcomes: BTreeMap<TxnId, (TxnOutcome, Moment)>,
}

impl Builder {
    fn push(&mut self, m: Marker, at: Moment) {
        match m {
            Marker::Op(op) => {
                self.sessions.entry(op.txn).or_insert(op.session);
                self.ops.entry(op.txn).or_default().push(op);
            }
            Marker::Commit(t) => {
                self.outcomes.insert(t, (TxnOutcome::Committed, at));
            }
            Marker::Abort(t) => {
                self.outcomes.insert(t, (TxnOutcome::Aborted, at));
            }
        }
    }

    fn finish(self) -> Result<History, DecodeError> {
        let mut txns: BTreeMap<TxnId, Transaction> = BTreeMap::new();
        // Every transaction that issued ops must have a terminal marker.
        for (&id, ops) in &self.ops {
            let Some(&(outcome, at)) = self.outcomes.get(&id) else {
                return Err(DecodeError::UnterminatedTxn(id));
            };
            let mut ops = ops.clone();
            ops.sort_by(|a, b| a.at.cmp(&b.at).then(a.kind.cmp(&b.kind)));
            txns.insert(
                id,
                Transaction {
                    id,
                    session: self.sessions.get(&id).copied().unwrap_or(0),
                    ops,
                    outcome,
                    at,
                },
            );
        }
        // A commit/abort for a transaction that issued no ops is an empty txn
        // (its outcome is known, its op list empty) — kept so an aborted no-op
        // is still a fact of the history.
        for (&id, &(outcome, at)) in &self.outcomes {
            txns.entry(id).or_insert_with(|| Transaction {
                id,
                session: self.sessions.get(&id).copied().unwrap_or(0),
                ops: Vec::new(),
                outcome,
                at,
            });
        }
        Ok(History { txns })
    }
}

/// Parse a comma-separated observed list (`"1,2,3"`, or empty for an unwritten
/// key) into elements.
fn parse_list(s: &str) -> Result<Vec<Elem>, DecodeError> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    s.split(',')
        .map(|tok| {
            tok.trim()
                .parse::<Elem>()
                .map_err(|_| DecodeError::Malformed(format!("bad element {tok:?} in list {s:?}")))
        })
        .collect()
}

/// Assemble one op from its recovered fields. `payload` is the write value, the
/// append value, or the observed list, tagged by `verb` (`"W" | "A" | "R"`).
fn assemble_op(
    session: Session,
    txn: TxnId,
    key: Vec<u8>,
    verb: &str,
    payload: &str,
    at: Moment,
) -> Result<Op, DecodeError> {
    let kind = match verb {
        "W" => OpKind::Write(
            payload
                .trim()
                .parse::<Elem>()
                .map_err(|_| DecodeError::Malformed(format!("bad write value {payload:?}")))?,
        ),
        "A" => OpKind::Append(
            payload
                .trim()
                .parse::<Elem>()
                .map_err(|_| DecodeError::Malformed(format!("bad append value {payload:?}")))?,
        ),
        "R" => OpKind::Read(parse_list(payload)?),
        other => {
            return Err(DecodeError::Malformed(format!(
                "unknown op verb {other:?} (want W/A/R)"
            )));
        }
    };
    Ok(Op {
        session,
        txn,
        kind,
        key,
        at,
    })
}

// ---------------------------------------------------------------------------
// RecordDecoder — the scrape-tier line format
// ---------------------------------------------------------------------------

/// Decodes ops from `RunTrace.records`: `elle`-tagged, whitespace-separated
/// `key=value` lines. Non-`elle` lines are ignored so op records coexist with
/// ordinary log output.
#[derive(Clone, Debug, Default)]
pub struct RecordDecoder;

impl RecordDecoder {
    /// A record decoder (stateless).
    pub fn new() -> Self {
        Self
    }

    /// Parse one `elle ...` line into a marker (`None` for a non-`elle` line).
    fn parse_line(line: &str) -> Result<Option<Marker>, DecodeError> {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("elle ") else {
            return Ok(None);
        };
        let mut fields = rest.split_whitespace();
        let verb = fields
            .next()
            .ok_or_else(|| DecodeError::Malformed(format!("empty elle line {line:?}")))?;
        // Collect the `k=v` tokens deterministically.
        let mut kv: BTreeMap<&str, &str> = BTreeMap::new();
        for tok in fields {
            let (k, v) = tok.split_once('=').ok_or_else(|| {
                DecodeError::Malformed(format!("token {tok:?} not k=v in {line:?}"))
            })?;
            kv.insert(k, v);
        }
        let get_u64 = |k: &str| -> Result<u64, DecodeError> {
            kv.get(k)
                .ok_or_else(|| DecodeError::Malformed(format!("missing {k}= in {line:?}")))?
                .parse::<u64>()
                .map_err(|_| DecodeError::Malformed(format!("bad {k}= in {line:?}")))
        };
        match verb {
            "commit" => Ok(Some(Marker::Commit(get_u64("t")?))),
            "abort" => Ok(Some(Marker::Abort(get_u64("t")?))),
            "op" => {
                let session = get_u64("s")?;
                let txn = get_u64("t")?;
                let key = kv
                    .get("k")
                    .ok_or_else(|| DecodeError::Malformed(format!("missing k= in {line:?}")))?
                    .as_bytes()
                    .to_vec();
                // Exactly one of W/A/R is the op payload.
                let payload = ["W", "A", "R"]
                    .into_iter()
                    .find_map(|verb| kv.get(verb).map(|v| (verb, *v)));
                let (verb, payload) = payload.ok_or_else(|| {
                    DecodeError::Malformed(format!("op line has no W=/A=/R= payload: {line:?}"))
                })?;
                Ok(Some(Marker::Op(assemble_op(
                    session,
                    txn,
                    key,
                    verb,
                    payload,
                    Moment(0), // stamped by the caller with the record's Moment
                )?)))
            }
            other => Err(DecodeError::Malformed(format!(
                "unknown elle verb {other:?} in {line:?}"
            ))),
        }
    }
}

impl OpDecode for RecordDecoder {
    fn decode(&self, t: &RunTrace) -> Result<History, DecodeError> {
        let mut b = Builder::default();
        for (at, rec) in &t.records {
            let line = String::from_utf8_lossy(&rec.line);
            if let Some(marker) = RecordDecoder::parse_line(&line)? {
                // Stamp the op with the record's Moment (the parse used a
                // placeholder).
                let marker = match marker {
                    Marker::Op(mut op) => {
                        op.at = *at;
                        Marker::Op(op)
                    }
                    other => other,
                };
                b.push(marker, *at);
            }
        }
        b.finish()
    }
}

// ---------------------------------------------------------------------------
// EventDecoder — the link-tier GuestEvent format
// ---------------------------------------------------------------------------

/// Decodes ops from `RunTrace.events`: link-tier [`GuestEvent`]s whose `kind` is
/// `"op"`, `"commit"`, or `"abort"`, with the same fields as [`RecordDecoder`]
/// carried in the event attributes.
#[derive(Clone, Debug, Default)]
pub struct EventDecoder;

impl EventDecoder {
    /// An event decoder (stateless).
    pub fn new() -> Self {
        Self
    }
}

/// Read an integer attribute (accepting `Int` or `UInt`).
fn attr_u64(ev: &GuestEvent, key: &str) -> Result<u64, DecodeError> {
    match ev.attrs.get(key) {
        Some(Value::UInt(v)) => Ok(*v),
        Some(Value::Int(v)) if *v >= 0 => Ok(*v as u64),
        Some(other) => Err(DecodeError::Malformed(format!(
            "attr {key} is {other:?}, want an unsigned integer"
        ))),
        None => Err(DecodeError::Malformed(format!("missing attr {key}"))),
    }
}

/// Read a string attribute's bytes (accepting `Str` or `Bytes`).
fn attr_bytes(ev: &GuestEvent, key: &str) -> Result<Vec<u8>, DecodeError> {
    match ev.attrs.get(key) {
        Some(Value::Str(s)) => Ok(s.as_bytes().to_vec()),
        Some(Value::Bytes(b)) => Ok(b.clone()),
        Some(other) => Err(DecodeError::Malformed(format!(
            "attr {key} is {other:?}, want a string/bytes"
        ))),
        None => Err(DecodeError::Malformed(format!("missing attr {key}"))),
    }
}

impl OpDecode for EventDecoder {
    fn decode(&self, t: &RunTrace) -> Result<History, DecodeError> {
        let mut b = Builder::default();
        for (at, ev) in &t.events {
            let marker = match ev.kind.as_str() {
                "commit" => Marker::Commit(attr_u64(ev, "t")?),
                "abort" => Marker::Abort(attr_u64(ev, "t")?),
                "op" => {
                    let session = attr_u64(ev, "s")?;
                    let txn = attr_u64(ev, "t")?;
                    let key = attr_bytes(ev, "k")?;
                    let (verb, payload) = ["W", "A", "R"]
                        .into_iter()
                        .find_map(|verb| ev.attrs.get(verb).map(|v| (verb, v)))
                        .ok_or_else(|| {
                            DecodeError::Malformed(format!(
                                "op event has no W/A/R attr (txn {txn})"
                            ))
                        })?;
                    // W/A carry an integer; R carries a comma list as a string.
                    let payload = match (verb, payload) {
                        ("R", Value::Str(s)) => s.clone(),
                        ("W" | "A", Value::Int(v)) => v.to_string(),
                        ("W" | "A", Value::UInt(v)) => v.to_string(),
                        (v, other) => {
                            return Err(DecodeError::Malformed(format!(
                                "op verb {v} has attr {other:?} of the wrong type"
                            )));
                        }
                    };
                    Marker::Op(assemble_op(session, txn, key, verb, &payload, *at)?)
                }
                // Non-op events (assertions, buggify, …) are not this decoder's
                // concern; skip them.
                _ => continue,
            };
            b.push(marker, *at);
        }
        b.finish()
    }
}
