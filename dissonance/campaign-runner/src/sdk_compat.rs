// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bridges campaign-runner's machine SDK capture to sdk-events' normalized
//! surface, and reconstructs the legacy GuestEvent stream the RunTrace journal
//! (and task-87 film) still consume.
use explorer::{GuestEvent, Moment, Value};
use sdk_events::{Normalized, ObservationId, Payload, SdkError, UpdateOp};

/// Decode a machine's raw SDK capture (`(u64, u32, bytes)`) into normalized
/// evidence.
pub fn decode_sdk(raw: &[(u64, u32, Vec<u8>)]) -> Result<Normalized, SdkError> {
    let recs: Vec<(sdk_events::Moment, u32, Vec<u8>)> = raw
        .iter()
        .map(|(m, id, b)| (sdk_events::Moment(*m), *id, b.clone()))
        .collect();
    sdk_events::decode_binary(&recs)
}

/// Reconstruct the legacy `(Moment, GuestEvent)` stream from normalized events,
/// byte-identical to the retired `sdk_events::decode_events` for state events
/// (kind "state", attrs reg/op/value) — what `RunTrace.events` / the runtrace
/// journal / film expect. Non-state payloads map to their analogous GuestEvent
/// kinds; keep it total (never panics).
pub fn guest_events_of(n: &Normalized) -> Vec<(Moment, GuestEvent)> {
    let mut out = Vec::new();
    for ev in &n.events {
        let at = Moment(ev.moment.0);
        let ge = match (&ev.id, &ev.payload) {
            (ObservationId::Point { local, .. }, Payload::State { op, value }) => {
                let op_str = match op {
                    UpdateOp::Max => "max",
                    _ => "set",
                };
                let attrs = [
                    ("reg".to_string(), Value::UInt(*local as u64)),
                    ("op".to_string(), Value::Str(op_str.to_string())),
                    ("value".to_string(), Value::UInt(*value)),
                ]
                .into_iter()
                .collect();
                GuestEvent {
                    kind: "state".to_string(),
                    attrs,
                }
            }
            _ => continue, // the game path only reads state events; skip the rest
        };
        out.push((at, ge));
    }
    out
}
