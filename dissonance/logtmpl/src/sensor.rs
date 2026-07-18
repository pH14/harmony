// SPDX-License-Identifier: AGPL-3.0-or-later
//! The log-template sensor — the scrape tier's first real signal channel.
//!
//! The codebook is **a stateful fold over the run *sequence*, not just one run**
//! (the EXPLORATION ruling / task-67 spec): template ids are minted in first-seen
//! order and stay stable *across* traces, so the same species keeps the same
//! `FeatureId` from one run to the next. A run seeing `A` then `B` mints `B = 1`;
//! a later run seeing only `B` must *reuse* `B = 1`, not remint it from zero —
//! otherwise downstream cells (a `Feature` carries only `(channel, id)`) would
//! conflate distinct species. The sensor therefore holds its codebook as
//! campaign state behind interior mutability ([`RefCell`] — a `&self`
//! [`observe`](LogSensor::observe) must thread the state; the campaign drives
//! one sensor sequentially, so this is sound).
//!
//! **Read/write split.** [`observe`](LogSensor::observe) is the *mutating* fold —
//! it advances the campaign codebook. [`adapt`](LogSensor::adapt) is a
//! *read-only, order-invariant* view — it folds a **clone**, so it never
//! advances the campaign, and it folds the whole trace before reading params.
//! Re-folding an already-absorbed trace is idempotent (every line re-matches its
//! template), so `observe(t) == observe(t)` holds (the spine's purity contract)
//! while genuinely *new* traces extend the codebook.
//!
//! Persistence ("serialize → reload → continue is indistinguishable") is
//! [`codebook_bytes`](LogSensor::codebook_bytes) (snapshot to opaque bytes) +
//! [`with_codebook_bytes`](LogSensor::with_codebook_bytes) (resume); nothing
//! codebook-shaped crosses the boundary (the internality ruling).

use std::cell::RefCell;

use explorer::{Moment, Record, RunTrace};

use crate::feature::{ChannelId, Feature, FeatureId};

use crate::cluster::{Assignment, Codebook};
use crate::error::Result;
use crate::record::TemplateRecord;

/// The default channel the log-template sensor files its species features under.
/// Channel numbering is a campaign convention (only stability matters); `0` was
/// the historical coverage channel, so the scrape tier starts at `1`.
pub const TEMPLATE_CHANNEL: ChannelId = ChannelId(1);

/// The log-template sensor: Drain clustering over a **campaign-persistent**
/// codebook (ids stable across the run sequence).
#[derive(Clone)]
pub struct LogSensor {
    channel: ChannelId,
    /// The campaign fold state. `RefCell` lets the `&self` `observe`/`adapt`
    /// extend it; the sensor is single-threaded per campaign (no `Sync` needed).
    codebook: RefCell<Codebook>,
}

/// A **redacted** `Debug`: `#[derive(Debug)]` would recurse into the codebook and
/// print its templates, tree keys, and thresholds through any external `{:?}` —
/// leaking exactly the codebook internals the internality ruling keeps off the
/// boundary. Show only the channel and the (opaque) species count.
impl std::fmt::Debug for LogSensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogSensor")
            .field("channel", &self.channel)
            .field("templates", &self.codebook.borrow().len())
            .field("codebook", &format_args!("<redacted>"))
            .finish()
    }
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

    /// Resume a campaign from a **persisted codebook snapshot** (the opaque bytes
    /// [`codebook_bytes`](LogSensor::codebook_bytes) returned earlier): ids keep
    /// their first-seen assignment, so serialize → reload → continue is
    /// indistinguishable from never having stopped. Errors if the bytes are not a
    /// codebook this build can reload (bad version, corrupt tree).
    pub fn with_codebook_bytes(channel: ChannelId, bytes: &[u8]) -> Result<Self> {
        Ok(Self {
            channel,
            codebook: RefCell::new(Codebook::from_json(bytes)?),
        })
    }

    /// The channel this sensor files features under.
    pub fn channel(&self) -> ChannelId {
        self.channel
    }

    /// An **opaque** serialized snapshot of the campaign codebook — persist it
    /// across process restarts and reload with
    /// [`with_codebook_bytes`](LogSensor::with_codebook_bytes). The bytes are
    /// deliberately opaque: nothing codebook-shaped (template text, tree, or
    /// thresholds) is exposed to a caller (the internality ruling).
    pub fn codebook_bytes(&self) -> Vec<u8> {
        self.codebook.borrow().to_json()
    }

    /// Decode a scrape record's verbatim line bytes into a clustering-ready
    /// string: UTF-8-**lossy** (task 65 stores bytes verbatim and leaves decoding
    /// to the consuming plugin; lossy keeps it total over arbitrary bytes — no
    /// panic on untrusted input) with **exactly one** line terminator dropped.
    ///
    /// A record holds one newline-delimited line (task 65), terminated by `\n`
    /// or `\r\n`. Strip that single terminator only — never all trailing
    /// `\r`/`\n` — so a payload that genuinely ends in `\r` (a progress bar, a
    /// protocol echo) keeps its bytes and clusters to the same template.
    fn log_line(record: &Record) -> String {
        let decoded = String::from_utf8_lossy(&record.line);
        let line = match decoded.strip_suffix('\n') {
            // `\n` terminator: also drop a single preceding `\r` (the CRLF case).
            Some(without_lf) => without_lf.strip_suffix('\r').unwrap_or(without_lf),
            // No `\n`: leave the content as-is (a bare trailing `\r` is payload).
            None => &decoded,
        };
        line.to_string()
    }

    /// Fold a trace's scrape records into `codebook`, yielding each line's moment,
    /// decoded text, and clustering assignment in record order. Every scrape
    /// record is a raw console line (task 65) — structural interpretation is this
    /// crate's job — so all records cluster, whatever stream they came on.
    fn fold_into(codebook: &mut Codebook, t: &RunTrace) -> Vec<(Moment, String, Assignment)> {
        let mut out = Vec::new();
        for (at, record) in &t.records {
            let line = Self::log_line(record);
            let assignment = codebook.ingest(&line);
            out.push((*at, line, assignment));
        }
        out
    }

    /// The matcher-DSL view of the run: one [`TemplateRecord`] per log line, each
    /// carrying the raw line, its template id, and its extracted parameters.
    ///
    /// This is the **diagnostic clone-view** fold. *Exact* re-derivation of a
    /// recorded trace is defined (integrator ruling D1, `INTEGRATION.md` 6c) as
    /// **replay against the recording-time codebook snapshot** — persisted by the
    /// task-65 runtrace store — not through `adapt`; `adapt` gives the matcher DSL
    /// a canonical view of a trace against the *current* codebook.
    ///
    /// **Read-only.** It folds a *clone* of the campaign codebook, so it never
    /// advances the campaign; and it folds the **whole trace first**, then
    /// canonicalizes each id and extracts params against the *final* (post-fold)
    /// templates. On a given base it yields the same canonical stream `observe(t)`
    /// would (a deterministic fold), so param extraction is order-invariant.
    /// Note, though, that `adapt(t)` run *after* `observe(t)` folds against
    /// `base ∪ t` — it **is** the double-fold — so for a trace with a cross-observe
    /// erosion-steal it reflects the **D1-accepted clustering drift**, not the
    /// fresh-base assignment; *exact* re-derivation of a recorded trace is snapshot
    /// replay (the crate re-derivation contract), not `adapt`. Every template id is
    /// canonicalized through the alias table (the shape-uniqueness ruling), so a
    /// species that merged mid-fold is reported under its survivor id here too. A
    /// merge that occurs only inside the clone is discarded with the clone — the
    /// consumer receives only survivor ids, never a retired one, so it never needs
    /// that alias.
    pub fn adapt(&self, t: &RunTrace) -> Vec<TemplateRecord> {
        let mut view = self.codebook.borrow().clone();
        // Pass 1: fold the entire trace so every template reaches its final form.
        let assigned: Vec<(Moment, String, u64)> = Self::fold_into(&mut view, t)
            .into_iter()
            .map(|(at, msg, a)| (at, msg, a.template))
            .collect();
        // Pass 2: canonicalize the id, then read params against the final
        // (survivor) template — order-invariant and alias-canonical.
        assigned
            .into_iter()
            .map(|(at, msg, raw_id)| {
                let id = view.canonical(raw_id);
                let params = view.params_for(id, &msg);
                TemplateRecord::new(at, msg, id, params)
            })
            .collect()
    }
}

impl LogSensor {
    /// The timestamped template-species stream: one feature per log line, `id` =
    /// the line's campaign-stable template. This is the **mutating** fold — it
    /// advances the campaign codebook (ids stable across the run sequence,
    /// *canonical modulo the accepted cross-observe drift* — ruling D1; exact
    /// re-derivation is snapshot replay, see the crate re-derivation contract).
    /// Each emitted id is canonicalized through the alias table, so a species that
    /// merged under the shape-uniqueness invariant is emitted under its survivor
    /// id — including for lines assigned the retired id earlier in the same fold.
    /// (An inherent fold since task 132 M3 retired the compat `Sensor` trait;
    /// the purity contract `observe(t) == observe(t)` is unchanged.)
    pub fn observe(&self, t: &RunTrace) -> Vec<(Moment, Feature)> {
        let mut codebook = self.codebook.borrow_mut();
        let derived = Self::fold_into(&mut codebook, t);
        derived
            .into_iter()
            .map(|(at, _, a)| {
                (
                    at,
                    Feature {
                        channel: self.channel,
                        id: FeatureId(codebook.canonical(a.template)),
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
                vtime: explorer::Moment(0),
            },
            env: explorer::Reproducer {
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
    fn adapt_is_a_read_only_view() {
        // `adapt` is a VIEW: it never advances the campaign fold. Observing a
        // trace, then adapting it twice, yields identical records and leaves the
        // codebook serialization byte-for-byte unchanged (the round-4 fix — the
        // old shared mutating fold double-folded, inflating species and drifting
        // `param.N` between calls).
        let s = LogSensor::new();
        let t = trace_of("database system is ready\ndatabase system is starting\n");
        s.observe(&t);
        let before = s.codebook_bytes();

        let r1 = s.adapt(&t);
        let r2 = s.adapt(&t);
        assert_eq!(r1, r2, "adapting twice is identical (no double-fold)");
        assert_eq!(
            s.codebook_bytes(),
            before,
            "adapt does not mutate the campaign codebook"
        );
    }

    #[test]
    fn adapt_is_invariant_to_observe_order() {
        // A generalizing trace: the first line mints a literal template, the
        // second generalizes it (`is <*>`). `adapt` folds the WHOLE trace before
        // reading params, so the first line's params reflect the FINAL template —
        // identical whether or not `observe(t)` ran first (round-5 fix).
        let t = trace_of("database system is ready\ndatabase system is starting\n");

        // adapt on a fresh campaign (the view generalizes as it folds).
        let fresh = LogSensor::new();
        let a = fresh.adapt(&t);

        // observe first (campaign already generalized), then adapt.
        let primed = LogSensor::new();
        primed.observe(&t);
        let b = primed.adapt(&t);

        assert_eq!(a, b, "adapt(t) is invariant to a prior observe(t)");
        // Both read the first line's now-generalized position 3 as a parameter.
        assert_eq!(a[0].params(), ["ready".to_string()]);
        assert_eq!(a[1].params(), ["starting".to_string()]);
    }

    #[test]
    fn merged_species_are_canonical_across_observe_and_adapt() {
        // The round-8 convergent-generalization collision, driven through the
        // sensor: a line assigned the retired id must surface under the survivor
        // id in BOTH observe and adapt (canonicalization through the alias table).
        let s = LogSensor::new();
        let t = trace_of(concat!(
            "a b c d e\n",
            "a b x y z\n", // distinct species (id 1)
            "a b x y q\n",
            "a b x w q\n",
            "a b w w q\n", // id 1 → a b <*> <*> <*>
            "a b c d q\n",
            "a b c w q\n",
            "a b w w q\n",              // id 0 collides with id 1 → merge, alias 1→0
            "a b x y z\n",              // re-arrives → survivor
            "zzz distinct line here\n", // a genuinely separate species
        ));
        let obs: Vec<u64> = s.observe(&t).iter().map(|(_, f)| f.id.0).collect();
        let adp: Vec<u64> = s.adapt(&t).iter().map(|r| r.template()).collect();

        assert_eq!(
            obs, adp,
            "observe and adapt agree on every id under merging"
        );
        assert!(!obs.contains(&1), "the retired id 1 never surfaces");
        assert_eq!(obs[8], obs[0], "the re-arrival gets the survivor species");
        assert_ne!(
            obs[9], obs[0],
            "the distinct trailing line is its own species"
        );
    }

    #[test]
    fn zero_constant_lines_get_stable_ids() {
        // Round-9: a blank line and an all-digit line (both zero-constant shapes),
        // each observed twice, must keep a STABLE id and mint no duplicates.
        let s = LogSensor::new();
        let t = trace_of("\n123 456\n\n123 456\n"); // blank, digits, blank, digits
        let ids: Vec<u64> = s.observe(&t).iter().map(|(_, f)| f.id.0).collect();
        assert_eq!(ids.len(), 4);
        assert_eq!(ids[0], ids[2], "the blank line is a stable species");
        assert_eq!(ids[1], ids[3], "the all-digit line is a stable species");
        assert_ne!(ids[0], ids[1], "blank and all-digit are different species");
        let distinct: std::collections::BTreeSet<u64> = ids.iter().copied().collect();
        assert_eq!(
            distinct.len(),
            2,
            "no duplicate zero-constant templates minted"
        );
    }

    #[test]
    fn snapshot_and_resume_continue_the_fold() {
        // Fold half a sequence, snapshot to opaque bytes, resume in a new sensor,
        // fold the rest — ids must match an uninterrupted campaign.
        let uninterrupted = LogSensor::new();
        let t1 = trace_of("alpha start 1\nbeta stop 2\n");
        let t2 = trace_of("gamma tick 3\nbeta stop 4\n");
        uninterrupted.observe(&t1);
        let ref_stream = uninterrupted.observe(&t2);

        let first = LogSensor::new();
        first.observe(&t1);
        // Persist and reload through opaque bytes (serialize → reload → continue).
        let bytes = first.codebook_bytes();
        let resumed = LogSensor::with_codebook_bytes(first.channel(), &bytes).expect("reload");
        let resumed_stream = resumed.observe(&t2);

        assert_eq!(resumed_stream, ref_stream, "resume is indistinguishable");
    }

    #[test]
    fn records_from_any_stream_cluster() {
        // A scrape record is a raw console line (task 65); structural meaning is
        // this crate's job, so every record clusters whatever stream it rode.
        use explorer::{Record as R, StreamId};
        let mut t = trace_of("connection open 1\n"); // stream 0
        t.records.push((
            Moment(1),
            R {
                stream: StreamId(7),
                line: b"connection open 2\n".to_vec(),
            },
        ));
        let stream = LogSensor::new().observe(&t);
        assert_eq!(stream.len(), 2, "both records contribute a feature");
        // Both lines share a template (differ only in a masked digit), across
        // different byte streams.
        assert_eq!(stream[0].1, stream[1].1);
    }

    /// The decoded `msg` of a single-record trace whose line is exactly `bytes`.
    fn msg_of(bytes: &[u8]) -> String {
        use explorer::{Record as R, StreamId};
        let t = RunTrace {
            terminal: StopReason::Quiescent {
                vtime: explorer::Moment(0),
            },
            env: explorer::Reproducer {
                blob_version: 1,
                bytes: vec![],
            },
            coverage: None,
            events: vec![],
            records: vec![(
                Moment(0),
                R {
                    stream: StreamId(0),
                    line: bytes.to_vec(),
                },
            )],
        };
        LogSensor::new().adapt(&t)[0].msg().to_string()
    }

    #[test]
    fn strips_exactly_one_line_terminator() {
        // One `\n` or `\r\n` terminator is dropped — never more (round-5 fix).
        assert_eq!(msg_of(b"data\n"), "data");
        assert_eq!(msg_of(b"data\r\n"), "data");
        assert_eq!(msg_of(b"data"), "data", "no terminator, nothing stripped");
        // A payload that genuinely ends in `\r` keeps that byte: only the single
        // `\r\n` terminator is removed (the old `trim_end_matches` lost it).
        assert_eq!(msg_of(b"data\r\r\n"), "data\r");
        // A bare trailing `\r` (no `\n`) is payload, not a terminator.
        assert_eq!(msg_of(b"data\r"), "data\r");
    }

    #[test]
    fn invalid_utf8_decodes_lossily_without_panic() {
        // Invalid UTF-8 bytes + a CRLF terminator: decoded lossily, one
        // terminator dropped, never a panic.
        let msg = msg_of(b"boot stage \xff\xfe done\r\n");
        assert!(
            !msg.ends_with('\n') && !msg.ends_with('\r'),
            "terminator dropped"
        );
        assert!(msg.starts_with("boot stage"));
    }
}
