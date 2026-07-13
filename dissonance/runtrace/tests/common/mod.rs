// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared test fixtures: an arbitrary-`RunTrace` strategy and a **test-local
//! `Sensor`** (a marker-hit line counter — not a shipped sensor) used to prove
//! re-derivation is stable across serialize→reload (gate 3).

#![allow(dead_code)] // each integration-test binary uses a subset

use std::collections::BTreeMap;

use explorer::{
    ChannelId, CoverageView, Reproducer, Feature, FeatureId, GuestEvent, Moment, Record,
    RunTrace, Sensor, StopReason, StreamId, Value,
};
use proptest::collection::{btree_map, vec};
use proptest::prelude::*;

/// A marker-hit line counter: emits one [`Feature`] per record whose bytes
/// contain `marker`, stamped at the record's [`Moment`], with the running hit
/// count as the feature id. A **pure function of the trace's records**, so it
/// re-derives identically over a reloaded trace — exactly the "a new Sensor over
/// recorded runs" replay-plane property, in miniature.
pub struct MarkerSensor {
    pub marker: Vec<u8>,
    pub channel: ChannelId,
}

impl MarkerSensor {
    pub fn new(marker: &[u8]) -> Self {
        MarkerSensor {
            marker: marker.to_vec(),
            channel: ChannelId(1),
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

impl Sensor for MarkerSensor {
    fn observe(&self, t: &RunTrace) -> Vec<(Moment, Feature)> {
        let mut out = Vec::new();
        let mut hits = 0u64;
        for (at, rec) in &t.records {
            if contains(&rec.line, &self.marker) {
                hits += 1;
                out.push((
                    *at,
                    Feature {
                        channel: self.channel,
                        id: FeatureId(hits),
                    },
                ));
            }
        }
        out
    }
}

fn arb_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::Int),
        any::<u64>().prop_map(Value::UInt),
        ".{0,16}".prop_map(Value::Str),
        vec(any::<u8>(), 0..16).prop_map(Value::Bytes),
    ]
}

fn arb_stop() -> impl Strategy<Value = StopReason> {
    prop_oneof![
        any::<u64>().prop_map(|v| StopReason::Deadline { vtime: Moment(v) }),
        any::<u64>().prop_map(|v| StopReason::Quiescent { vtime: Moment(v) }),
        (any::<u64>(), vec(any::<u8>(), 0..12)).prop_map(|(v, info)| StopReason::Crash {
            vtime: Moment(v),
            info
        }),
        (any::<u64>(), any::<u64>(), vec(any::<u8>(), 0..12)).prop_map(|(v, id, ctx)| {
            StopReason::Decision {
                vtime: Moment(v),
                id,
                ctx,
            }
        }),
        (any::<u64>(), any::<u32>(), vec(any::<u8>(), 0..12)).prop_map(|(v, id, data)| {
            StopReason::Assertion {
                vtime: Moment(v),
                id,
                data,
            }
        }),
        any::<u64>().prop_map(|v| StopReason::SnapshotPoint { vtime: Moment(v) }),
    ]
}

fn arb_guest_event() -> impl Strategy<Value = GuestEvent> {
    (".{0,12}", btree_map(".{0,8}", arb_value(), 0..4))
        .prop_map(|(kind, attrs): (String, BTreeMap<String, Value>)| GuestEvent { kind, attrs })
}

/// An arbitrary [`RunTrace`] exercising every field and variant — including a
/// non-empty `events` stream, so the journal codec's day-one serialization of
/// the (task-73) link tier is actually round-tripped.
pub fn arb_run_trace() -> impl Strategy<Value = RunTrace> {
    (
        arb_stop(),
        (any::<u16>(), vec(any::<u8>(), 0..24)),
        proptest::option::of(vec(any::<u8>(), 0..24)),
        vec((any::<u64>(), arb_guest_event()), 0..4),
        vec((any::<u64>(), any::<u16>(), vec(any::<u8>(), 0..24)), 0..6),
    )
        .prop_map(
            |(terminal, (blob_version, bytes), cov, events, records)| RunTrace {
                terminal,
                env: Reproducer {
                    blob_version,
                    bytes,
                },
                coverage: cov.map(|map| CoverageView { map }),
                events: events.into_iter().map(|(m, ev)| (Moment(m), ev)).collect(),
                records: records
                    .into_iter()
                    .map(|(m, s, line)| {
                        (
                            Moment(m),
                            Record {
                                stream: StreamId(s),
                                line,
                            },
                        )
                    })
                    .collect(),
            },
        )
}
