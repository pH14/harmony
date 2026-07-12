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

/// The manifest's spelling of a [`Configuration`], for cross-checking the
/// manifest label against the self-describing log without depending on `Debug`.
fn config_label(config: Configuration) -> &'static str {
    match config {
        Configuration::Signal => "Signal",
        Configuration::Baseline => "Baseline",
    }
}

/// The manifest's `(config, seed)` label must match the identity the campaign
/// log carries. Scoring reads `log.config` but keys under the manifest seed, and
/// the uniqueness gate dedups on the manifest strings, so a mislabelled or
/// spoofed entry that passed hashing could otherwise be scored under an identity
/// its trace does not carry. **Both** fields must match; either alone failing is
/// an [`Error::IdentityMismatch`].
fn check_identity(
    member: &str,
    manifest_config: &str,
    manifest_seed: u64,
    log: &CampaignLog,
) -> Result<()> {
    if manifest_config == config_label(log.config) && manifest_seed == log.seed {
        Ok(())
    } else {
        Err(Error::IdentityMismatch {
            member: member.to_string(),
            manifest_config: manifest_config.to_string(),
            manifest_seed,
            log_config: config_label(log.config).to_string(),
            log_seed: log.seed,
        })
    }
}

/// The slice-level attributes the manifest declares for a member — the `bug` it
/// targets and the `explore_period` it ran under — must match the member's own
/// log. `check_identity` covers only `(config, seed)`; without this a manifest
/// that mislabels a slice's bug passes every hash and identity gate while the
/// report copies the wrong bug identity from the slice.
fn check_slice_membership(
    member: &str,
    slice_bug: u16,
    slice_explore_period: u64,
    log: &CampaignLog,
) -> Result<()> {
    if u64::from(log.bug.0) != u64::from(slice_bug) {
        return Err(Error::SliceMismatch {
            member: member.to_string(),
            field: "bug",
            declared: u64::from(slice_bug),
            actual: u64::from(log.bug.0),
        });
    }
    if log.explore_period != slice_explore_period {
        return Err(Error::SliceMismatch {
            member: member.to_string(),
            field: "explore_period",
            declared: slice_explore_period,
            actual: log.explore_period,
        });
    }
    Ok(())
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

    // The manifest's `(config, seed)` label is otherwise trusted on faith: scoring
    // reads `log.config` but keys the campaign under `raw.seed`, and the
    // uniqueness gate dedups on the manifest strings. Cross-check both against the
    // self-describing log, so a mislabelled or spoofed entry cannot be scored (or
    // deduped) under an identity its trace does not actually carry.
    check_identity(&raw.member, &raw.config, raw.seed, &log)?;
    // …and the slice-level attributes the report copies (bug, explore_period): a
    // mislabelled slice (bug 3 tagged as bug 1) must not pass verification and
    // publish the wrong identity. Each member's own log is the ground truth.
    check_slice_membership(&raw.member, raw.bug, raw.explore_period, &log)?;

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

    /// The manifest label is cross-checked against the self-describing log: a
    /// matching `(config, seed)` passes, and a disagreement in **either** field
    /// is refused — so a spoofed or mislabelled entry cannot be scored (or
    /// deduped) under an identity its trace does not carry.
    #[test]
    fn check_identity_rejects_a_mislabelled_campaign() {
        let log = |config, seed| CampaignLog {
            bug: benchmark::BugId(3),
            config,
            seed,
            events: Vec::new(),
            finds: Vec::new(),
            explore_period: 4,
            order_range: 64,
        };
        let signal_1 = log(Configuration::Signal, 1);

        // The truthful label passes.
        assert!(check_identity("m", "Signal", 1, &signal_1).is_ok());

        // A wrong config — the double-score hole: the same member relabelled.
        match check_identity("m", "Baseline", 1, &signal_1) {
            Err(Error::IdentityMismatch {
                manifest_config,
                log_config,
                ..
            }) => {
                assert_eq!(manifest_config, "Baseline");
                assert_eq!(log_config, "Signal");
            }
            other => panic!("expected IdentityMismatch, got {other:?}"),
        }

        // A wrong seed is refused too — either field alone failing is enough.
        assert!(matches!(
            check_identity("m", "Signal", 2, &signal_1),
            Err(Error::IdentityMismatch { log_seed: 1, .. })
        ));
        // And the Baseline label is accepted when it is the truth (so the check
        // is not vacuously rejecting one config).
        assert!(check_identity("m", "Baseline", 7, &log(Configuration::Baseline, 7)).is_ok());
    }

    /// The slice-level attributes (`bug`, `explore_period`) are cross-checked
    /// against the member's own log, so a mislabelled slice cannot pass
    /// verification and publish the wrong bug identity.
    #[test]
    fn check_slice_membership_rejects_a_mislabelled_slice() {
        let mut log = CampaignLog {
            bug: benchmark::BugId(3),
            config: Configuration::Signal,
            seed: 1,
            events: Vec::new(),
            finds: Vec::new(),
            explore_period: 4,
            order_range: 64,
        };

        // The truthful slice attributes pass.
        assert!(check_slice_membership("m", 3, 4, &log).is_ok());

        // A slice that claims the wrong bug (3 tagged as 1) is refused, and the
        // error carries both the declared and the actual bug.
        match check_slice_membership("m", 1, 4, &log) {
            Err(Error::SliceMismatch {
                field,
                declared,
                actual,
                ..
            }) => {
                assert_eq!(field, "bug");
                assert_eq!((declared, actual), (1, 3));
            }
            other => panic!("expected a bug SliceMismatch, got {other:?}"),
        }

        // A slice that claims the wrong explore_period is refused too — the
        // ablation (ep=1) must never be scored as the campaign (ep=4).
        assert!(matches!(
            check_slice_membership("m", 3, 1, &log),
            Err(Error::SliceMismatch {
                field: "explore_period",
                declared: 1,
                actual: 4,
                ..
            })
        ));

        // The check is symmetric: a genuine ablation log (ep=1) passes at ep=1.
        log.explore_period = 1;
        assert!(check_slice_membership("m", 3, 1, &log).is_ok());
    }

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
