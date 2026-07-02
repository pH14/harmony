// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ingest a telemetry NDJSON recording into a scrape chunk stream.
//!
//! The scrape decoder ([`crate::decode_chunks`]) runs offline over any recorded
//! `(Moment, bytes)` stream. One such source is a telemetry `Console` recording
//! (task 29): an NDJSON file whose lines are `Event`s, the console ones carrying
//! `{"vns": …, "kind": {"Console": {"text": …}}}`. This helper turns that file
//! into the chunk stream `decode_chunks` consumes — the offline path the task's
//! §2 calls out.
//!
//! The telemetry schema is mirrored **locally** (conventions rule 2 — runtrace
//! stays a pure dissonance replay-plane crate with no `consonance` edge), read
//! with a minimal `serde_json` view so unknown `kind`s (Io/Msr/Tsc/… — not
//! console) are skipped rather than rejected. A line that is not valid JSON for
//! an event is a loud [`TraceError::Ingest`], never a silent drop. Console
//! `text` is a UTF-8 string on the wire (display fidelity), so its bytes are the
//! chunk bytes; the `vns` becomes the chunk's [`Moment`] via the same one-for-one
//! axis mapping the recorder uses.

use explorer::Moment;
use serde::Deserialize;

use crate::error::TraceError;

/// A minimal view of a telemetry `Event`: the deterministic `vns` stamp and the
/// opaque `kind` payload (only `Console` is consumed here).
#[derive(Deserialize)]
struct WireEvent {
    vns: u64,
    kind: serde_json::Value,
}

/// Ingest an NDJSON telemetry recording into a `(Moment, bytes)` chunk stream of
/// its `Console` output, in file order. Non-console events are skipped; a
/// malformed line is [`TraceError::Ingest`].
///
/// Feed the result straight to [`decode_chunks`](crate::decode_chunks) with the
/// [`StreamId`](explorer::StreamId) you assign the console.
pub fn ingest_ndjson(ndjson: &str) -> Result<Vec<(Moment, Vec<u8>)>, TraceError> {
    let mut out = Vec::new();
    for line in ndjson.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let ev: WireEvent = serde_json::from_str(line)
            .map_err(|e| TraceError::Ingest(format!("{e} in line: {line:?}")))?;
        // Externally-tagged `Console` variant: `{"Console": {"text": "…"}}`.
        // Non-console kinds are skipped, but a *Console* line whose `text` is
        // missing or non-string is malformed console — a loud [`TraceError::Ingest`],
        // never a silent chunk drop (the console stream must not lose bytes).
        if let Some(console) = ev.kind.get("Console") {
            let text = console
                .get("text")
                .and_then(|t| t.as_str())
                .ok_or_else(|| {
                    TraceError::Ingest(format!(
                        "Console event with missing/non-string `text` in line: {line:?}"
                    ))
                })?;
            out.push((Moment(ev.vns), text.as_bytes().to_vec()));
        }
    }
    Ok(out)
}
