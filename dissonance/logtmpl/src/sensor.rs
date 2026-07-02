// SPDX-License-Identifier: AGPL-3.0-or-later
//! The log-template [`Sensor`] — the scrape tier's first real signal channel.
//!
//! The codebook is **a stateful fold over the run *sequence*, not just one run**
//! (the EXPLORATION ruling / task-67 spec): template ids are minted in first-seen
//! order and stay stable *across* traces, so the same species keeps the same
//! `FeatureId` from one run to the next. A run seeing `A` then `B` mints `B = 1`;
//! a later run seeing only `B` must *reuse* `B = 1`, not remint it from zero —
//! otherwise downstream cells (a `Feature` carries only `(channel, id)`) would
//! conflate distinct species. The sensor therefore holds its codebook as
//! campaign state and `observe`/`adapt` fold each trace into it.
//!
//! Interior mutability ([`RefCell`]) is how a `&self` [`Sensor::observe`] threads
//! that state; `Box<dyn Sensor>` carries no `Send`/`Sync` bound, so this is
//! sound (the campaign drives one sensor sequentially). Re-folding a trace the
//! codebook has already absorbed is idempotent — every line re-matches its
//! existing template — so `observe(t) == observe(t)` still holds (the spine's
//! purity contract) while genuinely *new* traces extend the codebook.
//!
//! Persistence ("serialize → reload → continue is indistinguishable") is
//! [`LogSensor::codebook`] (snapshot the fold) + [`LogSensor::with_codebook`]
//! (resume it) on top of [`Codebook`]'s `to_json`/`from_json`.

use std::cell::RefCell;

use explorer::{ChannelId, Feature, FeatureId, Moment, Record, RunTrace, Sensor, Value};

use crate::cluster::{Assignment, ClusterConfig, Codebook};
use crate::record::TemplateRecord;

/// The default channel the log-template sensor files its species features under.
/// Channel numbering is a campaign convention (the spine only needs stability);
/// `0` is the explorer defaults' coverage channel, so the scrape tier starts
/// at `1`.
pub const TEMPLATE_CHANNEL: ChannelId = ChannelId(1);

/// The kind discriminator of the scrape-tier records this sensor consumes.
const LOG_KIND: &str = "log";
/// The attribute holding the raw log line on a scrape-tier record.
const MSG_ATTR: &str = "msg";

/// The log-template sensor: Drain clustering behind the spine [`Sensor`] trait,
/// over a **campaign-persistent** codebook (ids stable across the run sequence).
#[derive(Clone, Debug)]
pub struct LogSensor {
    channel: ChannelId,
    /// The campaign fold state. `RefCell` lets the `&self` `observe`/`adapt`
    /// extend it; the sensor is single-threaded per campaign (no `Sync` needed).
    codebook: RefCell<Codebook>,
}

impl Default for LogSensor {
    fn default() -> Self {
        Self::new()
    }
}

impl LogSensor {
    /// A sensor with the default channel and default clustering knobs, over a
    /// fresh (empty) codebook.
    pub fn new() -> Self {
        Self {
            channel: TEMPLATE_CHANNEL,
            codebook: RefCell::new(Codebook::default()),
        }
    }

    /// Override the channel the emitted features are filed under.
    pub fn with_channel(mut self, channel: ChannelId) -> Self {
        self.channel = channel;
        self
    }

    /// Override the clustering knobs — resets to a fresh codebook with them
    /// (set config before folding any traces).
    pub fn with_config(self, config: ClusterConfig) -> Self {
        Self {
            channel: self.channel,
            codebook: RefCell::new(Codebook::new(config)),
        }
    }

    /// Resume a campaign from a persisted codebook (the "reload → continue"
    /// path): ids keep their first-seen assignment from before the snapshot.
    pub fn with_codebook(channel: ChannelId, codebook: Codebook) -> Self {
        Self {
            channel,
            codebook: RefCell::new(codebook),
        }
    }

    /// The channel this sensor files features under.
    pub fn channel(&self) -> ChannelId {
        self.channel
    }

    /// A snapshot of the current campaign codebook — serialize it
    /// ([`Codebook::to_json`]) to persist the fold across process restarts.
    pub fn codebook(&self) -> Codebook {
        self.codebook.borrow().clone()
    }

    /// Pull the raw line out of a scrape-tier log record, if it is one.
    fn log_line(record: &Record) -> Option<&str> {
        if record.kind != LOG_KIND {
            return None;
        }
        match record.attrs.get(MSG_ATTR) {
            Some(Value::Str(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Fold the trace's log records into the **campaign** codebook, yielding each
    /// line's moment, raw text, and clustering assignment in record order. The
    /// single fold both `observe` and `adapt` build on; ids are stable across the
    /// run sequence because the codebook persists between calls.
    fn derive(&self, t: &RunTrace) -> Vec<(Moment, String, Assignment)> {
        let mut codebook = self.codebook.borrow_mut();
        let mut out = Vec::new();
        for (at, record) in &t.records {
            if let Some(line) = Self::log_line(record) {
                let assignment = codebook.ingest(line);
                out.push((*at, line.to_string(), assignment));
            }
        }
        out
    }

    /// The matcher-DSL view of the run: one [`TemplateRecord`] per log line,
    /// each carrying the raw line, its assigned template id, and its extracted
    /// parameters — the same ids `observe` emits (shared fold, shared codebook).
    pub fn adapt(&self, t: &RunTrace) -> Vec<TemplateRecord> {
        self.derive(t)
            .into_iter()
            .map(|(at, msg, a)| TemplateRecord::new(at, msg, a.template, a.params))
            .collect()
    }
}

impl Sensor for LogSensor {
    /// The timestamped template-species stream: one feature per log line,
    /// `id` = the line's campaign-stable template.
    fn observe(&self, t: &RunTrace) -> Vec<(Moment, Feature)> {
        self.derive(t)
            .into_iter()
            .map(|(at, _, a)| {
                (
                    at,
                    Feature {
                        channel: self.channel,
                        id: FeatureId(a.template),
                    },
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_console_log;
    use explorer::StopReason;

    fn trace_of(lines: &str) -> RunTrace {
        RunTrace {
            terminal: StopReason::Quiescent {
                vtime: explorer::VTime(0),
            },
            env: explorer::Environment {
                blob_version: 1,
                bytes: vec![],
            },
            coverage: None,
            events: vec![],
            records: load_console_log(lines),
        }
    }

    #[test]
    fn observe_emits_one_stable_feature_per_line() {
        let t = trace_of("a b 1\na b 2\nc d 3\n");
        let s = LogSensor::new();
        let stream = s.observe(&t);
        assert_eq!(stream.len(), 3);
        // Lines 0 and 1 share a template (differ only in a masked digit); line 2
        // is a new species.
        assert_eq!(stream[0].1, stream[1].1);
        assert_ne!(stream[0].1, stream[2].1);
        // Moments are the synthetic line indices.
        assert_eq!(stream[0].0, Moment(0));
        assert_eq!(stream[2].0, Moment(2));
        // Filed under the configured channel.
        assert_eq!(stream[0].1.channel, TEMPLATE_CHANNEL);
    }

    #[test]
    fn observe_is_idempotent_on_the_same_trace() {
        // Re-folding a trace the codebook already absorbed reproduces the stream
        // (the spine's "same trace, same stream" contract).
        let t = trace_of("x 1\ny 2\nx 3\n");
        let s = LogSensor::new();
        assert_eq!(s.observe(&t), s.observe(&t));
    }

    #[test]
    fn ids_are_stable_across_the_run_sequence() {
        // The codex counterexample: trace1 sees A then B (B = 1); trace2 sees
        // only B and must reuse B = 1, not remint it from zero.
        let s = LogSensor::new();
        let t1 = trace_of("alpha start 1\nbeta stop 2\n"); // A=alpha…, B=beta…
        let t2 = trace_of("beta stop 3\n"); // only B

        let s1 = s.observe(&t1);
        let b_id = s1[1].1.id;
        assert_ne!(s1[0].1.id, b_id, "A and B are distinct species");
        assert_eq!(b_id, FeatureId(1), "B is the second species minted");

        let s2 = s.observe(&t2);
        assert_eq!(s2[0].1.id, b_id, "B keeps its id across traces");

        // A fresh sensor (independent campaign) would remint B from zero — the
        // exact conflation the persistent codebook prevents.
        let fresh = LogSensor::new();
        assert_eq!(fresh.observe(&t2)[0].1.id, FeatureId(0));
    }

    #[test]
    fn adapt_shares_the_campaign_codebook_with_observe() {
        let s = LogSensor::new();
        // Prime the codebook with a species so adapt sees a cross-trace id.
        s.observe(&trace_of("connection received port 5432\n"));
        let t = trace_of("connection received port 5433\nquery took 12ms\n");
        let stream = s.observe(&t);
        let records = s.adapt(&t);
        assert_eq!(stream.len(), records.len());
        for ((_, feat), rec) in stream.iter().zip(&records) {
            assert_eq!(feat.id.0, rec.template());
        }
        // The connection line reuses the primed species (id 0), not a fresh id.
        assert_eq!(records[0].template(), 0);
        assert_eq!(records[0].params(), ["5433".to_string()]);
    }

    #[test]
    fn snapshot_and_resume_continue_the_fold() {
        // Fold half a sequence, snapshot, resume in a new sensor, fold the rest —
        // ids must match an uninterrupted campaign.
        let uninterrupted = LogSensor::new();
        let t1 = trace_of("alpha start 1\nbeta stop 2\n");
        let t2 = trace_of("gamma tick 3\nbeta stop 4\n");
        uninterrupted.observe(&t1);
        let ref_stream = uninterrupted.observe(&t2);

        let first = LogSensor::new();
        first.observe(&t1);
        // Persist and reload (serialize → reload → continue).
        let bytes = first.codebook().to_json();
        let reloaded = Codebook::from_json(&bytes).expect("reload");
        let resumed = LogSensor::with_codebook(first.channel(), reloaded);
        let resumed_stream = resumed.observe(&t2);

        assert_eq!(resumed_stream, ref_stream, "resume is indistinguishable");
    }

    #[test]
    fn non_log_records_are_ignored() {
        use explorer::Record as R;
        let mut t = trace_of("a 1\n");
        t.records.push((
            Moment(99),
            R {
                kind: "span".into(),
                attrs: Default::default(),
            },
        ));
        // The span record contributes no feature.
        assert_eq!(LogSensor::new().observe(&t).len(), 1);
    }
}
