// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared fixture scaffolding for the integration gates.

#![allow(dead_code)] // each test binary uses a different subset

use explorer::{
    CellFn, Feature, FeatureId, FeatureSet, Moment, Record, RunTrace, Sensor, StopReason, VTime,
    Value,
};
use logtmpl::{CellFnV1, Codebook, LogSensor, load_console_log};

/// The committed k3s console capture (≥ 5,000 lines — the cardinality gate).
pub const K3S: &str = include_str!("../fixtures/k3s-console.log");
/// The committed Postgres console capture.
pub const POSTGRES: &str = include_str!("../fixtures/postgres-console.log");

/// A `RunTrace` whose scrape-tier records are the fixture's lines.
pub fn trace(fixture: &str) -> RunTrace {
    RunTrace {
        terminal: StopReason::Quiescent { vtime: VTime(0) },
        env: explorer::Environment {
            blob_version: 1,
            bytes: vec![],
        },
        coverage: None,
        events: vec![],
        records: load_console_log(fixture),
    }
}

/// Just the `"log"` lines of a fixture, in order.
pub fn log_lines(fixture: &str) -> Vec<String> {
    load_console_log(fixture)
        .into_iter()
        .filter_map(|(_, r): (Moment, Record)| match r.attrs.get("msg") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

/// Fold a fresh codebook over the fixture, returning the finished codebook and
/// the per-line template-id stream (the "species set" derivation).
pub fn derive(fixture: &str) -> (Codebook, Vec<u64>) {
    let mut cb = Codebook::default();
    let ids = log_lines(fixture)
        .iter()
        .map(|line| cb.ingest(line).template)
        .collect();
    (cb, ids)
}

/// The distinct cell keys produced by keying the cumulative species slice at
/// every moment of a fixture's timeline, using the default (spec) knobs.
pub fn timeline_cell_keys(fixture: &str) -> Vec<Vec<u8>> {
    let sensor = LogSensor::new();
    let cell = CellFnV1::new();
    let mut live = FeatureSet::new();
    let mut keys = Vec::new();
    for (at, feat) in sensor.observe(&trace(fixture)) {
        live.insert(feat);
        keys.push(cell.key(at, &live));
    }
    keys
}

/// A convenience for building an expected feature.
pub fn feat(channel: u16, id: u64) -> Feature {
    Feature {
        channel: explorer::ChannelId(channel),
        id: FeatureId(id),
    }
}
