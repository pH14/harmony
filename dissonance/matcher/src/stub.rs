// SPDX-License-Identifier: AGPL-3.0-or-later
//! Test / example stubs for the [`ChannelSource`](crate::ChannelSource) and
//! [`ContextSource`](crate::ContextSource) seams.
//!
//! Per the task-66 non-goals this crate ships **stubs only** — the concrete
//! channel adapters are later tasks (log records task 67, SDK/link events task
//! 73, OTel spans task 74) and the production schema-aware fault index is
//! campaign assembly (task 69). These types let the crate's own tests (and a
//! downstream plugin author reading for an example) drive the router without
//! any of that machinery:
//!
//! - [`RecordRec`] — a minimal [`Matchable`]: a kind, an attribute map, and a
//!   moment.
//! - [`TraceRecords`] — a [`ChannelSource`](crate::ChannelSource) adapting the
//!   trace's scrape-tier `records` stream (the realistic path).
//! - [`OwnedRecords`] — a [`ChannelSource`](crate::ChannelSource) serving a
//!   fixed, owned record list *ignoring the trace* — the "records absent from
//!   the trace verbatim" case (task 74's reassembled spans), and the convenient
//!   injection point for property tests.
//! - [`FaultMoments`] — a [`ContextSource`](crate::ContextSource) carrying an
//!   explicit fault-`Moment` list.

use std::collections::BTreeMap;

use explorer::{Matchable, Moment, RunTrace, Value};

use crate::{ChannelSource, ContextSource};

/// A minimal [`Matchable`] record: a kind, an attribute map, and the moment
/// observed. Task 65 made the spine's scrape-tier [`Record`](explorer::Record)
/// **raw and structural** (`{stream, line}` bytes); a `Matchable` needs the
/// *structured* `kind`/`attrs` a channel codebook (task 67) derives from those
/// bytes, so this stub carries them itself rather than wrapping the raw record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordRec {
    /// The moment the record was observed.
    pub moment: Moment,
    /// The record kind discriminator (e.g. `"log"`, `"span"`).
    pub kind: String,
    /// The record attributes, deterministically ordered.
    pub attrs: BTreeMap<String, Value>,
}

impl RecordRec {
    /// Build a record from a kind, a moment, and attribute pairs.
    pub fn new(
        moment: Moment,
        kind: &str,
        attrs: impl IntoIterator<Item = (String, Value)>,
    ) -> Self {
        Self {
            moment,
            kind: kind.to_string(),
            attrs: attrs.into_iter().collect::<BTreeMap<_, _>>(),
        }
    }
}

impl Matchable for RecordRec {
    fn kind(&self) -> &str {
        &self.kind
    }

    fn attr(&self, k: &str) -> Option<Value> {
        self.attrs.get(k).cloned()
    }

    fn moment(&self) -> Moment {
        self.moment
    }
}

/// A [`ChannelSource`] adapting the trace's scrape-tier `records` stream — the
/// realistic path a log/span channel plugin (task 67) would take.
#[derive(Clone, Debug, Default)]
pub struct TraceRecords;

impl ChannelSource for TraceRecords {
    type Rec = RecordRec;

    fn records(&self, t: &RunTrace) -> Vec<RecordRec> {
        t.records
            .iter()
            .map(|(moment, record)| RecordRec {
                moment: *moment,
                // Stub structuring of a raw scrape line: task 67's codebook does
                // the real work (log templates, field extraction); here every
                // line is a `"log"` record exposing its raw bytes and stream.
                kind: "log".to_string(),
                attrs: [
                    ("line".to_string(), Value::Bytes(record.line.clone())),
                    (
                        "stream".to_string(),
                        Value::UInt(u64::from(record.stream.0)),
                    ),
                ]
                .into_iter()
                .collect(),
            })
            .collect()
    }
}

/// A [`ChannelSource`] serving a fixed, owned record list, **ignoring** the
/// trace — the "records reassembled outside the trace verbatim" case, and the
/// injection point property tests use to feed an arbitrary record stream.
#[derive(Clone, Debug, Default)]
pub struct OwnedRecords(pub Vec<RecordRec>);

impl ChannelSource for OwnedRecords {
    type Rec = RecordRec;

    fn records(&self, _t: &RunTrace) -> Vec<RecordRec> {
        self.0.clone()
    }
}

/// A [`ContextSource`] carrying an explicit fault-`Moment` list — the stub for
/// the production schema-aware fault index (task 69).
#[derive(Clone, Debug, Default)]
pub struct FaultMoments(pub Vec<Moment>);

impl ContextSource for FaultMoments {
    fn fault_moments(&self, _t: &RunTrace) -> Vec<Moment> {
        self.0.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use explorer::{Moment, Record, Reproducer, StopReason, StreamId};

    #[test]
    fn trace_records_adapts_the_scrape_stream() {
        let t = RunTrace {
            terminal: StopReason::Quiescent { vtime: Moment(1) },
            env: Reproducer {
                blob_version: 1,
                bytes: vec![],
            },
            coverage: None,
            events: vec![],
            records: vec![(
                Moment(7),
                Record {
                    stream: StreamId(0),
                    line: b"hi\n".to_vec(),
                },
            )],
        };
        let recs = TraceRecords.records(&t);
        assert_eq!(recs.len(), 1);
        // The stub structures each raw line as a "log" record exposing its bytes.
        assert_eq!(recs[0].kind(), "log");
        assert_eq!(recs[0].moment(), Moment(7));
        assert_eq!(recs[0].attr("line"), Some(Value::Bytes(b"hi\n".to_vec())));
        assert_eq!(recs[0].attr("stream"), Some(Value::UInt(0)));
        assert_eq!(recs[0].attr("absent"), None);
    }

    #[test]
    fn owned_records_ignore_the_trace() {
        let src = OwnedRecords(vec![RecordRec::new(
            Moment(3),
            "span",
            [("k".to_string(), Value::UInt(1))],
        )]);
        // Even an empty trace yields the owned record (reassembled-verbatim).
        let t = RunTrace {
            terminal: StopReason::Quiescent { vtime: Moment(0) },
            env: Reproducer {
                blob_version: 1,
                bytes: vec![],
            },
            coverage: None,
            events: vec![],
            records: vec![],
        };
        assert_eq!(src.records(&t).len(), 1);
        assert_eq!(
            FaultMoments(vec![Moment(2)]).fault_moments(&t),
            vec![Moment(2)]
        );
    }
}
