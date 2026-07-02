// SPDX-License-Identifier: AGPL-3.0-or-later
//! The role router: the channel/context seams and the generic
//! [`MatchSensor`] / [`MatchOracle`] that evaluate a [`SignalSet`] over a
//! finished [`RunTrace`] and route every match by its **one** declared role.
//!
//! ## Router totality
//!
//! Every matched `(signal, record)` pair routes to exactly its declared role's
//! consumer, and to no other:
//!
//! | role | consumer | output |
//! |---|---|---|
//! | `sometimes` | [`MatchSensor`] | a `Feature` on the signal's channel, id [`FIRED`](Self::FIRED_FEATURE) constant, at the match `Moment` |
//! | `cell` | [`MatchSensor`] | a `Feature` on the signal's channel, id = truncated `sha2` of the matched value's canonical bytes |
//! | `state_max` | [`MatchSensor`] | a `Feature` whose id is the log2 bucket of the running max, emitted each `Moment` the bucket increases |
//! | `never` | [`MatchOracle`] | `Some(Bug)` with `fingerprint = sha2(name ‖ kind ‖ matched attr bytes)` |
//!
//! A signal's **channel** is the rank of its name in the sorted name set — a
//! stable, per-signal identity that is invariant under permuting the
//! declaration order (task-66 semantics 4), so no signal's output depends on
//! where another signal sits in the list. `never` signals occupy a rank too but
//! emit no `Feature`, so a feature never lands on a `never` signal's channel:
//! cross-role leakage is impossible by construction.
//!
//! ## Purity + determinism
//!
//! Both structs are pure functions of the `RunTrace`: records are processed in
//! a canonical `(Moment, index)` order, output is sorted, and every derived id
//! / fingerprint is a `sha2` digest of canonical bytes — no floats, no
//! `HashMap` iteration, seedless. Evaluating the same trace twice yields
//! byte-identical output.

use std::collections::{BTreeMap, BTreeSet};

use explorer::{Bug, ChannelId, Feature, FeatureId, Matchable, Moment, Oracle, RunTrace, Sensor};
use sha2::{Digest, Sha256};

use crate::signal::{During, MatchExpr, Role, SignalId, SignalSet};
use crate::{glob, value};

/// The channel seam (conventions rule 2 — defined here, in the consumer): a
/// channel plugin (tasks 67 / 73 / 74) pulls its record type out of a
/// [`RunTrace`]. Records are returned **owned** (not borrowed from the trace)
/// so a plugin can serve reassembled records absent from the trace verbatim
/// (task 74's OTLP spans). This crate ships test stubs only ([`crate::stub`]).
pub trait ChannelSource {
    /// The record type this channel yields; adapted to the DSL via
    /// [`Matchable`].
    type Rec: Matchable;

    /// Pull this channel's records out of a finished run.
    fn records(&self, t: &RunTrace) -> Vec<Self::Rec>;
}

/// The fault-`Moment` index seam for the `during:` predicate. The
/// [`Environment`](explorer::Environment) is an opaque blob, so the production
/// impl (schema-aware, via the `environment` codec) is campaign assembly (task
/// 69); this crate ships a test stub ([`crate::stub`]).
pub trait ContextSource {
    /// The moments at which a fault was injected in this run, in any order.
    fn fault_moments(&self, t: &RunTrace) -> Vec<Moment>;
}

/// The number of significant bits of `v` — its log2 bucket (`0` for `0`, `1`
/// for `1`, `2` for `2..=3`, `3` for `4..=7`, …). The `state_max` register's
/// IJON-style magnitude bucket.
fn bucket(v: u64) -> u64 {
    64 - u64::from(v.leading_zeros())
}

/// The canonical `(Moment, original-index)` processing order — deterministic
/// and independent of the source's emission order, so `state_max`'s fold is a
/// true timeline max and the output stream is stable.
fn canonical_order<R: Matchable>(recs: &[R]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..recs.len()).collect();
    order.sort_by_key(|&i| (recs[i].moment(), i));
    order
}

/// Whether `rec` matches `expr`: exact `kind`, every `attr` glob satisfied by
/// the named (present) attribute, and the `during` context predicate (if any).
/// `attr_max` does **not** gate the match — it is a `state_max` extraction, and
/// a bad value there is a decode miss, not a match failure.
fn record_matches<R: Matchable>(rec: &R, expr: &MatchExpr, earliest_fault: Option<Moment>) -> bool {
    if rec.kind() != expr.kind {
        return false;
    }
    for (key, pattern) in &expr.attr {
        match rec.attr(key) {
            Some(v) => {
                if !glob::matches(pattern.as_bytes(), &value::glob_bytes(&v)) {
                    return false;
                }
            }
            None => return false,
        }
    }
    match expr.during {
        // `no_faults`: no fault at or before this record's moment.
        Some(During::NoFaults) => earliest_fault.is_none_or(|ef| ef > rec.moment()),
        None => true,
    }
}

/// The `cell` role's stable `FeatureId`: the low 64 bits of
/// `sha2(kind ‖ matched attr bytes)`. Stable across runs with no codebook; a
/// collision merely merges two cells (safe, per the task-66 ruling).
fn cell_id<R: Matchable>(rec: &R, expr: &MatchExpr) -> FeatureId {
    let mut h = Sha256::new();
    h.update(b"dissonance.matcher.cell.v1");
    let kind = rec.kind().as_bytes();
    h.update((kind.len() as u64).to_le_bytes());
    h.update(kind);
    for key in expr.attr.keys() {
        h.update((key.len() as u64).to_le_bytes());
        h.update(key.as_bytes());
        if let Some(v) = rec.attr(key) {
            h.update(value::canonical(&v));
        }
    }
    let digest = h.finalize();
    let mut low = [0u8; 8];
    low.copy_from_slice(&digest[..8]);
    FeatureId(u64::from_le_bytes(low))
}

/// The `never` role's provisional [`Bug`] fingerprint:
/// `sha2(name ‖ kind ‖ matched attr bytes)`. Deterministic and stable across
/// re-derivation (stable coordinates, never learned cells). **Provisional** —
/// task 75 pins the authoritative stable-coordinate schema and supersedes this
/// minting site.
fn never_fingerprint<R: Matchable>(name: &SignalId, rec: &R, expr: &MatchExpr) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"dissonance.matcher.never.v1");
    h.update((name.0.len() as u64).to_le_bytes());
    h.update(name.0.as_bytes());
    let kind = rec.kind().as_bytes();
    h.update((kind.len() as u64).to_le_bytes());
    h.update(kind);
    for key in expr.attr.keys() {
        h.update((key.len() as u64).to_le_bytes());
        h.update(key.as_bytes());
        if let Some(v) = rec.attr(key) {
            h.update(value::canonical(&v));
        }
    }
    h.finalize().into()
}

/// Assign each signal a stable channel: the rank of its name in the sorted set
/// of all names. Permutation-invariant (task-66 semantics 4) and collision-free
/// (names are unique). `SignalSet` construction rejects a set larger than
/// `u16::MAX + 1` ([`MatchError::TooManySignals`](crate::MatchError::TooManySignals)),
/// so every rank fits in `u16` and `rank as u16` never truncates.
fn channels_of(signals: &SignalSet) -> BTreeMap<SignalId, ChannelId> {
    let names: BTreeSet<SignalId> = signals.signals().iter().map(|d| d.name.clone()).collect();
    names
        .into_iter()
        .enumerate()
        .map(|(rank, name)| (name, ChannelId(rank as u16)))
        .collect()
}

/// The routed [`Sensor`] for the `sometimes` / `cell` / `state_max` roles —
/// every match routes to a [`Feature`]; `never` signals are inert here (they
/// route to [`MatchOracle`]).
pub struct MatchSensor<S: ChannelSource, C: ContextSource> {
    signals: SignalSet,
    source: S,
    context: C,
    channels: BTreeMap<SignalId, ChannelId>,
}

impl<S: ChannelSource, C: ContextSource> MatchSensor<S, C> {
    /// The `sometimes` role's fixed feature id: "this signal fired". The signal
    /// identity is carried by the channel, so the id is a constant marker.
    pub const FIRED_FEATURE: FeatureId = FeatureId(0);

    /// Build a sensor over a signal set, a channel source, and a context source.
    pub fn new(signals: SignalSet, source: S, context: C) -> Self {
        let channels = channels_of(&signals);
        Self {
            signals,
            source,
            context,
            channels,
        }
    }

    /// The channel a signal's features are filed under, if the signal is in the
    /// set. (Every signal gets a channel; only non-`never` ones emit features.)
    pub fn channel_of(&self, name: &SignalId) -> Option<ChannelId> {
        self.channels.get(name).copied()
    }

    /// The routed feature stream, sorted by `(Moment, channel, id)` — a
    /// deterministic, permutation-invariant function of the trace. This is the
    /// [`Sensor::observe`] body, exposed inherently for direct use.
    pub fn features(&self, t: &RunTrace) -> Vec<(Moment, Feature)> {
        let recs = self.source.records(t);
        let earliest_fault = self.context.fault_moments(t).into_iter().min();
        let order = canonical_order(&recs);
        let mut out: Vec<(Moment, Feature)> = Vec::new();

        for decl in self.signals.signals() {
            let Some(&channel) = self.channels.get(&decl.name) else {
                continue;
            };
            match decl.role {
                Role::Sometimes => {
                    for &i in &order {
                        if record_matches(&recs[i], &decl.expr, earliest_fault) {
                            out.push((
                                recs[i].moment(),
                                Feature {
                                    channel,
                                    id: Self::FIRED_FEATURE,
                                },
                            ));
                        }
                    }
                }
                Role::Cell => {
                    for &i in &order {
                        if record_matches(&recs[i], &decl.expr, earliest_fault) {
                            out.push((
                                recs[i].moment(),
                                Feature {
                                    channel,
                                    id: cell_id(&recs[i], &decl.expr),
                                },
                            ));
                        }
                    }
                }
                Role::StateMax => {
                    let mut running_max: Option<u64> = None;
                    let mut last_bucket = 0u64;
                    for &i in &order {
                        if !record_matches(&recs[i], &decl.expr, earliest_fault) {
                            continue;
                        }
                        let Some(attr) = decl.expr.attr_max.as_deref() else {
                            continue;
                        };
                        // A non-integer / absent value is a counted decode miss
                        // (see `decode_misses`), never a panic and never a
                        // feature.
                        if let Some(v) = recs[i].attr(attr).as_ref().and_then(value::as_u64) {
                            let m = running_max.map_or(v, |cur| cur.max(v));
                            running_max = Some(m);
                            let b = bucket(m);
                            if b > last_bucket {
                                last_bucket = b;
                                out.push((
                                    recs[i].moment(),
                                    Feature {
                                        channel,
                                        id: FeatureId(b),
                                    },
                                ));
                            }
                        }
                    }
                }
                // Routed to `MatchOracle`; emits no feature here (no leakage).
                Role::Never => {}
            }
        }

        out.sort();
        out
    }

    /// The set of non-`never` signals that matched at least one record — the
    /// catalog fired-marks for the feature-producing roles.
    pub fn fired(&self, t: &RunTrace) -> BTreeSet<SignalId> {
        let recs = self.source.records(t);
        let earliest_fault = self.context.fault_moments(t).into_iter().min();
        let mut fired = BTreeSet::new();
        for decl in self.signals.signals() {
            if decl.role == Role::Never {
                continue;
            }
            if recs
                .iter()
                .any(|r| record_matches(r, &decl.expr, earliest_fault))
            {
                fired.insert(decl.name.clone());
            }
        }
        fired
    }

    /// How many `state_max` extractions hit a non-integer / absent `attr_max`
    /// value on an otherwise-matching record. A quality counter — a high count
    /// means a misdeclared register, but it is never a panic or a match failure.
    pub fn decode_misses(&self, t: &RunTrace) -> u64 {
        let recs = self.source.records(t);
        let earliest_fault = self.context.fault_moments(t).into_iter().min();
        let mut misses = 0u64;
        for decl in self.signals.signals() {
            if decl.role != Role::StateMax {
                continue;
            }
            let Some(attr) = decl.expr.attr_max.as_deref() else {
                continue;
            };
            for r in &recs {
                if record_matches(r, &decl.expr, earliest_fault)
                    && r.attr(attr).as_ref().and_then(value::as_u64).is_none()
                {
                    misses += 1;
                }
            }
        }
        misses
    }
}

impl<S: ChannelSource, C: ContextSource> Sensor for MatchSensor<S, C> {
    fn observe(&self, t: &RunTrace) -> Vec<(Moment, Feature)> {
        self.features(t)
    }
}

/// The routed [`Oracle`] for the `never` role — a match is a bug. Non-`never`
/// signals are inert here (they route to [`MatchSensor`]).
pub struct MatchOracle<S: ChannelSource, C: ContextSource> {
    signals: SignalSet,
    source: S,
    context: C,
    /// Indices of the `never` declarations, sorted by **name** — a
    /// permutation-invariant iteration order, so the earliest-violation
    /// tie-break (two `never` signals matching the same record) never depends
    /// on declaration order (task-66 semantics 4).
    never_idx: Vec<usize>,
}

impl<S: ChannelSource, C: ContextSource> MatchOracle<S, C> {
    /// Build an oracle over a signal set, a channel source, and a context source.
    pub fn new(signals: SignalSet, source: S, context: C) -> Self {
        let mut never_idx: Vec<usize> = signals
            .signals()
            .iter()
            .enumerate()
            .filter(|(_, d)| d.role == Role::Never)
            .map(|(i, _)| i)
            .collect();
        never_idx.sort_by(|&a, &b| signals.signals()[a].name.cmp(&signals.signals()[b].name));
        Self {
            signals,
            source,
            context,
            never_idx,
        }
    }

    /// Every `never`-rule violation in the run, one [`Bug`] per matching
    /// `(signal, record)` pair, in canonical `(Moment, record-index, name)`
    /// order. [`judge`](Oracle::judge) returns the first of these.
    pub fn verdicts(&self, t: &RunTrace) -> Vec<Bug> {
        let recs = self.source.records(t);
        let earliest_fault = self.context.fault_moments(t).into_iter().min();
        let order = canonical_order(&recs);
        let mut out = Vec::new();
        for &i in &order {
            for &di in &self.never_idx {
                let decl = &self.signals.signals()[di];
                if record_matches(&recs[i], &decl.expr, earliest_fault) {
                    out.push(Bug {
                        env: t.env.clone(),
                        stop: t.terminal.clone(),
                        fingerprint: never_fingerprint(&decl.name, &recs[i], &decl.expr),
                    });
                }
            }
        }
        out
    }

    /// The set of `never` signals that matched at least one record — the
    /// catalog fired-marks for the oracle role (each is also a bug).
    pub fn fired(&self, t: &RunTrace) -> BTreeSet<SignalId> {
        let recs = self.source.records(t);
        let earliest_fault = self.context.fault_moments(t).into_iter().min();
        let mut fired = BTreeSet::new();
        for decl in self.signals.signals() {
            if decl.role != Role::Never {
                continue;
            }
            if recs
                .iter()
                .any(|r| record_matches(r, &decl.expr, earliest_fault))
            {
                fired.insert(decl.name.clone());
            }
        }
        fired
    }
}

impl<S: ChannelSource, C: ContextSource> Oracle for MatchOracle<S, C> {
    fn judge(&self, t: &RunTrace) -> Option<Bug> {
        // The earliest `never`-violation; re-judging after a fix finds the next
        // (the offline replay-plane property). Walk canonical order and stop —
        // `never_idx` is name-sorted so the tie-break is permutation-invariant.
        let recs = self.source.records(t);
        let earliest_fault = self.context.fault_moments(t).into_iter().min();
        let order = canonical_order(&recs);
        for &i in &order {
            for &di in &self.never_idx {
                let decl = &self.signals.signals()[di];
                if record_matches(&recs[i], &decl.expr, earliest_fault) {
                    return Some(Bug {
                        env: t.env.clone(),
                        stop: t.terminal.clone(),
                        fingerprint: never_fingerprint(&decl.name, &recs[i], &decl.expr),
                    });
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub::{FaultMoments, OwnedRecords, RecordRec};
    use explorer::{Environment, Record, StopReason, VTime, Value};

    fn trace() -> RunTrace {
        RunTrace {
            terminal: StopReason::Quiescent { vtime: VTime(9) },
            env: Environment {
                blob_version: 1,
                bytes: vec![1, 2, 3],
            },
            coverage: None,
            events: vec![],
            records: vec![],
        }
    }

    fn rec(moment: u64, kind: &str, attrs: &[(&str, Value)]) -> RecordRec {
        RecordRec {
            moment: Moment(moment),
            record: Record {
                kind: kind.into(),
                attrs: attrs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect(),
            },
        }
    }

    #[test]
    fn bucket_is_bit_length() {
        assert_eq!(bucket(0), 0);
        assert_eq!(bucket(1), 1);
        assert_eq!(bucket(2), 2);
        assert_eq!(bucket(3), 2);
        assert_eq!(bucket(4), 3);
        assert_eq!(bucket(7), 3);
        assert_eq!(bucket(8), 4);
    }

    #[test]
    fn state_max_emits_on_bucket_increase_only() {
        let signals = SignalSet::from_json(
            r#"{ "signals": [ { "name": "lsn", "role": "state_max",
                 "match": { "kind": "span", "attr": { "name": "wal" }, "attr_max": "lsn" } } ] }"#,
        )
        .unwrap();
        // lsn values 1, 3, 2, 8, 5 → running max 1,3,3,8,8 → buckets 1,2,2,4,4.
        // Emissions at the moments the bucket first reaches 1, 2, 4.
        let recs = vec![
            rec(
                1,
                "span",
                &[("name", Value::Str("wal".into())), ("lsn", Value::UInt(1))],
            ),
            rec(
                2,
                "span",
                &[("name", Value::Str("wal".into())), ("lsn", Value::UInt(3))],
            ),
            rec(
                3,
                "span",
                &[("name", Value::Str("wal".into())), ("lsn", Value::UInt(2))],
            ),
            rec(
                4,
                "span",
                &[("name", Value::Str("wal".into())), ("lsn", Value::UInt(8))],
            ),
            rec(
                5,
                "span",
                &[("name", Value::Str("wal".into())), ("lsn", Value::UInt(5))],
            ),
        ];
        let sensor = MatchSensor::new(signals, OwnedRecords(recs), FaultMoments(vec![]));
        let feats = sensor.features(&trace());
        let ids: Vec<(u64, u64)> = feats.iter().map(|(m, f)| (m.0, f.id.0)).collect();
        assert_eq!(ids, vec![(1, 1), (2, 2), (4, 4)]);
    }

    #[test]
    fn state_max_non_integer_is_a_counted_decode_miss() {
        let signals = SignalSet::from_json(
            r#"{ "signals": [ { "name": "lsn", "role": "state_max",
                 "match": { "kind": "span", "attr_max": "lsn" } } ] }"#,
        )
        .unwrap();
        let recs = vec![
            rec(1, "span", &[("lsn", Value::Str("not-a-number".into()))]),
            rec(2, "span", &[]), // absent attr_max
            rec(3, "span", &[("lsn", Value::UInt(4))]),
        ];
        let sensor = MatchSensor::new(signals, OwnedRecords(recs), FaultMoments(vec![]));
        // No panic; two misses; one feature (bucket 3 at moment 3).
        assert_eq!(sensor.decode_misses(&trace()), 2);
        let feats = sensor.features(&trace());
        assert_eq!(feats.len(), 1);
        assert_eq!((feats[0].0.0, feats[0].1.id.0), (3, 3));
    }

    #[test]
    fn during_no_faults_gates_on_the_fault_index() {
        let signals = SignalSet::from_json(
            r#"{ "signals": [ { "name": "clean", "role": "never",
                 "match": { "kind": "span", "attr": { "op": "commit" }, "during": "no_faults" } } ] }"#,
        )
        .unwrap();
        let recs = vec![
            rec(5, "span", &[("op", Value::Str("commit".into()))]),
            rec(15, "span", &[("op", Value::Str("commit".into()))]),
        ];
        // A fault at moment 10: the record at 5 is "no faults ≤ 5" (match), the
        // record at 15 has a fault at 10 ≤ 15 (no match).
        let oracle = MatchOracle::new(signals, OwnedRecords(recs), FaultMoments(vec![Moment(10)]));
        let verdicts = oracle.verdicts(&trace());
        assert_eq!(verdicts.len(), 1, "only the pre-fault commit violates");
        // judge returns the earliest.
        assert!(oracle.judge(&trace()).is_some());
    }

    #[test]
    fn cross_role_channels_are_disjoint() {
        let signals = SignalSet::from_json(
            r#"{ "signals": [
                { "name": "a.some", "role": "sometimes", "match": { "kind": "log", "attr": { "x": "1" } } },
                { "name": "b.never", "role": "never", "match": { "kind": "log", "attr": { "x": "1" } } }
            ] }"#,
        )
        .unwrap();
        let recs = vec![rec(1, "log", &[("x", Value::Str("1".into()))])];
        let sensor = MatchSensor::new(
            signals.clone(),
            OwnedRecords(recs.clone()),
            FaultMoments(vec![]),
        );
        let never_channel = sensor.channel_of(&SignalId("b.never".into())).unwrap();
        // The never signal's channel never carries a feature.
        assert!(
            sensor
                .features(&trace())
                .iter()
                .all(|(_, f)| f.channel != never_channel),
            "never role must not leak into the sensor's feature stream"
        );
        // And the never signal is not in the sensor's fired set.
        assert!(!sensor.fired(&trace()).contains(&SignalId("b.never".into())));
    }
}
