// SPDX-License-Identifier: AGPL-3.0-or-later
//! The replay core (`docs/SCORING.md` R1): recompute every retained timeline's
//! sensor observations from the recorded console — **no guest re-execution**.
//!
//! The observations are computed **once per campaign** and shared by every
//! candidate, because the sensor fold is upstream of the cell function: the
//! task-67 `LogSensor` clusters a campaign's consoles into template species with
//! a codebook that accumulates across that campaign's branches (and is
//! independent across seeds — the task-69 M2 ruling), and *that* stream is what
//! a `CellFn` keys. Only the keying is candidate-specific.
//!
//! Each branch's observations are an ordered list of [`Arrival`]s, one per
//! marker-filtered console record: the record's template species, plus the
//! chosen sparse state value the line carries, if any.
//!
//! ## Genesis-completeness
//!
//! R1's re-key contract is genesis-rooted. It holds here without a parent-chain
//! fold: every campaign branch runs from the campaign's sealed base, so each
//! recorded `RunTrace` covers its whole timeline. There is no cross-fork suffix
//! gap to close.

use explorer::{AdapterEnv, Moment, Record, RunTrace, Sensor};
use logtmpl::LogSensor;

use benchmark::report::CampaignLog;
use benchmark::{Benchmark, BugSpec, Configuration};

use crate::error::{Error, Result};
use crate::manifest::VerifiedCampaign;

/// The chosen sparse state observable the bug-3 workload prints once per branch:
/// the entropy draw it made. The prefix a line must carry for the harness to
/// read a draw out of it.
const DRAW_PREFIX: &[u8] = b"UUID_DRAW: draw=0x";

/// One sensor arrival: the point-in-time slice grows by this record's features,
/// then the candidate keys it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Arrival {
    /// The moment the record was observed (the cell function is moment-blind;
    /// carried so `CellFn::key` receives what the campaign passed it).
    pub at: Moment,
    /// The line's campaign-stable log-template species.
    pub species: explorer::FeatureId,
    /// The entropy draw this line carries, if it is the workload's chosen state
    /// observable. Stored raw; a candidate's [`StateProjection`] is applied at
    /// keying time, so the observation is shared across candidates.
    ///
    /// [`StateProjection`]: crate::candidate::StateProjection
    pub draw: Option<u64>,
}

/// One branch's observations.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BranchObs {
    /// The branch index within the campaign.
    pub branch: u64,
    /// The arrivals, in recorded console order.
    pub arrivals: Vec<Arrival>,
}

/// The debut of one template species in a campaign: which branch first emitted
/// it, and the line that minted it. The evidence behind the report's
/// species-attribution table.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SpeciesDebut {
    /// The species id (minted in first-seen order).
    pub species: u64,
    /// The branch it first appeared on.
    pub branch: u64,
    /// The console line that minted it.
    pub line: String,
}

/// One campaign's replayed observations, plus everything the harness needs to
/// reconstruct its search and check itself against the record.
pub struct CampaignObs {
    /// The corpus slice.
    pub slice: String,
    /// `Baseline` or `Signal`.
    pub config: Configuration,
    /// The campaign seed.
    pub seed: u64,
    /// The `explore_period` the campaign ran under.
    pub explore_period: u64,
    /// The bug the campaign targeted.
    pub bug: benchmark::BugId,
    /// Per-branch observations, in branch order (0..n, contiguous).
    pub branches: Vec<BranchObs>,
    /// Each branch's recorded environment seed — the ground truth the selection
    /// stream reconstruction is checked against.
    pub env_seeds: Vec<u64>,
    /// Each species' debut, in id order.
    pub debuts: Vec<SpeciesDebut>,
    /// The campaign's recorded discovery-event log.
    pub log: CampaignLog,
}

impl CampaignObs {
    /// A short `slice/config-seed` name for error messages and report rows.
    pub fn name(&self) -> String {
        format!("{}/{:?}-{}", self.slice, self.config, self.seed)
    }

    /// The branch the campaign's first certified find landed on, if any.
    pub fn find_branch(&self) -> Option<u64> {
        self.log.finds.first().map(|f| f.branch)
    }
}

/// Whether `hay` contains `needle` as a byte substring. Mirrors the campaign's
/// marker test (`conductor::benchcampaign::contains`).
fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// Read the entropy draw out of a `UUID_DRAW: draw=0x… prefix_bits=…` line.
/// Total: any other line, or a malformed one, yields `None`.
fn parse_draw(line: &[u8]) -> Option<u64> {
    let start = line
        .windows(DRAW_PREFIX.len())
        .position(|w| w == DRAW_PREFIX)?
        + DRAW_PREFIX.len();
    let digits = line[start..]
        .iter()
        .take_while(|b| b.is_ascii_hexdigit())
        .count();
    if digits == 0 || digits > 16 {
        return None;
    }
    let text = std::str::from_utf8(&line[start..start + digits]).ok()?;
    u64::from_str_radix(text, 16).ok()
}

/// The bug's terminal serial marker, filtered out of the console **before**
/// clustering exactly as the campaign did: the marker is *attribution*, not a
/// behavioural cell, and letting it mint a template species would make novelty
/// correlate with bug discovery spuriously.
fn filtered(trace: &RunTrace, marker: &[u8]) -> RunTrace {
    RunTrace {
        terminal: trace.terminal.clone(),
        env: trace.env.clone(),
        coverage: trace.coverage.clone(),
        events: trace.events.clone(),
        records: trace
            .records
            .iter()
            .filter(|(_, r)| !contains(&r.line, marker))
            .cloned()
            .collect(),
    }
}

/// Replay one verified campaign's sensor observations.
///
/// Fails loudly if the corpus does not have the shape the harness folds: a
/// branch without a trace (the campaign skipped an inadmissible proposal, whose
/// environment was never recorded, so its PRNG draws cannot be reconstructed), a
/// branch out of order, or an environment that is not an adapter blob.
pub fn observe_campaign(raw: &VerifiedCampaign, bugs: &Benchmark) -> Result<CampaignObs> {
    let log: CampaignLog = serde_json::from_slice(&raw.log_json).map_err(|source| Error::Json {
        what: format!("{}/{}-{} campaign log", raw.slice, raw.config, raw.seed),
        source,
    })?;
    let traces: Vec<(u64, RunTrace)> =
        serde_json::from_slice(&raw.traces_json).map_err(|source| Error::Json {
            what: format!("{}/{}-{} traces", raw.slice, raw.config, raw.seed),
            source,
        })?;

    let name = format!("{}/{}-{}", raw.slice, raw.config, raw.seed);
    let corpus = |why: String| Error::Corpus {
        campaign: name.clone(),
        why,
    };
    let spec: &BugSpec = bugs
        .get(log.bug)
        .ok_or_else(|| corpus(format!("no manifest entry for bug {:?}", log.bug)))?;

    if traces.len() != log.events.len() {
        return Err(corpus(format!(
            "the campaign log has {} branches but only {} traces were retained; a skipped \
             (inadmissible) branch's environment is unrecorded, so its selection draws cannot \
             be reconstructed",
            log.events.len(),
            traces.len()
        )));
    }

    let marker = spec.serial_marker.as_bytes();
    let sensor = LogSensor::new();
    let mut branches = Vec::with_capacity(traces.len());
    let mut env_seeds = Vec::with_capacity(traces.len());
    let mut debuts: Vec<SpeciesDebut> = Vec::new();
    let mut minted = 0u64;

    for (expected, (branch, trace)) in traces.iter().enumerate() {
        if *branch != expected as u64 {
            return Err(corpus(format!(
                "traces are not contiguous in branch order: expected {expected}, found {branch}"
            )));
        }
        let env_seed = AdapterEnv::decode(&trace.env)
            .map_err(|e| {
                corpus(format!(
                    "branch {branch}: environment is not an adapter blob: {e}"
                ))
            })?
            .spec
            .seed();
        env_seeds.push(env_seed);

        let view = filtered(trace, marker);
        let stream = sensor.observe(&view);
        if stream.len() != view.records.len() {
            return Err(corpus(format!(
                "branch {branch}: the sensor emitted {} features for {} records",
                stream.len(),
                view.records.len()
            )));
        }

        let mut arrivals = Vec::with_capacity(stream.len());
        for ((at, feature), (_, record)) in stream.into_iter().zip(&view.records) {
            // A species whose id has never been minted before debuts here. Ids
            // are minted in first-seen order, so `id >= minted` is exactly that.
            if feature.id.0 >= minted {
                minted = feature.id.0 + 1;
                debuts.push(SpeciesDebut {
                    species: feature.id.0,
                    branch: *branch,
                    line: line_text(record),
                });
            }
            arrivals.push(Arrival {
                at,
                species: feature.id,
                draw: parse_draw(&record.line),
            });
        }
        branches.push(BranchObs {
            branch: *branch,
            arrivals,
        });
    }

    debuts.sort_by_key(|d| d.species);
    Ok(CampaignObs {
        slice: raw.slice.clone(),
        config: log.config,
        seed: raw.seed,
        explore_period: raw.explore_period,
        bug: log.bug,
        branches,
        env_seeds,
        debuts,
        log,
    })
}

/// A record's line as display text: UTF-8-lossy, one trailing terminator dropped
/// (the sensor's own `log_line` discipline).
fn line_text(record: &Record) -> String {
    let decoded = String::from_utf8_lossy(&record.line);
    let trimmed = match decoded.strip_suffix('\n') {
        Some(without_lf) => without_lf.strip_suffix('\r').unwrap_or(without_lf),
        None => &decoded,
    };
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_draw_reads_the_chosen_observable() {
        assert_eq!(
            parse_draw(b"UUID_DRAW: draw=0xa56e6b675fd8f3a8 prefix_bits=8\n"),
            Some(0xa56e_6b67_5fd8_f3a8)
        );
        // The campaign's hex is not zero-padded: a short draw parses too.
        assert_eq!(
            parse_draw(b"UUID_DRAW: draw=0x40671e63f6451d2 prefix_bits=8"),
            Some(0x0406_71e6_3f64_51d2)
        );
    }

    /// Total on every other line the console carries, and on malformed input —
    /// library code never panics on untrusted bytes.
    #[test]
    fn parse_draw_is_total() {
        assert_eq!(parse_draw(b""), None);
        assert_eq!(parse_draw(b"supervisor: checkpoint committed"), None);
        assert_eq!(parse_draw(b"UUID_DRAW: draw=0x"), None, "no digits");
        assert_eq!(parse_draw(b"UUID_DRAW: draw=0xzz"), None, "not hex");
        assert_eq!(
            parse_draw(b"UUID_DRAW: draw=0x00000000000000000 x"),
            None,
            "17 digits cannot be a u64"
        );
        assert_eq!(parse_draw(b"UUID_DRAW: draw=0"), None, "truncated prefix");
        // Invalid UTF-8 before the prefix does not stop the scan or panic.
        assert_eq!(
            parse_draw(b"\xff\xfe UUID_DRAW: draw=0x1 prefix_bits=8"),
            Some(1)
        );
    }

    #[test]
    fn contains_matches_the_campaigns_marker_test() {
        assert!(contains(
            b"UUID_BUG: rare-entropy prefix matched",
            b"UUID_BUG"
        ));
        assert!(!contains(b"supervisor: checkpoint", b"UUID_BUG"));
        assert!(!contains(b"anything", b""), "an empty needle never matches");
    }

    #[test]
    fn line_text_drops_exactly_one_terminator() {
        let rec = |b: &[u8]| Record {
            stream: explorer::StreamId(0),
            line: b.to_vec(),
        };
        assert_eq!(line_text(&rec(b"a\r\n")), "a");
        assert_eq!(line_text(&rec(b"a\n")), "a");
        assert_eq!(line_text(&rec(b"a\r")), "a\r", "a bare CR is payload");
        assert_eq!(line_text(&rec(b"a")), "a");
    }

    /// The marker is filtered before clustering, so it can never mint a species.
    #[test]
    fn filtered_drops_only_marker_records() {
        let rec = |b: &[u8]| {
            (
                Moment(0),
                Record {
                    stream: explorer::StreamId(0),
                    line: b.to_vec(),
                },
            )
        };
        let trace = RunTrace {
            terminal: explorer::StopReason::Quiescent {
                vtime: explorer::VTime(0),
            },
            env: explorer::Environment {
                blob_version: 1,
                bytes: vec![],
            },
            coverage: None,
            events: vec![],
            records: vec![
                rec(b"supervisor: ok\n"),
                rec(b"UUID_BUG: matched\n"),
                rec(b"traps: general protection fault\n"),
            ],
        };
        let view = filtered(&trace, b"UUID_BUG");
        assert_eq!(view.records.len(), 2);
        assert_eq!(
            line_text(&view.records[1].1),
            "traps: general protection fault"
        );
    }
}
