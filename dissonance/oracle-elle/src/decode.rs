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
    fn push(&mut self, m: Marker, at: Moment) -> Result<(), DecodeError> {
        match m {
            Marker::Op(op) => {
                // A transaction belongs to exactly one session. A second session
                // on an already-seen txn id is a **reused id** — never silently
                // merge the two sessions' ops into one node (which would collapse
                // two transactions and hide anomalies); fail loud.
                match self.sessions.get(&op.txn) {
                    Some(&s) if s != op.session => {
                        return Err(DecodeError::ReusedTxnId {
                            txn: op.txn,
                            first_session: s,
                            second_session: op.session,
                        });
                    }
                    Some(_) => {}
                    None => {
                        self.sessions.insert(op.txn, op.session);
                    }
                }
                self.ops.entry(op.txn).or_default().push(op);
            }
            Marker::Commit(t) => self.mark(t, TxnOutcome::Committed, at)?,
            Marker::Abort(t) => self.mark(t, TxnOutcome::Aborted, at)?,
        }
        Ok(())
    }

    /// Record a transaction's terminal outcome, rejecting a **contradictory**
    /// second marker (a commit after an abort, or vice versa) — never last-wins,
    /// which could flip a bug's visibility. An identical repeat marker is
    /// idempotent (harmless); the earliest moment is kept.
    fn mark(&mut self, txn: TxnId, outcome: TxnOutcome, at: Moment) -> Result<(), DecodeError> {
        match self.outcomes.get_mut(&txn) {
            Some((prev, prev_at)) => {
                if *prev != outcome {
                    return Err(DecodeError::ConflictingLifecycle {
                        txn,
                        first: *prev,
                        second: outcome,
                    });
                }
                *prev_at = (*prev_at).min(at);
            }
            None => {
                self.outcomes.insert(txn, (outcome, at));
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<History, DecodeError> {
        let mut txns: BTreeMap<TxnId, Transaction> = BTreeMap::new();
        // Every transaction that issued ops must have a terminal marker, and no
        // op may occur *strictly after* that marker (post-termination activity
        // would silently mutate the graph).
        //
        // **Boundary:** an op AT the marker's exact Moment is **legal** — the
        // commit/abort is recorded at the same V-time tick as the transaction's
        // final op (they are one instant on the deterministic timeline, the op
        // then the commit). Only `op.at > marker` is post-termination. So the
        // check is strict-greater, and an at-Moment op stays in the transaction.
        for (&id, ops) in &self.ops {
            let Some(&(outcome, at)) = self.outcomes.get(&id) else {
                return Err(DecodeError::UnterminatedTxn(id));
            };
            if let Some(stray) = ops.iter().find(|op| op.at > at) {
                return Err(DecodeError::OpAfterTermination {
                    txn: id,
                    op_at: stray.at.0,
                    marker_at: at.0,
                });
            }
            let mut ops = ops.clone();
            // Program order is by Moment, but two ops at the SAME Moment need a
            // **total** content tie-break so the decoded history — and hence the
            // verdict and fingerprint — is a pure function of the trace content,
            // never of record emission order. `Op`'s full `Ord` (kind, key, …) is
            // total: two ops compare Equal only when fully identical, where order
            // is irrelevant. (Comparing only `kind` left same-kind different-key
            // ops order-dependent — the round-5 leak.)
            ops.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.cmp(b)));
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
    ///
    /// Parses at the **byte** level: numeric/verb fields are ASCII, but the key
    /// value is kept **verbatim** (never UTF-8-lossy-decoded — a mangled key
    /// could collide two distinct keys and hide or fabricate an anomaly).
    fn parse_line(line: &[u8]) -> Result<Option<Marker>, DecodeError> {
        let line = line.trim_ascii();
        // The `elle` tag is separated from its fields by **any** ASCII whitespace
        // (the rest of the parser is whitespace-agnostic), so a tab-delimited
        // `elle\t...` record must parse — matching only a literal space silently
        // drops it and the oracle could report a clean empty history.
        let Some(rest) = line.strip_prefix(b"elle") else {
            return Ok(None);
        };
        let show = || String::from_utf8_lossy(line).into_owned();
        match rest.first() {
            // A real record: the tag is followed by whitespace then its fields.
            Some(b) if b.is_ascii_whitespace() => {}
            // A **different** tag (`elleish…`): the byte after `elle` is not a
            // separator, so this line is not ours — skip it.
            Some(_) => return Ok(None),
            // The **bare** `elle` tag with no fields: a record that IS ours but is
            // empty — malformed, not foreign. Fail loud rather than silently skip
            // a tagged-yet-fieldless line (which could hide a truncated stream).
            None => {
                return Err(DecodeError::Malformed(format!(
                    "bare `elle` tag with no fields: {:?}",
                    show()
                )));
            }
        }
        let mut fields = rest
            .split(|b: &u8| b.is_ascii_whitespace())
            .filter(|f| !f.is_empty());
        let verb = fields
            .next()
            .ok_or_else(|| DecodeError::Malformed(format!("empty elle line {:?}", show())))?;
        // Collect the `k=v` tokens deterministically (values kept as raw bytes).
        // Records are the untrusted op source: a duplicate field (`t=1 t=2`)
        // must be a loud error, never a silent last-wins that could re-target an
        // op onto a different txn/key/value.
        let mut kv: BTreeMap<&[u8], &[u8]> = BTreeMap::new();
        for tok in fields {
            let eq = tok.iter().position(|&b| b == b'=').ok_or_else(|| {
                DecodeError::Malformed(format!(
                    "token {:?} not k=v in {:?}",
                    String::from_utf8_lossy(tok),
                    show()
                ))
            })?;
            if kv.insert(&tok[..eq], &tok[eq + 1..]).is_some() {
                return Err(DecodeError::Malformed(format!(
                    "duplicate field {:?}= in {:?}",
                    String::from_utf8_lossy(&tok[..eq]),
                    show()
                )));
            }
        }
        let get_u64 = |k: &[u8]| -> Result<u64, DecodeError> {
            let raw = kv.get(k).ok_or_else(|| {
                DecodeError::Malformed(format!(
                    "missing {}= in {:?}",
                    String::from_utf8_lossy(k),
                    show()
                ))
            })?;
            std::str::from_utf8(raw)
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .ok_or_else(|| {
                    DecodeError::Malformed(format!(
                        "bad {}= in {:?}",
                        String::from_utf8_lossy(k),
                        show()
                    ))
                })
        };
        match verb {
            b"commit" => Ok(Some(Marker::Commit(get_u64(b"t")?))),
            b"abort" => Ok(Some(Marker::Abort(get_u64(b"t")?))),
            b"op" => {
                let session = get_u64(b"s")?;
                let txn = get_u64(b"t")?;
                let key = kv
                    .get(b"k".as_slice())
                    .ok_or_else(|| DecodeError::Malformed(format!("missing k= in {:?}", show())))?
                    .to_vec();
                // Exactly one of W/A/R may be present — more than one is an
                // ambiguous op kind, never last-wins.
                let present: Vec<(&str, &[u8])> = [
                    ("W", b"W".as_slice()),
                    ("A", b"A".as_slice()),
                    ("R", b"R".as_slice()),
                ]
                .into_iter()
                .filter_map(|(verb, k)| kv.get(k).map(|v| (verb, *v)))
                .collect();
                let (verb, payload) = match present.as_slice() {
                    [] => {
                        return Err(DecodeError::Malformed(format!(
                            "op line has no W=/A=/R= payload: {:?}",
                            show()
                        )));
                    }
                    [one] => *one,
                    _ => return Err(DecodeError::AmbiguousOp { txn }),
                };
                let payload = std::str::from_utf8(payload).map_err(|_| {
                    DecodeError::Malformed(format!("non-UTF-8 {verb}= payload in {:?}", show()))
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
                "unknown elle verb {:?} in {:?}",
                String::from_utf8_lossy(other),
                show()
            ))),
        }
    }
}

impl OpDecode for RecordDecoder {
    fn decode(&self, t: &RunTrace) -> Result<History, DecodeError> {
        let mut b = Builder::default();
        for (at, rec) in &t.records {
            if let Some(marker) = RecordDecoder::parse_line(&rec.line)? {
                // Stamp the op with the record's Moment (the parse used a
                // placeholder).
                let marker = match marker {
                    Marker::Op(mut op) => {
                        op.at = *at;
                        Marker::Op(op)
                    }
                    other => other,
                };
                b.push(marker, *at)?;
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
                    // Exactly one of W/A/R may be present — more than one is an
                    // ambiguous op kind, never first-wins.
                    let present: Vec<(&str, &Value)> = ["W", "A", "R"]
                        .into_iter()
                        .filter_map(|verb| ev.attrs.get(verb).map(|v| (verb, v)))
                        .collect();
                    let (verb, payload) = match present.as_slice() {
                        [] => {
                            return Err(DecodeError::Malformed(format!(
                                "op event has no W/A/R attr (txn {txn})"
                            )));
                        }
                        [one] => *one,
                        _ => return Err(DecodeError::AmbiguousOp { txn }),
                    };
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
            b.push(marker, *at)?;
        }
        b.finish()
    }
}
