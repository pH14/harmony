// SPDX-License-Identifier: AGPL-3.0-or-later

//! The campaign-scale instrumentor loop: the mechanical stall proof, the
//! operator view assembled from recorded artifacts, and the per-stall
//! attempt record the loop carries between summonses.
//!
//! This is phase 4a's `prepare`/`install` discipline at campaign scale. The
//! loop is level-triggered on the stall state: a campaign that ends with no
//! watermark advance past its origin's frontier is proven stalled from its
//! recorded outputs alone, the operator view is assembled read-only from
//! those same outputs, and each installed attempt is recorded so the next
//! summons reads every prior attempt instead of repeating it. Success needs
//! no model judgment: the stall proof re-runs on the attempt's own recorded
//! outputs and decides mechanically.

use std::{error::Error, fs, path::Path};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    campaign::{
        SmbCampaignAdmissionDecision, SmbCampaignArtifactRecord, SmbCampaignModeReport,
        SmbCampaignStreamHeader, SmbCampaignStreamRecord,
    },
    phase4b::{SmbDetectorStats, SmbProgressWatermark},
    phase4c::{SmbArchiveReport, SmbGeneratedMutatorAccounting, SmbLadder, derive_smb_ladder},
};

/// Attempts one stall may consume before it escalates to the integrator
/// with every attempt attached.
pub const SMB_STALL_ATTEMPT_CAP: u8 = 3;

/// Request failures — invalid decisions, build failures, fixture-verify
/// failures — one attempt slot may consume before the loop refuses further
/// installs for it. Bounds the retry surface inside each attempt, so a
/// model that cannot produce working output stops burning budget outside
/// the attempt cap.
pub const SMB_STALL_REQUEST_RETRY_CAP: u8 = 3;

/// File name of one attempt's recorded request failures.
pub const SMB_STALL_REQUEST_FAILURES: &str = "request-failures.json";

/// Read one attempt slot's recorded request failures, in order.
///
/// # Errors
///
/// Returns an error when the failure record exists but cannot be parsed.
pub fn read_smb_request_failures(attempt_dir: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let path = attempt_dir.join(SMB_STALL_REQUEST_FAILURES);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_slice(&fs::read(&path)?)?)
}

/// Record one request failure for an attempt slot, returning the total so
/// far. Every failed attempt at working output is written before another
/// retry, the M12 discipline.
///
/// # Errors
///
/// Returns an error when the failure record cannot be read or written.
pub fn record_smb_request_failure(
    attempt_dir: &Path,
    error_text: &str,
) -> Result<u8, Box<dyn Error>> {
    fs::create_dir_all(attempt_dir)?;
    let mut failures = read_smb_request_failures(attempt_dir)?;
    failures.push(error_text.to_owned());
    fs::write(
        attempt_dir.join(SMB_STALL_REQUEST_FAILURES),
        serde_json::to_vec_pretty(&failures)?,
    )?;
    Ok(u8::try_from(failures.len()).unwrap_or(u8::MAX))
}

/// Refuse further installs for an attempt slot whose request retry cap is
/// consumed.
///
/// # Errors
///
/// Returns an error when the cap is consumed, or the record cannot be read.
pub fn enforce_smb_request_retry_cap(attempt_dir: &Path) -> Result<(), Box<dyn Error>> {
    let failures = read_smb_request_failures(attempt_dir)?;
    if failures.len() >= usize::from(SMB_STALL_REQUEST_RETRY_CAP) {
        return Err(format!(
            "this attempt slot has consumed its {SMB_STALL_REQUEST_RETRY_CAP}-retry request cap; \
             record a stop decision or escalate"
        )
        .into());
    }
    Ok(())
}

/// Mechanical proof that one finished campaign stalled: its recorded
/// watermark never advanced past its origin's frontier.
///
/// This is the campaign-scale analog of the phase-4a base-plateau proof. It
/// is computed from recorded outputs alone — no emulation, no randomness —
/// and running it changes no recorded artifact: the two archive hashes pin
/// exactly the evidence the verdict was read from.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbCampaignPlateauProof {
    /// SHA-256 of the origin archive file bytes.
    pub origin_archive_sha256: String,
    /// SHA-256 of the produced archive file bytes.
    pub produced_archive_sha256: String,
    /// The origin's frontier: its recorded watermark joined with its deepest
    /// retained tuple.
    pub origin_frontier: SmbProgressWatermark,
    /// The produced run's recorded watermark.
    pub produced_watermark: SmbProgressWatermark,
    /// The produced run's deepest retained tuple.
    pub produced_max_tuple: Option<(u8, u8, u16)>,
    /// Executions the produced run completed.
    pub produced_executions: u64,
    /// States the produced run retained.
    pub produced_retained: u64,
    /// Candidates the produced run rejected.
    pub produced_rejected: u64,
    /// True exactly when the produced watermark did not advance past the
    /// origin frontier.
    pub stalled: bool,
}

/// Join a recorded watermark with a mechanical tuple, keeping the greater.
fn join_watermark(watermark: SmbProgressWatermark, tuple: (u8, u8, u16)) -> SmbProgressWatermark {
    watermark.max(SmbProgressWatermark {
        world: tuple.0,
        level: tuple.1,
        progress: tuple.2,
    })
}

/// Prove whether one finished campaign stalled against its origin.
///
/// # Errors
///
/// Returns an error when the origin archive has no retained entries, since
/// a frontier cannot be read from an empty origin.
pub fn prove_smb_campaign_plateau(
    origin: &SmbArchiveReport,
    produced: &SmbArchiveReport,
    origin_archive_sha256: &str,
    produced_archive_sha256: &str,
) -> Result<SmbCampaignPlateauProof, Box<dyn Error>> {
    let origin_tuple = origin
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level, entry.key.progress))
        .max()
        .ok_or("origin archive contains no retained entries")?;
    let origin_frontier = join_watermark(origin.progress_watermark, origin_tuple);
    let produced_watermark = produced.progress_watermark;
    let produced_max_tuple = produced
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level, entry.key.progress))
        .max();
    Ok(SmbCampaignPlateauProof {
        origin_archive_sha256: origin_archive_sha256.to_owned(),
        produced_archive_sha256: produced_archive_sha256.to_owned(),
        origin_frontier,
        produced_watermark,
        produced_max_tuple,
        produced_executions: produced.executions,
        produced_retained: produced.retained,
        produced_rejected: produced.rejected,
        stalled: produced_watermark <= origin_frontier,
    })
}

/// One occupied cell of the frontier occupancy grid.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbOccupancyCell {
    /// 16-pixel progress bucket.
    pub progress: u16,
    /// Coarse player vertical band, the archive key's vertical term.
    pub vertical_band: u8,
    /// Retained entries in this cell.
    pub entries: u64,
}

/// Retained-population grid over the frontier `(world, level)` pair:
/// progress bucket × vertical band, read from the recorded archive alone.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbOccupancyGrid {
    /// Frontier `(world, level)` pair the grid covers.
    pub pair: (u8, u8),
    /// Retained entries at the frontier pair.
    pub entries: u64,
    /// Occupied cells in key order.
    pub cells: Vec<SmbOccupancyCell>,
}

/// Build the frontier occupancy grid from a recorded archive.
///
/// # Errors
///
/// Returns an error when the archive has no retained entries.
pub fn smb_frontier_occupancy_grid(
    archive: &SmbArchiveReport,
) -> Result<SmbOccupancyGrid, Box<dyn Error>> {
    use std::collections::BTreeMap;
    let pair = archive
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level))
        .max()
        .ok_or("archive contains no retained entries")?;
    let mut cells = BTreeMap::<(u16, u8), u64>::new();
    let mut entries = 0_u64;
    for entry in &archive.entries {
        if (entry.key.world, entry.key.level) != pair {
            continue;
        }
        entries = entries.saturating_add(1);
        *cells
            .entry((entry.key.progress, entry.key.player_y_bucket))
            .or_insert(0) += 1;
    }
    Ok(SmbOccupancyGrid {
        pair,
        entries,
        cells: cells
            .into_iter()
            .map(|((progress, vertical_band), entries)| SmbOccupancyCell {
                progress,
                vertical_band,
                entries,
            })
            .collect(),
    })
}

/// Admission outcomes for jobs extending parents at one frontier progress
/// bucket.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbRetentionFlowBucket {
    /// Parent 16-pixel progress bucket.
    pub parent_progress: u16,
    /// Jobs executed from parents in this bucket.
    pub jobs: u64,
    /// Candidates retained.
    pub retained: u64,
    /// Candidates resolved as duplicates.
    pub duplicates: u64,
    /// Candidates rejected by bounded quality-diversity retention.
    pub rejected: u64,
    /// Candidates refused by the admission probe.
    pub probe_refused: u64,
    /// Jobs skipped before execution as known duplicates.
    pub skips: u64,
}

/// Candidate flow at the frontier, read from the recorded stream and the
/// produced archive alone: what happened to everything produced from
/// frontier parents, bucket by bucket.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbRetentionFlowReport {
    /// Frontier `(world, level)` pair of the produced archive.
    pub pair: (u8, u8),
    /// Stream records examined.
    pub records: u64,
    /// Per-bucket admission outcomes, in bucket order.
    pub buckets: Vec<SmbRetentionFlowBucket>,
}

/// Build the frontier retention flow from a recorded stream.
///
/// Parent ids resolve against the produced archive, which holds every
/// entry the run's coordinator held, because entries are append-only.
///
/// # Errors
///
/// Returns an error when the stream is malformed or names a parent the
/// produced archive does not hold.
pub fn smb_frontier_retention_flow(
    stream_text: &str,
    produced: &SmbArchiveReport,
) -> Result<SmbRetentionFlowReport, Box<dyn Error>> {
    use std::collections::BTreeMap;
    let pair = produced
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level))
        .max()
        .ok_or("produced archive contains no retained entries")?;
    let by_id: BTreeMap<u64, &crate::phase4c::SmbArchiveEntryReport> = produced
        .entries
        .iter()
        .map(|entry| (entry.id, entry))
        .collect();
    let mut lines = stream_text.lines();
    let _header: SmbCampaignStreamHeader =
        serde_json::from_str(lines.next().ok_or("campaign stream is empty")?)?;
    let mut buckets = BTreeMap::<u16, SmbRetentionFlowBucket>::new();
    let mut records = 0_u64;
    // Resolve one parent to its frontier progress bucket, or `None` when it
    // sits off the frontier pair.
    let parent_bucket = |parent_id: u64| -> Result<Option<u16>, Box<dyn Error>> {
        let parent = by_id
            .get(&parent_id)
            .ok_or("recorded job names a parent the produced archive does not hold")?;
        Ok(((parent.key.world, parent.key.level) == pair).then_some(parent.key.progress))
    };
    for line in lines {
        records = records.saturating_add(1);
        match serde_json::from_str::<SmbCampaignStreamRecord>(line)? {
            SmbCampaignStreamRecord::Job(job) => {
                if let Some(progress) = parent_bucket(job.parent_id)? {
                    let bucket =
                        buckets
                            .entry(progress)
                            .or_insert_with(|| SmbRetentionFlowBucket {
                                parent_progress: progress,
                                ..SmbRetentionFlowBucket::default()
                            });
                    bucket.jobs = bucket.jobs.saturating_add(1);
                    for decision in &job.decisions {
                        match decision {
                            SmbCampaignAdmissionDecision::Retained { .. } => {
                                bucket.retained = bucket.retained.saturating_add(1);
                            }
                            SmbCampaignAdmissionDecision::Duplicate { .. } => {
                                bucket.duplicates = bucket.duplicates.saturating_add(1);
                            }
                            SmbCampaignAdmissionDecision::Rejected => {
                                bucket.rejected = bucket.rejected.saturating_add(1);
                            }
                            SmbCampaignAdmissionDecision::ProbeRefused => {
                                bucket.probe_refused = bucket.probe_refused.saturating_add(1);
                            }
                        }
                    }
                }
            }
            SmbCampaignStreamRecord::Skip(skip) => {
                if let Some(progress) = parent_bucket(skip.parent_id)? {
                    let bucket =
                        buckets
                            .entry(progress)
                            .or_insert_with(|| SmbRetentionFlowBucket {
                                parent_progress: progress,
                                ..SmbRetentionFlowBucket::default()
                            });
                    bucket.skips = bucket.skips.saturating_add(1);
                }
            }
        }
    }
    Ok(SmbRetentionFlowReport {
        pair,
        records,
        buckets: buckets.into_values().collect(),
    })
}

/// The origin and produced extended ladders side by side.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbLadderHistory {
    /// Ladder derived from the origin archive.
    pub origin: SmbLadder,
    /// Ladder derived from the produced archive.
    pub produced: SmbLadder,
}

/// Pointer to the film of the frontier trajectory, plus the arguments that
/// render it when it has not been rendered yet.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbFrontierFilmPointer {
    /// Deepest retained `(world, level, progress)` tuple of the produced
    /// archive; the film target.
    pub frontier: Option<(u8, u8, u16)>,
    /// Path of a rendered film, when the operator supplied one.
    pub film_path: Option<String>,
    /// How to render the film from the recorded archive when absent.
    pub render_command: String,
}

/// Everything one attempt at one stall leaves on the record: the decision,
/// the complete authored source, the run it launched, its retirement
/// counters, and the mechanical outcome.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbStallAttemptRecord {
    /// One-based attempt number within this stall.
    pub attempt: u8,
    /// Requested instrumentor action: `install_detector`, `install_mutator`,
    /// or `install_policy_value`.
    pub action: String,
    /// Provenance record of an installed authored artifact: name, source
    /// hash, scope. Absent for policy-value attempts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<SmbCampaignArtifactRecord>,
    /// Registered policy family of a policy-value attempt, named by its
    /// header field. Absent for authored-artifact attempts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_family: Option<String>,
    /// Proposed policy value as its recorded header identifier. Absent for
    /// authored-artifact attempts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_identifier: Option<String>,
    /// The instrumentor's recorded rationale.
    pub rationale: String,
    /// Complete authored source, carried so a re-summoned model reads its
    /// prior attempt instead of repeating it. Absent for policy-value
    /// attempts, whose whole content is the family and identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authored_source: Option<String>,
    /// Deterministic summary of the run the attempt launched.
    pub run: SmbStallAttemptRunSummary,
    /// Stall proof re-evaluated on the attempt's own recorded outputs.
    pub proof: SmbCampaignPlateauProof,
    /// True exactly when the attempt's run broke the stall.
    pub stall_broken: bool,
}

/// Deterministic summary of one attempt's campaign run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbStallAttemptRunSummary {
    /// Campaign seed of the attempt run.
    pub campaign_seed: u64,
    /// Worker count of the attempt run.
    pub workers: u32,
    /// Bounded execution budget, header-recorded like every run.
    pub execution_budget: u64,
    /// Executions completed.
    pub executions_completed: u64,
    /// States retained.
    pub retained: u64,
    /// Candidates rejected.
    pub rejected: u64,
    /// Candidates refused by the admission probe.
    pub probe_refused: u64,
    /// Terminal deaths observed.
    pub deaths: u64,
    /// Recorded watermark of the attempt run.
    pub progress_watermark: SmbProgressWatermark,
    /// Deepest retained tuple of the attempt run.
    pub max_tuple: Option<(u8, u8, u16)>,
    /// Installed-mutator retirement counters.
    pub generated_mutator: SmbGeneratedMutatorAccounting,
    /// Installed-detector retirement counters.
    pub detector: SmbDetectorStats,
}

/// Build an attempt run summary from a campaign report.
#[must_use]
pub fn smb_stall_attempt_run_summary(report: &SmbCampaignModeReport) -> SmbStallAttemptRunSummary {
    SmbStallAttemptRunSummary {
        campaign_seed: report.campaign_seed,
        workers: report.workers,
        execution_budget: report.execution_budget,
        executions_completed: report.executions_completed,
        retained: report.archive.retained,
        rejected: report.archive.rejected,
        probe_refused: report.probe_refused,
        deaths: report.archive.deaths,
        progress_watermark: report.archive.progress_watermark,
        max_tuple: derive_smb_ladder(&report.archive).max_tuple,
        generated_mutator: report.archive.generated_mutator,
        detector: report.archive.detector,
    }
}

/// The record a stall leaves for the integrator: the proof, the outcome,
/// and every attempt attached.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbStallEscalationRecord {
    /// `stall_broken` — the artifact goes up for a promotion ruling — or
    /// `attempt_cap_reached` — the widened-options tier.
    pub disposition: String,
    /// The stall proof this escalation rests on: for a broken stall the
    /// winning attempt's proof, otherwise the standing stall proof.
    pub proof: SmbCampaignPlateauProof,
    /// Every attempt at this stall, in order.
    pub attempts: Vec<SmbStallAttemptRecord>,
}

/// File name of one attempt's record inside its attempt directory.
pub const SMB_STALL_ATTEMPT_RECORD: &str = "attempt-record.json";

/// File name of the escalation record inside a stall directory.
pub const SMB_STALL_ESCALATION_RECORD: &str = "escalation.json";

/// Read every recorded attempt of one stall directory, in attempt order.
///
/// # Errors
///
/// Returns an error when an attempt record exists but cannot be parsed.
pub fn read_smb_stall_attempts(
    stall_dir: &Path,
) -> Result<Vec<SmbStallAttemptRecord>, Box<dyn Error>> {
    let mut attempts = Vec::new();
    for attempt in 1..=SMB_STALL_ATTEMPT_CAP {
        let record_path = stall_dir
            .join(format!("attempt-{attempt}"))
            .join(SMB_STALL_ATTEMPT_RECORD);
        if !record_path.is_file() {
            break;
        }
        attempts.push(serde_json::from_slice(&fs::read(&record_path)?)?);
    }
    Ok(attempts)
}

/// The next attempt number for one stall, enforcing the three-attempt cap.
///
/// # Errors
///
/// Returns an error when the cap is already consumed; the stall then
/// escalates instead of re-firing.
pub fn next_smb_stall_attempt(stall_dir: &Path) -> Result<u8, Box<dyn Error>> {
    let attempts = read_smb_stall_attempts(stall_dir)?;
    let next = u8::try_from(attempts.len())?.saturating_add(1);
    if next > SMB_STALL_ATTEMPT_CAP {
        return Err(format!(
            "this stall has consumed its {SMB_STALL_ATTEMPT_CAP}-attempt cap; escalate with every attempt attached"
        )
        .into());
    }
    Ok(next)
}

/// The census tools of the diagnostic pattern library, named so a model
/// session can ask for one by name. Every mode runs on recorded artifacts
/// through the `smb-completion` binary.
pub const SMB_CENSUS_TOOLS: &str = "\
Census tools run on recorded artifacts via the smb-completion binary; ask for one by name.\n\
derive-ladder — extended ladder from a recorded archive, no emulation.\n\
census-control-authority — control-authority census over recorded entries.\n\
diagnose-down-census — Down-press census over a recorded stream.\n\
diagnose-x-transit — candidate player-x histogram against recorded admission decisions.\n\
diagnose-loop-differential — work-RAM differential between advancing and looping states.\n\
diagnose-refused-grid — probe grid over probe-refused candidates from a recorded stream.\n\
diagnose-span — span boundaries over recorded entries.\n\
measure-viable-progress — viable-progress measurement over a recorded archive.\n\
audit-frontier-viability — frontier-viability audit of active representatives.\n\
smb-film archive-key <archive> <world> <level> <progress> <out> — film of the deepest trajectory.\n";

/// Assemble the operator view for one proven stall, read-only over the
/// recorded artifacts, into `view_dir`.
///
/// Refuses to assemble unless the plateau proof holds, exactly as the
/// phase-4a `prepare` refuses an open plateau.
///
/// # Errors
///
/// Returns an error when the stall is not proven, an input cannot be
/// parsed, or the view cannot be written.
#[allow(clippy::too_many_arguments)]
pub fn assemble_smb_stall_operator_view(
    view_dir: &Path,
    origin: &SmbArchiveReport,
    origin_archive_sha256: &str,
    produced: &SmbArchiveReport,
    produced_archive_sha256: &str,
    stream_text: &str,
    film_path: Option<&str>,
    attempts: &[SmbStallAttemptRecord],
) -> Result<SmbCampaignPlateauProof, Box<dyn Error>> {
    let proof = prove_smb_campaign_plateau(
        origin,
        produced,
        origin_archive_sha256,
        produced_archive_sha256,
    )?;
    if !proof.stalled {
        return Err("prepare input is not a stalled campaign: the watermark advanced".into());
    }
    let header: SmbCampaignStreamHeader = serde_json::from_str(
        stream_text
            .lines()
            .next()
            .ok_or("campaign stream is empty")?,
    )?;
    fs::create_dir_all(view_dir)?;
    fs::write(
        view_dir.join("plateau-proof.json"),
        serde_json::to_vec_pretty(&proof)?,
    )?;
    fs::write(
        view_dir.join("link-summary.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "header": header,
            "executions_completed": produced.executions,
            "retained": produced.retained,
            "rejected": produced.rejected,
            "deaths": produced.deaths,
            "progress_watermark": produced.progress_watermark,
            "selector": produced.selector,
            "generated_mutator": produced.generated_mutator,
            "detector": produced.detector,
        }))?,
    )?;
    fs::write(
        view_dir.join("occupancy-grid.json"),
        serde_json::to_vec_pretty(&smb_frontier_occupancy_grid(produced)?)?,
    )?;
    fs::write(
        view_dir.join("retention-flow.json"),
        serde_json::to_vec_pretty(&smb_frontier_retention_flow(stream_text, produced)?)?,
    )?;
    fs::write(
        view_dir.join("ladder-history.json"),
        serde_json::to_vec_pretty(&SmbLadderHistory {
            origin: derive_smb_ladder(origin),
            produced: derive_smb_ladder(produced),
        })?,
    )?;
    let frontier = derive_smb_ladder(produced).max_tuple;
    fs::write(
        view_dir.join("frontier-film.json"),
        serde_json::to_vec_pretty(&SmbFrontierFilmPointer {
            frontier,
            film_path: film_path.map(str::to_owned),
            render_command: frontier.map_or_else(
                || "no retained frontier to film".to_owned(),
                |(world, level, progress)| {
                    format!(
                        "smb-film archive-key <archive> {world} {level} {progress} <out>, then ffmpeg at framerate 60"
                    )
                },
            ),
        })?,
    )?;
    fs::write(view_dir.join("census-tools.txt"), SMB_CENSUS_TOOLS)?;
    fs::write(
        view_dir.join("prior-attempts.json"),
        serde_json::to_vec_pretty(&attempts)?,
    )?;
    fs::write(
        view_dir.join("fuzzer_stats"),
        format!(
            "target : smb-campaign\nexecs_done : {}\ncorpus_count : {}\nprogress_watermark : ({}, {}, {})\nstalled : true\nprior_attempts : {}\n",
            produced.executions,
            produced.entries.len(),
            produced.progress_watermark.world,
            produced.progress_watermark.level,
            produced.progress_watermark.progress,
            attempts.len(),
        ),
    )?;
    Ok(proof)
}

/// Write the escalation record for one stall.
///
/// # Errors
///
/// Returns an error when the record cannot be serialized or written.
pub fn write_smb_stall_escalation(
    stall_dir: &Path,
    record: &SmbStallEscalationRecord,
) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(stall_dir)?;
    fs::write(
        stall_dir.join(SMB_STALL_ESCALATION_RECORD),
        serde_json::to_vec_pretty(record)?,
    )?;
    Ok(())
}

/// SHA-256 of a file's bytes, hex-encoded, for pinning recorded artifacts.
///
/// # Errors
///
/// Returns an error when the file cannot be read.
pub fn file_sha256(path: &Path) -> Result<String, Box<dyn Error>> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}

#[cfg(test)]
mod tests {
    use super::{
        SMB_STALL_ATTEMPT_CAP, SMB_STALL_ATTEMPT_RECORD, SmbCampaignPlateauProof,
        SmbStallAttemptRecord, SmbStallAttemptRunSummary, SmbStallEscalationRecord,
        assemble_smb_stall_operator_view, next_smb_stall_attempt, prove_smb_campaign_plateau,
        read_smb_stall_attempts, smb_frontier_occupancy_grid, smb_frontier_retention_flow,
        write_smb_stall_escalation,
    };
    use crate::{
        campaign::SmbCampaignArtifactRecord,
        phase4b::{SmbDetectorStats, SmbInput, SmbMilestones, SmbProgressWatermark},
        phase4c::{
            SmbArchiveEntryReport, SmbArchiveKey, SmbArchiveReport, SmbGeneratedMutatorAccounting,
            SmbLadder,
        },
    };
    use std::fs;

    fn entry(id: u64, world: u8, level: u8, progress: u16, vertical: u8) -> SmbArchiveEntryReport {
        SmbArchiveEntryReport {
            id,
            parent_id: None,
            created_execution: id,
            input: SmbInput::default(),
            key: SmbArchiveKey {
                world,
                level,
                progress,
                player_y_bucket: vertical,
                player_engine_state: 0,
                state_fingerprint: u8::try_from(id % 64).expect("fingerprint"),
                room_x_bucket: 0,
                detector_features: 0,
            },
            milestones: SmbMilestones::default(),
            selector: None,
        }
    }

    fn archive(entries: Vec<SmbArchiveEntryReport>, watermark: (u8, u8, u16)) -> SmbArchiveReport {
        SmbArchiveReport {
            seed: 1,
            executions: 100,
            milestones: SmbMilestones::default(),
            progress_watermark: SmbProgressWatermark {
                world: watermark.0,
                level: watermark.1,
                progress: watermark.2,
            },
            first_reached: Default::default(),
            first_inputs: Default::default(),
            champion_input: SmbInput::default(),
            entries,
            progress_curve: Vec::new(),
            retained: 10,
            rejected: 20,
            deaths: 3,
            ranking: Default::default(),
            generated_mutator: Default::default(),
            detector: SmbDetectorStats::default(),
            ladder: SmbLadder::default(),
            selector: Default::default(),
        }
    }

    fn stalled_fixture() -> (SmbArchiveReport, SmbArchiveReport) {
        // C81 shape: the produced ladder tops at exactly the origin frontier
        // and the watermark never moves.
        let origin = archive(
            vec![entry(0, 6, 3, 72, 9), entry(1, 6, 3, 73, 10)],
            (6, 3, 73),
        );
        let produced = archive(
            vec![
                entry(0, 6, 3, 60, 8),
                entry(1, 6, 3, 72, 9),
                entry(2, 6, 3, 73, 11),
            ],
            (6, 3, 73),
        );
        (origin, produced)
    }

    #[test]
    fn plateau_proof_decides_the_c81_shape_stalled() {
        let (origin, produced) = stalled_fixture();
        let proof =
            prove_smb_campaign_plateau(&origin, &produced, "aa", "bb").expect("prove stall");
        assert!(proof.stalled);
        assert_eq!(
            proof.origin_frontier,
            SmbProgressWatermark {
                world: 6,
                level: 3,
                progress: 73
            }
        );
        assert_eq!(proof.produced_max_tuple, Some((6, 3, 73)));
        // Determinism: the same recorded inputs prove the same record.
        let again =
            prove_smb_campaign_plateau(&origin, &produced, "aa", "bb").expect("prove again");
        assert_eq!(proof, again);
    }

    #[test]
    fn plateau_proof_decides_the_h75_shape_broken() {
        // H75 shape: the pipe opened and the watermark ran two worlds ahead.
        let origin = archive(vec![entry(0, 3, 1, 208, 9)], (3, 1, 208));
        let produced = archive(
            vec![entry(0, 3, 1, 208, 9), entry(1, 5, 0, 166, 7)],
            (5, 0, 168),
        );
        let proof =
            prove_smb_campaign_plateau(&origin, &produced, "aa", "bb").expect("prove advance");
        assert!(!proof.stalled);
    }

    #[test]
    fn plateau_proof_rejects_an_empty_origin() {
        let (_, produced) = stalled_fixture();
        let empty = archive(Vec::new(), (0, 0, 0));
        assert!(prove_smb_campaign_plateau(&empty, &produced, "aa", "bb").is_err());
    }

    #[test]
    fn occupancy_grid_covers_the_frontier_pair_only() {
        let (_, produced) = stalled_fixture();
        let grid = smb_frontier_occupancy_grid(&produced).expect("grid");
        assert_eq!(grid.pair, (6, 3));
        assert_eq!(grid.entries, 3);
        assert_eq!(grid.cells.len(), 3);
        assert_eq!(grid.cells[0].progress, 60);
        assert_eq!(grid.cells[0].vertical_band, 8);
        assert_eq!(grid.cells[0].entries, 1);
    }

    fn attempt_record(attempt: u8) -> SmbStallAttemptRecord {
        SmbStallAttemptRecord {
            attempt,
            action: "install_mutator".to_owned(),
            artifact: Some(SmbCampaignArtifactRecord {
                name: format!("attempt_{attempt}"),
                source_sha256: "00".to_owned(),
                scope: "6,3,60,73".to_owned(),
            }),
            policy_family: None,
            policy_identifier: None,
            rationale: "test".to_owned(),
            authored_source: Some("pub struct InstalledMacro;".to_owned()),
            run: SmbStallAttemptRunSummary {
                campaign_seed: 7,
                workers: 2,
                execution_budget: 100,
                executions_completed: 100,
                retained: 5,
                rejected: 5,
                probe_refused: 0,
                deaths: 0,
                progress_watermark: SmbProgressWatermark {
                    world: 6,
                    level: 3,
                    progress: 73,
                },
                max_tuple: Some((6, 3, 73)),
                generated_mutator: SmbGeneratedMutatorAccounting::default(),
                detector: SmbDetectorStats::default(),
            },
            proof: SmbCampaignPlateauProof {
                origin_archive_sha256: "aa".to_owned(),
                produced_archive_sha256: "bb".to_owned(),
                origin_frontier: SmbProgressWatermark {
                    world: 6,
                    level: 3,
                    progress: 73,
                },
                produced_watermark: SmbProgressWatermark {
                    world: 6,
                    level: 3,
                    progress: 73,
                },
                produced_max_tuple: Some((6, 3, 73)),
                produced_executions: 100,
                produced_retained: 5,
                produced_rejected: 5,
                stalled: true,
            },
            stall_broken: false,
        }
    }

    #[test]
    fn attempt_cap_enforces_three_attempts_per_stall() {
        let stall_dir = std::env::temp_dir().join("fuzzer-instrumentor-attempt-cap-test");
        let _ = fs::remove_dir_all(&stall_dir);
        fs::create_dir_all(&stall_dir).expect("create stall dir");
        assert_eq!(next_smb_stall_attempt(&stall_dir).expect("first"), 1);
        for attempt in 1..=SMB_STALL_ATTEMPT_CAP {
            let attempt_dir = stall_dir.join(format!("attempt-{attempt}"));
            fs::create_dir_all(&attempt_dir).expect("create attempt dir");
            fs::write(
                attempt_dir.join(SMB_STALL_ATTEMPT_RECORD),
                serde_json::to_vec_pretty(&attempt_record(attempt)).expect("serialize attempt"),
            )
            .expect("write attempt record");
            let next = next_smb_stall_attempt(&stall_dir);
            if attempt < SMB_STALL_ATTEMPT_CAP {
                assert_eq!(next.expect("next attempt"), attempt + 1);
            } else {
                // The fourth summons must refuse: the cap escalates instead.
                assert!(next.is_err());
            }
        }
        let attempts = read_smb_stall_attempts(&stall_dir).expect("read attempts");
        assert_eq!(attempts.len(), usize::from(SMB_STALL_ATTEMPT_CAP));
        let escalation = SmbStallEscalationRecord {
            disposition: "attempt_cap_reached".to_owned(),
            proof: attempts[2].proof.clone(),
            attempts: attempts.clone(),
        };
        write_smb_stall_escalation(&stall_dir, &escalation).expect("write escalation");
        let read_back: SmbStallEscalationRecord = serde_json::from_slice(
            &fs::read(stall_dir.join(super::SMB_STALL_ESCALATION_RECORD)).expect("read escalation"),
        )
        .expect("parse escalation");
        assert_eq!(read_back, escalation);
        assert_eq!(read_back.attempts.len(), 3);
        let _ = fs::remove_dir_all(&stall_dir);
    }

    #[test]
    fn request_retry_cap_bounds_the_request_surface_per_attempt() {
        let attempt_dir = std::env::temp_dir().join("fuzzer-instrumentor-request-retry-test");
        let _ = fs::remove_dir_all(&attempt_dir);
        fs::create_dir_all(&attempt_dir).expect("create attempt dir");
        assert!(super::enforce_smb_request_retry_cap(&attempt_dir).is_ok());
        for failure in 1..=super::SMB_STALL_REQUEST_RETRY_CAP {
            let total = super::record_smb_request_failure(
                &attempt_dir,
                &format!("failure {failure}: authored source contains forbidden token"),
            )
            .expect("record failure");
            assert_eq!(total, failure);
        }
        // The cap refuses further installs for this attempt slot, and every
        // recorded failure survives for the next summons to read.
        assert!(super::enforce_smb_request_retry_cap(&attempt_dir).is_err());
        let failures =
            super::read_smb_request_failures(&attempt_dir).expect("read recorded failures");
        assert_eq!(
            failures.len(),
            usize::from(super::SMB_STALL_REQUEST_RETRY_CAP)
        );
        assert!(failures[0].starts_with("failure 1"));
        let _ = fs::remove_dir_all(&attempt_dir);
    }

    /// A minimal recorded stream: real header shape, no records needed for
    /// the flow report to be well-formed.
    fn fixture_stream(produced: &SmbArchiveReport) -> String {
        let header = serde_json::json!({
            "format": "smb-campaign-stream-v1",
            "campaign_seed": 1_u64,
            "workers": 2_u32,
            "host": "unit-test",
            "origin_kind": "archive",
            "origin_path": "origin.json",
            "origin_archive_sha256": "aa",
            "resume_input_sha256": "cc",
            "resume_actions": 0_usize,
            "execution_budget": 100_u64,
            "wall_budget_seconds": null,
            "action_limit": 96_usize,
            "duration_policy": "stratified",
            "suffix_policy": "one_or_two",
            "retention_policy": "probe_at_admission_45",
            "parent_scheduler": "concentrated_recency_128",
            "executor_mode": "snapshot_resume_archive",
            "worker_seed_derivation": "sha256(campaign_seed_le || worker_index_le)[0..8] as u64 le",
            "rom_sha256": "dd",
        });
        let job = serde_json::json!({
            "event": "job",
            "sequence": 1_u64,
            "worker": 0_u32,
            "parent_id": produced.entries[1].id,
            "mutation_seed": 5_u64,
            "frames": 60_u64,
            "result_sha256": "ee",
            "decisions": [
                {"decision": "retained", "id": 2_u64},
                {"decision": "rejected"},
                {"decision": "probe_refused"},
            ],
        });
        let skip = serde_json::json!({
            "event": "skip",
            "worker": 1_u32,
            "parent_id": produced.entries[2].id,
            "mutation_seed": 6_u64,
        });
        format!("{header}\n{job}\n{skip}\n")
    }

    #[test]
    fn retention_flow_tallies_frontier_decisions_by_parent_bucket() {
        let (_, produced) = stalled_fixture();
        let stream = fixture_stream(&produced);
        let flow = smb_frontier_retention_flow(&stream, &produced).expect("flow");
        assert_eq!(flow.pair, (6, 3));
        assert_eq!(flow.records, 2);
        assert_eq!(flow.buckets.len(), 2);
        let at_72 = &flow.buckets[0];
        assert_eq!(at_72.parent_progress, 72);
        assert_eq!(at_72.jobs, 1);
        assert_eq!(at_72.retained, 1);
        assert_eq!(at_72.rejected, 1);
        assert_eq!(at_72.probe_refused, 1);
        let at_73 = &flow.buckets[1];
        assert_eq!(at_73.parent_progress, 73);
        assert_eq!(at_73.skips, 1);
    }

    #[test]
    fn operator_view_is_read_only_and_reproducible() {
        let (origin, produced) = stalled_fixture();
        let stream = fixture_stream(&produced);
        let base = std::env::temp_dir().join("fuzzer-instrumentor-operator-view-test");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create test dir");
        // Write the recorded artifacts as files so the read-only claim is
        // checked against real bytes.
        let origin_path = base.join("origin.json");
        let produced_path = base.join("archive-live.json");
        let stream_path = base.join("stream.jsonl");
        fs::write(
            &origin_path,
            serde_json::to_vec_pretty(&origin).expect("origin"),
        )
        .expect("write origin");
        fs::write(
            &produced_path,
            serde_json::to_vec_pretty(&produced).expect("produced"),
        )
        .expect("write produced");
        fs::write(&stream_path, &stream).expect("write stream");
        let before = [
            super::file_sha256(&origin_path).expect("hash origin"),
            super::file_sha256(&produced_path).expect("hash produced"),
            super::file_sha256(&stream_path).expect("hash stream"),
        ];
        let view_a = base.join("view-a");
        let proof = assemble_smb_stall_operator_view(
            &view_a,
            &origin,
            &before[0],
            &produced,
            &before[1],
            &stream,
            None,
            &[attempt_record(1)],
        )
        .expect("assemble view");
        assert!(proof.stalled);
        // Running the check changed no recorded artifact or hash.
        let after = [
            super::file_sha256(&origin_path).expect("rehash origin"),
            super::file_sha256(&produced_path).expect("rehash produced"),
            super::file_sha256(&stream_path).expect("rehash stream"),
        ];
        assert_eq!(before, after);
        // A second assembly produces byte-identical files.
        let view_b = base.join("view-b");
        assemble_smb_stall_operator_view(
            &view_b,
            &origin,
            &before[0],
            &produced,
            &before[1],
            &stream,
            None,
            &[attempt_record(1)],
        )
        .expect("assemble view again");
        for name in [
            "plateau-proof.json",
            "link-summary.json",
            "occupancy-grid.json",
            "retention-flow.json",
            "ladder-history.json",
            "frontier-film.json",
            "census-tools.txt",
            "prior-attempts.json",
            "fuzzer_stats",
        ] {
            let a = fs::read(view_a.join(name)).expect("read view a file");
            let b = fs::read(view_b.join(name)).expect("read view b file");
            assert_eq!(a, b, "operator view file {name} must be reproducible");
        }
        // An advanced (unstalled) link refuses to prepare.
        let advanced = archive(
            vec![entry(0, 6, 3, 73, 9), entry(1, 7, 0, 10, 9)],
            (7, 0, 12),
        );
        assert!(
            assemble_smb_stall_operator_view(
                &base.join("view-c"),
                &origin,
                "aa",
                &advanced,
                "bb",
                &stream,
                None,
                &[],
            )
            .is_err()
        );
        let _ = fs::remove_dir_all(&base);
    }

    /// The full loop entry over genuinely recorded outputs: a two-link
    /// synthetic chain whose second link cannot advance is proven stalled
    /// from its own recorded stream and archives, and its operator view
    /// assembles.
    #[test]
    fn recorded_synthetic_chain_stall_proves_and_prepares() {
        use crate::campaign::{
            SmbCampaignConfig, SmbCampaignOrigin, SmbCampaignVocabulary, run_smb_campaign,
        };
        use crate::phase4c::SmbArchiveSelectorPolicy;
        use crate::phase4c::{SmbArchiveKeyPolicy, SmbArchiveRetentionPolicy};
        use sha2::{Digest, Sha256};

        let mut rom = vec![0_u8; 16 + (16 * 1024) + (8 * 1024)];
        rom[..16].copy_from_slice(&[b'N', b'E', b'S', 0x1a, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let prg_len = 16 * 1024;
        let prg = &mut rom[16..16 + prg_len];
        prg.fill(0xea);
        prg[..3].copy_from_slice(&[0x4c, 0x00, 0x80]);
        for vector in [0x3ffa, 0x3ffc, 0x3ffe] {
            prg[vector..vector + 2].copy_from_slice(&0x8000_u16.to_le_bytes());
        }
        let config = |seed: u64| SmbCampaignConfig {
            campaign_seed: seed,
            workers: 2,
            execution_budget: 10,
            action_limit: 96,
            host: "unit-test".to_owned(),
            wall_budget: None,
            selector_policy: SmbArchiveSelectorPolicy::ConcentratedRecency,
            retention_policy: SmbArchiveRetentionPolicy::Frozen,
            archive_entry_limit: 32_768,
            vocabulary: SmbCampaignVocabulary::FrozenNineMask,
            key_policy: SmbArchiveKeyPolicy::Frozen,
        };
        let mut first_stream = Vec::new();
        let first = run_smb_campaign(
            &rom,
            &config(0x5eed_ca10),
            &SmbCampaignOrigin::Genesis,
            &mut first_stream,
        )
        .expect("first link");
        let origin_bytes = serde_json::to_vec_pretty(&first.archive).expect("origin bytes");
        let origin_sha = format!("{:x}", Sha256::digest(&origin_bytes));
        let mut second_stream = Vec::new();
        let second = run_smb_campaign(
            &rom,
            &config(0x5eed_ca11),
            &SmbCampaignOrigin::Archive {
                path: "first-archive.json".to_owned(),
                file_sha256: origin_sha.clone(),
                report: Box::new(first.archive.clone()),
            },
            &mut second_stream,
        )
        .expect("second link");
        // The synthetic target has no forward progress to give, so the
        // second link stalls at its origin's frontier by construction.
        let produced_bytes = serde_json::to_vec_pretty(&second.archive).expect("produced bytes");
        let produced_sha = format!("{:x}", Sha256::digest(&produced_bytes));
        let proof =
            prove_smb_campaign_plateau(&first.archive, &second.archive, &origin_sha, &produced_sha)
                .expect("prove recorded stall");
        assert!(proof.stalled);
        let view_dir = std::env::temp_dir().join("fuzzer-instrumentor-recorded-chain-test");
        let _ = fs::remove_dir_all(&view_dir);
        let stream_text = String::from_utf8(second_stream).expect("stream utf-8");
        let assembled = assemble_smb_stall_operator_view(
            &view_dir,
            &first.archive,
            &origin_sha,
            &second.archive,
            &produced_sha,
            &stream_text,
            None,
            &[],
        )
        .expect("assemble view over recorded outputs");
        assert_eq!(assembled, proof);
        let flow: super::SmbRetentionFlowReport = serde_json::from_slice(
            &fs::read(view_dir.join("retention-flow.json")).expect("read flow"),
        )
        .expect("parse flow");
        assert_eq!(
            flow.records,
            second.executions_completed + second.duplicates_skipped
        );
        let _ = fs::remove_dir_all(&view_dir);
    }
}
