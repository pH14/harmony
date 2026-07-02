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
//! A signal's **channel** is `channel_base + rank`, where `rank` is the
//! position of its name in the sorted name set — a stable, per-signal identity
//! invariant under permuting the declaration order (task-66 semantics 4), so no
//! signal's output depends on where another signal sits in the list. `never`
//! signals occupy a rank too but emit no `Feature`, so a feature never lands on
//! a `never` signal's channel: cross-role leakage is impossible by construction.
//!
//! **Channel allocation is the campaign's, not the router's.** The base is a
//! [`MatchSensor::new`] parameter; by convention channel 0 is coverage's
//! (`explorer::COVERAGE_CHANNEL`), so the base must be `>= 1` — otherwise a
//! matcher `Feature{channel:0, id:1}` would be indistinguishable from a coverage
//! edge and the archive would dedup them together (the round-2 P1 fix). The
//! sensor occupies `[base, base + signal_count)`; a later channel plugin (task
//! 74's OTel spans) bases above that (see [`MatchSensor::next_free_channel`]).
//!
//! ## Purity + determinism
//!
//! Both structs are pure functions of the `RunTrace`'s **content**, never of a
//! channel source's record *emission order*: the sensor stream is sorted by
//! `(Moment, channel, id)`, `state_max` folds through a `BTreeMap` keyed by
//! `Moment` (`max` is order-independent), and the oracle orders its verdicts by
//! `(Moment, fingerprint)` — no `(Moment, index)` tie-break that could leak
//! emission order (the round-3 fix). Every derived id / fingerprint is a `sha2`
//! digest of canonical bytes; no floats, no `HashMap` iteration, seedless.
//! Evaluating the same trace — in any record order — yields byte-identical
//! output.

use std::collections::{BTreeMap, BTreeSet};

use explorer::{Bug, ChannelId, Feature, FeatureId, Matchable, Moment, Oracle, RunTrace, Sensor};
use sha2::{Digest, Sha256};

use crate::error::MatchError;
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

/// Assign each signal a stable channel: `base + rank`, where `rank` is the
/// position of its name in the sorted set of all names. Permutation-invariant
/// (task-66 semantics 4) and collision-free (names are unique). The caller has
/// validated `base >= 1` and `base + count <= u16::MAX`, so `base + rank` never
/// wraps and never lands on channel 0 (coverage's).
fn channels_of(signals: &SignalSet, base: u16) -> BTreeMap<SignalId, ChannelId> {
    let names: BTreeSet<SignalId> = signals.signals().iter().map(|d| d.name.clone()).collect();
    names
        .into_iter()
        .enumerate()
        .map(|(rank, name)| (name, ChannelId(base + rank as u16)))
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
    base: ChannelId,
}

impl<S: ChannelSource, C: ContextSource> MatchSensor<S, C> {
    /// The `sometimes` role's fixed feature id: "this signal fired". The signal
    /// identity is carried by the channel, so the id is a constant marker.
    pub const FIRED_FEATURE: FeatureId = FeatureId(0);

    /// Build a sensor over a signal set, a channel source, a context source, and
    /// the campaign's **channel base**.
    ///
    /// Channel allocation is the campaign's to decide, not the router's: the
    /// matcher's signals occupy `[base, base + signal_count)` in the spine
    /// `Feature` channel space. By convention **channel 0 is coverage's**
    /// (`explorer::COVERAGE_CHANNEL`), so `base` must be `>= 1` — otherwise a
    /// matcher feature would be indistinguishable from a coverage edge and the
    /// archive would dedup them together. A later channel plugin (task 74's OTel
    /// spans) allocates its own base above this range (see
    /// [`next_free_channel`](Self::next_free_channel)).
    ///
    /// Returns [`MatchError::ReservedChannelBase`] if `base == 0`, or
    /// [`MatchError::ChannelSpaceExhausted`] if `base + signal_count` overflows
    /// `u16::MAX` (folding the channel-capacity guard into base validation).
    pub fn new(
        signals: SignalSet,
        source: S,
        context: C,
        channel_base: ChannelId,
    ) -> Result<Self, MatchError> {
        let base = channel_base.0;
        if base == 0 {
            return Err(MatchError::ReservedChannelBase);
        }
        let count = signals.len();
        if base as usize + count > u16::MAX as usize {
            return Err(MatchError::ChannelSpaceExhausted { base, count });
        }
        let channels = channels_of(&signals, base);
        Ok(Self {
            signals,
            source,
            context,
            channels,
            base: channel_base,
        })
    }

    /// The channel a signal's features are filed under, if the signal is in the
    /// set. (Every signal gets a channel; only non-`never` ones emit features.)
    pub fn channel_of(&self, name: &SignalId) -> Option<ChannelId> {
        self.channels.get(name).copied()
    }

    /// The channel base this sensor was constructed with — the low end of its
    /// occupied range `[base, base + signal_count)`.
    pub fn channel_base(&self) -> ChannelId {
        self.base
    }

    /// The first channel **above** this sensor's occupied range — where a
    /// composing campaign should base the next channel plugin so their `Feature`
    /// spaces never overlap.
    pub fn next_free_channel(&self) -> ChannelId {
        ChannelId(self.base.0 + self.signals.len() as u16)
    }

    /// The routed feature stream, sorted by `(Moment, channel, id)` — a
    /// deterministic, permutation-invariant function of the trace. This is the
    /// [`Sensor::observe`] body, exposed inherently for direct use.
    pub fn features(&self, t: &RunTrace) -> Vec<(Moment, Feature)> {
        let recs = self.source.records(t);
        let earliest_fault = self.context.fault_moments(t).into_iter().min();
        let mut out: Vec<(Moment, Feature)> = Vec::new();

        // No record-ordering step: `sometimes`/`cell` push per matching record
        // (in any order) and the whole stream is sorted by content at the end;
        // `state_max` folds through a `BTreeMap` keyed by `Moment`. Nothing keys
        // on the source's emission order, so the output is a pure function of
        // record *content* — no `(Moment, index)` tie-break to leak emission
        // order (round-3 determinism fix).
        for decl in self.signals.signals() {
            let Some(&channel) = self.channels.get(&decl.name) else {
                continue;
            };
            match decl.role {
                Role::Sometimes => {
                    for rec in &recs {
                        if record_matches(rec, &decl.expr, earliest_fault) {
                            out.push((
                                rec.moment(),
                                Feature {
                                    channel,
                                    id: Self::FIRED_FEATURE,
                                },
                            ));
                        }
                    }
                }
                Role::Cell => {
                    for rec in &recs {
                        if record_matches(rec, &decl.expr, earliest_fault) {
                            out.push((
                                rec.moment(),
                                Feature {
                                    channel,
                                    id: cell_id(rec, &decl.expr),
                                },
                            ));
                        }
                    }
                }
                Role::StateMax => {
                    // The register's value at a Moment is `max(all valid values
                    // at that Moment)`; `max` is order-independent, so folding
                    // per-Moment maxima through a `BTreeMap` (sorted by `Moment`)
                    // is content-deterministic. Walk the Moments ascending,
                    // carry the running max, and emit one feature each Moment its
                    // log2 bucket increases. A non-integer / absent `attr_max` on
                    // a matched record is a counted decode miss
                    // (`decode_misses`), never a panic and never folded.
                    let Some(attr) = decl.expr.attr_max.as_deref() else {
                        // Validation guarantees a state_max carries an attr_max;
                        // stay safe if a hand-built decl slipped past it.
                        continue;
                    };
                    let mut per_moment: BTreeMap<Moment, u64> = BTreeMap::new();
                    for rec in &recs {
                        if record_matches(rec, &decl.expr, earliest_fault)
                            && let Some(v) = rec.attr(attr).as_ref().and_then(value::as_u64)
                        {
                            per_moment
                                .entry(rec.moment())
                                .and_modify(|cur| *cur = (*cur).max(v))
                                .or_insert(v);
                        }
                    }
                    let mut running_max = 0u64;
                    let mut last_bucket = 0u64;
                    for (&m, &vmax) in &per_moment {
                        running_max = running_max.max(vmax);
                        let b = bucket(running_max);
                        if b > last_bucket {
                            last_bucket = b;
                            out.push((
                                m,
                                Feature {
                                    channel,
                                    id: FeatureId(b),
                                },
                            ));
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
}

impl<S: ChannelSource, C: ContextSource> MatchOracle<S, C> {
    /// Build an oracle over a signal set, a channel source, and a context source.
    pub fn new(signals: SignalSet, source: S, context: C) -> Self {
        Self {
            signals,
            source,
            context,
        }
    }

    /// Every `never`-rule violation as its content coordinates `(Moment,
    /// fingerprint)`, in no particular order — the raw hits that [`verdicts`]
    /// and [`judge`](Oracle::judge) order deterministically.
    ///
    /// A bug's identity is `(Moment, fingerprint)`: the [`Bug::env`] and
    /// [`Bug::stop`] are the run's, identical for every hit, so the fingerprint
    /// (`sha2(name ‖ kind ‖ matched attr bytes)`) fully distinguishes them —
    /// and it is a function of record *content*, never of emission or
    /// declaration order.
    ///
    /// [`verdicts`]: MatchOracle::verdicts
    fn never_hits(&self, t: &RunTrace) -> Vec<(Moment, [u8; 32])> {
        let recs = self.source.records(t);
        let earliest_fault = self.context.fault_moments(t).into_iter().min();
        let mut hits = Vec::new();
        for decl in self.signals.signals() {
            if decl.role != Role::Never {
                continue;
            }
            for rec in &recs {
                if record_matches(rec, &decl.expr, earliest_fault) {
                    hits.push((rec.moment(), never_fingerprint(&decl.name, rec, &decl.expr)));
                }
            }
        }
        hits
    }

    /// Every `never`-rule violation, one [`Bug`] per matching `(signal, record)`
    /// pair, ordered by content `(Moment, fingerprint)`. That ordering is a pure
    /// function of the trace — invariant under both declaration order (round 1)
    /// and the source's emission order of same-Moment records (round 3);
    /// same-`(Moment, fingerprint)` ties are, by definition, the same bug.
    /// [`judge`](Oracle::judge) returns the first of these.
    pub fn verdicts(&self, t: &RunTrace) -> Vec<Bug> {
        let mut hits = self.never_hits(t);
        hits.sort_unstable();
        hits.into_iter()
            .map(|(_, fingerprint)| Bug {
                env: t.env.clone(),
                stop: t.terminal.clone(),
                fingerprint,
            })
            .collect()
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
        // The content-earliest `never`-violation: the minimum `(Moment,
        // fingerprint)` hit, so it is deterministic regardless of emission or
        // declaration order. Re-judging after a fix surfaces the next (the
        // offline replay-plane property).
        self.never_hits(t)
            .into_iter()
            .min()
            .map(|(_, fingerprint)| Bug {
                env: t.env.clone(),
                stop: t.terminal.clone(),
                fingerprint,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub::{FaultMoments, OwnedRecords, RecordRec};
    use explorer::{COVERAGE_CHANNEL, Environment, Record, StopReason, VTime, Value};

    /// A minimal valid channel base for tests (channel 0 is coverage's).
    const BASE: ChannelId = ChannelId(1);

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
        let sensor =
            MatchSensor::new(signals, OwnedRecords(recs), FaultMoments(vec![]), BASE).unwrap();
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
        let sensor =
            MatchSensor::new(signals, OwnedRecords(recs), FaultMoments(vec![]), BASE).unwrap();
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
            BASE,
        )
        .unwrap();
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

    /// Regression (codex round-2 P1): matcher features must never collide with
    /// the coverage channel (spine `COVERAGE_CHANNEL` = channel 0), and base
    /// validation is enforced.
    #[test]
    fn channel_base_reserves_coverage_and_is_validated() {
        let signals = SignalSet::from_json(
            r#"{ "signals": [ { "name": "a.some", "role": "sometimes", "match": { "kind": "log", "attr": { "x": "1" } } } ] }"#,
        )
        .unwrap();
        let recs = vec![rec(1, "log", &[("x", Value::Str("1".into()))])];

        // Base 0 is coverage's — rejected.
        assert!(matches!(
            MatchSensor::new(
                signals.clone(),
                OwnedRecords(recs.clone()),
                FaultMoments(vec![]),
                COVERAGE_CHANNEL,
            ),
            Err(MatchError::ReservedChannelBase)
        ));

        // A valid base: the first signal's feature is on channel `base`, which
        // differs from coverage's channel 0. So a coverage Feature{0, id:1} and
        // the first matcher feature can never be confused in the archive.
        let sensor =
            MatchSensor::new(signals, OwnedRecords(recs), FaultMoments(vec![]), BASE).unwrap();
        let feats = sensor.features(&trace());
        assert_eq!(feats.len(), 1);
        let coverage_feature = Feature {
            channel: COVERAGE_CHANNEL,
            id: FeatureId(1),
        };
        assert_ne!(feats[0].1, coverage_feature);
        assert!(feats.iter().all(|(_, f)| f.channel != COVERAGE_CHANNEL));
        // The base and next-free accessors bound the occupied range.
        assert_eq!(sensor.channel_base(), BASE);
        assert_eq!(sensor.next_free_channel(), ChannelId(2));
    }

    /// Base + signal_count overflowing `u16::MAX` is rejected (the folded
    /// capacity guard).
    #[test]
    fn channel_space_exhaustion_is_rejected() {
        // A tiny set, but a base so high that base + count > u16::MAX.
        let signals = SignalSet::from_json(
            r#"{ "signals": [
                { "name": "s0", "role": "cell", "match": { "kind": "log" } },
                { "name": "s1", "role": "cell", "match": { "kind": "log" } }
            ] }"#,
        )
        .unwrap();
        assert!(matches!(
            MatchSensor::new(
                signals,
                OwnedRecords(vec![]),
                FaultMoments(vec![]),
                ChannelId(u16::MAX), // u16::MAX + 2 > u16::MAX
            ),
            Err(MatchError::ChannelSpaceExhausted { base, count })
                if base == u16::MAX && count == 2
        ));
    }

    /// Regression (codex round-3 P1, replay-plane purity): with several records
    /// sharing a Moment, the feature stream and the oracle verdicts must be a
    /// pure function of record *content* — never of the source's emission order.
    #[test]
    fn same_moment_emission_order_does_not_affect_output() {
        let signals = SignalSet::from_json(
            r#"{ "signals": [
                { "name": "reg",  "role": "state_max", "match": { "kind": "span", "attr": { "name": "wal" }, "attr_max": "lsn" } },
                { "name": "n.x",  "role": "never",     "match": { "kind": "span", "attr": { "op": "x" } } },
                { "name": "n.y",  "role": "never",     "match": { "kind": "span", "attr": { "op": "y" } } },
                { "name": "cell", "role": "cell",      "match": { "kind": "log",  "attr": { "phase": "*" } } }
            ] }"#,
        )
        .unwrap();

        // Four records, all at Moment 5, with distinct content.
        let a = rec(
            5,
            "span",
            &[
                ("name", Value::Str("wal".into())),
                ("lsn", Value::UInt(8)),
                ("op", Value::Str("x".into())),
            ],
        );
        let b = rec(
            5,
            "span",
            &[
                ("name", Value::Str("wal".into())),
                ("lsn", Value::UInt(3)),
                ("op", Value::Str("y".into())),
            ],
        );
        let c = rec(5, "log", &[("phase", Value::Str("ready".into()))]);
        let d = rec(5, "log", &[("phase", Value::Str("starting".into()))]);

        let forward = vec![a.clone(), b.clone(), c.clone(), d.clone()];
        let reversed: Vec<RecordRec> = forward.iter().rev().cloned().collect();

        let eval = |recs: Vec<RecordRec>| {
            let sensor = MatchSensor::new(
                signals.clone(),
                OwnedRecords(recs.clone()),
                FaultMoments(vec![]),
                BASE,
            )
            .unwrap();
            let oracle =
                MatchOracle::new(signals.clone(), OwnedRecords(recs), FaultMoments(vec![]));
            (
                serde_json::to_string(&sensor.features(&trace())).unwrap(),
                oracle.judge(&trace()),
                oracle.verdicts(&trace()),
            )
        };
        let (f_fwd, j_fwd, v_fwd) = eval(forward);
        let (f_rev, j_rev, v_rev) = eval(reversed);
        assert_eq!(f_fwd, f_rev, "feature stream depends on emission order");
        assert_eq!(j_fwd, j_rev, "judge verdict depends on emission order");
        assert_eq!(v_fwd, v_rev, "verdict list depends on emission order");

        // The state_max register reports bucket 4 (max(8, 3) = 8) exactly once —
        // never an intermediate bucket 2 from folding 3 before 8.
        let sensor = MatchSensor::new(
            signals,
            OwnedRecords(vec![a, b, c, d]),
            FaultMoments(vec![]),
            BASE,
        )
        .unwrap();
        let reg_ch = sensor.channel_of(&SignalId("reg".into())).unwrap();
        let reg_ids: Vec<u64> = sensor
            .features(&trace())
            .into_iter()
            .filter(|(_, f)| f.channel == reg_ch)
            .map(|(_, f)| f.id.0)
            .collect();
        assert_eq!(reg_ids, vec![4], "per-Moment max bucket only");
        // Two never signals matched at the same Moment → two distinct verdicts.
        assert_eq!(v_fwd.len(), 2);
    }
}
