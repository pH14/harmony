// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared fixture scaffolding for the integration gates.

#![allow(dead_code)] // each test binary uses a different subset

use explorer::{CellFn, Feature, FeatureId, FeatureSet, RunTrace, Sensor, StopReason, VTime};
use logtmpl::{CellFnV1, LogSensor, load_console_log};

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

/// The decoded console lines of a fixture, in order — decoded exactly as the
/// sensor decodes a scrape record (UTF-8-lossy, terminator dropped).
pub fn log_lines(fixture: &str) -> Vec<String> {
    load_console_log(fixture)
        .into_iter()
        .map(|(_, r)| {
            String::from_utf8_lossy(&r.line)
                .trim_end_matches(['\n', '\r'])
                .to_string()
        })
        .collect()
}

/// A fresh-campaign derivation over the fixture through the **public** sensor
/// API: the opaque codebook snapshot bytes plus the per-line template-id stream
/// (the "species set" derivation). The codebook type itself is internal.
pub fn derive(fixture: &str) -> (Vec<u8>, Vec<u64>) {
    let sensor = LogSensor::new();
    let ids = sensor
        .observe(&trace(fixture))
        .into_iter()
        .map(|(_, f)| f.id.0)
        .collect();
    (sensor.codebook_bytes(), ids)
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
