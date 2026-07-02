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

use explorer::{Matchable, Moment, Record, RunTrace, Value};

use crate::{ChannelSource, ContextSource};

/// A minimal [`Matchable`] record: kind, attributes, and the moment observed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordRec {
    /// The moment the record was observed.
    pub moment: Moment,
    /// The spine record (kind + attributes).
    pub record: Record,
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
            record: Record {
                kind: kind.to_string(),
                attrs: attrs.into_iter().collect::<BTreeMap<_, _>>(),
            },
        }
    }
}

impl Matchable for RecordRec {
    fn kind(&self) -> &str {
        &self.record.kind
    }

    fn attr(&self, k: &str) -> Option<Value> {
        self.record.attrs.get(k).cloned()
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
                record: record.clone(),
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
    use explorer::{Environment, StopReason, VTime};

    #[test]
    fn trace_records_adapts_the_scrape_stream() {
        let t = RunTrace {
            terminal: StopReason::Quiescent { vtime: VTime(1) },
            env: Environment {
                blob_version: 1,
                bytes: vec![],
            },
            coverage: None,
            events: vec![],
            records: vec![(
                Moment(7),
                Record {
                    kind: "log".into(),
                    attrs: [("msg".to_string(), Value::Str("hi".into()))]
                        .into_iter()
                        .collect(),
                },
            )],
        };
        let recs = TraceRecords.records(&t);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].kind(), "log");
        assert_eq!(recs[0].moment(), Moment(7));
        assert_eq!(recs[0].attr("msg"), Some(Value::Str("hi".into())));
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
            terminal: StopReason::Quiescent { vtime: VTime(0) },
            env: Environment {
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
