// SPDX-License-Identifier: AGPL-3.0-or-later
//! The log-template [`Sensor`] — the scrape tier's first real signal channel.
//!
//! `observe` folds a **fresh** codebook over the run's log records (in record
//! order) and emits one `Feature { channel, id }` per log line, stamped at the
//! line's moment: the open-vocabulary console stream becomes a stable,
//! low-cardinality species stream. Purity holds by construction — a fresh
//! codebook each call means the same trace always yields the same stream, and
//! nothing codebook-shaped appears in the signature.
//!
//! The same fold also produces the [`TemplateRecord`] stream for the matcher
//! adapter ([`LogSensor::adapt`]); sharing one fold guarantees the ids the
//! matcher sees and the ids the sensor emits agree.

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

/// The log-template sensor: Drain clustering behind the spine [`Sensor`] trait.
#[derive(Clone, Debug)]
pub struct LogSensor {
    channel: ChannelId,
    config: ClusterConfig,
}

impl Default for LogSensor {
    fn default() -> Self {
        Self::new()
    }
}

impl LogSensor {
    /// A sensor with the default channel and default clustering knobs.
    pub fn new() -> Self {
        Self {
            channel: TEMPLATE_CHANNEL,
            config: ClusterConfig::default(),
        }
    }

    /// Override the channel the emitted features are filed under.
    pub fn with_channel(mut self, channel: ChannelId) -> Self {
        self.channel = channel;
        self
    }

    /// Override the clustering knobs.
    pub fn with_config(mut self, config: ClusterConfig) -> Self {
        self.config = config;
        self
    }

    /// The channel this sensor files features under.
    pub fn channel(&self) -> ChannelId {
        self.channel
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

    /// Fold a fresh codebook over the trace's log records, yielding each line's
    /// moment, raw text, and clustering assignment in record order. The single
    /// fold both `observe` and `adapt` build on.
    fn derive(&self, t: &RunTrace) -> Vec<(Moment, String, Assignment)> {
        let mut codebook = Codebook::new(self.config.clone());
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
    /// parameters — the same ids `observe` emits.
    pub fn adapt(&self, t: &RunTrace) -> Vec<TemplateRecord> {
        self.derive(t)
            .into_iter()
            .map(|(at, msg, a)| TemplateRecord::new(at, msg, a.template, a.params))
            .collect()
    }
}

impl Sensor for LogSensor {
    /// The timestamped template-species stream: one feature per log line,
    /// `id` = the line's stable template. Pure per trace.
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
    fn observe_is_pure_across_calls() {
        let t = trace_of("x 1\ny 2\nx 3\n");
        let s = LogSensor::new();
        assert_eq!(s.observe(&t), s.observe(&t));
    }

    #[test]
    fn adapt_shares_ids_with_observe() {
        let t = trace_of("connection received port 5432\nconnection received port 5433\n");
        let s = LogSensor::new();
        let stream = s.observe(&t);
        let records = s.adapt(&t);
        assert_eq!(stream.len(), records.len());
        for ((_, feat), rec) in stream.iter().zip(&records) {
            assert_eq!(feat.id.0, rec.template());
        }
        // Both lines cluster together; the second's param is its raw port.
        assert_eq!(records[0].template(), records[1].template());
        assert_eq!(records[1].params(), ["5433".to_string()]);
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
